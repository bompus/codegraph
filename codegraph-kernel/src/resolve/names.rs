//! Name machinery (name-matcher.ts): visibility, exact and fuzzy matching, best-match scoring.

use super::*;

impl KernelResolver {
    // -----------------------------------------------------------------------
    // Name machinery (name-matcher.ts)
    // -----------------------------------------------------------------------

    /// applyLanguageGate (name-matcher.ts).
    pub(super) fn apply_language_gate(&self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Vec<Arc<KNode>> {
        if r.reference_kind == "references" || r.reference_kind == "function_ref" {
            return candidates
                .into_iter()
                .filter(|c| same_language_family(&c.language, &r.language))
                .collect();
        }
        if r.reference_kind == "imports" {
            return candidates
                .into_iter()
                .filter(|c| {
                    !crosses_known_family(&c.language, &r.language)
                        && !crosses_code_boundary(&c.language, &r.language)
                })
                .collect();
        }
        candidates
            .into_iter()
            .filter(|c| !crosses_code_boundary(&c.language, &r.language))
            .collect()
    }

    /// isLexicallyReachable (name-matcher.ts): a function nested in a
    /// same-file function/method is reachable only from inside the parent.
    pub(super) fn is_lexically_reachable(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if candidate.kind != "function" {
            return Ok(true);
        }
        if no_nested_functions(&candidate.language) {
            return Ok(true);
        }
        let qn = &candidate.qualified_name;
        let Some(sep) = qn.rfind("::") else { return Ok(true) };
        let parent_qn = &qn[..sep];
        let containers: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(parent_qn)?
            .iter()
            .filter(|p| {
                p.file_path == candidate.file_path
                    && (p.kind == "function" || p.kind == "method")
                    && p.start_line <= candidate.start_line
                    && p.end_line >= candidate.end_line
            })
            .cloned()
            .collect();
        if containers.is_empty() {
            return Ok(true);
        }
        Ok(r.file_path == candidate.file_path
            && containers
                .iter()
                .any(|p| r.line >= p.start_line && r.line <= p.end_line))
    }

    /// isSealedModule (name-matcher.ts): an ESM file with import statements
    /// and no export of any form offers nothing cross-file.
    pub(super) fn is_sealed_module(&mut self, file_path: &str) -> Res<bool> {
        if let Some(&hit) = self.sealed_memo.get(file_path) {
            return Ok(hit);
        }
        let rows = self.bindings(file_path)?;
        let nodes = self.nodes_in_file(file_path)?;
        let sealed = !rows.is_empty()
            && nodes.iter().any(|n| n.kind == "import")
            && !rows.iter().any(|r| r.exported_as.is_some())
            && !nodes.iter().any(|n| n.is_exported);
        self.sealed_memo.insert(file_path.to_string(), sealed);
        Ok(sealed)
    }

    /// isStaticCFunction (name-matcher.ts): binding row storage === 'static'.
    pub(super) fn is_static_c_function(&mut self, candidate: &KNode) -> Res<bool> {
        if let Some(&hit) = self.c_static_memo.get(&candidate.id) {
            return Ok(hit);
        }
        let rows = self.bindings(&candidate.file_path)?;
        let is_static = rows
            .iter()
            .find(|r| r.node_id.as_deref() == Some(candidate.id.as_str()))
            .is_some_and(|r| r.storage.as_deref() == Some("static"));
        self.c_static_memo.insert(candidate.id.clone(), is_static);
        Ok(is_static)
    }

    /// isRustTraitImplMethod (name-matcher.ts) — scan upward for the nearest
    /// `impl` header; `impl Trait for` wins, a top-level item ends the scan.
    pub(super) fn is_rust_trait_impl_method(&mut self, candidate: &KNode) -> Res<bool> {
        if candidate.kind != "method" {
            return Ok(false);
        }
        if let Some(&hit) = self.rust_trait_memo.get(&candidate.id) {
            return Ok(hit);
        }
        let lines = self
            .read_file(&candidate.file_path)
            .unwrap_or_else(|| Rc::new(SourceFile::new(Vec::new())));
        let mut is_trait = false;
        let mut i = candidate.start_line.saturating_sub(2);
        while i >= 0 {
            let line = lines.get(i as usize).map(|s| s.as_str()).unwrap_or("");
            if impl_re().is_match(line) {
                let stripped = line.split("//").next().unwrap_or("");
                is_trait = impl_for_re().is_match(stripped);
                break;
            }
            if item_re().is_match(line) {
                break;
            }
            i -= 1;
        }
        self.rust_trait_memo.insert(candidate.id.clone(), is_trait);
        Ok(is_trait)
    }

    /// isCrossFileReachable (name-matcher.ts).
    pub(super) fn is_cross_file_reachable(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "markdown"
            && candidate.language == "markdown"
            && !markdown_path_re().is_match(&r.reference_name.replace('\\', "/"))
        {
            return Ok(false);
        }
        if r.reference_kind == "calls"
            && is_esm_family(&candidate.language)
            && (candidate.kind == "constant" || candidate.kind == "variable")
            && is_json_require_signature(candidate.signature.as_deref().unwrap_or(""))
        {
            return Ok(false);
        }
        if candidate.file_path == r.file_path {
            return Ok(true);
        }
        if !is_esm_family(&candidate.language) {
            return Ok(true);
        }
        self.is_sealed_module(&candidate.file_path).map(|s| !s)
    }

    /// isVisibleAcrossFiles (name-matcher.ts).
    pub(super) fn is_visible_across_files(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if candidate.file_path == r.file_path {
            return Ok(true);
        }
        let lang = candidate.language.as_str();
        if lang == "c" || lang == "cpp" {
            return Ok(candidate.kind != "function"
                || !c_source_ext_re().is_match(&candidate.file_path)
                || !self.is_static_c_function(candidate)?);
        }
        if lang == "go" {
            let first = candidate.name.chars().next().unwrap_or('_');
            return Ok(first.is_ascii_uppercase()
                || pos_dirname(&candidate.file_path) == pos_dirname(&r.file_path));
        }
        if lang == "rust" {
            if candidate.visibility.as_deref() != Some("private") {
                return Ok(true);
            }
            if self.is_rust_trait_impl_method(candidate)? {
                return Ok(true);
            }
            let owner = rust_module_dir(&candidate.file_path);
            return Ok(r.file_path.starts_with(&format!("{}/", owner)));
        }
        if private_is_file_local(lang) {
            return Ok(candidate.visibility.as_deref() != Some("private"));
        }
        self.is_cross_file_reachable(candidate, r)
    }

    /// isBareJsCall (name-matcher.ts): a receiver-less JS/TS `calls` ref.
    pub(super) fn is_bare_js_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.reference_kind != "calls" || !is_js_family(&r.language) {
            return Ok(false);
        }
        // `!name.includes('.')` — dead on the bare path, live for non-bare
        // callers (a `Foo::bar` calls ref carries no '.').
        if r.reference_name.contains('.') {
            return Ok(false);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(false) };
        let at = js_slice(line, r.column as usize);
        // `new RegExp('^' + nameEsc + '\\s*[(<]')` — the name is a literal
        // prefix, then the call opener.
        let is_call = at
            .strip_prefix(r.reference_name.as_str())
            .is_some_and(|rest| bare_call_opener_re().is_match(rest));
        if !is_call {
            return Ok(false);
        }
        let before = js_prefix(line, r.column as usize);
        Ok(!js_call_prefix_re().is_match(before) || js_call_keyword_re().is_match(before))
    }

    /// cppBareCallForm (name-matcher.ts) — only the ADL range names.
    pub(super) fn cpp_bare_call_form(&mut self, r: &ResolveRefIn) -> Res<Option<&'static str>> {
        if r.reference_kind != "calls" {
            return Ok(None);
        }
        if r.language != "c" && r.language != "cpp" {
            return Ok(None);
        }
        if !is_cpp_adl_name(&r.reference_name) {
            return Ok(None);
        }
        // `name.includes('.') || includes('::')` — dead for bare names.
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(None) };
        let from = (r.column as usize).min(utf16_len(line));
        let hay = js_slice(line, from);
        // `(^|[^A-Za-z0-9_])NAME\s*\(` — the literal name, not preceded by a
        // word byte, then the call opener.
        let name = r.reference_name.as_str();
        let Some(at) = occurrences(hay, name, 0).find(|&at| {
            (at == 0 || !is_word_byte(hay.as_bytes()[at - 1]))
                && cpp_call_opener_re().is_match(&hay[at + name.len()..])
        }) else {
            return Ok(None);
        };
        // m.index + m[1].length in TS: the name's UTF-16 offset in `hay`.
        let name_at = from + utf16_len(&hay[..at]);
        let before: String = js_prefix(line, name_at)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if before.ends_with("::") {
            return Ok(Some("qualified"));
        }
        if cpp_this_arrow_re().is_match(&before) || cpp_this_dot_re().is_match(&before) {
            return Ok(Some("this-member"));
        }
        if before.ends_with("->") || before.ends_with('.') {
            return Ok(Some("explicit-member"));
        }
        let after_name = js_slice(line, name_at + utf16_len(&r.reference_name));
        let Some(open) = after_name.find('(') else { return Ok(Some("implicit-this")) };
        Ok(Some(
            if after_name_paren_re().is_match(&after_name[open + 1..]) {
                "implicit-this"
            } else {
                "free-args"
            },
        ))
    }

    /// isImplicitThisFieldCall (name-matcher.ts): the one case a bare `calls`
    /// name can be a `field` — C++ `this->fp(...)` arrives as a bare ref (the
    /// extractor drops `this`), so allow it only when the field's owner is
    /// the call site's enclosing type.
    pub(super) fn is_implicit_this_field_call(&mut self, r: &ResolveRefIn, field: &KNode) -> Res<bool> {
        if r.language != "cpp" {
            return Ok(false);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(false);
        };
        let Some(prefix) = enclosing_type_prefix(Some(caller.as_ref())) else {
            return Ok(false);
        };
        Ok(enclosing_type_prefix(Some(field)).as_deref() == Some(prefix.as_str()))
    }

    /// Returns `Ok(None)` when the call-site form vetoes every candidate
    /// (the TS `null` — reference stays unresolved).
    pub(super) fn apply_cpp_call_site_form(
        &mut self,
        r: &ResolveRefIn,
        candidates: Vec<Arc<KNode>>,
    ) -> Res<Option<Vec<Arc<KNode>>>> {
        let Some(form) = self.cpp_bare_call_form(r)? else {
            return Ok(Some(candidates));
        };
        match form {
            "qualified" | "explicit-member" => Ok(None),
            "this-member" => {
                let origin = self.node_by_id(&r.from_node_id)?;
                let Some(prefix) = enclosing_type_prefix(origin.as_deref()) else {
                    return Ok(None);
                };
                Ok(Some(
                    candidates
                        .into_iter()
                        .filter(|n| callable_on_type(n, &prefix))
                        .collect(),
                ))
            }
            "free-args" => {
                let local: Vec<Arc<KNode>> = candidates
                    .into_iter()
                    .filter(|n| n.kind == "function" && n.file_path == r.file_path)
                    .collect();
                Ok(Some(if local.len() == 1 { local } else { Vec::new() }))
            }
            _ => Ok(Some(candidates)),
        }
    }

    /// findBestMatch (name-matcher.ts) — strict `>` first-max scoring.
    pub(super) fn find_best_match(&self, r: &ResolveRefIn, candidates: &[Arc<KNode>]) -> Option<Arc<KNode>> {
        let mut best_score = -1f64;
        let mut best: Option<Arc<KNode>> = None;
        let mut ref_dirs: Vec<String> =
            r.file_path.split('/').map(|s| s.to_string()).collect();
        ref_dirs.pop();
        let has_same_language = candidates.iter().any(|c| c.language == r.language);
        for candidate in candidates {
            if has_same_language && candidate.language != r.language {
                continue;
            }
            let mut score = 0f64;
            if candidate.file_path == r.file_path {
                score += 100.0;
            }
            score += path_proximity_from_dirs(&ref_dirs, &candidate.file_path) as f64;
            if candidate.language == r.language {
                score += 50.0;
            } else {
                score -= 80.0;
            }
            if r.reference_kind == "calls"
                && (candidate.kind == "function" || candidate.kind == "method")
            {
                score += 25.0;
            }
            if r.reference_kind == "instantiates"
                && matches!(candidate.kind.as_str(), "class" | "struct" | "union" | "interface")
            {
                score += 25.0;
            }
            if r.reference_kind == "decorates" {
                if candidate.kind == "function" || candidate.kind == "method" {
                    score += 25.0;
                } else if candidate.kind == "class" || candidate.kind == "interface" {
                    score += 15.0;
                }
            }
            if candidate.is_exported {
                score += 10.0;
            }
            if candidate.file_path == r.file_path && candidate.start_line > 0 {
                let distance = (candidate.start_line - r.line).abs() as f64;
                score += (20.0 - distance / 10.0).max(0.0);
            }
            if score > best_score {
                best_score = score;
                best = Some(candidate.clone());
            }
        }
        best
    }

    /// matchByExactName (name-matcher.ts).
    pub(super) fn match_by_exact_name(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let bare_js = self.is_bare_js_call(r)?;
        let all_named: Vec<Arc<KNode>> = self
            .nodes_by_name(&r.reference_name)?
            .iter()
            .cloned()
            .collect();
        let mut candidates = self.apply_language_gate(all_named, r);
        candidates.retain(|n| n.kind != "import");
        // Nested locals reachable only from inside their container (#1230).
        let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(candidates.len());
        for n in candidates.into_iter() {
            if self.is_lexically_reachable(&n, r)? {
                kept.push(n);
            }
        }
        let mut candidates = kept;
        candidates.retain(|n| !is_inheritance_ref(&r.reference_kind) || is_supertype_target_kind(&n.kind));
        candidates.retain(|n| r.reference_kind != "imports" || is_importable_kind(&n.kind));
        {
            let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(candidates.len());
            for n in candidates.into_iter() {
                if r.reference_kind != "imports"
                    || n.file_path == r.file_path
                    || !is_esm_family(&n.language)
                    || !self.is_sealed_module(&n.file_path)?
                {
                    kept.push(n);
                }
            }
            candidates = kept;
        }
        candidates.retain(|n| !(bare_js && n.kind == "method"));
        // A C/C++ `field` is reachable only through a receiver — `s.f`,
        // `p->f`, `T::f` — which the extractor encodes as a dotted or
        // `::`-qualified ref, so a bare name can never mean one. C++ keeps
        // an implicit-this exception (`this->` is dropped by the extractor)
        // when the field's owner is the ref site's enclosing type. Without
        // this, Linux-scale corpora resolved ~39k bare libc/kernel calls
        // (`bind`, `close`, `ioctl`) onto whatever fn-pointer member shared
        // the name. Other languages' `field` nodes are exempt — Solidity
        // `emit Event(...)` legitimately bare-calls an event field.
        // (name-matcher.ts: matchByExactName / isImplicitThisFieldCall)
        {
            let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(candidates.len());
            for n in candidates.into_iter() {
                if n.kind == "field"
                    && (n.language == "c" || n.language == "cpp")
                    && !self.is_implicit_this_field_call(r, &n)?
                {
                    continue;
                }
                kept.push(n);
            }
            candidates = kept;
        }
        if bare_js {
            let locally_bound =
                self.is_locally_bound_js_name(&r.reference_name, &r.file_path, Some(r.line))?;
            if locally_bound {
                candidates.retain(|n| n.file_path == r.file_path);
            }
        }

        // Whatever the call looks like, a name a parameter or local shadows at
        // the ref line is never another file's symbol.
        if candidates.iter().any(|n| n.file_path != r.file_path) && self.is_shadowed_import_name(r)? {
            candidates.retain(|n| n.file_path == r.file_path);
        }

        // A name bound to a bare external import can land only on a same-file
        // definition (#1709).
        if candidates.iter().any(|n| n.file_path != r.file_path)
            && self.is_bound_to_bare_import(r)?
        {
            candidates.retain(|n| n.file_path == r.file_path);
        }

        // C/C++ call-site form.
        let Some(cpp_form) = self.apply_cpp_call_site_form(r, candidates)? else {
            return Ok(None);
        };
        let candidates = cpp_form;
        if candidates.is_empty() {
            return Ok(None);
        }
        if candidates.len() == 1 {
            if !self.is_cross_file_reachable(&candidates[0], r)? {
                return Ok(None);
            }
            if bare_js && !is_bare_call_target_kind(&candidates[0].kind) {
                return Ok(None);
            }
            let cross = candidates[0].language != r.language;
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: if cross { 0.5 } else { 0.9 },
                resolved_by: "exact-match",
            }));
        }
        if candidates.len() as i64 > self.ambiguous_ceiling {
            return Ok(None);
        }
        let Some(best) = self.find_best_match(r, &candidates) else {
            return Ok(None);
        };
        if bare_js && !is_bare_call_target_kind(&best.kind) {
            return Ok(None);
        }
        if self.is_cross_file_reachable(&best, r)? {
            let proximity = compute_path_proximity(&r.file_path, &best.file_path);
            return Ok(Some(KCand {
                node: best,
                confidence: if proximity >= 30 { 0.7 } else { 0.4 },
                resolved_by: "exact-match",
            }));
        }
        Ok(None)
    }

    /// matchFuzzy (name-matcher.ts).
    pub(super) fn match_fuzzy(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if self.is_bound_to_bare_import(r)? {
            return Ok(None);
        }
        if let Some(form) = self.cpp_bare_call_form(r)? {
            if matches!(form, "qualified" | "explicit-member" | "this-member" | "free-args") {
                return Ok(None);
            }
        }
        let candidates = self.nodes_by_lower_name(&r.reference_name)?;
        let callable: Vec<Arc<KNode>> = candidates
            .iter()
            .filter(|n| matches!(n.kind.as_str(), "function" | "method" | "class"))
            .cloned()
            .collect();
        let gated = self.apply_language_gate(callable, r);
        let same_language: Vec<Arc<KNode>> = gated
            .iter()
            .filter(|n| n.language == r.language)
            .cloned()
            .collect();
        let final_candidates = if same_language.is_empty() {
            gated
        } else {
            same_language
        };
        if final_candidates.len() == 1 {
            let only = final_candidates[0].clone();
            let reachable = self.is_lexically_reachable(&only, r)?
                && self.is_visible_across_files(&only, r)?
                && self.is_cross_file_reachable(&only, r)?;
            let bare_decline = self.is_bare_js_call(r)?
                && (only.kind == "method"
                    || (only.file_path != r.file_path
                        && self.is_locally_bound_js_name(
                            &r.reference_name,
                            &r.file_path,
                            Some(r.line),
                        )?));
            let shadowed = only.file_path != r.file_path && self.is_shadowed_import_name(r)?;
            let reachable = reachable && !bare_decline && !shadowed && self.is_lexically_reachable(&only, r)?;
            if reachable {
                let cross = only.language != r.language;
                return Ok(Some(KCand {
                    node: only,
                    confidence: if cross { 0.3 } else { 0.5 },
                    resolved_by: "fuzzy",
                }));
            }
        }
        Ok(None)
    }

    /// matchReference restricted to the bare-name slice: every strategy
    /// before exact-name keys on a separator a bare name cannot carry, so
    /// the pipeline is exact → fuzzy.
    pub(super) fn match_reference_bare(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if let Some(c) = self.match_by_exact_name(r)? {
            return Ok(Some(c));
        }
        self.match_fuzzy(r)
    }

    /// resolveObjectLiteralMember (name-matcher.ts): an imported object
    /// literal used as a namespace — find the member by containment.
    pub(super) fn resolve_object_literal_member(
        &mut self,
        container: &KNode,
        member: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
    ) -> Res<Option<KCand>> {
        if container.kind != "constant" && container.kind != "variable" {
            return Ok(None);
        }
        if !is_object_literal_language(&container.language) {
            return Ok(None);
        }
        if !same_language_family(&container.language, &r.language) {
            return Ok(None);
        }
        let in_file = self.nodes_in_file(&container.file_path)?;
        let callable =
            |n: &KNode| n.kind == "function" || n.kind == "method";
        let accepts = |n: &KNode| {
            if r.reference_kind == "calls" {
                callable(n)
            } else {
                callable(n)
                    || n.kind == "property"
                    || n.kind == "variable"
                    || n.kind == "constant"
            }
        };
        // rangeWithin / sameRange (name-matcher.ts).
        let range_within = |inner: &KNode, outer: &KNode| {
            !(inner.start_line < outer.start_line
                || inner.end_line > outer.end_line
                || (inner.start_line == outer.start_line
                    && inner.start_column < outer.start_column)
                || (inner.end_line == outer.end_line && inner.end_column > outer.end_column))
        };
        let same_range = |a: &KNode, b: &KNode| {
            a.start_line == b.start_line
                && a.start_column == b.start_column
                && a.end_line == b.end_line
                && a.end_column == b.end_column
        };
        let inside: Vec<Arc<KNode>> = in_file
            .iter()
            .filter(|n| n.id != container.id && range_within(n, container))
            .cloned()
            .collect();
        let mut candidates: Vec<Arc<KNode>> = inside
            .iter()
            .filter(|n| n.name == member && accepts(n))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        // Drop members nested inside another callable's body in the literal.
        let bodies: Vec<&Arc<KNode>> = inside.iter().filter(|n| callable(n)).collect();
        candidates.retain(|c| {
            !bodies
                .iter()
                .any(|b| b.id != c.id && !same_range(b, c) && range_within(c, b))
        });
        if candidates.is_empty() {
            return Ok(None);
        }
        candidates.sort_by(|a, b| {
            let ca = if callable(a) { 0 } else { 1 };
            let cb = if callable(b) { 0 } else { 1 };
            ca.cmp(&cb)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.start_column.cmp(&b.start_column))
        });
        Ok(Some(KCand {
            node: candidates[0].clone(),
            confidence,
            resolved_by,
        }))
    }

    /// matchByQualifiedName (name-matcher.ts) — exact `qualifiedName` lookup,
    /// then the last-segment suffix match. Erlang's arity arms are dead (not
    /// a migrated language).
    pub(super) fn match_by_qualified_name(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if !r.reference_name.contains("::") && !r.reference_name.contains('.') {
            return Ok(None);
        }
        // A `calls` ref never resolves to a yaml/properties config key (#1180).
        let keep_for_ref = |nodes: &[Arc<KNode>]| -> Vec<Arc<KNode>> {
            nodes
                .iter()
                .filter(|n| {
                    r.reference_kind != "calls"
                        || !(n.kind == "constant"
                            && (n.language == "yaml" || n.language == "properties"))
                })
                .cloned()
                .collect()
        };

        let candidates = keep_for_ref(&self.nodes_by_qualified_name(&r.reference_name)?);
        if candidates.len() == 1 {
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: 0.95,
                resolved_by: "qualified-name",
            }));
        }
        if candidates.len() > 1 {
            let ordered = prefer_call_site_file(candidates, &r.file_path);
            if ordered[0].file_path == r.file_path {
                return Ok(Some(KCand {
                    node: ordered[0].clone(),
                    confidence: 0.95,
                    resolved_by: "qualified-name",
                }));
            }
        }

        // Partial match — the last `:`/`.` segment, then the suffix filter.
        let last_name = r
            .reference_name
            .rsplit([':', '.'])
            .next()
            .unwrap_or("");
        if !last_name.is_empty() {
            let partial = keep_for_ref(&self.nodes_by_name(last_name)?)
                .into_iter()
                .filter(|n| n.qualified_name.ends_with(&r.reference_name))
                .collect();
            let chosen = prefer_call_site_file(partial, &r.file_path);
            if let Some(first) = chosen.into_iter().next() {
                return Ok(Some(KCand {
                    node: first,
                    confidence: 0.85,
                    resolved_by: "qualified-name",
                }));
            }
        }
        Ok(None)
    }


    /// matchFunctionRef, bare arm (name-matcher.ts): name-exact
    /// function/method nodes in the ref's language family — plus Python
    /// classes — excluding the origin node; JS/TS/ArkTS/C++/Python/PHP match
    /// functions only (a bare identifier there is never a method value).
    /// Same-file wins by earliest line; cross-file is unique-or-drop.
    pub(super) fn match_function_ref_bare(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let bare_fn_only = matches!(
            r.language.as_str(),
            "typescript" | "tsx" | "javascript" | "jsx" | "arkts" | "cpp" | "python" | "php"
        );
        let bare_class_ok = r.language == "python";
        let mut candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(&r.reference_name)?
            .iter()
            .filter(|n| {
                (n.kind == "function"
                    || (!bare_fn_only && n.kind == "method")
                    || (bare_class_ok && n.kind == "class"))
                    && same_language_family(&n.language, &r.language)
                    && n.id != r.from_node_id
            })
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        // Swift implicit-self: a bare identifier names a method only of the
        // enclosing type; same-named methods elsewhere are parameter
        // collisions. Free functions are unaffected; top-level code has no
        // implicit self, so method targets drop entirely there.
        if r.language == "swift" && candidates.iter().any(|n| n.kind == "method") {
            let class_prefix = match self.node_by_id(&r.from_node_id)? {
                Some(from) => match from.qualified_name.rfind("::") {
                    Some(sep) if sep > 0 => Some(from.qualified_name[..sep].to_string()),
                    _ => None,
                },
                None => None,
            };
            candidates.retain(|n| {
                if n.kind != "method" {
                    return true;
                }
                let Some(cp) = &class_prefix else { return false };
                match n.qualified_name.rfind("::") {
                    Some(msep) if msep > 0 => {
                        let mp = &n.qualified_name[..msep];
                        mp == cp.as_str()
                            || mp.ends_with(&format!("::{cp}"))
                            || cp.ends_with(&format!("::{mp}"))
                    }
                    _ => false,
                }
            });
            if candidates.is_empty() {
                return Ok(None);
            }
        }
        // Same-file definition wins; same-name overloads in one file are the
        // same conceptual symbol — first by position for determinism
        // (min_by_key keeps the first minimum, matching TS's `<=` reduce).
        let same_file: Vec<Arc<KNode>> = candidates
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        if !same_file.is_empty() {
            // Swift: several same-named METHODS in one file are an overload
            // family — a bare identifier is a same-named parameter, not a
            // method value. A single method still resolves.
            if r.language == "swift"
                && same_file.len() > 1
                && same_file.iter().all(|n| n.kind == "method")
            {
                return Ok(None);
            }
            let target = same_file.iter().min_by_key(|n| n.start_line).unwrap().clone();
            return Ok(Some(KCand {
                node: target,
                confidence: if same_file.len() == 1 { 0.95 } else { 0.9 },
                resolved_by: "function-ref",
            }));
        }
        // Cross-file: only an unambiguous match resolves.
        if candidates.len() == 1 {
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: 0.8,
                resolved_by: "function-ref",
            }));
        }
        Ok(None)
    }

    /// matchFunctionRef's `::` member-pointer arm (name-matcher.ts): an
    /// explicit `Cls::member` shape (`&Widget::on_click` emitted as
    /// `Widget::on_click`) resolves the member ON THAT SCOPE — exempt from
    /// bareFnOnly, origin excluded, qualified-name equality or `::`-suffix.
    /// Same-file pool wins by earliest line @0.9; cross-file unique-or-drop.
    pub(super) fn match_function_ref_scoped(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(sep) = r.reference_name.rfind("::") else {
            return Ok(None);
        };
        let member = &r.reference_name[sep + 2..];
        let suffix = format!("::{}", r.reference_name);
        let scoped: Vec<Arc<KNode>> = self
            .nodes_by_name(member)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "function" | "method")
                    && same_language_family(&n.language, &r.language)
                    && n.id != r.from_node_id
                    && (n.qualified_name == r.reference_name
                        || n.qualified_name.ends_with(&suffix))
            })
            .cloned()
            .collect();
        if scoped.is_empty() {
            return Ok(None);
        }
        let same_file: Vec<Arc<KNode>> = scoped
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        if same_file.is_empty() && scoped.len() > 1 {
            return Ok(None);
        }
        let pool = if same_file.is_empty() { scoped } else { same_file };
        // `<=` reduce keeps the first minimum — min_by_key does the same.
        let target = pool.iter().min_by_key(|n| n.start_line).unwrap().clone();
        Ok(Some(KCand {
            node: target,
            confidence: 0.9,
            resolved_by: "function-ref",
        }))
    }

}

/// JS `string.slice(i)` over UTF-16 code units — `column` values are
/// extraction offsets consumed as JS string indices, so replicate that
/// indexing (round a mid-surrogate boundary up to the next char).
pub(super) fn js_slice(s: &str, start: usize) -> &str {
    &s[js_unit_to_byte(s, start)..]
}

/// JS `string.slice(0, i)` over UTF-16 code units.
pub(super) fn js_prefix(s: &str, end: usize) -> &str {
    &s[..js_unit_to_byte(s, end)]
}

/// UTF-16 unit offset → byte offset (clamped to the next char boundary).
pub(super) fn js_unit_to_byte(s: &str, units: usize) -> usize {
    let mut seen = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if seen >= units {
            return byte_idx;
        }
        seen += ch.len_utf16();
    }
    s.len()
}

/// UTF-16 length — the `.length` JS sees.
pub(super) fn utf16_len(s: &str) -> usize {
    // One unit per char (every non-continuation byte), two for a 4-byte one.
    s.bytes().map(|b| usize::from(b & 0xC0 != 0x80) + usize::from(b >= 0xF0)).sum()
}

/// `utf16_len(s) > limit`, settled from the byte length when it can be: a
/// char is 1–3 bytes per UTF-16 unit.
pub(super) fn utf16_len_exceeds(s: &str, limit: usize) -> bool {
    s.len() > limit && (s.len() > 3 * limit || utf16_len(s) > limit)
}

/// rustModuleDir (name-matcher.ts).
pub(super) fn rust_module_dir(file_path: &str) -> String {
    let base = pos_basename(file_path);
    let dir = pos_dirname(file_path);
    if base == "mod.rs" || base == "lib.rs" || base == "main.rs" {
        return dir.to_string();
    }
    pos_join(dir, base.strip_suffix(".rs").unwrap_or(base))
}

/// The `= require("….json")` signature guard (isCrossFileReachable).
/// Hand-rolled because the JS pattern uses a backreference.
pub(super) fn is_json_require_signature(sig: &str) -> bool {
    // ^=\s*require\s*\(\s*(['"])[^'"]+\.json\1\s*\)\s*;?\s*$
    let s = sig.trim();
    let Some(s) = s.strip_prefix('=') else { return false };
    let s = s.trim_start();
    let Some(s) = s.strip_prefix("require") else { return false };
    let s = s.trim_start();
    let Some(s) = s.strip_prefix('(') else { return false };
    let s = s.trim_start();
    let Some(q) = s.chars().next() else { return false };
    if q != '\'' && q != '"' {
        return false;
    }
    let Some(end) = s[1..].find(q) else { return false };
    let content = &s[1..1 + end];
    // `[^'"]+\.json` — at least one non-quote char, then literal `.json`.
    if content.len() <= ".json".len()
        || !content.ends_with(".json")
        || content.contains(['\'', '"'])
    {
        return false;
    }
    let s = s[1 + end + 1..].trim_start();
    let Some(s) = s.strip_prefix(')') else { return false };
    let s = s.trim_start();
    let s = s.strip_prefix(';').unwrap_or(s);
    s.trim().is_empty()
}

/// enclosingTypePrefix + callableOnType + applyCppCallSiteForm
/// (name-matcher.ts).
pub(super) fn enclosing_type_prefix(node: Option<&KNode>) -> Option<String> {
    let node = node?;
    let sep = node.qualified_name.rfind("::")?;
    if sep == 0 {
        return None;
    }
    Some(node.qualified_name[..sep].to_string())
}

pub(super) fn callable_on_type(candidate: &KNode, type_prefix: &str) -> bool {
    if candidate.kind != "method" && candidate.kind != "function" {
        return false;
    }
    let qn = &candidate.qualified_name;
    if !qn.contains("::") {
        return false;
    }
    let want = format!("{}::{}", type_prefix, candidate.name);
    *qn == want || qn.ends_with(&format!("::{}", want)) || want.ends_with(&format!("::{}", qn))
}

/// pathProximityFromDirs + computePathProximity (name-matcher.ts).
pub(super) fn path_proximity_from_dirs(dir1: &[String], file_path2: &str) -> i64 {
    (shared_dir_prefix(dir1, file_path2) as i64 * 15).min(80)
}

pub(super) fn compute_path_proximity(file_path1: &str, file_path2: &str) -> i64 {
    let mut dir1: Vec<String> = file_path1.split('/').map(|s| s.to_string()).collect();
    dir1.pop();
    path_proximity_from_dirs(&dir1, file_path2)
}
