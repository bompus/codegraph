//! Function-reference candidates and value references: capture during the walk, flush at its end.

use super::*;

impl<'t> Walker<'t> {
    /// maybeCaptureFnRefs + captureFnRefCandidates for the cFamily dispatch:
    /// argument_list(args), assignment_expression(rhs:right),
    /// init_declarator(varinit:value), initializer_list(list),
    /// initializer_pair(value:value).
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        let mode = match node.kind() {
            "argument_list" => Mode::Args,
            "assignment_expression" => Mode::Rhs,
            "init_declarator" => Mode::Varinit,
            "initializer_list" => Mode::List,
            "initializer_pair" => Mode::Value,
            _ => return,
        };
        if self.stack.is_empty() {
            return;
        }
        let from = self.top_row();

        let mut values: Vec<Node<'t>> = Vec::new();
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
                    // Param-storage skip: `o->cb = cb` (LHS last name == RHS).
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
                // (init_declarator has no name/pattern field — no destructure skip)
                if let Some(v) = node.child_by_field_name("value") {
                    values.push(v);
                }
            }
        }

        for v in values {
            let explicit_ref = v.kind() != "identifier"; // !idTypes.has(type)
            self.normalize_fn_ref_value(v, from, mode, explicit_ref, 0);
        }
    }

    /// normalizeValue for cFamilySpec: bare identifiers, and the
    /// pointer_expression unwrap (`&fn`; `&Cls::m` keeps the qualified name).
    pub(super) fn normalize_fn_ref_value(&mut self, v: Node<'t>, from: u32, mode: Mode, explicit_ref: bool, depth: u32) {
        stack_guard!();
        if depth > 4 {
            return;
        }
        match v.kind() {
            "identifier" => {
                let name = self.text(v);
                if name.is_empty() || is_stoplisted(name) {
                    return;
                }
                self.push_fn_ref_cand(from, name.to_string(), mode, explicit_ref, v);
            }
            "pointer_expression" => {
                // `&x` is a function value; `*x` is a data read.
                if v.child(0).map(|c| c.kind() != "&").unwrap_or(true) {
                    return;
                }
                let Some(inner) = v.child_by_field_name("argument") else { return };
                if inner.kind() == "qualified_identifier" {
                    let text = self.text(inner).trim();
                    if qualified_ref_re().is_match(text) && !is_stoplisted(text) {
                        self.push_fn_ref_cand(from, text.to_string(), mode, explicit_ref, inner);
                    }
                    return;
                }
                self.normalize_fn_ref_value(inner, from, mode, explicit_ref, depth + 1);
            }
            _ => {}
        }
    }

    pub(super) fn push_fn_ref_cand(&mut self, from: u32, name: String, mode: Mode, explicit_ref: bool, node: Node) {
        let p = node.start_position();
        self.fn_ref_cands.push(Cand {
            from,
            name,
            mode,
            explicit_ref,
            line: p.row as u32 + 1,
            column_byte: node.start_byte(),
            row: p.row,
        });
    }

    /// scanFnRefSubtree: capture-only walk of subtrees the main walkers skip
    /// (variable-declaration initializers). Halts at nested functions/lambdas.
    pub(super) fn scan_fn_ref_subtree(&mut self, node: Node<'t>, depth: u32) {
        stack_guard!();
        if depth > 12 {
            return;
        }
        if depth > 0
            && matches!(
                node.kind(),
                "function_definition" | "arrow_function" | "function_expression"
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
        if self.variant != Variant::C {
            return;
        }
        if std::env::var("CODEGRAPH_VALUE_REFS").as_deref() == Ok("0") {
            return;
        }
        if targets.is_empty() || scopes.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }

        // Shadow prune — the C declarator shape is init_declarator (a
        // file-scope const AND the local that shadows it both count).
        let mut decl_counts: HashMap<&str, u32> = HashMap::new();
        let mut dstack: Vec<Node> = vec![root];
        let mut dvisited = 0usize;
        while let Some(n) = dstack.pop() {
            if dvisited >= crate::walker::MAX_VALUE_REF_NODES {
                break;
            }
            dvisited += 1;
            if n.kind() == "init_declarator" {
                if let Some(name_node) = c_declarator_identifier(n) {
                    if matches!(name_node.kind(), "identifier" | "simple_identifier") {
                        let nm = self.text(name_node);
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

    /// flushFnRefCandidates with the cFamily gate policy: value/list positions
    /// at FILE scope skip the same-file/import gate (C has no symbol imports);
    /// cpp additionally requires explicit `&` forms outside those positions.
    pub(super) fn flush_fn_ref_candidates(&mut self) {
        let cands = std::mem::take(&mut self.fn_ref_cands);
        if cands.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }
        let address_of_only = self.variant == Variant::Cpp;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for c in cands {
            let at_file_scope = self.node_ids[c.from as usize].starts_with("file:");
            if address_of_only
                && !c.explicit_ref
                && !(at_file_scope && matches!(c.mode, Mode::Value | Mode::List))
            {
                continue;
            }
            if !c.name.starts_with("this.") && !c.name.contains("::") {
                let skip_gate = matches!(c.mode, Mode::Value | Mode::List) && at_file_scope;
                if !skip_gate
                    && !self.defined_fn_names.contains(&c.name)
                    && !self.imported_names.contains(&c.name)
                {
                    continue;
                }
            }
            if !seen.insert((self.node_ids[c.from as usize].clone(), c.name.clone())) {
                continue;
            }
            let column = self.cols.col(self.src, c.row, c.column_byte);
            let name_ref = self.arena.put(&c.name);
            self.tables.push_ref(&RefRow {
                from_idx: c.from,
                kind: FUNCTION_REF_CODE,
                line: c.line,
                column,
                reference_name: name_ref,
                candidates: NONE_STR,
                from_id_str: NONE_STR,
            });
        }
    }
}
