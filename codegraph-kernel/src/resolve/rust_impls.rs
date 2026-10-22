//! Rust associated calls and foreign-type trait implementations (#2697, #2700).
use super::*;
use super::iteration::descendant_for_position;

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
                    root: None, trait_name: None, trait_args: Vec::new(), is_trait: true });
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
                return Some(SelfContext { owner: type_name(ty, text)?, root, trait_name, trait_args, is_trait: false });
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
