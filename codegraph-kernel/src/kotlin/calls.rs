//! Calls, instantiations, static member references, inheritance and type references.

use super::*;

impl<'t> Walker<'t> {
    /// extractCall — the kotlin paths: navigation member branch (+ the #750
    /// re-encode) and the raw-text else (paren-then-lambda / glued-invoke
    /// garbage preserved).
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
                        return; // `"literal".uppercase()` / `5.toString()`
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
                } else if let Some(recv) = receiver.filter(|r| r.kind() == "call_expression") {
                    // #750 kotlin re-encode: innerNav = receiver.namedChild(0)
                    // (NOT a function field), ws-stripped, /^[A-Z]/ gate.
                    let inner = recv.named_child(0);
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
                    // this_expression / super_expression / 2-hop nav /
                    // postfix `!!` / parenthesized → bare method name.
                    callee_name = method_name.to_string();
                }
            }
        } else {
            // Raw func text: bare `helper`, constructor `WidgetK` (NO
            // instantiates ever), backticked names verbatim, the
            // paren-then-lambda `trailing()` and glued-invoke chains
            // byte-for-byte.
            callee_name = self.text(func).to_string();
        }

        if !callee_name.is_empty() {
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
            self.push_ref_at(caller, &callee_name, edge_kind_index("calls").unwrap(), node);
        }
    }

    /// extractStaticMemberRef — navigation_expression value reads, body
    /// walker only (assignment WRITES parse as directly_assignable_expression
    /// — not a member-access kind — and emit nothing).
    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if node.kind() != "navigation_expression" {
            return;
        }
        if self.stack.is_empty() {
            return;
        }
        let owner = self.top_row();
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

    /// extractInheritance — delegation_specifier: user_type ?? its
    /// constructor_invocation's user_type → FIRST type_identifier → ONE
    /// `extends` ref at the typeId (interfaces ride extends too; qualified
    /// supertypes take the FIRST segment — `com`; `by`-delegation emits
    /// NOTHING).
    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let extends_kind = edge_kind_index("extends").unwrap();
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "delegation_specifier" {
                continue;
            }
            let user_type = (0..child.named_child_count())
                .filter_map(|j| child.named_child(j))
                .find(|c| c.kind() == "user_type");
            let ctor_inv = (0..child.named_child_count())
                .filter_map(|j| child.named_child(j))
                .find(|c| c.kind() == "constructor_invocation");
            let target = user_type.or(ctor_inv);
            let Some(target) = target else { continue };
            let type_id: Node = if target.kind() == "user_type" {
                (0..target.named_child_count())
                    .filter_map(|j| target.named_child(j))
                    .find(|c| c.kind() == "type_identifier")
                    .unwrap_or(target)
            } else {
                // constructor_invocation → its user_type → first type_identifier
                let ut = (0..target.named_child_count())
                    .filter_map(|j| target.named_child(j))
                    .find(|c| c.kind() == "user_type");
                match ut {
                    Some(ut) => (0..ut.named_child_count())
                        .filter_map(|j| ut.named_child(j))
                        .find(|c| c.kind() == "type_identifier")
                        .unwrap_or(ut),
                    None => target,
                }
            };
            let name = self.text(type_id).to_string();
            self.push_ref_at(class_row, &name, extends_kind, type_id);
        }
    }
}
