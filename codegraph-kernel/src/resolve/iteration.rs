//! Receiver evidence from a scoped construct (receiver-iteration.ts):
//! inferIterationReceiver — Kotlin `x.let { it.m() }` / `x.also { v -> v.m() }`
//! and Go `for _, v := range xs { v.m() }` — and inferGuardedReceiver, a PHP
//! `if ($x instanceof T) { $x->m(); }` body.
//!
//! The TS side walks the kernel's serialized tree (tree.rs), whose positions
//! are UTF-16 columns; this walks the tree-sitter tree directly and converts
//! byte columns the same way, so `descendantForPosition` picks the same node.

use super::*;
use tree_sitter::Node as TsNode;

/// A receiver type and the site its type resolves from.
pub(super) struct IterationHit {
    pub(super) ty: String,
    pub(super) site: ResolveRefIn,
}

/// (row, UTF-16 column) of a node boundary, as the TS facade reports it.
fn point16(text: &str, byte: usize, point: tree_sitter::Point) -> (usize, usize) {
    let start = byte.saturating_sub(point.column);
    (point.row, utf16_len(&text[start..byte]))
}

fn before(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

/// descendantForPosition (tree.ts): descend into the first child, named or
/// not, whose span contains the point, until none does.
pub(super) fn descendant_for_position<'t>(root: TsNode<'t>, text: &str, at: (usize, usize)) -> TsNode<'t> {
    let mut node = root;
    'down: loop {
        let mut cursor = node.walk();
        for c in node.children(&mut cursor) {
            let start = point16(text, c.start_byte(), c.start_position());
            let end = point16(text, c.end_byte(), c.end_position());
            if before(start, at) && before(at, end) {
                node = c;
                continue 'down;
            }
        }
        return node;
    }
}

pub(super) fn named_children<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// descendantsOfType (tree.ts): preorder, the node itself included.
fn descendants_of_type<'t>(node: TsNode<'t>, kind: &str, out: &mut Vec<TsNode<'t>>) {
    if node.kind() == kind {
        out.push(node);
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        descendants_of_type(c, kind, out);
    }
}

/// A Kotlin lambda's declared parameter names, or None when it declares
/// none (it then binds `it`, unless its callee passes the value as `this`).
fn kotlin_declared_lambda_names<'a>(lambda: TsNode, text: &'a str) -> Option<Vec<&'a str>> {
    let params = named_children(lambda).into_iter().find(|n| n.kind() == "lambda_parameters")?;
    let mut ids = Vec::new();
    descendants_of_type(params, "simple_identifier", &mut ids);
    Some(ids.into_iter().map(|n| node_text(n, text)).collect())
}

/// The name a Kotlin call's callee is written as: `run` for `run { }`,
/// `let` for `x?.let { }`.
fn kotlin_callee_name<'a>(callee: TsNode, text: &'a str) -> &'a str {
    match callee.kind() {
        "navigation_expression" => named_children(callee)
            .get(1)
            .map_or("", |m| node_text(*m, text).trim_start_matches(['?', '.'])),
        _ => node_text(callee, text),
    }
}

/// Whether a Kotlin signature `(a: A, block: (T) -> Unit)` ends in a
/// function-typed parameter, so a call can pass it a trailing lambda.
fn kotlin_takes_trailing_lambda(signature: Option<&str>) -> bool {
    let Some(sig) = signature else { return false };
    let sig = sig.trim();
    let inner = if sig.starts_with('(') {
        let mut depth = 0usize;
        let close = sig.char_indices().find_map(|(i, ch)| {
            if ch == '(' { depth += 1; }
            else if ch == ')' { depth -= 1; if depth == 0 { return Some(i); } }
            None
        });
        close.map(|end| &sig[1..end]).unwrap_or(sig)
    } else { sig };
    // Depth-0 split points, reading `->` as an arrow rather than a closer.
    let top_level = |s: &str, target: u8| -> Vec<usize> {
        let (b, mut depth, mut out, mut i) = (s.as_bytes(), 0i32, Vec::new(), 0);
        while i < b.len() {
            match b[i] {
                b'-' if b.get(i + 1) == Some(&b'>') => {
                    if target == b'-' && depth == 0 {
                        out.push(i);
                    }
                    i += 1;
                }
                b'(' | b'<' | b'[' => depth += 1,
                b')' | b'>' | b']' => depth -= 1,
                c if c == target && depth == 0 => out.push(i),
                _ => {}
            }
            i += 1;
        }
        out
    };
    // The opening parenthesis closes only at the end.
    let encloses = |s: &str| {
        let mut depth = 0i32;
        s.bytes().enumerate().all(|(i, c)| {
            match c {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            depth > 0 || i == s.len() - 1
        })
    };
    let last = top_level(inner, b',').last().map_or(inner, |&i| &inner[i + 1..]);
    let Some(&colon) = top_level(last, b':').first() else { return false };
    let mut ty = &last[colon + 1..];
    if let Some(&eq) = top_level(ty, b'=').first() {
        ty = &ty[..eq];
    }
    let mut ty = ty.trim().trim_end_matches('?').trim();
    // `((Int) -> Unit)?`: parentheses around the whole type.
    if ty.starts_with('(') && ty.ends_with(')') && encloses(ty) {
        ty = ty[1..ty.len() - 1].trim();
    }
    let ty = ty.strip_prefix("suspend").map_or(ty, str::trim_start);
    !top_level(ty, b'-').is_empty()
}

/// The (row, UTF-16 column) site of `node` in `r`'s file.
pub(super) fn site_at(node: TsNode, text: &str, r: &ResolveRefIn) -> ResolveRefIn {
    let (row, col) = point16(text, node.start_byte(), node.start_position());
    let mut site = r.clone();
    site.line = row as i64 + 1;
    site.column = col as i64;
    site
}

/// The call a Kotlin lambda is an argument of: climb through the argument
/// wrappers, never out of an enclosing body.
fn kotlin_lambda_call(lambda: TsNode) -> Option<TsNode> {
    let mut call = lambda.parent();
    while let Some(c) = call {
        match c.kind() {
            "call_expression" => return Some(c),
            "annotated_lambda" | "call_suffix" | "value_argument" | "value_arguments" => call = c.parent(),
            _ => return None,
        }
    }
    None
}

/// A Java lambda's parameter names: `x ->`, `(x, y) ->`, `(T x) ->`.
fn java_lambda_names<'a>(lambda: TsNode, text: &'a str) -> Vec<&'a str> {
    let Some(params) = lambda.child_by_field_name("parameters") else { return Vec::new() };
    if params.kind() == "identifier" {
        return vec![node_text(params, text)];
    }
    named_children(params)
        .into_iter()
        .filter_map(|p| match p.kind() {
            "identifier" => Some(p),
            _ => p.child_by_field_name("name"),
        })
        .map(|n| node_text(n, text))
        .collect()
}

/// The result types of a Go signature `(params) T` / `(params) (A, B)`,
/// split at top-level commas; `None` when the shape is anything else.
fn go_result_types(signature: &str) -> Option<Vec<&str>> {
    let close = top_level_close(signature)?;
    let rest = signature[close + 1..].trim();
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let Some(inner) = rest.strip_prefix('(') else { return Some(vec![rest]) };
    if top_level_close(rest)? != rest.len() - 1 {
        return None;
    }
    let inner = &inner[..inner.len() - 1];
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, b) in inner.bytes().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(inner[start..].trim());
    Some(out)
}

/// Byte index of the `)` closing the `(` that `s` starts with.
fn top_level_close(s: &str) -> Option<usize> {
    if !s.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn node_text<'a>(node: TsNode, text: &'a str) -> &'a str {
    &text[node.start_byte()..node.end_byte()]
}

impl KernelResolver {
    /// `file`'s tree, parsed once per resolver instead of once per ref.
    pub(super) fn parsed_tree(&mut self, file: &Rc<SourceFile>, r: &ResolveRefIn) -> Option<Rc<tree_sitter::Tree>> {
        let key = format!("{}\0{}", r.language, r.file_path);
        if let Some((source, tree)) = self.tree_cache.get(&key) {
            if Rc::ptr_eq(source, file) {
                return tree.clone();
            }
        }
        let tree = crate::tree::parse_with_cached_parser(file.text(), &r.language).ok().map(Rc::new);
        self.tree_cache.put(key, (file.clone(), tree.clone()));
        tree
    }

    /// The call expression that supplies a Kotlin navigation receiver across lines.
    pub(super) fn kotlin_chain_receiver_call(&mut self, r: &ResolveRefIn) -> Option<String> {
        let file = self.read_file(&r.file_path)?;
        let tree = self.parsed_tree(&file, r)?;
        let mut node = descendant_for_position(tree.root_node(), file.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1));
        if node.kind() != "simple_identifier" { return None; }
        while let Some(parent) = node.parent() {
            if parent.kind() == "navigation_expression" {
                let receiver = parent.named_child(0)?;
                return (receiver.kind() == "call_expression").then(|| node_text(receiver, file.text()).to_string());
            }
            if parent.kind() != "navigation_suffix" { return None; }
            node = parent;
        }
        None
    }

    /// The names a Kotlin lambda binds: its declared parameters, else
    /// implicit `it`, except a lambda given to the stdlib `run`, `apply` or
    /// `with`, which takes its value as `this` and binds nothing. A
    /// user-defined function of one of those names is the callee when the
    /// project declares it, and its lambda binds `it` like any other.
    fn kotlin_lambda_names<'a>(&mut self, lambda: TsNode, text: &'a str, r: &ResolveRefIn) -> Res<Vec<&'a str>> {
        if let Some(names) = kotlin_declared_lambda_names(lambda, text) {
            return Ok(names);
        }
        let Some(mut callee) = kotlin_lambda_call(lambda).and_then(|c| named_children(c).into_iter().next()) else {
            return Ok(vec!["it"]);
        };
        // `with(x) { }`: the arguments before a trailing lambda parse as a
        // call of their own.
        while callee.kind() == "call_expression" {
            let Some(inner) = named_children(callee).into_iter().next() else { break };
            callee = inner;
        }
        let name = kotlin_callee_name(callee, text);
        if matches!(name, "run" | "apply" | "with") && !self.kotlin_user_scope_callee(callee, name, text, r)? {
            return Ok(Vec::new());
        }
        Ok(vec!["it"])
    }

    /// Whether a `run`/`apply`/`with` callee is a function the project
    /// declares rather than the stdlib one: a top-level Kotlin function of
    /// that name visible here for a bare call, a member or visible extension
    /// on the receiver's type for `x.run { }`. Either must take a trailing
    /// lambda. An untyped receiver keeps the stdlib reading.
    pub(super) fn kotlin_user_scope_callee(&mut self, callee: TsNode, name: &str, text: &str, r: &ResolveRefIn) -> Res<bool> {
        let declared: Vec<Arc<KNode>> = self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| n.language == "kotlin" && matches!(n.kind.as_str(), "function" | "method"))
            .cloned()
            .collect();
        if declared.is_empty() {
            return Ok(false);
        }
        if callee.kind() != "navigation_expression" {
            for n in declared.iter().filter(|n| n.kind == "function") {
                if kotlin_takes_trailing_lambda(n.signature.as_deref()) && self.kotlin_top_level_visible(n, r)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let Some(root) = named_children(callee).into_iter().next().filter(|n| n.kind() == "simple_identifier") else {
            return Ok(false);
        };
        let site = site_at(callee, text, r);
        let root_name = node_text(root, text).to_string();
        let Some(ty) = self.infer_local_receiver_type(&root_name, &site, true)? else {
            return Ok(false);
        };
        self.kotlin_user_member(&ty, name, &site)
    }

    /// Whether Kotlin type `ty` has a member `method`, its own or
    /// inherited, or a visible extension `fun Ty.method`, that takes a
    /// trailing lambda.
    pub(super) fn kotlin_user_member(&mut self, ty: &str, method: &str, site: &ResolveRefIn) -> Res<bool> {
        if let Some(m) = self.match_bound_type_member(ty, method, site)? {
            return Ok(kotlin_takes_trailing_lambda(m.node.signature.as_deref()));
        }
        let simple = ty.rsplit(['.', ':']).next().unwrap_or(ty);
        let simple = simple.split('<').next().unwrap_or(simple);
        let extensions: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{simple}::{method}"))?
            .iter()
            .filter(|n| n.language == "kotlin" && n.kind == "method" && kotlin_takes_trailing_lambda(n.signature.as_deref()))
            .cloned()
            .collect();
        for n in &extensions {
            if self.kotlin_top_level_visible(n, site)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a top-level Kotlin declaration (a function, or an extension)
    /// is visible from `r`'s file: the same file or package, or imported by
    /// name or by its package's wildcard.
    pub(super) fn kotlin_top_level_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.file_path == r.file_path {
            return Ok(true);
        }
        let package = |this: &mut Self, file: &str| -> Res<Option<String>> {
            Ok(this.nodes_in_file(file)?.iter().find(|n| n.kind == "namespace").map(|n| n.qualified_name.clone()))
        };
        let theirs = package(self, &n.file_path)?;
        if theirs == package(self, &r.file_path)? {
            return Ok(true);
        }
        let Some(pkg) = theirs else { return Ok(false) };
        let (wildcard, named) = (format!("{pkg}.*"), format!("{pkg}.{}", n.name));
        Ok(self
            .import_mappings(&r.file_path)?
            .iter()
            .any(|i| (i.is_namespace && i.source == wildcard) || (i.local_name == n.name && i.source == named)))
    }

    /// inferIterationReceiver — `None` when no scoped construct introduces
    /// the receiver (or its type can't be proven).
    pub(super) fn infer_iteration_receiver(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<IterationHit>> {
        // The gate is the TS parse precondition (language, and a declaration
        // shaped like a range/lambda binding).
        if !self.mc_iteration_gate(receiver, r)? {
            return Ok(None);
        }
        self.iteration_receiver_in_tree(receiver, r)
    }

    /// inferIterationReceiver past its gate: the nearest construct at the
    /// call site that binds `receiver`.
    pub(super) fn iteration_receiver_in_tree(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<IterationHit>> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let text = lines.text();
        if text.is_empty() {
            return Ok(None);
        }
        let Some(tree) = self.parsed_tree(&lines, r) else {
            return Ok(None);
        };
        let at = ((r.line - 1).max(0) as usize, r.column.max(0) as usize);
        let mut cur = Some(descendant_for_position(tree.root_node(), text, at));
        while let Some(node) = cur {
            cur = node.parent();
            if r.language == "kotlin" && node.kind() == "lambda_literal" {
                // The nearest lambda that binds `it` owns it, even when its type is unknown.
                if !self.kotlin_lambda_names(node, text, r)?.contains(&receiver) {
                    continue;
                }
                // Only the lambda passed to the call takes the receiver.
                let Some(call) = kotlin_lambda_call(node) else {
                    return Ok(None);
                };
                let Some(navigation) = named_children(call).into_iter().next() else {
                    return Ok(None);
                };
                if navigation.kind() != "navigation_expression" {
                    return Ok(None);
                }
                let nav = named_children(navigation);
                let root = nav.first().copied();
                let method = nav.get(1).map(|m| node_text(*m, text).trim_start_matches(['?', '.']));
                let Some(root) = root.filter(|n| n.kind() == "simple_identifier") else {
                    return Ok(None);
                };
                if !matches!(method, Some("let" | "also")) {
                    return Ok(None);
                }
                let site = site_at(navigation, text, r);
                let root_name = node_text(root, text).to_string();
                let Some(ty) = self.infer_local_receiver_type(&root_name, &site, true)? else {
                    return Ok(None);
                };
                // A member or extension `let`/`also` on the receiver's type
                // wins over the stdlib one, and its lambda's `it` is whatever
                // it passes.
                if let Some(method) = method {
                    if self.kotlin_user_member(&ty, method, &site)? {
                        return Ok(None);
                    }
                }
                return Ok(Some(IterationHit { ty, site }));
            }
            if r.language == "go" && node.kind() == "for_statement" {
                let range = named_children(node).into_iter().find(|n| n.kind() == "range_clause");
                let names = range.and_then(|rc| rc.child_by_field_name("left")).map(named_children);
                let collection = range.and_then(|rc| rc.child_by_field_name("right"));
                let (Some(names), Some(collection)) = (names, collection) else {
                    continue;
                };
                let Some(index) = names.iter().position(|n| node_text(*n, text) == receiver) else {
                    continue;
                };
                if index != 1 {
                    return Ok(None);
                }
                return self.go_range_element(collection, text, r);
            }
        }
        Ok(None)
    }

    /// Whether a Java/Kotlin lambda enclosing the call declares `receiver`
    /// as its parameter after the receiver's binding row at `binding_line`,
    /// hiding that binding (a field, an outer local or parameter).
    pub(super) fn lambda_param_shadows(&mut self, receiver: &str, r: &ResolveRefIn, binding_line: i64) -> Res<bool> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        // A lambda with a named parameter has its `->` on a line between the
        // binding and the call, next to the name.
        let from = (binding_line.max(1) - 1) as usize;
        let to = (r.line.max(0) as usize).min(lines.len());
        if from >= to || !lines[from..to].iter().any(|l| l.contains("->") && has_word(l, receiver)) {
            return Ok(false);
        }
        let text = lines.text();
        let Some(tree) = self.parsed_tree(&lines, r) else {
            return Ok(false);
        };
        let at = ((r.line - 1).max(0) as usize, r.column.max(0) as usize);
        let mut cur = Some(descendant_for_position(tree.root_node(), text, at));
        while let Some(node) = cur {
            cur = node.parent();
            let names = match (r.language.as_str(), node.kind()) {
                ("kotlin", "lambda_literal") => self.kotlin_lambda_names(node, text, r)?,
                ("java", "lambda_expression") => java_lambda_names(node, text),
                _ => continue,
            };
            if names.contains(&receiver) {
                return Ok(binding_line <= node.start_position().row as i64 + 1);
            }
        }
        Ok(false)
    }

    /// The declared type of the Java lambda parameter `receiver` at the
    /// call site: `Foo` for `(Foo f) -> f.bar()`. None when the nearest
    /// lambda binding it leaves the type to inference (`f ->`, `(f, g) ->`,
    /// `(var f) ->`) or no lambda binds it.
    pub(super) fn java_lambda_param_type(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let text = lines.text();
        let Some(tree) = self.parsed_tree(&lines, r) else {
            return Ok(None);
        };
        let at = ((r.line - 1).max(0) as usize, r.column.max(0) as usize);
        let mut cur = Some(descendant_for_position(tree.root_node(), text, at));
        while let Some(node) = cur {
            cur = node.parent();
            if node.kind() != "lambda_expression" || !java_lambda_names(node, text).contains(&receiver) {
                continue;
            }
            let params = node.child_by_field_name("parameters");
            let typed = params.filter(|p| p.kind() == "formal_parameters").and_then(|p| {
                named_children(p).into_iter().find(|fp| {
                    fp.child_by_field_name("name").is_some_and(|n| node_text(n, text) == receiver)
                })
            });
            let ty = typed.and_then(|fp| fp.child_by_field_name("type")).map(|t| node_text(t, text));
            let ty = ty.map(|t| t.split('<').next().unwrap_or(t).trim()).filter(|t| !t.is_empty() && *t != "var");
            return Ok(ty.map(str::to_string));
        }
        Ok(None)
    }

    /// The element type of a Go range collection: a field of a typed owner
    /// (`o.items`), a declared slice/array/map, or a factory whose signature
    /// returns a slice.
    fn go_range_element(
        &mut self,
        collection: TsNode,
        text: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<IterationHit>> {
        if collection.kind() == "selector_expression" {
            let base = collection.child_by_field_name("operand");
            let field = collection.child_by_field_name("field");
            let (Some(base), Some(field)) = (base.filter(|b| b.kind() == "identifier"), field) else {
                return Ok(None);
            };
            // The owner is typed where the range reads it, not at the call:
            // the loop body may shadow it (`o := OuterB{}`).
            let (row, col) = point16(text, base.start_byte(), base.start_position());
            let mut at_range = r.clone();
            at_range.line = row as i64 + 1;
            at_range.column = col as i64;
            let Some(owner_type) = self.infer_local_receiver_type(node_text(base, text), &at_range, true)? else {
                return Ok(None);
            };
            // A field's slice element type is resolved in its owner's file.
            let Some(owner) = self.resolve_bound_type(&owner_type, r, 0)? else {
                return Ok(None);
            };
            let Some(owner_lines) = self.read_file(&owner.file_path) else {
                return Ok(None);
            };
            let from = ((owner.start_line - 1).max(0) as usize).min(owner_lines.len());
            let to = (owner.end_line.max(0) as usize).clamp(from, owner_lines.len());
            let body = owner_lines[from..to].join("\n");
            // `\bFIELD\s+\[\d*\]\s*\*?([\w.]+)`
            static FIELD: LazyLock<Affix> = LazyLock::new(|| {
                Affix::new("", r"\s+\[[0-9]*\]\s*\*?([A-Za-z0-9_.]+)", true, false, false)
            });
            return Ok(FIELD.capture(&body, node_text(field, text)).map(|ty| {
                let mut site = r.clone();
                site.file_path = owner.file_path.clone();
                site.line = owner.start_line;
                IterationHit { ty: ty.to_string(), site }
            }));
        }
        if collection.kind() != "identifier" {
            return Ok(None);
        }
        let name = node_text(collection, text);
        let bindings = self.bindings(&r.file_path)?;
        let Some(binding) = innermost_binding(&bindings, name, Some(r.line)).cloned() else {
            return Ok(None);
        };
        let declaration = self
            .read_file(&r.file_path)
            .and_then(|ls| ls.get((binding.line - 1).max(0) as usize).cloned())
            .unwrap_or_default();
        // `\bNAME\s*(?::=|=)?\s*(?:\[\d*\]|map\[[^\]]+\])\s*\*?([\w.]+)` — the
        // first slice/array binding is the index; map keys need their own type.
        static DECLARED: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new(
                "",
                r"\s*(?::=|=)?\s*(?:\[[0-9]*\]|map\[[^\]]+\])\s*\*?([A-Za-z0-9_.]+)",
                true,
                false,
                false,
            )
        });
        if let Some(ty) = DECLARED.capture(&declaration, name) {
            return Ok(Some(IterationHit { ty: ty.to_string(), site: r.clone() }));
        }
        // `\bNAME(?:\s*,\s*\w+)*\s*:=\s*([\w.]+)\s*\(`
        static FACTORY: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new("", r"(?:\s*,\s*[A-Za-z0-9_]+)*\s*:=\s*([A-Za-z0-9_.]+)\s*\(", true, false, false)
        });
        let Some(factory) = FACTORY.capture(&declaration, name).map(str::to_string) else {
            return Ok(None);
        };
        // The result the collection takes: its position in the `:=` list.
        let Some(slot) = re!(r"(?-u:\b)([A-Za-z0-9_]+(?:\s*,\s*[A-Za-z0-9_]+)*)\s*:=")
            .captures_iter(&declaration)
            .find_map(|c| c[1].split(',').position(|n| n.trim() == name))
        else {
            return Ok(None);
        };
        // resolveCall: matchBoundReceiverCall on the factory name at the
        // collection's declaration — undefined for a non-receiver shape.
        let mut call_site = r.clone();
        call_site.line = binding.line;
        call_site.reference_name = factory;
        if !is_binding_receiver_call(&call_site) {
            return Ok(None);
        }
        let Some(callee) = self.bound_receiver_claim(&call_site)?.map(|c| c.node) else {
            return Ok(None);
        };
        let element = callee
            .signature
            .as_deref()
            .and_then(go_result_types)
            .and_then(|results| results.get(slot).copied())
            .and_then(|t| re!(r"^\[\]\s*\*?([A-Za-z0-9_.]+)$").captures(t).map(|c| c[1].to_string()));
        Ok(element.map(|ty| {
            let mut site = r.clone();
            site.file_path = callee.file_path.clone();
            site.line = callee.start_line;
            IterationHit { ty, site }
        }))
    }
}

impl KernelResolver {
    /// inferGuardedReceiver — the `T` of the nearest enclosing
    /// `if ($receiver instanceof T)` whose body holds the call, unless the
    /// receiver is redeclared or reassigned inside it first.
    pub(super) fn infer_guarded_receiver(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if r.language != "php" {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        if lines.lines_containing("instanceof").is_empty() {
            return Ok(None);
        }
        let text = lines.text();
        let Some(tree) = self.parsed_tree(&lines, r) else {
            return Ok(None);
        };
        let at = ((r.line - 1).max(0) as usize, r.column.max(0) as usize);
        let call = descendant_for_position(tree.root_node(), text, at);
        let mut cur = Some(call);
        while let Some(node) = cur {
            cur = node.parent();
            if matches!(
                node.kind(),
                "anonymous_function"
                    | "anonymous_function_creation_expression"
                    | "arrow_function"
                    | "function_definition"
                    | "method_declaration"
            ) {
                return Ok(None);
            }
            if node.kind() != "if_statement" {
                continue;
            }
            let Some(body) = node.child_by_field_name("body") else { continue };
            if call.start_byte() < body.start_byte() || call.end_byte() > body.end_byte() {
                continue;
            }
            let Some(condition) = node.child_by_field_name("condition") else { continue };
            let Some(m) = re!(r"^\(\s*\$([A-Za-z0-9_]+)\s+instanceof\s+([A-Za-z0-9_\\]+)\s*\)$")
                .captures(node_text(condition, text))
            else {
                continue;
            };
            if &m[1] != receiver {
                continue;
            }
            let if_line = node.start_position().row as i64 + 1;
            let shadow = self.bindings(&r.file_path)?.iter().any(|b| {
                b.name == receiver && b.scope_start > if_line && b.scope_start <= r.line && b.scope_end >= r.line
            });
            let mut assignments = Vec::new();
            // `$x =& $other` rebinds `$x` as surely as `$x = $other`.
            descendants_of_type(body, "assignment_expression", &mut assignments);
            descendants_of_type(body, "reference_assignment_expression", &mut assignments);
            let var = format!("${receiver}");
            let assigned = assignments.iter().any(|a| {
                a.start_byte() < call.start_byte()
                    && a.child_by_field_name("left").is_some_and(|l| node_text(l, text) == var)
            });
            // `foreach ($x->kids as $x)` / `as $k => $x` rebinds it too.
            let mut loops = Vec::new();
            descendants_of_type(body, "foreach_statement", &mut loops);
            let iterated = loops.iter().any(|l| {
                let body = l.child_by_field_name("body").map(|b| b.id());
                l.start_byte() < call.start_byte()
                    && named_children(*l).into_iter().skip(1).filter(|c| Some(c.id()) != body).any(|c| {
                        let mut vars = Vec::new();
                        descendants_of_type(c, "variable_name", &mut vars);
                        vars.iter().any(|v| node_text(*v, text) == var)
                    })
            });
            let assigned = assigned || iterated;
            return Ok((!shadow && !assigned).then(|| m[2].to_string()));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::kotlin_takes_trailing_lambda as trailing;

    #[test]
    fn a_trailing_lambda_needs_a_function_typed_last_parameter() {
        for sig in [
            "(block: (Widget) -> Unit)",
            "(items: List<T>, f: (T) -> Unit)",
            "(f: suspend () -> Unit)",
            "(f: ((Int) -> Unit)?)",
            "(block: Widget.() -> Unit = {})",
            "(a: Map<String, Int>, b: (Pair<A, B>) -> Unit)",
        ] {
            assert!(trailing(Some(sig)), "{sig}");
        }
        for sig in ["()", "(job: Job)", "(f: (Int) -> Unit, n: Int)", "(xs: List<(Int) -> Unit>)", "(a: (A) -> (B), n: Int)"] {
            assert!(!trailing(Some(sig)), "{sig}");
        }
        assert!(!trailing(None));
    }
}
