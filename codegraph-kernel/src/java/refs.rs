//! Function-reference candidates and value references: capture during the walk, flush at its end.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        let mode_field: Option<&str> = match node.kind() {
            "argument_list" => Some(""),          // args: every named child
            "assignment_expression" => Some("right"),
            "variable_declarator" => Some("value"),
            _ => None,
        };
        let Some(field) = mode_field else { return };
        let from = self.top_row();

        let mut values: Vec<Node> = Vec::new();
        if field.is_empty() {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    values.push(c);
                }
            }
        } else if field == "right" {
            if let Some(rhs) = node.child_by_field_name("right") {
                let lhs_text = node
                    .child_by_field_name("left")
                    .map(|l| self.text(l))
                    .unwrap_or("");
                let lhs_last = util::lhs_last_name()
                    .captures(lhs_text)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str());
                if !(lhs_last.is_some() && lhs_last == Some(self.text(rhs).trim())) {
                    values.push(rhs);
                }
            }
        } else if let Some(v) = node.child_by_field_name("value") {
            // varinit — destructuring patterns don't exist in Java.
            values.push(v);
        }

        for v in values {
            if v.kind() != "method_reference" {
                continue; // idTypes is EMPTY for Java — only method references
            }
            let mut last_ident: Option<Node> = None;
            for i in 0..v.named_child_count() {
                if let Some(c) = v.named_child(i) {
                    if c.kind() == "identifier" {
                        last_ident = Some(c);
                    }
                }
            }
            let Some(last) = last_ident else { continue };
            let m = self.text(last);
            let text = self.text(v);
            let name = if text.starts_with("this::") || text.starts_with("super::") {
                format!("this.{m}")
            } else if let Some(c) = method_ref_type_re().captures(text) {
                if m == "new" {
                    continue;
                }
                format!("{}::{m}", &c[1])
            } else {
                continue;
            };
            let p = last.start_position();
            self.fn_ref_cands.push(Cand {
                from,
                name,
                line: p.row as u32 + 1,
                column_byte: last.start_byte(),
                row: p.row,
            });
        }
    }

    pub(super) fn scan_fn_ref_subtree(&mut self, node: Node<'t>, depth: u32) {
        stack_guard!();
        if depth > 12 {
            return;
        }
        // (functionTypes is empty for Java; lambda_expression halts the scan)
        if depth > 0 && matches!(node.kind(), "lambda_literal" | "lambda_expression") {
            return;
        }
        self.maybe_capture_fn_refs(node);
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.scan_fn_ref_subtree(c, depth + 1);
            }
        }
    }

    pub(super) fn flush_value_refs(&mut self, root: Node<'t>) {
        let scopes = std::mem::take(&mut self.value_scopes);
        let mut targets = std::mem::take(&mut self.fs_values);
        let counts = std::mem::take(&mut self.fs_value_counts);
        if std::env::var("CODEGRAPH_VALUE_REFS").as_deref() == Ok("0") {
            return;
        }
        if targets.is_empty() || scopes.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }

        let mut decl_counts: HashMap<&str, u32> = HashMap::new();
        let mut dstack: Vec<Node> = vec![root];
        let mut dvisited = 0usize;
        while let Some(n) = dstack.pop() {
            if dvisited >= crate::walker::MAX_VALUE_REF_NODES {
                break;
            }
            dvisited += 1;
            if n.kind() == "variable_declarator" {
                if let Some(first) = n.named_child(0) {
                    if first.kind() == "identifier" {
                        let nm = self.text(first);
                        if targets.contains_key(nm) {
                            *decl_counts.entry(nm).or_insert(0) += 1;
                        }
                    }
                }
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i) {
                    dstack.push(c);
                }
            }
        }
        let shadowed: Vec<String> = decl_counts
            .iter()
            .filter(|(nm, c)| **c > counts.get(**nm).copied().unwrap_or(1))
            .map(|(nm, _)| nm.to_string())
            .collect();
        for nm in shadowed {
            targets.remove(&nm);
        }
        if targets.is_empty() {
            return;
        }

        crate::walker::emit_value_refs(self.src, &self.node_ids, &mut self.arena, &mut self.tables, &scopes, &targets);
    }

    pub(super) fn capture_value_ref_scope(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>) {
        let target_kind_ok = kind == "constant" || kind == "variable";
        if target_kind_ok
            && util::utf16_len(name) >= 3
            && util::has_upper_or_underscore().is_match(name)
        {
            let parent_ok = self
                .stack
                .last()
                .map(|s| matches!(s.kind, "file" | "class" | "module" | "struct" | "enum"))
                .unwrap_or(false);
            if parent_ok {
                self.fs_values.insert(name.to_string(), row);
                *self.fs_value_counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
        if matches!(kind, "function" | "method" | "constant" | "variable") {
            self.value_scopes.push(ValueScope { row, node, name: name.to_string() });
        }
    }
}
