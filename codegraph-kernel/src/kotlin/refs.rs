//! Function-reference candidates and value references: capture during the walk, flush at its end.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        enum Mode {
            Args,
            Rhs,
        }
        let mode = match node.kind() {
            "value_arguments" => Mode::Args,
            "assignment" => Mode::Rhs, // NO field — RHS = LAST named child
            _ => return,
        };
        let from = self.top_row();

        let mut values: Vec<Node> = Vec::new();
        match mode {
            Mode::Args => {
                for i in 0..node.named_child_count() {
                    if let Some(c) = node.named_child(i) {
                        values.push(c);
                    }
                }
            }
            Mode::Rhs => {
                let rhs = if node.named_child_count() > 0 {
                    node.named_child(node.named_child_count() - 1)
                } else {
                    None
                };
                if let Some(rhs) = rhs {
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
            // value_argument layer with NO field resolution (zero fields) —
            // the label-forward skip is DEAD for kotlin; fan out namedChildren.
            "value_argument" => {
                for i in 0..v.named_child_count() {
                    if let Some(c) = v.named_child(i) {
                        self.normalize_fn_ref_value(c, from, depth + 1);
                    }
                }
            }
            // `::topLevel` / `OtherClass::handle` — receiver = LAST
            // type_identifier child, member = LAST simple_identifier child;
            // `String::class` has no member (anon keyword) → nothing;
            // lowercase receivers dropped by the CASE regex, not node type.
            "callable_reference" => {
                let mut receiver: Option<Node> = None;
                let mut member: Option<Node> = None;
                for i in 0..v.named_child_count() {
                    let Some(child) = v.named_child(i) else { continue };
                    if child.kind() == "type_identifier" {
                        receiver = Some(child);
                    }
                    if child.kind() == "simple_identifier" {
                        member = Some(child);
                    }
                }
                let Some(member) = member else { return };
                let m = self.text(member);
                match receiver {
                    None => self.push_fn_ref_cand(from, m, member),
                    Some(recv) => {
                        let recv_text = self.text(recv);
                        if recv_text.as_bytes().first().map(|b| b.is_ascii_uppercase()).unwrap_or(false) {
                            let name = format!("{recv_text}::{m}");
                            self.push_fn_ref_cand(from, &name, member);
                        }
                    }
                }
            }
            // `this::caller` → this.<member> (class-scoped, always flushes).
            "navigation_expression" => {
                if !self.text(v).starts_with("this::") {
                    return;
                }
                for i in 0..v.named_child_count() {
                    let Some(child) = v.named_child(i) else { continue };
                    if child.kind() == "navigation_suffix" && self.text(child).starts_with("::") {
                        if child.named_child_count() > 0 {
                            if let Some(id) = child.named_child(child.named_child_count() - 1) {
                                let name = format!("this.{}", self.text(id));
                                self.push_fn_ref_cand(from, &name, id);
                            }
                        }
                        return;
                    }
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
        // Halts at functionTypes (function_declaration) + the fixed list —
        // lambda_literal halts (no captures inside `by lazy { }` under a
        // hook-consumed property).
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

        // Shadow prune — kotlin cases: property_declaration (its
        // variable_declaration's first simple_identifier; destructuring bumps
        // nothing) AND the shared `assignment` case (the swift-sweep lesson —
        // directly_assignable_expression children bump).
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
                    .find(|c| c.kind() == "variable_declaration");
                if let Some(vd) = vd {
                    let id = (0..vd.named_child_count())
                        .filter_map(|i| vd.named_child(i))
                        .find(|c| c.kind() == "simple_identifier");
                    if let Some(id) = id {
                        let nm = self.text(id);
                        if targets.contains_key(nm) {
                            *decl_counts.entry(nm).or_insert(0) += 1;
                        }
                    }
                }
                // (the Swift name-field half of the shared case is a null
                // path for kotlin — variable_declaration always present)
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
