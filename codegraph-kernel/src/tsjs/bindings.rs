//! Scoped binding rows the node walk does not produce
//! (docs/design/resolution-binding-model-plan.md, Phase 2).
//!
//! The walk emits a `decl`/`local` row for every node it creates. The names
//! that shadow a cross-file symbol without ever becoming a node — parameters,
//! a function-body `const x = await load()`, a `catch (e)` — and the CommonJS
//! import forms (`const m = require('./m')`, `const { a, b: c } = require(..)`)
//! come from this pre-walk instead. It runs before the node walk so the walk
//! can skip a `decl` row for a name the pre-walk already classed as an import.

use super::{is_function_type, Walker};
use crate::buffers::{BindingRow, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, EXPORT_NONE, NONE, NONE_STR};
use tree_sitter::Node;

impl<'t> Walker<'t> {
    pub(super) fn collect_scoped_bindings(&mut self, root: Node<'t>) {
        self.scan_scoped(root, None);
    }

    /// `scope`: the line range of the innermost enclosing function body or
    /// block (None at module level, where the node walk owns declarations).
    fn scan_scoped(&mut self, node: Node<'t>, scope: Option<(u32, u32)>) {
        stack_guard!();
        let kind = node.kind();
        let mut child_scope = scope;
        if is_function_type(kind) || kind == "method_definition" {
            let range = self.line_range(node);
            child_scope = Some(range);
            if let Some(params) = node.child_by_field_name("parameters") {
                for i in 0..params.named_child_count() {
                    if let Some(p) = params.named_child(i) {
                        self.emit_pattern_bindings(p, BINDING_PARAM, range);
                    }
                }
            } else if let Some(p) = node.child_by_field_name("parameter") {
                self.emit_pattern_bindings(p, BINDING_PARAM, range);
            }
        } else if kind == "catch_clause" {
            let range = self.line_range(node);
            child_scope = Some(range);
            if let Some(p) = node.child_by_field_name("parameter") {
                self.emit_pattern_bindings(p, BINDING_PARAM, range);
            }
        } else if matches!(kind, "statement_block" | "class_body") && scope.is_none() {
            child_scope = Some(self.line_range(node));
        } else if kind == "variable_declarator" {
            self.scan_declarator(node, scope);
        } else if kind == "assignment_expression" {
            // CommonJS exports wherever they sit: a template interpolation or a
            // function body executes `module.exports = …` just the same.
            self.collect_cjs_export(node);
        }
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.scan_scoped(c, child_scope);
            }
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
