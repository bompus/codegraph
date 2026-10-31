//! Calls, instantiations, static member references, inheritance and type references.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// The middle identifier of an infix expression is its callee.
    pub(super) fn extract_infix_call(&mut self, node: Node<'t>) {
        if self.stack.is_empty() { return; }
        let children: Vec<_> = named_kids(node).filter(|n| !n.is_extra()).collect();
        if children.len() != 3 { return; }
        let (lhs, func) = (children[0], children[1]);
        if func.kind() != "simple_identifier" || is_literal_receiver(lhs.kind()) { return; }
        let caller = self.top_row();
        let name = self.text(func);
        // `(b) put 1` calls `put` on what `b` holds, as `b put 1` does.
        let mut receiver_node = lhs;
        while receiver_node.kind() == "parenthesized_expression" {
            match named_kids(receiver_node).filter(|n| !n.is_extra()).collect::<Vec<_>>().as_slice() {
                [inner] => receiver_node = *inner,
                _ => break,
            }
        }
        let receiver = if receiver_node.kind() == "simple_identifier" { self.text(receiver_node) } else { "" };
        let callee = if !receiver.is_empty() && receiver != "this" && receiver != "super" {
            format!("{receiver}.{name}")
        } else { name.to_string() };
        self.push_ref_at(caller, &callee, crate::buffers::EDGE_CALLS, if callee.contains('.') { node } else { func });
    }

    /// extractCall — the kotlin paths: navigation member branch (+ the #750
    /// re-encode) and the raw-text else (paren-then-lambda / glued-invoke
    /// garbage preserved).
    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        let caller = self.top_row();
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let Some(func) = func else { return };
        let mut callee_name = String::new();
        let mut call_at = node;

        if func.kind() == "navigation_expression" {
            let property = func
                .child_by_field_name("property")
                .or_else(|| func.child_by_field_name("field"))
                .or_else(|| {
                    let c1 = func.named_child(1);
                    match c1 {
                        Some(c) if c.kind() == "navigation_suffix" => named_kids(c)
                            .find(|g| g.kind() == "simple_identifier")
                            .or(Some(c)),
                        other => other,
                    }
                });
            if let Some(property) = property {
                let method_name = self.text(property);
                call_at = property;
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
            self.push_ref_at(caller, &callee_name, crate::buffers::EDGE_CALLS, if callee_name.contains('.') { node } else { call_at });
        }
    }

    /// extractStaticMemberRef — navigation_expression value reads, body
    /// walker only (assignment WRITES parse as directly_assignable_expression
    /// — not a member-access kind — and emit nothing).
    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if node.kind() != "navigation_expression" {
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
                self.push_ref_at(owner, text, crate::buffers::EDGE_REFERENCES, recv);
            }
        }
    }

    /// extractInheritance — delegation_specifier: user_type ?? its
    /// constructor_invocation's user_type → ONE `extends` ref named by the
    /// user_type's type_identifiers joined with `.` (`Outer.Inner`, type
    /// arguments left out), at the first one (interfaces ride extends too;
    /// `by`-delegation emits NOTHING).
    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let extends_kind = crate::buffers::EDGE_EXTENDS;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "delegation_specifier" {
                continue;
            }
            let user_type = named_kids(child)
                .find(|c| c.kind() == "user_type");
            let ctor_inv = named_kids(child)
                .find(|c| c.kind() == "constructor_invocation");
            let target = user_type.or(ctor_inv);
            let Some(target) = target else { continue };
            let type_node: Option<Node> = if target.kind() == "user_type" {
                Some(target)
            } else {
                named_kids(target).find(|c| c.kind() == "user_type")
            };
            let ids: Vec<Node> = match type_node {
                Some(t) => named_kids(t).filter(|c| c.kind() == "type_identifier").collect(),
                None => Vec::new(),
            };
            let type_id: Node = ids.first().copied().or(type_node).unwrap_or(target);
            let name = if ids.is_empty() {
                self.text(type_id).to_string()
            } else {
                ids.iter().map(|c| self.text(*c)).collect::<Vec<_>>().join(".")
            };
            self.push_ref_at(class_row, &name, extends_kind, type_id);
        }
    }

    /// extractKotlinObjectLiteral — `object : Api.Listener { … }`: an anonymous
    /// class. Headers (each delegation_specifier) are walked first by the
    /// CURRENT walker in the enclosing scope (`body_walker` picks the body
    /// walker); an `instantiates` ref from the stack top to the primary
    /// supertype as written; a class `<Short$anon@line>`; `extends` refs
    /// (dotted path) from it, at each path's first type_identifier; its
    /// class_body children through visit_node. False (nothing done) without a
    /// class_body.
    pub(super) fn extract_object_literal(&mut self, node: Node<'t>, body_walker: bool) -> bool {
        stack_guard!();
        let Some(body) = named_kids(node).find(|c| c.kind() == "class_body") else {
            return false;
        };
        let specs: Vec<Node<'t>> = named_kids(node).filter(|c| c.kind() == "delegation_specifier").collect();
        // (name, first type_identifier, built by a constructor_invocation)
        let mut supertypes: Vec<(String, Node<'t>, bool)> = Vec::new();
        for spec in &specs {
            let holder = named_kids(*spec)
                .find(|c| c.kind() == "constructor_invocation" || c.kind() == "explicit_delegation");
            let from = holder.unwrap_or(*spec);
            let user_type = named_kids(from).find(|c| c.kind() == "user_type");
            let ids: Vec<Node<'t>> = match user_type {
                Some(t) => named_kids(t).filter(|c| c.kind() == "type_identifier").collect(),
                None => Vec::new(),
            };
            if let Some(first) = ids.first() {
                let name = ids.iter().map(|c| self.text(*c)).collect::<Vec<_>>().join(".");
                let built = holder.map(|h| h.kind() == "constructor_invocation").unwrap_or(false);
                supertypes.push((name, *first, built));
            }
        }
        for spec in &specs {
            if body_walker {
                self.visit_for_calls_and_structure(*spec);
            } else {
                self.visit_node(*spec);
            }
        }
        // The class it builds, else its first supertype: the object's name and,
        // as written, what the code around instantiates.
        let primary = supertypes.iter().find(|s| s.2).or(supertypes.first());
        let short_name = match primary {
            Some((name, _, _)) => match name.rfind('.') {
                Some(dot) => name[dot + 1..].to_string(),
                None => name.clone(),
            },
            None => "Object".to_string(),
        };
        if let Some((written, at, _)) = primary {
            if !self.stack.is_empty() {
                let owner = self.top_row();
                let at = *at;
                let written = written.clone();
                self.push_ref_at(owner, &written, crate::buffers::EDGE_INSTANTIATES, at);
            }
        }
        let anon_name = format!("<{short_name}$anon@{}>", node.start_position().row + 1);
        let Some(row) = self.create_node("class", &anon_name, node, Extra::default()) else {
            for c in named_kids(body) {
                if body_walker {
                    self.visit_for_calls_and_structure(c);
                } else {
                    self.visit_node(c);
                }
            }
            return true;
        };
        for (name, at, _) in &supertypes {
            let at = *at;
            self.push_ref_at(row, name, crate::buffers::EDGE_EXTENDS, at);
        }
        self.stack.push(Scope { row, kind: "class", name: anon_name });
        for c in named_kids(body) {
            self.visit_node(c);
        }
        self.stack.pop();
        true
    }
}
