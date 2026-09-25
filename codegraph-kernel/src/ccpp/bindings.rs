//! Binding rows (docs/design/resolution-binding-model-plan.md): declarations, parameters, locals and imports, with their scopes.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// Parameter names of a function definition: the identifier inside each
    /// `parameter_declaration`, under any pointer / reference / array wrapping.
    pub(super) fn parameter_names(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        let Some(mut decl) = node.child_by_field_name("declarator") else { return out };
        while matches!(decl.kind(), "pointer_declarator" | "reference_declarator") {
            match decl.child_by_field_name("declarator").or_else(|| decl.named_child(0)) {
                Some(i) => decl = i,
                None => break,
            }
        }
        if decl.kind() != "function_declarator" {
            return out;
        }
        let Some(params) = decl.child_by_field_name("parameters") else { return out };
        for i in 0..params.named_child_count() {
            let Some(p) = params.named_child(i) else { continue };
            if !matches!(p.kind(), "parameter_declaration" | "optional_parameter_declaration" | "variadic_parameter_declaration") {
                continue;
            }
            if let Some(d) = p.child_by_field_name("declarator") {
                if let Some(id) = declarator_identifier(d) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// Names a `declaration` binds: every declarator's identifier.
    pub(super) fn local_names(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        for i in 0..node.named_child_count() {
            let Some(c) = node.named_child(i) else { continue };
            if matches!(c.kind(), "identifier" | "init_declarator" | "pointer_declarator" | "array_declarator" | "reference_declarator") {
                if let Some(id) = declarator_identifier(c) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// A file-level definition is reachable from every unit that includes or
    /// links it (`public`); `static` narrows it to its own translation unit,
    /// recorded as `storage = static` and left to the resolver, which exempts
    /// a header (a `static inline` there exists in every includer).
    pub(super) fn file_level_decl_row(&mut self, name: &str, node_idx: u32, line: u32, is_static: bool) {
        let storage = if is_static { Some("static") } else { None };
        self.push_binding_row(BINDING_DECL, name, node_idx, (1, self.line_count), line, None, true, storage);
    }

    pub(super) fn emit_decl_binding(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>) {
        if matches!(kind, "file" | "import") {
            return;
        }
        let line = self.line_of(node);
        match self.enclosing_scope() {
            None => { let st = self.has_static_storage(node); self.file_level_decl_row(name, row, line, st); }
            Some(scope) => self.push_binding_row(BINDING_LOCAL, name, row, scope, line, None, false, None),
        }
    }

    /// `static` among the declaration's storage-class specifiers, on the
    /// definition itself or on the `declaration` a declarator sits in.
    pub(super) fn has_static_storage(&self, node: Node<'t>) -> bool {
        let mut cur = Some(node);
        while let Some(n) = cur {
            for c in named_kids(n) {
                if c.kind() == "storage_class_specifier" && self.text(c) == "static" {
                    return true;
                }
            }
            if matches!(n.kind(), "function_definition" | "declaration" | "type_definition") {
                return false;
            }
            cur = n.parent();
        }
        false
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
            if name.is_empty() {
                continue;
            }
            let line = self.line_of(n);
            self.push_binding_row(BINDING_LOCAL, &name, NONE, scope, line, None, false, None);
        }
    }

    /// An `#include` row: the header's basename without its extension as the
    /// local name (the resolver's long-standing reading), the path as written.
    pub(super) fn emit_import_binding(&mut self, local: &str, path: &str, node: Node<'t>) {
        let line = self.line_of(node);
        let line_count = self.line_count;
        self.push_binding_row(BINDING_IMPORT, local, NONE, (1, line_count), line, Some((path, "*")), false, None);
    }
}
