//! Type declarations: classes, structs/unions, enums and their members, typedefs and aliases.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    fn class_like_docstring(&self, node: Node<'t>) -> Option<String> {
        preceding_docstring(defining_declaration(node).unwrap_or(node), self.src)
    }

    /// extractClass for cpp class_specifier (skipBodilessClass, #1093).
    pub(super) fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        let Some(body) = node.child_by_field_name("body") else { return };
        let name = self.extract_name(node);
        let scoped = self.written_scope_qualified_name(node, &name);
        let extra = Extra {
            docstring: self.class_like_docstring(node),
            visibility: self.visibility_of(node),
            end: self.brace_body_end(body),
            qualified_name: scoped.clone(),
            ..Extra::default()
        };
        let Some(row) = self.create_node("class", &name, node, extra) else { return };
        if let Some(q) = scoped {
            self.written_scope_rows.insert(row, q);
        }
        self.extract_inheritance(node, row);
        self.push_member_type_refs(node, row, &name);
        let scope = Scope { row, kind: "class", name };
        self.open_class_scope(&scope, body);
        self.stack.push(scope);
        for c in named_kids(body) {
            self.visit_node(c);
        }
        self.stack.pop();
    }

    /// Extract a struct-like declaration while preserving its semantic kind.
    pub(super) fn extract_aggregate(&mut self, node: Node<'t>, kind: &'static str) {
        stack_guard!();
        let Some(body) = node.child_by_field_name("body") else { return };
        let name = self.extract_name(node);
        let scoped = self.written_scope_qualified_name(node, &name);
        let extra = Extra {
            docstring: self.class_like_docstring(node),
            visibility: if self.variant == Variant::Cpp { self.visibility_of(node) } else { None },
            end: self.brace_body_end(body),
            qualified_name: scoped.clone(),
            ..Extra::default()
        };
        let Some(row) = self.create_node(kind, &name, node, extra) else { return };
        if let Some(q) = scoped {
            self.written_scope_rows.insert(row, q);
        }
        self.extract_inheritance(node, row);
        self.push_member_type_refs(node, row, &name);
        let scope = Scope { row, kind, name };
        self.open_class_scope(&scope, body);
        self.stack.push(scope);
        for c in named_kids(body) {
            self.visit_node(c);
        }
        self.stack.pop();
    }

    pub(super) fn extract_enum(&mut self, node: Node<'t>) {
        stack_guard!();
        let Some(body) = node.child_by_field_name("body") else { return };
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: self.class_like_docstring(node),
            visibility: if self.variant == Variant::Cpp { self.visibility_of(node) } else { None },
            ..Extra::default()
        };
        let Some(row) = self.create_node("enum", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "enum", name });
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else { continue };
            if child.kind() == "enumerator" {
                self.extract_enum_members(child);
            } else {
                self.visit_node(child);
            }
        }
        self.stack.pop();
    }

    /// extractEnumMembers: enumerator's `name` field (C/C++ always has one;
    /// the TS fallbacks for other grammars are unreachable here).
    pub(super) fn extract_enum_members(&mut self, node: Node<'t>) {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.text(name_node).to_string();
            self.create_node("enum_member", &name, node, Extra::default());
        }
    }

    /// extractTypeAlias for type_definition / alias_declaration. Returns true
    /// when children were consumed (typedef struct/enum bodies).
    pub(super) fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let docstring = preceding_docstring(node, self.src);

        // resolveTypeAliasKind: first child that is an enum/struct specifier
        // WITH a body decides the node kind (anon inner specifier takes the
        // typedef's name).
        let mut resolved: Option<&'static str> = None;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() == "enum_specifier" && child.child_by_field_name("body").is_some() {
                resolved = Some("enum");
                break;
            }
            if child.kind() == "struct_specifier" && child.child_by_field_name("body").is_some() {
                resolved = Some("struct");
                break;
            }
            if child.kind() == "union_specifier" && child.child_by_field_name("body").is_some() {
                resolved = Some("union");
                break;
            }
        }

        if matches!(resolved, Some("struct") | Some("union")) {
            let kind = resolved.unwrap();
            let Some(row) = self.create_node(
                kind,
                &name,
                node,
                Extra { docstring, ..Extra::default() },
            ) else {
                return true;
            };
            let type_child = node
                .child_by_field_name("type")
                .or_else(|| self.find_child_by_kind(node, "struct_specifier"))
                .or_else(|| self.find_child_by_kind(node, "union_specifier"));
            if let Some(tc) = type_child {
                self.extract_inheritance(tc, row);
                self.push_member_type_refs(tc, row, &name);
            }
            self.stack.push(Scope { row, kind, name });
            if let Some(tc) = type_child {
                let body = tc.child_by_field_name("body").unwrap_or(tc);
                for c in named_kids(body) {
                    self.visit_node(c);
                }
            }
            self.stack.pop();
            return true;
        }

        if resolved == Some("enum") {
            let Some(row) = self.create_node(
                "enum",
                &name,
                node,
                Extra { docstring, ..Extra::default() },
            ) else {
                return true;
            };
            self.stack.push(Scope { row, kind: "enum", name });
            if let Some(inner) = self.find_child_by_kind(node, "enum_specifier") {
                self.extract_inheritance(inner, row);
                if let Some(body) = inner.child_by_field_name("body") {
                    for i in 0..body.named_child_count() {
                        let Some(child) = body.named_child(i) else { continue };
                        if child.kind() == "enumerator" {
                            self.extract_enum_members(child);
                        } else {
                            self.visit_node(child);
                        }
                    }
                }
            }
            self.stack.pop();
            return true;
        }

        if let Some(row) = self.create_node("type_alias", &name, node, Extra { docstring, ..Extra::default() }) {
            let names = self.alias_type_names(node, &name);
            self.push_type_refs(names, row);
        }
        false
    }

    pub(super) fn find_child_by_kind(&self, node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        named_kids(node)
            .find(|c| c.kind() == kind)
    }
}

impl<'t> Walker<'t> {
    // --- the types members and aliases are declared with --------------------

    /// extractCMemberTypeRefs (tree-sitter.ts): a class, struct or union
    /// references the types its members are declared with, as the class:
    /// members mint no nodes, and impact never climbs from a member to its
    /// container.
    fn push_member_type_refs(&mut self, specifier: Node<'t>, row: u32, own_name: &str) {
        if is_unnamed_member_type(specifier) {
            return;
        }
        let names = self.member_type_names(specifier, own_name);
        self.push_type_refs(names, row);
    }

    fn push_type_refs(&mut self, names: Vec<(String, Node<'t>)>, row: u32) {
        let references = crate::buffers::edge_kind_index("references").unwrap();
        for (name, node) in names {
            self.push_ref_at(row, &name, references, node);
        }
    }

    /// cMemberTypeNames (languages/c-cpp.ts): the type names a class, struct
    /// or union's members are declared with, in source order.
    fn member_type_names(&self, specifier: Node<'t>, own_name: &str) -> Vec<(String, Node<'t>)> {
        let mut names = Vec::new();
        let mut own = HashSet::from([own_name.to_string()]);
        if let Some(tag) = specifier.child_by_field_name("name") {
            own.insert(strip_cpp_template_args(self.text(tag)));
        }
        if let Some(body) = specifier.child_by_field_name("body") {
            self.collect_member_type_names(body, &own, &mut names);
        }
        names
    }

    /// cAliasTypeNames (languages/c-cpp.ts): the type names an alias is
    /// declared with.
    fn alias_type_names(&self, alias: Node<'t>, own_name: &str) -> Vec<(String, Node<'t>)> {
        let mut names = Vec::new();
        let own = HashSet::from([own_name.to_string()]);
        if let Some(ty) = alias.child_by_field_name("type") {
            self.collect_type_names(ty, &own, &mut names);
        }
        let mut cursor = alias.walk();
        for declarator in alias.children_by_field_name("declarator", &mut cursor) {
            self.collect_declarator_type_names(declarator, &own, &mut names);
        }
        names
    }

    fn collect_member_type_names(&self, list: Node<'t>, own: &HashSet<String>, names: &mut Vec<(String, Node<'t>)>) {
        stack_guard!();
        for child in (0..list.named_child_count()).filter_map(|i| list.named_child(i)) {
            if is_member_conditional(child.kind()) {
                self.collect_member_type_names(child, own, names);
                continue;
            }
            if child.kind() != "field_declaration" {
                continue;
            }
            let Some(ty) = child.child_by_field_name("type") else { continue };
            let mut cursor = child.walk();
            let declarators: Vec<Node<'t>> =
                child.children_by_field_name("declarator", &mut cursor).collect();
            let unnamed_body = if is_class_like_specifier(ty.kind())
                && ty.child_by_field_name("name").is_none()
            {
                ty.child_by_field_name("body")
            } else {
                None
            };
            if let Some(body) = unnamed_body {
                self.collect_member_type_names(body, own, names);
            } else if !declarators.is_empty() {
                self.collect_type_names(ty, own, names);
            }
            for declarator in declarators {
                if !declares_member_function(declarator) {
                    self.collect_declarator_type_names(declarator, own, names);
                }
            }
        }
    }

    /// collectDeclaratorTypeNames (languages/c-cpp.ts): those of a function
    /// pointer's parameters.
    fn collect_declarator_type_names(&self, declarator: Node<'t>, own: &HashSet<String>, names: &mut Vec<(String, Node<'t>)>) {
        stack_guard!();
        if declarator.kind() == "function_declarator" {
            if let Some(parameters) = declarator.child_by_field_name("parameters") {
                self.collect_type_names(parameters, own, names);
            }
        }
        if !declarator.kind().ends_with("declarator") {
            return;
        }
        if let Some(inner) = inner_declarator(declarator) {
            self.collect_declarator_type_names(inner, own, names);
        }
    }

    /// collectCTypeNames (languages/c-cpp.ts).
    fn collect_type_names(&self, node: Node<'t>, own: &HashSet<String>, names: &mut Vec<(String, Node<'t>)>) {
        stack_guard!();
        if matches!(node.kind(), "decltype" | "placeholder_type_specifier") {
            return;
        }
        if matches!(node.kind(), "type_identifier" | "qualified_identifier" | "template_type") {
            let name = strip_cpp_template_args(self.text(node)).trim().to_string();
            if !name.is_empty() && !own.contains(&name) && !c_standard_type_name_re().is_match(&name) {
                names.push((name, node));
            }
            self.collect_template_arguments(node, own, names);
            return;
        }
        if is_class_like_specifier(node.kind()) {
            if let Some(tag) = node.child_by_field_name("name") {
                self.collect_type_names(tag, own, names);
                return;
            }
        }
        for c in (0..node.named_child_count()).filter_map(|i| node.named_child(i)) {
            self.collect_type_names(c, own, names);
        }
    }

    /// collectTemplateArguments (languages/c-cpp.ts).
    fn collect_template_arguments(&self, node: Node<'t>, own: &HashSet<String>, names: &mut Vec<(String, Node<'t>)>) {
        stack_guard!();
        for c in (0..node.named_child_count()).filter_map(|i| node.named_child(i)) {
            match c.kind() {
                "template_argument_list" => self.collect_type_names(c, own, names),
                "qualified_identifier" | "template_type" => {
                    self.collect_template_arguments(c, own, names)
                }
                _ => {}
            }
        }
    }
}

/// C_STANDARD_TYPE_NAME (languages/c-cpp.ts).
fn c_standard_type_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:(?:::\s*)?std\s*::|(?:_Bool|_Complex|wchar_t|char(?:8|16|32)_t|u?intmax_t|u?int_(?:least|fast)\d+_t|max_align_t|nullptr_t)$)")
            .unwrap()
    })
}

/// CLASS_LIKE_SPECIFIERS (languages/c-cpp.ts).
fn is_class_like_specifier(kind: &str) -> bool {
    matches!(
        kind,
        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
    )
}

/// isUnnamedMemberType (languages/c-cpp.ts): its members are its holder's.
fn is_unnamed_member_type(specifier: Node<'_>) -> bool {
    specifier.child_by_field_name("name").is_none()
        && specifier
            .parent()
            .map(|p| p.kind() == "field_declaration")
            .unwrap_or(false)
}

/// MEMBER_CONDITIONALS (languages/c-cpp.ts).
fn is_member_conditional(kind: &str) -> bool {
    matches!(
        kind,
        "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif" | "preproc_elifdef"
    )
}

/// innerDeclarator (languages/c-cpp.ts).
fn inner_declarator(declarator: Node<'_>) -> Option<Node<'_>> {
    declarator
        .child_by_field_name("declarator")
        .or_else(|| declarator.named_child(0))
}

/// declaresMemberFunction (languages/c-cpp.ts).
fn declares_member_function(declarator: Node<'_>) -> bool {
    let mut d = Some(declarator);
    while let Some(n) =
        d.filter(|n| matches!(n.kind(), "pointer_declarator" | "reference_declarator"))
    {
        d = inner_declarator(n);
    }
    d.filter(|n| n.kind() == "function_declarator")
        .map(|n| n.child_by_field_name("declarator").map(|i| i.kind()) != Some("parenthesized_declarator"))
        .unwrap_or(false)
}
