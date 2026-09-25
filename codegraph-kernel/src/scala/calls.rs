//! Calls, instantiations, static member references, inheritance and type references.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        let caller_row = self.top_row();
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let Some(func) = func else { return };

        let mut callee: Option<String> = None;
        if func.kind() == "field_expression" {
            // Member branch (:4364): property = `field` field for scala.
            let property = func
                .child_by_field_name("property")
                .or_else(|| func.child_by_field_name("field"))
                .or_else(|| func.named_child(1));
            if let Some(property) = property {
                let method_name = self.text(property);
                let receiver = func
                    .child_by_field_name("object")
                    .or_else(|| func.child_by_field_name("operand"))
                    .or_else(|| func.child_by_field_name("argument"))
                    .or_else(|| func.named_child(0));
                if let Some(receiver) = receiver {
                    if is_literal_receiver(receiver.kind()) {
                        return; // literal receivers emit NOTHING (#1230)
                    }
                    if matches!(receiver.kind(), "identifier" | "simple_identifier" | "field_identifier") {
                        let recv_name = self.text(receiver);
                        if matches!(recv_name, "self" | "this" | "cls" | "super") {
                            callee = Some(method_name.to_string());
                        } else {
                            callee = Some(format!("{recv_name}.{method_name}"));
                        }
                    } else if receiver.kind() == "call_expression" {
                        // The #750 re-encode, scala arm (:4443-4464): inner
                        // callee via the REAL `function` field; re-encode only
                        // capitalized (companion-factory / apply) chains.
                        let inner_fn = receiver.child_by_field_name("function");
                        let inner_callee = inner_fn
                            .map(|f| {
                                let t = self.text(f).replace("->", ".");
                                ws_re().replace_all(&t, "").into_owned()
                            })
                            .unwrap_or_default();
                        let reencode = crate::textutil::starts_upper_re().is_match(&inner_callee);
                        callee = Some(if reencode {
                            format!("{inner_callee}().{method_name}")
                        } else {
                            method_name.to_string()
                        });
                    } else {
                        callee = Some(method_name.to_string());
                    }
                } else {
                    callee = Some(method_name.to_string());
                }
            }
        } else {
            // Else branch (:4518-4520): RAW func text (apply-sugar `WidgetS`,
            // `genericCall[Int]` type args kept, curried `curried(1)` inners).
            callee = Some(self.text(func).to_string());
        }

        let Some(mut callee) = callee else { return };
        // Parenthesized-conversion (:4529-4532).
        if let Some(caps) = util::paren_conversion().captures(&callee) {
            if let Some(inner) = caps.get(1) {
                callee = inner.as_str().to_string();
            }
        }
        if callee.is_empty() {
            return;
        }
        self.push_ref_at(caller_row, &callee, "calls", node);
    }

    pub(super) fn extract_instantiation(&mut self, node: Node<'t>) {
        let from_row = self.top_row();
        let ctor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };
        if let Some(name) = self.scala_base_type_name(Some(ctor)) {
            self.push_ref_at(from_row, &name, "instantiates", node);
        }
    }

    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        let owner_row = self.top_row();
        // MEMBER_ACCESS_TYPES — only field_expression occurs in scala trees.
        if !matches!(
            node.kind(),
            "field_access" | "member_access_expression" | "navigation_expression"
                | "field_expression" | "class_constant_access_expression"
                | "scoped_property_access_expression" | "qualified_identifier"
        ) {
            return;
        }
        // Callee-of-call skip: `Type.method()`'s callee access is already a
        // calls ref.
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
            if crate::textutil::capitalized_re().is_match(text) {
                let text = text.to_string();
                self.push_ref_at(owner_row, &text, "references", recv);
            }
        }
    }

    pub(super) fn extract_decorators_for(&mut self, decl: Node<'t>, decorated_row: u32) {
        // consider(): scala annotations are `annotation` nodes; the name is
        // the first identifier-ish child (type_identifier for scala), with
        // call_expression unwrap for invoked decorators.
        // Scan 1: direct children (+ modifiers descent — inert for scala,
        // annotations aren't inside modifiers in this grammar, but ported).
        let mut cursor = decl.walk();
        for child in decl.named_children(&mut cursor) {
            self.consider_decorator(child, decorated_row);
            if child.kind() == "modifiers" {
                let mut mc = child.walk();
                let inner: Vec<Node<'t>> = child.named_children(&mut mc).collect();
                for m in inner {
                    self.consider_decorator(m, decorated_row);
                }
            }
        }
        // Scan 2: preceding siblings (TS class style — inert for scala where
        // annotations are children, ported for fidelity).
        if let Some(parent) = decl.parent() {
            let decl_start = decl.start_byte();
            let mut decl_idx: Option<usize> = None;
            for i in 0..parent.named_child_count() {
                if let Some(sib) = parent.named_child(i) {
                    if sib.start_byte() == decl_start {
                        decl_idx = Some(i);
                        break;
                    }
                }
            }
            if let Some(di) = decl_idx {
                for j in (0..di).rev() {
                    let Some(sib) = parent.named_child(j) else { continue };
                    if !matches!(sib.kind(), "decorator" | "annotation" | "marker_annotation") {
                        break;
                    }
                    self.consider_decorator(sib, decorated_row);
                }
            }
        }
    }

    pub(super) fn consider_decorator(&mut self, n: Node<'t>, decorated_row: u32) {
        if !matches!(n.kind(), "decorator" | "annotation" | "marker_annotation" | "attribute") {
            return;
        }
        let mut target: Option<Node<'t>> = None;
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            if child.kind() == "call_expression" {
                let fnn = child.child_by_field_name("function").or_else(|| child.named_child(0));
                if let Some(f) = fnn {
                    target = Some(f);
                }
                if target.is_some() {
                    break;
                }
            }
            if matches!(
                child.kind(),
                "identifier" | "member_expression" | "scoped_identifier" | "navigation_expression"
                    | "user_type" | "type_identifier"
            ) {
                target = Some(child);
                break;
            }
        }
        let Some(target) = target else { return };
        let name = crate::textutil::strip_generic_and_qualifier(self.text(target));
        if name.is_empty() {
            return;
        }
        self.push_ref_at(decorated_row, &name, "decorates", n);
    }

    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(
                child.kind(),
                "extends_clause" | "superclass" | "base_clause" | "extends_interfaces"
            ) {
                // Iterate ALL supertypes (with-chains, comma form); unwrap
                // each via scalaBaseTypeName; `arguments` children → None →
                // skipped. `derives_clause` is a different kind — silent.
                let mut cc = child.walk();
                let targets: Vec<Node<'t>> = child.named_children(&mut cc).collect();
                for target in targets {
                    if let Some(name) = self.scala_base_type_name(Some(target)) {
                        self.push_ref_at(class_row, &name, "extends", target);
                    }
                }
            }
        }
    }

    pub(super) fn extract_type_annotations(&mut self, node: Node<'t>, row: u32) {
        // Scala walks EVERY `parameters`-TYPE child (all curried lists; the
        // type_parameters node is a different kind, matched by walk 3).
        let mut cursor = node.walk();
        let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        for pc in &kids {
            if pc.kind() == "parameters" {
                self.type_refs_from_subtree(*pc, row);
            }
        }
        if let Some(rt) = node.child_by_field_name("return_type") {
            self.type_refs_from_subtree(rt, row);
        }
        // Context/upper bounds: the first type_parameters child.
        if let Some(tp) = kids.iter().find(|c| c.kind() == "type_parameters") {
            self.type_refs_from_subtree(*tp, row);
        }
        // Direct type_annotation child — no such scala kind; ported cheaply.
        if let Some(ta) = kids.iter().find(|c| c.kind() == "type_annotation") {
            self.type_refs_from_subtree(*ta, row);
        }
    }

    pub(super) fn type_refs_from_subtree(&mut self, node: Node<'t>, from_row: u32) {
        stack_guard!();
        if node.kind() == "type_identifier" {
            let name = self.text(node);
            if !name.is_empty() && !is_builtin_type(name) {
                let name = name.to_string();
                self.push_ref_at(from_row, &name, "references", node);
            }
            return;
        }
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            self.type_refs_from_subtree(c, from_row);
        }
    }
}
