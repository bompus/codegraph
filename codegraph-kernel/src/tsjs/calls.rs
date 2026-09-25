//! Calls, instantiations, decorators, inheritance and type references.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// Identifier-rooted nested receivers retain their full call-site text.
    pub(super) fn is_identifier_chain(&self, receiver: Node<'t>) -> bool {
        let mut cur = receiver;
        if !matches!(cur.kind(), "member_expression" | "subscript_expression") {
            return false;
        }
        while matches!(cur.kind(), "member_expression" | "subscript_expression") {
            match cur.child_by_field_name("object") {
                Some(next) => cur = next,
                None => return false,
            }
        }
        cur.kind() == "identifier"
    }

    /// Identifier-rooted member chains have no inferred property type (#1566),
    /// including host API chains (#1707). Keep the existing window namespace
    /// escape; call-result and `this` receivers are outside this guard.
    pub(super) fn is_unresolved_member_chain(&self, receiver: Node<'t>) -> bool {
        let mut cur = receiver;
        if !matches!(cur.kind(), "member_expression" | "subscript_expression") {
            return false;
        }
        while matches!(cur.kind(), "member_expression" | "subscript_expression") {
            match cur.child_by_field_name("object") {
                Some(next) => cur = next,
                None => return false,
            }
        }
        cur.kind() == "identifier" && self.text(cur) != "window"
    }

    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        // Dynamic `import('x')`: the specifier is an import boundary, not a
        // callee — emit it as an `imports` ref like a static `import … from
        // 'x'` so lazy edges (`load: () => import('./x')`, React.lazy, split
        // points) stay traversable. The `.then(v => v.m)` unwrap is a member
        // access on the module namespace generic resolution cannot type —
        // the module edge is the honest deliverable. Declarator specs go
        // through `require_spec`; bare call sites come through here.
        if func.is_some_and(|f| f.kind() == "import") {
            if let Some(args) = node.child_by_field_name("arguments") {
                if let Some(first) = args.named_child(0) {
                    if first.kind() == "string" {
                        let spec: String =
                            self.text(first).chars().filter(|c| *c != '\'' && *c != '"').collect();
                        if !spec.is_empty() {
                            let from_row = self.top_row();
                            self.push_ref(from_row, &spec, crate::buffers::EDGE_IMPORTS, node);
                        }
                    }
                }
            }
            return;
        }
        let mut callee_name = String::new();

        if let Some(func) = func {
            if func.kind() == "member_expression" {
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
                    // Literal receivers call builtins, never project symbols (#1230).
                    if let Some(r) = receiver {
                        if is_literal_receiver(r.kind()) {
                            return;
                        }
                    }
                    let recv_ident = receiver.filter(|r| {
                        matches!(r.kind(), "identifier" | "simple_identifier" | "field_identifier")
                    });
                    if let Some(r) = recv_ident {
                        let receiver_name = self.text(r);
                        if !matches!(receiver_name, "self" | "this" | "cls" | "super") {
                            callee_name = format!("{receiver_name}.{method_name}");
                        } else {
                            callee_name = method_name.to_string();
                        }
                    } else if receiver.is_some_and(|r| self.is_unresolved_member_chain(r)) {
                        // Retain the call site for effects without guessing a
                        // project method. Mirrors the TS extraction path.
                        let chain = self.text(func).replace("?.", ".");
                        let Some(chain) = Self::plain_member_name(&chain) else { return };
                        callee_name = chain;
                    } else if let Some(field) = receiver.and_then(|r| self.this_field_of(r)) {
                        // `this.<field>.<method>()` — keep the field so the
                        // resolver can read its declared type (#1496). Mirrors
                        // TreeSitterExtractor.extractCall.
                        callee_name = format!("this.{field}.{method_name}");
                    } else if let Some(r) = receiver.filter(|r| r.kind() == "call_expression") {
                        // Call receiver — `make().run()` (#1683): keep the inner
                        // callee as `<inner>().<method>`, or emit nothing when it
                        // is not a plain name / member chain. Mirrors
                        // TreeSitterExtractor.extractCall.
                        let Some(inner) = self.plain_inner_callee(r) else { return };
                        callee_name = format!("{inner}().{method_name}");
                    } else if let Some(r) = receiver.filter(|r| self.is_identifier_chain(*r)) {
                        // Frameworks and Steps need the call site even when
                        // generic resolution cannot prove a target (#1794).
                        callee_name = format!("{}.{method_name}", self.text(r));
                    } else {
                        callee_name = method_name.to_string();
                    }
                }
            } else {
                callee_name = self.text(func).to_string();
            }
        }

        // Parenthesized-callee normalization (`(fn)()` → fn).
        if !callee_name.is_empty() {
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
        }

        if !callee_name.is_empty() {
            self.push_call_ref(&callee_name, node);
        }
    }

    /// `this.<field>` as a member_expression receiver → Some(field) (#1496).
    pub(super) fn this_field_of(&self, receiver: Node<'t>) -> Option<String> {
        if receiver.kind() != "member_expression" {
            return None;
        }
        let object = receiver.child_by_field_name("object")?;
        let property = receiver.child_by_field_name("property")?;
        if object.kind() != "this" || property.kind() != "property_identifier" {
            return None;
        }
        Some(self.text(property).to_string())
    }

    /// The callee of a call-expression receiver when it is a plain identifier
    /// or member chain (`make`, `d.setdefault`), whitespace stripped (#1683).
    pub(super) fn plain_inner_callee(&self, call: Node<'t>) -> Option<String> {
        let inner = call.child_by_field_name("function")?;
        Self::plain_member_name(self.text(inner))
    }

    pub(super) fn plain_member_name(source: &str) -> Option<String> {
        let text: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        if text.is_empty() {
            return None;
        }
        let ok = text.split('.').all(|seg| {
            let mut chars = seg.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        });
        if ok { Some(text) } else { None }
    }

    pub(super) fn extract_instantiation(&mut self, node: Node<'t>) {
        let ctor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };

        let class_name = crate::textutil::strip_generic_and_qualifier(self.text(ctor));
        if !class_name.is_empty() {
            let from = self.top_row();
            self.push_ref(from, &class_name, crate::buffers::EDGE_INSTANTIATES, node);
        }
    }

    pub(super) fn extract_decorators_for(&mut self, decl: Node<'t>, decorated_row: u32) {
        // 1. Direct children (method/property style).
        for i in 0..decl.named_child_count() {
            let Some(child) = decl.named_child(i) else { continue };
            self.consider_decorator(child, decorated_row);
            if child.kind() == "modifiers" {
                for m in named_kids(child) {
                    self.consider_decorator(m, decorated_row);
                }
            }
        }
        // 2. Preceding siblings (TypeScript class style), stopping at the
        //    first non-decorator so an earlier declaration's decorators never
        //    leak in. Matching by startIndex, not object identity.
        let Some(parent) = decl.parent() else { return };
        let decl_start = decl.start_byte();
        let mut decl_idx: isize = -1;
        for i in 0..parent.named_child_count() {
            if let Some(sib) = parent.named_child(i) {
                if sib.start_byte() == decl_start {
                    decl_idx = i as isize;
                    break;
                }
            }
        }
        if decl_idx > 0 {
            let mut j = decl_idx - 1;
            while j >= 0 {
                let Some(sib) = parent.named_child(j as usize) else {
                    j -= 1;
                    continue;
                };
                if !matches!(sib.kind(), "decorator" | "annotation" | "marker_annotation") {
                    break;
                }
                self.consider_decorator(sib, decorated_row);
                j -= 1;
            }
        }
    }

    pub(super) fn consider_decorator(&mut self, n: Node<'t>, decorated_row: u32) {
        if !matches!(n.kind(), "decorator" | "annotation" | "marker_annotation" | "attribute") {
            return;
        }
        let mut target: Option<Node> = None;
        for i in 0..n.named_child_count() {
            let Some(child) = n.named_child(i) else { continue };
            if child.kind() == "call_expression" {
                target = child.child_by_field_name("function").or_else(|| child.named_child(0));
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
        self.push_ref(decorated_row, &name, crate::buffers::EDGE_DECORATES, n);
    }

    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        stack_guard!();
        let extends_kind = crate::buffers::EDGE_EXTENDS;
        let implements_kind = crate::buffers::EDGE_IMPLEMENTS;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                // TS `extends_clause` (the other spellings are other grammars').
                "extends_clause" | "superclass" | "base_clause" | "extends_interfaces" => {
                    if let Some(target) = child.named_child(0) {
                        let name = self.text(target).to_string();
                        self.push_ref(class_row, &name, extends_kind, target);
                    }
                }
                "implements_clause" | "class_interface_clause" | "super_interfaces" | "interfaces" => {
                    for iface in named_kids(child) {
                        let name = self.text(iface).to_string();
                        self.push_ref(class_row, &name, implements_kind, iface);
                    }
                }
                // JS `class Foo extends Bar` — class_heritage holds a bare
                // identifier without an extends_clause wrapper.
                "identifier" | "type_identifier" if node.kind() == "class_heritage" => {
                    let name = self.text(child).to_string();
                    self.push_ref(class_row, &name, extends_kind, child);
                }
                // TS class_heritage wraps extends/implements — recurse.
                "field_declaration_list" | "class_heritage" => {
                    self.extract_inheritance(child, class_row);
                }
                _ => {}
            }
        }
    }

    pub(super) fn extract_type_annotations(&mut self, node: Node<'t>, from_row: u32) {
        if !self.variant.is_ts() {
            return;
        }
        if let Some(params) = node.child_by_field_name("parameters") {
            self.extract_type_refs_from_subtree(params, from_row);
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.extract_type_refs_from_subtree(ret, from_row);
        }
        let type_annotation = named_kids(node)
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = type_annotation {
            self.extract_type_refs_from_subtree(ta, from_row);
        }
    }

    pub(super) fn extract_variable_type_annotation(&mut self, node: Node<'t>, from_row: u32) {
        if !self.variant.is_ts() {
            return;
        }
        let type_annotation = named_kids(node)
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = type_annotation {
            self.extract_type_refs_from_subtree(ta, from_row);
        }
    }

    pub(super) fn extract_type_refs_from_subtree(&mut self, node: Node<'t>, from_row: u32) {
        stack_guard!();
        if node.kind() == "type_identifier" {
            let type_name = self.text(node).to_string();
            if !type_name.is_empty() && !is_builtin_type(&type_name) {
                self.push_ref(from_row, &type_name, crate::buffers::EDGE_REFERENCES, node);
            }
            return;
        }
        for c in named_kids(node) {
            self.extract_type_refs_from_subtree(c, from_row);
        }
    }
}
