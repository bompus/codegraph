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
    pub(super) fn resolve_c_include_import_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
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
        let name_cand = match self.match_by_file_path(r)? {
            Some(c) => Some(c),
            None => match self.match_by_qualified_name(r)? {
                Some(c) => Some(c),
                None => match self.match_method_call_free(r)? {
                    McRes::Hit(c) => Some(c),
                    McRes::Punt(p) => return Ok(ResolveOutcome::passthrough(p)),
                    McRes::Null => None,
                },
            },
        };
        let Some(c) = self.gate_language(name_cand, r) else {
            return Ok(ResolveOutcome::passthrough("member-tail"));
        };
        // The nameMatch result takes the cross-file visibility post-check; a
        // rejection leaves the later arms live in TS, so punt — never verdict.
        if !self.is_visible_across_files(&c.node, r)? {
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

    /// matchByFilePath (name-matcher.ts): path-shaped (`a/b.h`) or
    /// extension-bearing bare (`Foo.h`) names → `file` nodes.
    pub(super) fn match_by_file_path(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let normalized = r.reference_name.replace('\\', "/");
        let (path_and_symbol, anchor) = split_anchor(&normalized);
        let (path_wo_anchor, symbol_name) = split_file_symbol(path_and_symbol);
        if !path_wo_anchor.contains('/') && !thread_regex(&FILE_PATH_EXT_RE).is_match(path_wo_anchor) {
            return Ok(None);
        }
        let file_name = pos_basename(path_wo_anchor);
        if file_name.is_empty() {
            return Ok(None);
        }
        let file_nodes: Vec<Arc<KNode>> = self
            .nodes_by_name(file_name)?
            .iter()
            .filter(|n| n.kind == "file")
            .cloned()
            .collect();
        if file_nodes.is_empty() {
            return Ok(None);
        }
        if let Some(symbol_name) = symbol_name.filter(|s| !s.is_empty()) {
            if let Some(symbol) =
                self.find_symbol_in_referenced_file(path_wo_anchor, symbol_name, &file_nodes)?
            {
                return Ok(Some(KCand {
                    node: symbol,
                    confidence: 0.99,
                    resolved_by: "file-path",
                }));
            }
        }
        if let Some(anchor) = anchor.filter(|a| !a.is_empty()) {
            if let Some(anchored) =
                self.find_anchored_markdown_section(path_wo_anchor, anchor, &file_nodes)?
            {
                return Ok(Some(KCand {
                    node: anchored,
                    confidence: 0.98,
                    resolved_by: "file-path",
                }));
            }
        }
        if let Some(exact) = file_nodes
            .iter()
            .find(|n| n.qualified_name == path_wo_anchor || n.file_path == path_wo_anchor)
        {
            return Ok(Some(KCand {
                node: exact.clone(),
                confidence: 0.95,
                resolved_by: "file-path",
            }));
        }
        let suffix_matches: Vec<Arc<KNode>> = file_nodes
            .iter()
            .filter(|n| {
                n.qualified_name.ends_with(path_wo_anchor) || n.file_path.ends_with(path_wo_anchor)
            })
            .cloned()
            .collect();
        if !suffix_matches.is_empty() {
            return Ok(Some(KCand {
                node: pick_closest_file_node(&suffix_matches, r),
                confidence: 0.85,
                resolved_by: "file-path",
            }));
        }
        if file_nodes.len() == 1 {
            return Ok(Some(KCand {
                node: file_nodes[0].clone(),
                confidence: 0.7,
                resolved_by: "file-path",
            }));
        }
        Ok(None)
    }

    /// findSymbolInReferencedFile (name-matcher.ts): `path::symbol` — search
    /// the path-matched files (or the lone candidate) for the symbol.
    pub(super) fn find_symbol_in_referenced_file(
        &mut self,
        path_wo_anchor: &str,
        symbol_name: &str,
        file_nodes: &[Arc<KNode>],
    ) -> Result<Option<Arc<KNode>>> {
        let candidate_files: Vec<Arc<KNode>> = file_nodes
            .iter()
            .filter(|n| {
                n.qualified_name == path_wo_anchor
                    || n.file_path == path_wo_anchor
                    || n.qualified_name.ends_with(path_wo_anchor)
                    || n.file_path.ends_with(path_wo_anchor)
            })
            .cloned()
            .collect();
        let search: &[Arc<KNode>] = if !candidate_files.is_empty() {
            &candidate_files
        } else if file_nodes.len() == 1 {
            file_nodes
        } else {
            &[]
        };
        for file_node in search {
            let nodes = self.nodes_in_file(&file_node.file_path)?;
            let normalized_symbol = symbol_name.replace('/', ".");
            let file_prefix = format!("{}::{}", file_node.file_path, normalized_symbol);
            let colon_tail = format!("::{}", normalized_symbol);
            let dot_tail = format!(".{}", normalized_symbol);
            if let Some(exact) = nodes.iter().find(|n| {
                n.name == normalized_symbol
                    || n.qualified_name == file_prefix
                    || n.qualified_name.ends_with(&colon_tail)
                    || n.qualified_name.ends_with(&dot_tail)
            }) {
                return Ok(Some(exact.clone()));
            }
            let last_part = normalized_symbol.split(['.', ':']).next_back().unwrap_or("");
            if last_part.is_empty() {
                continue;
            }
            if let Some(by_last) = nodes.iter().find(|n| {
                n.name == last_part
                    && matches!(
                        n.kind.as_str(),
                        "function" | "method" | "class" | "module" | "constant" | "variable"
                    )
            }) {
                return Ok(Some(by_last.clone()));
            }
        }
        Ok(None)
    }

    /// findAnchoredMarkdownSection (name-matcher.ts): `path#anchor` → the
    /// markdown section module node.
    pub(super) fn find_anchored_markdown_section(
        &mut self,
        path_wo_anchor: &str,
        anchor: &str,
        file_nodes: &[Arc<KNode>],
    ) -> Result<Option<Arc<KNode>>> {
        let normalized_anchor = normalize_markdown_anchor(anchor);
        for file_node in file_nodes.iter().filter(|n| {
            n.qualified_name == path_wo_anchor
                || n.file_path == path_wo_anchor
                || n.qualified_name.ends_with(path_wo_anchor)
                || n.file_path.ends_with(path_wo_anchor)
        }) {
            let section_qn = format!("{}#{}", file_node.file_path, normalized_anchor);
            if let Some(exact) = self
                .nodes_by_qualified_name(&section_qn)?
                .iter()
                .find(|n| n.kind == "module" && n.language == "markdown")
            {
                return Ok(Some(exact.clone()));
            }
            if let Some(section) = self
                .nodes_in_file(&file_node.file_path)?
                .iter()
                .find(|n| {
                    n.kind == "module"
                        && n.language == "markdown"
                        && n.qualified_name == section_qn
                })
            {
                return Ok(Some(section.clone()));
            }
        }
        Ok(None)
    }

    // -----------------------------------------------------------------------
    // Non-bare migrated refs — resolveOneInner's member slice (§5.16).
    // Ported arms: builtin/external, prefilter, phpStatic, boundReceiver's
    // DB sub-arms (br:import + claim refusals), viaImport's member descent
    // (static member + object literal), filePath, qualifiedName. Arms that
    // read source (receiver-type inference, object-literal alias/instance
    // member, store bindings) or defer (chains, this.members) punt so the TS
    // spine reproduces them exactly.
    // -----------------------------------------------------------------------

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
        Self::js_slice(line, r.column.max(0) as usize).starts_with('$')
    }

    /// resolvePhpImportedStaticCall (import-resolver.ts): `Alias.method()`
    /// where `Alias` is a PHP class import — resolves to the imported class's
    /// qualified member. Returns None when the arm does not claim the ref;
    /// Some carries the terminal outcome (this arm precedes frameworks).
    pub(super) fn resolve_php_imported_static(&mut self, r: &ResolveRefIn) -> Result<Option<ResolveOutcome>> {
        if r.language != "php" || r.reference_kind != "calls" {
            return Ok(None);
        }
        let Some(call) = thread_regex(&PHP_STATIC_CALL_RE).captures(&r.reference_name) else {
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

    /// resolveObjectLiteralMember (name-matcher.ts): an imported object
    /// literal used as a namespace — find the member by containment.
    pub(super) fn resolve_object_literal_member(
        &mut self,
        container: &KNode,
        member: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
    ) -> Result<Option<KCand>> {
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
    pub(super) fn match_by_qualified_name(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
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

    /// resolveOneInner for non-bare refs in migrated languages. Ported arms
    /// run in TS order; every unported arm punts so the TS spine re-derives
    /// the outcome. A bound-receiver claim is terminal — a refusal never
    /// falls through to name matching.
    pub(super) fn resolve_nonbare_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
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
                ViaImport::Punt(reason) => {
                    return Ok(ResolveOutcome::passthrough(reason));
                }
                ViaImport::Miss => {}
                ViaImport::Hit(c) => {
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
                BoundClaim::Punt(reason) => {
                    return Ok(ResolveOutcome::passthrough(reason));
                }
                BoundClaim::Refused => {
                    return Ok(self.refused());
                }
                BoundClaim::Hit(c) => {
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
            && thread_regex(&CHAIN_SHAPE_RE).is_match(&r.reference_name)
            && matches!(
                r.language.as_str(),
                "typescript" | "javascript" | "tsx" | "jsx" | "python"
            )
        {
            return Ok(ResolveOutcome::passthrough("chain"));
        }

        let mut cands: Vec<KCand> = Vec::new();
        match self.resolve_via_import_member(r)? {
            ViaImport::Punt(reason) => {
                return Ok(ResolveOutcome::passthrough(reason));
            }
            ViaImport::Miss => {}
            ViaImport::Hit(c) => {
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
        let name_cand = match self.match_by_file_path(r)? {
            Some(c) => Some(c),
            None => match self.match_by_qualified_name(r)? {
                Some(c) => Some(c),
                None => match self.match_call_chain(r)? {
                    McRes::Hit(c) => Some(c),
                    McRes::Punt(p) => return Ok(ResolveOutcome::passthrough(p)),
                    McRes::Null => match self.match_method_call_free(r)? {
                        McRes::Hit(c) => Some(c),
                        McRes::Punt(p) => return Ok(ResolveOutcome::passthrough(p)),
                        McRes::Null => None,
                    },
                },
            },
        };
        let name_result = self.gate_language(name_cand, r);
        if let Some(c) = name_result {
            if self.is_visible_across_files(&c.node, r)? {
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
        Some(cand)
    }

    /// gateTargetKind (index.ts): the imports/inheritance target-kind gates
    /// plus the out-of-repo import check.
    pub(super) fn gate_target_kind(&mut self, cand: KCand, r: &ResolveRefIn) -> Result<Option<KCand>> {
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
    ) -> Result<Option<Arc<KNode>>> {
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
            _ => BARE_ALIAS_RE
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

    /// is_bare_name: the eligibility shape — no separator any skipped
    /// strategy keys on. Leading `$` stays (arkts `$r` builtins are handled);
    /// a `$` elsewhere could feed the R method pattern's `[\w.]+\$` arm.
    pub(super) fn name_is_bare(name: &str) -> bool {
        !name.is_empty()
            && !name
                .char_indices()
                .any(|(i, c)| matches!(c, '.' | ':' | '/' | '\\' | '#' | '(' | ')') || (c == '$' && i > 0))
    }

    /// Necessary condition for matchJsStoreBindingCall (name-matcher.ts):
    /// both store-binding arms bind the ref's own name through `const` — a
    /// destructure (`const {a} = X.getState()`) or a selector alias
    /// (`const a = useStore(s => s.a)`). Absent that shape the matcher cannot
    /// fire, so the kernel may adjudicate the ref itself. The whole file is
    /// scanned (the destructure can span lines); false positives only cost a
    /// TS fallback, never a wrong verdict.
    pub(super) fn file_could_store_bind(&mut self, r: &ResolveRefIn) -> Result<bool> {
        if r.reference_kind != "calls" || !is_js_family(&r.language) {
            return Ok(false);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let text = lines.join("\n");
        Ok(js_const_binds(&text, &r.reference_name))
    }

    /// The `function_ref` block of resolveOneInner (index.ts). TS order:
    /// prefilter → this.-member arm (non-bare, unreachable here) →
    /// resolveViaImport with a target-kind gate → matchFunctionRef. A
    /// prefilter miss routes to matchJsStoreBindingCall, which is
    /// `calls`-gated — dead for function_ref — and frameworks never run on
    /// this path, so the miss is terminal either way.
    pub(super) fn resolve_function_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
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

    /// matchFunctionRef, bare arm (name-matcher.ts): name-exact
    /// function/method nodes in the ref's language family — plus Python
    /// classes — excluding the origin node; JS/TS/ArkTS/C++/Python/PHP match
    /// functions only (a bare identifier there is never a method value).
    /// Same-file wins by earliest line; cross-file is unique-or-drop.
    pub(super) fn match_function_ref_bare(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
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
    pub(super) fn match_function_ref_scoped(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
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
    pub(super) fn resolve_rust_path_ref(&mut self, r: &ResolveRefIn) -> Result<Option<ResolveOutcome>> {
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
    pub(super) fn match_rust_path_reference(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
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
    ) -> Result<Option<String>> {
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
            let as_file = pos_normalize(&format!("{}/{}.rs", dir, seg));
            let as_mod = pos_normalize(&format!("{}/{}/mod.rs", dir, seg));
            if self.file_exists(&as_file) {
                target = Some(as_file);
            } else if self.file_exists(&as_mod) {
                target = Some(as_mod);
            } else {
                return None;
            }
            dir = pos_normalize(&format!("{}/{}", dir, seg));
        }
        target
    }

    /// rustCrateRootDir (import-resolver.ts): the directory holding
    /// `lib.rs`/`main.rs`, walking up from the ref's file (≤64 levels). When
    /// no such anchor exists — kernel-style modules whose root file is
    /// named e.g. `rust_binder_main.rs` — falls back to climbing the `mod`
    /// declaration chain to the file no other file declares.
    pub(super) fn rust_crate_root_dir(&mut self, from_file: &str) -> Result<Option<String>> {
        if let Some(v) = self.rust_crate_root_memo.get(from_file) {
            return Ok(v.clone());
        }
        let mut dir = pos_dirname(from_file).to_string();
        let mut found = None;
        for _ in 0..64 {
            if self.file_exists(&pos_normalize(&format!("{}/lib.rs", dir)))
                || self.file_exists(&pos_normalize(&format!("{}/main.rs", dir)))
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
    pub(super) fn rust_mod_chain_crate_root(&mut self, from_file: &str) -> Result<Option<String>> {
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
            let nested = pos_normalize(&format!(
                "{}/{}.rs",
                pos_dirname(&parent_dir),
                pos_basename(&parent_dir)
            ));
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
    pub(super) fn rust_rs_files_in_dir(&mut self, dir: &str) -> Rc<Vec<String>> {
        if self.rust_rs_dir_index.is_none() {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for f in self.table().map(|t| &t.files).into_iter().flatten() {
                let normalized = pos_normalize(f);
                if normalized.ends_with(".rs") {
                    m.entry(pos_dirname(&normalized).to_string())
                        .or_default()
                        .push(normalized);
                }
            }
            for v in m.values_mut() {
                v.sort();
            }
            self.rust_rs_dir_index = Some(Rc::new(
                m.into_iter().map(|(k, v)| (k, Rc::new(v))).collect(),
            ));
        }
        self.rust_rs_dir_index
            .as_ref()
            .unwrap()
            .get(dir)
            .cloned()
            .unwrap_or_else(|| Rc::new(Vec::new()))
    }

    /// Whether `rel_file` contains a `mod <stem>;` declaration (comment-
    /// stripped; `pub`/`pub(...)` qualifiers allowed). `mod <stem> {`
    /// inline modules end in `{`, not `;`, and do not match.
    pub(super) fn rust_file_declares_mod(&mut self, rel_file: &str, stem: &str) -> Result<bool> {
        let Some(lines) = self.read_file(rel_file) else {
            return Ok(false);
        };
        // `^\s*(?:pub\s*(?:\([^)]*\))?\s+)?mod\s+STEM\s*;`
        static MOD_DECL: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new(r"^\s*(?:pub\s*(?:\([^)]*\))?\s+)?mod\s+", r"\s*;", false, false, false)
        });
        Ok(lines.iter().any(|l| MOD_DECL.is_match(&strip_line_comments(l), stem)))
    }

    pub(super) fn resolve_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
        // Rust pure-`::` path refs (`crate::m::Item`, `a::b::c`): TS
        // resolves them through resolveViaImport's module-file arm, which
        // needs no bindings rows — run it ahead of the eligibility gate.
        // A miss falls through for migrated rust (qualified-name/exact arms
        // mirror matchReference's continuation) and punts otherwise.
        // Dot-bearing `::` names stay punted (the boundReceiver claim can
        // own a `a::b.c` receiver in TS), and `function_ref` keeps its own
        // block — a module-path hit on a non-callable leaf is DISCARDED
        // there, not returned.
        if r.language == "rust"
            && r.reference_kind != "function_ref"
            && r.reference_name.contains("::")
            && !r.reference_name.contains('.')
        {
            match self.resolve_rust_path_ref(r)? {
                Some(o) => return Ok(o),
                None if is_migrated_language(&r.language) => {}
                None => return Ok(ResolveOutcome::passthrough("ineligible:lang")),
            }
        }
        // ref_is_eligible, split so the passthrough reason names the gate.
        if !is_migrated_language(&r.language) {
            return Ok(ResolveOutcome::passthrough("ineligible:lang"));
        }
        if !Self::name_is_bare(&r.reference_name) {
            // The measured-dominant slice of the non-bare tail (§5.14):
            // C/C++ `#include` path refs resolve through their own arm.
            if (r.language == "c" || r.language == "cpp") && r.reference_kind == "imports" {
                return self.resolve_c_include_import_ref(r);
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
                        || (!r.reference_name.contains("::")
                            && !r.reference_name.contains("()"))))
            {
                return Ok(ResolveOutcome::passthrough("member-tail"));
            }
            // Member-access slice (§5.16): boundReceiver's DB sub-arms, the
            // import member descent, filePath and qualifiedName — the rest
            // of the member matchers punt back to the TS spine.
            return probe!(r, "resolve_nonbare_ref", self.resolve_nonbare_ref(r));
        }
        if r.file_path.is_empty() {
            return Ok(ResolveOutcome::passthrough("ineligible:path"));
        }

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
        let name_result = self.gate_language(name_cand, r);
        if let Some(c) = name_result {
            // Post-pipeline visibility check on the committed target — a
            // rejection must NOT promote a runner-up (#1730/#1745).
            if self.is_visible_across_files(&c.node, r)? {
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
    ) -> Result<ResolveOutcome> {
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
}
