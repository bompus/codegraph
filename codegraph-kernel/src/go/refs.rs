//! Function-reference candidates and value references: capture during the walk, flush at its end.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        let (mode, field): (&str, &str) = match node.kind() {
            "argument_list" => ("args", ""),
            "assignment_statement" => ("rhs", "right"),
            "short_var_declaration" => ("rhs", "right"),
            "var_spec" => ("varinit", "value"),
            "keyed_element" => ("value", ""), // value = LAST named child
            "literal_value" => ("list", ""),
            _ => return,
        };
        let from = self.top_row();

        let mut values: Vec<Node> = Vec::new();
        match mode {
            "args" | "list" => {
                for i in 0..node.named_child_count() {
                    if let Some(c) = node.named_child(i) {
                        values.push(c);
                    }
                }
            }
            "rhs" => {
                if let Some(rhs) = node.child_by_field_name(field) {
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
            }
            "value" => {
                let v = node
                    .child_by_field_name("value")
                    .or_else(|| {
                        if node.named_child_count() > 0 {
                            node.named_child(node.named_child_count() - 1)
                        } else {
                            None
                        }
                    });
                if let Some(v) = v {
                    values.push(v);
                }
            }
            _ => {
                // varinit — Go var_spec names are plain identifiers (no
                // destructuring patterns to skip).
                if let Some(v) = node.child_by_field_name(field) {
                    values.push(v);
                }
            }
        }

        for v in values {
            self.normalize_fn_ref_value(v, from, 0);
        }
    }

    /// normalizeValue with GO_SPEC's transparent layers (literal_element,
    /// expression_list — both fan out to named children).
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
            "literal_element" | "expression_list" => {
                for i in 0..v.named_child_count() {
                    if let Some(c) = v.named_child(i) {
                        self.normalize_fn_ref_value(c, from, depth + 1);
                    }
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
        if depth > 0
            && matches!(
                node.kind(),
                "function_declaration" | "arrow_function" | "function_expression"
                    | "lambda_literal" | "lambda_expression"
            )
        {
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

        // Shadow prune — Go declarator shapes: const_spec/var_spec (name =
        // first child) and short_var_declaration (left / expression_list).
        let mut decl_counts: HashMap<&str, u32> = HashMap::new();
        let bump = |decl_counts: &mut HashMap<&'t str, u32>, name_node: Option<Node<'t>>, src: &'t str, targets: &HashMap<String, u32>| {
            if let Some(n) = name_node {
                if matches!(n.kind(), "identifier" | "simple_identifier") {
                    let nm = &src[n.byte_range()];
                    if targets.contains_key(nm) {
                        *decl_counts.entry(nm).or_insert(0) += 1;
                    }
                }
            }
        };
        let mut dstack: Vec<Node> = vec![root];
        let mut dvisited = 0usize;
        while let Some(n) = dstack.pop() {
            if dvisited >= crate::walker::MAX_VALUE_REF_NODES {
                break;
            }
            dvisited += 1;
            match n.kind() {
                "const_spec" | "var_spec" => bump(&mut decl_counts, n.named_child(0), self.src, &targets),
                "short_var_declaration" => {
                    let left = n
                        .child_by_field_name("left")
                        .or_else(|| n.child_by_field_name("pattern"))
                        .or_else(|| n.named_child(0));
                    if let Some(left) = left {
                        if left.kind() == "identifier" {
                            bump(&mut decl_counts, Some(left), self.src, &targets);
                        } else {
                            for i in 0..left.named_child_count() {
                                bump(&mut decl_counts, left.named_child(i), self.src, &targets);
                            }
                        }
                    }
                }
                _ => {}
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
}
