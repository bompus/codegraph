//! Rust module and item lookup, including inline modules and re-exported uses.
use super::super::iteration::descendant_for_position;
use super::super::rust_modules::rust_self_module_dir;
use super::super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in super::super) struct RustModule {
    pub(in super::super) file: String,
    pub(in super::super) inline: Vec<String>,
}

pub(in super::super) fn rust_children(n: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    (0..n.named_child_count())
        .filter_map(|i| n.named_child(i))
        .collect()
}
pub(in super::super) fn rust_text<'a>(n: tree_sitter::Node<'_>, text: &'a str) -> &'a str {
    &text[n.byte_range()]
}
pub(in super::super) fn rust_ancestor<'a>(
    mut n: tree_sitter::Node<'a>,
    kinds: &[&str],
) -> Option<tree_sitter::Node<'a>> {
    loop {
        if kinds.contains(&n.kind()) {
            return Some(n);
        }
        n = n.parent()?;
    }
}
pub(in super::super) fn rust_at<'a>(
    tree: &'a tree_sitter::Tree,
    text: &str,
    line: i64,
    col: i64,
) -> tree_sitter::Node<'a> {
    descendant_for_position(
        tree.root_node(),
        text,
        ((line - 1).max(0) as usize, col.max(0) as usize),
    )
}
pub(in super::super) fn rust_inline_at(mut n: tree_sitter::Node<'_>, text: &str) -> Vec<String> {
    let mut names = Vec::new();
    loop {
        if n.kind() == "mod_item" && n.child_by_field_name("body").is_some() {
            if let Some(name) = n.child_by_field_name("name") {
                names.push(rust_text(name, text).to_string());
            }
        }
        let Some(p) = n.parent() else { break };
        n = p;
    }
    names.reverse();
    names
}

/// Flatten one grammar use tree, preserving full paths and local aliases.
fn rust_use_leaves(
    n: tree_sitter::Node<'_>,
    text: &str,
    prefix: &[String],
    out: &mut Vec<(String, Vec<String>)>,
) {
    let join = |s: &str| {
        let mut p = prefix.to_vec();
        p.extend(s.split("::").filter(|s| !s.is_empty()).map(str::to_string));
        p
    };
    match n.kind() {
        "use_declaration" => {
            for c in rust_children(n) {
                if c.kind() != "visibility_modifier" {
                    rust_use_leaves(c, text, prefix, out);
                }
            }
        }
        "scoped_use_list" => {
            let p = n
                .child_by_field_name("path")
                .map(|p| join(rust_text(p, text)))
                .unwrap_or(prefix.to_vec());
            if let Some(list) = n.child_by_field_name("list") {
                rust_use_leaves(list, text, &p, out);
            }
        }
        "use_list" => {
            for c in rust_children(n) {
                rust_use_leaves(c, text, prefix, out);
            }
        }
        "use_as_clause" => {
            if let (Some(path), Some(alias)) = (
                n.child_by_field_name("path"),
                n.child_by_field_name("alias"),
            ) {
                out.push((
                    rust_text(alias, text).to_string(),
                    join(rust_text(path, text)),
                ));
            }
        }
        "use_wildcard" => {
            let p = rust_children(n)
                .first()
                .map(|p| join(rust_text(*p, text)))
                .unwrap_or(prefix.to_vec());
            out.push(("*".into(), p));
        }
        "identifier" | "scoped_identifier" | "self" | "super" | "crate" => {
            let mut p = join(rust_text(n, text));
            if p.last().is_some_and(|s| s == "self") {
                p.pop();
            }
            if let Some(last) = p.last() {
                out.push((last.clone(), p));
            }
        }
        _ => {}
    }
}

impl KernelResolver {
    pub(in super::super) fn rust_tree_for(
        &mut self,
        file: &str,
    ) -> Option<(Rc<SourceFile>, Rc<tree_sitter::Tree>)> {
        let source = self.read_file(file)?;
        let cache_key = format!("rust-written\0{file}");
        if let Some((cached, Some(tree))) = self.tree_cache.get(&cache_key) {
            if Rc::ptr_eq(cached, &source) {
                return Some((source, tree.clone()));
            }
        }
        let r = ResolveRefIn {
            file_path: file.into(),
            language: "rust".into(),
            row_id: None,
            from_node_id: String::new(),
            reference_name: String::new(),
            reference_kind: String::new(),
            line: 1,
            column: 0,
            candidates: None,
            failure_reason: None, generated: None,
        };
        let mut tree = self.parsed_tree(&source, &r)?;
        // pin-project exposes its struct declaration inside a macro token tree.
        // Blank only the macro wrapper, keeping every source byte offset.
        let mut stack = vec![tree.root_node()];
        let mut wrappers = Vec::new();
        while let Some(node) = stack.pop() {
            if node.kind() == "macro_invocation"
                && node
                    .child_by_field_name("macro")
                    .is_some_and(|m| rust_text(m, source.text()).ends_with("pin_project"))
            {
                if let Some(body) = rust_children(node)
                    .into_iter()
                    .find(|n| n.kind() == "token_tree")
                {
                    wrappers.push((
                        node.start_byte(),
                        body.start_byte() + 1,
                        body.end_byte() - 1,
                    ));
                }
            } else if !matches!(node.kind(), "function_item" | "function_signature_item") {
                stack.extend(rust_children(node));
            }
        }
        if !wrappers.is_empty() {
            let mut bytes = source.text().as_bytes().to_vec();
            for (start, end, close) in wrappers {
                for b in &mut bytes[start..end] {
                    if *b != b'\n' && *b != b'\r' {
                        *b = b' ';
                    }
                }
                bytes[close] = b' ';
            }
            if let Ok(parsed) =
                crate::tree::parse_with_cached_parser(std::str::from_utf8(&bytes).ok()?, "rust")
            {
                tree = Rc::new(parsed);
                self.tree_cache.put(
                    format!("rust\0{file}"),
                    (source.clone(), Some(tree.clone())),
                );
            }
        }
        self.tree_cache.put(cache_key, (source.clone(), Some(tree.clone())));
        Some((source, tree))
    }
    pub(in super::super) fn rust_module_at(
        &mut self,
        file: &str,
        line: i64,
        col: i64,
    ) -> RustModule {
        let inline = self
            .rust_tree_for(file)
            .map(|(s, t)| rust_inline_at(rust_at(&t, s.text(), line, col), s.text()))
            .unwrap_or_default();
        RustModule {
            file: file.into(),
            inline,
        }
    }
    fn rust_module_body<'a>(
        tree: &'a tree_sitter::Tree,
        text: &str,
        inline: &[String],
    ) -> Option<tree_sitter::Node<'a>> {
        let mut body = tree.root_node();
        for seg in inline {
            body = rust_children(body)
                .into_iter()
                .find(|n| {
                    n.kind() == "mod_item"
                        && n.child_by_field_name("name")
                            .is_some_and(|x| rust_text(x, text) == seg)
                })?
                .child_by_field_name("body")?;
        }
        Some(body)
    }
    pub(in super::super) fn rust_visible_uses(
        &mut self,
        file: &str,
        at: usize,
    ) -> Vec<(String, Vec<String>, RustModule)> {
        let Some((source, tree)) = self.rust_tree_for(file) else {
            return Vec::new();
        };
        let text = source.text();
        let mut n = tree
            .root_node()
            .named_descendant_for_byte_range(at.min(text.len()), at.min(text.len()))
            .unwrap_or(tree.root_node());
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        loop {
            for child in rust_children(n) {
                if child.kind() != "use_declaration" {
                    continue;
                }
                let mut leaves = Vec::new();
                rust_use_leaves(child, text, &[], &mut leaves);
                for (name, path) in leaves {
                    if name == "*" || seen.insert(name.clone()) {
                        out.push((
                            name,
                            path,
                            RustModule {
                                file: file.into(),
                                inline: rust_inline_at(child, text),
                            },
                        ));
                    }
                }
            }
            let Some(parent) = n.parent() else { break };
            n = parent;
        }
        out
    }
    pub(in super::super) fn rust_root_module(&mut self, file: &str) -> Option<RustModule> {
        let dir = self.rust_crate_root_dir(file).ok()??;
        for name in ["lib.rs", "main.rs"] {
            let p = pos_join(&dir, name);
            if self.file_exists(&p) {
                return Some(RustModule {
                    file: p,
                    inline: Vec::new(),
                });
            }
        }
        None
    }
    fn rust_submodule(&mut self, m: &RustModule, name: &str) -> Option<RustModule> {
        if let Some((source, tree)) = self.rust_tree_for(&m.file) {
            let body = Self::rust_module_body(&tree, source.text(), &m.inline)?;
            if rust_children(body).iter().any(|n| {
                n.kind() == "mod_item"
                    && n.child_by_field_name("name")
                        .is_some_and(|x| rust_text(x, source.text()) == name)
                    && n.child_by_field_name("body").is_some()
            }) {
                let mut sub = m.clone();
                sub.inline.push(name.into());
                return Some(sub);
            }
        }
        let mut dir = rust_self_module_dir(&m.file);
        for seg in &m.inline {
            dir = pos_join(&dir, seg);
        }
        for p in [
            pos_join(&dir, &format!("{name}.rs")),
            pos_join(&dir, &format!("{name}/mod.rs")),
        ] {
            if self.file_exists(&p) {
                return Some(RustModule {
                    file: p,
                    inline: Vec::new(),
                });
            }
        }
        None
    }
    fn rust_super_module(&mut self, m: &RustModule) -> Option<RustModule> {
        let mut m = m.clone();
        if m.inline.pop().is_some() {
            return Some(m);
        }
        let dir = pos_dirname(&rust_self_module_dir(&m.file)).to_string();
        for p in [
            pos_join(&dir, "mod.rs"),
            pos_join(&dir, "lib.rs"),
            pos_join(&dir, "main.rs"),
            format!("{dir}.rs"),
        ] {
            if self.file_exists(&p) {
                return Some(RustModule {
                    file: p,
                    inline: Vec::new(),
                });
            }
        }
        None
    }
    pub(in super::super) fn rust_walk_module(
        &mut self,
        path: &[String],
        from: &RustModule,
        depth: usize,
    ) -> Option<RustModule> {
        if depth > 24 {
            return None;
        }
        let mut m = from.clone();
        let mut i = 0;
        if let Some(first) = path.first() {
            match first.as_str() {
                "crate" => {
                    m = self.rust_root_module(&from.file)?;
                    i = 1;
                }
                "self" => i = 1,
                "super" => {
                    while path.get(i).is_some_and(|s| s == "super") {
                        m = self.rust_super_module(&m)?;
                        i += 1;
                    }
                }
                "std" | "core" | "alloc" => return None,
                _ => {
                    // Resolve a project crate through its manifest, never an unrelated namesake module.
                    let graph = self.rust_crate_graph().ok()?;
                    let own = graph
                        .crates
                        .iter()
                        .filter(|(dir, _)| {
                            from.file.starts_with(&format!("{dir}/")) || dir.is_empty()
                        })
                        .max_by_key(|(dir, _)| dir.len());
                    let target = own.and_then(|(_, c)| {
                        if c.name == *first {
                            own.map(|(d, _)| d.clone())
                        } else {
                            c.keys.get(first).cloned()
                        }
                    });
                    if let Some(dir) = target {
                        let p = pos_join(&dir, "src/lib.rs");
                        if self.file_exists(&p) {
                            m = RustModule {
                                file: p,
                                inline: Vec::new(),
                            };
                            i = 1;
                        }
                    }
                }
            }
        }
        while let Some(seg) = path.get(i) {
            if let Some(sub) = self.rust_submodule(&m, seg) {
                m = sub;
                i += 1;
                continue;
            }
            let (source, tree) = self.rust_tree_for(&m.file)?;
            let body = Self::rust_module_body(&tree, source.text(), &m.inline)?;
            let mut alias = None;
            for child in rust_children(body) {
                if child.kind() == "use_declaration" {
                    let mut leaves = Vec::new();
                    rust_use_leaves(child, source.text(), &[], &mut leaves);
                    if let Some((_, p)) = leaves.into_iter().find(|(n, _)| n == seg) {
                        alias = Some(p);
                        break;
                    }
                }
            }
            if let Some(mut p) = alias {
                p.extend_from_slice(&path[i + 1..]);
                return self.rust_walk_module(&p, &m, depth + 1);
            }
            // Bare module names in Rust 2015 use paths may be crate-relative.
            if i == 0 {
                let root = self.rust_root_module(&from.file)?;
                if root != m {
                    return self.rust_walk_module(path, &root, depth + 1);
                }
            }
            return None;
        }
        Some(m)
    }
    pub(in super::super) fn rust_items_in_module(
        &mut self,
        m: &RustModule,
        name: &str,
        public_only: bool,
        seen: &mut HashSet<String>,
    ) -> Res<Vec<Arc<KNode>>> {
        let key = format!("{}:{:?}:{name}:{public_only}", m.file, m.inline);
        if seen.len() > 64 || !seen.insert(key) {
            return Ok(Vec::new());
        }
        let mut own = Vec::new();
        let nodes = self.nodes_in_file(&m.file)?;
        for node in nodes.iter().filter(|n| {
            n.language == "rust"
                && n.name == name
                && matches!(
                    n.kind.as_str(),
                    "function"
                        | "struct"
                        | "enum"
                        | "union"
                        | "trait"
                        | "type_alias"
                        | "variable"
                        | "constant"
                )
        }) {
            if node.qualified_name.ends_with(&format!("::{}", node.name)) {
                continue;
            }
            if public_only && node.visibility.as_deref() == Some("private") {
                continue;
            }
            if self
                .rust_module_at(&node.file_path, node.start_line, node.start_column)
                .inline
                == m.inline
            {
                own.push(node.clone());
            }
        }
        if !own.is_empty() {
            return Ok(own);
        }
        let Some((source, tree)) = self.rust_tree_for(&m.file) else {
            return Ok(own);
        };
        let Some(body) = Self::rust_module_body(&tree, source.text(), &m.inline) else {
            return Ok(own);
        };
        // Macro token trees contain module declarations and re-exports too.
        let mut stack = rust_children(body);
        while let Some(n) = stack.pop() {
            if n.kind() == "use_declaration" {
                if public_only && !rust_text(n, source.text()).trim_start().starts_with("pub") {
                    continue;
                }
                let mut leaves = Vec::new();
                rust_use_leaves(n, source.text(), &[], &mut leaves);
                for (local, p) in leaves {
                    if local == name {
                        if let Some(items) = self.rust_path_items(&p, m, false, seen)? {
                            own.extend(items);
                        }
                    } else if local == "*" {
                        if let Some(sub) = self.rust_walk_module(&p, m, 0) {
                            own.extend(self.rust_items_in_module(&sub, name, true, seen)?);
                        }
                    }
                }
            } else if matches!(n.kind(), "macro_invocation" | "token_tree") {
                stack.extend(rust_children(n));
            }
        }
        // Tree-sitter leaves a macro's use statements inside a token_tree; parse
        // that token text separately while retaining the same module identity.
        if own.is_empty() {
            let text = rust_text(body, source.text());
            for captures in re!(r"(?m)\b(pub(?:\([^)]*\))?\s+)?use\s+([^;]+);").captures_iter(text)
            {
                if public_only && captures.get(1).is_none() {
                    continue;
                }
                let snippet = format!("use {};", &captures[2]);
                if let Ok(t) = crate::tree::parse_with_cached_parser(&snippet, "rust") {
                    let mut leaves = Vec::new();
                    for n in rust_children(t.root_node()) {
                        rust_use_leaves(n, &snippet, &[], &mut leaves);
                    }
                    for (local, p) in leaves {
                        if local == name {
                            if let Some(items) = self.rust_path_items(&p, m, false, seen)? {
                                own.extend(items);
                            }
                        }
                    }
                }
            }
        }
        own.sort_by(|a, b| a.id.cmp(&b.id));
        own.dedup_by(|a, b| a.id == b.id);
        Ok(own)
    }
    pub(in super::super) fn rust_path_items(
        &mut self,
        path: &[String],
        from: &RustModule,
        public_only: bool,
        seen: &mut HashSet<String>,
    ) -> Res<Option<Vec<Arc<KNode>>>> {
        let Some((leaf, prefix)) = path.split_last() else {
            return Ok(None);
        };
        let Some(m) = self.rust_walk_module(prefix, from, 0) else {
            return Ok(None);
        };
        if self
            .read_file(&m.file)
            .is_some_and(|s| re!(r"#\s*\[\s*path\s*=").is_match(s.text()))
        {
            return Ok(None);
        }
        self.rust_items_in_module(&m, leaf, public_only, seen)
            .map(Some)
    }

    /// An authoritative scope answer for written paths, associated items and imports.
    pub(in super::super) fn rust_precise_name_scope(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<bool> {
        if r.language != "rust" {
            return None;
        }
        let (source, tree) = self.rust_tree_for(&r.file_path)?;
        let text = source.text();
        let at = rust_at(&tree, text, r.line, r.column);
        let offset = at.start_byte();
        let name = r
            .reference_name
            .rsplit("::")
            .next()
            .unwrap_or(&r.reference_name);
        let associated = matches!(n.kind.as_str(), "type_alias" | "variable" | "constant")
            && n.qualified_name.ends_with(&format!("::{}", n.name));
        let line = source.get((r.line - 1).max(0) as usize)?;
        let col =
            super::super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len());
        if r.reference_kind == "references"
            && line[col..].starts_with(name)
            && re!(r"^\s*(?:=|:[^:])").is_match(&line[col + name.len()..])
            && rust_ancestor(at, &["type_binding", "type_arguments"]).is_some()
        {
            return Some(false);
        }
        if r.reference_kind == "references" && matches!(n.kind.as_str(), "enum" | "struct" | "union" | "class")
            && line[col..].starts_with(&format!("{name}::")) {
            return None;
        }
        let mut path = None;
        // Extraction often emits only a type path's leaf. Read its actual grammar ancestor.
        let mut cursor = at;
        loop {
            if matches!(
                cursor.kind(),
                "scoped_type_identifier" | "scoped_identifier"
            ) {
                let raw = rust_text(cursor, text);
                if raw.ends_with(name) || r.reference_name.contains("::") {
                    path = Some(raw.to_string());
                }
            }
            let Some(p) = cursor.parent() else { break };
            if !matches!(
                p.kind(),
                "scoped_type_identifier"
                    | "scoped_identifier"
                    | "generic_type"
                    | "qualified_type"
                    | "bracketed_type"
            ) {
                break;
            }
            cursor = p;
        }
        if path.is_none() && !line[col..].starts_with(name) {
            let pat = Self::cached_regex(&format!(
                r"((?:[A-Za-z_]\w*\s*::\s*)+){}\b",
                regex::escape(name)
            ))
            .ok()?;
            if let Some(c) = pat.captures(&line[col..]) {
                path = Some(format!("{}{}", &c[1], name));
            }
        }
        if path.is_none() && r.reference_name.contains("::") {
            path = Some(r.reference_name.clone());
        }
        if let Some(path) = path {
            let path = re!(r"\s+").replace_all(&path, "").into_owned();
            if path.contains('<') && path.contains("as") {
                let cap = re!(r"<([A-Za-z_]\w*)as(?:(?:\w+)::)*([A-Za-z_]\w*)>::(\w+)$")
                    .captures(&path)?;
                let owner = &cap[1];
                let tr = &cap[2];
                if !associated {
                    return Some(false);
                }
                if owner == "Self" {
                    return Some(self.rust_same_impl(n, r));
                }
                let nr = r.clone().at(n);
                let cx = self.rust_self_context(&nr)?;
                return Some(
                    n.qualified_name == format!("{owner}::{}", n.name)
                        && cx.trait_name.as_deref() == Some(tr),
                );
            }
            let segs: Vec<String> = path
                .split("::")
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let root = segs.first()?.as_str();
            if root == "Self" && !associated {
                return None;
            }
            if root == "Self" && associated {
                return Some(self.rust_same_impl(n, r));
            }
            if associated {
                return Some(false);
            }
            let facts = self.rust_use_facts(&r.file_path)?;
            if matches!(root, "std" | "core" | "alloc")
                || (facts.external.contains(root) && !facts.bound.contains(root))
            {
                return Some(false);
            }
            if self
                .rust_generic_traits(root, &r.file_path, offset)
                .is_some()
            {
                return Some(false);
            }
            let from = self.rust_module_at(&r.file_path, r.line, r.column);
            let uses = self.rust_visible_uses(&r.file_path, offset);
            let mut expanded = segs.clone();
            let mut base = from;
            if let Some((_, p, m)) = uses.iter().find(|(local, _, _)| local == root) {
                expanded = p.clone();
                expanded.extend_from_slice(&segs[1..]);
                base = m.clone();
            }
            if expanded
                .first()
                .is_some_and(|s| matches!(s.as_str(), "std" | "core" | "alloc"))
            {
                return Some(false);
            }
            if let Some(items) = self
                .rust_path_items(&expanded, &base, false, &mut HashSet::new())
                .ok()?
            {
                if !items.is_empty() {
                    return Some(items.iter().any(|i| i.id == n.id));
                }
            }
            let parent = segs.get(segs.len().saturating_sub(2))?;
            if n.file_path == r.file_path {
                let module = self.rust_module_at(&n.file_path, n.start_line, n.start_column);
                return Some(
                    module.inline.last() == Some(parent)
                        || n.qualified_name.split("::").any(|s| s == parent)
                        || matches!(root, "crate" | "self" | "super"),
                );
            }
            return None;
        }
        if associated {
            return Some(false);
        }
        if matches!(
            n.kind.as_str(),
            "method" | "field" | "property" | "enum_member" | "file" | "module" | "namespace"
        ) {
            return None;
        }
        for (local, p, m) in self.rust_visible_uses(&r.file_path, offset) {
            if local == name {
                if let Some(items) = self
                    .rust_path_items(&p, &m, false, &mut HashSet::new())
                    .ok()?
                {
                    return Some(items.iter().any(|i| i.id == n.id));
                }
            }
        }
        if n.file_path != r.file_path {
            let uses = self.rust_visible_uses(&r.file_path, offset);
            let mut seen_glob = false;
            let mut ids = HashSet::new();
            for (local, p, m) in uses {
                if local != "*" {
                    continue;
                }
                if let Some(sub) = self.rust_walk_module(&p, &m, 0) {
                    seen_glob = true;
                    for item in self
                        .rust_items_in_module(&sub, name, false, &mut HashSet::new())
                        .ok()?
                    {
                        ids.insert(item.id.clone());
                    }
                }
            }
            if seen_glob {
                return Some(ids.contains(&n.id));
            }
        }
        None
    }
    fn rust_same_impl(&mut self, n: &KNode, r: &ResolveRefIn) -> bool {
        if n.file_path != r.file_path {
            return false;
        }
        let Some((s, t)) = self.rust_tree_for(&r.file_path) else {
            return false;
        };
        let enclosing = |line, col| {
            rust_ancestor(
                rust_at(&t, s.text(), line, col),
                &["impl_item", "trait_item"],
            )
            .map(|n| n.id())
        };
        enclosing(n.start_line, n.start_column)
            .is_some_and(|id| Some(id) == enclosing(r.line, r.column))
    }
}
