//! Calls, instantiations, static member references, inheritance and type references.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn bare_call_name(&self, node: Node<'t>) -> Option<String> {
        if node.kind() == "selector" {
            let mut cursor = node.walk();
            let has_arg_part = node.named_children(&mut cursor).any(|c| c.kind() == "argument_part");
            if !has_arg_part {
                return None;
            }
            let prev = node.prev_named_sibling()?;
            if prev.kind() == "identifier" {
                return Some(self.text(prev).to_string());
            }
            if prev.kind() == "selector" {
                let mut pc = prev.walk();
                let accessor = prev.named_children(&mut pc).find(|c| {
                    matches!(
                        c.kind(),
                        "unconditional_assignable_selector" | "conditional_assignable_selector"
                    )
                });
                if let Some(accessor) = accessor {
                    let mut ac = accessor.walk();
                    let method_id = accessor.named_children(&mut ac).find(|c| c.kind() == "identifier");
                    if let Some(method_id) = method_id {
                        let accessor_prev = prev.prev_named_sibling();
                        if let Some(ap) = accessor_prev {
                            if ap.kind() == "identifier" {
                                return Some(format!("{}.{}", self.text(ap), self.text(method_id)));
                            }
                            // Chained static-factory: the receiver is itself
                            // a call — re-encode `<inner>().<method>` when
                            // the chain starts capitalized (#750).
                            if ap.kind() == "selector" {
                                let mut apc = ap.walk();
                                if ap.named_children(&mut apc).any(|c| c.kind() == "argument_part") {
                                    if let Some(inner) = self.callee_of_arg_part(ap) {
                                        if crate::textutil::starts_upper_re().is_match(&inner) {
                                            return Some(format!("{}().{}", inner, self.text(method_id)));
                                        }
                                    }
                                }
                            }
                        }
                        return Some(self.text(method_id).to_string());
                    }
                }
            }
            // super.method() / this.method(): prev is a bare accessor.
            if matches!(
                prev.kind(),
                "unconditional_assignable_selector" | "conditional_assignable_selector"
            ) {
                let mut pc = prev.walk();
                let id = prev.named_children(&mut pc).find(|c| c.kind() == "identifier");
                if let Some(id) = id {
                    return Some(self.text(id).to_string());
                }
            }
            return None;
        }

        // new_expression arm — DEAD in practice (the INSTANTIATION branch
        // fires first in the body walker); ported for fidelity.
        if node.kind() == "new_expression" {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "type_identifier")
                .map(|t| self.text(t).to_string());
            return found;
        }

        // const EdgeInsets.all(8.0) — const constructor call.
        if node.kind() == "const_object_expression" {
            let mut c1 = node.walk();
            let type_id = node.named_children(&mut c1).find(|c| c.kind() == "type_identifier");
            let mut c2 = node.walk();
            let name_id = node.named_children(&mut c2).find(|c| c.kind() == "identifier");
            return match (type_id, name_id) {
                (Some(t), Some(n)) => Some(format!("{}.{}", self.text(t), self.text(n))),
                (Some(t), None) => Some(self.text(t).to_string()),
                _ => None,
            };
        }

        None
    }

    /// dartCalleeOfArgPart (dart.ts:100-116).
    pub(super) fn callee_of_arg_part(&self, arg_part: Node<'t>) -> Option<String> {
        let prev = arg_part.prev_named_sibling()?;
        if prev.kind() == "identifier" {
            return Some(self.text(prev).to_string());
        }
        if prev.kind() == "selector" {
            let mut pc = prev.walk();
            let accessor = prev.named_children(&mut pc).find(|c| {
                matches!(
                    c.kind(),
                    "unconditional_assignable_selector" | "conditional_assignable_selector"
                )
            });
            let method_id = accessor.and_then(|a| {
                let mut ac = a.walk();
                let found = a.named_children(&mut ac).find(|c| c.kind() == "identifier");
                found
            });
            if let Some(method_id) = method_id {
                let accessor_prev = prev.prev_named_sibling();
                if let Some(ap) = accessor_prev {
                    if ap.kind() == "identifier" {
                        return Some(format!("{}.{}", self.text(ap), self.text(method_id)));
                    }
                }
                return Some(self.text(method_id).to_string());
            }
        }
        None
    }

    pub(super) fn extract_instantiation(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let from_row = self.top_row();
        let ctor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };
        let class_name = crate::textutil::strip_generic_and_qualifier(self.text(ctor));
        if class_name.is_empty() {
            return;
        }
        self.push_ref_at(from_row, &class_name, "instantiates", node);
    }

    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let owner_row = self.top_row();
        if node.kind() != "selector" {
            return;
        }
        let mut cursor = node.walk();
        if node.named_children(&mut cursor).any(|c| c.kind() == "argument_part") {
            return;
        }
        let Some(prev) = node.prev_named_sibling() else { return };
        if prev.kind() == "identifier" && crate::textutil::capitalized_re().is_match(self.text(prev)) {
            let name = self.text(prev).to_string();
            // NO callee-of-call skip — `ConfigT.load()` double-emits
            // (references + calls). Position = the IDENTIFIER (receiver).
            self.push_ref_at(owner_row, &name, "references", prev);
        }
    }
}
