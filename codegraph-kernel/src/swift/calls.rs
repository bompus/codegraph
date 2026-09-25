//! Calls, instantiations, static member references, inheritance and type references.

use super::*;

impl<'t> Walker<'t> {
    /// extractCall — swift rides the generic member branch (navigation) and
    /// the raw-text else; the full matrix is in the checklist.
    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let caller = self.top_row();
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let Some(func) = func else { return };
        let mut callee_name = String::new();

        if func.kind() == "navigation_expression" {
            // property = property/field fields (null) → namedChild(1), with
            // the navigation_suffix simple_identifier unwrap.
            let property = func
                .child_by_field_name("property")
                .or_else(|| func.child_by_field_name("field"))
                .or_else(|| {
                    let c1 = func.named_child(1);
                    match c1 {
                        Some(c) if c.kind() == "navigation_suffix" => (0..c.named_child_count())
                            .filter_map(|i| c.named_child(i))
                            .find(|g| g.kind() == "simple_identifier")
                            .or(Some(c)),
                        other => other,
                    }
                });
            if let Some(property) = property {
                let method_name = self.text(property);
                let receiver = func
                    .child_by_field_name("object")
                    .or_else(|| func.child_by_field_name("operand"))
                    .or_else(|| func.child_by_field_name("argument"))
                    .or_else(|| func.named_child(0));
                if let Some(r) = receiver {
                    if is_literal_receiver(r.kind()) {
                        return; // `"lit".upper()` / `5.times()` — nothing
                    }
                }
                let recv_ident = receiver.filter(|r| {
                    matches!(r.kind(), "identifier" | "simple_identifier" | "field_identifier")
                });
                if let Some(r) = recv_ident {
                    let receiver_name = self.text(r);
                    if matches!(receiver_name, "self" | "this" | "cls" | "super") {
                        callee_name = method_name.to_string();
                    } else {
                        callee_name = format!("{receiver_name}.{method_name}");
                    }
                } else if receiver.map(|r| r.kind() == "call_expression").unwrap_or(false) {
                    // #750 swift re-encode: innerNav = receiver.namedChild(0),
                    // ws-stripped; capitalized chains only.
                    let inner = receiver.unwrap().named_child(0);
                    let inner_callee =
                        inner.map(|n| crate::textutil::strip_js_ws(self.text(n))).unwrap_or_default();
                    let reencode = inner_callee
                        .as_bytes()
                        .first()
                        .map(|b| b.is_ascii_uppercase())
                        .unwrap_or(false);
                    callee_name = if reencode {
                        format!("{inner_callee}().{method_name}")
                    } else {
                        method_name.to_string()
                    };
                } else {
                    // self_expression / super_expression / inner nav /
                    // postfix / multi_line_string_literal → bare method name.
                    callee_name = method_name.to_string();
                }
            }
        } else {
            // Raw func text: bare `helper`, `Foo` (constructor = plain call),
            // `arr` (subscript reads!), `m[i]`, `defer`, `.make`, tuple
            // callees (conv-regex below), array-literal callees (bump delta 8).
            callee_name = self.text(func).to_string();
        }

        if !callee_name.is_empty() {
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
            self.push_ref_at(caller, &callee_name, edge_kind_index("calls").unwrap(), node);
        }
    }

    /// extractStaticMemberRef — swift's navigation_expression value reads,
    /// body walker + walkAttrArgs only.
    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if node.kind() != "navigation_expression" {
            return;
        }
        if self.stack.is_empty() {
            return;
        }
        let owner = self.top_row();
        // Skip the callee nav of a call.
        if let Some(parent) = node.parent() {
            if parent.kind() == "call_expression" {
                let callee = parent
                    .child_by_field_name("function")
                    .or_else(|| parent.child_by_field_name("method"))
                    .or_else(|| parent.named_child(0));
                if let Some(callee) = callee {
                    if callee.start_byte() == node.start_byte() {
                        return;
                    }
                }
            }
        }
        let recv = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("expression"))
            .or_else(|| node.child_by_field_name("scope"))
            .or_else(|| node.named_child(0));
        let Some(recv) = recv else { return };
        if matches!(
            recv.kind(),
            "identifier" | "type_identifier" | "simple_identifier" | "name" | "scoped_type_identifier"
        ) {
            let text = self.text(recv);
            if capitalized_re().is_match(text) {
                self.push_ref_at(owner, text, edge_kind_index("references").unwrap(), recv);
            }
        }
    }

    /// extractInheritance — the swift inheritance_specifier case: FIRST
    /// type_identifier of each specifier's user_type, everything as `extends`
    /// (conformances included; `: Module.Base` takes `Module`).
    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let extends_kind = edge_kind_index("extends").unwrap();
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "inheritance_specifier" {
                continue;
            }
            let user_type = (0..child.named_child_count())
                .filter_map(|j| child.named_child(j))
                .find(|c| c.kind() == "user_type");
            let Some(user_type) = user_type else { continue };
            let type_id = (0..user_type.named_child_count())
                .filter_map(|j| user_type.named_child(j))
                .find(|c| c.kind() == "type_identifier");
            let Some(type_id) = type_id else { continue };
            let name = self.text(type_id).to_string();
            self.push_ref_at(class_row, &name, extends_kind, type_id);
        }
    }

    /// extractTypeAnnotations — generic path: the 'parameter' field NEVER
    /// resolves (zero param refs), 'return_type' DOES; the direct
    /// type_annotation find is null for functions.
    pub(super) fn extract_type_annotations(&mut self, node: Node<'t>, from_row: u32) {
        if let Some(params) = node.child_by_field_name("parameter") {
            // Unreachable (field never resolves) — mirrored for shape.
            self.extract_type_refs_from_subtree(params, from_row);
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.extract_type_refs_from_subtree(ret, from_row);
        }
        let ta = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = ta {
            self.extract_type_refs_from_subtree(ta, from_row);
        }
    }
}
