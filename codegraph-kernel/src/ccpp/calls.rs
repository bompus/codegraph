//! Calls, instantiations, static member references and inheritance.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        let caller_row = self.top_row();
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let calls_kind = crate::buffers::EDGE_CALLS;

        // C++ explicit operator call `a.operator+(b)` (#1247): the
        // operator_name hides in an ERROR child.
        if self.variant == Variant::Cpp {
            if let Some(func) = func {
                let mut operator_name = String::new();
                'err: for i in 0..node.named_child_count() {
                    let Some(child) = node.named_child(i) else { continue };
                    if child.kind() != "ERROR" {
                        continue;
                    }
                    for op in named_kids(child) {
                        if op.kind() == "operator_name" {
                            operator_name = self.text(op).to_string();
                            break 'err;
                        }
                    }
                }
                if !operator_name.is_empty() {
                    // `operatorName.slice(8)` — an ERROR-recovered node may be
                    // shorter than the keyword; JS yields "" where a fixed
                    // byte slice would panic.
                    let sym = operator_name.get("operator".len()..).unwrap_or("").trim().to_string();
                    if symbolic_op_re().is_match(&sym) {
                        let compact: String = sym.chars().filter(|c| !c.is_whitespace()).collect();
                        operator_name = format!("operator{compact}");
                    }
                    let receiver = arrow_dot_no_ws(self.text(func));
                    if receiver != "this" && !operator_receiver_re().is_match(&receiver) {
                        return;
                    }
                    let callee = if receiver == "this" {
                        operator_name
                    } else {
                        format!("{receiver}.{operator_name}")
                    };
                    self.push_ref_at(caller_row, &callee, calls_kind, node);
                    return;
                }
            }
        }

        let mut callee_name = String::new();
        if let Some(func) = func {
            if func.kind() == "field_expression" {
                // `obj.method()` / `ptr->method()` — the `field` field.
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
                    if let Some(r) = receiver {
                        if is_literal_receiver(r.kind()) {
                            return; // #1230: literal receivers emit nothing
                        }
                    }
                    match receiver.map(|r| r.kind()) {
                        Some("identifier") | Some("simple_identifier") | Some("field_identifier") => {
                            let receiver_name = self.text(receiver.unwrap());
                            if !matches!(receiver_name, "self" | "this" | "cls" | "super") {
                                callee_name = format!("{receiver_name}.{method_name}");
                            } else {
                                callee_name = method_name.to_string();
                            }
                        }
                        Some("call_expression") => {
                            // Call-result receiver (#645/#608): re-encode as
                            // `<innerCallee>().<method>` — C/C++ re-encode any inner.
                            let inner_fn = receiver.unwrap().child_by_field_name("function");
                            let inner_callee =
                                inner_fn.map(|f| arrow_dot_no_ws(self.text(f))).unwrap_or_default();
                            if !inner_callee.is_empty() {
                                callee_name = format!("{inner_callee}().{method_name}");
                            } else {
                                callee_name = method_name.to_string();
                            }
                        }
                        _ => {
                            callee_name = method_name.to_string();
                        }
                    }
                }
            } else {
                // Bare / qualified / templated / parenthesized callee.
                callee_name = self.text(func).to_string();
            }
        }

        if callee_name.starts_with('(') {
            // `(*fp)(x)` → `fp` (parenthesized-conversion normalization; the
            // pattern is `^\(`-anchored, so only a `(`-led callee can match).
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
        }

        // Template-arg strip on callees (`fn<T, 256>(args)`, `ns::fn<T>()`).
        if !callee_name.is_empty() && callee_name.contains('<') && !callee_name.contains("operator")
        {
            callee_name = strip_cpp_template_args(&callee_name);
        }

        // Local fn-pointer fan-out: a bare callee bound earlier from `&fn`
        // emits one calls ref PER recorded target (insertion order).
        if !callee_name.is_empty()
            && self.variant == Variant::Cpp
            && crate::textutil::ascii_ident_re().is_match(&callee_name)
        {
            let targets = self
                .local_fn_ptrs
                .get(&caller_row)
                .and_then(|locals| locals.get(&callee_name))
                .cloned();
            if let Some(targets) = targets {
                if !targets.is_empty() {
                    for target in &targets {
                        self.push_ref_at(caller_row, target, calls_kind, node);
                    }
                    return;
                }
            }
        }

        if !callee_name.is_empty() {
            self.push_ref_at(caller_row, &callee_name, calls_kind, node);
        }
    }

    /// extractInstantiation: `new Foo(...)` and stack constructions (both
    /// read the type from the `type` field; template args + qualifiers strip).
    pub(super) fn extract_instantiation(&mut self, node: Node<'t>) {
        let from = self.top_row();
        let ctor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };

        let class_name = crate::textutil::strip_generic_and_qualifier(self.text(ctor));
        if !class_name.is_empty() {
            self.push_ref_at(from, &class_name, crate::buffers::EDGE_INSTANTIATES, node);
        }
    }

    /// isCppStackConstruction (#1035).
    pub(super) fn is_cpp_stack_construction(&self, node: Node) -> bool {
        let Some(type_node) = node.child_by_field_name("type") else { return false };
        if !matches!(
            type_node.kind(),
            "type_identifier" | "template_type" | "qualified_identifier"
        ) {
            return false;
        }
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "init_declarator" {
                continue;
            }
            if let Some(value) = child.child_by_field_name("value") {
                if matches!(value.kind(), "argument_list" | "initializer_list") {
                    return true;
                }
            }
        }
        false
    }

    /// extractStaticMemberRef — cpp only (c is not in STATIC_MEMBER_LANGS).
    /// In this grammar the firing shape is `field_expression` (listed in
    /// MEMBER_ACCESS_TYPES for Scala — same node kind here): a capitalized
    /// simple receiver's value read.
    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if self.variant != Variant::Cpp {
            return;
        }
        if !matches!(node.kind(), "field_expression" | "qualified_identifier") {
            return;
        }
        // Skip `Type.method()` — the access is a call's callee, already linked.
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
        if !matches!(
            recv.kind(),
            "identifier" | "type_identifier" | "simple_identifier" | "name" | "scoped_type_identifier"
        ) {
            return;
        }
        let text = self.text(recv);
        if capitalized_re().is_match(text) {
            let owner = self.top_row();
            let name = text.to_string();
            self.push_ref_at(owner, &name, crate::buffers::EDGE_REFERENCES, recv);
        }
    }

    /// extractInheritance — the branches whose node kinds occur in the c/cpp
    /// grammars: base_class_clause (#1043), the field_declaration Go-embedding
    /// shape, and the field_declaration_list recursion that reaches it.
    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        stack_guard!();
        let extends_kind = crate::buffers::EDGE_EXTENDS;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                "base_class_clause" => {
                    for j in 0..child.named_child_count() {
                        let Some(t) = child.named_child(j) else { continue };
                        if matches!(
                            t.kind(),
                            "type_identifier" | "qualified_identifier" | "template_type"
                        ) {
                            let name = strip_cpp_template_args(self.text(t));
                            self.push_ref_at(class_row, &name, extends_kind, t);
                        }
                    }
                }
                "field_declaration" => {
                    let has_field_identifier = named_kids(child)
                        .any(|c| c.kind() == "field_identifier");
                    if !has_field_identifier {
                        let type_id = named_kids(child)
                            .find(|c| c.kind() == "type_identifier");
                        if let Some(type_id) = type_id {
                            let name = self.text(type_id).to_string();
                            self.push_ref_at(class_row, &name, extends_kind, type_id);
                        }
                    }
                }
                "field_declaration_list" | "class_heritage" => {
                    self.extract_inheritance(child, class_row);
                }
                _ => {}
            }
        }
    }
}
