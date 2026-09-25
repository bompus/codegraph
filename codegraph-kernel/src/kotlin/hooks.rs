//! Kotlin declaration hooks: properties and their accessors, `fun interface` recovery, and modifier reads.

use super::*;

impl<'t> Walker<'t> {
    /// extractModifiers — expect/actual platform modifiers, matched by NODE
    /// TYPE (never text), in order. Runs inside create_node for every node.
    pub(super) fn extract_modifiers(&self, node: Node) -> Option<Vec<String>> {
        let mut mods: Vec<String> = Vec::new();
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            if child.kind() != "modifiers" {
                continue;
            }
            for j in 0..child.child_count() {
                let Some(pm) = child.child(j) else { continue };
                if pm.kind() != "platform_modifier" {
                    continue;
                }
                for k in 0..pm.child_count() {
                    let Some(kw) = pm.child(k) else { continue };
                    if matches!(kw.kind(), "expect" | "actual") {
                        mods.push(kw.kind().to_string());
                    }
                }
            }
        }
        if mods.is_empty() { None } else { Some(mods) }
    }

    /// A property's node kind, or None when the declaration mints no node at
    /// all: destructuring, an unreadable name, or a local (inside a function
    /// body / `init` block / lambda / accessor). Kind by enclosing scope — a
    /// singleton `object` / `companion object` (and a top-level property) holds
    /// SHARED values (`val`→constant, `var`→variable, the Scala-object rule; a
    /// `const val` is just a val); a class/interface/enum instance `val`/`var`
    /// is per-instance state → `field`.
    pub(super) fn property_kind(&self, node: Node<'t>) -> Option<&'static str> {
        let var_decl = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "variable_declaration")?;
        let name_node = (0..var_decl.named_child_count())
            .filter_map(|i| var_decl.named_child(i))
            .find(|c| c.kind() == "simple_identifier")?;
        if self.text(name_node).is_empty() {
            return None;
        }
        let mut scope: &str = "const";
        let mut p = node.parent();
        while let Some(pn) = p {
            match pn.kind() {
                "function_body" | "function_declaration" | "lambda_literal"
                | "anonymous_initializer" | "control_structure_body" | "getter" | "setter" => {
                    scope = "local";
                    break;
                }
                "companion_object" | "object_declaration" => {
                    scope = "const";
                    break;
                }
                "class_declaration" => {
                    scope = "instance";
                    break;
                }
                _ => {}
            }
            p = pn.parent();
        }
        if scope == "local" {
            return None;
        }
        let binding = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "binding_pattern_kind");
        let is_val = binding.map(|b| self.text(b) == "val").unwrap_or(false);
        Some(if scope == "instance" {
            "field"
        } else if is_val {
            "constant"
        } else {
            "variable"
        })
    }

    /// isFunInterfaceNode (languages/kotlin.ts): a `fun` keyword child plus a
    /// `user_type` whose type_identifier reads `interface`, directly or inside
    /// an ERROR child.
    pub(super) fn is_fun_interface_node(&self, node: Node<'t>) -> bool {
        let user_type_is_interface = |ut: Node<'t>| -> bool {
            (0..ut.named_child_count())
                .filter_map(|i| ut.named_child(i))
                .any(|c| c.kind() == "type_identifier" && self.text(c) == "interface")
        };
        let mut has_fun = false;
        let mut has_interface = false;
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            if child.kind() == "fun" && !child.is_named() {
                has_fun = true;
            }
            if child.kind() == "user_type" && user_type_is_interface(child) {
                has_interface = true;
            }
            if child.kind() == "ERROR" {
                for j in 0..child.child_count() {
                    if let Some(gc) = child.child(j) {
                        if gc.kind() == "user_type" && user_type_is_interface(gc) {
                            has_interface = true;
                        }
                    }
                }
            }
        }
        has_fun && has_interface
    }

    pub(super) fn try_visit_hook(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        // An own-line accessor already walked by its owning property below. The
        // ownership test re-derives the property's kind rather than remembering
        // it: a destructured or local declaration mints no node, so its
        // accessors were NOT consumed and must keep falling through.
        if matches!(node.kind(), "getter" | "setter") {
            return accessor_owner(node)
                .and_then(|owner| self.property_kind(owner))
                .is_some();
        }
        // Kotlin `fun interface` misparse recovery (languages/kotlin.ts visitNode
        // hook). tree-sitter-kotlin has no `fun interface` production, so the
        // declaration lands as (1) a top-level ERROR node followed by a sibling
        // lambda_literal holding the body, or (2) a function_declaration whose
        // real name sits inside an ERROR child. Mint the interface node; walk a
        // pattern-1 body; skip the lambda_literal a pattern-1 ERROR consumed.
        if node.kind() == "lambda_literal" {
            if let Some(prev) = node.prev_sibling() {
                if prev.kind() == "ERROR" && self.is_fun_interface_node(prev) {
                    return true;
                }
            }
            return false;
        }
        if matches!(node.kind(), "ERROR" | "function_declaration") {
            // An ERROR that is a mangled class BODY (starts with `{`) holds the
            // parent's methods; resolve_body walks it — never consume it here.
            let body_error = node.kind() == "ERROR"
                && node.child(0).map(|c| c.kind() == "{").unwrap_or(false);
            if !body_error && self.is_fun_interface_node(node) {
                let mut name: Option<String> = None;
                if node.kind() == "function_declaration" {
                    'outer: for i in 0..node.child_count() {
                        let Some(child) = node.child(i) else { continue };
                        if child.kind() != "ERROR" {
                            continue;
                        }
                        for j in 0..child.child_count() {
                            if let Some(gc) = child.child(j) {
                                if gc.kind() == "simple_identifier" {
                                    name = Some(self.text(gc).to_string());
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
                if name.is_none() {
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "simple_identifier" {
                                name = Some(self.text(child).to_string());
                                break;
                            }
                        }
                    }
                }
                let Some(name) = name else { return false };
                let extra = Extra {
                    docstring: preceding_docstring(node, self.src),
                    ..Extra::default()
                };
                let Some(row) = self.create_node("interface", &name, node, extra) else {
                    return false;
                };
                self.stack.push(Scope { row, kind: "interface", name });
                if node.kind() == "ERROR" {
                    if let Some(next) = node.next_sibling() {
                        if next.kind() == "lambda_literal" {
                            for i in 0..next.named_child_count() {
                                let Some(child) = next.named_child(i) else { continue };
                                if child.kind() != "statements" {
                                    continue;
                                }
                                for j in 0..child.named_child_count() {
                                    if let Some(stmt) = child.named_child(j) {
                                        self.visit_node(stmt);
                                    }
                                }
                            }
                        }
                    }
                }
                self.stack.pop();
                return true;
            }
        }
        if node.kind() != "property_declaration" {
            return false;
        }
        let var_decl = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "variable_declaration");
        let name_node = var_decl.and_then(|vd| {
            (0..vd.named_child_count())
                .filter_map(|i| vd.named_child(i))
                .find(|c| c.kind() == "simple_identifier")
        });
        // Destructuring (`val (a, b) = makePair()`): NEITHER arm mints a symbol
        // for the destructured names — declining just routes the node to
        // extractField/extractVariable, which both find nothing for kotlin and
        // end in the same fn-ref scan. But the RHS is CODE, and it was vanishing
        // whole. Consume the node here and walk it at the ENCLOSING scope (no
        // symbol of its own to attribute to).
        let Some(name_node) = name_node else {
            for init in property_initializers(node) {
                self.visit_for_calls_and_structure(init);
            }
            return true;
        };
        let name = self.text(name_node).to_string();
        if name.is_empty() {
            return false;
        }
        let Some(kind) = self.property_kind(node) else {
            // A local — no node is minted, but the initializer is still code.
            // Walk it at the ENCLOSING scope: an `init { }` block's
            // `val q = load()` is the CLASS calling load, and it used to
            // disappear entirely (only the block's bare statements survived).
            for init in property_initializers(node) {
                self.visit_for_calls_and_structure(init);
            }
            return true;
        };
        // The `type`-field signature read is dead (zero fields) → signature
        // undefined; NO docstring/visibility/isStatic — the modifiers merge in
        // create_node still decorates expect/actual properties.
        let row = self.create_node(kind, &name, node, Extra::default());
        // Walk the initializer ATTRIBUTED to the declared symbol (#693, the Go
        // fix, ported): without this the subtree is only fn-ref-scanned, so a
        // lambda / SAM / object initializer (`val cb = Runnable { target() }` —
        // the idiomatic Android callback field) contributed NO call edge at all.
        // The property also OWNS any accessor written on its own line, which the
        // grammar makes a following SIBLING rather than a child; those bodies
        // used to attribute to the enclosing class.
        if let Some(row) = row {
            self.stack.push(Scope { row, kind, name: name.clone() });
            for init in property_initializers(node) {
                self.visit_for_calls_and_structure(init);
            }
            for acc in following_accessors(node) {
                self.visit_for_calls_and_structure(acc);
            }
            self.stack.pop();
        }
        true
    }
}
