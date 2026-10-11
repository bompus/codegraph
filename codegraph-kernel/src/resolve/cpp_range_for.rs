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
use super::cpp_receivers::{cpp_ast_declared_type, cpp_binding_type, cpp_declarator_name, cpp_held_type};
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
/// argument. A map's elements are key and value pairs, which no call this resolves is made on.
const LIBRARY_SEQUENCES: &[&str] = &[
    "vector", "list", "forward_list", "deque", "set", "multiset", "unordered_set", "unordered_multiset", "array", "span",
    "initializer_list", "Span", "InlinedVector", "FixedArray", "flat_hash_set", "node_hash_set", "btree_set", "btree_multiset",
];
/// Widely used project containers of the same kind: protobuf's repeated fields, rocksdb's `autovector`.
const PROJECT_SEQUENCES: &[&str] = &["RepeatedField", "RepeatedPtrField", "autovector"];
const MAX_DEPTH: u32 = 4;

/// A written type without nullability annotations (`const FieldDescriptor* absl_nonnull`) or
/// cv-qualifiers, which are no part of the name it is looked up by.
pub(super) fn cpp_plain_type(text: &str) -> String {
    let text = re!(r"\b(?:absl_nonnull|absl_nullable|absl_nullability_unknown|_Nonnull|_Nullable|_Null_unspecified|const|volatile)\b").replace_all(text, " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A type written `auto`, `auto&`, `const auto*`.
pub(super) fn is_cpp_auto(raw: &str) -> bool {
    type_segments(raw).is_some_and(|parts| parts.len() == 1 && parts[0] == "auto")
}

/// Whether `text` is a pointer: a `*` outside its template arguments.
fn top_level_pointer(text: &str) -> bool {
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
    let held = re!(r"^(?:::)?std::(?:unique_ptr|shared_ptr|optional)\s*<(.*)>$").captures(&plain)?;
    cpp_held_type(&held[1])
}

/// Whether a declarator declares a function (`f()`, `*f()`, `&f()`), not a variable.
fn declares_function(mut declarator: TsNode) -> bool {
    loop {
        match declarator.kind() {
            "function_declarator" => return true,
            "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
                let Some(inner) = declarator.child_by_field_name("declarator").or_else(|| declarator.named_child(0)) else { return false };
                declarator = inner;
            }
            _ => return false,
        }
    }
}

/// The initializer of `name`'s declaration in the block `parent`, before `at`: `vstorage->
/// GetBlobFiles()` in `const auto& blob_files = vstorage->GetBlobFiles();`. The same declarations
/// `cpp_binding_type` reads.
fn binding_initializer<'t>(parent: TsNode<'t>, at: TsNode<'t>, name: &str, source: &SourceFile) -> Option<TsNode<'t>> {
    if !matches!(parent.kind(), "compound_statement" | "declaration_list" | "translation_unit") { return None; }
    for declaration in named_children(parent).into_iter().rev().filter(|n| n.kind() == "declaration" && n.end_byte() <= at.start_byte()) {
        let mut cursor = declaration.walk();
        let declarators: Vec<_> = declaration.children_by_field_name("declarator", &mut cursor).collect();
        if let Some(declarator) = declarators.into_iter().find(|d| cpp_declarator_name(*d, source) == name) {
            if declarator.kind() != "init_declarator" { return None; }
            return declarator.child_by_field_name("value").filter(|v| !matches!(v.kind(), "initializer_list" | "argument_list"));
        }
    }
    None
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
        let mut at = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1));
        while let Some(parent) = at.parent() {
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
    fn cpp_written_template_parameter(&mut self, written: &CppWritten) -> Res<bool> {
        let Some(parts) = type_segments(&written.text) else { return Ok(false) };
        let Some(node) = self.node_by_id(&written.site.from_node_id)? else { return Ok(false) };
        self.cpp_template_parameter(&node, &parts[0], &written.site)
    }

    /// `written` once the aliases it is written with are followed, each from its own declaration:
    /// rocksdb's `const BlobFiles&` is `std::vector<std::shared_ptr<BlobFileMetaData>>`. None when an
    /// alias on the way can't be read: a member of an aliased type, `typename`, `decltype`.
    pub(super) fn cpp_follow_written_aliases(&mut self, written: CppWritten, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
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
        let mut owner = if parts.len() == 1 { self.cpp_class_scope_type(&parts[0], &written.site)? } else { None };
        if owner.is_none() {
            let plain = cpp_plain_type(&written.text);
            let primary = plain.split('<').next().unwrap_or(&plain).trim_end_matches('&').trim();
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
    /// where it is declared.
    fn cpp_return_type_of(&mut self, functions: &[Arc<KNode>], r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        for function in functions {
            if let Some(text) = self.cpp_written_return_type(function, r)? {
                return Ok(Some(CppWritten { text, site: site_at(r, function) }));
            }
        }
        Ok(None)
    }

    /// The return type of a C or C++ function as its declaration writes it: `std::vector<const
    /// FieldDescriptor*>` (the index keeps only `vector`). None for a constructor, `void`, `auto`
    /// or `decltype(…)`.
    fn cpp_written_return_type(&mut self, function: &Arc<KNode>, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(source) = self.read_file(&function.file_path) else { return Ok(None) };
        let site = site_at(r, function);
        let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((function.start_line - 1).max(0) as usize, function.start_column.max(0) as usize + 1));
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
        let (Some(ty), Some(declarator)) = (declaration.child_by_field_name("type"), declaration.child_by_field_name("declarator")) else { return Ok(None) };
        if !declares_function(declarator) { return Ok(None); }
        let raw = cpp_plain_type(&cpp_ast_declared_type(ty, declarator, &source));
        if raw.is_empty() || raw == "void" || is_cpp_auto(&raw) || raw.contains("decltype") { return Ok(None); }
        Ok(Some(raw))
    }

    /// The type the C++ class `cls` declares its data member `field` with, as written in the class.
    fn cpp_data_member_written(&mut self, cls: &str, field: &str, r: &ResolveRefIn) -> Res<Option<CppWritten>> {
        let visible = self.namespace_visible_files(&r.file_path, "cpp")?;
        let decls: Vec<_> = self.nodes_by_qualified_name(cls)?.iter().filter(|n| cpp_class_node(n) && visible.contains(&n.file_path)).cloned().collect();
        for decl in decls {
            if let Some(text) = self.cpp_field_type(&decl, field, r)? {
                return Ok(Some(CppWritten { text, site: site_at(r, &decl) }));
            }
        }
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
                Ok(self.cpp_member_owner(&caller)?.map(|owner| CppWritten { text: format!("::{owner}*"), site: r.clone() }))
            }
            "identifier" => self.cpp_name_written(node, source, r, depth),
            "pointer_expression" => {
                if node.child_by_field_name("operator").map(|o| o.kind()) != Some("*") { return Ok(None); }
                let Some(argument) = node.child_by_field_name("argument") else { return Ok(None) };
                let Some(pointer) = self.cpp_expression_written(argument, source, r, depth + 1)? else { return Ok(None) };
                let Some(pointer) = self.cpp_follow_written_aliases(pointer, r)? else { return Ok(None) };
                Ok(cpp_pointee(&pointer.text).map(|text| CppWritten { text, site: pointer.site }))
            }
            // `compact_->sub_compact_states`: a data member of the object's class.
            "field_expression" => {
                let Some((cls, field)) = self.cpp_member_of_expression(node, source, r, depth)? else { return Ok(None) };
                self.cpp_data_member_written(&cls.qualified_name, &field, r)
            }
            "call_expression" => {
                let Some(function) = node.child_by_field_name("function") else { return Ok(None) };
                match function.kind() {
                    // `recv.method(…)`, `recv->method(…)`: the method of the receiver's class.
                    "field_expression" => {
                        let Some((cls, method)) = self.cpp_member_of_expression(function, source, r, depth)? else { return Ok(None) };
                        let methods = self.cpp_methods_named(&cls, &method, r)?;
                        self.cpp_return_type_of(&methods, r)
                    }
                    // `f(…)`, `ns::f(…)`, `Class::f(…)`: looked up from the caller's scope outwards.
                    "identifier" | "qualified_identifier" => {
                        let spelled: String = source.text()[function.byte_range()].split_whitespace().collect();
                        if !re!(r"^(?:[A-Za-z_]\w*::)*[A-Za-z_]\w*$").is_match(&spelled) { return Ok(None); }
                        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
                        let scopes: Vec<String> = caller.qualified_name.split("::").map(str::to_string).collect();
                        for i in (0..scopes.len()).rev() {
                            let qualified = if i == 0 { spelled.clone() } else { format!("{}::{spelled}", scopes[..i].join("::")) };
                            let functions: Vec<_> = self.nodes_by_qualified_name(&qualified)?.iter()
                                .filter(|n| matches!(n.kind.as_str(), "method" | "function") && matches!(n.language.as_str(), "c" | "cpp")).cloned().collect();
                            if !functions.is_empty() { return self.cpp_return_type_of(&functions, r); }
                        }
                        Ok(None)
                    }
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    /// The class of the object a member expression `x.m` or `x->m` names a member of, and `m`.
    fn cpp_member_of_expression(&mut self, member: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn, depth: u32) -> Res<Option<(Arc<KNode>, String)>> {
        let (Some(argument), Some(field)) = (member.child_by_field_name("argument"), member.child_by_field_name("field")) else { return Ok(None) };
        let name = source.text()[field.byte_range()].to_string();
        if !re!(r"^[A-Za-z_]\w*$").is_match(&name) { return Ok(None); }
        let operator = super::awaited::strip_ts_comments(&source.text()[argument.end_byte()..field.start_byte()]);
        let Some(object) = self.cpp_expression_written(argument, source, r, depth + 1)? else { return Ok(None) };
        let Some(object) = self.cpp_follow_written_aliases(object, r)? else { return Ok(None) };
        let object = match operator.trim() {
            "->" => match cpp_pointee(&object.text) {
                Some(text) => CppWritten { text, site: object.site },
                None => return Ok(None),
            },
            "." if !top_level_pointer(&object.text) => object,
            _ => return Ok(None),
        };
        Ok(self.cpp_written_class(&object, r)?.map(|cls| (cls, name)))
    }

    /// The declared type of the name `node` in `r`'s caller: a parameter or local (an `auto` local
    /// through its initializer), an enclosing loop's variable, else a data member of its class.
    fn cpp_name_written(&mut self, node: TsNode, source: &Rc<SourceFile>, r: &ResolveRefIn, depth: u32) -> Res<Option<CppWritten>> {
        let name = &source.text()[node.byte_range()];
        let mut at = node;
        while let Some(parent) = at.parent() {
            if let Some(raw) = cpp_binding_type(parent, at, name, source) {
                if raw.is_empty() || raw.contains('[') { return Ok(None); }
                if !is_cpp_auto(&raw) { return Ok(Some(CppWritten { text: raw, site: r.clone() })); }
                // `const auto& blob_files = vstorage->GetBlobFiles();`: what its initializer is.
                let Some(value) = binding_initializer(parent, at, name, source) else { return Ok(None) };
                return self.cpp_expression_written(value, source, r, depth + 1);
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
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
        let Some(owner) = self.cpp_member_owner(&caller)? else { return Ok(None) };
        self.cpp_data_member_written(&owner, name, r)
    }

    /// The element type of a range of the type `range`, as written: a container's first template
    /// argument, or, for one of the project's classes, what its `begin()` returns, dereferenced.
    fn cpp_range_element_of(&mut self, range: CppWritten, r: &ResolveRefIn, hops: u32) -> Res<Option<CppWritten>> {
        if hops > 2 { return Ok(None); }
        let Some(range) = self.cpp_follow_written_aliases(range, r)? else { return Ok(None) };
        if top_level_pointer(&range.text) { return Ok(None); }
        let Some(parts) = type_segments(&range.text) else { return Ok(None) };
        let last = parts.last().map(String::as_str).unwrap_or("");
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
        let bare = re!(r"\b(?:typename|const)\b").replace_all(&iterator.text, " ").replace('&', " ");
        if let Some(nested) = re!(r"^(.*>)\s*::\s*(?:const_)?(?:reverse_)?iterator$").captures(bare.trim()) {
            return self.cpp_range_element_of(CppWritten { text: nested[1].trim().to_string(), site: iterator.site.clone() }, r, hops + 1);
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
