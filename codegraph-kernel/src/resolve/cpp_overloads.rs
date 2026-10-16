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
use super::cpp_range_for::CppWritten;
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
    Type(String),
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

/// A C++ parameter declaration's type: without its default argument and its name.
/// An array type (`Char[]`) as the pointer to its element an argument of it decays to (`Char*`).
fn cpp_array_as_pointer(text: String) -> String {
    match super::cpp_range_for::cpp_array_split(&text) {
        Some((base, dimensions)) if dimensions > 0 => format!("{base}*"),
        _ => text,
    }
}

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
        Ok(self.cpp_argument_type_choice(tied, name, r)?.unwrap_or(first))
    }

    /// Of overloads a call's arguments fit alike by count (in preference order), the one the types
    /// the arguments have where the call reads them fit best; the first of those that fit best
    /// alike. None when no argument's type can be read, or every overload fits the types alike.
    pub(super) fn cpp_argument_type_choice(&mut self, tied: Vec<Arc<KNode>>, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(types) = self.cpp_call_argument_types(name, r)? else { return Ok(None) };
        if types.iter().all(Option::is_none) { return Ok(None); }
        let mut best: Option<(Arc<KNode>, i64)> = None;
        let mut decided = false;
        for n in tied {
            let fit = self.cpp_argument_type_fit(&n, &types, r)?;
            match &best {
                None => best = Some((n, fit)),
                Some((_, top)) if fit > *top => { best = Some((n, fit)); decided = true; }
                Some((_, top)) if fit < *top => decided = true,
                Some(_) => {}
            }
        }
        Ok(best.filter(|_| decided).map(|(n, _)| n))
    }

    /// The C++ function a call of `chosen`'s name reaches among `candidates` (cppFunctionOverload,
    /// name-matcher.ts at 4a591aee): of its overloads, the functions of its qualified name the
    /// call's file can see, `chosen` first, the one the call's arguments fit. The name strategies
    /// pick a function by where it is; protobuf's `ParseWithLimit(limit, data, result)` on an
    /// `int32_t result` is the overload taking an `int32_t&`, not the `int64_t&` one nearer the
    /// call. A function hides a class of its name, so a call is to the function when `chosen` is
    /// that class. `chosen` itself for anything but a C++ call, or when no function is in sight.
    pub(super) fn cpp_function_overload(&mut self, chosen: Arc<KNode>, candidates: &[Arc<KNode>], r: &ResolveRefIn) -> Res<Arc<KNode>> {
        if r.language != "cpp" || r.reference_kind != "calls" { return Ok(chosen); }
        let mut overloads = Vec::new();
        for n in candidates {
            if n.kind == "function" && n.qualified_name == chosen.qualified_name && self.is_visible_across_files(n, r)? { overloads.push(n.clone()); }
        }
        if overloads.is_empty() { return Ok(chosen); }
        if let Some(at) = overloads.iter().position(|n| n.id == chosen.id) {
            let own = overloads.remove(at);
            overloads.insert(0, own);
        }
        let name = chosen.name.clone();
        self.cpp_overload_for(overloads, &name, r)
    }

    /// Whether the C++ function or method `n`, named `name`, is declared deleted: from its name to
    /// the declaration's `;` or body, it says `= delete` (protobuf's `void reset(T*
    /// PROTOBUF_NULLABLE) = delete;`).
    fn cpp_deleted_function(&mut self, n: &KNode, name: &str) -> bool {
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

    /// How well the read argument types `types` fit the parameters of the overload `n`: the same
    /// type, or a character to a character, scores; a character, number or pointer to a string, or
    /// a string or pointer to a number, counts against. Classes of other names score nothing: a
    /// conversion may join them.
    fn cpp_argument_type_fit(&mut self, n: &KNode, types: &[Option<CppArgument>], r: &ResolveRefIn) -> Res<i64> {
        let Some(params) = self.declaration_parameters(n, r) else { return Ok(0) };
        let params: Vec<_> = params.into_iter().filter(|p| p != "void").collect();
        let mut score = 0;
        for (argument, param) in types.iter().zip(&params) {
            let Some(argument) = argument else { continue };
            let param = cpp_parameter_type(param);
            if param.contains("...") { continue; }
            let category = cpp_type_category(&param);
            score += match argument {
                CppArgument::Type(text) => self.cpp_written_type_fit(text, &param, category)?,
                literal => literal_fit(literal, category),
            };
        }
        Ok(score)
    }

    /// How an argument written as the type `argument` fits a parameter of the type `param`.
    fn cpp_written_type_fit(&mut self, argument: &str, param: &str, category: CppTypeCategory) -> Res<i64> {
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
                    Some(name) => self.cpp_written_name(name, source, r)?.map(|w| CppArgument::Type(format!("{}*", w.text))),
                    None => self.cpp_written_expression(node, source, r)?.map(|w| CppArgument::Type(w.text)),
                }
            }
            // (An array is a pointer to its element: fmt's `format_base2e(4, buf, …)` on a `Char
            // buf[8]` is the `Char* out` overload.)
            "identifier" => self.cpp_written_name(node, source, r)?.map(|w| CppArgument::Type(cpp_array_as_pointer(w.text))),
            // `keys[i]`: the element a subscript reads (`cpp_subscript_element`), through the expression typer.
            _ => self.cpp_written_expression(node, source, r)?.map(|w: CppWritten| CppArgument::Type(cpp_array_as_pointer(w.text))),
        })
    }
}
