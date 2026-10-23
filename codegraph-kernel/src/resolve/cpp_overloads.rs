//! Which of a C++ class's overloads a call's arguments fit: by their count and string literals
//! (`cpp_overload_fit`), then, among overloads those fit alike, by the types the arguments have
//! where the call reads them. rocksdb's `Insert(ch)` on a `char ch` is `Insert(char key, …)`, not
//! `Insert(const std::string& key, …)`, and `db->Get(ro, key, &value)` is not the five-parameter
//! `Get(…, std::string* timestamp)`.
//!
//! Port of upstream's `cppOverloadFor`, `cppArgumentType` and `cppArgumentTypeFit` (name-matcher.ts
//! at a6ceb047, #2541); read the original with `git show a6ceb047:src/resolution/name-matcher.ts`.
//! The arguments are read from the syntax tree: a literal's kind, or the type a name, `&name`,
//! `name[i]` or another expression has as written where it is declared.
use super::cpp_implicit_this::strip_template_arguments;
use super::iteration::{descendant_for_position, named_children};
use super::receivers::cpp_last_segment;
use super::*;

/// What a C++ call's argument is: a literal's kind, or the type a name or expression has as written.
pub(super) enum CppArgument {
    Char,
    Number,
    Zero,
    Bool,
    Null,
    /// `lvalue`: whether the argument names an object (a name, a dereference, a member) rather than
    /// a value a call returns, where that can be told. `dependent`: its type is a template
    /// parameter of the calling function or its class, which could be a pointer as well as an object.
    Type { text: String, lvalue: Option<bool>, dependent: bool },
}

/// What a C++ type is, for an argument to fit a parameter of it.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum CppTypeCategory {
    String,
    CString,
    Char,
    Number,
    Bool,
    Pointer,
    /// `std::nullptr_t`.
    Null,
    Class,
}

const CHAR_TYPES: &[&str] = &["char", "wchar_t", "char8_t", "char16_t", "char32_t"];
const NUMBER_TYPES: &[&str] = &[
    "int", "unsigned", "signed", "short", "long", "float", "double", "size_t", "ssize_t", "ptrdiff_t",
    "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "intptr_t", "uintptr_t",
];
const STRING_TYPES: &[&str] = &["string", "wstring", "string_view", "wstring_view", "basic_string", "basic_string_view"];

/// The category of the written C++ type `ty`: a pointer's, a string's, a character's, a number's, a
/// class's.
pub(super) fn cpp_type_category(ty: &str) -> CppTypeCategory {
    use CppTypeCategory::*;
    let stripped = strip_template_arguments(ty);
    let flat = re!(r"\b(?:const|volatile|struct|class|typename|enum)\b").replace_all(&stripped, " ").replace('&', " ");
    let flat = flat.trim();
    let words: Vec<&str> = flat.split(|c: char| c == '*' || c.is_whitespace()).filter(|w| !w.is_empty()).collect();
    let last = cpp_last_segment(words.last().copied().unwrap_or(""));
    if flat.contains('*') {
        return if CHAR_TYPES.contains(&last.as_str()) && flat.matches('*').count() == 1 { CString } else { Pointer };
    }
    if last == "nullptr_t" { return Null; }
    if last == "bool" { return Bool; }
    if CHAR_TYPES.contains(&last.as_str()) && words.iter().all(|w| CHAR_TYPES.contains(w) || *w == "signed" || *w == "unsigned") { return Char; }
    if NUMBER_TYPES.contains(&last.as_str()) || (!words.is_empty() && words.iter().all(|w| NUMBER_TYPES.contains(w))) { return Number; }
    if STRING_TYPES.contains(&last.as_str()) { String } else { Class }
}

/// An array type (`Char[]`) as the pointer to its element an argument of it decays to (`Char*`);
/// a multidimensional one (`int[][]`) as the pointer to its first row (`int[]*`).
fn cpp_array_as_pointer(text: String) -> String {
    match super::cpp_range_for::cpp_array_split(&text) {
        Some((base, dimensions)) if dimensions > 0 => format!("{base}{}*", "[]".repeat(dimensions - 1)),
        _ => text,
    }
}

/// Whether the C++ type `ty` is a pointer to an array: an argument's `int[]*`, a parameter's
/// `int (*rows)[4]` or `int rows[][4]`.
fn cpp_pointer_to_array(ty: &str) -> bool {
    re!(r"\[\s*\]\s*\*|\(\s*\*[^()]*\)\s*\[|\]\s*\[").is_match(ty)
}

/// A C++ parameter declaration's type: without its default argument and its name.
pub(super) fn cpp_parameter_type(param: &str) -> String {
    let mut text = param;
    let mut depth = 0i32;
    for (i, c) in param.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            '=' if depth == 0 => { text = &param[..i]; break; }
            _ => {}
        }
    }
    let text = text.trim();
    // The name, when a type comes before it.
    if let Some(named) = re!(r"^(.*[\w>*&\s])\b[A-Za-z_]\w*\s*(?:\[[^\]]*\])?$").captures(text) {
        let ty = named[1].trim();
        if re!(r"[A-Za-z_]").is_match(&named[1]) && !re!(r"^(?:const|volatile|unsigned|signed|struct|class|enum)\s*$").is_match(ty) {
            return ty.to_string();
        }
    }
    text.to_string()
}

/// How a literal fits a parameter of each category. `nullptr` is a `std::nullptr_t` exactly, and a
/// pointer only by conversion: protobuf's `ptr.reset(nullptr)` is `reset(std::nullptr_t)`, not
/// `reset(T*)` (#2554).
fn literal_fit(argument: &CppArgument, category: CppTypeCategory) -> i64 {
    use CppTypeCategory::*;
    match (argument, category) {
        (CppArgument::Char, Char) => 3,
        (CppArgument::Char, Number) => 1,
        (CppArgument::Char, String | CString | Pointer | Null) => -3,
        (CppArgument::Number, Number) => 2,
        (CppArgument::Number, Char) => 1,
        (CppArgument::Number, String | CString | Pointer | Null) => -3,
        (CppArgument::Zero, Number) => 2,
        (CppArgument::Zero, Char | Pointer | CString | Null) => 1,
        (CppArgument::Zero, String) => -3,
        (CppArgument::Bool, Bool) => 3,
        (CppArgument::Bool, Number) => 1,
        (CppArgument::Bool, String | CString | Pointer | Null) => -3,
        (CppArgument::Null, Null) => 4,
        (CppArgument::Null, Pointer | CString) => 3,
        (CppArgument::Null, Class) => 1,
        (CppArgument::Null, String | Char | Number) => -3,
        (CppArgument::Null, Bool) => -1,
        _ => 0,
    }
}

impl KernelResolver {
    /// Of one class's overloads of `name` (in preference order), the one the call's arguments fit
    /// best: by their count and string literals, then, among overloads those fit alike, by the
    /// types the arguments have where the call reads them. A deleted overload (`void reset(T*) =
    /// delete;`) is none a call that compiles reaches, so it is only taken when every one is.
    pub(super) fn cpp_overload_for(&mut self, candidates: Vec<Arc<KNode>>, name: &str, r: &ResolveRefIn) -> Res<Arc<KNode>> {
        if candidates.len() == 1 { return Ok(candidates[0].clone()); }
        let callable: Vec<_> = candidates.iter().filter(|n| !self.cpp_deleted_function(n, name)).cloned().collect();
        let overloads = if callable.is_empty() { candidates } else { callable };
        if overloads.len() == 1 { return Ok(overloads[0].clone()); }
        let Some(args) = self.call_arguments(r, name) else { return Ok(overloads[0].clone()) };
        let fits: Vec<i64> = overloads.iter().map(|n| self.cpp_overload_fit(n, &args, r)).collect();
        let top = fits.iter().copied().max().unwrap_or(0);
        let tied: Vec<_> = overloads.iter().zip(&fits).filter(|(_, f)| **f == top).map(|(n, _)| n.clone()).collect();
        if tied.len() == 1 || args.is_empty() { return Ok(tied[0].clone()); }
        let first = tied[0].clone();
        Ok(self.cpp_argument_type_ranking(tied, name, r)?.map_or(first, |(choice, _)| choice))
    }

    /// Of overloads a call's arguments fit alike by count (in preference order), the one the types
    /// the arguments have where the call reads them fit best. None when no argument's type can be
    /// read, or several fit the types best alike (however many fit them worse) and no const twin
    /// tells them apart: `f(short)` beside `f(long)`, `f(int)` and `f(char)` is none of them.
    pub(super) fn cpp_argument_type_choice(&mut self, tied: Vec<Arc<KNode>>, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        Ok(self.cpp_argument_type_ranking(tied, name, r)?.and_then(|(choice, decided)| decided.then_some(choice)))
    }

    /// `cpp_argument_type_choice`'s ranking: the first of the overloads the argument types fit
    /// best, or its const twin, and whether it alone fits them best. None when no argument's type
    /// can be read.
    fn cpp_argument_type_ranking(&mut self, tied: Vec<Arc<KNode>>, name: &str, r: &ResolveRefIn) -> Res<Option<(Arc<KNode>, bool)>> {
        let Some(types) = self.cpp_call_argument_types(name, r)? else { return Ok(None) };
        if types.iter().all(Option::is_none) { return Ok(None); }
        let mut fits = Vec::with_capacity(tied.len());
        for n in &tied { fits.push(self.cpp_argument_type_fit(n, &types, r)?); }
        let top = fits.iter().copied().max().unwrap_or(0);
        let at = fits.iter().position(|f| *f == top).unwrap_or(0);
        let best = tied[at].clone();
        let others: Vec<_> = tied.iter().zip(&fits).enumerate().filter(|(i, (_, f))| **f == top && *i != at).map(|(_, (n, _))| n.clone()).collect();
        // Several fit the types best: decided only when a const twin is.
        let alone = others.is_empty();
        let choice = self.cpp_const_twin_choice(best.clone(), others, &types, r);
        let decided = alone || choice.id != best.id;
        Ok(Some((choice, decided)))
    }

    /// The C++ function a call of `chosen`'s name reaches among `candidates` (cppFunctionOverload,
    /// name-matcher.ts at 4a591aee): of its overloads, the functions of its qualified name the
    /// call's file can see, `chosen` first, the one the call's arguments fit. The name strategies
    /// pick a function by where it is; protobuf's `ParseWithLimit(limit, data, result)` on an
    /// `int32_t result` is the overload taking an `int32_t&`, not the `int64_t&` one nearer the
    /// call. A function hides a class of its name, so a call is to the function when `chosen` is
    /// that class. `chosen` itself for anything but a C or C++ call, or when no function is in sight.
    pub(super) fn cpp_function_overload(&mut self, chosen: Arc<KNode>, candidates: &[Arc<KNode>], r: &ResolveRefIn) -> Res<Arc<KNode>> {
        if !matches!(r.language.as_str(), "c" | "cpp") || r.reference_kind != "calls" { return Ok(chosen); }
        let mut overloads = Vec::new();
        for n in candidates {
            if n.kind == "function" && n.qualified_name == chosen.qualified_name && self.is_visible_across_files(n, r)? { overloads.push(n.clone()); }
        }
        if overloads.is_empty() { return Ok(chosen); }
        // C has no overloads: a C call reaches the function of the name its file sees, its own or
        // one a header it includes defines, else one with external linkage, the best placed of
        // them. jemalloc's tests call the `static inline do_arena_create` of the arena_util.h they
        // include, not a neighbouring test's own `static do_arena_create`. What the file sees only
        // when the build sets a flag nothing in the project sets (is_c_feature_variant) yields to
        // a definition with external linkage that the build without it calls: lua-cjson's fpconv.h
        // defines `fpconv_init` under `#ifdef USE_INTERNAL_FPCONV`, and redis's build calls
        // fpconv.c's. A call compiled only with such a flag itself is left to the file's own:
        // redis's quicklist.c tests call its `static mstime` inside `#ifdef REDIS_TEST`.
        if r.language == "c" {
            let visible = self.namespace_visible_files(&r.file_path, "c")?;
            let (own, others): (Vec<_>, Vec<_>) = overloads.iter().cloned().partition(|n| n.file_path == r.file_path || visible.contains(&n.file_path));
            let mut external = Vec::new();
            if !own.is_empty() {
                for n in others { if !self.is_static_c_function(&n)? { external.push(n); } }
            }
            let mut outside = Vec::new();
            if !external.is_empty() && own.iter().all(|n| self.is_c_feature_variant(n, r)) && !self.is_off_by_default(&r.file_path, r.line) {
                outside = external.into_iter().filter(|n| !self.is_c_feature_variant(n, r)).collect();
            }
            let pool = if !outside.is_empty() { &outside } else if !own.is_empty() { &own } else { &overloads };
            return Ok(self.find_best_match(r, pool).unwrap_or(chosen));
        }
        if let Some(at) = overloads.iter().position(|n| n.id == chosen.id) {
            let own = overloads.remove(at);
            overloads.insert(0, own);
        }
        let name = chosen.name.clone();
        self.cpp_overload_for(overloads, &name, r)
    }

    /// Whether the C++ function or method `n`, named `name`, is declared deleted: from its name to
    /// the declaration's `;` or body, it says `= delete` (protobuf's `void reset(T*
    /// PROTOBUF_NULLABLE) = delete;`). A method a macro defines never is.
    fn cpp_deleted_function(&mut self, n: &KNode, name: &str) -> bool {
        // A method a macro defines has a body.
        if cpp_macro_method_head(n).is_some() { return false; }
        let Some(source) = self.read_file(&n.file_path) else { return false };
        let from = (n.start_line - 1).max(0) as usize;
        let Ok(word) = shared_regex(&format!(r"\b{}\s*\(", regex::escape(name))) else { return false };
        // (A declaration may run over any number of lines before its `;` or body.)
        let mut text = String::new();
        for line in source.iter().skip(from).take(200) {
            text.push_str(line);
            text.push('\n');
            if let Some(at) = word.find(&text) {
                if let Some(end) = text[at.start()..].find([';', '{']) { return re!(r"=\s*delete\b").is_match(&text[at.start()..at.start() + end]); }
            }
        }
        false
    }

    /// A primitive cannot bind to a non-const class reference. A concrete
    /// standard-library overload requires a proved argument owner; an unindexed
    /// generic standard overload may otherwise be the actual callee.
    pub(super) fn cpp_reference_argument_mismatch(&mut self, node: &Arc<KNode>, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(types) = self.cpp_call_argument_types(name, r)? else { return Ok(false) };
        let Some(params) = self.declaration_parameters(node, r) else { return Ok(false) };
        let own = self.cpp_own_template_parameters(node, r)?;
        for (arg, param) in types.iter().zip(params) {
            let param = cpp_parameter_type(&param);
            if cpp_type_category(&param) != CppTypeCategory::Class || cpp_refers_to_const(&param) != Some(false) { continue; }
            if self.normalize_cpp_type_name(&param, false)?.is_some_and(|name| own.contains(&name)) { continue; }
            if let Some(CppArgument::Type { text, dependent: false, .. }) = arg {
                if matches!(cpp_type_category(text), CppTypeCategory::Char | CppTypeCategory::Number | CppTypeCategory::Bool | CppTypeCategory::Pointer | CppTypeCategory::CString) { return Ok(true); }
                if node.qualified_name.trim_start_matches("::").starts_with("std::") {
                    if self.normalize_cpp_type_name(text, false)? == self.normalize_cpp_type_name(&param, false)? { continue; }
                    let visible = self.namespace_visible_files(&r.file_path, "cpp")?;
                    let argument_owner = self.cpp_type_owner_visible(text, r, 0, false, &visible)?;
                    let mut site = r.clone().at(node); site.from_node_id = node.id.clone();
                    let parameter_owner = self.cpp_type_owner_visible(&param, &site, 0, false, &visible)?;
                    if argument_owner.zip(parameter_owner).is_some_and(|(a, p)| a.id == p.id) { continue; }
                    return Ok(true);
                }
            } else if node.qualified_name.trim_start_matches("::").starts_with("std::") {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// How well the read argument types `types` fit the parameters of the overload `n`: the same
    /// type, or a character to a character, scores; a character, number or pointer to a string, or
    /// a string or pointer to a number, counts against. Classes of other names score nothing: a
    /// conversion may join them.
    fn cpp_argument_type_fit(&mut self, n: &KNode, types: &[Option<CppArgument>], r: &ResolveRefIn) -> Res<i64> {
        let Some(params) = self.declaration_parameters(n, r) else { return Ok(0) };
        let params: Vec<_> = params.into_iter().filter(|p| p != "void").collect();
        let own = if types.iter().any(|t| matches!(t, Some(CppArgument::Type { .. }))) { self.cpp_own_template_parameters(&Arc::new(n.clone()), r)? } else { Vec::new() };
        let mut score = 0;
        for (argument, param) in types.iter().zip(&params) {
            let Some(argument) = argument else { continue };
            let param = cpp_parameter_type(param);
            if param.contains("...") { continue; }
            let category = cpp_type_category(&param);
            score += match argument {
                CppArgument::Type { text, lvalue, dependent } => {
                    // (A parameter of the function's own template parameter's type, `T* x`, takes the argument's `const` into `T`.)
                    let deduced = !own.is_empty() && self.normalize_cpp_type_name(&param, false)?.is_some_and(|p| own.contains(&p));
                    self.cpp_written_type_fit(text, &param, category, *lvalue, *dependent, deduced)?
                }
                literal => literal_fit(literal, category),
            };
        }
        Ok(score)
    }

    /// An overload's parameter types without their `const`s: what its const twins share.
    fn cpp_constless_parameters(&mut self, n: &KNode, r: &ResolveRefIn) -> Option<Vec<String>> {
        let params = self.declaration_parameters(n, r)?;
        Some(params.into_iter().filter(|p| p != "void").map(|p| re!(r"\bconst\b").replace_all(&cpp_parameter_type(&p), " ").split_whitespace().collect::<String>()).collect())
    }

    /// How many of the arguments `types` an overload's pointer or reference parameters add `const` to.
    fn cpp_const_added(&mut self, n: &KNode, types: &[Option<CppArgument>], r: &ResolveRefIn) -> usize {
        let Some(params) = self.declaration_parameters(n, r) else { return 0 };
        let params: Vec<_> = params.into_iter().filter(|p| p != "void").collect();
        types.iter().zip(&params).filter(|(argument, param)| {
            let param = cpp_parameter_type(param);
            matches!(argument, Some(CppArgument::Type { text, .. }) if cpp_refers_to_const(&param) == Some(true) && cpp_argument_const(text, &param) == Some(false))
        }).count()
    }

    /// `best`, or of its twins among `others` (overloads alike but for the `const` of a pointer or
    /// reference parameter) the one that adds `const` to the fewest of the arguments `types`, as
    /// C++ ranks them: protobuf's `DynamicCastMessage<T>(ptr.get())` on a
    /// `std::shared_ptr<Message> ptr` is `T* DynamicCastMessage(MessageLite* from)`, not the `const
    /// T* DynamicCastMessage(const MessageLite* from)` declared before it. (A const object fits only
    /// the const twin: `cpp_written_type_fit`.)
    fn cpp_const_twin_choice(&mut self, best: Arc<KNode>, others: Vec<Arc<KNode>>, types: &[Option<CppArgument>], r: &ResolveRefIn) -> Arc<KNode> {
        if others.is_empty() { return best; }
        let Some(shape) = self.cpp_constless_parameters(&best, r) else { return best };
        let mut added = self.cpp_const_added(&best, types, r);
        let mut choice = best;
        for n in others {
            if self.cpp_constless_parameters(&n, r).as_ref() != Some(&shape) { continue; }
            let count = self.cpp_const_added(&n, types, r);
            if count < added {
                choice = n;
                added = count;
            }
        }
        choice
    }

    /// How an argument written as the type `argument` fits a parameter of the type `param` (of
    /// category `category`). An object is no pointer, nor a pointer to characters a pointer to
    /// anything but characters or `void`, or the other way round: protobuf's
    /// `DynamicCastMessage<T>(*msg)` takes a reference, and rocksdb's `db_->Put(opts, key, ts,
    /// value)` on a `char key[]` no `ColumnFamilyHandle*`. A pointer or reference to a non-const
    /// type never takes a const object, nor a reference a value a call returns (`lvalue` false).
    /// An object of the parameter's own type that a name or dereference reads (`lvalue`) never
    /// binds an rvalue reference: `Foo x; Take(x)` is `Take(const Foo&)`, not `Take(Foo&&)`; an
    /// `int` converted to a `Value` is a temporary, which does. None of that holds for an argument of a
    /// template parameter's type (`dependent`), and the pointer, `const` and rvalue rules not for a
    /// parameter whose type the function's own template parameter is (`deduced`). (Of two
    /// overloads alike but for a `const`, which one a non-const object takes is
    /// `cpp_const_twin_choice`'s.)
    fn cpp_written_type_fit(&mut self, argument: &str, param: &str, category: CppTypeCategory, lvalue: Option<bool>, dependent: bool, deduced: bool) -> Res<i64> {
        use CppTypeCategory::*;
        let fit = self.cpp_unqualified_type_fit(argument, param, category)?;
        if fit < 0 || dependent { return Ok(fit); }
        let argument_category = cpp_type_category(argument);
        if category == Pointer && argument_category == Class { return Ok(-3); }
        if !deduced && category == Pointer && argument_category == CString && !re!(r"\bvoid\b").is_match(param) { return Ok(-3); }
        if category == CString && argument_category == Pointer { return Ok(-3); }
        // A pointer to an array (`int a[3][4]` decayed) is no pointer to its element, nor the other
        // way round; `void*` takes either.
        if !deduced && matches!(category, Pointer | CString) && cpp_pointer_to_array(argument) != cpp_pointer_to_array(param) && !re!(r"\bvoid\b").is_match(param) { return Ok(-3); }
        if !deduced && lvalue == Some(true) && fit == 3 && re!(r"&&\s*$").is_match(&strip_template_arguments(param)) { return Ok(-3); }
        if cpp_refers_to_const(param) != Some(false) { return Ok(fit); }
        if !deduced && cpp_argument_const(argument, param) == Some(true) { return Ok(-3); }
        Ok(if lvalue == Some(false) && !strip_template_arguments(param).contains('*') { -3 } else { fit })
    }

    /// How an argument written as the type `argument` fits a parameter of the type `param`, `const` aside.
    fn cpp_unqualified_type_fit(&mut self, argument: &str, param: &str, category: CppTypeCategory) -> Res<i64> {
        use CppTypeCategory::*;
        let argument_category = cpp_type_category(argument);
        let pointer = |c: CppTypeCategory| c == Pointer || c == CString;
        if self.normalize_cpp_type_name(argument, false)? == self.normalize_cpp_type_name(param, false)? && pointer(argument_category) == pointer(category) { return Ok(3); }
        if argument_category == category && argument_category != Class { return Ok(2); }
        let textual = |c: CppTypeCategory| c == String || c == CString;
        Ok(if matches!(argument_category, Char | Number | Pointer) && category == String
            || (textual(argument_category) || argument_category == Pointer) && matches!(category, Char | Number)
            || matches!(argument_category, Char | Number) && matches!(category, Pointer | CString) { -3 } else { 0 })
    }

    /// What each argument of the call of `name` at `r` is, where that can be read (cppArgumentType):
    /// a character, number, `true`/`false` or `nullptr` literal, or the written type of a name the
    /// calling function declares, of `&name`, of `name[i]` (rocksdb's `TryLock(column_family[i],
    /// keys[i], …)` on a `std::vector<Slice>`), or of an expression. None for the call when it
    /// can't be found. (String literals are `cpp_overload_fit`'s.)
    pub(super) fn cpp_call_argument_types(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Vec<Option<CppArgument>>>> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(None) };
        let callee = shared_regex(&format!(r"(?:^|[.:>])\s*{}(?:\s*<[\s\S]*>)?\s*$", regex::escape(name)))?;
        let mut call = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        loop {
            if call.kind() == "call_expression" && call.child_by_field_name("function").is_some_and(|f| callee.is_match(&source.text()[f.byte_range()])) { break; }
            let Some(parent) = call.parent() else { return Ok(None) };
            call = parent;
        }
        let Some(arguments) = call.child_by_field_name("arguments") else { return Ok(None) };
        let mut types = Vec::new();
        for argument in named_children(arguments).into_iter().filter(|n| !n.kind().contains("comment")) {
            types.push(self.cpp_argument_type(argument, &source, r)?);
        }
        Ok(Some(types))
    }

    fn cpp_argument_type(&mut self, node: tree_sitter::Node, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Res<Option<CppArgument>> {
        let text = source.text()[node.byte_range()].trim();
        Ok(match node.kind() {
            "char_literal" => Some(CppArgument::Char),
            "true" | "false" => Some(CppArgument::Bool),
            // `NULL` is an integer zero (a null pointer constant); only `nullptr` is a nullptr_t.
            "null" | "identifier" if text == "NULL" => Some(CppArgument::Zero),
            "null" | "nullptr" => Some(CppArgument::Null),
            "number_literal" if re!(r"^0+[uUlL]*$").is_match(text) => Some(CppArgument::Zero),
            "number_literal" => Some(CppArgument::Number),
            "unary_expression" if re!(r"^[-+]\s*[0-9.]").is_match(text) => Some(CppArgument::Number),
            "string_literal" | "raw_string_literal" | "concatenated_string" => None,
            // `&value`: a pointer to what the name is.
            "pointer_expression" => {
                let operator = node.child_by_field_name("operator").map(|o| o.kind());
                match node.child_by_field_name("argument").filter(|a| a.kind() == "identifier" && operator == Some("&")) {
                    // (`&ref` is a pointer to the referee.)
                    Some(name) => match self.cpp_written_name(name, source, r)? {
                        Some(w) => {
                            let text = format!("{}*", cpp_declared_constness(&w.text, name, source).trim_end_matches('&').trim_end());
                            let dependent = self.cpp_dependent_argument(&text, r)?;
                            Some(CppArgument::Type { text, lvalue: Some(false), dependent })
                        }
                        None => None,
                    },
                    None => match self.cpp_written_expression(node, source, r)? {
                        Some(w) => {
                            let dependent = self.cpp_dependent_argument(&w.text, r)?;
                            Some(CppArgument::Type { lvalue: cpp_names_object(text, &w.text), text: w.text, dependent })
                        }
                        None => None,
                    },
                }
            }
            // (An array is a pointer to its element: fmt's `format_base2e(4, buf, …)` on a `Char
            // buf[8]` is the `Char* out` overload.)
            "identifier" => match self.cpp_written_name(node, source, r)? {
                Some(w) => {
                    let text = cpp_array_as_pointer(cpp_declared_constness(&w.text, node, source));
                    let dependent = self.cpp_dependent_argument(&text, r)?;
                    Some(CppArgument::Type { text, lvalue: Some(true), dependent })
                }
                None => None,
            },
            // `keys[i]`: the element a subscript reads (`cpp_subscript_element`), through the expression typer.
            _ => match self.cpp_written_expression(node, source, r)? {
                Some(w) => {
                    let text = cpp_array_as_pointer(w.text);
                    let dependent = self.cpp_dependent_argument(&text, r)?;
                    Some(CppArgument::Type { lvalue: cpp_names_object(source.text()[node.byte_range()].trim(), &text), text, dependent })
                }
                None => None,
            },
        })
    }

    /// Whether an argument of the written type `ty` has the type of a template parameter of the
    /// calling function or its class (fmt's `const S& fmt`), which could be a pointer as well as an
    /// object.
    fn cpp_dependent_argument(&mut self, ty: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(name) = self.normalize_cpp_type_name(ty, false)? else { return Ok(false) };
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(false) };
        self.cpp_template_parameter(&caller, &name, r)
    }
}

/// `written`, with a `const` in front when the declaration of the name `node` writes one before
/// its type (`const Buffer* b`, `constexpr char key[]`), which the declared type leaves out.
fn cpp_declared_constness(written: &str, node: tree_sitter::Node, source: &SourceFile) -> String {
    if !re!(r"^\s*const\b").is_match(written) && super::cpp_receivers::cpp_declared_const(node, source) { format!("const {written}") } else { written.to_string() }
}

/// Whether the C++ argument `expr`, of the written type `ty`, names an object, which a reference to
/// a non-const type can bind: a dereference, a subscript, a member, or a call that returns an
/// lvalue reference. Some(false) for a call that returns a value; None when that can't be told.
fn cpp_names_object(expr: &str, ty: &str) -> Option<bool> {
    let mut text = expr.trim();
    while text.starts_with('(') && text.ends_with(')') && opening_paren(text, text.len() - 1) == Some(0) { text = text[1..text.len() - 1].trim(); }
    if text.starts_with('*') || text.ends_with(']') { return Some(true); }
    if !text.ends_with(')') { return re!(r"^[A-Za-z_][\w\s.:>-]*$").is_match(text).then_some(true); }
    if re!(r"&&\s*$").is_match(ty) { return None; }
    Some(re!(r"&\s*$").is_match(ty))
}

/// Whether the C++ type `ty`, a pointer or an lvalue reference of one level, refers to a const
/// object: `const T*`, `T const&`. (A pointer's own `const`, `T* const`, is not the object's.)
/// None for any other type.
fn cpp_refers_to_const(ty: &str) -> Option<bool> {
    let text = strip_template_arguments(ty);
    let text = text.trim();
    let stars = text.matches('*').count();
    if stars > 1 || (stars == 0 && !re!(r"[^&]&\s*$").is_match(text)) { return None; }
    let object = if stars == 1 { &text[..text.find('*').unwrap_or(text.len())] } else { text };
    Some(re!(r"\bconst\b").is_match(object))
}

/// Whether the object an argument of the written type `argument` gives a pointer or reference
/// parameter `param` is const: what the pointer points at, or the object itself. None when the
/// argument is no such thing.
fn cpp_argument_const(argument: &str, param: &str) -> Option<bool> {
    let argument_pointer = strip_template_arguments(argument).contains('*');
    if strip_template_arguments(param).contains('*') { return if argument_pointer { cpp_refers_to_const(argument) } else { None }; }
    if argument_pointer { None } else { Some(re!(r"\bconst\b").is_match(&strip_template_arguments(argument))) }
}

/// Where the `(` the `)` at byte `close` of `text` closes is.
fn opening_paren(text: &str, close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in text[..=close].char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' => { depth -= 1; if depth == 0 { return Some(i); } }
            _ => (),
        }
    }
    None
}

/// The expanded definition a C++ method that a macro defines carries as its signature
/// (cpp-macro-methods.ts), `inline const FileDescriptor* Descriptor::file() const` for protobuf's
/// `PROTOBUF_DEFINE_ACCESSOR(Descriptor, file, const FileDescriptor*)`: its source is the
/// invocation, which spells neither its return type nor its parameters. None for any other node.
pub(super) fn cpp_macro_method_head(n: &KNode) -> Option<&str> {
    if n.kind != "method" { return None; }
    let signature = n.signature.as_deref()?;
    shared_regex(&format!(r"::\s*{}\s*\(", regex::escape(&n.name))).ok()?.is_match(signature).then_some(signature)
}

/// The expansion `head` of a method a macro defines, parsed as a definition of its own.
pub(super) fn cpp_macro_method_tree(head: &str) -> Option<(Rc<SourceFile>, tree_sitter::Tree)> {
    let source = Rc::new(SourceFile::new(vec![format!("{head} {{}}")]));
    let text = super::cpp_annotations::cpp_without_annotations(source.text()).into_owned();
    let tree = crate::tree::parse_with_cached_parser(&text, "cpp").ok()?;
    Some((source, tree))
}
