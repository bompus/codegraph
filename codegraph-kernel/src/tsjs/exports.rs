//! Module exports and the binding rows they produce: later `export` statements, CommonJS exports, default exports, declaration/import/re-export rows.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// Pre-walk: the names a later top-level `export` statement exports.
    pub(super) fn collect_later_exports(&mut self, root: Node<'t>) {
        for i in 0..root.named_child_count() {
            let Some(stmt) = root.named_child(i) else { continue };
            if stmt.kind() != "export_statement" || stmt.child_by_field_name("source").is_some() {
                continue;
            }
            // `export default NAME;` / `export = NAME;` name a declaration.
            // `export default <expression>` binds none: a nodeless `default`
            // row records that the module still exports something.
            if let Some(value) = stmt.child_by_field_name("value") {
                if value.kind() == "identifier" {
                    let name = self.text(value).to_string();
                    self.later_exports.entry(name).or_insert(("default".to_string(), EXPORT_ESM_DEFAULT));
                } else if !matches!(value.kind(), "function_declaration" | "class_declaration" | "generator_function_declaration") {
                    self.push_default_export_row(stmt);
                }
                continue;
            }
            if stmt.child_by_field_name("declaration").is_none() && self.has_keyword_child(stmt, "default") {
                self.push_default_export_row(stmt);
                continue;
            }
            // `export { a, b as c, d as default }`
            let clause = named_kids(stmt)
                .find(|c| c.kind() == "export_clause");
            let Some(clause) = clause else { continue };
            for j in 0..clause.named_child_count() {
                let Some(spec) = clause.named_child(j) else { continue };
                if spec.kind() != "export_specifier" {
                    continue;
                }
                let Some(name_node) = spec.child_by_field_name("name").or_else(|| spec.named_child(0)) else { continue };
                let name = self.text(name_node).to_string();
                let exported_as = spec
                    .child_by_field_name("alias")
                    .map(|a| self.text(a).to_string())
                    .unwrap_or_else(|| name.clone());
                let form = if exported_as == "default" { EXPORT_ESM_DEFAULT } else { EXPORT_ESM_LATER };
                self.later_exports.entry(name).or_insert((exported_as, form));
            }
        }
    }

    /// CommonJS export assignments, keyed by the LOCAL name they
    /// export: `module.exports = { a, b: c }` (`cjs-object`), `module.exports =
    /// NAME` (as `default`), `exports.x = NAME` / `exports['x'] = NAME` /
    /// `module.exports.x = NAME` (`cjs`). A function-valued right side is the
    /// walk's own exported node and needs no entry here.
    pub(super) fn collect_cjs_export(&mut self, expr: Node<'t>) {
        let (Some(left), Some(right)) = (expr.child_by_field_name("left"), expr.child_by_field_name("right")) else { return };
        let left_text: String = self.text(left).chars().filter(|c| !c.is_whitespace()).collect();
        if left_text == "module.exports" {
            if right.kind() == "object" {
                for j in 0..right.named_child_count() {
                    let Some(prop) = right.named_child(j) else { continue };
                    match prop.kind() {
                        "shorthand_property_identifier" => {
                            let name = self.text(prop).to_string();
                            self.later_exports.entry(name.clone()).or_insert((name, EXPORT_CJS_OBJECT));
                        }
                        "pair" => {
                            let key = prop.child_by_field_name("key").map(|k| self.text(k).trim_matches(|c| c == '\'' || c == '"').to_string());
                            let value = prop.child_by_field_name("value").filter(|v| v.kind() == "identifier");
                            if let (Some(key), Some(value)) = (key, value) {
                                self.later_exports.entry(self.text(value).to_string()).or_insert((key, EXPORT_CJS_OBJECT));
                            }
                        }
                        _ => {}
                    }
                }
            } else if right.kind() == "identifier" {
                self.later_exports.entry(self.text(right).to_string()).or_insert(("default".to_string(), EXPORT_CJS));
            }
            return;
        }
        let fn_valued = matches!(right.kind(), "function_expression" | "arrow_function" | "generator_function");
        if (right.kind() != "identifier" && !fn_valued) || !matches!(left.kind(), "member_expression" | "subscript_expression") {
            return;
        }
        let Some(obj) = left.child_by_field_name("object") else { return };
        let obj_text: String = self.text(obj).chars().filter(|c| !c.is_whitespace()).collect();
        if obj_text != "exports" && obj_text != "module.exports" {
            return;
        }
        let key = left
            .child_by_field_name("property")
            .or_else(|| left.child_by_field_name("index"))
            .map(|k| self.text(k).trim_matches(|c| c == '\'' || c == '"').to_string());
        if let Some(key) = key {
            if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
                if fn_valued {
                    self.cjs_fn_exports.push((key, self.line_of(expr)));
                } else {
                    self.later_exports.entry(self.text(right).to_string()).or_insert((key, EXPORT_CJS));
                }
            }
        }
    }

    /// A `default` row with no node: `export default defineConfig({ … })`.
    pub(super) fn push_default_export_row(&mut self, stmt: Node<'t>) {
        let name_ref = self.arena.put("default");
        let line = self.line_of(stmt);
        let line_count = self.line_count;
        self.tables.push_binding(&BindingRow {
            kind: BINDING_DECL,
            export_form: EXPORT_ESM_DEFAULT,
            node_idx: NONE,
            scope_start: 1,
            scope_end: line_count,
            name: name_ref,
            target_spec: NONE_STR,
            target_name: NONE_STR,
            exported_as: name_ref,
            storage: NONE_STR,
            line,
        });
    }

    /// The (exported-as, form) a later statement gives `name`, or none.
    pub(super) fn later_export_of(&mut self, name: &str) -> (StrRef, u8) {
        match self.later_exports.get(name).cloned() {
            Some((alias, form)) => (self.arena.put(&alias), form),
            None => (NONE_STR, EXPORT_NONE),
        }
    }

    /// True when the pre-walk already emitted a binding row for `name` on `line`.
    pub(super) fn has_scoped_row(&self, name: &str, line: u32) -> bool {
        self.scoped_rows.get(&line).is_some_and(|names| names.iter().any(|n| n == name))
    }

    /// Records `name`@`line` as emitted; false when it already was.
    pub(super) fn mark_scoped_row(&mut self, name: &str, line: u32) -> bool {
        let names = self.scoped_rows.entry(line).or_default();
        if names.iter().any(|n| n == name) {
            return false;
        }
        names.push(name.to_string());
        true
    }

    /// The `require()` spec a module-level declarator on `line` binds `name` to.
    pub(super) fn import_decl_spec(&self, name: &str, line: u32) -> Option<&str> {
        self.import_decls
            .get(&line)?
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, spec)| spec.as_str())
    }

    /// Records the spec (a later declarator of the same name on the same line
    /// replaces it, as a keyed insert did).
    pub(super) fn set_import_decl(&mut self, name: String, line: u32, spec: String) {
        let entries = self.import_decls.entry(line).or_default();
        match entries.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = spec,
            None => entries.push((name, spec)),
        }
    }

    /// Replaces the regex `is_exported_later`: same question, answered from the AST.
    pub(super) fn is_exported_later(&self, name: &str) -> bool {
        self.later_exports.contains_key(name)
    }

    /// A `decl` row for a module-scope declaration, a `local` row for one nested
    /// in a function/class body. `export_statement` ancestry is `esm`; a
    /// declaration exported by a later statement takes that statement's form;
    /// an exported flag with neither is the CommonJS assignment path.
    pub(super) fn emit_decl_binding(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>, exported_flag: bool) {
        if matches!(kind, "file" | "import") {
            return;
        }
        let line = self.line_of(node);
        if self.has_scoped_row(name, line) {
            return;
        }
        let require_spec = self.import_decl_spec(name, line).map(str::to_string);
        // Scope from the AST, not the node stack: an IIFE or a callback has no
        // node, so a function declared inside one is still nested. The same
        // rule the pre-walk and the AST-only emitter use (bindings.rs).
        let enclosing = self.enclosing_scope(node);
        let at_module_scope = enclosing.is_none();
        let (scope_start, scope_end) = enclosing.unwrap_or((1, self.line_count));
        let mut exported_as: Option<String> = None;
        let mut form = EXPORT_NONE;
        if at_module_scope {
            if exported_flag && self.is_exported(node) {
                // `export default function NAME` / `class NAME` exports the name
                // as `default`: the declaration itself is the statement's value.
                // A member nested in a default-exported object literal is not.
                let is_default = self
                    .export_statement_of(node)
                    .map(|e| self.has_keyword_child(e, "default") && e.child_by_field_name("declaration") == Some(node))
                    .unwrap_or(false);
                exported_as = Some(if is_default { "default".to_string() } else { name.to_string() });
                form = if is_default { EXPORT_ESM_DEFAULT } else { EXPORT_ESM };
            } else if self.in_declare_global(node) {
                // `declare global { … }` contributes the name to every file.
                exported_as = Some(name.to_string());
                form = EXPORT_PUBLIC;
            } else if let Some((alias, f)) = self.later_exports.get(name) {
                exported_as = Some(alias.clone());
                form = *f;
            } else if exported_flag || self.cjs_fn_exports.iter().any(|(n, l)| n == name && *l == line) {
                exported_as = Some(name.to_string());
                form = EXPORT_CJS;
            }
        }
        let name_ref = self.arena.put(name);
        let exported_ref = self.arena.put_opt(exported_as.as_deref());
        let (kind_code, target_spec, target_name) = match require_spec {
            Some(spec) => (BINDING_IMPORT, self.arena.put(&spec), self.arena.put("default")),
            None => (if at_module_scope { BINDING_DECL } else { BINDING_LOCAL }, NONE_STR, NONE_STR),
        };
        self.tables.push_binding(&BindingRow {
            kind: kind_code,
            export_form: form,
            node_idx: row,
            scope_start,
            scope_end,
            name: name_ref,
            target_spec,
            target_name,
            exported_as: exported_ref,
            storage: NONE_STR,
            line,
        });
    }

    /// An `import` row: the local name, the specifier, and the imported name.
    /// An imported name a later clause exports (`import X from './x'; export
    /// { X }`) carries that export like a declaration would.
    pub(super) fn emit_import_binding(&mut self, local: &str, spec: &str, imported: &str, node: Node<'t>) {
        let name_ref = self.arena.put(local);
        let spec_ref = self.arena.put(spec);
        let imported_ref = self.arena.put(imported);
        let line = self.line_of(node);
        let line_count = self.line_count;
        let (exported_as, form) = self.later_export_of(local);
        self.tables.push_binding(&BindingRow {
            kind: BINDING_IMPORT,
            export_form: form,
            node_idx: NONE,
            scope_start: 1,
            scope_end: line_count,
            name: name_ref,
            target_spec: spec_ref,
            target_name: imported_ref,
            exported_as,
            storage: NONE_STR,
            line,
        });
    }

    /// A `reexport` row: `export { name as alias } from spec`.
    pub(super) fn emit_reexport_binding(&mut self, name: &str, exported_as: &str, spec: &str, node: Node<'t>) {
        let name_ref = self.arena.put(name);
        let spec_ref = self.arena.put(spec);
        let exported_ref = self.arena.put(exported_as);
        let line = self.line_of(node);
        let line_count = self.line_count;
        self.tables.push_binding(&BindingRow {
            kind: BINDING_REEXPORT,
            export_form: EXPORT_ESM,
            node_idx: NONE,
            scope_start: 1,
            scope_end: line_count,
            name: name_ref,
            target_spec: spec_ref,
            target_name: name_ref,
            exported_as: exported_ref,
            storage: NONE_STR,
            line,
        });
    }
}
