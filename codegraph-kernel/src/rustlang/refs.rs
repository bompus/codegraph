//! Function-reference candidates and value references: capture during the walk, flush at its end.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// maybeCaptureFnRefs with RUST_SPEC's dispatch: arguments→args,
    /// assignment_expression→rhs(right), field_initializer→value(value),
    /// array_expression→list, static_item/let_declaration→varinit(value).
    /// No layers/unwrap/special — only bare identifiers qualify (`&handler`
    /// captures nothing). QUIRK: const_item is NOT in the dispatch.
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        enum Mode {
            Args,
            Rhs,
            Value,
            List,
            Varinit,
        }
        let mode = match node.kind() {
            "arguments" => Mode::Args,
            "assignment_expression" => Mode::Rhs,
            "field_initializer" => Mode::Value,
            "array_expression" => Mode::List,
            "static_item" | "let_declaration" => Mode::Varinit,
            _ => return,
        };
        let from = self.top_row();

        let mut values: Vec<Node> = Vec::new();
        match mode {
            Mode::Args | Mode::List => {
                for c in named_kids(node) {
                    values.push(c);
                }
            }
            Mode::Rhs => {
                if let Some(rhs) = node.child_by_field_name("right") {
                    // Param-storage skip: `o.cb = cb`.
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
            Mode::Value => {
                let v = node.child_by_field_name("value").or_else(|| {
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
            Mode::Varinit => {
                // Destructuring skip: a tuple/struct pattern LHS extracts data,
                // never a function alias (static_item's name is an identifier,
                // let_declaration's `pattern` field can be a pattern).
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
                if let Some(v) = node.child_by_field_name("value") {
                    values.push(v);
                }
            }
        }

        for v in values {
            // normalizeValue: idTypes = {identifier} only, no layers/unwrap.
            if v.kind() == "identifier" {
                let name = self.text(v).to_string();
                self.fn_ref_cands.extend(Cand::at(from, name, v));
            }
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
                "function_item" | "function_signature_item" | "arrow_function"
                    | "function_expression" | "lambda_literal" | "lambda_expression"
            )
        {
            return;
        }
        self.maybe_capture_fn_refs(node);
        for c in named_kids(node) {
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

        // Shadow prune — rust declarator shapes: const_item/static_item (name
        // field) and let_declaration (the shadow source: `pattern` field; a
        // tuple pattern bumps every named child).
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
                "const_item" | "static_item" => {
                    bump(&mut decl_counts, n.child_by_field_name("name"), self.src, &targets)
                }
                "let_declaration" => {
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

        crate::walker::emit_value_refs(self.src, &self.node_ids, &mut self.arena, &mut self.tables, &scopes, &targets);
    }
}
