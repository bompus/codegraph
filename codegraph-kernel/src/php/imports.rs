//! PHP `use` and `include`/`require`: import refs and the names they bind.

use super::*;

impl<'t> Walker<'t> {
    /// pushPhpUseRef (3563): `Foo\Bar\Baz` → an `imports` ref named
    /// `Foo\Bar::Baz`; a global-namespace name (no `\` after stripping one
    /// leading `\`) emits nothing here.
    pub(super) fn push_php_use_ref(&mut self, fqn: &str, from_row: u32, node: Node) {
        let clean = fqn.strip_prefix('\\').unwrap_or(fqn);
        let Some(last_sep) = clean.rfind('\\') else { return };
        let name = format!("{}::{}", &clean[..last_sep], &clean[last_sep + 1..]);
        self.push_ref_at(from_row, &name, crate::buffers::EDGE_IMPORTS, node);
    }

    pub(super) fn extract_import(&mut self, node: Node<'t>) {
        let kind = node.kind();
        let import_text = self.text(node).trim().to_string();
        let imports_kind = crate::buffers::EDGE_IMPORTS;

        if matches!(
            kind,
            "include_expression" | "include_once_expression" | "require_expression"
                | "require_once_expression"
        ) {
            // phpStaticIncludePath: static string literals only; dynamic
            // forms (`__DIR__ . '/x'`, interpolation) emit NOTHING.
            let mut arg = node.named_child(0);
            if let Some(a) = arg {
                if a.kind() == "parenthesized_expression" {
                    arg = a.named_child(0);
                }
            }
            let Some(arg) = arg else { return };
            if !matches!(arg.kind(), "string" | "encapsed_string") {
                return;
            }
            let mut content: Option<Node> = None;
            for i in 0..arg.named_child_count() {
                let Some(c) = arg.named_child(i) else { continue };
                if c.kind() != "string_content" {
                    return; // interpolation/escape → not a static path
                }
                if content.is_none() {
                    content = Some(c);
                }
            }
            let Some(content) = content else { return };
            let module_name = self.text(content).to_string();
            if module_name.is_empty() {
                return;
            }
            self.create_node(
                "import",
                &module_name,
                node,
                Extra { signature: Some(import_text), ..Extra::default() },
            );
            let parent = self.top_row();
            self.push_ref_at(parent, &module_name, imports_kind, node);
            return;
        }

        // namespace_use_declaration.
        let ns_prefix = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "namespace_name");
        let use_group = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "namespace_use_group");
        if let (Some(ns_prefix), Some(use_group)) = (ns_prefix, use_group) {
            // Grouped `use A\{B, C as D, Sub\E}` — hook declines, the inline
            // branch emits per-member nodes named `A\B` (first `name` child =
            // the SOURCE name; a nested `Sub\E` clause has a qualified_name,
            // no direct `name` → SKIPPED, grammar-bump delta #2). All nodes
            // and refs sit at the whole declaration's position.
            let prefix = self.text(ns_prefix).to_string();
            let clauses: Vec<Node> = (0..use_group.named_child_count())
                .filter_map(|i| use_group.named_child(i))
                .filter(|c| {
                    matches!(c.kind(), "namespace_use_group_clause" | "namespace_use_clause")
                })
                .collect();
            for clause in clauses {
                let ns_name = (0..clause.named_child_count())
                    .filter_map(|i| clause.named_child(i))
                    .find(|c| c.kind() == "namespace_name");
                let name = match ns_name {
                    Some(nn) => (0..nn.named_child_count())
                        .filter_map(|i| nn.named_child(i))
                        .find(|c| c.kind() == "name"),
                    None => (0..clause.named_child_count())
                        .filter_map(|i| clause.named_child(i))
                        .find(|c| c.kind() == "name"),
                };
                if let Some(name) = name {
                    let full = format!("{prefix}\\{}", self.text(name));
                    self.create_node(
                        "import",
                        &full,
                        node,
                        Extra { signature: Some(import_text.clone()), ..Extra::default() },
                    );
                    let parent = self.top_row();
                    self.push_php_use_ref(&full, parent, node);
                    let local = self.use_local_name(clause, &full);
                    self.emit_import_binding(&local, &full, node);
                }
            }
            return;
        }

        // Single use (incl. `use function`/`use const`/aliased): the hook's
        // qualified_name-else-name read; alias never included.
        let use_clause = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "namespace_use_clause");
        let Some(use_clause) = use_clause else { return };
        let target = (0..use_clause.named_child_count())
            .filter_map(|i| use_clause.named_child(i))
            .find(|c| c.kind() == "qualified_name")
            .or_else(|| {
                (0..use_clause.named_child_count())
                    .filter_map(|i| use_clause.named_child(i))
                    .find(|c| c.kind() == "name")
            });
        let Some(target) = target else { return }; // hook null → nothing
        let module_name = self.text(target).to_string();
        if module_name.is_empty() {
            return;
        }
        self.create_node(
            "import",
            &module_name,
            node,
            Extra { signature: Some(import_text), ..Extra::default() },
        );
        let parent = self.top_row();
        self.push_ref_at(parent, &module_name, imports_kind, node);
        // emitPhpUseRefs → the `Foo\Bar::Baz` ref (bare single-segment `use
        // Countable;` has no `\` → no `::` ref).
        self.push_php_use_ref(&module_name, parent, node);
        let local = self.use_local_name(use_clause, &module_name);
        self.emit_import_binding(&local, &module_name, node);
    }

    /// The local name a `use` clause binds: its `as` alias, else the last
    /// segment of the imported name. The grammar puts the alias as a bare
    /// `name` child after the target (`qualified_name` or, in a group, the
    /// first `name`).
    pub(super) fn use_local_name(&self, clause: Node<'t>, full: &str) -> String {
        let names: Vec<Node<'t>> = (0..clause.named_child_count()).filter_map(|i| clause.named_child(i)).filter(|c| c.kind() == "name").collect();
        let has_qualified = (0..clause.named_child_count()).filter_map(|i| clause.named_child(i)).any(|c| c.kind() == "qualified_name");
        let alias = if has_qualified { names.first() } else { names.get(1) };
        match alias {
            Some(a) => self.text(*a).to_string(),
            None => full.rsplit('\\').next().unwrap_or(full).to_string(),
        }
    }
}
