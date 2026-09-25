//! Function-reference candidates and value references: capture during the walk, flush at its end.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        #[derive(PartialEq)]
        enum Mode {
            Args,
            Rhs,
            List,
            Varinit,
        }
        let mode = match node.kind() {
            "argument_list" => Mode::Args,
            "assignment_expression" => Mode::Rhs, // covers `+=` event subscription
            "initializer_expression" => Mode::List,
            "variable_declarator" => Mode::Varinit,
            _ => return,
        };
        if self.stack.is_empty() {
            return;
        }
        let from = self.top_row();

        let mut values: Vec<Node> = Vec::new();
        match mode {
            Mode::Args | Mode::List => {
                for i in 0..node.named_child_count() {
                    if let Some(c) = node.named_child(i) {
                        values.push(c);
                    }
                }
            }
            Mode::Rhs => {
                if let Some(rhs) = node.child_by_field_name("right") {
                    // Param-storage skip: `this.status = status`.
                    let lhs = node
                        .child_by_field_name("left")
                        .or_else(|| node.child_by_field_name("lhs"))
                        .or_else(|| node.child_by_field_name("target"))
                        .or_else(|| {
                            if node.named_child_count() >= 2 { node.named_child(0) } else { None }
                        });
                    let lhs_text = lhs.map(|l| self.text(l)).unwrap_or("");
                    let lhs_last = util::lhs_last_name()
                        .captures(lhs_text)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str());
                    let rhs_text = self.text(rhs).trim();
                    if !(lhs_last.is_some() && lhs_last == Some(rhs_text)) {
                        values.push(rhs);
                    }
                }
            }
            Mode::Varinit => {
                // No `value` field on C# variable_declarator: the initializer
                // is the LAST named child — but an initializer-less declarator
                // has its NAME there. Require ≥2 named children and never pick
                // the name child. (Destructuring pattern gate never matches C#.)
                let name_child = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("pattern"));
                let is_destructuring = name_child
                    .map(|nc| {
                        matches!(
                            nc.kind(),
                            "object_pattern" | "array_pattern" | "tuple_pattern" | "struct_pattern"
                        )
                    })
                    .unwrap_or(false);
                if !is_destructuring && node.named_child_count() >= 2 {
                    if let Some(value) = node.named_child(node.named_child_count() - 1) {
                        let is_name = name_child.map(|nc| nc.id() == value.id()).unwrap_or(false);
                        if !is_name {
                            values.push(value);
                        }
                    }
                }
            }
        }

        for v in values {
            self.normalize_fn_ref_value(v, from, 0);
        }
    }

    /// normalizeValue (function-ref.ts:525) for CSHARP_SPEC: bare identifiers,
    /// the transparent `argument` layer, and the `this.Member` special.
    pub(super) fn normalize_fn_ref_value(&mut self, v: Node<'t>, from: u32, depth: u32) {
        stack_guard!();
        if depth > 4 {
            return;
        }
        match v.kind() {
            "identifier" => {
                let name = self.text(v);
                self.push_fn_ref_cand(from, name, v);
            }
            "argument" => {
                // Transparent layer (field=null) → recurse all named children.
                for i in 0..v.named_child_count() {
                    if let Some(c) = v.named_child(i) {
                        self.normalize_fn_ref_value(c, from, depth + 1);
                    }
                }
            }
            "member_access_expression" => {
                // `this.Run0` — receiver must be EXACTLY `this` (the vendored
                // grammar yields the anonymous `this` token via the field;
                // text-prefix fallback for field-less shapes). Candidate is
                // the BARE member name at the name node's position.
                let Some(name_node) = v.child_by_field_name("name") else { return };
                let is_this = match v.child_by_field_name("expression") {
                    Some(e) => e.kind() == "this_expression" || e.kind() == "this",
                    None => self.text(v).starts_with("this."),
                };
                if is_this {
                    let name = self.text(name_node);
                    self.push_fn_ref_cand(from, name, name_node);
                }
            }
            _ => {}
        }
    }

    pub(super) fn push_fn_ref_cand(&mut self, from: u32, name: &str, node: Node) {
        if name.is_empty() || is_stoplisted(name) {
            return;
        }
        let p = node.start_position();
        self.fn_ref_cands.push(Cand {
            from,
            name: name.to_string(),
            line: p.row as u32 + 1,
            column_byte: node.start_byte(),
            row: p.row,
        });
    }

    pub(super) fn scan_fn_ref_subtree(&mut self, node: Node<'t>, depth: u32) {
        stack_guard!();
        if depth > 12 {
            return;
        }
        // functionTypes is EMPTY for C#; the literal halt list applies —
        // lambda_expression IS C#'s lambda, so initializer lambdas stop the
        // scan; anonymous_method_expression is NOT listed and scans through.
        if depth > 0
            && matches!(
                node.kind(),
                "arrow_function" | "function_expression" | "lambda_literal" | "lambda_expression"
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

        // Shadow prune: count every variable_declarator declaring a target
        // name (field declarators AND method-body/top-level locals); more
        // declarations than file-scope captures → shadowed → dropped.
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
            // (property_declaration's bump is structurally null for C# — not ported.)
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
