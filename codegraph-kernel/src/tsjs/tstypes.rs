//! TypeScript type aliases, their members, tuple contracts and function-typed properties.

use super::*;
use super::extractors::*;

impl<'t> Walker<'t> {
    /// Returns skipChildren (always false on the TS path — the alias value is
    /// still traversed by the dispatcher).
    pub(super) fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let extra = Extra {
            docstring: crate::docstring::preceding_docstring(node, self.src),
            is_exported: Some(self.is_exported(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("type_alias", &name, node, extra) else {
            return false;
        };
        if let Some(value) = node.child_by_field_name("value") {
            self.extract_type_refs_from_subtree(value, row);
            self.extract_ts_type_alias_members(value, row, &name);
            self.extract_ts_tuple_contract_names(value, row, &name);
        }
        false
    }

    pub(super) fn extract_ts_type_alias_members(&mut self, value: Node<'t>, alias_row: u32, alias_name: &str) {
        let mut object_types: Vec<Node> = Vec::new();
        if value.kind() == "object_type" {
            object_types.push(value);
        } else if value.kind() == "intersection_type" {
            for i in 0..value.named_child_count() {
                if let Some(op) = value.named_child(i) {
                    if op.kind() == "object_type" {
                        object_types.push(op);
                    }
                }
            }
        } else {
            return;
        }

        self.stack.push(Scope { row: alias_row, kind: "type_alias", name: alias_name.to_string() });
        for obj_type in object_types {
            for i in 0..obj_type.named_child_count() {
                let Some(child) = obj_type.named_child(i) else { continue };
                if !matches!(child.kind(), "property_signature" | "method_signature") {
                    continue;
                }
                let Some(name_node) = child.child_by_field_name("name") else { continue };
                let member_name = self.text(name_node).to_string();
                if member_name.is_empty() {
                    continue;
                }
                let member_kind: &'static str = if child.kind() == "method_signature"
                    || self.is_ts_function_typed_property(child)
                {
                    "method"
                } else {
                    "property"
                };
                let extra = Extra {
                    docstring: crate::docstring::preceding_docstring(child, self.src),
                    signature: Some(self.text(child).to_string()),
                    qualified_name: Some(format!("{alias_name}::{member_name}")),
                    ..Extra::default()
                };
                self.create_node(member_kind, &member_name, child, extra);
                self.extract_type_annotations(child, alias_row);
            }
        }
        self.stack.pop();
    }

    pub(super) fn extract_ts_tuple_contract_names(&mut self, value: Node<'t>, alias_row: u32, alias_name: &str) {
        let mut tuples: Vec<Node> = Vec::new();
        fn collect<'t>(n: Node<'t>, depth: u32, out: &mut Vec<Node<'t>>) {
            stack_guard!();
            if depth > 6 {
                return;
            }
            if n.kind() == "tuple_type" {
                out.push(n);
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i) {
                    collect(c, depth + 1, out);
                }
            }
        }
        collect(value, 0, &mut tuples);
        if tuples.is_empty() {
            return;
        }

        self.stack.push(Scope { row: alias_row, kind: "type_alias", name: alias_name.to_string() });
        for tuple in tuples {
            for i in 0..tuple.named_child_count() {
                let Some(entry) = tuple.named_child(i) else { continue };
                if entry.kind() != "generic_type" {
                    continue;
                }
                let Some(type_args) = entry.child_by_field_name("type_arguments") else { continue };
                for j in 0..type_args.named_child_count() {
                    let Some(arg) = type_args.named_child(j) else { continue };
                    if arg.kind() != "literal_type" {
                        continue;
                    }
                    let Some(str_node) = arg.named_child(0) else { continue };
                    if str_node.kind() != "string" {
                        continue;
                    }
                    let name = util::object_key_name(self.text(str_node).trim());
                    if !util::ident_dollar().is_match(&name) {
                        continue;
                    }
                    let collapsed = collapse_ws(self.text(entry));
                    let signature = util::slice_utf16(collapsed.trim(), 120);
                    let extra = Extra {
                        signature: Some(signature),
                        qualified_name: Some(format!("{alias_name}::{name}")),
                        ..Extra::default()
                    };
                    self.create_node("method", &name, entry, extra);
                }
            }
        }
        self.stack.pop();
    }

    pub(super) fn is_ts_function_typed_property(&self, property_signature: Node) -> bool {
        let Some(type_anno) = property_signature.child_by_field_name("type") else {
            return false;
        };
        for i in 0..type_anno.named_child_count() {
            if let Some(inner) = type_anno.named_child(i) {
                if inner.kind() == "function_type" {
                    return true;
                }
            }
        }
        false
    }
}
