//! resolveOneInner's non-bare slices, the gates, and the per-ref pipeline.

use super::*;

impl KernelResolver {
    // -----------------------------------------------------------------------
    // Non-bare c/cpp `imports` refs — the #include-path slice (resolveOneInner
    // restricted). Measured on the linux corpus this leg resolves ~388k of
    // the ~393k ineligible:name edges; everything it can't prove punts to the
    // full TS spine rather than guessing.
    // -----------------------------------------------------------------------

    /// The resolveOneInner ordering for `(c|cpp, 'imports', non-bare)`:
    /// builtin → prefilter → frameworks → boundReceiver → viaImport →
    /// nameMatch. For this slice jvmImport/razor/phpStatic are
    /// language-gated dead, boundReceiver is calls-gated dead, the chain
    /// guards need calls + `().`, and viaImport's first branch IS the c/cpp
    /// include arm (≥0.9 or nothing — its <0.9 candidate path can't fire).
    /// The nameMatch tail (qualifiedName → cppChain → methodCall →
    /// exactName → fuzzy) stays in TS: a `member-tail` passthrough
    /// reproduces the full-spine verdict exactly.
    pub(super) fn resolve_c_include_import_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        let pre_pass = self.has_any_possible_match(&r.reference_name)
            || self.matches_any_import(r)?
            || self.framework_claims(&r.reference_name);
        if !pre_pass {
            // The prefilter-miss fallback is matchJsStoreBindingCall — gated
            // to JS calls — so a c/cpp ref dies here exactly as in TS.
            return Ok(ResolveOutcome::unresolved());
        }
        let import_hit = probe!(r, "resolve_via_import", self.resolve_via_import(r)?);
        let import_hit = self.gate_language(import_hit, r);
        if let Some(c) = import_hit {
            let winner = match self.gate_target_kind(c, r)? {
                Some(w) => w,
                None => {
                    // A gated-out ≥0.9 import can still lose to a ≥0.9
                    // framework hit — only the full TS spine distinguishes.
                    return Ok(self.gated_import());
                }
            };
            return self.finish(r, winner, None, true);
        }
        // matchReference's name arms in TS order: filePath, qualifiedName
        // on a file-path miss (`#include <sys/ioctl.h>` names its own import
        // node's qualified name — filePath can't see it, qualifiedName can),
        // then methodCall's unbound arm (an `a.h` name is a dot-shape
        // receiver). The chain arms can't fire — include names never carry
        // `()` — so they are skipped; exactName/fuzzy stay in TS.
        let mut name_cand = self.match_by_file_path(r)?;
        if name_cand.is_none() {
            name_cand = self.match_by_qualified_name(r)?;
        }
        if name_cand.is_none() {
            name_cand = self.match_method_call_free(r)?;
        }
        let Some(c) = self.gate_language(name_cand, r) else {
            return Ok(ResolveOutcome::passthrough("member-tail"));
        };
        // The nameMatch result takes the post-checks; a rejection leaves the
        // later arms live in TS, so punt — never verdict.
        if !self.name_result_stands(&c, r)? {
            return Ok(ResolveOutcome::passthrough("member-tail"));
        }
        let Some(winner) = self.gate_target_kind(c, r)? else {
            return Ok(ResolveOutcome::passthrough("member-tail"));
        };
        // A file-path hit is a nameMatch candidate — it never early-returns
        // in TS, it first-maxes against framework candidates. Under active
        // frameworks report it for the merge instead of verdicting.
        if self.frameworks_active {
            let reported = vec![KernelCandidateOut::from(&winner)];
            return self.finish(r, winner, Some(reported), false);
        }
        self.finish(r, winner, None, false)
    }

    /// The `$`-receiver guard both phpStatic and boundReceiver share:
    /// `getFileLines(file)?.[line-1]?.slice(column).startsWith('$')`.
    pub(super) fn ref_line_starts_with_dollar(&mut self, r: &ResolveRefIn) -> bool {
        let Some(lines) = self.read_file(&r.file_path) else {
            return false;
        };
        let Some(line) = (r.line - 1)
            .try_into()
            .ok()
            .and_then(|i: usize| lines.get(i))
        else {
            return false;
        };
        js_slice(line, r.column.max(0) as usize).starts_with('$')
    }

    /// resolvePhpImportedStaticCall (import-resolver.ts): `Alias.method()`
    /// where `Alias` is a PHP class import — resolves to the imported class's
    /// qualified member. Returns None when the arm does not claim the ref;
    /// Some carries the terminal outcome (this arm precedes frameworks).
    pub(super) fn resolve_php_imported_static(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.language != "php" || r.reference_kind != "calls" {
            return Ok(None);
        }
        let Some(call) = php_static_call_re().captures(&r.reference_name) else {
            return Ok(None);
        };
        let receiver = call.get(1).unwrap().as_str();
        let member = call.get(2).unwrap().as_str();
        let imports = self.import_mappings(&r.file_path)?;
        let Some(imp) = imports.iter().find(|i| i.local_name == receiver) else {
            return Ok(None);
        };
        let imp_source = imp.source.clone();
        if self.ref_line_starts_with_dollar(r) {
            return Ok(None);
        }
        let fqn = imp_source.trim_start_matches('\\');
        let type_name = match fqn.rfind('\\') {
            Some(sep) => format!("{}::{}", &fqn[..sep], &fqn[sep + 1..]),
            None => fqn.to_string(),
        };
        let owners: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&type_name)?
            .iter()
            .filter(|n| n.language == "php" && is_static_member_container(&n.kind))
            .cloned()
            .collect();
        // Claimed: a single owner is required; ambiguity or absence is a
        // terminal refusal, never a name-match fallthrough.
        if owners.len() != 1 {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let owner = owners[0].clone();
        let member_qn = format!("{}::{}", owner.qualified_name, member);
        let methods: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&member_qn)?
            .iter()
            .filter(|n| {
                n.language == "php" && n.kind == "method" && n.file_path == owner.file_path
            })
            .cloned()
            .collect();
        if methods.len() != 1 {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let gated = self.gate_language(
            Some(KCand {
                node: methods[0].clone(),
                confidence: 0.95,
                resolved_by: "import",
            }),
            r,
        );
        let Some(cand) = gated else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        // gateTargetKind is a no-op for `calls` refs (imports/inheritance arms
        // only); the alias forward applies inside finish.
        self.finish(r, cand, None, true).map(Some)
    }

    /// resolveOneInner for non-bare refs in migrated languages. Ported arms
    /// run in TS order; every unported arm punts so the TS spine re-derives
    /// the outcome. A bound-receiver claim is terminal — a refusal never
    /// falls through to name matching.
    pub(super) fn resolve_nonbare_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        // Prefilter — `existenceName` strips arkts' leading '.';
        // matchJsStoreBindingCall needs a dot-free name, so a non-bare
        // `Foo::bar` can still reach it — punt when the source check applies.
        let existence = if r.language == "arkts" && r.reference_name.starts_with('.') {
            &r.reference_name[1..]
        } else {
            &r.reference_name
        };
        let pre_pass = probe!(r, "pre-pass", self.has_any_possible_match(existence)
            || self.matches_any_import(r)?
            || self.framework_claims(&r.reference_name));
        if !pre_pass {
            if self.is_bare_js_call(r)? {
                return Ok(ResolveOutcome::passthrough("store-bind"));
            }
            return Ok(ResolveOutcome::unresolved());
        }

        // `function_ref` refs resolve ONLY through matchFunctionRef, never
        // the fallthrough below — TS's function_ref block for non-bare
        // names, in order: viaImport (a `.` member-descent can still claim
        // an `a.b` function_ref), then the `::` member-pointer arm (the
        // only non-bare shape matchFunctionRef resolves; `.`/`this.` forms
        // always miss in it). A miss punts back to that same block.
        if r.reference_kind == "function_ref" {
            match self.resolve_via_import_member(r)? {
                None => {}
                Some(c) => {
                    // An import resolving to a non-callable is discarded —
                    // the scoped arm still runs, exactly like the bare path.
                    if let Some(c) = self.gate_language(Some(c), r) {
                        if c.node.kind == "function"
                            || c.node.kind == "method"
                            || (r.language == "python" && c.node.kind == "class")
                        {
                            return self.finish(r, c, None, true);
                        }
                    }
                }
            }
            if let Some(c) = self.match_function_ref_scoped(r)? {
                // Frameworks never run on this path — a gated candidate is
                // discarded to terminal unresolved, exactly like the bare arm.
                return match self.gate_language(Some(c), r) {
                    Some(c) => self.finish(r, c, None, true),
                    None => Ok(ResolveOutcome::unresolved()),
                };
            }
            return Ok(ResolveOutcome::passthrough("member-tail"));
        }
        // resolveJvmImport (java/kotlin `imports` refs) reads decorators —
        // an unselected column; the arm stays in TS.
        if r.reference_kind == "imports"
            && (r.language == "java" || r.language == "kotlin")
        {
            return Ok(ResolveOutcome::passthrough("jvm"));
        }
        // PHP `imports` refs take the include-path arm inside resolveViaImport
        // (unported — include-path file resolution) — punt the whole kind.
        if r.language == "php" && r.reference_kind == "imports" {
            return Ok(ResolveOutcome::passthrough("php-inc"));
        }
        // resolvePhpImportedStaticCall — terminal before frameworks.
        if let Some(outcome) = self.resolve_php_imported_static(r)? {
            return Ok(outcome);
        }
        // matchBoundReceiverCall — claimed refs are terminal either way.
        if is_binding_receiver_call(r) {
            match probe!(r, "bound_receiver_claim", self.bound_receiver_claim(r)?) {
                None => {
                    return Ok(self.refused());
                }
                Some(c) => {
                    let gated = self.gate_language(Some(c), r);
                    return match gated {
                        Some(cand) => {
                            let Some(winner) = self.gate_target_kind(cand, r)? else {
                                return Ok(self.refused());
                            };
                            if self.frameworks_active {
                                // Framework <0.9 candidates merge first-max;
                                // a ≥0.9 framework hit would have pre-empted.
                                let reported = vec![KernelCandidateOut::from(&winner)];
                                self.finish(r, winner, Some(reported), false)
                            } else {
                                self.finish(r, winner, None, true)
                            }
                        }
                        None => Ok(self.refused()),
                    };
                }
            }
        }
        // isUnresolvedJsMemberCall — terminal null; framework candidates are
        // discarded by the TS `return null` as well.
        if is_unresolved_js_member_call(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        // The chain guard routes `x().y` calls through matchReference only —
        // the chain matchers there (storeAccessorChain et al.) are unported.
        if r.reference_kind == "calls"
            && chain_shape_re().is_match(&r.reference_name)
            && matches!(
                r.language.as_str(),
                "typescript" | "javascript" | "tsx" | "jsx" | "python"
            )
        {
            return Ok(ResolveOutcome::passthrough("chain"));
        }

        let mut cands: Vec<KCand> = Vec::new();
        match self.resolve_via_import_member(r)? {
            None => {}
            Some(c) => {
                if let Some(c) = self.gate_language(Some(c), r) {
                    if c.confidence >= 0.9 {
                        let Some(winner) = self.gate_target_kind(c, r)? else {
                            return Ok(self.gated_import());
                        };
                        return self.finish(r, winner, None, true);
                    }
                    cands.push(c);
                }
            }
        }
        // isPhpIncludePathRef / cobol / nix / terraform terminal-merge arm:
        // php imports punt above; the rest are unmigrated languages.

        // matchReference's leading arkts arm — `.attr` names resolve ONLY to
        // decorator-marked helpers, never the name-match fallthrough.
        if r.language == "arkts" && r.reference_name.starts_with('.') {
            return Ok(ResolveOutcome::passthrough("arkts-dot"));
        }
        // matchReference's ported arms, in order: filePath, qualifiedName,
        // the per-language chain arm (cppChain/scopedChain/dottedChain), then
        // methodCall's requireReceiverEvidence=false arm. Everything after
        // (exactName/fuzzy, then deferred drains) stays in TS behind the
        // member-tail punt.
        let mut name_cand = self.match_by_file_path(r)?;
        if name_cand.is_none() {
            name_cand = self.match_by_qualified_name(r)?;
        }
        if name_cand.is_none() {
            name_cand = self.match_call_chain(r)?;
        }
        if name_cand.is_none() {
            name_cand = self.match_method_call_free(r)?;
        }
        if let Some(c) = self.gate_language(name_cand, r) {
            if self.name_result_stands(&c, r)? {
                cands.push(c);
            }
        }
        if cands.is_empty() {
            // nameMatch's remaining arms may still hit — the ref goes back.
            return Ok(ResolveOutcome::passthrough("member-tail"));
        }
        self.settle(r, cands)
    }

    // -----------------------------------------------------------------------
    // Gates (index.ts) + alias forwarding (alias-binding.ts)
    // -----------------------------------------------------------------------

    /// gateLanguage (index.ts): drop import/name results crossing a family.
    pub(super) fn gate_language(&self, cand: Option<KCand>, r: &ResolveRefIn) -> Option<KCand> {
        let cand = cand?;
        let tgt = cand.node.language.as_str();
        // getLanguageFromNodeId is the node's language — already in hand.
        if tgt == "markdown" || r.language == "markdown" {
            return Some(cand);
        }
        if (r.reference_kind == "references" || r.reference_kind == "function_ref")
            && !same_language_family(tgt, &r.language)
        {
            return None;
        }
        if r.reference_kind == "imports" && crosses_known_family(tgt, &r.language) {
            return None;
        }
        if crosses_code_boundary(tgt, &r.language) {
            return None;
        }
        Some(cand)
    }

    /// gateTargetKind (index.ts): the imports/inheritance target-kind gates
    /// plus the out-of-repo import check.
    pub(super) fn gate_target_kind(&mut self, cand: KCand, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.reference_kind == "imports" {
            return Ok(if is_importable_kind(&cand.node.kind) {
                Some(cand)
            } else {
                None
            });
        }
        if !is_inheritance_ref(&r.reference_kind) {
            return Ok(Some(cand));
        }
        if !is_supertype_target_kind(&cand.node.kind) {
            return Ok(None);
        }
        if self.is_bound_to_out_of_repo_import(r)? {
            return Ok(None);
        }
        Ok(Some(cand))
    }

    /// aliasTargetName + resolveAliasBinding (alias-binding.ts). `member_name`
    /// is the ref's last `.` segment (null for bare/`::`-only names): a
    /// member access through an object-literal alias resolves the keyed or
    /// shorthand property binding (`{ member: fn }` / `{ member }`).
    pub(super) fn resolve_alias_binding(
        &mut self,
        alias_node: &KNode,
        member_name: Option<&str>,
    ) -> Res<Option<Arc<KNode>>> {
        if !is_alias_binding_kind(&alias_node.kind) {
            return Ok(None);
        }
        let sig = alias_node.signature.as_deref().unwrap_or("").trim();
        let target_name = match member_name {
            Some(m) if !m.is_empty() => {
                // `[{,]\s*KEY\s*:\s*(target)\s*[,}]`, else the shorthand `[{,]\s*KEY\s*[,}]`.
                static EXPLICIT: LazyLock<Affix> = LazyLock::new(|| {
                    Affix::new(r"[{,]\s*", r"\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[,}]", false, false, false)
                });
                static SHORTHAND: LazyLock<Affix> =
                    LazyLock::new(|| Affix::new(r"[{,]\s*", r"\s*[,}]", false, false, false));
                match EXPLICIT.capture(sig, m) {
                    Some(t) => Some(t.to_string()),
                    None => SHORTHAND.is_match(sig, m).then(|| m.to_string()),
                }
            }
            _ => bare_alias_re()
                .captures(sig)
                .map(|c| c.get(1).unwrap().as_str().to_string()),
        };
        let Some(target_name) = target_name else { return Ok(None) };
        if target_name == alias_node.name {
            return Ok(None);
        }
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(&target_name)?
            .iter()
            .filter(|n| is_callable_kind(&n.kind))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        let same_file: Vec<&Arc<KNode>> = candidates
            .iter()
            .filter(|n| n.file_path == alias_node.file_path)
            .collect();
        if same_file.len() == 1 {
            return Ok(Some(same_file[0].clone()));
        }
        if same_file.len() > 1 {
            return Ok(None);
        }
        Ok(if candidates.len() == 1 {
            Some(candidates[0].clone())
        } else {
            None
        })
    }

    // -----------------------------------------------------------------------
    // The pipeline — resolveOneInner restricted to migrated+bare refs, then
    // resolveOne's gateTargetKind + calls alias-forward.
    // -----------------------------------------------------------------------

    /// Necessary condition for matchJsStoreBindingCall (name-matcher.ts):
    /// both store-binding arms bind the ref's own name through `const` — a
    /// destructure (`const {a} = X.getState()`) or a selector alias
    /// (`const a = useStore(s => s.a)`). Absent that shape the matcher cannot
    /// fire, so the kernel may adjudicate the ref itself. The whole file is
    /// scanned (the destructure can span lines); false positives only cost a
    /// TS fallback, never a wrong verdict.
    pub(super) fn file_could_store_bind(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.reference_kind != "calls" || !is_js_family(&r.language) {
            return Ok(false);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        Ok(js_const_binds(lines.text(), &r.reference_name))
    }

    /// The `function_ref` block of resolveOneInner (index.ts). TS order:
    /// prefilter → this.-member arm (non-bare, unreachable here) →
    /// resolveViaImport with a target-kind gate → matchFunctionRef. A
    /// prefilter miss routes to matchJsStoreBindingCall, which is
    /// `calls`-gated — dead for function_ref — and frameworks never run on
    /// this path, so the miss is terminal either way.
    pub(super) fn resolve_function_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        let pre_pass = probe!(r, "pre-pass",
            self.has_any_possible_match(&r.reference_name) || self.matches_any_import(r)?);
        if !pre_pass {
            return Ok(ResolveOutcome::unresolved());
        }
        // An import hit resolves only when its target is a callable value —
        // function/method, or a Python class (bareClassOk). A gated-out
        // import is discarded, not pooled, before the name matcher runs.
        let import_cand = probe!(r, "resolve_via_import", self.resolve_via_import(r)?);
        let import_result = self.gate_language(import_cand, r);
        if let Some(c) = import_result {
            if c.node.kind == "function"
                || c.node.kind == "method"
                || (r.language == "python" && c.node.kind == "class")
            {
                return self.finish(r, c, None, true);
            }
        }
        let name_cand = self.match_function_ref_bare(r)?;
        match self.gate_language(name_cand, r) {
            Some(c) => self.finish(r, c, None, true),
            None => Ok(ResolveOutcome::unresolved()),
        }
    }

    /// The per-ref pipeline: the Rust `::`-path arm, then one route.
    pub(super) fn resolve_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        // Rust pure-`::` path refs (`crate::m::Item`, `a::b::c`): TS
        // resolves them through resolveViaImport's module-file arm, which
        // needs no bindings rows — run it ahead of the eligibility gate.
        // A miss falls through for migrated rust (qualified-name/exact arms
        // mirror matchReference's continuation) and punts otherwise.
        if is_rust_path_ref(r) {
            match self.resolve_rust_path_ref(r)? {
                Some(o) => return Ok(o),
                None if is_migrated_language(&r.language) => {}
                None => return Ok(ResolveOutcome::passthrough("ineligible:lang")),
            }
        }
        match route(r) {
            Route::Passthrough(reason) => Ok(ResolveOutcome::passthrough(reason)),
            Route::CInclude => self.resolve_c_include_import_ref(r),
            // Member-access slice (§5.16): boundReceiver's DB sub-arms, the
            // import member descent, filePath and qualifiedName — the rest
            // of the member matchers punt back to the TS spine.
            Route::NonBare => probe!(r, "resolve_nonbare_ref", self.resolve_nonbare_ref(r)),
            Route::Bare => self.resolve_bare_ref(r),
        }
    }

    /// resolveOneInner's bare slice (a name with no separator).
    pub(super) fn resolve_bare_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        // resolveOneInner, bare slice:
        //   builtin/external → CFML/jvm/razor/phpStatic arms all dead →
        //   prefilter → frameworks (TS) → boundReceiver (dead) → chain guard
        //   (dead) → viaImport → name-match → post-checks → first-max.
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        // The store-binding matcher stays in TS (source-reading): it can fire
        // on a prefilter miss AND short-circuits matchByExactName's candidate
        // list, so a JS bare call whose file const-binds its name must
        // passthrough wherever it would otherwise settle.
        if probe!(r, "bare-js-call", self.is_bare_js_call(r)?) && probe!(r, "store-bind", self.file_could_store_bind(r)?) {
            return Ok(ResolveOutcome::passthrough("store-bind"));
        }
        // `function_ref` (#756) has a dedicated, strictly-gated TS path that
        // never reaches frameworks or the fuzzy matchers — resolve it here
        // the same way (import, then the name-matcher's bare arm). The
        // `Cls::member` and `this.member` shapes are non-bare and stay on
        // the TS side via the eligibility gate.
        if r.reference_kind == "function_ref" {
            return self.resolve_function_ref(r);
        }
        // Rust bare `Self` — a references/instantiates/calls ref naming the
        // enclosing impl's type. Binds to the concrete owner off the
        // caller's qualified name; a trait-kind owner is the abstract
        // implementor and declines. Advisory: a miss keeps the ref's normal
        // bare-name verdict.
        if r.language == "rust" && r.reference_name == "Self" {
            if let Some(c) = self.match_rust_bare_self(r)? {
                return self.finish(r, c, None, true);
            }
        }
        // nix-path/arkts-dot/erlang-arity arms are dead for migrated bare
        // names; the claimsReference arm is evaluated natively — a claimed
        // name still reaches the framework resolvers through the TS path.
        let pre_pass = probe!(r, "pre-pass",
            self.has_any_possible_match(&r.reference_name) || self.matches_any_import(r)?);
        if !pre_pass {
            return Ok(if self.framework_claims(&r.reference_name) {
                ResolveOutcome::passthrough("claimed")
            } else {
                ResolveOutcome::unresolved()
            });
        }

        let mut cands: Vec<KCand> = Vec::new();
        let import_cand = probe!(r, "resolve_via_import", self.resolve_via_import(r)?);
        let import_result = self.gate_language(import_cand, r);
        let mut final_import: Option<KCand> = None;
        if let Some(c) = import_result {
            if c.confidence >= 0.9 {
                final_import = Some(c);
            } else {
                cands.push(c);
            }
        }

        if let Some(c) = final_import {
            // ≥0.9 import wins over everything below (and over framework
            // candidates < 0.9; a ≥0.9 framework hit still displaces it on
            // the TS side when frameworks are active).
            let winner = match self.gate_target_kind(c, r)? {
                Some(w) => w,
                None => {
                    // A gated-out ≥0.9 import resolves to nothing in the TS
                    // spine — its <0.9 framework candidates were discarded by
                    // the early return — but a ≥0.9 framework hit would have
                    // pre-empted the import entirely. Only the full TS spine
                    // can distinguish; hand it back when frameworks are live.
                    return Ok(self.gated_import());
                }
            };
            return self.finish(r, winner, None, true);
        }

        let name_cand = probe!(r, "match_reference_bare", self.match_reference_bare(r)?);
        if let Some(c) = self.gate_language(name_cand, r) {
            if self.name_result_stands(&c, r)? {
                cands.push(c);
            }
        }

        if cands.is_empty() {
            // The chain/php-prop defer arms need `().`/`this->` — dead.
            return Ok(self.refused());
        }
        // Candidate order is [import?, name?] — see settle.
        self.settle(r, cands)
    }

    /// Apply resolveOne's tail — the `calls` alias forward — and emit the
    /// outcome. `candidates` is the reported kernel list (original order)
    /// for the TS framework merge when frameworks are active.
    pub(super) fn finish(
        &mut self,
        r: &ResolveRefIn,
        winner: KCand,
        candidates: Option<Vec<KernelCandidateOut>>,
        is_final: bool,
    ) -> Res<ResolveOutcome> {
        let mut winner = winner;
        if r.reference_kind == "calls" {
            // memberName = the last `.` segment — `Cls::member` and bare
            // names carry none (no '.' → bare arm); `a.b::c` yields `b::c`,
            // matching lastIndexOf('.') + slice.
            let member_name = r
                .reference_name
                .rfind('.')
                .map(|i| &r.reference_name[i + 1..]);
            if let Some(forwarded) = self.resolve_alias_binding(&winner.node, member_name)? {
                if forwarded.id != winner.node.id {
                    winner = KCand {
                        node: forwarded,
                        confidence: winner.confidence.min(0.85),
                        resolved_by: winner.resolved_by,
                    };
                }
            }
        }
        Ok(ResolveOutcome::resolved(
            &winner.node,
            winner.confidence,
            winner.resolved_by,
            is_final,
            candidates,
        ))
    }

    /// First-max on strict `>` over `cands` (non-empty), the TS
    /// candidates.reduce, then the target-kind gate and `finish`. Under active
    /// frameworks the reported list keeps the ORIGINAL candidate order so the
    /// TS merge can re-run the reduce with framework candidates prepended; a
    /// gated-out winner still reports it, since a framework candidate may win
    /// the merged first-max on the TS side.
    pub(super) fn settle(&mut self, r: &ResolveRefIn, mut cands: Vec<KCand>) -> Res<ResolveOutcome> {
        let reported = self
            .frameworks_active
            .then(|| cands.iter().map(KernelCandidateOut::from).collect::<Vec<_>>());
        let mut bi = 0usize;
        for i in 1..cands.len() {
            if cands[i].confidence > cands[bi].confidence {
                bi = i;
            }
        }
        let winner = cands.remove(bi);
        match self.gate_target_kind(winner, r)? {
            Some(w) => self.finish(r, w, reported, false),
            None => Ok(ResolveOutcome { candidates: reported, ..ResolveOutcome::unresolved() }),
        }
    }

    /// resolveOneInner's post-checks on a name match: a definition its
    /// language keeps file-local cannot be what another file names (#1730),
    /// and nothing calls into a Nix binding symbolically. A rejection never
    /// promotes a runner-up (#1745).
    pub(super) fn name_result_stands(&mut self, c: &KCand, r: &ResolveRefIn) -> Res<bool> {
        Ok(c.node.language != "nix" && self.is_visible_across_files(&c.node, r)?)
    }

    /// No kernel verdict: framework candidates may still exist on the TS
    /// side, so report an empty list when frameworks are active.
    pub(super) fn refused(&self) -> ResolveOutcome {
        if self.frameworks_active {
            ResolveOutcome::no_candidates()
        } else {
            ResolveOutcome::unresolved()
        }
    }

    /// A gated-out ≥0.9 import: only the full TS spine can tell whether a
    /// ≥0.9 framework hit would have pre-empted it, so hand it back when
    /// frameworks are live.
    pub(super) fn gated_import(&self) -> ResolveOutcome {
        if self.frameworks_active {
            ResolveOutcome::passthrough("gated-import")
        } else {
            ResolveOutcome::unresolved()
        }
    }
}

/// Rust pure-`::` path refs take resolveRustPathReference ahead of every gate.
/// Dot-bearing `::` names stay out (the boundReceiver claim can own an
/// `a::b.c` receiver in TS), and `function_ref` keeps its own block — a
/// module-path hit on a non-callable leaf is DISCARDED there, not returned.
fn is_rust_path_ref(r: &ResolveRefIn) -> bool {
    r.language == "rust"
        && r.reference_kind != "function_ref"
        && r.reference_name.contains("::")
        && !r.reference_name.contains('.')
}

/// Where a ref goes once the Rust path arm has missed.
enum Route {
    /// Back to the TS spine, with the reason for the profile.
    Passthrough(&'static str),
    /// C/C++ `#include` path refs — the measured-dominant slice of the
    /// non-bare tail (§5.14).
    CInclude,
    NonBare,
    Bare,
}

/// ref_is_eligible, split so a passthrough names its gate.
fn route(r: &ResolveRefIn) -> Route {
    if !is_migrated_language(&r.language) {
        return Route::Passthrough("ineligible:lang");
    }
    if !name_is_bare(&r.reference_name) {
        if (r.language == "c" || r.language == "cpp") && r.reference_kind == "imports" {
            return Route::CInclude;
        }
        // Rust dotted receivers: `calls` names without `::`/`()` ride the
        // ported pipeline — inferLocalReceiverType (`let ctx: Ctx`) is
        // native for rust now, and the self.-arms/strategies that follow
        // reproduce TS's tail (§5.25's confidence-drift class is exactly
        // what the inference arm resolves natively). `a::b.c`/`x::y().z`
        // (::+.), `x().y`, and non-call `x.y`/`self.x` stay punted — TS
        // verdicts by delegation.
        if r.language == "rust"
            && r.reference_name.contains('.')
            && !(r.reference_kind == "calls"
                && (r.reference_name.starts_with("self.")
                    || (!r.reference_name.contains("::") && !r.reference_name.contains("()"))))
        {
            return Route::Passthrough("member-tail");
        }
        return Route::NonBare;
    }
    if r.file_path.is_empty() {
        return Route::Passthrough("ineligible:path");
    }
    Route::Bare
}

/// is_bare_name: the eligibility shape — no separator any skipped
/// strategy keys on. Leading `$` stays (arkts `$r` builtins are handled);
/// a `$` elsewhere could feed the R method pattern's `[\w.]+\$` arm.
pub(super) fn name_is_bare(name: &str) -> bool {
    !name.is_empty()
        && !name
            .char_indices()
            .any(|(i, c)| matches!(c, '.' | ':' | '/' | '\\' | '#' | '(' | ')') || (c == '$' && i > 0))
}
