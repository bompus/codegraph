//! The element type of what a C++ range-based `for` over `auto` iterates, so the loop's variable
//! has a type: `for (auto cfd : cfds)` over a `const autovector<ColumnFamilyData*>&` parameter
//! calls `ColumnFamilyData`'s methods, not those of an earlier variable named `cfd`.
//!
//! Port of upstream's `cppAutoLoopElement` and the helpers it reads with (name-matcher.ts at
//! 01ab45d0, #2492); read the original with `git show 01ab45d0:src/resolution/name-matcher.ts`.
//! The range is read from the syntax tree rather than from lines of text:
//!  - a parameter or local of the calling function (an `auto` local through its initializer), an
//!    enclosing `auto` loop's variable, or a data member of the caller's class;
//!  - `*x`, `x.m`, `x->m`, `this`, and a call of a function or method whose declaration writes its
//!    return type (`FieldNumberOrder(desc)`, `vstorage->GetBlobFiles()`);
//!
//! with aliases followed, template arguments and all, from where each is declared. Its element is a
//! standard or abseil container's first template argument (protobuf's repeated fields' and rocksdb's
//! `autovector`'s too), or what a project class's `begin()` returns, dereferenced. Every type is
//! looked up where it is written. A type a template parameter decides, or one that can't be read,
//! gives no element, and the call resolves as it did before.
use super::cpp_aliases::type_segments;
use super::cpp_implicit_this::strip_template_arguments;
use super::cpp_receivers::{cpp_ast_declared_type, cpp_binding_type, cpp_declarator_name, cpp_held_type, cpp_holder_arguments};
use super::iteration::{descendant_for_position, named_children};
use super::*;

type TsNode<'t> = tree_sitter::Node<'t>;

/// A C++ type as written, and the site it is written at, which its names are looked up from.
#[derive(Clone)]
pub(super) struct CppWritten {
    pub text: String,
    pub site: ResolveRefIn,
}

/// The standard library's and abseil's containers whose elements are their first template
/// argument.
const LIBRARY_SEQUENCES: &[&str] = &[
    "vector", "list", "forward_list", "deque", "set", "multiset", "unordered_set", "unordered_multiset", "array", "span",
    "initializer_list", "Span", "InlinedVector", "FixedArray", "flat_hash_set", "node_hash_set", "btree_set", "btree_multiset",
];
/// Widely used project containers of the same kind: protobuf's repeated fields, rocksdb's `autovector`.
const PROJECT_SEQUENCES: &[&str] = &["RepeatedField", "RepeatedPtrField", "autovector"];
/// The standard library's and abseil's maps, whose elements are `std::pair`s of a key and a value.
pub(super) const LIBRARY_MAPS: &[&str] = &["map", "multimap", "unordered_map", "unordered_multimap", "flat_hash_map", "node_hash_map", "btree_map", "btree_multimap"];
/// The standard library's container adaptors, whose element is their first template argument.
const LIBRARY_ADAPTORS: &[&str] = &["stack", "queue", "priority_queue"];
/// The members of a library container that return an iterator into it, with the iterator's kind.
const ITERATOR_MEMBERS: &[(&str, &str)] = &[
    ("begin", "iterator"), ("end", "iterator"), ("find", "iterator"), ("lower_bound", "iterator"), ("upper_bound", "iterator"),
    ("cbegin", "const_iterator"), ("cend", "const_iterator"), ("rbegin", "reverse_iterator"), ("rend", "reverse_iterator"),
    ("crbegin", "const_reverse_iterator"), ("crend", "const_reverse_iterator"),
];
const MAX_DEPTH: u32 = 4;

/// A written type and the array dimensions `cpp_ast_declared_type` gives it (`Status[]` for
/// `Status statuses[3]`, `int[][]` for two): the element type, and how many. None when a `[` is
/// anywhere else.
pub(super) fn cpp_array_split(text: &str) -> Option<(&str, usize)> {
    let mut base = text.trim_end();
    let mut dimensions = 0;
    while let Some(inner) = base.strip_suffix("[]") {
        base = inner.trim_end();
        dimensions += 1;
    }
    (!base.contains('[')).then_some((base, dimensions))
}

/// An element of the array `text` (`Status[]`): its type with one dimension less. None for a
/// type that is no array.
fn cpp_array_element(text: &str) -> Option<String> {
    let (base, dimensions) = cpp_array_split(text)?;
    (dimensions > 0).then(|| format!("{base}{}", "[]".repeat(dimensions - 1)))
}

/// A written type without `typename`, `const` and `&`, which name no part of it.
fn without_type_keywords(text: &str) -> String {
    re!(r"\b(?:typename|const)\b").replace_all(text, " ").replace('&', " ").trim().to_string()
}

/// The container of a container's iterator type (`std::vector<Foo*>` of
/// `std::vector<Foo*>::const_iterator`).
fn iterator_container(text: &str) -> Option<String> {
    let bare = without_type_keywords(text);
    re!(r"^(.*>)\s*::\s*(?:const_)?(?:reverse_)?iterator$").captures(&bare).map(|m| m[1].trim().to_string())
}

/// A written type without nullability annotations (`const FieldDescriptor* absl_nonnull`) or
/// cv-qualifiers, which are no part of the name it is looked up by.
pub(super) fn cpp_plain_type(text: &str) -> String {
    let text = re!(r"\b(?:absl_nonnull|absl_nullable|absl_nullability_unknown|_Nonnull|_Nullable|_Null_unspecified|const|volatile)\b").replace_all(text, " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The object a member expression reads from, the member's name, and its explicit template
/// arguments (`<…>` as written).
type CppMemberRead = (CppWritten, String, Option<String>);

/// A type written `auto`, `auto&`, `const auto*`.
pub(super) fn is_cpp_auto(raw: &str) -> bool {
    type_segments(raw).is_some_and(|parts| parts.len() == 1 && parts[0] == "auto")
}

/// Whether `text` is a pointer: a `*` outside its template arguments.
pub(super) fn top_level_pointer(text: &str) -> bool {
    let mut depth = 0i32;
    text.chars().any(|c| {
        match c { '<' => depth += 1, '>' => depth -= 1, _ => {} }
        c == '*' && depth == 0
    })
}

/// The template arguments of `text`'s first template, split at its top-level commas.
fn template_arguments(text: &str) -> Option<Vec<String>> {
    let open = text.find('<')?;
    let mut depth = 0i32;
    let mut arguments = Vec::new();
    let mut current = String::new();
    for c in text[open..].chars() {
        match c {
            '<' | '(' => {
                depth += 1;
                if depth == 1 { continue; }
            }
            '>' | ')' => {
                depth -= 1;
                if depth == 0 {
                    arguments.push(current.trim().to_string());
                    return Some(arguments);
                }
            }
            ',' if depth == 1 => {
                arguments.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    None
}

/// The type `*x` has when `x` has the type `text`: what a pointer points at, or what a standard
/// smart pointer or optional holds. None for any other type.
pub(super) fn cpp_pointee(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches('&').trim();
    let mut depth = 0i32;
    for (i, c) in text.char_indices().rev() {
        match c {
            '>' => depth += 1,
            '<' => depth -= 1,
            '*' if depth == 0 => return Some(text[..i].trim().to_string()),
            _ => {}
        }
    }
    let plain = cpp_plain_type(text);
    cpp_held_type(cpp_holder_arguments(&plain)?)
}

/// The function declarator of a declarator that declares a function (`f()`, `*f()`, `&f()`), not
/// a variable.
fn declared_function(mut declarator: TsNode) -> Option<TsNode> {
    loop {
        match declarator.kind() {
            "function_declarator" => return Some(declarator),
            "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
                declarator = declarator.child_by_field_name("declarator").or_else(|| declarator.named_child(0))?;
            }
            _ => return None,
        }
    }
}

fn without_spaces(text: &str) -> String { text.chars().filter(|c| !c.is_whitespace()).collect() }

/// The initializer of `name`'s declaration in the block `parent`, before `at`: `vstorage->
/// GetBlobFiles()` in `const auto& blob_files = vstorage->GetBlobFiles();`. The same declarations
/// `cpp_binding_type` reads.
pub(super) fn binding_initializer<'t>(parent: TsNode<'t>, at: TsNode<'t>, name: &str, source: &SourceFile) -> Option<TsNode<'t>> {
    let children = match parent.kind() {
        "compound_statement" | "declaration_list" | "translation_unit" => named_children(parent),
        _ => statement_initializer(parent).into_iter().collect(),
    };
    if children.is_empty() { return None; }
    for declaration in children.into_iter().rev().filter(|n| n.kind() == "declaration" && n.end_byte() <= at.start_byte()) {
        let mut cursor = declaration.walk();
        let declarators: Vec<_> = declaration.children_by_field_name("declarator", &mut cursor).collect();
        if let Some(declarator) = declarators.into_iter().find(|d| cpp_declarator_name(*d, source) == name) {
            if declarator.kind() != "init_declarator" { return None; }
            return declarator.child_by_field_name("value").filter(|v| !matches!(v.kind(), "initializer_list" | "argument_list"));
        }
    }
    None
}

/// The declaration a `for`, `if`, `switch` or `while` statement (or its condition) opens with,
/// which names variables for the statement as a block's declarations do for the block.
pub(super) fn statement_initializer(node: TsNode<'_>) -> Option<TsNode<'_>> {
    match node.kind() {
        "for_statement" | "condition_clause" => node.child_by_field_name("initializer"),
        "if_statement" | "switch_statement" | "while_statement" => node.child_by_field_name("condition").and_then(|c| c.child_by_field_name("initializer")),
        _ => None,
    }.filter(|n| n.kind() == "declaration")
}

/// Whether the written type `text` is a template parameter of `parameters`, or a library template
/// of one (`std::shared_ptr<T>`, `std::vector<T>`).
fn type_depends_on(text: &str, parameters: &HashSet<String>, depth: u32) -> bool {
    if depth > 3 { return false; }
    let plain = cpp_plain_type(text);
    let primary = plain.split('<').next().unwrap_or("").trim().trim_start_matches("::").trim_end_matches(['*', '&', ' ']);
    if parameters.contains(primary.split("::").next().unwrap_or("").trim()) { return true; }
    if let Some(held) = cpp_holder_arguments(plain.trim_end_matches(['*', '&', ' '])).and_then(cpp_held_type) {
        return type_depends_on(&held, parameters, depth + 1);
    }
    re!(r"^(?:::)?(?:std|absl)::").is_match(&plain) && re!(r"[A-Za-z_]\w*").find_iter(&plain).any(|word| parameters.contains(word.as_str()))
}

/// A call expression's text as the extractor spells a chain: no spaces, `.` for `->`, and the
/// arguments of each call left out (`factory.NewEnumGenerator()`).
fn chain_spelling(text: &str) -> String {
    let text: String = text.split_whitespace().collect::<String>().replace("->", ".");
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' => { if depth == 0 { out.push('('); } depth += 1; }
            ')' => { depth = depth.saturating_sub(1); if depth == 0 { out.push(')'); } }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn site_at(r: &ResolveRefIn, node: &KNode) -> ResolveRefIn {
    let mut site = r.clone().at(node);
    site.from_node_id = node.id.clone();
    site.column = node.start_column;
    site
}

fn cpp_class_node(node: &KNode) -> bool {
    matches!(node.language.as_str(), "c" | "cpp") && matches!(node.kind.as_str(), "class" | "struct" | "union")
}

impl KernelResolver {
    /// The element type of the range the `auto` range-for `range_for` (in `r`'s file) iterates, as
    /// written where the range's type is declared: what the loop's variable is. None when it can't be
    /// read reliably, or a template parameter decides it.
    pub(super) fn cpp_range_for_element(&mut self, range_for: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn, depth: u32) -> Res<Option<CppWritten>> {
        if depth > MAX_DEPTH { return Ok(None); }
        let Some(right) = range_for.child_by_field_name("right") else { return Ok(None) };
        let Some(range) = self.cpp_expression_written(right, source, r, depth + 1)? else { return Ok(None) };
        let Some(element) = self.cpp_range_element_of(range, r, 0)? else { return Ok(None) };
        let Some(element) = self.cpp_follow_written_aliases(element, r)? else { return Ok(None) };
        if self.cpp_written_template_parameter(&element)? { return Ok(None); }
        Ok(Some(element))
    }

    /// The project class of the variable `receiver` an `auto` range-for opening on line `line`
    /// (0-based) declares, when the call at `r` is in that loop's body: what its range's element is,
    /// or points at. None for any other loop, an element that is no project class (a smart pointer
    /// or a standard type), or one that can't be read.
    pub(super) fn cpp_enclosing_range_element(&mut self, receiver: &str, line: usize, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        if r.language != "cpp" { return Ok(None); }
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(None) };
        let mut at = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        while let Some(parent) = at.parent() {
            // A nearer declaration of the name, `{ Gadget* w = g; w->Self(); }` in the body, hides the loop's.
            if cpp_binding_type(parent, at, receiver, &source).is_some() { return Ok(None); }
            if parent.kind() == "for_range_loop" && parent.start_position().row == line
                && parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id())
                && parent.child_by_field_name("declarator").is_some_and(|d| cpp_declarator_name(d, &source) == receiver) {
                let (Some(ty), Some(declarator)) = (parent.child_by_field_name("type"), parent.child_by_field_name("declarator")) else { return Ok(None) };
                if !is_cpp_auto(&cpp_ast_declared_type(ty, declarator, &source)) { return Ok(None); }
                let Some(element) = self.cpp_range_for_element(parent, &source, r, 0)? else { return Ok(None) };
                let element = if top_level_pointer(&element.text) {
                    match cpp_pointee(&element.text) { Some(text) if !top_level_pointer(&text) => CppWritten { text, site: element.site }, _ => return Ok(None) }
                } else { element };
                if re!(r"^(?:::)?(?:std|absl)::").is_match(element.text.trim()) { return Ok(None); }
                return self.cpp_written_class(&element, r);
            }
            at = parent;
        }
        Ok(None)
    }

    /// Whether the type `written` names is a template parameter's, which a template argument decides.
    pub(super) fn cpp_written_template_parameter(&mut self, written: &CppWritten) -> Res<bool> {
        let Some(parts) = type_segments(&written.text) else { return Ok(false) };
        let Some(node) = self.node_by_id(&written.site.from_node_id)? else { return Ok(false) };
        self.cpp_template_parameter(&node, &parts[0], &written.site)
    }

    /// `written` once the aliases it is written with are followed, each from its own declaration:
    /// rocksdb's `const BlobFiles&` is `std::vector<std::shared_ptr<BlobFileMetaData>>`. None when an
    /// alias on the way can't be read: a member of an aliased type, `typename`, `decltype`.
    pub(super) fn cpp_follow_written_aliases(&mut self, written: CppWritten, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        // An array of an alias (`Handle handles[2]`) is an array of what the alias names.
        if let Some((base, dimensions)) = cpp_array_split(&written.text).filter(|(_, d)| *d > 0) {
            let base = CppWritten { text: base.to_string(), site: written.site };
            return Ok(self.cpp_follow_written_aliases(base, r)?.map(|f| CppWritten { text: format!("{}{}", f.text, "[]".repeat(dimensions)), site: f.site }));
        }
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        let mut current = written;
        for _ in 0..4 {
            let Some(parts) = type_segments(&current.text) else { return Ok(Some(current)) };
            if parts[0] == "std" { return Ok(Some(current)); }
            let Some((rhs, declaration, consumed)) = self.cpp_alias_declaration(&current.text, &parts, &current.site, 0, &included)? else { return Ok(Some(current)) };
            if consumed < parts.len() || rhs.trim().is_empty() || re!(r"\b(?:typename|decltype)\b").is_match(&rhs) { return Ok(None); }
            let text = if top_level_pointer(&current.text) { format!("{}*", rhs.trim()) } else { rhs.trim().to_string() };
            let site = if declaration.id == current.site.from_node_id { current.site.clone() } else { site_at(&current.site, &declaration) };
            current = CppWritten { text, site };
        }
        Ok(None)
    }

    /// The project class `written` names, looked up as C++ looks it up where it is written: the
    /// classes around that place and their bases first, then the namespaces.
    pub(super) fn cpp_written_class(&mut self, written: &CppWritten, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(written) = self.cpp_follow_written_aliases(written.clone(), r)? else { return Ok(None) };
        if top_level_pointer(&written.text) || self.cpp_written_template_parameter(&written)? { return Ok(None); }
        let Some(parts) = type_segments(&written.text) else { return Ok(None) };
        let plain = cpp_plain_type(&written.text);
        let primary = plain.split('<').next().unwrap_or(&plain).trim_end_matches('&').trim();
        // `::Registry` is the global one, never one nested in the classes around it.
        let mut owner = if parts.len() == 1 && !primary.starts_with("::") { self.cpp_class_scope_type(&parts[0], &written.site)? } else { None };
        if owner.is_none() {
            let included = self.namespace_visible_files(&r.file_path, "cpp")?;
            owner = self.cpp_type_owner_visible(primary, &written.site, 0, false, &included)?;
        }
        Ok(owner.filter(|n| cpp_class_node(n)))
    }

    /// The C++ class `cls`'s methods named `name`: its own declarations and definitions, else the
    /// nearest one a class it derives from has.
    pub(super) fn cpp_methods_named(&mut self, cls: &KNode, name: &str, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let own: Vec<_> = self.nodes_by_qualified_name(&format!("{}::{name}", cls.qualified_name))?.iter()
            .filter(|n| matches!(n.kind.as_str(), "method" | "function") && matches!(n.language.as_str(), "c" | "cpp")).cloned().collect();
        if !own.is_empty() { return Ok(own); }
        let mut seen = HashSet::new();
        Ok(self.cpp_method_of(cls, name, r, 0, &mut seen)?.into_iter().collect())
    }

    /// What one of `functions` (overloads, a declaration and its definition) returns, as written
    /// where it is declared. None when overloads taking different parameters return different
    /// types, since which one a call picks is not read; a `const` and a non-`const` overload with
    /// the same parameters (`items()`, `items() const`) give the first one's.
    fn cpp_return_type_of(&mut self, functions: &[Arc<KNode>], r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let mut first: Option<(CppWritten, String)> = None;
        for function in functions {
            let Some((text, parameters)) = self.cpp_written_return_type(function, r)? else { continue };
            match &first {
                None => first = Some((CppWritten { text, site: site_at(r, function) }, parameters)),
                Some((seen, seen_parameters)) => if without_spaces(&seen.text) != without_spaces(&text) && *seen_parameters != parameters { return Ok(None); },
            }
        }
        Ok(first.map(|(written, _)| written))
    }

    /// The return type of a C or C++ function as its declaration writes it: `std::vector<const
    /// FieldDescriptor*>` (the index keeps only `vector`), and its parameter list without spaces.
    /// None for a constructor, `void`, `auto` or `decltype(…)`.
    fn cpp_written_return_type(&mut self, function: &Arc<KNode>, r: &ResolveRefIn) -> Res<Option<(String, String)>> {
        // A method a macro defines is read from its expansion.
        if let Some(head) = super::cpp_overloads::cpp_macro_method_head(function) {
            let Some((source, tree)) = super::cpp_overloads::cpp_macro_method_tree(head) else { return Ok(None) };
            let Some(declaration) = named_children(tree.root_node()).into_iter().find(|n| n.kind() == "function_definition") else { return Ok(None) };
            return Ok(cpp_declared_return(declaration, &source));
        }
        let Some(source) = self.read_file(&function.file_path) else { return Ok(None) };
        let site = site_at(r, function);
        let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((function.start_line - 1).max(0) as usize, function.start_column.max(0) as usize));
        let declaration = loop {
            match node.kind() {
                "function_definition" | "field_declaration" | "declaration" => break node,
                "template_declaration" => match named_children(node).into_iter().find(|n| matches!(n.kind(), "function_definition" | "field_declaration" | "declaration")) {
                    Some(inner) => break inner,
                    None => return Ok(None),
                },
                _ => {}
            }
            let Some(parent) = node.parent() else { return Ok(None) };
            node = parent;
        };
        Ok(cpp_declared_return(declaration, &source))
    }

    /// The type the C++ class `cls` declares its data member `field` with, as written in the class,
    /// or in the nearest class it derives from that declares it. A base that depends on the
    /// template's parameters is not looked in: a bare name is not looked up there.
    pub(super) fn cpp_data_member_written(&mut self, cls: &str, field: &str, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        self.cpp_member_written_in(cls, field, r, 0, &mut HashSet::new())
    }

    fn cpp_member_written_in(&mut self, cls: &str, field: &str, r: &ResolveRefIn, depth: usize, seen: &mut HashSet<String>) -> Res<Option<CppWritten>> {
        if !seen.insert(cls.to_string()) { return Ok(None); }
        let visible = self.namespace_visible_files(&r.file_path, "cpp")?;
        let decls: Vec<_> = self.nodes_by_qualified_name(cls)?.iter().filter(|n| cpp_class_node(n) && visible.contains(&n.file_path)).cloned().collect();
        // Classes that share a name each write their own: the first one that declares the member
        // decides, unless another writes it differently.
        let mut found: Option<CppWritten> = None;
        for decl in &decls {
            match self.cpp_field_types(decl, field, r)?.as_slice() {
                [] => {}
                [text] => match &found {
                    None => found = Some(CppWritten { text: text.clone(), site: site_at(r, decl) }),
                    Some(first) if without_spaces(&first.text) == without_spaces(text) => {}
                    Some(_) => return Ok(None),
                },
                // Declared more than once: not one of them is the member's.
                _ => return Ok(None),
            }
        }
        if found.is_some() { return Ok(found); }
        if depth >= 4 || !self.supertypes_complete { return Ok(None); }
        for decl in &decls {
            for base in self.supertype_nodes(&decl.id)? {
                if !cpp_class_node(&base) || (self.cpp_dependent_base(decl, &base)? && !self.cpp_using_declares(decl, &base, field)) { continue; }
                if let Some(member) = self.cpp_member_written_in(&base.qualified_name, field, r, depth + 1, seen)? { return Ok(Some(member)); }
            }
        }
        Ok(None)
    }

    /// The data member `name` of the class the caller's body is in, or of its gtest fixture: where
    /// the scan leaves the calling function, C++ looks a name up in its class and the classes that
    /// class derives from.
    pub(super) fn cpp_caller_member(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
        let owner = match self.cpp_member_owner(&caller)? {
            Some(owner) => Some(owner),
            None => self.cpp_gtest_fixture(&caller, r)?,
        };
        match owner {
            Some(owner) => self.cpp_data_member_written(&owner, name, r),
            None => Ok(None),
        }
    }

    /// The fixture class of a gtest `TEST_F(Fixture, Name)` / `TEST_P(Fixture, Name)` body, which is
    /// a method of a class derived from it.
    pub(super) fn cpp_gtest_fixture(&mut self, caller: &KNode, r: &ResolveRefIn) -> Res<Option<String>> {
        if !matches!(caller.name.as_str(), "TEST_F" | "TEST_P") || !matches!(caller.kind.as_str(), "function" | "method") { return Ok(None); }
        let Some(source) = self.read_file(&caller.file_path) else { return Ok(None) };
        let from = (caller.start_line - 1).max(0) as usize;
        let head = (from..from + 3).filter_map(|i| source.get(i).map(String::as_str)).collect::<Vec<_>>().join(" ");
        let Some(fixture) = re!(r"\bTEST_[FP]\s*\(\s*((?:::)?[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*,").captures(&head).map(|m| m[1].to_string()) else { return Ok(None) };
        Ok(self.cpp_type_owner(&fixture, r, 0, false)?.filter(|n| cpp_class_node(n)).map(|n| n.qualified_name.clone()))
    }

    /// Whether `written` is a template parameter's type where it is written (`T*`, `const T&`), or
    /// a library template of one (`std::shared_ptr<T>`, `std::vector<T>`), whose members are the
    /// parameter's. A class template of the project's is that class, whatever its arguments:
    /// `std::unique_ptr<ExternalTableReaderBase<Mode>>` holds one.
    fn cpp_written_template_dependent(&mut self, written: &CppWritten) -> Res<bool> {
        let Some(node) = self.node_by_id(&written.site.from_node_id)? else { return Ok(false) };
        let parameters = self.cpp_template_parameter_names(&node)?;
        Ok(!parameters.is_empty() && type_depends_on(&written.text, &parameters, 0))
    }

    /// Whether the type `raw`, which nothing the file sees declares, is written under a namespace
    /// the project declares nothing in (`absl::Status`, `grpc::ServerContext`), so that it is from
    /// outside the project. Not when its first name is a template parameter, a namespace alias or a
    /// namespace a macro opens. A lowercase name, and anything of `std`, is the library type check's:
    /// a trait or a member type may name one of the project's.
    pub(super) fn cpp_outside_qualified_type(&mut self, raw: &str, site: &ResolveRefIn) -> Res<bool> {
        let Some(parts) = type_segments(&cpp_plain_type(raw)) else { return Ok(false) };
        if parts.len() < 2 { return Ok(false); }
        let head = &parts[0];
        if head == "std" || !parts[parts.len() - 1].starts_with(|c: char| c.is_ascii_uppercase()) { return Ok(false); }
        if let Some(node) = self.node_by_id(&site.from_node_id)? {
            if self.cpp_template_parameter_names(&node)?.contains(head) { return Ok(false); }
        }
        if self.namespace_frames(&site.file_path)?.iter().any(|(_, _, path)| path.contains(head)) { return Ok(false); }
        Ok(!self.cpp_project_scope_names()?.contains(head))
    }

    /// The type of the initializer `value` of an `auto` local, as written where it is declared, with
    /// its aliases followed. None when it can't be read reliably, or what a template parameter
    /// decides: protobuf's `DownCastMessage<T>` returns a `const T&`, a `T*` or a
    /// `std::shared_ptr<T>`, by its argument.
    pub(super) fn cpp_initializer_written(&mut self, value: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let Some(written) = self.cpp_expression_written(value, source, r, 0)? else { return Ok(None) };
        let Some(followed) = self.cpp_follow_written_aliases(written, r)? else { return Ok(None) };
        if self.cpp_written_template_dependent(&followed)? { return Ok(None); }
        Ok(Some(followed))
    }

    /// A chained call `inner().method` whose inner call's return type is read as its declaration
    /// writes it: `method` is that class's, looked up as C++ looks the type up there, or no project
    /// method's. By the type's last name, protobuf's `factory->NewEnumGenerator(…)->Generate(…)` in
    /// the Java generator reached `csharp::EnumGenerator`.
    ///  - `Some(Some(_))`: the method of that class or of a class it derives from.
    ///  - `Some(None)`: the type is from outside the project, or a class that surely lacks the method.
    ///  - `None`: the type can't be read or placed; the last-name path decides, as before.
    pub(super) fn cpp_chain_on_written_type(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let Some(chain) = call_chain_re().captures(&r.reference_name).map(|m| (m[1].to_string(), m[2].to_string())) else { return Ok(None) };
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(None) };
        // The call expression the reference names: the one calling `method` on a call spelled `inner()`.
        let wanted: String = format!("{}()", chain.0).split_whitespace().collect();
        let mut at = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        let (receiver, arrow) = loop {
            if at.kind() == "call_expression" {
                let function = at.child_by_field_name("function").filter(|f| f.kind() == "field_expression");
                let field = function.and_then(|f| f.child_by_field_name("field"));
                if let (Some(function), Some(field)) = (function, field) {
                    if source.text()[field.byte_range()] == chain.1 {
                        let mut receiver = function.child_by_field_name("argument");
                        while let Some(inner) = receiver.filter(|n| n.kind() == "parenthesized_expression") {
                            receiver = match named_children(inner).as_slice() { [only] => Some(*only), _ => None };
                        }
                        if let Some(receiver) = receiver.filter(|n| n.kind() == "call_expression" && chain_spelling(&source.text()[n.byte_range()]) == wanted) {
                            let arrow = function.child_by_field_name("operator").is_some_and(|op| &source.text()[op.byte_range()] == "->");
                            break (receiver, arrow);
                        }
                    }
                }
            }
            match at.parent() { Some(parent) => at = parent, None => return Ok(None) }
        };
        let Some(written) = self.cpp_expression_written(receiver, &source, r, 1)? else { return Ok(None) };
        let Some(followed) = self.cpp_follow_written_aliases(written, r)? else { return Ok(None) };
        if self.cpp_written_template_dependent(&followed)? { return Ok(None); }
        // `->` on a pointer or a smart pointer reaches what it points at; `.` on a smart pointer or
        // an optional is its own member (`optional.value()`), no method of what it holds. The chain
        // is spelled with `.` for either, so the operator is the AST's.
        let object = match cpp_pointee(&followed.text).filter(|_| arrow) { Some(text) => CppWritten { text, site: followed.site }, None => followed };
        let plain = cpp_plain_type(&object.text);
        if let Some(parts) = type_segments(&plain) {
            let last = parts.last().map(String::as_str).unwrap_or("");
            if parts[0] == "std" || (parts[0] == "absl" && last.starts_with(|c: char| c.is_ascii_lowercase())) { return Ok(Some(None)); }
        }
        let Some(cls) = self.cpp_written_class(&object, r)? else {
            return Ok(if self.cpp_outside_qualified_type(&object.text, &object.site)? { Some(None) } else { None });
        };
        let methods = self.cpp_methods_named(&cls, &chain.1, r)?;
        let named = !methods.is_empty();
        // Of a member template's explicit specializations, the one the call's template arguments
        // name; none of them when it names another (cpp_specialization_pool).
        if let Some(method) = self.cpp_specialization_pool(methods, r).into_iter().next() {
            return Ok(Some(Some(KCand { node: method, confidence: 0.85, resolved_by: "instance-method" })));
        }
        if named { return Ok(Some(None)); }
        if self.supertypes_complete && !self.cpp_alias_template_owner(&cls, r)? && self.cpp_bases_known(&cls, 0, &mut HashSet::new())? { return Ok(Some(None)); }
        Ok(None)
    }

    /// The type of a C++ expression in `r`'s caller, as written where it is declared. None for
    /// anything else, or a name declared only outside the caller and its class.
    fn cpp_expression_written(&mut self, node: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn, depth: u32) -> Res<Option<CppWritten>> {
        if depth > MAX_DEPTH { return Ok(None); }
        match node.kind() {
            "parenthesized_expression" => match named_children(node).as_slice() {
                [only] => self.cpp_expression_written(*only, source, r, depth + 1),
                _ => Ok(None),
            },
            "this" => {
                let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
                // (A gtest `TEST_F` body's object is of a class derived from its fixture.)
                let owner = match self.cpp_member_owner(&caller)? { Some(owner) => Some(owner), None => self.cpp_gtest_fixture(&caller, r)? };
                Ok(owner.map(|owner| CppWritten { text: format!("::{owner}*"), site: r.clone() }))
            }
            "identifier" => self.cpp_name_written(node, source, r, depth),
            "pointer_expression" => {
                if node.child_by_field_name("operator").map(|o| o.kind()) != Some("*") { return Ok(None); }
                let Some(argument) = node.child_by_field_name("argument") else { return Ok(None) };
                let Some(pointer) = self.cpp_expression_written(argument, source, r, depth + 1)? else { return Ok(None) };
                let Some(pointer) = self.cpp_follow_written_aliases(pointer, r)? else { return Ok(None) };
                self.cpp_dereferenced(pointer, r)
            }
            // `statuses[i]`, `(*m)[key]`: the element a subscript reads.
            "subscript_expression" => {
                let Some(argument) = node.child_by_field_name("argument") else { return Ok(None) };
                let Some(container) = self.cpp_expression_written(argument, source, r, depth + 1)? else { return Ok(None) };
                self.cpp_subscript_element(container, r)
            }
            // `compact_->sub_compact_states`: a data member of the object's class; `it->second`
            // through a map's iterator: the member of the `std::pair`.
            "field_expression" => {
                let Some((object, field, None)) = self.cpp_member_object_of(node, source, r, depth)? else { return Ok(None) };
                if let Some(element) = self.cpp_pair_member(&object, &field, r)? { return Ok(Some(element)); }
                let Some(cls) = self.cpp_written_class(&object, r)? else { return Ok(None) };
                self.cpp_data_member_written(&cls.qualified_name, &field, r)
            }
            // `new Foo(…)`: a pointer to what it constructs.
            "new_expression" => {
                let Some(ty) = node.child_by_field_name("type") else { return Ok(None) };
                let text = source.text()[ty.byte_range()].split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(Some(CppWritten { text: format!("{text}*"), site: r.clone() }))
            }
            // `(Foo*)arg`: the type it casts to.
            "cast_expression" => {
                let Some(ty) = node.child_by_field_name("type") else { return Ok(None) };
                let text = source.text()[ty.byte_range()].split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(Some(CppWritten { text, site: r.clone() }))
            }
            "call_expression" => {
                let Some(function) = node.child_by_field_name("function") else { return Ok(None) };
                let callee = source.text()[function.byte_range()].trim();
                // `static_cast<Foo*>(…)`: the type it casts to.
                if let Some(cast) = re!(r"^(?:static|dynamic|const|reinterpret)_cast\s*<([\s\S]+)>$").captures(callee) {
                    return Ok(Some(CppWritten { text: cast[1].trim().to_string(), site: r.clone() }));
                }
                // `std::make_unique<Foo>(…)`: the smart pointer it makes.
                if let Some(made) = re!(r"^(?:::\s*)?(?:std|absl)\s*::\s*make_(unique|shared)\s*<([\s\S]+)>$").captures(callee) {
                    return Ok(Some(CppWritten { text: format!("std::{}_ptr<{}>", &made[1], made[2].trim()), site: r.clone() }));
                }
                // `std::unique_ptr<Iterator>(…)`: the smart pointer or optional it makes.
                if cpp_holder_arguments(&callee.split_whitespace().collect::<String>()).is_some() {
                    return Ok(Some(CppWritten { text: callee.to_string(), site: r.clone() }));
                }
                match function.kind() {
                    // `recv.method(…)`, `recv->method(…)`, `recv->method<T>(…)`: the method of the
                    // receiver's class.
                    "field_expression" => {
                        let Some((object, method, template_arguments)) = self.cpp_member_object_of(function, source, r, depth)? else { return Ok(None) };
                        if template_arguments.is_none() {
                            // `logs_.begin()`, `map.find(key)`: the iterator a library container hands
                            // out. (A container a name declares `const` keeps it: its declared type
                            // leaves the `const` out.)
                            let constant = function.child_by_field_name("argument").filter(|a| a.kind() == "identifier")
                                .is_some_and(|name| super::cpp_receivers::cpp_declared_const(name, source));
                            let container = if constant { CppWritten { text: format!("const {}", object.text), site: object.site.clone() } } else { object.clone() };
                            if let Some(iterator) = cpp_library_iterator_of(&container, &method) { return Ok(Some(iterator)); }
                            // `ws.front()`, `m.at(key)`, `p.get()`, `s.value()`: what it holds.
                            if let Some(element) = self.cpp_library_element_of(&object, &method, r)? { return Ok(Some(element)); }
                        }
                        let Some(cls) = self.cpp_written_class(&object, r)? else { return Ok(None) };
                        let methods = self.cpp_methods_named(&cls, &method, r)?;
                        let Some(arguments) = template_arguments else { return self.cpp_return_type_of(&methods, r) };
                        // Explicit template arguments call a template, never an overload that is none.
                        let mut templates = Vec::new();
                        for function in methods {
                            if !self.cpp_own_template_parameters(&function, r)?.is_empty() { templates.push(function); }
                        }
                        match self.cpp_member_template_return(&templates, &arguments, r)? {
                            Some(returned) => Ok(Some(returned)),
                            None => self.cpp_return_type_of(&templates, r),
                        }
                    }
                    // `f(…)`, `ns::f(…)`, `Class::f(…)`, `Foo<int>(…)`: the caller's class's own or
                    // inherited method first, then the function looked up from the caller's scope
                    // outwards, then the class it constructs.
                    "identifier" | "qualified_identifier" | "template_function" => {
                        // `create_iterator()` on a `std::function<Iterator*()> create_iterator`: what
                        // its function type returns.
                        if function.kind() == "identifier" {
                            if let Some(wrapper) = self.cpp_name_written(function, source, r, depth + 1)? {
                                if let Some(returned) = cpp_wrapper_return_type(&wrapper) { return Ok(Some(returned)); }
                            }
                        }
                        let spelled: String = strip_template_arguments(callee).split_whitespace().collect();
                        // `std::lower_bound(v.begin(), v.end(), key)`: the iterator it is given.
                        if re!(r"^(?:::)?std::(?:find|find_if|find_if_not|lower_bound|upper_bound|min_element|max_element|partition_point|adjacent_find|next|prev)$").is_match(&spelled) {
                            let first = node.child_by_field_name("arguments").and_then(|a| named_children(a).into_iter().find(|n| !n.kind().contains("comment")));
                            return match first { Some(first) => self.cpp_expression_written(first, source, r, depth + 1), None => Ok(None) };
                        }
                        if !re!(r"^(?:[A-Za-z_]\w*::)*[A-Za-z_]\w*$").is_match(&spelled) { return Ok(None); }
                        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
                        // `NewIterator(…)` in a method, or in a gtest body: what the method of its
                        // class, or of a class it derives from, returns, looked up as on `this`.
                        if !spelled.contains("::") {
                            if let Some(own) = self.cpp_implicit_this_method(&spelled, &caller, r)? { return self.cpp_return_type_of(&[own], r); }
                        }
                        let scopes: Vec<String> = caller.qualified_name.split("::").map(str::to_string).collect();
                        for i in (0..scopes.len()).rev() {
                            let qualified = if i == 0 { spelled.clone() } else { format!("{}::{spelled}", scopes[..i].join("::")) };
                            let functions: Vec<_> = self.nodes_by_qualified_name(&qualified)?.iter()
                                .filter(|n| matches!(n.kind.as_str(), "method" | "function") && matches!(n.language.as_str(), "c" | "cpp")).cloned().collect();
                            if !functions.is_empty() { return self.cpp_return_type_of(&functions, r); }
                        }
                        // `Foo(…)`, `Foo<int>(…)`: what it constructs, when C++ lookup finds the class.
                        let constructed = CppWritten { text: callee.split_whitespace().collect::<Vec<_>>().join(" "), site: r.clone() };
                        Ok(self.cpp_written_class(&constructed, r)?.map(|_| constructed))
                    }
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    /// The declared type of the name `node` in `r`'s caller, as `cpp_name_written` reads it.
    pub(super) fn cpp_written_name(&mut self, node: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        self.cpp_name_written(node, source, r, 0)
    }

    /// The type of a C++ expression in `r`'s caller, as `cpp_expression_written` reads it.
    pub(super) fn cpp_written_expression(&mut self, node: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        self.cpp_expression_written(node, source, r, 0)
    }

    /// What `x[i]` is when `x` has the type `container`: an array's element, what a pointer points
    /// at, a library map's mapped value, a sequence's element (a library container's, or one of
    /// PROJECT_SEQUENCES'), or what a class's `operator[]` returns. None for anything else: a
    /// `std::string`'s `char`, or a class whose `operator[]` the index doesn't hold.
    pub(super) fn cpp_subscript_element(&mut self, container: CppWritten, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let Some(followed) = self.cpp_follow_written_aliases(container, r)? else { return Ok(None) };
        if let Some(text) = cpp_array_element(&followed.text) { return Ok(Some(CppWritten { text, site: followed.site })); }
        if top_level_pointer(&followed.text) { return Ok(cpp_pointee(&followed.text).map(|text| CppWritten { text, site: followed.site })); }
        let plain = cpp_plain_type(&followed.text);
        let Some(parts) = type_segments(&plain) else { return Ok(None) };
        let last = parts.last().map(String::as_str).unwrap_or("");
        let library = parts[0] == "std" || parts[0] == "absl";
        if library && LIBRARY_MAPS.contains(&last) {
            return Ok(template_arguments(&plain).and_then(|a| a.into_iter().nth(1)).map(|value| CppWritten { text: cpp_plain_type(&value), site: followed.site }));
        }
        if (library && LIBRARY_SEQUENCES.contains(&last)) || PROJECT_SEQUENCES.contains(&last) { return self.cpp_range_element_of(followed, r, 0); }
        if library { return Ok(None); }
        let Some(cls) = self.cpp_written_class(&followed, r)? else { return Ok(None) };
        let subscript = self.cpp_methods_named(&cls, "operator[]", r)?;
        Ok(self.cpp_return_type_of(&subscript, r)?.map(|t| CppWritten { text: t.text.trim_end_matches('&').trim().to_string(), site: t.site }))
    }

    /// What dereferencing a value of the type `written` gives, for `*x` and `x->`: an array's first
    /// element, what a pointer points at or a holder holds (`cpp_pointee`), or the element a
    /// container's iterator points at (`cpp_iterator_target`): the `Node*` of `*nodes.begin()`
    /// over a `std::vector<Node*>`.
    pub(super) fn cpp_dereferenced(&mut self, written: CppWritten, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        if let Some(text) = cpp_array_element(&written.text) { return Ok(Some(CppWritten { text, site: written.site })); }
        if let Some(text) = cpp_pointee(&written.text) { return Ok(Some(CppWritten { text, site: written.site })); }
        self.cpp_iterator_target(written, r)
    }

    /// The element a container's iterator of the type `iterator` points at, read from the
    /// container's template arguments before the `iterator` alias would lose them: `Slice` for
    /// `std::deque<Slice>::const_iterator`, protobuf's `SourceCodeInfo_Location` for
    /// `RepeatedPtrField<SourceCodeInfo_Location>::iterator`. None for any other type, and for an
    /// element a class template's own `begin()` or `operator*` writes with its template parameter
    /// (googletest's `ParamGenerator<T>::iterator`), which the specialization decides. A
    /// `const_iterator`'s element is const (`const Slice`); a pointer element stays as it is.
    pub(super) fn cpp_iterator_target(&mut self, iterator: CppWritten, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let Some(container) = iterator_container(&iterator.text) else { return Ok(None) };
        let Some(element) = self.cpp_range_element_of(CppWritten { text: container, site: iterator.site.clone() }, r, 1)? else { return Ok(None) };
        if element.site.from_node_id != iterator.site.from_node_id && self.cpp_written_template_parameter(&element)? { return Ok(None); }
        let constant = re!(r"::\s*const_(?:reverse_)?iterator\s*$").is_match(&without_type_keywords(&iterator.text));
        if constant && !top_level_pointer(&element.text) && !re!(r"^\s*const\b").is_match(&element.text) {
            return Ok(Some(CppWritten { text: format!("const {}", element.text), site: element.site }));
        }
        Ok(Some(element))
    }

    /// The member `first` or `second` of a `std::pair` of the type `pair`, as a map's iterator
    /// points at one: its first or second element. None for any other member or type.
    fn cpp_pair_member(&mut self, pair: &CppWritten, member: &str, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let index = match member { "first" => 0, "second" => 1, _ => return Ok(None) };
        let Some(followed) = self.cpp_follow_written_aliases(pair.clone(), r)? else { return Ok(None) };
        let plain = cpp_plain_type(&followed.text);
        if top_level_pointer(&plain) || !type_segments(&plain).is_some_and(|parts| parts.len() == 2 && parts[0] == "std" && parts[1] == "pair") { return Ok(None); }
        Ok(template_arguments(&plain).and_then(|a| a.into_iter().nth(index)).map(|text| CppWritten { text: cpp_plain_type(&text), site: followed.site }))
    }

    /// What the member `member` of a library container or holder of the type `object` hands out,
    /// where the container's type is written: an element of a sequence for `front()`, `back()` and
    /// `at(i)` (of one of PROJECT_SEQUENCES' too), the value a map's `at(key)` maps to, the element
    /// of a `std::stack`'s `top()` or a `std::queue`'s `front()` and `back()`, the pointer a smart
    /// pointer's `get()` returns and the value an optional's or an `absl::StatusOr`'s `value()`
    /// returns. rocksdb's `cfds.front()->GetName()` over an `autovector<ColumnFamilyData*>` calls
    /// ColumnFamilyData's. None for any other member or type.
    fn cpp_library_element_of(&mut self, object: &CppWritten, member: &str, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        if top_level_pointer(&object.text) { return Ok(None); }
        let Some(parts) = type_segments(&without_type_keywords(&object.text)) else { return Ok(None) };
        let last = parts.last().map(String::as_str).unwrap_or("");
        // (Its own `const` and `&` aside: a `const` in a template argument, `std::shared_ptr<const Message>`, stays.)
        let text = re!(r"(?:\s*\bconst\b)?\s*&+\s*$").replace(re!(r"^\s*(?:typename\s+)?const\s+").replace(&object.text, "").as_ref(), "").trim().to_string();
        let library = parts[0] == "std" || parts[0] == "absl";
        let element = |text: Option<String>| text.map(|t| CppWritten { text: cpp_plain_type(&t), site: object.site.clone() });
        if matches!(member, "front" | "back" | "at") {
            if (library && LIBRARY_SEQUENCES.contains(&last)) || PROJECT_SEQUENCES.contains(&last) { return self.cpp_range_element_of(object.clone(), r, 0); }
            if member == "at" && library && LIBRARY_MAPS.contains(&last) { return Ok(element(template_arguments(&text).and_then(|a| a.into_iter().nth(1)))); }
        }
        if parts[0] == "std" && LIBRARY_ADAPTORS.contains(&last) && (member == "top" || (matches!(member, "front" | "back") && last == "queue")) {
            return Ok(element(template_arguments(&text).and_then(|a| a.into_iter().next())));
        }
        let spaced = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let arguments = cpp_holder_arguments(&spaced);
        let Some(held) = arguments.and_then(cpp_held_type) else { return Ok(None) };
        if held.contains('[') { return Ok(None); }
        // (What it holds keeps its `const`: `get()` of a `std::shared_ptr<const Message>` is a `const Message*`.)
        let held = if arguments.is_some_and(|a| re!(r"^\s*const\s").is_match(a)) { format!("const {held}") } else { held };
        let holder = parts.join("::");
        if member == "get" && matches!(holder.as_str(), "std::unique_ptr" | "std::shared_ptr" | "absl::NoDestructor") { return Ok(Some(CppWritten { text: format!("{held}*"), site: object.site.clone() })); }
        if matches!(member, "value" | "value_or") && matches!(holder.as_str(), "std::optional" | "absl::optional" | "absl::StatusOr") { return Ok(Some(CppWritten { text: held, site: object.site.clone() })); }
        Ok(None)
    }

    /// The object a C++ call through the operator `operator` (`.` or `->`) on the receiver
    /// expression `receiver` is made on, as `cpp_member_object_of` reads it.
    pub(super) fn cpp_call_object(&mut self, receiver: TsNode, operator: &str, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let Some(object) = self.cpp_expression_written(receiver, source, r, 1)? else { return Ok(None) };
        let Some(object) = self.cpp_follow_written_aliases(object, r)? else { return Ok(None) };
        match operator {
            "->" => self.cpp_dereferenced(object, r),
            "." if !top_level_pointer(&object.text) && cpp_array_element(&object.text).is_none() => Ok(Some(object)),
            _ => Ok(None),
        }
    }

    /// The type of the object a member expression `recv.name`, `recv->name` or `recv->name<…>`
    /// reads from: `recv`'s own type for `.`, what it points at, holds or iterates over for `->`
    /// (`cpp_dereferenced`), with the member's name and its explicit template arguments (`<…>` as
    /// written).
    fn cpp_member_object_of(&mut self, member: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn, depth: u32) -> Res<Option<CppMemberRead>> {
        let (Some(argument), Some(mut field)) = (member.child_by_field_name("argument"), member.child_by_field_name("field")) else { return Ok(None) };
        let mut template_arguments = None;
        if field.kind() == "template_method" {
            let (Some(name), Some(arguments)) = (field.child_by_field_name("name"), field.child_by_field_name("arguments")) else { return Ok(None) };
            template_arguments = Some(source.text()[arguments.byte_range()].to_string());
            field = name;
        }
        let name = source.text()[field.byte_range()].to_string();
        if !re!(r"^[A-Za-z_]\w*$").is_match(&name) { return Ok(None); }
        let operator = super::awaited::strip_ts_comments(&source.text()[argument.end_byte()..field.start_byte()]);
        let Some(object) = self.cpp_expression_written(argument, source, r, depth + 1)? else { return Ok(None) };
        let Some(object) = self.cpp_follow_written_aliases(object, r)? else { return Ok(None) };
        let object = match operator.trim() {
            "->" => match self.cpp_dereferenced(object, r)? {
                Some(object) => object,
                None => return Ok(None),
            },
            "." if !top_level_pointer(&object.text) && cpp_array_element(&object.text).is_none() => object,
            _ => return Ok(None),
        };
        Ok(Some((object, name, template_arguments)))
    }

    /// The names of the template parameters a C++ function template's own `template <…>` header
    /// declares, in order, without their defaults: `["Type"]` for protobuf's `template <typename
    /// Type> const Type& GetRaw(…)`. Empty for a function that is no template.
    pub(super) fn cpp_own_template_parameters(&mut self, function: &Arc<KNode>, r: &ResolveRefIn) -> Res<Vec<String>> {
        let Some(source) = self.read_file(&function.file_path) else { return Ok(Vec::new()) };
        let site = site_at(r, function);
        let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(Vec::new()) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((function.start_line - 1).max(0) as usize, function.start_column.max(0) as usize));
        loop {
            if node.kind() == "template_declaration" { break; }
            if matches!(node.kind(), "field_declaration_list" | "compound_statement" | "translation_unit" | "namespace_definition") { return Ok(Vec::new()); }
            let Some(parent) = node.parent() else { return Ok(Vec::new()) };
            node = parent;
        }
        let Some(list) = node.child_by_field_name("parameters") else { return Ok(Vec::new()) };
        Ok(named_children(list).into_iter().filter(|p| !p.kind().contains("comment")).map(|p| {
            let name = p.child_by_field_name("name").or_else(|| p.child_by_field_name("declarator"))
                .or_else(|| named_children(p).into_iter().rev().find(|c| matches!(c.kind(), "type_identifier" | "identifier")));
            name.map(|n| source.text()[n.byte_range()].trim_start_matches("...").trim().to_string()).unwrap_or_default()
        }).collect())
    }

    /// What a call of the C++ member template whose overloads are `templates`, with the explicit
    /// template arguments `arguments` (`<…>` as written), returns when every overload is declared
    /// to return the same one of its own template parameters, perhaps const, a pointer or a
    /// reference: that argument, so qualified, as written at the call. protobuf's `template
    /// <typename Type> const Type& GetRaw(…)` called as `GetRaw<RepeatedPtrField<int>>(…)` returns
    /// a `RepeatedPtrField<int>&`. None otherwise: then the return type is read as written.
    ///
    /// Port of upstream's `cppMemberTemplateReturn` (name-matcher.ts at 1753472a, #2556).
    fn cpp_member_template_return(&mut self, templates: &[Arc<KNode>], arguments: &str, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let mut slot: Option<(usize, String, String)> = None;
        for function in templates {
            let Some((written, _)) = self.cpp_written_return_type(function, r)? else { continue };
            let param = re!(r"(?:const|volatile)|[*&]").replace_all(&written, " ").trim().to_string();
            let index = if param.is_empty() { None } else { self.cpp_own_template_parameters(function, r)?.iter().position(|p| *p == param) };
            let Some(index) = index else { return Ok(None) };
            if slot.as_ref().is_some_and(|(seen, _, _)| *seen != index) { return Ok(None); }
            slot.get_or_insert((index, written, param));
        }
        let Some((index, written, param)) = slot else { return Ok(None) };
        let inner = arguments.trim().strip_prefix('<').and_then(|a| a.strip_suffix('>')).unwrap_or("");
        let Some(argument) = super::overloads_upstream::split_top_level(inner).into_iter().nth(index).map(|a| a.trim().to_string()).filter(|a| !a.is_empty()) else { return Ok(None) };
        let Ok(word) = shared_regex(&format!(r"\b{}\b", regex::escape(&param))) else { return Ok(None) };
        Ok(Some(CppWritten { text: word.replace(&written, regex::NoExpand(&argument)).into_owned(), site: r.clone() }))
    }

    /// The declared type of the name `node` in `r`'s caller: a parameter or local (an `auto` local
    /// through its initializer), an enclosing loop's variable, else a data member of its class.
    fn cpp_name_written(&mut self, node: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn, depth: u32) -> Res<Option<CppWritten>> {
        let name = &source.text()[node.byte_range()];
        let mut at = node;
        while let Some(parent) = at.parent() {
            if let Some(bound) = self.cpp_structured_binding_written(parent, at, name, source, r)? { return Ok(bound); }
            if let Some(raw) = cpp_binding_type(parent, at, name, source) {
                // (An array, `Status statuses[3]`, is written `Status[]`.)
                if raw.is_empty() || cpp_array_split(&raw).is_none_or(|(base, dimensions)| dimensions > 0 && is_cpp_auto(base)) { return Ok(None); }
                if !is_cpp_auto(&raw) { return Ok(Some(CppWritten { text: raw, site: r.clone() })); }
                // `const auto& blob_files = vstorage->GetBlobFiles();`: what its initializer is.
                // (Read with a depth budget of its own: a chain of calls on the local, whose own
                // initializer is a chain, gets no shorter for how it was reached.)
                let Some(value) = binding_initializer(parent, at, name, source) else { return Ok(None) };
                return self.cpp_expression_written(value, source, r, 0);
            }
            if parent.kind() == "for_range_loop" && parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id())
                && parent.child_by_field_name("declarator").is_some_and(|d| cpp_declarator_name(d, source) == name) {
                let (Some(ty), Some(declarator)) = (parent.child_by_field_name("type"), parent.child_by_field_name("declarator")) else { return Ok(None) };
                let raw = cpp_ast_declared_type(ty, declarator, source);
                if !is_cpp_auto(&raw) { return Ok(Some(CppWritten { text: raw, site: r.clone() })); }
                return self.cpp_range_for_element(parent, source, r, depth + 1);
            }
            at = parent;
        }
        // Declared nowhere in the calling function (`versions_` in db_impl_write.cc, whose class is
        // declared in db_impl.h): a data member of its class.
        self.cpp_caller_member(name, r)
    }

    /// The type a structured binding in `parent` gives `name` where `at` is: the element at the
    /// name's place of the pair or tuple bound (cppStructuredBindingType). That is an element of
    /// the range a range-for over the binding iterates, when `at` is the loop's body (rocksdb's
    /// `for (auto& [cfh, iter] : cfh_iter_pairs_)`, a map's key and value), or the initializer of
    /// a declaration before `at` (`auto [status, iter] = Open();`).
    ///  - `None`: `parent` binds no such name there.
    ///  - `Some(None)`: it does, but the type can't be read.
    pub(super) fn cpp_structured_binding_written(&mut self, parent: TsNode, at: TsNode, name: &str, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Res<Option<Option<CppWritten>>> {
        let bound = if parent.kind() == "for_range_loop" {
            if !parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id()) { return Ok(None); }
            let Some(place) = parent.child_by_field_name("declarator").and_then(|d| binding_place(d, name, source)) else { return Ok(None) };
            self.cpp_range_for_element(parent, source, r, 0)?.map(|value| (value, place))
        } else {
            let children = match parent.kind() {
                "compound_statement" | "declaration_list" | "translation_unit" => named_children(parent),
                _ => statement_initializer(parent).into_iter().collect(),
            };
            let mut found = None;
            for declaration in children.into_iter().rev().filter(|n| n.kind() == "declaration" && n.end_byte() <= at.start_byte()) {
                let mut cursor = declaration.walk();
                let declarators: Vec<_> = declaration.children_by_field_name("declarator", &mut cursor).collect();
                if let Some((declarator, place)) = declarators.iter().find_map(|d| binding_place(*d, name, source).map(|p| (*d, p))) {
                    found = Some((declarator, place));
                    break;
                }
                // A nearer declaration of the name of any other kind is the name's.
                if declarators.iter().any(|d| cpp_declarator_name(*d, source) == name) { return Ok(None); }
            }
            let Some((declarator, place)) = found else { return Ok(None) };
            match declarator.child_by_field_name("value").filter(|_| declarator.kind() == "init_declarator").filter(|v| !matches!(v.kind(), "initializer_list" | "argument_list")) {
                Some(value) => self.cpp_initializer_written(value, source, r)?.map(|value| (value, place)),
                None => None,
            }
        };
        let Some((value, place)) = bound else { return Ok(Some(None)) };
        Ok(Some(self.cpp_binding_elements(value, r)?.and_then(|elements| elements.into_iter().nth(place))))
    }

    /// What a structured binding of a value of the type `value` names, in order: a `std::pair`'s or
    /// `std::tuple`'s elements. None for any other type.
    fn cpp_binding_elements(&mut self, value: CppWritten, r: &ResolveRefIn) -> Res<Option<Vec<CppWritten>>> {
        let Some(followed) = self.cpp_follow_written_aliases(value, r)? else { return Ok(None) };
        let plain = cpp_plain_type(&followed.text);
        if !type_segments(&plain).is_some_and(|parts| parts.len() == 2 && parts[0] == "std" && (parts[1] == "pair" || parts[1] == "tuple")) { return Ok(None); }
        if top_level_pointer(&plain) { return Ok(None); }
        Ok(template_arguments(&plain).map(|arguments| arguments.into_iter().map(|text| CppWritten { text: cpp_plain_type(&text), site: followed.site.clone() }).collect()))
    }

    /// The element type of a range of the type `range`, as written: a container's first template
    /// argument, or, for one of the project's classes, what its `begin()` returns, dereferenced.
    fn cpp_range_element_of(&mut self, range: CppWritten, r: &ResolveRefIn, hops: u32) -> Res<Option<CppWritten>> {
        if hops > 2 { return Ok(None); }
        let Some(range) = self.cpp_follow_written_aliases(range, r)? else { return Ok(None) };
        if let Some(text) = cpp_array_element(&range.text) { return Ok(Some(CppWritten { text, site: range.site })); }
        if top_level_pointer(&range.text) { return Ok(None); }
        let Some(parts) = type_segments(&range.text) else { return Ok(None) };
        let last = parts.last().map(String::as_str).unwrap_or("");
        // A map's elements pair its key with its value.
        if LIBRARY_MAPS.contains(&last) && (parts[0] == "std" || parts[0] == "absl") {
            let Some([key, value]) = template_arguments(&range.text).and_then(|a| <[String; 2]>::try_from(a.into_iter().take(2).collect::<Vec<_>>()).ok()) else { return Ok(None) };
            return Ok(Some(CppWritten { text: format!("std::pair<const {}, {}>", cpp_plain_type(&key), cpp_plain_type(&value)), site: range.site }));
        }
        let library = LIBRARY_SEQUENCES.contains(&last);
        if library || PROJECT_SEQUENCES.contains(&last) {
            // A project type of a standard container's name, written without `std::` or `absl::`,
            // is that type, not the container.
            if library && parts[0] != "std" && parts[0] != "absl" && self.nodes_by_name(last)?.iter().any(|n| matches!(n.language.as_str(), "c" | "cpp")
                && matches!(n.kind.as_str(), "class" | "struct" | "union" | "type_alias")) { return Ok(None); }
            let Some(first) = template_arguments(&range.text).and_then(|a| a.into_iter().next()) else { return Ok(None) };
            return Ok(Some(CppWritten { text: cpp_plain_type(&first), site: range.site }));
        }
        let Some(cls) = self.cpp_written_class(&range, r)? else { return Ok(None) };
        let begin = self.cpp_methods_named(&cls, "begin", r)?;
        let Some(begin) = self.cpp_return_type_of(&begin, r)? else { return Ok(None) };
        self.cpp_iterator_element(begin, r, hops)
    }

    /// What dereferencing an iterator of the type `iterator` gives, as written.
    fn cpp_iterator_element(&mut self, iterator: CppWritten, r: &ResolveRefIn, hops: u32) -> Res<Option<CppWritten>> {
        let Some(iterator) = self.cpp_follow_written_aliases(iterator, r)? else { return Ok(None) };
        if top_level_pointer(&iterator.text) { return Ok(cpp_pointee(&iterator.text).map(|text| CppWritten { text, site: iterator.site })); }
        // `std::vector<Foo*>::const_iterator`: the container's element.
        if let Some(container) = iterator_container(&iterator.text) {
            return self.cpp_range_element_of(CppWritten { text: container, site: iterator.site.clone() }, r, hops + 1);
        }
        let Some(cls) = self.cpp_written_class(&iterator, r)? else { return Ok(None) };
        let star = self.cpp_methods_named(&cls, "operator*", r)?;
        Ok(self.cpp_return_type_of(&star, r)?.map(|t| CppWritten { text: t.text.trim_end_matches('&').trim().to_string(), site: t.site }))
    }

    /// Whether every base the C++ class `cls` writes, and every base of those, is one a base edge
    /// links to a class, or a standard-library type, which declares no project method. A base the
    /// index doesn't link may declare any method: protobuf's `FlatAllocator` derives from a
    /// `decltype(…)` that names `FlatAllocatorImpl`.
    pub(super) fn cpp_bases_known(&mut self, cls: &KNode, depth: usize, seen: &mut HashSet<String>) -> Res<bool> {
        if !seen.insert(cls.qualified_name.clone()) { return Ok(true); }
        let decls: Vec<_> = self.nodes_by_qualified_name(&cls.qualified_name)?.iter().filter(|n| cpp_class_node(n)).cloned().collect();
        for decl in decls {
            let bases = self.supertype_nodes(&decl.id)?;
            let written = self.cpp_base_specifiers(&decl).into_iter().filter(|s| !re!(r"^(?:::)?std::").is_match(s.trim())).count();
            // (`cpp_method_of` looks no deeper than this.)
            if if depth >= 4 { bases.len() + written > 0 } else { written > bases.len() } { return Ok(false); }
            for base in bases {
                if !cpp_class_node(&base) || !self.cpp_bases_known(&base, depth + 1, seen)? { return Ok(false); }
            }
        }
        Ok(true)
    }
}

/// The iterator the member `member` of a standard library or abseil container of the type `object`
/// returns, where the container's type is written: `std::deque<LogWriterNumber>::iterator` for
/// rocksdb's `logs_.begin()`, whose `->` reaches the container's element (`cpp_iterator_target`).
/// A const container's is its `const_iterator`. None for any other member or type.
fn cpp_library_iterator_of(object: &CppWritten, member: &str) -> Option<CppWritten> {
    let kind = ITERATOR_MEMBERS.iter().find(|(m, _)| *m == member)?.1;
    if top_level_pointer(&object.text) { return None; }
    let container = without_type_keywords(&object.text);
    let parts = type_segments(&container)?;
    let last = parts.last()?.as_str();
    if parts[0] != "std" && parts[0] != "absl" { return None; }
    let constant = !kind.starts_with("const_") && re!(r"\bconst\b").is_match(&super::cpp_implicit_this::strip_template_arguments(&object.text));
    let kind = if constant { format!("const_{kind}") } else { kind.to_string() };
    (LIBRARY_SEQUENCES.contains(&last) || LIBRARY_MAPS.contains(&last)).then(|| CppWritten { text: format!("{container}::{kind}"), site: object.site.clone() })
}

/// Where `name` is in the structured binding `declarator` writes (`[cfh, iter]` in `auto& [cfh,
/// iter]`, `auto [a, b] = …`), when it writes one.
fn binding_place(mut declarator: TsNode, name: &str, source: &SourceFile) -> Option<usize> {
    while matches!(declarator.kind(), "init_declarator" | "reference_declarator" | "pointer_declarator" | "parenthesized_declarator") {
        declarator = declarator.child_by_field_name("declarator").or_else(|| declarator.named_child(0))?;
    }
    if declarator.kind() != "structured_binding_declarator" { return None; }
    named_children(declarator).iter().position(|n| &source.text()[n.byte_range()] == name)
}

/// What calling a function wrapper of the written type `wrapper` returns, as written: what the
/// function type that is its template argument returns (`Iterator*` for
/// `std::function<Iterator*()>`). None for any other type.
fn cpp_wrapper_return_type(wrapper: &CppWritten) -> Option<CppWritten> {
    let plain = cpp_plain_type(&wrapper.text);
    let plain = plain.trim_end_matches(['&', ' ']);
    if !re!(r"^(?:::\s*)?(?:std\s*::\s*(?:function|move_only_function|copyable_function)|absl\s*::\s*(?:FunctionRef|AnyInvocable))\s*<").is_match(plain) { return None; }
    let signature = template_arguments(plain)?.into_iter().next()?;
    let mut depth = 0i32;
    for (i, c) in signature.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            '(' if depth == 0 => {
                let returned = signature[..i].trim();
                return (!returned.is_empty()).then(|| CppWritten { text: returned.to_string(), site: wrapper.site.clone() });
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{cpp_pointee, template_arguments, cpp_plain_type};

    #[test]
    fn written_type_pieces() {
        assert_eq!(template_arguments("std::vector<std::shared_ptr<Foo>>&").unwrap(), vec!["std::shared_ptr<Foo>"]);
        assert_eq!(template_arguments("autovector<ColumnFamilyData*, 8>").unwrap(), vec!["ColumnFamilyData*", "8"]);
        assert_eq!(cpp_plain_type("const FieldDescriptor* const absl_nonnull"), "FieldDescriptor*");
        assert_eq!(cpp_pointee("ColumnFamilySet*").as_deref(), Some("ColumnFamilySet"));
        assert_eq!(cpp_pointee("std::unique_ptr<VersionSet>&").as_deref(), Some("VersionSet"));
        assert_eq!(cpp_pointee("std::vector<Foo*>"), None);
    }
}

/// The return type a C or C++ function `declaration` writes, and its parameter list without
/// spaces (cpp_written_return_type).
fn cpp_declared_return(declaration: tree_sitter::Node, source: &SourceFile) -> Option<(String, String)> {
    let (Some(ty), Some(declarator)) = (declaration.child_by_field_name("type"), declaration.child_by_field_name("declarator")) else { return None };
    let function_declarator = declared_function(declarator)?;
    let raw = cpp_plain_type(&cpp_ast_declared_type(ty, declarator, source));
    if raw.is_empty() || raw == "void" || is_cpp_auto(&raw) || raw.contains("decltype") { return None; }
    let parameters = function_declarator.child_by_field_name("parameters").map(|p| without_spaces(&source.text()[p.byte_range()])).unwrap_or_default();
    Some((raw, parameters))
}
