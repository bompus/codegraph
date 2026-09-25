//! Type declarations: classes, structs/unions, enums and their members, typedefs and aliases.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// extractClass for cpp class_specifier (skipBodilessClass, #1093).
    pub(super) fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        let Some(body) = node.child_by_field_name("body") else { return };
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: self.visibility_of(node),
            ..Extra::default()
        };
        let Some(row) = self.create_node("class", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "class", name });
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
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: if self.variant == Variant::Cpp { self.visibility_of(node) } else { None },
            ..Extra::default()
        };
        let Some(row) = self.create_node(kind, &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind, name });
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
            docstring: preceding_docstring(node, self.src),
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
            self.stack.push(Scope { row, kind, name });
            let type_child = node
                .child_by_field_name("type")
                .or_else(|| self.find_child_by_kind(node, "struct_specifier"))
                .or_else(|| self.find_child_by_kind(node, "union_specifier"));
            if let Some(tc) = type_child {
                self.extract_inheritance(tc, row);
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

        self.create_node("type_alias", &name, node, Extra { docstring, ..Extra::default() });
        false
    }

    pub(super) fn find_child_by_kind(&self, node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        named_kids(node)
            .find(|c| c.kind() == kind)
    }
}
