//! Function-as-value capture (#756) — the TS/JS slice of
//! src/extraction/function-ref.ts (TS_JS_SPEC): container dispatch, value
//! normalization, and the `this.member` special form. The flush-time gate
//! lives in the walker (it needs the file's nodes and import refs).

use super::*;
use crate::walker::Cand;
use tree_sitter::Node;

/// CaptureMode (function-ref.ts) — gate policy keys on it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Args,
    Rhs,
    Value,
    List,
    VarInit,
}


/// TS_JS_SPEC.dispatch: container node type → capture mode.
pub fn dispatch(kind: &str) -> Option<Mode> {
    match kind {
        "arguments" => Some(Mode::Args),
        "assignment_expression" => Some(Mode::Rhs),
        "variable_declarator" => Some(Mode::VarInit),
        "pair" => Some(Mode::Value),
        "array" => Some(Mode::List),
        // A JSX attribute value or child (`onPress={handleSubmit}`): the
        // expression's one named child is the value. Mirrors TS_JS_SPEC.
        "jsx_expression" => Some(Mode::List),
        // An object literal's shorthand members (`return { handleApprove }`).
        "object" => Some(Mode::List),
        _ => None,
    }
}

/// captureFnRefCandidates for the TS/JS spec: the candidates `container`
/// holds, attributed to row `from`.
pub fn capture(container: Node, mode: Mode, src: &str, from: u32) -> Vec<Cand> {
    let mut value_nodes: Vec<Node> = Vec::new();

    match mode {
        Mode::Args | Mode::List => {
            for i in 0..container.named_child_count() {
                if let Some(c) = container.named_child(i) {
                    value_nodes.push(c);
                }
            }
        }
        Mode::Rhs => {
            if let Some(rhs) = container.child_by_field_name("right") {
                // Param-storage skip: `this.status = status` — LHS's trailing
                // identifier equals the RHS text ⇒ a stored local/parameter.
                let lhs_text = container
                    .child_by_field_name("left")
                    .map(|l| &src[l.byte_range()])
                    .unwrap_or("");
                let lhs_last = super::util::lhs_last_name()
                    .captures(lhs_text)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str());
                let rhs_text = src[rhs.byte_range()].trim();
                if !(lhs_last.is_some() && lhs_last == Some(rhs_text)) {
                    value_nodes.push(rhs);
                }
            }
        }
        Mode::Value => {
            if let Some(v) = container.child_by_field_name("value") {
                value_nodes.push(v);
            }
        }
        Mode::VarInit => {
            // Destructuring extracts DATA, never a function alias.
            let name_node = container.child_by_field_name("name");
            let is_pattern = name_node
                .map(|n| matches!(n.kind(), "object_pattern" | "array_pattern"))
                .unwrap_or(false);
            if !is_pattern {
                if let Some(v) = container.child_by_field_name("value") {
                    value_nodes.push(v);
                }
            }
        }
    }

    let mut out = Vec::new();
    for v in value_nodes {
        for (name, node) in normalize(v, src) {
            out.extend(Cand::at(from, name, node));
        }
    }
    out
}

/// normalizeValue for the TS/JS spec: bare identifiers, plus the
/// `this.<member>` member_expression special form (object EXACTLY `this`).
fn normalize<'t>(node: Node<'t>, src: &str) -> Vec<(String, Node<'t>)> {
    match node.kind() {
        "identifier" | "shorthand_property_identifier" => vec![(src[node.byte_range()].to_string(), node)],
        "member_expression" => {
            let obj = node.child_by_field_name("object");
            let prop = node.child_by_field_name("property");
            if let (Some(o), Some(p)) = (obj, prop) {
                if o.kind() == "this" && p.kind() == "property_identifier" {
                    return vec![(format!("this.{}", &src[p.byte_range()]), p)];
                }
            }
            vec![]
        }
        _ => vec![],
    }
}

impl<'t> Walker<'t> {
    pub(super) fn capture_value_ref_scope(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>) {
        if !self.variant.value_refs() {
            return;
        }
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

    pub(super) fn flush_value_refs(&mut self, root: Node<'t>) {
        let scopes = std::mem::take(&mut self.value_scopes);
        let mut targets = std::mem::take(&mut self.fs_values);
        let counts = std::mem::take(&mut self.fs_value_counts);
        if !self.variant.value_refs() || std::env::var("CODEGRAPH_VALUE_REFS").as_deref() == Ok("0") {
            return;
        }
        if targets.is_empty() || scopes.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }

        // Shadow prune: count declarators of each target name across the whole
        // tree; more declarators than file-scope nodes ⇒ an inner re-binding
        // shadows the target. (TS/JS declarators are `variable_declarator`;
        // the other kinds in the TS switch belong to other grammars.)
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

    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        let Some(mode) = fnref::dispatch(node.kind()) else { return };
        let from = self.top_row();
        for cand in fnref::capture(node, mode, self.src, from) {
            self.fn_ref_cands.push(cand);
        }
    }

    /// scanFnRefSubtree: capture-only walk of subtrees the main walkers skip.
    pub(super) fn scan_fn_ref_subtree(&mut self, node: Node<'t>, depth: u32) {
        stack_guard!();
        if depth > 12 {
            return;
        }
        let kind = node.kind();
        if depth > 0
            && (is_function_type(kind) || matches!(kind, "lambda_literal" | "lambda_expression"))
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
}
