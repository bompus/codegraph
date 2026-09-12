//! Scoped binding rows the node walk does not produce
//! (docs/design/resolution-binding-model-plan.md, Phase 2).
//!
//! The walk emits a `decl`/`local` row for every node it creates. The names
//! that shadow a cross-file symbol without ever becoming a node — parameters,
//! a function-body `const x = await load()`, a `catch (e)` — and the CommonJS
//! import forms (`const m = require('./m')`, `const { a, b: c } = require(..)`)
//! come from this pre-walk instead. It runs before the node walk so the walk
//! can skip a `decl` row for a name the pre-walk already classed as an import.
//!
//! The same pre-walks serve `bindings_only` (mod.rs): rows for a TS/JS file
//! the walker does not extract (a stack-guard defer, ArkTS), where the
//! declaration and import rows come from the AST alone (`collect_decl_rows`,
//! `collect_import_rows`) and the TS side attaches node ids by name and line.
//! Every walk here is iterative, so a file too deep for the recursive walker
//! still gets its rows.

use super::{is_function_type, is_scope_block, Walker};
use crate::buffers::{
    BindingRow, BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, EXPORT_ESM, EXPORT_ESM_DEFAULT, EXPORT_NONE,
    EXPORT_PUBLIC, NONE, NONE_STR,
};
use tree_sitter::Node;

const DECL_KINDS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "interface_declaration",
    "enum_declaration",
    "type_alias_declaration",
    "internal_module",
    "module",
];

impl<'t> Walker<'t> {
    pub(super) fn collect_scoped_bindings(&mut self, root: Node<'t>) {
        let mut stack: Vec<(Node<'t>, Option<(u32, u32)>)> = vec![(root, None)];
        while let Some((node, scope)) = stack.pop() {
            let child_scope = self.scan_scoped(node, scope);
            for i in (0..node.named_child_count()).rev() {
                if let Some(c) = node.named_child(i) {
                    stack.push((c, child_scope));
                }
            }
        }
    }

    /// One node of the scoped pre-walk. `scope`: the line range of the
    /// innermost enclosing function body or block (None at module level,
    /// where the node walk owns declarations). Returns the scope for the
    /// node's children.
    fn scan_scoped(&mut self, node: Node<'t>, scope: Option<(u32, u32)>) -> Option<(u32, u32)> {
        let kind = node.kind();
        if is_function_type(kind) || kind == "method_definition" {
            let range = self.line_range(node);
            if let Some(params) = node.child_by_field_name("parameters") {
                for i in 0..params.named_child_count() {
                    if let Some(p) = params.named_child(i) {
                        self.emit_pattern_bindings(p, BINDING_PARAM, range);
                    }
                }
            } else if let Some(p) = node.child_by_field_name("parameter") {
                self.emit_pattern_bindings(p, BINDING_PARAM, range);
            }
            return Some(range);
        }
        if kind == "catch_clause" {
            let range = self.line_range(node);
            if let Some(p) = node.child_by_field_name("parameter") {
                self.emit_pattern_bindings(p, BINDING_PARAM, range);
            }
            return Some(range);
        }
        if is_scope_block(node) && scope.is_none() {
            return Some(self.line_range(node));
        }
        if kind == "variable_declarator" {
            self.scan_declarator(node, scope);
        } else if kind == "assignment_expression" {
            // CommonJS exports wherever they sit: a template interpolation or a
            // function body executes `module.exports = …` just the same.
            self.collect_cjs_export(node);
        }
        scope
    }

    /// AST-only declaration rows for `bindings_only`: a `decl` row for every
    /// module-level declaration (with its export form), an `import` row for a
    /// module-level `require` declarator, and a `local` row for a function or
    /// class declared inside another. The TS side attaches node ids.
    pub(super) fn collect_decl_rows(&mut self, root: Node<'t>) {
        let mut stack: Vec<(Node<'t>, Option<(u32, u32)>)> = vec![(root, None)];
        while let Some((node, scope)) = stack.pop() {
            let kind = node.kind();
            let mut child_scope = scope;
            if DECL_KINDS.contains(&kind) {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = self.text(name_node).to_string();
                    match scope {
                        None => self.push_ast_decl_row(&name, node),
                        Some(range) => self.push_scoped_row(&name, BINDING_LOCAL, node, range),
                    }
                }
            }
            if is_function_type(kind) || kind == "method_definition" || kind == "catch_clause" {
                child_scope = Some(self.line_range(node));
            } else if is_scope_block(node) && scope.is_none() {
                child_scope = Some(self.line_range(node));
            } else if kind == "variable_declarator" && scope.is_none() {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if name_node.kind() == "identifier" {
                        let name = self.text(name_node).to_string();
                        let line = self.line_of(name_node);
                        if let Some(spec) = self.import_decls.get(&(name.clone(), line)).cloned() {
                            self.push_import_row(&name, &spec, "default", name_node, (1, self.line_count));
                        } else {
                            self.push_ast_decl_row(&name, node);
                        }
                    }
                }
            }
            for i in (0..node.named_child_count()).rev() {
                if let Some(c) = node.named_child(i) {
                    stack.push((c, child_scope));
                }
            }
        }
        // `exports.x = function () {}`: the walker names that node `x`; here
        // the row is nodeless and the TS side attaches the node by name/line.
        let fn_exports = std::mem::take(&mut self.cjs_fn_exports);
        for (name, line) in fn_exports {
            let name_ref = self.arena.put(&name);
            let line_count = self.line_count;
            self.tables.push_binding(&BindingRow {
                kind: BINDING_DECL,
                export_form: crate::buffers::EXPORT_CJS,
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
    }

    /// A module-level `decl` row from the AST, with the export form the walker
    /// would have given the node.
    fn push_ast_decl_row(&mut self, name: &str, node: Node<'t>) {
        let line = self.line_of(node);
        if !self.scoped_rows.insert((name.to_string(), line)) {
            return;
        }
        let (exported_as, form) = if let Some(e) = self.export_statement_of(node) {
            let direct = e.child_by_field_name("declaration").map(|d| d == node || d == node.parent().unwrap_or(node)).unwrap_or(false);
            let is_default = self.has_keyword_child(e, "default") && e.child_by_field_name("declaration") == Some(node);
            if direct || e.child_by_field_name("declaration").is_some() {
                (Some(if is_default { "default".to_string() } else { name.to_string() }), if is_default { EXPORT_ESM_DEFAULT } else { EXPORT_ESM })
            } else {
                (None, EXPORT_NONE)
            }
        } else if let Some((alias, f)) = self.later_exports.get(name).cloned() {
            (Some(alias), f)
        } else if self.in_declare_global(node) {
            (Some(name.to_string()), EXPORT_PUBLIC)
        } else {
            (None, EXPORT_NONE)
        };
        let name_ref = self.arena.put(name);
        let exported_ref = match exported_as { Some(a) => self.arena.put(&a), None => NONE_STR };
        let line_count = self.line_count;
        self.tables.push_binding(&BindingRow {
            kind: BINDING_DECL,
            export_form: form,
            node_idx: NONE,
            scope_start: 1,
            scope_end: line_count,
            name: name_ref,
            target_spec: NONE_STR,
            target_name: NONE_STR,
            exported_as: exported_ref,
            storage: NONE_STR,
            line,
        });
    }

    /// AST-only import and re-export rows for `bindings_only`: the same rows
    /// `emit_import_binding_refs` / `emit_re_export_refs` produce during the
    /// walk, without the `imports` refs.
    pub(super) fn collect_import_rows(&mut self, root: Node<'t>) {
        for i in 0..root.named_child_count() {
            let Some(stmt) = root.named_child(i) else { continue };
            match stmt.kind() {
                "import_statement" => self.import_rows_of(stmt),
                "export_statement" if stmt.child_by_field_name("source").is_some() => self.reexport_rows_of(stmt),
                _ => {}
            }
        }
    }

    fn spec_of(&self, stmt: Node<'t>) -> String {
        stmt.child_by_field_name("source")
            .map(|s| self.text(s).chars().filter(|c| *c != '\'' && *c != '"').collect())
            .unwrap_or_default()
    }

    fn import_rows_of(&mut self, node: Node<'t>) {
        let clause = (0..node.named_child_count()).filter_map(|i| node.named_child(i)).find(|c| c.kind() == "import_clause");
        let Some(clause) = clause else { return };
        let spec = self.spec_of(node);
        for i in 0..clause.named_child_count() {
            let Some(child) = clause.named_child(i) else { continue };
            match child.kind() {
                "identifier" => {
                    let name = self.text(child).to_string();
                    self.emit_import_binding(&name, &spec, "default", child);
                }
                "named_imports" => {
                    for j in 0..child.named_child_count() {
                        let Some(s) = child.named_child(j) else { continue };
                        if s.kind() != "import_specifier" {
                            continue;
                        }
                        let original = s.child_by_field_name("name").or_else(|| s.named_child(0));
                        let imported = original.map(|o| self.text(o).to_string()).unwrap_or_default();
                        if let Some(n) = s.child_by_field_name("alias").or(original) {
                            let name = self.text(n).to_string();
                            if !name.is_empty() {
                                self.emit_import_binding(&name, &spec, &imported, n);
                            }
                        }
                    }
                }
                "namespace_import" => {
                    let n = (0..child.named_child_count()).filter_map(|k| child.named_child(k)).find(|c| c.kind() == "identifier").or_else(|| child.named_child(0));
                    if let Some(n) = n {
                        let name = self.text(n).to_string();
                        self.emit_import_binding(&name, &spec, "*", n);
                    }
                }
                _ => {}
            }
        }
    }

    fn reexport_rows_of(&mut self, node: Node<'t>) {
        let spec = self.spec_of(node);
        if spec.is_empty() {
            return;
        }
        let clause = (0..node.named_child_count()).filter_map(|i| node.named_child(i)).find(|c| c.kind() == "export_clause");
        let Some(clause) = clause else {
            let ns = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| c.kind() == "namespace_export")
                .and_then(|ne| (0..ne.named_child_count()).filter_map(|k| ne.named_child(k)).find(|c| c.kind() == "identifier"))
                .map(|id| self.text(id).to_string());
            self.emit_reexport_binding("*", ns.as_deref().unwrap_or("*"), &spec, node);
            return;
        };
        for i in 0..clause.named_child_count() {
            let Some(s) = clause.named_child(i) else { continue };
            if s.kind() != "export_specifier" {
                continue;
            }
            let Some(n) = s.child_by_field_name("name").or_else(|| s.named_child(0)) else { continue };
            let name = self.text(n).to_string();
            if name.is_empty() {
                continue;
            }
            let exported_as = s.child_by_field_name("alias").map(|a| self.text(a).to_string()).unwrap_or_else(|| name.clone());
            self.emit_reexport_binding(&name, &exported_as, &spec, n);
        }
    }

    fn scan_declarator(&mut self, node: Node<'t>, scope: Option<(u32, u32)>) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let value = node.child_by_field_name("value");
        let range = scope.unwrap_or((1, self.line_count));
        // `require('./m')` / `await import('./m')`: an import, whatever the scope.
        if let Some(spec) = value.and_then(|v| self.require_spec(v)) {
            match name_node.kind() {
                "identifier" => {
                    let name = self.text(name_node).to_string();
                    if scope.is_none() {
                        // The walk creates a node for a module-level declarator;
                        // its row becomes the `import` row (it may be re-exported).
                        self.import_decls.insert((name, self.line_of(name_node)), spec);
                    } else {
                        self.push_import_row(&name, &spec, "default", name_node, range);
                        self.scoped_rows.insert((name, self.line_of(name_node)));
                    }
                }
                "object_pattern" => {
                    for i in 0..name_node.named_child_count() {
                        let Some(p) = name_node.named_child(i) else { continue };
                        match p.kind() {
                            "shorthand_property_identifier_pattern" => {
                                let name = self.text(p).to_string();
                                self.push_import_row(&name, &spec, &name, p, range);
                            }
                            "pair_pattern" => {
                                let key = p.child_by_field_name("key").map(|k| self.text(k).trim_matches(|c| c == '\'' || c == '"').to_string());
                                let local = p.child_by_field_name("value").filter(|v| v.kind() == "identifier");
                                if let (Some(key), Some(local)) = (key, local) {
                                    let name = self.text(local).to_string();
                                    self.push_import_row(&name, &spec, &key, local, range);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        // Module-level declarations are nodes; the walk emits their rows.
        let Some(scope) = scope else { return };
        // Function-valued declarators become function nodes in the walk.
        if let Some(v) = value {
            if matches!(v.kind(), "arrow_function" | "function_expression" | "generator_function" | "class") {
                return;
            }
        }
        if name_node.kind() != "identifier" {
            // `const { fetchUser } = useStore.getState()` re-names a member of
            // something defined elsewhere; the graph's symbol is what a call
            // through it means, so a destructured name is not a local binding.
            return;
        }
        let name = self.text(name_node);
        // `const setZipUri = useStore((s) => s.setZipUri)`: a selector picks a
        // same-named member out of a store; the store action is what it means.
        if let Some(v) = value {
            if self.is_selector_of(v, name) {
                return;
            }
        }
        let name = name.to_string();
        self.push_scoped_row(&name, BINDING_LOCAL, name_node, scope);
    }

    /// Identifiers a parameter pattern binds: the plain name, the left side of
    /// a default, the rest name, and every leaf of an object/array pattern.
    fn emit_pattern_bindings(&mut self, p: Node<'t>, kind: u8, range: (u32, u32)) {
        match p.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                let name = self.text(p).to_string();
                self.push_scoped_row(&name, kind, p, range);
            }
            "required_parameter" | "optional_parameter" => {
                if let Some(pat) = p.child_by_field_name("pattern") {
                    self.emit_pattern_bindings(pat, kind, range);
                }
            }
            "assignment_pattern" => {
                if let Some(left) = p.child_by_field_name("left") {
                    self.emit_pattern_bindings(left, kind, range);
                }
            }
            "rest_pattern" => {
                if let Some(inner) = p.named_child(0) {
                    self.emit_pattern_bindings(inner, kind, range);
                }
            }
            "object_pattern" | "array_pattern" => {
                for i in 0..p.named_child_count() {
                    let Some(c) = p.named_child(i) else { continue };
                    match c.kind() {
                        "shorthand_property_identifier_pattern" => {
                            let name = self.text(c).to_string();
                            self.push_scoped_row(&name, kind, c, range);
                        }
                        "pair_pattern" => {
                            if let Some(v) = c.child_by_field_name("value") {
                                self.emit_pattern_bindings(v, kind, range);
                            }
                        }
                        "object_assignment_pattern" => {
                            if let Some(left) = c.child_by_field_name("left") {
                                self.emit_pattern_bindings(left, kind, range);
                            }
                        }
                        _ => self.emit_pattern_bindings(c, kind, range),
                    }
                }
            }
            _ => {}
        }
    }

    /// The specifier of `require('x')`, `import('x')`, or either under `await`.
    fn require_spec(&self, value: Node<'t>) -> Option<String> {
        let call = if value.kind() == "await_expression" { value.named_child(0)? } else { value };
        if call.kind() != "call_expression" {
            return None;
        }
        let callee = call.child_by_field_name("function")?;
        let is_require = callee.kind() == "identifier" && self.text(callee) == "require";
        if !is_require && callee.kind() != "import" {
            return None;
        }
        let args = call.child_by_field_name("arguments")?;
        let first = args.named_child(0)?;
        if first.kind() != "string" {
            return None;
        }
        let spec: String = self.text(first).chars().filter(|c| *c != '\'' && *c != '"').collect();
        if spec.is_empty() { None } else { Some(spec) }
    }

    /// `f((s) => s.NAME)`: an arrow argument whose body is `NAME` picked off
    /// its own single parameter.
    fn is_selector_of(&self, value: Node<'t>, name: &str) -> bool {
        if value.kind() != "call_expression" {
            return false;
        }
        let Some(args) = value.child_by_field_name("arguments") else { return false };
        for i in 0..args.named_child_count() {
            let Some(arg) = args.named_child(i) else { continue };
            if arg.kind() != "arrow_function" {
                continue;
            }
            let param = arg.child_by_field_name("parameter").or_else(|| {
                let ps = arg.child_by_field_name("parameters")?;
                if ps.named_child_count() != 1 { return None; }
                let p = ps.named_child(0)?;
                if p.kind() == "identifier" { Some(p) } else { p.child_by_field_name("pattern") }
            });
            let Some(param) = param else { continue };
            if param.kind() != "identifier" {
                continue;
            }
            let Some(body) = arg.child_by_field_name("body") else { continue };
            if body.kind() != "member_expression" {
                continue;
            }
            let obj = body.child_by_field_name("object");
            let prop = body.child_by_field_name("property");
            if let (Some(o), Some(p)) = (obj, prop) {
                if self.text(o) == self.text(param) && self.text(p) == name {
                    return true;
                }
            }
        }
        false
    }

    fn line_range(&self, node: Node<'t>) -> (u32, u32) {
        (self.line_of(node), node.end_position().row as u32 + 1)
    }

    fn push_scoped_row(&mut self, name: &str, kind: u8, node: Node<'t>, (scope_start, scope_end): (u32, u32)) {
        let line = self.line_of(node);
        if !self.scoped_rows.insert((name.to_string(), line)) {
            return;
        }
        let name_ref = self.arena.put(name);
        self.tables.push_binding(&BindingRow {
            kind,
            export_form: EXPORT_NONE,
            node_idx: NONE,
            scope_start,
            scope_end,
            name: name_ref,
            target_spec: NONE_STR,
            target_name: NONE_STR,
            exported_as: NONE_STR,
            storage: NONE_STR,
            line,
        });
    }

    fn push_import_row(&mut self, local: &str, spec: &str, imported: &str, node: Node<'t>, (scope_start, scope_end): (u32, u32)) {
        let name_ref = self.arena.put(local);
        let spec_ref = self.arena.put(spec);
        let imported_ref = self.arena.put(imported);
        let line = self.line_of(node);
        let (exported_as, form) = if scope_start == 1 { self.later_export_of(local) } else { (NONE_STR, EXPORT_NONE) };
        self.tables.push_binding(&BindingRow {
            kind: BINDING_IMPORT,
            export_form: form,
            node_idx: NONE,
            scope_start,
            scope_end,
            name: name_ref,
            target_spec: spec_ref,
            target_name: imported_ref,
            exported_as,
            storage: NONE_STR,
            line,
        });
    }
}
