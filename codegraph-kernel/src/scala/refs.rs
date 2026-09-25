//! Function-reference candidates and value references: capture during the walk, flush at its end.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        let (mode, field): (&str, &str) = match node.kind() {
            "arguments" => ("args", ""),
            "assignment_expression" => ("rhs", "right"),
            "val_definition" => ("varinit", "value"),
            _ => return,
        };
        let from = self.top_row();

        let mut values: Vec<Node<'t>> = Vec::new();
        match mode {
            "args" => {
                let mut cursor = node.walk();
                for c in node.named_children(&mut cursor) {
                    values.push(c);
                }
            }
            "rhs" => {
                if let Some(rhs) = node.child_by_field_name(field) {
                    // Param-storage skip: lhs tail == rhs text.
                    let lhs = node
                        .child_by_field_name("left")
                        .or_else(|| node.child_by_field_name("lhs"))
                        .or_else(|| node.child_by_field_name("target"))
                        .or_else(|| {
                            if node.named_child_count() >= 2 {
                                node.named_child(0)
                            } else {
                                None
                            }
                        });
                    let lhs_text = lhs.map(|l| self.text(l)).unwrap_or("");
                    let lhs_last = util::lhs_last_name()
                        .captures(lhs_text)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str());
                    if !(lhs_last.is_some() && lhs_last == Some(self.text(rhs).trim())) {
                        values.push(rhs);
                    }
                }
            }
            _ => {
                // varinit — destructuring patterns capture nothing.
                let name_node = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("pattern"));
                if let Some(nn) = name_node {
                    if matches!(
                        nn.kind(),
                        "object_pattern" | "array_pattern" | "tuple_pattern" | "struct_pattern"
                    ) {
                        return;
                    }
                }
                if let Some(v) = node.child_by_field_name(field) {
                    values.push(v);
                }
            }
        }

        for v in values {
            self.normalize_fn_ref_value(v, from, 0);
        }
    }

    /// normalizeValue with SCALA_SPEC's unwrap (postfix_expression → first
    /// named child — eta-expansion `handler _`). No layers.
    pub(super) fn normalize_fn_ref_value(&mut self, v: Node<'t>, from: u32, depth: u32) {
        stack_guard!();
        if depth > 4 {
            return;
        }
        match v.kind() {
            "identifier" => {
                let name = self.text(v).to_string();
                self.fn_ref_cands.extend(Cand::at(from, name, v));
            }
            "postfix_expression" => {
                if let Some(inner) = v.named_child(0) {
                    self.normalize_fn_ref_value(inner, from, depth + 1);
                }
            }
            _ => {}
        }
    }

    pub(super) fn scan_fn_ref_subtree(&mut self, node: Node<'t>, depth: u32) {
        stack_guard!();
        if depth > 12 {
            return;
        }
        // Halt list: functionTypes is EMPTY for scala, so only the literal
        // lambda kinds halt — the scan descends into nested
        // function_definitions inside hook-consumed vals.
        if depth > 0
            && matches!(
                node.kind(),
                "arrow_function" | "function_expression" | "lambda_literal" | "lambda_expression"
            )
        {
            return;
        }
        self.maybe_capture_fn_refs(node);
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            self.scan_fn_ref_subtree(c, depth + 1);
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

        // Shadow prune — the scala declarator shape: val_definition /
        // var_definition with an `identifier` pattern (tuple/case-class
        // patterns bump nothing).
        let mut decl_counts: HashMap<&str, u32> = HashMap::new();
        let mut dstack: Vec<Node> = vec![root];
        let mut dvisited = 0usize;
        while let Some(n) = dstack.pop() {
            if dvisited >= crate::walker::MAX_VALUE_REF_NODES {
                break;
            }
            dvisited += 1;
            if matches!(n.kind(), "val_definition" | "var_definition") {
                if let Some(pat) = n.child_by_field_name("pattern") {
                    if pat.kind() == "identifier" {
                        let nm = self.text(pat);
                        if targets.contains_key(nm) {
                            *decl_counts.entry(nm).or_insert(0) += 1;
                        }
                    }
                }
            }
            for c in named_kids(n) {
                dstack.push(c);
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

        let refs_kind = crate::buffers::EDGE_REFERENCES;
        // One arena string for every value-ref edge of the file (unchanged when none).
        let mut value_ref_meta: Option<StrRef> = None;
        for scope in &scopes {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut stack: Vec<Node> = vec![scope.node];
            // The Dart/Pascal sibling-body pull (:891) — a next sibling of
            // kind function_body/block joins the scan. Effectively inert for
            // scala (bodies nest) but ported for fidelity.
            if let Some(sib) = scope.node.next_named_sibling() {
                if matches!(sib.kind(), "function_body" | "block") {
                    stack.push(sib);
                }
            }
            let mut visited = 0usize;
            while let Some(n) = stack.pop() {
                if visited >= crate::walker::MAX_VALUE_REF_NODES {
                    break;
                }
                visited += 1;
                if matches!(n.kind(), "identifier" | "constant" | "name" | "simple_identifier") {
                    let ref_name = self.text(n);
                    if let Some(&target_row) = targets.get(ref_name) {
                        let target_id = self.node_ids[target_row as usize].as_str();
                        if target_id != self.node_ids[scope.row as usize]
                            && ref_name != scope.name
                            && !seen.contains(&target_id)
                        {
                            seen.insert(target_id);
                            let meta = *value_ref_meta.get_or_insert_with(|| self.arena.put(r#"{"valueRef":true}"#));
                            self.tables.push_edge(&EdgeRow {
                                source_idx: scope.row,
                                target_idx: target_row,
                                kind: refs_kind,
                                provenance: 0,
                                line: NONE,
                                column: NONE,
                                metadata_json: meta,
                                source_id_str: NONE_STR,
                                target_id_str: NONE_STR,
                            });
                        }
                    }
                }
                for c in named_kids(n) {
                    stack.push(c);
                }
            }
        }
    }
}
