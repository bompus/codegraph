//! Rust binding scopes: which `use` or local binding a bare name means at a
//! call site (upstream's rustUsesOf/rustVisibleUse and rust-scopes.ts).
//!
//! A `use` binds in the block (or module) it is written in, a parameter in
//! its function's body, a `let` from its statement to the end of its block,
//! an `if let`/`match`/`for` pattern in the arm it guards, and a nested `fn`
//! item in the block that declares it. A `use` in a macro's braces at the
//! module's level is the module's, because the tokens are the module's code.
//! A nested `fn` does not see the parameters or locals of the function
//! around it, so a local binding is keyed by the function that encloses it.

use super::*;
use super::rust_modules::rust_paths::{rust_ancestor, rust_at, rust_children, rust_text, rust_use_leaves};

#[derive(Clone, Debug)]
pub(super) enum RustBound {
    /// A `use` path: local name → the path it names. `use_line`/`use_col`
    /// locate the declaration so its module (an inline `mod`'s) is known.
    Use {
        path: Vec<String>,
        use_line: i64,
        use_col: i64,
    },
    /// A local `fn` item declared in a block, by its start line.
    FnItem(i64),
    /// A `let` holding a value by path (`let take = take::<u8>;` or
    /// `let take = crate::other::take;`): the path and the initializer's
    /// position, where the same name is looked up again.
    Path {
        path: Vec<String>,
        init_line: i64,
        init_col: i64,
    },
    /// A parameter, closure parameter, `let` of a non-function value, or a
    /// pattern binding: a local answers the name, never a project item.
    Local,
}

#[derive(Clone, Debug)]
pub(super) struct RustScopeBinding {
    name: String,
    /// First and last byte where the name means this binding.
    start: usize,
    end: usize,
    depth: usize,
    /// The innermost `function_item` around the declaration; None at module
    /// level. A local binding only answers refs under the same function.
    owner: Option<usize>,
    bound: RustBound,
}

impl RustScopeBinding {
    fn is_local(&self) -> bool {
        !matches!(self.bound, RustBound::Use { .. })
    }
}

#[derive(Default)]
pub(super) struct RustScopeFacts {
    bindings: Vec<RustScopeBinding>,
}

impl RustScopeFacts {
    /// The innermost binding of `name` at `offset`, preferring a local over
    /// an outer `use` and a later `let` over an earlier one.
    pub(super) fn at(
        &self,
        offset: usize,
        owner: Option<usize>,
        name: &str,
    ) -> Option<&RustBound> {
        self.bindings
            .iter()
            .filter(|b| {
                b.name == name
                    && b.start <= offset
                    && offset < b.end
                    && (!b.is_local() || b.owner == owner)
            })
            .max_by_key(|b| (b.depth, b.start))
            .map(|b| &b.bound)
    }
}

/// Build the file's scope facts from its tree.
pub(super) fn build_rust_scopes(text: &str, tree: &tree_sitter::Tree) -> RustScopeFacts {
    struct Walk<'a> {
        text: &'a str,
        out: Vec<RustScopeBinding>,
    }
    impl<'a> Walk<'a> {
        fn push(
            &mut self,
            name: String,
            start: usize,
            end: usize,
            depth: usize,
            owner: Option<usize>,
            bound: RustBound,
        ) {
            self.out.push(RustScopeBinding { name, start, end, depth, owner, bound });
        }
        fn binds(&mut self, pattern: tree_sitter::Node<'_>, start: usize, end: usize, depth: usize, owner: Option<usize>) {
            let mut names = Vec::new();
            pattern_names(pattern, self.text, &mut names);
            for name in names {
                self.push(name, start, end, depth, owner, RustBound::Local);
            }
        }
        /// One `use` declaration's leaves, bound at the scope around it.
        fn use_bindings(
            &mut self,
            node: tree_sitter::Node<'_>,
            start: usize,
            end: usize,
            depth: usize,
            owner: Option<usize>,
        ) {
            let mut leaves = Vec::new();
            rust_use_leaves(node, self.text, &[], &mut leaves);
            let line = node.start_position().row as i64 + 1;
            let col = node.start_position().column as i64;
            for (local, path) in leaves {
                if local == "*" {
                    continue;
                }
                self.push(
                    local,
                    start.min(node.start_byte()),
                    end,
                    depth,
                    owner,
                    RustBound::Use { path, use_line: line, use_col: col },
                );
            }
        }
        /// `use`s inside a macro's token tree are the code around the
        /// invocation (`items! { use …; }`, `cfg_if!`'s nested braces).
        fn macro_uses(&mut self, node: tree_sitter::Node<'_>, start: usize, end: usize, depth: usize, owner: Option<usize>) {
            let mut stack = vec![node];
            while let Some(n) = stack.pop() {
                if n.kind() == "token_tree" {
                    let body = rust_text(n, self.text);
                    for cap in re!(r"(?m)\b(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);")
                        .captures_iter(body)
                    {
                        let snippet = format!("use {};", &cap[1]);
                        if let Ok(t) = crate::tree::parse_with_cached_parser(&snippet, "rust") {
                            for child in rust_children(t.root_node()) {
                                let mut leaves = Vec::new();
                                rust_use_leaves(child, &snippet, &[], &mut leaves);
                                let line = node.start_position().row as i64 + 1;
                                let col = node.start_position().column as i64;
                                for (local, path) in leaves {
                                    if local == "*" {
                                        continue;
                                    }
                                    self.push(
                                        local,
                                        start.min(node.start_byte()),
                                        end,
                                        depth,
                                        owner,
                                        RustBound::Use { path, use_line: line, use_col: col },
                                    );
                                }
                            }
                        }
                    }
                    continue;
                }
                for child in rust_children(n) {
                    stack.push(child);
                }
            }
        }
        fn walk(
            &mut self,
            node: tree_sitter::Node<'_>,
            scope: (usize, usize),
            depth: usize,
            module_scope: bool,
            owner: Option<usize>,
        ) {
            let (scope_start, scope_end) = scope;
            match node.kind() {
                "use_declaration" => {
                    self.use_bindings(node, scope_start, scope_end, depth, owner);
                    return;
                }
                "let_declaration" => {
                    if let Some(pattern) = node.child_by_field_name("pattern") {
                        let start = node.end_byte();
                        let mut names = Vec::new();
                        pattern_names(pattern, self.text, &mut names);
                        let value = node.child_by_field_name("value").and_then(|v| {
                            let path = value_path(v, self.text)?;
                            Some(RustBound::Path {
                                path,
                                init_line: v.start_position().row as i64 + 1,
                                init_col: v.start_position().column as i64,
                            })
                        });
                        for name in names {
                            self.push(
                                name,
                                start,
                                scope_end,
                                depth,
                                owner,
                                value.clone().unwrap_or(RustBound::Local),
                            );
                        }
                        // The value can hold code of its own — a closure, a
                        // block — whose bindings and calls need walking.
                        if let Some(v) = node.child_by_field_name("value") {
                            self.walk(v, scope, depth, module_scope, owner);
                        }
                    }
                    return;
                }
                "function_item" | "function_signature_item" => {
                    let fn_start = node.start_byte();
                    if !module_scope {
                        if let Some(name) = node.child_by_field_name("name") {
                            let name = rust_text(name, self.text).to_string();
                            self.push(
                                name,
                                scope_start,
                                scope_end,
                                depth,
                                owner,
                                RustBound::FnItem(node.start_position().row as i64 + 1),
                            );
                        }
                    }
                    if let Some(body) = node.child_by_field_name("body") {
                        if let Some(params) = node.child_by_field_name("parameters") {
                            for p in rust_children(params) {
                                if p.kind() == "parameter" {
                                    if let Some(pattern) = p.child_by_field_name("pattern") {
                                        self.binds(
                                            pattern,
                                            body.start_byte(),
                                            body.end_byte(),
                                            depth + 1,
                                            Some(fn_start),
                                        );
                                    }
                                }
                            }
                        }
                        self.walk(body, (body.start_byte(), body.end_byte()), depth + 1, false, Some(fn_start));
                    }
                    return;
                }
                "closure_expression" => {
                    if let Some(body) = node.child_by_field_name("body") {
                        if let Some(params) = node.child_by_field_name("parameters") {
                            for p in rust_children(params) {
                                if let Some(pattern) = p.child_by_field_name("pattern") {
                                    self.binds(
                                        pattern,
                                        body.start_byte(),
                                        body.end_byte(),
                                        depth + 1,
                                        owner,
                                    );
                                } else if matches!(p.kind(), "identifier" | "mut_pattern" | "ref_pattern") {
                                    self.binds(p, body.start_byte(), body.end_byte(), depth + 1, owner);
                                }
                            }
                        }
                        self.walk(body, (body.start_byte(), body.end_byte()), depth + 1, false, owner);
                    }
                    return;
                }
                "block" | "declaration_list" => {
                    self.walk_children(node, (node.start_byte(), node.end_byte()), depth + 1, module_scope, owner);
                    return;
                }
                "mod_item" => {
                    if let Some(body) = node.child_by_field_name("body") {
                        let inner_module = module_scope || owner.is_none();
                        self.walk(
                            body,
                            (body.start_byte(), body.end_byte()),
                            depth + 1,
                            inner_module,
                            if inner_module { None } else { owner },
                        );
                    }
                    return;
                }
                "if_expression" | "while_expression" => {
                    if let Some(cond) = node.child_by_field_name("condition") {
                        let guarded = if node.kind() == "if_expression" {
                            node.child_by_field_name("consequence")
                        } else {
                            node.child_by_field_name("body")
                        };
                        if let Some(guarded) = guarded {
                            self.pattern_condition(cond, guarded, depth + 1, owner);
                        }
                    }
                }
                "match_arm" => {
                    if let (Some(pattern), Some(value)) = (
                        node.child_by_field_name("pattern"),
                        node.child_by_field_name("value"),
                    ) {
                        self.binds(pattern, value.start_byte(), value.end_byte(), depth + 1, owner);
                    }
                }
                "for_expression" => {
                    if let (Some(pattern), Some(body)) = (
                        node.child_by_field_name("pattern"),
                        node.child_by_field_name("body"),
                    ) {
                        self.binds(pattern, body.start_byte(), body.end_byte(), depth + 1, owner);
                    }
                }
                "macro_invocation" => {
                    self.macro_uses(node, scope_start, scope_end, depth, owner);
                    return;
                }
                _ => {}
            }
            self.walk_children(node, scope, depth, module_scope, owner);
        }
        fn pattern_condition(
            &mut self,
            cond: tree_sitter::Node<'_>,
            guarded: tree_sitter::Node<'_>,
            depth: usize,
            owner: Option<usize>,
        ) {
            match cond.kind() {
                "let_condition" => {
                    if let Some(pattern) = cond.child_by_field_name("pattern") {
                        self.binds(pattern, guarded.start_byte(), guarded.end_byte(), depth, owner);
                    }
                }
                "let_chain" => {
                    for c in rust_children(cond) {
                        self.pattern_condition(c, guarded, depth, owner);
                    }
                }
                _ => {}
            }
        }
        fn walk_children(
            &mut self,
            node: tree_sitter::Node<'_>,
            scope: (usize, usize),
            depth: usize,
            module_scope: bool,
            owner: Option<usize>,
        ) {
            for child in rust_children(node) {
                self.walk(child, scope, depth, module_scope, owner);
            }
        }
    }

    let mut walk = Walk { text, out: Vec::new() };
    walk.walk(
        tree.root_node(),
        (0, text.len().saturating_add(1)),
        0,
        true,
        None,
    );
    RustScopeFacts { bindings: walk.out }
}

/// Names a pattern binds. Type positions (`Some`, `Point` in a struct
/// pattern) are not bindings.
fn pattern_names(node: tree_sitter::Node<'_>, text: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" => {
            let name = rust_text(node, text);
            if name != "_" {
                out.push(name.to_string());
            }
        }
        "mut_pattern" | "ref_pattern" | "reference_pattern" | "or_pattern" | "match_pattern"
        | "captured_pattern" | "generic_pattern" | "tuple_pattern" | "slice_pattern" => {
            for child in rust_children(node) {
                pattern_names(child, text, out);
            }
        }
        "tuple_struct_pattern" => {
            let ty = node.child_by_field_name("type");
            for child in rust_children(node) {
                if Some(child) == ty {
                    continue;
                }
                pattern_names(child, text, out);
            }
        }
        "struct_pattern" => {
            for child in rust_children(node) {
                match child.kind() {
                    "field_pattern" => {
                        if let Some(pattern) = child.child_by_field_name("pattern") {
                            pattern_names(pattern, text, out);
                        } else if let Some(name) = child.child_by_field_name("name") {
                            out.push(rust_text(name, text).to_string());
                        }
                    }
                    "shorthand_field_identifier" => {
                        out.push(rust_text(child, text).to_string());
                    }
                    _ => {}
                }
            }
        }
        "field_pattern" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                pattern_names(pattern, text, out);
            } else if let Some(name) = node.child_by_field_name("name") {
                out.push(rust_text(name, text).to_string());
            }
        }
        _ => {}
    }
}

/// The path a `let`'s value is, when it is one: `take`, `take::<u8>`,
/// `crate::other::take`. A closure, call or literal is not a path.
fn value_path(node: tree_sitter::Node<'_>, text: &str) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" | "scoped_identifier" | "crate" | "self" | "super" => {
            let raw = rust_text(node, text);
            let raw = raw.split('<').next().unwrap_or(raw);
            let path: Vec<String> = raw
                .split("::")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            (!path.is_empty()).then_some(path)
        }
        "generic_function" => node.child_by_field_name("function").and_then(|f| value_path(f, text)),
        "parenthesized_expression" => {
            let inner = rust_children(node).into_iter().next()?;
            value_path(inner, text)
        }
        _ => None,
    }
}

impl KernelResolver {
    pub(super) fn rust_scope_facts(
        &mut self,
        file: &str,
    ) -> Option<Rc<RustScopeFacts>> {
        if let Some(hit) = self.rust_scope_memo.get(file) {
            return Some(hit.clone());
        }
        let (source, tree) = self.rust_tree_for(file)?;
        let facts = Rc::new(build_rust_scopes(source.text(), &tree));
        self.rust_scope_memo.insert(file.to_string(), facts.clone());
        Some(facts)
    }

    /// The binding `name` means at a byte offset of `file`, and that offset.
    pub(super) fn rust_binding_at_offset(
        &mut self,
        file: &str,
        offset: usize,
        name: &str,
    ) -> Option<RustBound> {
        let (source, tree) = self.rust_tree_for(file)?;
        let at = tree
            .root_node()
            .named_descendant_for_byte_range(offset.min(source.text().len()), offset.min(source.text().len()))
            .unwrap_or(tree.root_node());
        let owner = rust_ancestor(at, &["function_item"]).map(|n| n.start_byte());
        let facts = self.rust_scope_facts(file)?;
        facts.at(offset, owner, name).cloned()
    }

    /// The binding a bare ref means, read at its call site.
    pub(super) fn rust_binding_at(&mut self, r: &ResolveRefIn) -> Option<RustBound> {
        let (source, tree) = self.rust_tree_for(&r.file_path)?;
        let at = rust_at(&tree, source.text(), r.line, r.column);
        self.rust_binding_at_offset(&r.file_path, at.start_byte(), &r.reference_name)
    }
}

impl KernelResolver {
    /// The items a `use` path names, read from the module the `use` is
    /// written in. An enum-variant tail (`crate::models::E::Good`) resolves
    /// to that enum's own variant, never a namesake in another module.
    pub(super) fn rust_use_path_items(
        &mut self,
        path: &[String],
        file: &str,
        use_line: i64,
        use_col: i64,
    ) -> Res<Option<Vec<Arc<KNode>>>> {
        if path.len() >= 2 {
            let enum_name = &path[path.len() - 2];
            if enum_name.starts_with(|c: char| c.is_ascii_uppercase()) {
                let variant = &path[path.len() - 1];
                let from = self.rust_module_at(file, use_line, use_col);
                let Some(module) = self.rust_walk_module(&path[..path.len() - 2], &from, 0) else {
                    return Ok(None);
                };
                let mut out = Vec::new();
                for node in self.rust_items_in_module(&module, enum_name, false, &mut HashSet::new())? {
                    if node.kind != "enum" {
                        continue;
                    }
                    let wanted = format!("{}::{variant}", node.qualified_name);
                    for member in self.nodes_in_file(&node.file_path)?.iter() {
                        if member.kind == "enum_member"
                            && member.name == *variant
                            && member.qualified_name == wanted
                            && member.start_line >= node.start_line
                            && member.end_line <= node.end_line
                            && !out.iter().any(|m: &Arc<KNode>| m.id == member.id)
                        {
                            out.push(member.clone());
                        }
                    }
                }
                if !out.is_empty() {
                    return Ok(Some(out));
                }
                return Ok(None);
            }
        }
        let from = self.rust_module_at(file, use_line, use_col);
        self.rust_path_items(path, &from, false, &mut HashSet::new())
    }

    /// A variant-shaped `use` tail (`…::E::Good`): its module and enum, so a
    /// failed lookup is terminal rather than a namesake guess.
    fn rust_use_path_is_variant(path: &[String]) -> bool {
        path.len() >= 2
            && path[path.len() - 2].starts_with(|c: char| c.is_ascii_uppercase())
    }

    fn rust_fn_node(&mut self, file: &str, line: i64, name: &str) -> Res<Option<Arc<KNode>>> {
        Ok(self
            .nodes_in_file(file)?
            .iter()
            .find(|n| {
                matches!(n.kind.as_str(), "function" | "method")
                    && n.name == name
                    && n.start_line == line
            })
            .cloned())
    }

    /// A bare name bound by a `use` alias or a local: the item the path
    /// names, the nested `fn` a block declares, or nothing when a local
    /// (a parameter, a `let` of a non-function value) owns the name.
    /// `Some(None)` is terminal; `None` leaves a plain import to name
    /// matching and its scope filter.
    pub(super) fn match_rust_bound_name(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<KCand>>> {
        if r.language != "rust"
            || !matches!(r.reference_kind.as_str(), "calls" | "function_ref")
            || r.reference_name.contains("::")
            || r.reference_name.contains('.')
            || !re!(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(&r.reference_name)
        {
            return Ok(None);
        }
        let Some(bound) = self.rust_binding_at(r) else {
            return Ok(None);
        };
        match bound {
            RustBound::Local => Ok(Some(None)),
            RustBound::FnItem(line) => {
                let node = self.rust_fn_node(&r.file_path, line, &r.reference_name)?;
                Ok(Some(node.map(|node| KCand { node, confidence: 0.9, resolved_by: "scope" })))
            }
            RustBound::Path { path, init_line, init_col } => {
                let path = if path.len() == 1 && path[0] == r.reference_name {
                    // `let take = take;` — the name at the initializer is
                    // the binding the `let` copies.
                    match self.rust_binding_at_offset_at(&r.file_path, init_line, init_col, &r.reference_name) {
                        Some(RustBound::Use { path, .. }) => path,
                        Some(RustBound::FnItem(line)) => {
                            let node = self.rust_fn_node(&r.file_path, line, &r.reference_name)?;
                            return Ok(Some(node.map(|node| KCand { node, confidence: 0.9, resolved_by: "scope" })));
                        }
                        _ => return Ok(Some(None)),
                    }
                } else {
                    path
                };
                let items = self.rust_use_path_items(&path, &r.file_path, init_line, init_col)?;
                Ok(Some(items.and_then(|items| {
                    items.into_iter().next().map(|node| KCand { node, confidence: 0.9, resolved_by: "scope" })
                })))
            }
            RustBound::Use { path, use_line, use_col } => {
                let alias = path.last() != Some(&r.reference_name);
                let variant = Self::rust_use_path_is_variant(&path);
                if !alias && !variant {
                    return Ok(None);
                }
                let items = self.rust_use_path_items(&path, &r.file_path, use_line, use_col)?;
                let Some(node) = items.and_then(|items| items.into_iter().next()) else {
                    return Ok(Some(None));
                };
                if r.reference_kind == "function_ref"
                    && !matches!(node.kind.as_str(), "function" | "method" | "enum_member")
                {
                    return Ok(Some(None));
                }
                Ok(Some(Some(KCand { node, confidence: 0.9, resolved_by: "scope" })))
            }
        }
    }

    fn rust_binding_at_offset_at(
        &mut self,
        file: &str,
        line: i64,
        col: i64,
        name: &str,
    ) -> Option<RustBound> {
        let (source, tree) = self.rust_tree_for(file)?;
        let offset = rust_at(&tree, source.text(), line, col).start_byte();
        self.rust_binding_at_offset(file, offset, name)
    }

    /// The scope answer for a bare name a binding owns: a `use` path's own
    /// item (variants included), the nested `fn` itself, or nothing when a
    /// parameter or `let` is what the name means. None leaves the older
    /// visible-use, glob and prelude rules in charge.
    pub(super) fn rust_binding_visible(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<bool> {
        let bound = self.rust_binding_at(r)?;
        match bound {
            RustBound::Local | RustBound::Path { .. } => Some(false),
            RustBound::FnItem(line) => {
                Some(
                    n.file_path == r.file_path
                        && matches!(n.kind.as_str(), "function" | "method")
                        && n.start_line == line,
                )
            }
            RustBound::Use { path, use_line, use_col } => {
                let variant = Self::rust_use_path_is_variant(&path);
                let items =
                    self.rust_use_path_items(&path, &r.file_path, use_line, use_col).ok()?;
                match items {
                    Some(items) => Some(items.iter().any(|i| i.id == n.id)),
                    // A variant path that names no variant is terminal: no
                    // namesake elsewhere stands in for it.
                    None if variant => Some(false),
                    None => None,
                }
            }
        }
    }
}



