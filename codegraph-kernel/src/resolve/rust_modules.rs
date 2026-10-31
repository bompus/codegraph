//! Rust module paths (import-resolver.ts resolveRustPathReference): `crate::`/`self::`/`super::` and bare paths to module files, and crate-root discovery.

use super::*;

impl KernelResolver {
    /// The Rust slice of resolveOneInner for pure `::` path refs
    /// (`crate::m::Item`, `self::sub::f`, `super::x::y`, `a::b::c`). TS
    /// reaches resolveViaImport → resolveRustPathReference on these; the
    /// boundReceiver claim can't fire on a dot-free name, and every gate
    /// between the prefilter and viaImport is language- or shape-gated
    /// dead for them. A `::`-AND-`.` name (a receiver-shaped `a::b.c`)
    /// stays on the TS side — the claim can own it there.
    /// `None` = the module-path arm missed; the caller hands migrated rust
    /// to the normal pipeline (qualified-name/exact arms mirror TS's
    /// matchReference continuation) and non-migrated rust back to TS.
    pub(super) fn resolve_rust_path_ref(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if self.is_built_in_or_external(r) {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let pre_pass = self.has_any_possible_match(&r.reference_name)
            || self.matches_any_import(r)?
            || self.framework_claims(&r.reference_name);
        if !pre_pass {
            // matchJsStoreBindingCall is JS-gated — dead for rust — so a
            // prefilter miss is terminal unresolved in TS.
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        // resolveViaImport early-nulls when imports are empty AND the file
        // is unreadable — for rust (no bindings rows) that's every missing
        // file, which then falls to matchReference's unported arms.
        if self.read_file(&r.file_path).is_none() {
            return Ok(None);
        }
        match self.match_rust_path_reference(r)? {
            // A gated candidate is discarded to terminal unresolved — the
            // spine returns the import hit verbatim at ≥0.9, and this arm
            // always carries 0.9.
            Some(c) => match self.gate_language(Some(c), r) {
                Some(c) => self.finish(r, c, None, true).map(Some),
                None => Ok(Some(ResolveOutcome::unresolved())),
            },
            None => Ok(None),
        }
    }

    /// resolveRustPathReference (import-resolver.ts): split `A::B::C` into
    /// module prefix `A::B` + leaf `C`, map the prefix to a file, find the
    /// leaf symbol in it. `import` @0.9 — the analog of
    /// resolvePythonModuleMember for Rust module paths.
    pub(super) fn match_rust_path_reference(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let path: Vec<String> = r.reference_name.split("::").filter(|s| !s.is_empty()).map(str::to_string).collect();
        if path.len() >= 2 {
            let from = self.rust_module_at(&r.file_path, r.line, r.column);
            if let Some(items) = self.rust_path_items(&path, &from, false, &mut HashSet::new())? {
                return Ok(items.into_iter().next().map(|node| KCand { node, confidence: 0.9, resolved_by: "import" }));
            }
        }
        let segments: Vec<&str> = r
            .reference_name
            .split("::")
            .filter(|s| !s.is_empty())
            .collect();
        if segments.len() < 2 {
            return Ok(None);
        }
        let leaf = segments[segments.len() - 1];
        let Some(file) =
            self.resolve_rust_module_file(&segments[..segments.len() - 1], &r.file_path)?
        else {
            return Ok(None);
        };
        if file == r.file_path {
            return Ok(None);
        }
        let file_nodes = self.nodes_in_file(&file)?;
        let target = file_nodes.iter().find(|n| {
            n.name == leaf
                && !n.qualified_name.ends_with(&format!("::{}", n.name))
                && matches!(
                    n.kind.as_str(),
                    "function"
                        | "struct"
                        | "union"
                        | "enum"
                        | "trait"
                        | "type_alias"
                        | "constant"
                        | "method"
                        | "class"
                        | "interface"
                )
        });
        Ok(target.map(|n| KCand {
            node: n.clone(),
            confidence: 0.9,
            resolved_by: "import",
        }))
    }

    /// `use a::b::f as g;` then a bare `g()` or `register(g)`: no node is
    /// named `g`, so only the use path finds `f`. A plain `use a::b::f` is
    /// left to the name arms, and a nearer binding of `g` shadows the use.
    pub(super) fn match_rust_use_alias(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.language != "rust" || r.reference_name.contains("::") || r.reference_name.contains('.') {
            return Ok(None);
        }
        let Some(content) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(path) = content.rust_uses().get(&r.reference_name).cloned() else { return Ok(None) };
        if path.rsplit("::").next() == Some(r.reference_name.as_str()) {
            return Ok(None);
        }
        let rows = self.bindings(&r.file_path)?;
        if innermost_binding(&rows, &r.reference_name, Some(r.line)).is_some_and(|b| b.kind != "import") {
            return Ok(None);
        }
        let mut at = r.clone();
        at.reference_name = path;
        Ok(self.match_rust_path_reference(&at)?.filter(|c| {
            r.reference_kind != "function_ref" || matches!(c.node.kind.as_str(), "function" | "method")
        }))
    }

    /// resolveRustModuleFile (import-resolver.ts): map module segments to
    /// `<seg>.rs` or `<seg>/mod.rs` files. Anchors on `crate`/`self`/`super`;
    /// a bare path tries self-relative (2018 expression position) then
    /// crate-relative (2015 crate-root items). External crates miss both.
    pub(super) fn resolve_rust_module_file(
        &mut self,
        segments: &[&str],
        from_file: &str,
    ) -> Res<Option<String>> {
        if segments.is_empty() {
            return Ok(None);
        }
        let first = segments[0];
        if first == "crate" {
            // The crate root's own `#[path]` decls are the module's — a name
            // one claims never reaches a namesake `head.rs`.
            if let Some(head) = segments.get(1) {
                if let Some(root) = self.rust_root_module(from_file) {
                    if self
                        .rust_mod_facts(&root.file)
                        .is_some_and(|f| f.path_mods.contains(*head))
                    {
                        return Ok(None);
                    }
                }
            }
            let root = self.rust_crate_root_dir(from_file)?;
            return Ok(self.rust_resolve_under(root, &segments[1..]));
        }
        if first == "self" {
            if segments.get(1).is_some_and(|h| {
                self.rust_mod_facts(from_file)
                    .is_some_and(|f| f.path_mods.contains(*h))
            }) {
                return Ok(None);
            }
            let dir = self.rust_module_dir_of(from_file);
            return Ok(self.rust_resolve_under(Some(dir), &segments[1..]));
        }
        if first == "super" {
            let mut supers = 0usize;
            while segments.get(supers) == Some(&"super") {
                supers += 1;
            }
            // Walk `super` by the module rules: a `#[path]`-loaded file's
            // `super` is the module that declares it, not a module of the
            // directory it happens to sit in.
            let mut scope = rust_paths::RustModule {
                file: from_file.to_string(),
                inline: Vec::new(),
            };
            for _ in 0..supers {
                let Some(s) = self.rust_super_module(&scope) else {
                    return Ok(None);
                };
                scope = s;
            }
            if segments.get(supers).is_some_and(|h| {
                self.rust_mod_facts(&scope.file)
                    .is_some_and(|f| f.path_mods.contains(*h))
            }) {
                return Ok(None);
            }
            let mut dir = self.rust_module_dir_of(&scope.file);
            for seg in &scope.inline {
                dir = pos_join(&dir, seg);
            }
            return Ok(self.rust_resolve_under(Some(dir), &segments[supers..]));
        }
        // A `#[path]` decl of the head stays unknown; the name-only lookup
        // below may not bypass it to a `head.rs` namesake (rustPathTarget).
        if self
            .rust_mod_facts(from_file)
            .is_some_and(|f| f.path_mods.contains(segments[0]))
        {
            return Ok(None);
        }
        let dir = self.rust_module_dir_of(from_file);
        let self_hit = self.rust_resolve_under(Some(dir), segments);
        if self_hit.is_some() {
            return Ok(self_hit);
        }
        if let Some(root) = self.rust_root_module(from_file) {
            if self
                .rust_mod_facts(&root.file)
                .is_some_and(|f| f.path_mods.contains(segments[0]))
            {
                return Ok(None);
            }
        }
        let root = self.rust_crate_root_dir(from_file)?;
        Ok(self.rust_resolve_under(root, segments))
    }

    /// The `resolveUnder` closure inside resolveRustModuleFile: walk module
    /// segments down from `start_dir`, each mapping to `<seg>.rs` or
    /// `<seg>/mod.rs`; `self`/`crate`/`super` mid-path are skipped (leading
    /// `super`s are consumed by the anchor dispatch). Returns the leaf
    /// module's file.
    pub(super) fn rust_resolve_under(&self, start_dir: Option<String>, rest: &[&str]) -> Option<String> {
        let mut dir = start_dir?;
        let mut target: Option<String> = None;
        if rest.is_empty() {
            let paths=[pos_join(&dir,"mod.rs"),pos_join(&dir,"lib.rs"),pos_join(&dir,"main.rs"),pos_join(pos_dirname(&dir),&format!("{}.rs",pos_basename(&dir)))];
            let mut files:Vec<_>=paths.into_iter().filter(|file|self.file_exists(file)).collect();
            files.sort();files.dedup();
            return match files.as_slice() {[only]=>Some(only.clone()),_=>None};
        }
        for seg in rest {
            if *seg == "self" || *seg == "crate" || *seg == "super" {
                continue;
            }
            let as_file = pos_join(&dir, &format!("{seg}.rs"));
            let as_mod = pos_join(&dir, &format!("{seg}/mod.rs"));
            if self.file_exists(&as_file) {
                target = Some(as_file);
            } else if self.file_exists(&as_mod) {
                target = Some(as_mod);
            } else {
                return None;
            }
            dir = pos_join(&dir, seg);
        }
        target
    }

    /// rustCrateRootDir (import-resolver.ts): the directory holding
    /// `lib.rs`/`main.rs`, walking up from the ref's file (≤64 levels). When
    /// no such anchor exists — kernel-style modules whose root file is
    /// named e.g. `rust_binder_main.rs` — falls back to climbing the `mod`
    /// declaration chain to the file no other file declares.
    pub(super) fn rust_crate_root_dir(&mut self, from_file: &str) -> Res<Option<String>> {
        self.rust_crate_root_dir_at(from_file, 0)
    }

    fn rust_crate_root_dir_at(&mut self, from_file: &str, depth: usize) -> Res<Option<String>> {
        if let Some(v) = self.rust_crate_root_memo.get(from_file) {
            return Ok(v.clone());
        }
        // A file a `#[path]` loads is in the crate of the file that declares
        // it (rustCrateRootModule, claims chained ≤8); an example, test or
        // bench its manifest gives a `path` of its own is its own crate's
        // root, its directory the crate's.
        if depth < 8 {
            if let Some(claim) = self.rust_path_claim(from_file) {
                let v = self.rust_crate_root_dir_at(&claim.file, depth + 1)?;
                self.rust_crate_root_memo
                    .insert(from_file.to_string(), v.clone());
                return Ok(v);
            }
        }
        if let Ok(graph) = self.rust_crate_graph() {
            if Self::rust_crate_dir_of(from_file, &graph).is_some_and(|dir| {
                graph
                    .crates
                    .get(&dir)
                    .is_some_and(|c| c.others.iter().any(|o| o == from_file))
            }) {
                let v = Some(pos_dirname(from_file).to_string());
                self.rust_crate_root_memo
                    .insert(from_file.to_string(), v.clone());
                return Ok(v);
            }
        }
        let mut dir = pos_dirname(from_file).to_string();
        let mut found = None;
        for _ in 0..64 {
            if self.file_exists(&pos_join(&dir, "lib.rs"))
                || self.file_exists(&pos_join(&dir, "main.rs"))
            {
                found = Some(dir);
                break;
            }
            let parent = pos_dirname(&dir);
            if parent == dir {
                break;
            }
            dir = parent.to_string();
        }
        let v = match found {
            Some(d) => Some(d),
            None => self.rust_mod_chain_crate_root(from_file)?,
        };
        self.rust_crate_root_memo
            .insert(from_file.to_string(), v.clone());
        Ok(v)
    }

    /// Fallback crate-root discovery for layouts whose root file is not
    /// `lib.rs`/`main.rs`. Climbs the `mod` declaration chain: a file's
    /// parent module is the `.rs` file declaring `mod <stem>;` — either
    /// `<dir>/<dirname>.rs` one level up (2018 nested modules) or any
    /// sibling `.rs` in the parent dir (flat roots like
    /// `rust_binder_main.rs`, `mod.rs`, `lib.rs`). The file with no
    /// declarant is the crate root; its directory is the root dir.
    /// Declarants must be indexed `.rs` files (graph-scoped, same as
    /// `file_exists`'s fast path).
    pub(super) fn rust_mod_chain_crate_root(&mut self, from_file: &str) -> Res<Option<String>> {
        let mut cur = pos_normalize(from_file);
        for _ in 0..64 {
            let base = pos_basename(&cur);
            let dir = pos_dirname(&cur);
            let (parent_dir, stem) = if base == "mod.rs" {
                (pos_dirname(dir).to_string(), pos_basename(dir).to_string())
            } else {
                (
                    dir.to_string(),
                    base.strip_suffix(".rs").unwrap_or(base).to_string(),
                )
            };
            // 2018 nested-module declarant: `<up>/<basename(parent)>.rs`
            // owns `<parent>/` as its module dir (e.g. `binder/node.rs`
            // declares `mod wrapper` for `binder/node/wrapper.rs`).
            let nested = pos_join(pos_dirname(&parent_dir), &format!("{}.rs", pos_basename(&parent_dir)));
            let mut declarant = None;
            if nested != cur && self.rust_file_declares_mod(&nested, &stem)? {
                declarant = Some(nested);
            } else {
                let candidates = self.rust_rs_files_in_dir(&parent_dir);
                for cand in candidates.iter() {
                    if *cand != cur && self.rust_file_declares_mod(cand, &stem)? {
                        declarant = Some(cand.clone());
                        break;
                    }
                }
            }
            match declarant {
                Some(d) => cur = d,
                None => return Ok(Some(dir.to_string())),
            }
        }
        Ok(None)
    }

    /// `.rs` files indexed under `dir` (exact parent dir, sorted for
    /// determinism), built once from `known_files`.
    pub(super) fn rust_rs_files_in_dir(&self, dir: &str) -> Arc<Vec<String>> {
        if let Some(files) = self.sorted_files() {
            let mut out: Vec<String> = files
                .iter()
                .map(|f| pos_normalize(f))
                .filter(|f| f.ends_with(".rs") && pos_dirname(f) == dir)
                .collect();
            out.sort();
            return Arc::new(out);
        }
        self.table().map(|t| t.rust_rs_files_in_dir(dir)).unwrap_or_default()
    }

    /// Whether `rel_file` contains a `mod <stem>;` declaration (comment-
    /// stripped; `pub`/`pub(...)` qualifiers allowed). `mod <stem> {`
    /// inline modules end in `{`, not `;`, and do not match.
    pub(super) fn rust_file_declares_mod(&mut self, rel_file: &str, stem: &str) -> Res<bool> {
        let Some(lines) = self.read_file(rel_file) else {
            return Ok(false);
        };
        // `^\s*(?:pub\s*(?:\([^)]*\))?\s+)?mod\s+STEM\s*;`
        static MOD_DECL: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new(r"^\s*(?:pub\s*(?:\([^)]*\))?\s+)?mod\s+", r"\s*;", false, false, false)
        });
        Ok(lines.iter().any(|l| MOD_DECL.is_match(&strip_line_comments(l), stem)))
    }
}

#[path = "rust_paths.rs"]
pub(super) mod rust_paths;
