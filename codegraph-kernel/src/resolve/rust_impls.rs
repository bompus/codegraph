//! Rust associated calls and foreign-type trait implementations (#2697, #2700).
use super::*;
use super::iteration::descendant_for_position;
use super::rust_modules::rust_paths::rust_at;
use self::rust_types::Written;

const STD_TYPES: &[&str] = &[
    "String", "str", "Vec", "VecDeque", "HashMap", "HashSet", "BTreeMap", "BTreeSet",
    "BinaryHeap", "LinkedList", "Box", "Rc", "Arc", "Weak", "Cow", "Cell", "RefCell",
    "Mutex", "RwLock", "Option", "Result", "PathBuf", "Path", "OsString", "OsStr",
    "CString", "CStr", "NonNull", "Pin", "Duration", "Instant", "SystemTime", "PhantomData",
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128",
    "isize", "f32", "f64", "bool", "char",
];

pub(super) struct SelfContext {
    owner: String,
    root: Option<String>,
    pub(super) trait_name: Option<String>,
    trait_args: Vec<String>,
    is_trait: bool,
    /// The impl header's `type` field as written (`serde_json::Value`, `&'a mut T`).
    self_text: String,
}

fn type_name(node: tree_sitter::Node, text: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "primitive_type" => Some(text[node.byte_range()].to_string()),
        "generic_type" | "reference_type" => type_name(node.child_by_field_name("type")?, text),
        "scoped_type_identifier" | "scoped_identifier" => type_name(node.child_by_field_name("name")?, text),
        _ => None,
    }
}

impl KernelResolver {
    /// Read the enclosing grammar node, including multiline impl headers and generic arguments.
    pub(super) fn rust_self_context(&mut self, r: &ResolveRefIn) -> Option<SelfContext> {
        let source = self.read_file(&r.file_path)?;
        let tree = self.parsed_tree(&source, r)?;
        let text = source.text();
        let mut at = descendant_for_position(tree.root_node(), text,
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        loop {
            if at.kind() == "trait_item" {
                return Some(SelfContext { owner: type_name(at.child_by_field_name("name")?, text)?,
                    root: None, trait_name: None, trait_args: Vec::new(), is_trait: true, self_text: String::new() });
            }
            if at.kind() == "impl_item" {
                let ty = at.child_by_field_name("type")?;
                let written = text[ty.byte_range()].trim();
                let root = re!(r"^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?)?([A-Za-z_]\w*)\s*::")
                    .captures(written).map(|m| m[1].to_string());
                let tr = at.child_by_field_name("trait");
                let trait_name = tr.and_then(|n| type_name(n, text));
                let trait_args = tr.and_then(|n| n.child_by_field_name("type_arguments"))
                    .map(|n| {
                        let args = re!(r"'\w+").replace_all(&text[n.byte_range()], "");
                        re!(r"[A-Za-z_]\w*").find_iter(&args).map(|m| m.as_str().to_string())
                            .filter(|s| s != "mut" && s != "dyn").collect()
                    }).unwrap_or_default();
                return Some(SelfContext { owner: type_name(ty, text)?, root, trait_name, trait_args,
                    is_trait: false, self_text: written.to_string() });
            }
            at = at.parent()?;
        }
    }

    fn rust_project_type(&mut self, name: &str, trait_only: bool) -> Res<bool> {
        Ok(self.nodes_by_name(name)?.iter().any(|n| n.language == "rust"
            && if trait_only { n.kind == "trait" } else {
                matches!(n.kind.as_str(), "struct" | "enum" | "union" | "type_alias" | "trait")
            }))
    }

    fn rust_trait_in_scope(&mut self, name: &str, r: &ResolveRefIn, target: &KNode) -> Res<bool> {
        if r.file_path == target.file_path { return Ok(true); }
        let traits: Vec<_> = self.nodes_by_name(name)?.iter()
            .filter(|n| n.language == "rust" && n.kind == "trait").cloned().collect();
        if traits.iter().any(|n| n.file_path == r.file_path) { return Ok(true); }
        let Some(source) = self.read_file(&r.file_path) else { return Ok(false) };
        let uses = source.rust_scope_uses();
        if uses.names.contains(name) { return Ok(true); }
        self.rust_use_facts(&r.file_path);
        Ok(uses.globs.iter().any(|g| g == "prelude" || g == "crate"
            || self.upstream_scope_memo.contains_key(&format!("rust-crate:{g}")))
            || traits.iter().any(|n| super::name_scope::rust_glob_covers(uses, n, r)))
    }

    fn rust_first_arg_is(&mut self, ty: &str, r: &ResolveRefIn, name: &str) -> bool {
        let Some(args) = self.call_arguments(r, name) else { return true };
        let Some(arg) = args.first() else { return false };
        let arg = re!(r"^(?:&\s*(?:mut\s+)?|\*)+").replace(arg.trim(), "");
        let arg = arg.strip_suffix(".clone()").unwrap_or(&arg);
        let typed = Self::cached_regex(&format!(r"^(?:[A-Za-z_]\w*::)*{}\b", regex::escape(ty)));
        if typed.is_ok_and(|pat| pat.is_match(arg)) { return true; }
        if !re!(r"^[A-Za-z_]\w*$").is_match(arg) { return false; }
        if arg == "self" { return self.rust_self_context(r).is_some_and(|cx| cx.owner == ty); }
        let Some(source) = self.read_file(&r.file_path) else { return false };
        let Ok(declared) = Self::cached_regex(&format!(
            r"\b{}\s*:\s*&?\s*(?:'\w+\s+)?(?:mut\s+)?(?:[A-Za-z_]\w*::)*{}\b", regex::escape(arg), regex::escape(ty))) else { return false };
        let Ok(built) = Self::cached_regex(&format!(
            r"\blet\s+(?:mut\s+)?{}\s*=\s*&?\s*(?:[A-Za-z_]\w*::)*{}\b", regex::escape(arg), regex::escape(ty))) else { return false };
        for line in source.iter().take(r.line.max(0) as usize).rev() {
            if declared.is_match(line) || built.is_match(line) { return true; }
            if re!(r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s"#).is_match(line) { break; }
        }
        false
    }

    pub(super) fn rust_foreign_impl_callable(&mut self, target: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if target.language != "rust" || target.kind != "method" { return Ok(true); }
        let owner = target.qualified_name.rsplit_once("::").map(|(o, _)| o.rsplit("::").next().unwrap_or("")).unwrap_or("");
        if !STD_TYPES.contains(&owner) || self.rust_project_type(owner, false)? { return Ok(true); }
        let mut site = r.clone().at(target);
        site.column = target.start_column;
        let Some(cx) = self.rust_self_context(&site) else { return Ok(true) };
        let Some(trait_name) = cx.trait_name else { return Ok(true) };
        if cx.owner != owner { return Ok(true); }
        if self.rust_project_type(&trait_name, true)? { return self.rust_trait_in_scope(&trait_name, r, target); }
        for ty in cx.trait_args {
            if self.rust_project_type(&ty, false)? { return Ok(self.rust_first_arg_is(&ty, r, &target.name)); }
        }
        Ok(true)
    }

    /// The `use` path a local name binds at `at` (`use serde_json::Value as J`
    /// binds `J` to `serde_json::Value`), innermost binding first.
    fn rust_use_path_at(&mut self, name: &str, file: &str, at: usize) -> Option<Vec<String>> {
        self.rust_visible_uses(file, at)
            .into_iter()
            .find(|(local, _, _)| local == name)
            .map(|(_, p, _)| p)
    }

    /// The crate dir a file belongs to: the longest crate dir prefixing it, or
    /// the root crate's "".
    pub(super) fn rust_crate_dir_of(file: &str, graph: &super::rust_crates::RustCrateGraph) -> Option<String> {
        let mut best = graph.crates.contains_key("").then(String::new);
        for dir in graph.crates.keys() {
            if !dir.is_empty()
                && file.starts_with(&format!("{dir}/"))
                && best.as_ref().is_none_or(|b| dir.len() > b.len())
            {
                best = Some(dir.clone());
            }
        }
        best
    }

    /// Whether a Rust path's first segment names std, or a crate the manifests
    /// depend on that the project doesn't hold. A path dependency the project
    /// holds, a workspace member or not, is the project's.
    fn is_rust_outside_crate(&mut self, root: &str, r: &ResolveRefIn) -> Res<bool> {
        if matches!(root, "std" | "core" | "alloc" | "proc_macro") {
            return Ok(true);
        }
        if matches!(root, "crate" | "self" | "super") {
            return Ok(false);
        }
        let graph = self.rust_crate_graph()?;
        if graph.crates.values().any(|c| c.name == root) {
            return Ok(false);
        }
        let _ = self.rust_use_facts(&r.file_path);
        let Some(deps) = &self.upstream_rust_deps else {
            return Ok(false);
        };
        if !deps.contains(root) {
            return Ok(false);
        }
        if let Some(own) = Self::rust_crate_dir_of(&r.file_path, &graph) {
            if graph.crates.get(&own).is_some_and(|c| c.keys.contains_key(root)) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The name of the type a Rust path `path::type` names from std or a
    /// dependency, where the file's `use`s or the path itself say it is from
    /// there — `J` under `use serde_json::Value as J` is `Value` — or None:
    /// one of the project's, or one nothing written says is from outside it.
    fn rust_outside_path_type(
        &mut self,
        ty: &str,
        path: &[&str],
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let at = self
            .rust_tree_for(&r.file_path)
            .map(|(s, t)| rust_at(&t, s.text(), r.line, r.column).start_byte())
            .unwrap_or(0);
        let (root, real);
        if path.is_empty() {
            let Some(bound) = self.rust_use_path_at(ty, &r.file_path, at) else {
                return Ok(None);
            };
            if bound.len() < 2 {
                return Ok(None);
            }
            root = bound[0].clone();
            real = bound.last().unwrap().clone();
        } else {
            root = self
                .rust_use_path_at(path[0], &r.file_path, at)
                .and_then(|p| p.first().cloned())
                .unwrap_or_else(|| path[0].to_string());
            real = ty.to_string();
        }
        Ok(self.is_rust_outside_crate(&root, r)?.then_some(real))
    }

    /// `(segments, name)` of a written Rust path (`serde_json::Value` →
    /// `(["serde_json"], "Value")`), trailing generic arguments dropped.
    fn rust_path_head(text: &str) -> Option<(Vec<String>, String)> {
        let t = text.trim().trim_start_matches("::").trim();
        let t = if let Some(open) = t.find('<') {
            let mut depth = 0usize;
            let mut end = None;
            for (i, c) in t[open..].char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            end = Some(open + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end != Some(t.len() - 1) {
                return None;
            }
            t[..open].trim().trim_end_matches("::").trim()
        } else {
            t
        };
        let parts: Vec<String> = t.split("::").map(|s| s.trim().to_string()).collect();
        if !parts.iter().all(|p| re!(r"^[A-Za-z_]\w*$").is_match(p)) {
            return None;
        }
        let name = parts.last().cloned()?;
        Some((parts[..parts.len() - 1].to_vec(), name))
    }

    /// Whether the type a `Written` names is std's or a dependency's, as the
    /// written path or the file's `use`s say — the `outside` arm of
    /// `rustReadType` (#2614): a `std::fs::File` parameter's, a
    /// `serde_json::Value` the `use` binds, or a `File` leaf the file pulls
    /// from outside and nothing inside.
    fn rust_written_outside(
        &mut self,
        w: &Written,
        leaf: &str,
        head: &str,
    ) -> Res<bool> {
        let mut segs: Vec<String> = head.split("::").map(|s| s.trim().to_string()).collect();
        let single = segs.len() == 1;
        let mut has_root = !single;
        if !single {
            if let Some(bound) = self.rust_use_path_at(&segs[0], &w.file, w.at) {
                if !bound.is_empty() {
                    segs.splice(0..1, bound);
                }
            }
        } else if let Some(bound) = self.rust_use_path_at(leaf, &w.file, w.at) {
            if bound.len() > 1 {
                segs = bound[..bound.len() - 1].to_vec();
                has_root = true;
            }
        }
        if has_root {
            if let Some(root) = segs.first().map(String::as_str) {
                if matches!(root, "std" | "core" | "alloc") {
                    return Ok(true);
                }
                if !matches!(root, "crate" | "self" | "super")
                    && !self
                        .rust_crate_graph()?
                        .crates
                        .values()
                        .any(|c| c.name == root)
                {
                    let _ = self.rust_use_facts(&w.file);
                    if self
                        .upstream_rust_deps
                        .as_ref()
                        .is_some_and(|d| d.contains(root))
                    {
                        return Ok(true);
                    }
                }
            }
        }
        if single {
            let facts = self.rust_use_facts(&w.file).unwrap_or_default();
            if facts.external.contains(leaf) && !facts.bound.contains(leaf) {
                return Ok(true);
            }
            if STD_TYPES.contains(&leaf) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a Rust call through a type's path can mean `target` (#2614). A
    /// type the path names from std or a dependency — through the file's `use`
    /// (`J::from(i)` under `use serde_json::Value as J`, `Value::default()`
    /// under `use serde_json::Value`) or as written
    /// (`serde_json::Value::default()`) — is that crate's own: the project
    /// holds a function of it only as its own impl of a trait for it (`impl
    /// Blank for serde_json::Value`, `impl From<Fail> for PathBuf`), and a call
    /// through a trait's path (`W::flush(s)` under `use std::io::Write as W`)
    /// reaches the project's impls of that trait. A same-named function of a
    /// project type is another type's: the reporter's `J::from(i)` went to a
    /// local `impl From<io::Error> for Fail`, a guess by the name alone, and
    /// `Value::default()` to a project `Value` of its own.
    pub(super) fn rust_path_call_reachable(&mut self, target: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if target.language != "rust" {
            return Ok(true);
        }
        let segments: Vec<&str> = r.reference_name.split("::").collect();
        if segments.len() < 2
            || !segments.iter().all(|s| re!(r"^[A-Za-z_]\w*$").is_match(s))
        {
            return Ok(true);
        }
        let ty = segments[segments.len() - 2];
        if !ty.starts_with(|c: char| c.is_ascii_uppercase()) || ty == "Self" {
            return Ok(true);
        }
        let Some(outside) =
            self.rust_outside_path_type(ty, &segments[..segments.len() - 2], r)?
        else {
            return Ok(true);
        };
        if target.kind != "method" {
            return Ok(false);
        }
        let mut site = r.clone().at(target);
        site.column = target.start_column;
        let Some(cx) = self.rust_self_context(&site) else {
            return Ok(false);
        };
        if cx.is_trait {
            return Ok(false);
        }
        if cx.trait_name.as_deref() == Some(outside.as_str()) {
            return Ok(true);
        }
        if cx.owner != outside {
            return Ok(false);
        }
        // The type the impl is for, as its own file names it: the outside one,
        // not a project namesake.
        let text = re!(r"^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?)+")
            .replace(&cx.self_text, "")
            .to_string();
        let Some((written, name)) = Self::rust_path_head(&text) else {
            return Ok(false);
        };
        let at = self
            .rust_impl_start(&target.file_path, target.start_line, target.start_column)
            .unwrap_or(0);
        let mut segs = written.clone();
        let mut real = name.clone();
        if segs.is_empty() {
            if let Some(bound) = self.rust_use_path_at(&name, &target.file_path, at) {
                if bound.len() > 1 {
                    real = bound.last().cloned().unwrap_or(real);
                    segs = bound[..bound.len() - 1].to_vec();
                }
            }
        } else if let Some(head) = self.rust_use_path_at(&segs[0], &target.file_path, at) {
            if !head.is_empty() {
                segs.splice(0..1, head);
            }
        }
        if let Some(root) = segs.first().map(String::as_str) {
            if matches!(root, "std" | "core" | "alloc") {
                return Ok(true);
            }
            if !matches!(root, "crate" | "self" | "super")
                && !self
                    .rust_crate_graph()?
                    .crates
                    .values()
                    .any(|c| c.name == root)
            {
                let _ = self.rust_use_facts(&target.file_path);
                if self
                    .upstream_rust_deps
                    .as_ref()
                    .is_some_and(|deps| deps.contains(root))
                {
                    return Ok(true);
                }
            }
        }
        if !self.rust_project_named(&real)? {
            return Ok(true);
        }
        Ok(!self
            .rust_type_decls(&Written {
                text: text.clone(),
                file: target.file_path.clone(),
                at,
            })?
            .iter()
            .any(|n| matches!(n.kind.as_str(), "struct" | "enum" | "union")))
    }

    fn rust_outside_self(&mut self, cx: &SelfContext, r: &ResolveRefIn) -> Res<bool> {
        let facts = self.rust_use_facts(&r.file_path).unwrap_or_default();
        if let Some(root) = &cx.root {
            return Ok(matches!(root.as_str(), "std" | "core" | "alloc")
                || self.upstream_rust_deps.as_ref().is_some_and(|deps| deps.contains(root)));
        }
        Ok((facts.external.contains(&cx.owner) && !facts.bound.contains(&cx.owner))
            || (STD_TYPES.contains(&cx.owner.as_str()) && !self.rust_project_type(&cx.owner, false)?))
    }

    fn rust_self_pick(&mut self, owner: &str, leaf: &str, trait_body: bool, outside: bool,
        cx: &SelfContext, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let suffix = format!("{owner}::{leaf}");
        let candidates: Vec<_> = self.nodes_by_name(leaf)?.iter().filter(|n| n.language == "rust"
            && matches!(n.kind.as_str(), "method" | "function" | "enum_member")
            && (n.qualified_name == suffix || n.qualified_name.ends_with(&format!("::{suffix}")))
            && (trait_body || !outside || n.file_path == r.file_path)
            && (cx.trait_name.is_none() || n.id != r.from_node_id)).cloned().collect();
        // Keep the same owner proof as resolve_rust_self_member before preferring inherent methods.
        let owners: Vec<_> = self.nodes_by_qualified_name(owner)?.iter()
            .filter(|n| n.language == "rust" && matches!(n.kind.as_str(),
                "struct" | "enum" | "union" | "trait" | "class")).cloned().collect();
        let local_only = owners.len() > 1;
        if local_only && owners.iter().filter(|n| n.file_path == r.file_path).count() != 1 {
            return Ok(None);
        }
        let mut all = Vec::new();
        let mut inherent = Vec::new();
        for n in candidates {
            if local_only && n.file_path != r.file_path { continue; }
            if trait_body && !self.nodes_in_file(&n.file_path)?.iter().any(|t| t.kind == "trait"
                && t.name == owner && t.start_line <= n.start_line && t.end_line >= n.start_line) { continue; }
            if !self.is_rust_trait_impl_method(&n)? { inherent.push(n.clone()); }
            all.push(n);
        }
        let candidates = if !inherent.is_empty() { inherent } else if all.len() == 1 { all } else { Vec::new() };
        Ok(candidates.into_iter().reduce(|best, n| {
            let score = |node: &KNode| (node.file_path == r.file_path,
                super::names::compute_path_proximity(&r.file_path, &node.file_path));
            if score(&n) > score(&best) { n } else { best }
        }))
    }

    /// Some(None) is authoritative: an impl's missing member is never another type's namesake.
    pub(super) fn rust_simple_self_call(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "rust" || r.reference_kind != "calls" { return Ok(None); }
        let name = re!(r"<[^>]*>").replace_all(&r.reference_name, "").into_owned();
        let Some(leaf) = name.strip_prefix("Self::").filter(|leaf| re!(r"^[A-Za-z_]\w*$").is_match(leaf)) else { return Ok(None) };
        let Some(cx) = self.rust_self_context(r) else { return Ok(None) };
        let outside = !cx.is_trait && self.rust_outside_self(&cx, r)?;
        let mut target = self.rust_self_pick(&cx.owner, leaf, cx.is_trait, outside, &cx, r)?;
        let delegates = self.node_by_id(&r.from_node_id)?.is_some_and(|n| n.name == leaf);
        if target.is_none() && !outside && !delegates {
            if let Some(trait_name) = &cx.trait_name {
                target = self.rust_self_pick(trait_name, leaf, true, false, &cx, r)?;
            }
        }
        Ok(Some(target.map(|node| KCand { confidence: if node.file_path == r.file_path { 0.9 } else { 0.8 },
            node, resolved_by: "qualified-name" })))
    }
}

#[path = "rust_types.rs"]
mod rust_types;
