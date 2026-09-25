//! Function-reference candidates and value references: capture during the walk, flush at its end.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    pub(super) fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        if node.kind() != "arguments" {
            return;
        }
        let from = self.top_row();
        for c in named_kids(node) {
            self.normalize_fn_ref_value(c, from, 0);
        }
    }

    pub(super) fn normalize_fn_ref_value(&mut self, v: Node<'t>, from: u32, depth: u32) {
        stack_guard!();
        if depth > 4 {
            return;
        }
        match v.kind() {
            "argument" => {
                for c in named_kids(v) {
                    self.normalize_fn_ref_value(c, from, depth + 1);
                }
            }
            // String callable — trustworthy ONLY as an argument to a known
            // callable-taking core function; skipGate (resolution's
            // unique-or-drop rule takes over). Namespaced strings drop.
            "string" | "encapsed_string" => {
                let Some(callee) = php_enclosing_call_name(v).map(|f| self.text(f)) else {
                    return;
                };
                if !is_php_callable_hof(callee) {
                    return;
                }
                let Some(content) = self.php_string_content(v) else { return };
                if crate::textutil::ascii_ident_re().is_match(&content) || qualified_callable_re().is_match(&content)
                {
                    if let Some(mut c) = Cand::at(from, content, v) {
                        c.ungated = true;
                        self.fn_ref_cands.push(c);
                    }
                }
            }
            // Array callables in ANY call's arguments: `[$this, 'm']` →
            // this.m; `[Foo::class, 'm']` → Foo::m; `['Cls', 'm']` → nothing.
            "array_creation_expression" => {
                if v.named_child_count() != 2 {
                    return;
                }
                let recv = v.named_child(0).and_then(|e| e.named_child(0));
                let str_el = v.named_child(1).and_then(|e| e.named_child(0));
                let (Some(recv), Some(str_el)) = (recv, str_el) else { return };
                if !matches!(str_el.kind(), "encapsed_string" | "string") {
                    return;
                }
                let Some(member) = self.php_string_content(str_el) else { return };
                if !crate::textutil::ascii_ident_re().is_match(&member) {
                    return;
                }
                if recv.kind() == "variable_name" && self.text(recv) == "$this" {
                    let name = format!("this.{member}");
                    self.fn_ref_cands.extend(Cand::at(from, &name, str_el));
                } else if recv.kind() == "class_constant_access_expression" {
                    let cls = recv.named_child(0);
                    let kw = recv.named_child(1);
                    if let (Some(cls), Some(kw)) = (cls, kw) {
                        if self.text(kw) == "class" {
                            let name = format!("{}::{member}", self.text(cls));
                            self.fn_ref_cands.extend(Cand::at(from, &name, str_el));
                        }
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
        // Halts at functionTypes (function_definition) + arrow_function (in
        // the fixed list); anonymous_function is NOT halted — scans descend
        // into closures.
        if depth > 0
            && matches!(
                node.kind(),
                "function_definition" | "arrow_function" | "function_expression" | "lambda_literal"
                    | "lambda_expression"
            )
        {
            return;
        }
        self.maybe_capture_fn_refs(node);
        for c in named_kids(node) {
            self.scan_fn_ref_subtree(c, depth + 1);
        }
    }

    pub(super) fn flush_value_refs(&mut self) {
        let scopes = std::mem::take(&mut self.value_scopes);
        let targets = std::mem::take(&mut self.fs_values);
        if std::env::var("CODEGRAPH_VALUE_REFS").as_deref() == Ok("0") {
            return;
        }
        if targets.is_empty() || scopes.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }

        // Shadow prune: the per-grammar declarator switch has NO resolving php
        // cases (`assignment` is python's node; property_declaration's
        // Kotlin/Swift path yields null) → declCounts stays empty → no php
        // target is ever pruned. Skipping the scan is byte-identical.

        crate::walker::emit_value_refs(self.src, &self.node_ids, &mut self.arena, &mut self.tables, &scopes, &targets);
    }

    /// phpStringContent: the string's first string_content child, trimmed.
    pub(super) fn php_string_content(&self, node: Node) -> Option<String> {
        for i in 0..node.named_child_count() {
            let Some(c) = node.named_child(i) else { continue };
            if c.kind() == "string_content" {
                return Some(self.text(c).trim().to_string());
            }
        }
        None
    }
}
