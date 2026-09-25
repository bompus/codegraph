//! Binding rows (docs/design/resolution-binding-model-plan.md): declarations, parameters, locals and imports, with their scopes.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    pub(super) fn parameter_names(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        let params = named_kids(node).find(|c| c.kind() == "formal_parameters");
        let Some(params) = params else { return out };
        for i in 0..params.named_child_count() {
            let Some(p) = params.named_child(i) else { continue };
            if let Some(n) = p.child_by_field_name("name") {
                out.push(n);
            }
        }
        out
    }

    /// `$x = …`: the assigned variable, as written (with its `$`).
    pub(super) fn local_names(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        if let Some(left) = node.child_by_field_name("left") {
            if left.kind() == "variable_name" {
                out.push(left);
            }
        }
        out
    }

    /// A file-level declaration is `public` unless its modifier narrows it:
    /// `private` and `internal` are not visible across files; `protected` is,
    /// through subclasses; PHP has no file-level visibility: every declaration is reachable.
    pub(super) fn file_level_decl_row(&mut self, name: &str, node_idx: u32, line: u32, visibility: Option<u8>) {
        let (exported, storage) = match visibility {
            Some(2) => (false, Some("private")),
            Some(3) => (true, Some("protected")),
            Some(4) => (false, Some("internal")),
            Some(_) => (true, None),
            None => (true, None),
        };
        self.push_binding_row(BINDING_DECL, name, node_idx, (1, self.line_count), line, None, exported, storage);
    }

    pub(super) fn emit_decl_binding(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>, visibility: Option<u8>) {
        // The package `namespace` node is qualified-name scaffolding, not a binding.
        if matches!(kind, "file" | "import" | "namespace") {
            return;
        }
        let line = self.line_of(node);
        match self.enclosing_scope() {
            None => self.file_level_decl_row(name, row, line, visibility),
            Some(scope) => self.push_binding_row(BINDING_LOCAL, name, row, scope, line, None, false, None),
        }
    }

    /// One `param` row per parameter name, scoped to the function.
    pub(super) fn emit_param_bindings(&mut self, node: Node<'t>) {
        let scope = (self.line_of(node), node.end_position().row as u32 + 1);
        let names = self.parameter_names(node);
        for n in names {
            let name = self.text(n).to_string();
            let line = self.line_of(n);
            self.push_binding_row(BINDING_PARAM, &name, NONE, scope, line, None, false, None);
        }
    }

    pub(super) fn emit_local_rows(&mut self, node: Node<'t>) {
        let Some(scope) = self.enclosing_scope() else { return };
        self.emit_local_rows_scoped(node, scope);
    }

    pub(super) fn emit_local_rows_scoped(&mut self, node: Node<'t>, scope: (u32, u32)) {
        let names = self.local_names(node);
        for n in names {
            let name = self.text(n).to_string();
            let line = self.line_of(n);
            self.push_binding_row(BINDING_LOCAL, &name, NONE, scope, line, None, false, None);
        }
    }

    pub(super) fn emit_import_binding(&mut self, local: &str, fqn: &str, node: Node<'t>) {
        let line = self.line_of(node);
        let line_count = self.line_count;
        self.push_binding_row(BINDING_IMPORT, local, NONE, (1, line_count), line, Some((fqn, local)), false, None);
    }
}
