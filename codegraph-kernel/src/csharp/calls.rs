//! Calls, instantiations, static member references, inheritance and type references.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// extractCall — the C# branch (tree-sitter.ts:4502) + shared tail.
    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        let caller = self.top_row();
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let Some(func) = func else { return };

        let mut callee_name: String;
        if func.kind() == "member_access_expression" {
            let recv = func.child_by_field_name("expression");
            let name_node = func.child_by_field_name("name");
            let method_name = name_node.map(|n| self.text(n)).unwrap_or("");
            let chained = recv
                .map(|r| r.kind() == "invocation_expression" && !method_name.is_empty())
                .unwrap_or(false);
            if chained {
                // Chained factory `Foo.Create(args).Bar()` → `Foo.Create().Bar`
                // (inner whitespace stripped, EVERY call-receiver re-encodes —
                // no capitalization gate, unlike kotlin/scala).
                let inner_func = recv.unwrap().child_by_field_name("function");
                let inner_callee = inner_func.map(|f| crate::textutil::strip_js_ws(self.text(f))).unwrap_or_default();
                callee_name = if inner_callee.is_empty() {
                    method_name.to_string()
                } else {
                    format!("{inner_callee}().{method_name}")
                };
            } else {
                // RAW full member-access text: `this.Run`, `base.Method`,
                // `"lit".ToUpper`, multi-line fluent chains with their
                // newlines — no SKIP_RECEIVERS, no literal filter (preserve).
                callee_name = self.text(func).to_string();
            }
        } else {
            // Bare `Helper()`, generic `Generic<int>` kept verbatim,
            // `nameof(...)` → a calls ref named `nameof`, `?.` chains raw,
            // `(myDel)(x)` → parenthesized text (normalized below).
            callee_name = self.text(func).to_string();
        }

        // Shared parenthesized-conversion normalization — the one shared
        // normalization C# actually hits: `(myDel)(x)` → `myDel`.
        if !callee_name.is_empty() {
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
        }
        // (template strip + fn-ptr fan-out are c/cpp-gated — not C#.)

        if !callee_name.is_empty() {
            self.push_ref_at(caller, &callee_name, crate::buffers::EDGE_CALLS, node);
        }
    }

    pub(super) fn extract_instantiation(&mut self, node: Node<'t>) {
        let ctor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };
        // `new List<Foo>()` → `List`; `new Ns.Foo()` → `Foo`. Target-typed
        // `new()` / anonymous `new { }` / arrays `new T[n]` never reach here
        // (not in INSTANTIATION_KINDS) — invisible by design.
        let class_name = strip_generic_and_qualifier(self.text(ctor));
        if !class_name.is_empty() {
            let from = self.top_row();
            self.push_ref_at(from, &class_name, crate::buffers::EDGE_INSTANTIATES, node);
        }
    }

    /// extractAnonymousClass — `new T() { ... }`. The C# grammar never
    /// produces a class_body/declaration_list child on object_creation
    /// (object initializers are initializer_expression), so this is
    /// unreachable — mirrored from the shared TS path like java.rs.
    pub(super) fn extract_anonymous_class(&mut self, node: Node<'t>, body: Node<'t>) {
        stack_guard!();
        let type_node = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let mut type_name =
            type_node.map(|t| self.text(t).to_string()).unwrap_or_else(|| "Object".to_string());
        type_name = strip_generic_and_qualifier(&type_name);
        if type_name.is_empty() {
            type_name = "Object".to_string();
        }

        let anon_name = format!("<{type_name}$anon@{}>", node.start_position().row + 1);
        let Some(row) = self.create_node("class", &anon_name, node, Extra::default()) else {
            return;
        };
        // Bug-for-bug: the TS code uses `startPosition.row` (0-based) as the
        // LINE here — the one place it forgets the +1.
        let (line, column) = match type_node {
            Some(t) => (t.start_position().row as u32, self.col_of(t)),
            None => (node.start_position().row as u32, self.col_of(node)),
        };
        self.push_ref(row, &type_name, crate::buffers::EDGE_EXTENDS, line, column);

        self.stack.push(Scope { row, kind: "class", name: anon_name });
        for c in named_kids(body) {
            self.visit_node(c);
        }
        self.stack.pop();
    }

    /// extractStaticMemberRef — csharp ∈ STATIC_MEMBER_LANGS; C#'s
    /// member-access node with the `expression` receiver field.
    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if node.kind() != "member_access_expression" {
            return;
        }
        let owner = self.top_row();
        // Skip `Type.Method()` — the access is a call's callee, already linked.
        if let Some(parent) = node.parent() {
            if parent.kind() == "invocation_expression" {
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

    /// extractInheritance — the C# base_list branch (5577): EVERY namedChild
    /// emits one `extends` ref (interfaces conflated by design; the garbage
    /// `(repo)` argument-list / `BaseDto(Name)` / `: byte` shapes preserved).
    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let extends_kind = crate::buffers::EDGE_EXTENDS;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "base_list" {
                continue;
            }
            for j in 0..child.named_child_count() {
                let Some(base) = child.named_child(j) else { continue };
                let name = if base.kind() == "generic_name" {
                    // `ClientBase<T>` → head identifier; position = generic_name.
                    let ident = named_kids(base)
                        .find(|c| c.kind() == "identifier");
                    match ident {
                        Some(idn) => self.text(idn).to_string(),
                        None => self.text(base).to_string(),
                    }
                } else {
                    self.text(base).to_string()
                };
                self.push_ref_at(class_row, &name, extends_kind, base);
            }
        }
    }
}
