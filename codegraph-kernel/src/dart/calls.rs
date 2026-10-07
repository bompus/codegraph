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
            // Past comments: `box.grow /* by */ (3)` calls `box.grow`.
            let prev = prev_named(node)?;
            return self.callee_name(prev);
        }

        // A generic call the grammar read as two comparisons
        // (`ref.read<Repo>(p)`), named at its `<`, where a parsed call's
        // argument part starts.
        if node.kind() == "relational_operator" {
            let (callee, _) = self.misparsed_generic_call(node)?;
            return self.callee_name(callee);
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

        // `=> BlocProvider<CounterCubit>.value(…)` — a constructor called
        // with type arguments, as most expressions parse it. The type is the
        // LAST type_identifier: an import prefix (`p.X<T>.named`) is one too.
        if node.kind() == "constructor_invocation" {
            let mut c1 = node.walk();
            let type_id = node.named_children(&mut c1).filter(|c| c.kind() == "type_identifier").last();
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
        let prev = prev_named(arg_part)?;
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
                let accessor_prev = prev_named(prev);
                if let Some(ap) = accessor_prev {
                    if ap.kind() == "identifier" {
                        return Some(format!("{}.{}", self.text(ap), self.text(method_id)));
                    }
                    if let Some(type_name) = self.type_arguments_receiver(ap) {
                        return Some(format!("{}.{}", type_name, self.text(method_id)));
                    }
                }
                return Some(self.text(method_id).to_string());
            }
        }
        None
    }

    pub(super) fn extract_instantiation(&mut self, node: Node<'t>) {
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
        self.push_ref_at(from_row, &class_name, crate::buffers::EDGE_INSTANTIATES, node);
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
        let Some(prev) = self.receiver_of(node) else { return };
        if prev.kind() == "identifier" && crate::textutil::capitalized_re().is_match(self.text(prev)) {
            // `Map` in `x.read<Map<K, V>>(y)` parsed as comparisons is a type
            // argument, which the body walker references as a type.
            if let Some(before) = prev.prev_named_sibling() {
                if before.kind() == "relational_operator" && self.misparsed_generic_call(before).is_some() {
                    return;
                }
            }
            let name = self.text(prev).to_string();
            // NO callee-of-call skip — `ConfigT.load()` double-emits
            // (references + calls). Position = the IDENTIFIER (receiver).
            self.push_ref_at(owner_row, &name, crate::buffers::EDGE_REFERENCES, prev);
        }
    }

    pub(super) fn receiver_of(&self, selector: Node<'t>) -> Option<Node<'t>> {
        let prev = prev_named(selector);
        let body = prev
            .filter(|p| p.kind() == "function_expression")
            .and_then(|p| p.named_child(p.named_child_count().checked_sub(1)?));
        match body {
            Some(body) if body.kind() == "function_expression_body" => {
                body.named_child(body.named_child_count().checked_sub(1)?)
            }
            _ => prev,
        }
    }

    pub(super) fn callee_name(&self, prev: Node<'t>) -> Option<String> {
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
                    let accessor_prev = self.receiver_of(prev);
                    if let Some(ap) = accessor_prev {
                        if ap.kind() == "identifier" {
                            return Some(format!("{}.{}", self.text(ap), self.text(method_id)));
                        }
                        // A constructor called with type arguments names
                        // its type all the same (`BlocProvider<T>.value`).
                        if let Some(type_name) = self.type_arguments_receiver(ap) {
                            return Some(format!("{}.{}", type_name, self.text(method_id)));
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
        None
    }

    pub(super) fn misparsed_generic_call(&self, lt: Node<'t>) -> Option<(Node<'t>, Node<'t>)> {
        if lt.kind() != "relational_operator" || lt.child(0)?.kind() != "<" {
            return None;
        }
        let inner = lt.parent()?;
        let outer = inner.parent()?;
        if inner.kind() != "relational_expression" || outer.kind() != "relational_expression" {
            return None;
        }
        let left = outer.named_child(0)?;
        if left.start_byte() != inner.start_byte() || left.end_byte() != inner.end_byte() {
            return None;
        }
        let gt = outer.named_child(1)?;
        if gt.kind() != "relational_operator" || gt.child(0)?.kind() != ">" {
            return None;
        }
        let args = outer.named_child(2)?;
        if !matches!(args.kind(), "parenthesized_expression" | "record_literal") || args.start_byte() != gt.end_byte() {
            return None;
        }
        // A comment against the `<` (`ref.read /* c */<Repo>(p)`) is a
        // sibling of its own, so the callee is not against it.
        let before = lt.prev_named_sibling()?;
        if is_comment(before) || before.end_byte() != lt.start_byte() {
            return None;
        }
        // The type argument: `T`, `p.T`, `T<…>` or `p.T<…>`, and nothing else.
        let head = lt.next_named_sibling()?;
        if head.kind() != "identifier" {
            return None;
        }
        let mut type_name = head;
        let mut rest = head.next_named_sibling();
        if let Some(sel) = rest {
            let accessor = sel.named_child(0).filter(|a| a.kind() == "unconditional_assignable_selector");
            if sel.kind() == "selector" {
                if let Some(accessor) = accessor {
                    let mut ac = accessor.walk();
                    let name = accessor.named_children(&mut ac).find(|c| c.kind() == "identifier");
                    type_name = name?;
                    rest = sel.next_named_sibling();
                }
            }
        }
        if let Some(sel) = rest {
            let only_type_args = sel.named_child_count() == 1
                && sel.named_child(0).map(|t| t.kind() == "type_arguments").unwrap_or(false);
            if sel.kind() == "selector" && only_type_args {
                rest = sel.next_named_sibling();
            }
        }
        if rest.is_some() {
            return None;
        }
        let name = self.text(type_name);
        if !is_upper_camel(name) && !is_lowercase_type(name) {
            return None;
        }
        // `await repo.load<User>(id)` and `-x.size<int>(y)` put the callee
        // under the prefix.
        let mut callee = before;
        while is_prefixed(callee.kind()) {
            let count = callee.named_child_count();
            callee = callee.named_child(count.checked_sub(1)?)?;
        }
        Some((callee, type_name))
    }

    pub(super) fn in_misparsed_generic_call(
        &self,
        receiver: Node<'t>,
        next: Option<Node<'t>>,
        parent: Option<Node<'t>>,
    ) -> bool {
        let mut after = next;
        let mut up = parent;
        while after.is_none() {
            let Some(u) = up.filter(|u| is_prefixed(u.kind())) else { break };
            after = u.next_named_sibling();
            up = if after.is_some() { None } else { u.parent() };
        }
        if let Some(a) = after {
            if a.kind() == "relational_operator" && self.misparsed_generic_call(a).is_some() {
                return true;
            }
        }
        if parent.map(|p| p.kind()) != Some("relational_expression") {
            return false;
        }
        receiver
            .prev_named_sibling()
            .map(|b| b.kind() == "relational_operator" && self.misparsed_generic_call(b).is_some())
            .unwrap_or(false)
    }

    pub(super) fn type_arguments_receiver(&self, selector: Node<'t>) -> Option<&'t str> {
        if selector.kind() != "selector" || selector.named_child_count() != 1 {
            return None;
        }
        if selector.named_child(0)?.kind() != "type_arguments" {
            return None;
        }
        let type_node = prev_named(selector)?;
        if type_node.kind() != "identifier" {
            return None;
        }
        Some(self.text(type_node))
    }
}
