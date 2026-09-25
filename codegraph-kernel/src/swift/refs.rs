//! Function-reference candidates and value references: capture during the walk, flush at its end.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        enum Mode {
            Args,
            Rhs,
            List,
            Varinit,
        }
        let mode = match node.kind() {
            "value_arguments" => Mode::Args,
            "assignment" => Mode::Rhs, // field 'result'
            "array_literal" => Mode::List,
            "property_declaration" => Mode::Varinit, // field 'value'
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
                if let Some(rhs) = node.child_by_field_name("result") {
                    // Param-storage skip — swift's LHS field is `target`.
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
                // Destructuring gate: swift's name field is a `pattern` node —
                // never in the pattern-kind set → never skipped.
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
                if !is_destructuring {
                    if let Some(v) = node.child_by_field_name("value") {
                        values.push(v);
                    }
                }
            }
        }

        for v in values {
            self.normalize_fn_ref_value(v, from, 0);
        }
    }

    pub(super) fn normalize_fn_ref_value(&mut self, v: Node<'t>, from: u32, depth: u32) {
        stack_guard!();
        if depth > 4 {
            return;
        }
        match v.kind() {
            "simple_identifier" => {
                let name = self.text(v);
                self.push_fn_ref_cand(from, name, v);
            }
            "value_argument" => {
                // Layer with field 'value' + the label-forward skip (the
                // Alamofire A/B finding): label text == value text → dropped.
                let label = v.child_by_field_name("name");
                let value = v.child_by_field_name("value").or_else(|| {
                    if v.named_child_count() > 0 {
                        v.named_child(v.named_child_count() - 1)
                    } else {
                        None
                    }
                });
                if let (Some(l), Some(val)) = (label, value) {
                    if self.text(l).trim() == self.text(val).trim() {
                        return;
                    }
                }
                if let Some(inner) = v.child_by_field_name("value") {
                    self.normalize_fn_ref_value(inner, from, depth + 1);
                }
            }
            "selector_expression" => {
                // `#selector(fire)` → fire; dotted → rightmost
                // simple_identifier (incl. the `_` quirk); else trimmed text.
                let Some(inner) = v.named_child(0) else { return };
                if matches!(inner.kind(), "identifier" | "simple_identifier") {
                    let name = self.text(inner);
                    self.push_fn_ref_cand(from, name, inner);
                    return;
                }
                if let Some(last) = last_simple_identifier(v) {
                    let name = self.text(last);
                    self.push_fn_ref_cand(from, name, last);
                    return;
                }
                let name = self.text(inner).trim().to_string();
                self.push_fn_ref_cand(from, &name, inner);
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
        // Halts at functionTypes (function_declaration) + the fixed list —
        // lambda_literal IS in it (closures halt the scan).
        if depth > 0
            && matches!(
                node.kind(),
                "function_declaration" | "arrow_function" | "function_expression" | "lambda_literal"
                    | "lambda_expression"
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

        // Shadow prune — TWO cases resolve for swift: property_declaration
        // (firstSimpleIdentifier over the name/binding pattern; guard-let/
        // if-let bindings have no property_declaration → never prune) AND the
        // shared `assignment` case — a declared-then-assigned `let X: T`
        // followed by `X = …` branches counts one bump per assignment (the
        // directly_assignable_expression's simple_identifier child), pruning
        // X exactly as the wasm arm does (caught by the swift-nio sweep).
        let mut decl_counts: HashMap<&str, u32> = HashMap::new();
        let mut dstack: Vec<Node> = vec![root];
        let mut dvisited = 0usize;
        while let Some(n) = dstack.pop() {
            if dvisited >= crate::walker::MAX_VALUE_REF_NODES {
                break;
            }
            dvisited += 1;
            if n.kind() == "assignment" {
                let left = n
                    .child_by_field_name("left")
                    .or_else(|| n.child_by_field_name("pattern"))
                    .or_else(|| n.named_child(0));
                if let Some(left) = left {
                    if left.kind() == "identifier" {
                        let nm = self.text(left);
                        if targets.contains_key(nm) {
                            *decl_counts.entry(nm).or_insert(0) += 1;
                        }
                    } else {
                        for i in 0..left.named_child_count() {
                            let Some(c) = left.named_child(i) else { continue };
                            if matches!(c.kind(), "identifier" | "simple_identifier") {
                                let nm = self.text(c);
                                if targets.contains_key(nm) {
                                    *decl_counts.entry(nm).or_insert(0) += 1;
                                }
                            }
                        }
                    }
                }
            }
            if n.kind() == "property_declaration" {
                let vd = (0..n.named_child_count())
                    .filter_map(|i| n.named_child(i))
                    .find(|c| c.kind() == "variable_declaration"); // kotlin shape — None for swift
                let id = match vd {
                    Some(vd) => (0..vd.named_child_count())
                        .filter_map(|i| vd.named_child(i))
                        .find(|c| c.kind() == "simple_identifier"),
                    None => first_simple_identifier(n.child_by_field_name("name").or_else(|| {
                        (0..n.named_child_count())
                            .filter_map(|i| n.named_child(i))
                            .find(|c| matches!(c.kind(), "value_binding_pattern" | "pattern"))
                    })),
                };
                if let Some(id) = id {
                    if matches!(id.kind(), "identifier" | "simple_identifier") {
                        let nm = self.text(id);
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
}
