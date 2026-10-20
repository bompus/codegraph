//! Calls, instantiations, static member references, inheritance and type references.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// extractCall — swift rides the generic member branch (navigation) and
    /// the raw-text else; the full matrix is in the checklist.
    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        let caller = self.top_row();
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let Some(func) = func else { return };
        // `-(a + b)`: an operator token applied to a parenthesized value leaves
        // the call with no callee node, only its arguments (tree-sitter.ts).
        // `!(x)` keeps its `bang` callee: a project may declare `prefix func !`.
        if func.kind() == "call_suffix" {
            return;
        }
        let func = peel_call_wrappers(func, self.src);
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
                        Some(c) if c.kind() == "navigation_suffix" => named_kids(c)
                            .find(|g| g.kind() == "simple_identifier")
                            .or(Some(c)),
                        other => other,
                    }
                });
            if let Some(property) = property {
                let method_name = self.text(property);
                // Through a prefix operator, `try` and `await`:
                // `(!store).contains(x)`, `(try! A()).b()`.
                let receiver = func
                    .child_by_field_name("object")
                    .or_else(|| func.child_by_field_name("operand"))
                    .or_else(|| func.child_by_field_name("argument"))
                    .or_else(|| func.named_child(0))
                    .map(|r| peel_call_wrappers(r, self.src));
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
                    // ws-stripped; capitalized chains only. Without a wrapper
                    // on its left edge (`try! Foo.make().bar().baz()`).
                    let inner = receiver.unwrap().named_child(0);
                    let inner_callee = inner
                        .map(|n| crate::textutil::strip_js_ws(unwrapped_text(n, self.src)))
                        .unwrap_or_default();
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
                } else if let Some(path) = receiver
                    .filter(|r| r.kind() == "navigation_expression")
                    .map(|r| crate::textutil::strip_js_ws(unwrapped_text(r, self.src)))
                    .filter(|p| is_type_path(p))
                {
                    // A type path, `API.PackageController.GetRoute.query(on:)`:
                    // keep it, so the resolver finds the member on the type the
                    // path names instead of any type's `query`
                    // (tree-sitter.ts SWIFT_TYPE_PATH_RECEIVER), without a
                    // prefix operator on its head (`!Settings.Defaults.isOn()`).
                    callee_name = format!("{path}.{method_name}");
                } else {
                    // self_expression / super_expression / instance nav /
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
            self.push_ref_at(caller, &callee_name, crate::buffers::EDGE_CALLS, node);
        }
    }

    /// extractStaticMemberRef — swift's navigation_expression value reads,
    /// body walker + walkAttrArgs only.
    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if node.kind() != "navigation_expression" {
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
        // A prefix operator binds to the head: `!Config.shared.isOn` is
        // `(!Config).shared.isOn` (tree-sitter.ts).
        let recv = peel_call_wrappers(recv, self.src);
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

    /// extractInheritance — the swift inheritance_specifier case: FIRST
    /// type_identifier of each specifier's user_type, everything as `extends`
    /// (conformances included; `: Module.Base` takes `Module`).
    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let extends_kind = crate::buffers::EDGE_EXTENDS;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "inheritance_specifier" {
                continue;
            }
            let user_type = named_kids(child)
                .find(|c| c.kind() == "user_type");
            let Some(user_type) = user_type else { continue };
            let type_id = named_kids(user_type)
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
        let ta = named_kids(node)
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = ta {
            self.extract_type_refs_from_subtree(ta, from_row);
        }
    }
}

/// isSwiftCallWrapper (tree-sitter.ts): a wrapper that leaves a call's target
/// the same — `try`/`await`, which before a chain bind to the part before its
/// last call (`try! A().b()` is `(try! A()).b()`), or a prefix operator, which
/// binds to the leftmost operand (`!isReady()` is `(!isReady)()`). An implicit
/// member's leading `.` (`.success(x)`) is part of the name.
fn is_call_wrapper(node: Node, src: &str) -> bool {
    match node.kind() {
        "try_expression" | "await_expression" => true,
        "prefix_expression" => !src[node.byte_range()].starts_with('.'),
        _ => false,
    }
}

/// swiftWrappedOperand: the operand a call wrapper wraps.
fn wrapped_operand<'t>(node: Node<'t>) -> Option<Node<'t>> {
    node.child_by_field_name(if node.kind() == "prefix_expression" { "target" } else { "expr" })
}

/// peelSwiftCallWrappers: strip call wrappers off a callee or receiver.
fn peel_call_wrappers<'t>(node: Node<'t>, src: &str) -> Node<'t> {
    let mut cur = node;
    while is_call_wrapper(cur, src) {
        match wrapped_operand(cur) {
            Some(inner) => cur = inner,
            None => break,
        }
    }
    cur
}

/// swiftUnwrappedText: a receiver's text from its innermost wrapped operand
/// on — the wrappers sit on the chain's left edge, so `!Settings.Defaults`
/// reads `Settings.Defaults` and `try! Foo.make().bar` reads `Foo.make().bar`.
fn unwrapped_text<'s>(node: Node, src: &'s str) -> &'s str {
    let mut start = node.start_byte();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if is_call_wrapper(n, src) {
            cur = wrapped_operand(n);
            if let Some(inner) = cur {
                start = inner.start_byte();
            }
        } else if matches!(n.kind(), "navigation_expression" | "call_expression") {
            cur = n.named_child(0);
        } else {
            break;
        }
    }
    &src[start..node.end_byte()]
}

/// SWIFT_TYPE_PATH_RECEIVER (tree-sitter.ts): `API.PackageController.GetRoute`,
/// two segments or more, each an ASCII capital then `[A-Za-z0-9_]*` (JS `\w`),
/// not led by `Self.`.
fn is_type_path(s: &str) -> bool {
    if s.starts_with("Self.") {
        return false;
    }
    let mut segments = 0;
    for seg in s.split('.') {
        let b = seg.as_bytes();
        if b.is_empty() || !b[0].is_ascii_uppercase() || !b[1..].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_') {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}
