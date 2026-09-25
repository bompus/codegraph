//! `use` declarations: import refs and the binding rows each `use` tree leaf produces.

use super::*;

impl<'t> Walker<'t> {
    /// extractImport via the rust hook: import node named by the ROOT module +
    /// one generic root `imports` ref + per-binding FULL-path refs.
    /// `use x::*;` (use_wildcard) → hook returns null → nothing at all.
    pub(super) fn extract_import(&mut self, node: Node<'t>) {
        let use_arg = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| matches!(c.kind(), "scoped_use_list" | "scoped_identifier" | "use_list" | "identifier"));
        let Some(use_arg) = use_arg else { return };

        let module_name = self.root_module(use_arg);
        let signature = self.text(node).trim().to_string();
        self.create_node(
            "import",
            &module_name,
            node,
            Extra { signature: Some(signature), ..Extra::default() },
        );
        let parent = self.top_row();
        let imports_kind = crate::buffers::EDGE_IMPORTS;
        if !module_name.is_empty() {
            self.push_ref_at(parent, &module_name, imports_kind, node);
        }
        self.emit_use_binding_refs(node, parent);
    }

    /// getRootModule (languages/rust.ts:124).
    pub(super) fn root_module(&self, n: Node) -> String {
        stack_guard!();
        let Some(first) = n.named_child(0) else {
            return self.text(n).to_string();
        };
        match first.kind() {
            "identifier" | "crate" | "super" | "self" => self.text(first).to_string(),
            "scoped_identifier" => self.root_module(first),
            _ => self.text(first).to_string(),
        }
    }

    /// emitRustUseBindingRefs (tree-sitter.ts:3451) — one FULL-path `imports`
    /// ref per binding; `Path as Alias` links the source path; leaves that are
    /// only `self`/`super`/`crate`/`*` are skipped.
    pub(super) fn emit_use_binding_refs(&mut self, node: Node<'t>, from_row: u32) {
        let mut paths: Vec<(String, Node)> = Vec::new();
        fn join(prefix: &str, seg: &str) -> String {
            if prefix.is_empty() { seg.to_string() } else { format!("{prefix}::{seg}") }
        }
        fn collect<'t>(w: &Walker<'t>, n: Node<'t>, prefix: &str, paths: &mut Vec<(String, Node<'t>)>) {
            stack_guard!();
            match n.kind() {
                "identifier" => paths.push((join(prefix, w.text(n)), n)),
                "scoped_identifier" => {
                    let full = w.text(n).trim();
                    paths.push((
                        if prefix.is_empty() { full.to_string() } else { format!("{prefix}::{full}") },
                        n,
                    ));
                }
                "scoped_use_list" => {
                    let seg = n
                        .child_by_field_name("path")
                        .map(|p| w.text(p).trim().to_string())
                        .unwrap_or_default();
                    let new_prefix = if seg.is_empty() { prefix.to_string() } else { join(prefix, &seg) };
                    let list = n.child_by_field_name("list").or_else(|| {
                        (0..n.named_child_count())
                            .filter_map(|i| n.named_child(i))
                            .find(|c| c.kind() == "use_list")
                    });
                    if let Some(list) = list {
                        collect(w, list, &new_prefix, paths);
                    }
                }
                "use_list" => {
                    for i in 0..n.named_child_count() {
                        if let Some(c) = n.named_child(i) {
                            collect(w, c, prefix, paths);
                        }
                    }
                }
                "use_as_clause" => {
                    let p = n.child_by_field_name("path").or_else(|| n.named_child(0));
                    if let Some(p) = p {
                        collect(w, p, prefix, paths);
                    }
                }
                _ => {} // visibility_modifier, use_wildcard, bare crate/self/super
            }
        }
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                collect(self, c, "", &mut paths);
            }
        }
        let imports_kind = crate::buffers::EDGE_IMPORTS;
        for (text, n) in paths {
            let leaf = text.rsplit("::").next().unwrap_or("");
            if leaf.is_empty() || matches!(leaf, "self" | "super" | "crate" | "*") {
                continue;
            }
            self.push_ref_at(from_row, &text, imports_kind, n);
        }
    }

    /// `use`-binding rows (resolution-binding-model-plan.md §2.1): one
    /// `import` row per bound local name — the path leaf, or the `as` alias —
    /// with `target_spec` the full `::` path as written and `target_name` its
    /// leaf. `use a::{b::{c, d}}` flattens through nested `use_list`s; `self`
    /// inside a list binds the parent path (`use a::b::{self}` → `b`); a `*`
    /// glob emits a `name="*"` row that records the import but binds no name —
    /// the resolver declines it. A `pub use` (any `visibility_modifier`)
    /// carries `export_form=public` + `exported_as`, so a consumer's leaf
    /// lookup can chase the re-export one hop.
    ///
    /// Scope is the use's PARENT extent — `source_file` for a top-level use,
    /// the `declaration_list` for a `mod` body, the `block` for a
    /// function-local use — the same region Rust scopes the import to.
    /// Emitted on BOTH walks (`visit_node` for items, the calls/structure
    /// walk for function bodies): the row is the whole contribution — a
    /// function-local `use` still emits no import node or `imports` refs.
    pub(super) fn emit_use_bindings(&mut self, node: Node<'t>) {
        fn join(prefix: &str, seg: &str) -> String {
            if prefix.is_empty() {
                seg.to_string()
            } else {
                format!("{prefix}::{seg}")
            }
        }
        fn last_seg(p: &str) -> &str {
            p.rsplit("::").next().unwrap_or(p)
        }
        /// One row per (spec, local-name-node) pair; the local name's own
        /// token supplies the line.
        fn collect<'t>(
            w: &Walker<'t>,
            n: Node<'t>,
            prefix: &str,
            out: &mut Vec<(String, String, Node<'t>)>,
        ) {
            stack_guard!();
            match n.kind() {
                "identifier" => {
                    let seg = w.text(n);
                    out.push((join(prefix, seg), seg.to_string(), n));
                }
                "scoped_identifier" => {
                    let full = w.text(n).trim();
                    let spec = if prefix.is_empty() {
                        full.to_string()
                    } else {
                        format!("{prefix}::{full}")
                    };
                    let local = last_seg(&spec).to_string();
                    out.push((spec, local, n));
                }
                "use_as_clause" => {
                    let p = n.child_by_field_name("path").or_else(|| n.named_child(0));
                    let a = n.child_by_field_name("alias");
                    if let (Some(p), Some(a)) = (p, a) {
                        // `use a::b::{self as io}` binds `io` to `a::b` —
                        // `self` in a list names the prefix, not a segment.
                        let spec = if p.kind() == "self" && !prefix.is_empty() {
                            prefix.to_string()
                        } else {
                            let ptext = w.text(p).trim();
                            if prefix.is_empty() {
                                ptext.to_string()
                            } else {
                                format!("{prefix}::{ptext}")
                            }
                        };
                        out.push((spec, w.text(a).trim().to_string(), a));
                    } else if let Some(p) = p {
                        collect(w, p, prefix, out);
                    }
                }
                // `use a::b::{self}` binds `b` — the prefix's own leaf.
                "self" | "super" | "crate" if !prefix.is_empty() => {
                    out.push((prefix.to_string(), last_seg(prefix).to_string(), n));
                }
                "use_list" => {
                    for i in 0..n.named_child_count() {
                        if let Some(c) = n.named_child(i) {
                            collect(w, c, prefix, out);
                        }
                    }
                }
                "scoped_use_list" => {
                    let seg = n
                        .child_by_field_name("path")
                        .map(|p| w.text(p).trim().to_string())
                        .unwrap_or_default();
                    let new_prefix = if seg.is_empty() {
                        prefix.to_string()
                    } else {
                        join(prefix, &seg)
                    };
                    let list = n.child_by_field_name("list").or_else(|| {
                        (0..n.named_child_count())
                            .filter_map(|i| n.named_child(i))
                            .find(|c| c.kind() == "use_list")
                    });
                    if let Some(list) = list {
                        collect(w, list, &new_prefix, out);
                    }
                }
                // `use a::*` / `use a::{b::*}` — a glob records its module
                // path under the never-matching name `*`; the resolver
                // declines it, so the wildcard binds nothing.
                "use_wildcard" => {
                    let path_text = (0..n.named_child_count())
                        .filter_map(|i| n.named_child(i))
                        .map(|c| w.text(c).trim().to_string())
                        .next()
                        .unwrap_or_default();
                    let spec = join(prefix, &path_text);
                    if !spec.is_empty() {
                        out.push((format!("{spec}::*"), "*".to_string(), n));
                    }
                }
                _ => {} // visibility_modifier, bare crate/self/super at the root
            }
        }

        let is_pub = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .any(|c| c.kind() == "visibility_modifier");
        let parent = node.parent().unwrap_or(node);
        let scope = (
            parent.start_position().row as u32 + 1,
            parent.end_position().row as u32 + 1,
        );
        let mut out: Vec<(String, String, Node<'t>)> = Vec::new();
        if let Some(arg) = node.child_by_field_name("argument") {
            collect(self, arg, "", &mut out);
        }
        for (spec, local, leaf_node) in out {
            // `self`/`super`/`crate` can't be bound names at a use site.
            if local.is_empty() || matches!(local.as_str(), "self" | "super" | "crate") {
                continue;
            }
            let exported = is_pub && local != "*";
            let (name_ref, spec_ref, target_ref) = (
                self.arena.put(&local),
                self.arena.put(&spec),
                self.arena.put(last_seg(&spec)),
            );
            self.tables.push_binding(&BindingRow {
                kind: BINDING_IMPORT,
                export_form: if exported { EXPORT_PUBLIC } else { EXPORT_NONE },
                node_idx: NONE,
                scope_start: scope.0,
                scope_end: scope.1,
                name: name_ref,
                target_spec: spec_ref,
                target_name: target_ref,
                exported_as: if exported { name_ref } else { NONE_STR },
                storage: NONE_STR,
                line: self.line_of(leaf_node),
            });
        }
    }
}
