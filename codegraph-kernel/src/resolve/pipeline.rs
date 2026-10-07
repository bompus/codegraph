//! resolveOneInner's non-bare slices, the gates, and the per-ref pipeline.

use super::*;

impl KernelResolver {
    // -----------------------------------------------------------------------
    // Non-bare c/cpp `imports` refs — the #include-path slice (resolveOneInner
    // restricted). A path the kernel cannot prove continues to the name arms
    // rather than guessing.
    // -----------------------------------------------------------------------

    /// The resolveOneInner ordering for `(c|cpp, 'imports', non-bare)`:
    /// builtin → prefilter → frameworks → boundReceiver → viaImport →
    /// nameMatch. For this slice jvmImport/razor/phpStatic are
    /// language-gated dead, boundReceiver is calls-gated dead, the chain
    /// guards need calls + `().`, and viaImport's first branch IS the c/cpp
    /// include arm (≥0.9 or nothing — its <0.9 candidate path can't fire).
    /// Then matchReference's name arms: filePath → qualifiedName →
    /// methodCall → exactName → fuzzy.
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
                    // A gated-out ≥0.9 import: a final miss that only a ≥0.9
                    // framework hit overturns.
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
        if name_cand.is_none() {
            name_cand = self.match_reference_bare(r)?;
        }
        // The first arm that answers is the name match; its post-checks and
        // the framework merge follow, as for every other name.
        self.after_name_match(r, Vec::new(), name_cand)
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
        self.finish_pre_framework(r, cand).map(Some)
    }

    /// resolveOneInner for non-bare refs, arms in the original TS order. A
    /// bound-receiver claim is terminal — a refusal never falls through to
    /// name matching.
    /// resolvePhpQualifiedClassRef (import-resolver.ts, #2256): a PHP class
    /// name written with a namespace in it — `new Field\FirstName()` through
    /// `use App\Fields as Field;`, `extends Sub\Base`, `\Ns\X::make()`. A
    /// leading `\` is absolute; otherwise a first segment a `use` imports is
    /// replaced by what it imports, and any other name is relative to the
    /// namespace in effect at the ref. None leaves the ref to the later arms
    /// (not a qualified class name, a type mention naming no single class, or
    /// a member the class inherits); Some carries the terminal outcome, an
    /// unresolved one when no single project class has the name.
    fn resolve_php_qualified_class(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.language != "php" {
            return Ok(None);
        }
        let (name, member) = if r.reference_kind == "calls" {
            let Some(call) = php_qualified_static_call_re().captures(&r.reference_name) else {
                return Ok(None);
            };
            (call.get(1).unwrap().as_str().to_string(), Some(call.get(2).unwrap().as_str().to_string()))
        } else if matches!(r.reference_kind.as_str(), "instantiates" | "extends" | "implements" | "references") {
            (r.reference_name.clone(), None)
        } else {
            return Ok(None);
        };
        let Some(separator) = name.find('\\') else {
            return Ok(None);
        };
        let fqn = if separator == 0 {
            name[1..].to_string()
        } else {
            let head = &name[..separator];
            let imported = self
                .import_mappings(&r.file_path)?
                .iter()
                .find(|i| i.local_name == head)
                .map(|i| i.source.trim_start_matches('\\').to_string());
            match imported {
                Some(source) => format!("{source}{}", &name[separator..]),
                None => {
                    // `namespace App;` applies until the next namespace statement.
                    let namespace = self
                        .nodes_in_file(&r.file_path)?
                        .iter()
                        .filter(|n| n.kind == "namespace" && n.start_line <= r.line)
                        .max_by_key(|n| n.start_line)
                        .map(|n| n.qualified_name.clone());
                    match namespace {
                        Some(ns) => format!("{ns}\\{name}"),
                        None => name.clone(),
                    }
                }
            }
        };
        let qualified_name = match fqn.rfind('\\') {
            Some(cut) => format!("{}::{}", &fqn[..cut], &fqn[cut + 1..]),
            None => fqn,
        };
        let classes: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&qualified_name)?
            .iter()
            .filter(|n| n.language == "php" && is_static_member_container(&n.kind))
            .cloned()
            .collect();
        // A type mention can name something other than a class (a namespaced
        // constant or function), so it keeps the ordinary arms.
        if classes.len() != 1 {
            return Ok((r.reference_kind != "references").then(ResolveOutcome::unresolved));
        }
        let owner = classes[0].clone();
        let target = match member {
            None => owner,
            Some(member) => {
                let methods: Vec<Arc<KNode>> = self
                    .nodes_by_qualified_name(&format!("{}::{member}", owner.qualified_name))?
                    .iter()
                    .filter(|n| n.language == "php" && n.kind == "method" && n.file_path == owner.file_path)
                    .cloned()
                    .collect();
                // A method the class inherits is left to the arms that walk supertypes.
                if methods.len() != 1 {
                    return Ok(None);
                }
                methods[0].clone()
            }
        };
        let cand = KCand { node: target, confidence: 0.95, resolved_by: "import" };
        let Some(cand) = self.gate_language(Some(cand), r) else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        match self.gate_target_kind(cand, r)? {
            Some(winner) => self.finish_pre_framework(r, winner).map(Some),
            None => Ok(Some(ResolveOutcome::unresolved())),
        }
    }

    pub(super) fn resolve_nonbare_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
if super::lang_scope::is_dart_member_read(r) {
    let Some(candidate) = self.match_dart_member_read(r)? else {
        return Ok(ResolveOutcome::unresolved());
    };
    let mut outcome = self.finish_pre_framework(r, candidate)?;
    // Parent adds this outcome field and its JS transport.
    outcome.edge_kind = Some("calls".to_string());
    return Ok(outcome);
}
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        // A CFML supertype written as a component path answers alone, ahead
        // of the prefilter (resolveCfmlComponentPath).
        if (r.language == "cfml" || r.language == "cfscript")
            && (r.reference_kind == "extends" || r.reference_kind == "implements")
            && (r.reference_name.contains('.') || r.reference_name.contains('/'))
        {
            return match self.resolve_cfml_component_path(r)? {
                Some(c) => match self.gate_target_kind(c, r)? {
                    Some(winner) => self.finish_pre_framework(r, winner),
                    None => Ok(ResolveOutcome::unresolved()),
                },
                None => Ok(ResolveOutcome::unresolved()),
            };
        }
        // A PHP class written with a namespace in it answers ahead of the
        // prefilter, which would drop most of them (#2256).
        if let Some(outcome) = self.resolve_php_qualified_class(r)? {
            return Ok(outcome);
        }
        // Prefilter — `existenceName` strips arkts' leading '.';
        // matchJsStoreBindingCall needs a dot-free name, so a non-bare
        // `Foo::bar` can still reach it.
        let existence = if r.language == "arkts" && r.reference_name.starts_with('.') {
            &r.reference_name[1..]
        } else if r.language == "erlang" {
            // The call-site arity (`f/1`) is not part of any node name.
            re!(r"/[0-9]{1,3}$").splitn(&r.reference_name, 2).next().unwrap_or(&r.reference_name)
        } else {
            &r.reference_name
        };
        let pre_pass = probe!(r, "pre-pass", is_nix_path_import_ref(r)
            || is_js_path_import_ref(r)
            || self.has_any_possible_match_in(existence, &r.language)
            || self.matches_any_import(r)?
            || self.framework_claims(&r.reference_name));
        if !pre_pass {
            // matchJsStoreBindingCall answers a prefilter miss alone.
            return self.store_binding_on_prefilter_miss(r);
        }

        // `function_ref` refs resolve ONLY through matchFunctionRef, never
        // the fallthrough below — TS's function_ref block for non-bare
        // names, in order: viaImport (a `.` member-descent can still claim
        // an `a.b` function_ref), then the `::` member-pointer arm (the
        // only non-bare shape matchFunctionRef resolves; `.`/`this.` forms
        // always miss in it).
        if r.reference_kind == "function_ref" {
            // `this.<member>`: the class-scoped arm answers alone — before the
            // import lookup, with no fallback (resolveThisMemberFnRef).
            if r.reference_name.starts_with("this.") {
                return match self.resolve_this_member_fn_ref(r)? {
                    this_member::ThisMember::Found(c) => match self.gate_language(Some(c), r) {
                        Some(c) => self.finish_pre_framework(r, c),
                        None => Ok(ResolveOutcome::unresolved()),
                    },
                    this_member::ThisMember::Defer => Ok(ResolveOutcome::deferred_this_member()),
                    this_member::ThisMember::Miss => Ok(ResolveOutcome::unresolved()),
                };
            }
            // A Python/Go member value (`self.store.fetch`, `c.store.Fetch`)
            // resolves through its receiver's scope alone (#1820).
            if (r.language == "python" || r.language == "go") && r.reference_name.contains('.') {
                let cand = self.match_member_function_ref(r)?;
                return match self.gate_language(cand, r) {
                    Some(c) => self.finish_pre_framework(r, c),
                    None => Ok(ResolveOutcome::unresolved()),
                };
            }
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
                            return self.finish_pre_framework(r, c);
                        }
                    }
                }
            }
            // matchFunctionRef: a `::` name takes only the member-pointer arm;
            // any other name the bare arm, over the whole dotted name.
            // Frameworks never run on this path — a gated candidate is
            // discarded to terminal unresolved, exactly like the bare arm.
            let cand = if r.reference_name.contains("::") {
                self.match_function_ref_scoped(r)?
            } else {
                self.match_function_ref_bare(r)?
            };
            return match self.gate_language(cand, r) {
                Some(c) => self.finish_pre_framework(r, c),
                None => Ok(ResolveOutcome::unresolved()),
            };
        }
        // resolveJvmImport — a java/kotlin `imports` ref that names a
        // declaration by FQN is answered before anything else; resolveOne's
        // gateTargetKind can still refuse it. A miss continues down the spine.
        if r.reference_kind == "imports" && (r.language == "java" || r.language == "kotlin") {
            if let Some(cand) = self.resolve_jvm_import(r)? {
                return match self.gate_target_kind(cand, r)? {
                    Some(winner) => self.finish_pre_framework(r, winner),
                    None => Ok(ResolveOutcome::unresolved()),
                };
            }
        }
        // resolvePhpImportedStaticCall — terminal before frameworks.
        if let Some(outcome) = self.resolve_php_imported_static(r)? {
            return Ok(outcome);
        }
        // A classic-script object can be declared in another file without an import.
        // Imported roots retain the binding resolver's authority.
        if let Some(c) = self.match_object_path_call(r)? {
            if let Some(c) = self.gate_language(Some(c), r) { return self.finish(r, c, None, true); }
        }
        // matchBoundReceiverCall — claimed refs are terminal either way.
        let namespace_chain = is_unresolved_js_member_call(r)
            && self.import_mappings(&r.file_path)?.iter().any(|m| m.is_namespace
                && m.local_name == r.reference_name.split('.').next().unwrap_or(""));
        if is_binding_receiver_call(r) && !namespace_chain && !self.is_component_receiver_out_of_scope(r)? {
            let mut bound = probe!(r, "bound_receiver_claim", self.bound_receiver_claim(r)?);
            if bound.is_none() && self.cpp_alias_allows_fallback(r)? {
                bound = self.match_method_call_free(r)?;
            }
            match bound {
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
            let root = r.reference_name.split('.').next().unwrap_or("");
            let namespace = self.import_mappings(&r.file_path)?.iter().any(|m| m.is_namespace && m.local_name == root);
            if namespace {
                if let Some(c) = self.resolve_via_import_member(r)? {
                    if matches!(c.node.kind.as_str(), "function" | "method" | "class" | "constant" | "variable") {
                        if let Some(c) = self.gate_language(Some(c), r) {
                            return self.finish(r, c, None, true);
                        }
                    }
                }
            }
            if !namespace {
                if let Some(c) = self.match_object_path_call(r)? {
                    if let Some(c) = self.gate_language(Some(c),r) { return self.finish(r,c,None,true); }
                }
            }
            return Ok(ResolveOutcome::unresolved());
        }
        // A TS/JS/Python `x().y` call names the root's import, not the
        // method's, so it skips the import arm and matchReference answers alone.
        if r.reference_kind == "calls"
            && chain_shape_re().is_match(&r.reference_name)
            && is_store_chain_language(&r.language)
        {
            return self.resolve_call_chain(r);
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
        // isPhpIncludePathRef: an include path resolves to a file through the
        // import arm or not at all — a name match would bind `inc/db.php` to
        // an unrelated `db.php` (#660). cobol / nix / terraform share the arm
        // in TS but are unmigrated languages.
        if is_import_only_ref(r) {
            return if cands.is_empty() { Ok(self.refused()) } else { self.settle(r, cands) };
        }

        // matchReference's leading arkts arm — `.attr` names resolve ONLY to
        // decorator-marked helpers, never the name-match fallthrough.
        if r.language == "arkts" && r.reference_name.starts_with('.') {
            let hit = self.match_arkts_attribute(r)?;
            return self.after_name_match(r, cands, hit);
        }
        // A Swift call through a type path (`API.PackageController.GetRoute.query`)
        // resolves on the type the path names, or not at all: the name
        // strategies below would bind it by the member's name alone.
        if r.language == "swift"
            && r.reference_kind == "calls"
            && !r.reference_name.starts_with("Self.")
            && re!(r"^[A-Z][A-Za-z0-9_]*(?:\.[A-Z][A-Za-z0-9_]*)+\.[A-Za-z_][A-Za-z0-9_]*$").is_match(&r.reference_name)
        {
            let hit = self.resolve_swift_type_path_call(r)?;
            return self.after_name_match(r, cands, hit);
        }
        // matchReference in TS order: filePath, qualifiedName, the
        // per-language chain arm (cppChain/scopedChain/dottedChain),
        // methodCall's requireReceiverEvidence=false arm, exactName, fuzzy.
        // The first strategy that answers is the name match; a gated-out
        // answer does not fall through to the next.
        if let Some(erlang) = self.match_erlang_reference(r)? {
            return self.after_name_match(r, cands, erlang);
        }
        let mut name_cand = self.match_by_file_path(r)?;
        if name_cand.is_none() {
            name_cand = self.match_by_qualified_name(r)?;
        }
        if name_cand.is_none() {
            name_cand = self.match_call_chain(r)?;
        }
        if name_cand.is_none() {
            // A `<inner>().` receiver in TS/JS/Python resolves only through
            // matchStoreAccessorChain, which returns before the method-call arm.
            if r.reference_name.contains("().") && is_store_chain_language(&r.language) {
                name_cand = self.match_store_accessor_chain(r)?;
                return self.after_name_match(r, cands, name_cand);
            }
            name_cand = self.match_method_call_free(r)?;
        }
        if name_cand.is_none() {
            // matchByExactName runs the store-binding matcher first for a
            // bare JS call.
            name_cand = probe!(r, "match_reference_bare", self.match_reference_bare(r)?);
        }
        if name_cand.is_none() { name_cand = self.match_cpp_macro_namespaced(r)?; }
        self.after_name_match(r, cands, name_cand)
    }

    /// resolveOneInner after the name match: its post-checks, then the
    /// first-max over the import candidate and the name result, deferring an
    /// unresolved chain call to the conformance pass.
    fn after_name_match(&mut self, r: &ResolveRefIn, mut cands: Vec<KCand>, name_cand: Option<KCand>) -> Res<ResolveOutcome> {
        if let Some(c) = self.gate_language(name_cand, r) {
            if self.name_result_stands(&c, r)? {
                cands.push(c);
            }
        }
        if cands.is_empty() {
            // An unresolved chain call waits for the conformance pass — TS
            // queues it, so it goes back; anything else is a plain miss.
            if is_deferred_chain_call(r) {
                return Ok(ResolveOutcome::deferred());
            }
            return Ok(self.refused());
        }
        self.settle(r, cands)
    }

    // -----------------------------------------------------------------------
    // Gates (index.ts) + alias forwarding (alias-binding.ts)
    // -----------------------------------------------------------------------

    /// gateLanguage (index.ts): drop import/name results crossing a family.
    pub(super) fn gate_language(&mut self, cand: Option<KCand>, r: &ResolveRefIn) -> Option<KCand> {
        let cand = cand?;
        if self.kotlin_number_bitwise(&cand.node, r) { return None; }
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
        if crosses_code_boundary(tgt, &r.language) && !self.has_bridge_evidence(&cand.node, r) {
            return None;
        }
        Some(cand)
    }

    /// A rejected imported callee must not be revived as its local require binding.
    pub(super) fn is_import_binding_call_target(&mut self, node: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if r.reference_kind != "calls"
            || !is_esm_family(&r.language)
            || node.file_path != r.file_path
            || !matches!(node.kind.as_str(), "variable" | "constant")
            || self.is_shadowed_import_name(r)?
        {
            return Ok(false);
        }
        Ok(self.bindings(&r.file_path)?.iter().any(|binding|
            binding.kind == "import"
                && binding.name == r.reference_name
                && binding.node_id.as_deref() == Some(node.id.as_str())))
    }

    /// gateTargetKind (index.ts): the imports/inheritance target-kind gates
    /// plus the out-of-repo import check.
    pub(super) fn gate_target_kind(&mut self, cand: KCand, r: &ResolveRefIn) -> Res<Option<KCand>> {
        // A `#define` is a value, never a callee (#1838).
        if r.reference_kind == "calls" && cpp::is_define(&cand.node) {
            return Ok(None);
        }
        if r.reference_kind == "imports" {
            if cand.node.kind == "import" { return Ok(None); }
            return Ok(if is_importable_kind(&cand.node.kind) {
                Some(cand)
            } else {
                None
            });
        }
        if self.is_import_binding_call_target(&cand.node, r)? {
            return Ok(None);
        }
        let mut cand = cand;
        self.cap_chain_confidence(&mut cand, r)?;
        if !is_inheritance_ref(&r.reference_kind) {
            return Ok(Some(cand));
        }
        if !is_supertype_target(&cand.node) {
            // `export const IFoo = createDecorator<IFoo>(…)` beside `export
            // interface IFoo` (VS Code's service pattern): the strategy found
            // the right file and name, so the edge moves to the type (#2055).
            let Some(ty) = self.same_named_type_of_value(&cand.node)? else {
                return Ok(None);
            };
            cand.node = ty;
        }
        if self.is_bound_to_out_of_repo_import(r)? {
            return Ok(None);
        }
        Ok(Some(cand))
    }

    /// The one supertype-kind node a TypeScript value shares its name and file with.
    fn same_named_type_of_value(&self, value: &KNode) -> Res<Option<Arc<KNode>>> {
        if !matches!(value.kind.as_str(), "constant" | "variable")
            || !matches!(value.language.as_str(), "typescript" | "tsx")
        {
            return Ok(None);
        }
        let mut types = self
            .nodes_in_file(&value.file_path)?
            .iter()
            .filter(|n| n.name == value.name && is_supertype_target(n))
            .cloned()
            .collect::<Vec<_>>();
        Ok(if types.len() == 1 { types.pop() } else { None })
    }

    /// aliasTargetName + resolveAliasBinding (alias-binding.ts). `member_name`
    /// is the ref's last `.` segment (null for bare/`::`-only names): a
    /// member access through an object-literal alias resolves the keyed or
    /// shorthand property binding (`{ member: fn }` / `{ member }`).
    pub(super) fn resolve_alias_binding(
        &mut self,
        alias_node: &KNode,
        member_name: Option<&str>,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        if !is_alias_binding_kind(&alias_node.kind) {
            return Ok(None);
        }
        // A JS object literal's member follows its lexical binding (#1932).
        if let Some(m) = member_name.filter(|m| !m.is_empty() && is_object_literal_language(&alias_node.language)) {
            let at = r.clone().naming(m, "calls");
            return Ok(self.resolve_object_literal_binding(alias_node, m, &at)?.map(|hit| hit.node));
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

    /// resolveOneInner's prefilter miss: `matchJsStoreBindingCall` alone,
    /// before the frameworks and every name arm (a bound action need not
    /// share its name with any definition).
    fn store_binding_on_prefilter_miss(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        let hit = self.match_js_store_binding_call(r)?;
        match self.gate_language(hit, r) {
            Some(c) => self.finish_pre_framework(r, c),
            None => Ok(ResolveOutcome::unresolved()),
        }
    }

    /// The `function_ref` block of resolveOneInner (index.ts). TS order:
    /// prefilter → this.-member arm (non-bare, unreachable here) →
    /// resolveViaImport with a target-kind gate → matchFunctionRef. A
    /// prefilter miss routes to matchJsStoreBindingCall, which is
    /// `calls`-gated — dead for function_ref — and frameworks never run on
    /// this path, so the miss is terminal either way.
    pub(super) fn resolve_function_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        let pre_pass = probe!(r, "pre-pass",
            self.has_any_possible_match_in(&r.reference_name, &r.language) || self.matches_any_import(r)?
                || (r.language == "csharp" && self.csharp_alias_at(&r.reference_name, r).is_some()));
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
                return self.finish_pre_framework(r, c);
            }
        }
        let name_cand = self.match_function_ref_bare(r)?;
        match self.gate_language(name_cand, r) {
            Some(c) => self.finish_pre_framework(r, c),
            None => Ok(ResolveOutcome::unresolved()),
        }
    }

    /// The per-ref pipeline: the Rust `::`-path arm, then one route.
    pub(super) fn resolve_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        if let Some(outcome) = self.resolve_shopify_file(r)? { return Ok(outcome); }
        if matches!(r.language.as_str(), "lua" | "luau") && r.reference_kind == "imports" {
            return match self.resolve_lua_require(r)? { Some(c) => self.finish_pre_framework(r, c), None => Ok(ResolveOutcome::unresolved()) };
        }
        if r.language == "dart" {
            if matches!(r.reference_kind.as_str(), "calls" | "function_ref") && !r.reference_name.contains('.') && self.dart_local_binding(&r.reference_name, r) == Some(false) { return Ok(ResolveOutcome::unresolved()); }
            if r.reference_kind == "imports" {
                let files = self.dart_uri_files(&r.file_path, &r.reference_name);
                if files.len() != 1 || files[0] == r.file_path { return Ok(ResolveOutcome::unresolved()); }
                let node = self.nodes_in_file(&files[0])?.iter().find(|n| n.kind == "file").cloned();
                return match node { Some(node) => self.finish_pre_framework(r, KCand { node, confidence: 0.95, resolved_by: "import" }), None => Ok(ResolveOutcome::unresolved()) };
            }
        }

        if self.python_receiver_uncertain(r)? { return Ok(ResolveOutcome::unresolved()); }
        if let Some(outcome) = self.resolve_dart_written(r)? { return Ok(outcome); }
        if let Some(outcome) = self.resolve_vb_explicit(r)? { return Ok(outcome); }
        // Rust pure-`::` path refs (`crate::m::Item`, `a::b::c`): TS
        // resolves them through resolveViaImport's module-file arm, which
        // needs no bindings rows — run it ahead of the eligibility gate.
        // A miss falls through to the ordinary route (the qualified-name and
        // exact arms mirror matchReference's continuation).
        // A local C++ object construction (`T obj(args)`, ref `ns::T::T/1`)
        // resolves only to a constructor of the lexically nearest `T` (#1839).
        if cpp::is_cpp_constructor_ref(r) {
            return match self.match_cpp_constructor(r)? {
                Some(c) => match self.gate_target_kind(c, r)? {
                    Some(w) => self.finish_pre_framework(r, w),
                    None => Ok(ResolveOutcome::unresolved()),
                },
                None => Ok(ResolveOutcome::unresolved()),
            };
        }
        // A C/C++ "call" whose name is a function-like macro visible in this
        // translation unit is a macro expansion, not a call (#1838).
        if self.is_visible_cpp_macro(r)? {
            return Ok(ResolveOutcome::unresolved());
        }
        if is_rust_path_ref(r) {
            if let Some(o) = self.resolve_rust_path_ref(r)? {
                return Ok(o);
            }
        }
        if let Some(c) = self.match_rust_use_alias(r)? {
            return match self.gate_language(Some(c), r) {
                Some(c) => self.finish(r, c, None, true),
                None => Ok(ResolveOutcome::unresolved()),
            };
        }
        if is_js_family(&r.language) && r.reference_kind == "calls" {
            if let Some(caller) = self.node_by_id(&r.from_node_id)? {
                let name = r.reference_name.rsplit(['.', ':']).next().unwrap_or(&r.reference_name);
                if caller.name == name && caller.kind == "method"
                    && self.same_owner_receiver_proven(&caller, r)
                {
                    return self.finish_pre_framework(r, KCand {
                        node: caller,
                        confidence: 0.9,
                        resolved_by: "instance-method",
                    });
                }
            }
        }
        match route(r) {
            Route::Unresolved => Ok(ResolveOutcome::unresolved()),
            Route::CInclude => self.resolve_c_include_import_ref(r),
            // Member-access slice (§5.16): boundReceiver's DB sub-arms, the
            // import member descent, filePath, qualifiedName and the member
            // matchers.
            Route::NonBare => probe!(r, "resolve_nonbare_ref", self.resolve_nonbare_ref(r)),
            Route::Bare => self.resolve_bare_ref(r),
        }
    }

    /// resolveOneInner's bare slice (a name with no separator).
    pub(super) fn resolve_bare_ref(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        if let Some(result) = self.cpp_complex_call(r)? {
            return match result { Some(candidate) => self.finish_pre_framework(r, candidate), None => Ok(self.refused()) };
        }

        // resolveOneInner, bare slice:
        //   builtin/external → CFML/jvm/razor/phpStatic arms all dead →
        //   prefilter → frameworks (TS) → boundReceiver (dead) → chain guard
        //   (dead) → viaImport → name-match → post-checks → first-max.
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
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
                return self.finish_pre_framework(r, c);
            }
        }
        // nix-path/arkts-dot/erlang-arity arms are dead for migrated bare
        // names; the claimsReference arm is evaluated natively. A claimed name
        // that no definition or import carries can only be answered by the
        // framework resolvers — every name arm finds nothing — so it settles
        // through the framework merge alone.
        let pre_pass = probe!(r, "pre-pass",
            self.has_any_possible_match_in(&r.reference_name, &r.language) || self.matches_any_import(r)?
                || self.js_typed_destructured_member(r)?.is_some()
                || (r.language == "csharp" && self.csharp_alias_at(&r.reference_name, r).is_some()));
        if !pre_pass {
            if self.framework_claims(&r.reference_name) {
                return Ok(ResolveOutcome::no_candidates());
            }
            return self.store_binding_on_prefilter_miss(r);
        }
        // A Razor/Blazor simple type through the file's `@using` namespaces
        // (resolveRazorUsing) — ahead of the import arm and the frameworks.
        if r.language == "razor" {
            if let Some(c) = self.resolve_razor_using(r)? {
                return match self.gate_target_kind(c, r)? {
                    Some(winner) => self.finish_pre_framework(r, winner),
                    None => Ok(ResolveOutcome::unresolved()),
                };
            }
        }

        let mut cands: Vec<KCand> = Vec::new();
        // `self.get_ip()` is a method call on the instance even when the file
        // also imports a function named `get_ip`: the import never names it,
        // unless the class rebinds the name to it (`get_ip = staticmethod(get_ip)`).
        let self_call = r.language == "python"
            && r.reference_kind == "calls"
            && self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == r.reference_name)
            && self.is_python_self_call(r)?
            && self.python_self_skips_import(r)?;
        let import_cand = if self_call { None } else { probe!(r, "resolve_via_import", self.resolve_via_import(r)?) };
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
                    // A gated-out ≥0.9 import resolves to nothing — its <0.9
                    // framework candidates are discarded — but a ≥0.9
                    // framework hit would have pre-empted the import: a final
                    // miss the framework merge may still overturn.
                    return Ok(self.gated_import());
                }
            };
            return self.finish(r, winner, None, true);
        }

        if is_import_only_ref(r) {
            return if cands.is_empty() { Ok(self.refused()) } else { self.settle(r, cands) };
        }
        let name_cand = match self.match_erlang_reference(r)? {
            Some(erlang) => erlang,
            None => probe!(r, "match_reference_bare", self.match_reference_bare(r)?),
        };
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
        // Bound receiver calls already prove their value/import owner. A Go
        // package-level value in another file is not an unknown package name.
        if r.language == "go" && !is_binding_receiver_call(r) && self.go_written_qualifier(r)?.is_some() && self.go_ref_qualifier(r)?.is_none() { return Ok(ResolveOutcome::unresolved()); }
        if !self.name_post_guard(&winner,r)? { return Ok(ResolveOutcome::unresolved()); }
        if winner.node.id == r.from_node_id && is_inheritance_ref(&r.reference_kind) {
            let Some(other) = self.other_supertype_named(r)? else { return Ok(ResolveOutcome::unresolved()); }; winner=other;
        }
if r.language == "dart" && r.reference_kind == "references"
    && winner.node.id == r.from_node_id {
    return Ok(ResolveOutcome::unresolved());
}
        winner=self.retarget_overload(winner,r)?;
        if r.reference_kind == "calls" {
            // memberName = the last `.` segment — `Cls::member` and bare
            // names carry none (no '.' → bare arm); `a.b::c` yields `b::c`,
            // matching lastIndexOf('.') + slice.
            let member_name = r
                .reference_name
                .rfind('.')
                .map(|i| &r.reference_name[i + 1..]);
            if let Some(forwarded) = self.resolve_alias_binding(&winner.node, member_name, r)? {
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

    fn cap_chain_confidence(&mut self, cand: &mut KCand, r: &ResolveRefIn) -> Res<()> {
        if cand.confidence>0.7 && self.rust_chain_is_heuristic(&cand.node,r)? {cand.confidence=0.7;}
        if cand.confidence > 0.7 && (self.kotlin_chain_evidence(&cand.node, r)? == Some(super::call_shape::KotlinChainEvidence::Heuristic)
            || (r.language == "kotlin" && r.reference_kind == "calls"
                && (self.kotlin_call_receiver_type(r)?.is_some_and(|hit| hit.heuristic)
                    || (self.is_kotlin_qualified_call(r) && self.kotlin_call_receiver_type(r)?.is_none() && (self.kotlin_visible_extension(&cand.node,r)? || self.kotlin_unique_unknown_member(&cand.node,r)?))))) {
            cand.confidence = 0.7;
        }
        Ok(())
    }

    /// First-max on strict `>` over `cands` (non-empty), the TS
    /// candidates.reduce, then the target-kind gate and `finish`. Under active
    /// frameworks the reported list keeps the ORIGINAL candidate order so the
    /// TS merge can re-run the reduce with framework candidates prepended; a
    /// gated-out winner still reports it, since a framework candidate may win
    /// the merged first-max on the TS side.
    pub(super) fn settle(&mut self, r: &ResolveRefIn, mut cands: Vec<KCand>) -> Res<ResolveOutcome> {
        for cand in &mut cands {
            self.cap_chain_confidence(cand, r)?;
        }
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
        if !self.name_post_guard(c,r)? || !self.is_visible_across_files(&c.node, r)? {
            return Ok(false);
        }
        // Nix binds lexically or through explicit imports: a Nix ref's name
        // match stays in its own file, and nothing else can name a Nix binding.
        Ok(if r.language == "nix" { c.node.file_path == r.file_path } else { c.node.language != "nix" })
    }

    /// No kernel verdict: framework candidates may still exist on the TS
    /// side, so report an empty list when frameworks are active.
    /// resolveOneInner's TS/JS/Python chain branch: matchReference's
    /// file-path and qualified-name arms, then matchStoreAccessorChain, whose
    /// only answer is a store accessor's action (`get().reset`,
    /// `useStore.getState().reset`) — source-reading, so that shape stays in
    /// TS. Any other chain says nothing about what the inner call returns and
    /// resolves to nothing. The branch returns before the framework merge and
    /// the name post-checks.
    fn resolve_call_chain(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        let mut cand = self.match_by_file_path(r)?;
        if cand.is_none() {
            cand = self.match_by_qualified_name(r)?;
        }
        if cand.is_none() {
            // Python's `super()` is the one inner call whose result the
            // class around the call names.
            cand = if r.language == "python" && r.reference_name.starts_with("super().") {
                self.python_super_method(r)?
            } else {
                self.match_store_accessor_chain(r)?
            };
        }
        let Some(cand) = cand else {
            return Ok(self.chain_miss());
        };
        let Some(cand) = self.gate_language(Some(cand), r) else { return Ok(self.chain_miss()) };
        let Some(winner) = self.gate_target_kind(cand, r)? else { return Ok(self.chain_miss()) };
        self.finish(r, winner, None, true)
    }

    fn chain_miss(&self) -> ResolveOutcome {
        if self.frameworks_active {
            ResolveOutcome::final_miss()
        } else {
            ResolveOutcome::unresolved()
        }
    }

    /// finish for an arm resolveOneInner evaluates before the framework loop:
    /// its verdict must not be overturned by a framework hit.
    pub(super) fn finish_pre_framework(&mut self, r: &ResolveRefIn, c: KCand) -> Res<ResolveOutcome> {
        let mut out = self.finish(r, c, None, true)?;
        out.pre_framework = true;
        Ok(out)
    }

    pub(super) fn refused(&self) -> ResolveOutcome {
        if self.frameworks_active {
            ResolveOutcome::no_candidates()
        } else {
            ResolveOutcome::unresolved()
        }
    }

    /// A gated-out ≥0.9 import resolves to nothing, unless a ≥0.9 framework
    /// hit pre-empts it (resolveOneInner returns the import before any <0.9
    /// framework candidate can merge): exactly the final-miss rule.
    pub(super) fn gated_import(&self) -> ResolveOutcome {
        if self.frameworks_active {
            ResolveOutcome::final_miss()
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
    /// C/C++ `#include` path refs — the measured-dominant slice of the
    /// non-bare tail (§5.14).
    CInclude,
    NonBare,
    Bare,
    /// A ref with no file: no arm can place it.
    Unresolved,
}

/// Which pipeline a ref takes.
fn route(r: &ResolveRefIn) -> Route {
    // Only the `unknown` placeholder (an undetected file) is unmigrated: no
    // arm can place a ref from it.
    if !is_migrated_language(&r.language) {
        return Route::Unresolved;
    }
    if !name_is_bare(&r.reference_name) {
        if (r.language == "c" || r.language == "cpp") && r.reference_kind == "imports" {
            return Route::CInclude;
        }
        return Route::NonBare;
    }
    if r.file_path.is_empty() {
        return Route::Unresolved;
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

/// matchReference's storeAccessorChain languages.
fn is_store_chain_language(language: &str) -> bool {
    matches!(language, "typescript" | "javascript" | "tsx" | "jsx" | "python")
}

/// A call resolveOneInner defers to the conformance pass when nothing
/// matched: a chain call in a CHAIN_LANGUAGES language, or PHP's
/// `this->prop.method`.
fn is_deferred_chain_call(r: &ResolveRefIn) -> bool {
    r.reference_kind == "calls"
        && ((CHAIN_LANGUAGES.contains(&r.language.as_str()) && chain_shape_re().is_match(&r.reference_name))
            || (r.language == "php" && php_prop_shape_re().is_match(&r.reference_name)))
}
