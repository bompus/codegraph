//! Binding rows (docs/design/resolution-binding-model-plan.md): declarations, parameters, locals and imports, with their scopes.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    pub(super) fn emit_decl_binding(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>) {
        if matches!(kind, "file" | "import") {
            return;
        }
        let line = self.line_of(node);
        match self.enclosing_scope() {
            None => self.package_level_decl_row(name, row, line),
            Some(scope) => self.push_binding_row(BINDING_LOCAL, name, row, scope, line, None, false, None),
        }
    }

    /// One `param` row per parameter and receiver name, scoped to the function
    /// or function literal.
    pub(super) fn emit_param_bindings(&mut self, node: Node<'t>) {
        let scope = (self.line_of(node), node.end_position().row as u32 + 1);
        for field in ["receiver", "parameters", "result"] {
            let Some(list) = node.child_by_field_name(field) else { continue };
            for i in 0..list.named_child_count() {
                let Some(p) = list.named_child(i) else { continue };
                if !matches!(p.kind(), "parameter_declaration" | "variadic_parameter_declaration") {
                    continue;
                }
                for j in 0..p.named_child_count() {
                    let Some(c) = p.named_child(j) else { continue };
                    if c.kind() == "identifier" {
                        let name = self.text(c).to_string();
                        let line = self.line_of(c);
                        self.push_binding_row(BINDING_PARAM, &name, NONE, scope, line, None, false, None);
                    }
                }
            }
        }
    }

    /// Nodeless `local` rows for names a function body binds: `x := …`,
    /// `var x T` and `for i, v := range …`.
    pub(super) fn emit_local_rows(&mut self, node: Node<'t>) {
        if self.enclosing_scope().is_none() {
            return;
        }
        let scope = local_block(node);
        self.emit_local_rows_scoped(node, (scope.start_position().row as u32 + 1, scope.end_position().row as u32 + 1));
    }

    pub(super) fn emit_local_rows_scoped(&mut self, node: Node<'t>, scope: (u32, u32)) {
        let mut names: Vec<Node<'t>> = Vec::new();
        match node.kind() {
            "short_var_declaration" | "range_clause" => {
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "expression_list" {
                        names.extend(named_kids(left).filter(|c| c.kind() == "identifier"));
                    } else if left.kind() == "identifier" {
                        names.push(left);
                    }
                }
            }
            "var_declaration" | "const_declaration" => {
                for spec in declaration_specs(node) {
                    names.extend(named_kids(spec).filter(|c| c.kind() == "identifier"));
                }
            }
            _ => {}
        }
        for n in names {
            let name = self.text(n).to_string();
            let line = self.line_of(n);
            self.push_binding_row(BINDING_LOCAL, &name, NONE, scope, line, None, false, None);
        }
    }

    pub(super) fn emit_import_binding(&mut self, local: &str, import_path: &str, node: Node<'t>) {
        let line = self.line_of(node);
        let line_count = self.line_count;
        self.push_binding_row(BINDING_IMPORT, local, NONE, (1, line_count), line, Some((import_path, "*")), false, None);
    }

    pub(super) fn package_level_decl_row(&mut self, name: &str, node_idx: u32, line: u32) {
        let exported = Self::is_exported_name(name);
        let storage = if exported { None } else { Some("package") };
        self.push_binding_row(BINDING_DECL, name, node_idx, (1, self.line_count), line, None, exported, storage);
    }
}

/// The block a local declared at `node` lives in: the nearest block, case
/// clause, or `if`/`for`/`switch` header scope (`if v := f(); …`, `for _, v :=
/// range …`), else the function. Sibling blocks' same-named locals then get
/// disjoint ranges instead of sharing the function's.
fn local_block(node: Node) -> Node {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if matches!(
            p.kind(),
            "block"
                | "expression_case"
                | "type_case"
                | "default_case"
                | "communication_case"
                | "if_statement"
                | "for_statement"
                | "expression_switch_statement"
                | "type_switch_statement"
                | "function_declaration"
                | "method_declaration"
                | "func_literal"
        ) {
            return p;
        }
        cur = p;
    }
    cur
}
