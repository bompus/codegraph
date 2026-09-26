//! Zustand-style store actions (name-matcher.ts matchJsStoreBindingCall,
//! matchDestructuredStoreCall, matchSelectedStoreCall, matchStoreAccessorChain,
//! resolveStoreAction): a call bound to a store's action — destructured from
//! `useStore.getState()`, selected through `useStore((s) => s.reset)`, or
//! chained off an accessor (`get().reset()`) — resolves to the action inside
//! that store's object literal, never a same-named function elsewhere.

use super::awaited::{blank_string_contents, declaration_matches, has_parameter_binding, strip_ts_comments};
use super::*;

/// rangeWithin (name-matcher.ts): `inner`'s span lies inside `outer`'s.
fn range_within(inner: &KNode, outer: &KNode) -> bool {
    !(inner.start_line < outer.start_line
        || inner.end_line > outer.end_line
        || (inner.start_line == outer.start_line && inner.start_column < outer.start_column)
        || (inner.end_line == outer.end_line && inner.end_column > outer.end_column))
}

/// The brace stack (opening offsets) in force at `end`.
fn brace_stack(code: &str, end: usize) -> Vec<usize> {
    let mut stack = Vec::new();
    for (i, b) in code.as_bytes()[..end].iter().enumerate() {
        match b {
            b'{' => stack.push(i),
            b'}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
}

/// A binding at `at` is visible at the end of `code` when its brace stack is
/// a prefix of the call site's — sibling or closed blocks don't count.
fn in_scope(code: &str, at: usize, call_scope: &[usize]) -> bool {
    brace_stack(code, at).iter().enumerate().all(|(i, p)| call_scope.get(i) == Some(p))
}

/// `lines[from..to].concat(lines[to].slice(0, column)).join('\n')` — the
/// source up to the call site, starting at line index `from`.
fn source_before(lines: &[String], from: usize, line: i64, column: i64) -> String {
    let to = ((line - 1).max(0) as usize).min(lines.len());
    let mut parts: Vec<&str> = lines[from.min(to)..to].iter().map(String::as_str).collect();
    let prefix = lines.get(to).map_or("", |l| js_prefix(l, column.max(0) as usize));
    parts.push(prefix);
    parts.join("\n")
}

impl KernelResolver {
    /// matchJsStoreBindingCall — a bare JS call bound to a store action.
    pub(super) fn match_js_store_binding_call(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if !self.is_bare_js_call(r)? {
            return Ok(None);
        }
        if let Some(c) = self.match_destructured_store_call(r)? {
            return Ok(Some(c));
        }
        self.match_selected_store_call(r)
    }

    /// matchDestructuredStoreCall — `const { reset } = useStore.getState();`
    /// visible at the call, not shadowed after it.
    fn match_destructured_store_call(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        if !lines.text().contains(".getState") {
            return Ok(None);
        }
        let start = (self.enclosing_scope_start_line(&r.file_path, &r.language, r.line)? - 1).max(0) as usize;
        let code = blank_string_contents(&strip_ts_comments(&source_before(&lines, start, r.line, r.column)));
        let binding = re!(r"(?-u:\b)const\s*\{([^{}]*)\}\s*=\s*([A-Za-z0-9_$]+)\.getState\s*\(\s*\)");
        let call_scope = brace_stack(&code, code.len());
        let name = r.reference_name.as_str();
        let matches: Vec<(usize, usize, String, String)> = binding
            .captures_iter(&code)
            .map(|c| {
                let m = c.get(0).unwrap();
                (m.start(), m.end(), c[1].to_string(), c[2].to_string())
            })
            .collect();
        for (at, end, names, store) in matches.into_iter().rev() {
            // Plain named bindings only; defaults, rest and computed keys need
            // their own value tracing.
            if !names.split(',').any(|part| part.trim() == name) {
                continue;
            }
            if !in_scope(&code, at, &call_scope) {
                continue;
            }
            let rest = &code[end..];
            if !declaration_matches(rest, name, &["const", "let", "var", "function", "class"], true).is_empty() {
                return Ok(None);
            }
            return self.resolve_store_action(&format!("{store}.getState"), name, r, false);
        }
        Ok(None)
    }

    /// matchSelectedStoreCall — `const reset = useStore((s) => s.reset);`
    /// visible at the call; a later declaration or parameter shadows it.
    fn match_selected_store_call(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        if !lines.text().contains("=>") {
            return Ok(None);
        }
        let names = match self.selector_names_memo.get(&r.file_path) {
            Some(n) => n.clone(),
            None => {
                let n = Rc::new(js_selector_names(lines.text()));
                self.selector_names_memo.insert(r.file_path.clone(), n.clone());
                n
            }
        };
        let name = r.reference_name.as_str();
        if !names.contains(name) {
            return Ok(None);
        }
        let code = blank_string_contents(&strip_ts_comments(&source_before(&lines, 0, r.line, r.column)));
        // `\bconst\s+NAME\s*=\s*(hook)\s*\(\s*(?:\((param)\)|(param))\s*=>\s*(obj)\.(member)\s*\)`,
        // global: leftmost matches, each scan resuming after the last.
        let tail = re!(
            r"^\s*=\s*([A-Za-z0-9_$]+)\s*\(\s*(?:\(\s*([A-Za-z0-9_$]+)\s*\)|([A-Za-z0-9_$]+))\s*=>\s*([A-Za-z0-9_$]+)\.([A-Za-z0-9_$]+)\s*\)"
        );
        let bytes = code.as_bytes();
        let mut found: Vec<(usize, usize, [String; 5])> = Vec::new();
        let mut p = 0;
        while let Some(off) = code[p..].find("const") {
            let at = p + off;
            p = at + 1;
            if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
                continue;
            }
            let after_kw = at + "const".len();
            let after_ws = after_kw + code[after_kw..].len() - code[after_kw..].trim_start().len();
            if after_ws == after_kw || !code[after_ws..].starts_with(name) {
                continue;
            }
            let after_name = after_ws + name.len();
            let Some(c) = tail.captures(&code[after_name..]) else { continue };
            let g = |i: usize| c.get(i).map_or(String::new(), |m| m.as_str().to_string());
            let end = after_name + c.get(0).unwrap().end();
            found.push((at, end, [g(1), g(2), g(3), g(4), g(5)]));
            p = end;
        }
        let call_scope = brace_stack(&code, code.len());
        for (at, end, [hook, paren_param, bare_param, obj, member]) in found.into_iter().rev() {
            let param = if paren_param.is_empty() { bare_param } else { paren_param };
            if param != obj {
                continue;
            }
            if !in_scope(&code, at, &call_scope) {
                continue;
            }
            let rest = &code[end..];
            if !declaration_matches(rest, name, &["const", "let", "var", "function", "class"], true).is_empty()
                || has_parameter_binding(rest, name)
            {
                return Ok(None);
            }
            return self.resolve_store_action(&format!("{hook}.getState"), &member, r, true);
        }
        Ok(None)
    }

    /// matchStoreAccessorChain — `get().reset` / `useStore.getState().reset`.
    /// JS/TS resolves the member inside the identified store; Python keeps
    /// its unique-callable fallback (implementations over interface
    /// signatures). Any other chain says nothing about its inner call's result.
    pub(super) fn match_store_accessor_chain(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(m) = re!(r"^([A-Za-z0-9_$.]+)\(\)\.([A-Za-z0-9_]+)$").captures(&r.reference_name) else {
            return Ok(None);
        };
        let (inner, method) = (m[1].to_string(), m[2].to_string());
        if !(inner == "get" || inner == "getState" || inner.ends_with(".getState")) {
            return Ok(None);
        }
        if is_js_family(&r.language) {
            return self.resolve_store_action(&inner, &method, r, false);
        }
        let callables: Vec<Arc<KNode>> = self
            .nodes_by_name(&method)?
            .iter()
            .filter(|n| {
                (n.kind == "function" || n.kind == "method")
                    && same_language_family(&n.language, &r.language)
                    && n.id != r.from_node_id
            })
            .cloned()
            .collect();
        let mut implementations = Vec::new();
        for n in &callables {
            if !self.is_interface_member(n)? {
                implementations.push(n.clone());
            }
        }
        let eligible = if implementations.is_empty() { callables } else { implementations };
        if eligible.len() != 1 {
            return Ok(None);
        }
        Ok(Some(KCand { node: eligible[0].clone(), confidence: 0.6, resolved_by: "exact-match" }))
    }

    /// isInterfaceMember — a same-file interface node's span covers `node`.
    fn is_interface_member(&mut self, node: &KNode) -> Res<bool> {
        Ok(self.nodes_in_file(&node.file_path)?.iter().any(|n| {
            n.kind == "interface" && n.start_line <= node.start_line && n.end_line >= node.end_line && n.id != node.id
        }))
    }

    /// resolveStoreAction — the implementation inside the identified store.
    fn resolve_store_action(&mut self, inner: &str, member: &str, r: &ResolveRefIn, selector: bool) -> Res<Option<KCand>> {
        let holders: Vec<Arc<KNode>> = if inner == "get" || inner == "getState" {
            let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
            // The accessor must be a parameter of the enclosing factory:
            // `(set, get) =>` / `(set, getState, api) =>`.
            let factory = if inner == "get" {
                re!(r"\(\s*[A-Za-z0-9_$]+\s*,\s*get\s*(?:,\s*[A-Za-z0-9_$]+\s*)?\)\s*=>")
            } else {
                re!(r"\(\s*[A-Za-z0-9_$]+\s*,\s*getState\s*(?:,\s*[A-Za-z0-9_$]+\s*)?\)\s*=>")
            };
            let mut out = Vec::new();
            for n in self.nodes_in_file(&r.file_path)?.iter() {
                if (n.kind != "constant" && n.kind != "variable") || !range_within(&caller, n) {
                    continue;
                }
                let text = self.read_file(&n.file_path).map_or(String::new(), |ls| {
                    let from = ((n.start_line - 1).max(0) as usize).min(ls.len());
                    let to = (caller.start_line.max(0) as usize).clamp(from, ls.len());
                    ls[from..to].join("\n")
                });
                if factory.is_match(&text) {
                    out.push(n.clone());
                }
            }
            out
        } else {
            let name = &inner[..inner.len() - ".getState".len()];
            if !re!(r"^[A-Za-z0-9_$]+$").is_match(name) {
                return Ok(None);
            }
            let mut import_ref = r.clone();
            import_ref.reference_name = name.to_string();
            import_ref.reference_kind = "references".to_string();
            match self.resolve_via_import(&import_ref)?.map(|c| c.node) {
                Some(node) => {
                    if self.import_shadowed_at(name, r)? {
                        return Ok(None);
                    }
                    vec![node]
                }
                None => {
                    let mut local = Vec::new();
                    for n in self.nodes_by_name(name)?.iter() {
                        if n.file_path == r.file_path && self.is_lexically_reachable(n, r)? {
                            local.push(n.clone());
                        }
                    }
                    local
                }
            }
        };
        if holders.len() != 1 {
            return Ok(None);
        }
        let holder = holders[0].clone();
        if selector {
            // Only a Zustand hook promises to return the selector's result:
            // `const useStore = create(...)` with `create` imported from zustand.
            static FACTORY: LazyLock<Affix> = LazyLock::new(|| {
                Affix::new(r"(?-u:\b)(?:const|let)\s+", r"\s*=\s*([A-Za-z0-9_$]+)\s*[<(]", false, false, false)
            });
            let text = self.read_file(&holder.file_path).map_or(String::new(), |ls| {
                let from = ((holder.start_line - 1).max(0) as usize).min(ls.len());
                let to = (holder.end_line.max(0) as usize).clamp(from, ls.len());
                ls[from..to].join("\n")
            });
            let Some(factory) = FACTORY.capture(&text, &holder.name).map(str::to_string) else {
                return Ok(None);
            };
            let zustand = self.import_mappings(&holder.file_path)?.iter().any(|m| {
                m.local_name == factory && m.source == "zustand" && (m.exported_name == "create" || m.is_default)
            });
            if !zustand {
                return Ok(None);
            }
        }
        self.resolve_object_literal_member(&holder, member, r, 0.9, "instance-method")
    }
}
