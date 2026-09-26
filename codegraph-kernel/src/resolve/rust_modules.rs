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
            return Ok(Some(ResolveOutcome::passthrough("ineligible:lang")));
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
            let root = self.rust_crate_root_dir(from_file)?;
            return Ok(self.rust_resolve_under(root, &segments[1..]));
        }
        if first == "self" {
            return Ok(
                self.rust_resolve_under(Some(rust_self_module_dir(from_file)), &segments[1..])
            );
        }
        if first == "super" {
            let mut supers = 0usize;
            while segments.get(supers) == Some(&"super") {
                supers += 1;
            }
            let mut dir = Some(rust_self_module_dir(from_file));
            for _ in 0..supers {
                dir = dir.map(|d| pos_dirname(&d).to_string());
            }
            return Ok(self.rust_resolve_under(dir, &segments[supers..]));
        }
        let self_hit = self.rust_resolve_under(Some(rust_self_module_dir(from_file)), segments);
        if self_hit.is_some() {
            return Ok(self_hit);
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
        if let Some(v) = self.rust_crate_root_memo.get(from_file) {
            return Ok(v.clone());
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
