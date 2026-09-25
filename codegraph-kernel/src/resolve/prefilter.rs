//! isBuiltInOrExternal, the prefilter, and the binding predicates (index.ts, name-matcher.ts).

use super::*;

impl KernelResolver {
    // -----------------------------------------------------------------------
    // isBuiltInOrExternal + the fast pre-filter (index.ts)
    // -----------------------------------------------------------------------

    /// isBuiltInOrExternal restricted to migrated languages and bare names —
    /// every arm the bare slice can reach, in the same order.
    pub(super) fn is_built_in_or_external(&self, r: &ResolveRefIn) -> bool {
        let name = r.reference_name.as_str();
        let is_js_ts = is_esm_family(&r.language);
        if is_js_ts && JS_BUILT_INS.contains(name) {
            return true;
        }
        if r.language == "arkts" && (name == "$r" || name == "$rawfile") {
            return true;
        }
        if is_js_ts && REACT_HOOKS.contains(name) {
            return true;
        }
        if r.language == "python" && PYTHON_BUILT_INS.contains(name) {
            return true;
        }
        if r.language == "python" && PYTHON_BUILT_IN_METHODS.contains(name) {
            // A bare name colliding with a builtin method is only a builtin
            // when nothing declares it.
            return !self.known_name(name);
        }
        // Go: stdlib package member access (`fmt.Println`) is external — but
        // only when the ref is not a binding-receiver call: a bound receiver
        // owns `pkg.member` names (isBindingReceiverCall, index.ts).
        if r.language == "go" && !is_binding_receiver_call(r) {
            if let Some(dot) = name.find('.') {
                if dot > 0 && GO_STDLIB_PACKAGES.contains(&name[..dot]) {
                    return true;
                }
            }
            if GO_BUILT_INS.contains(name) {
                return true;
            }
        }
        if r.language == "c" || r.language == "cpp" {
            // `std::` prefix — never a user-defined qualified name.
            if name.starts_with("std::") {
                return true;
            }
            if C_BUILT_INS.contains(name) || CPP_BUILT_INS.contains(name) {
                return !self.has_any_possible_match(name);
            }
        }
        false
    }

    /// hasAnyPossibleMatch (index.ts) — the full check: direct name, then the
    /// receiver/member segments around `.`/`::`/`:`/`$`, then the path tail.
    /// A bare name only reaches the direct check; the separator branches
    /// serve the non-bare callers (the member slice, C/C++ include paths).
    pub(super) fn has_any_possible_match(&self, name: &str) -> bool {
        let path_name = name.replace('\\', "/");
        let path_name = path_name.split('#').next().unwrap_or("");
        if self.known_name(name) {
            return true;
        }
        if path_name != name && self.known_name(path_name) {
            return true;
        }
        if let Some(dot_idx) = name.find('.') {
            if dot_idx > 0 {
                let (receiver, member) = (&name[..dot_idx], &name[dot_idx + 1..]);
                if self.known_name(receiver) || self.known_name(member) {
                    return true;
                }
                let capitalized = capitalize_first(receiver);
                if self.known_name(capitalized.as_str()) {
                    return true;
                }
                if let Some(last_dot) = name.rfind('.') {
                    if last_dot > dot_idx {
                        let tail = &name[last_dot + 1..];
                        if !tail.is_empty() && self.known_name(tail) {
                            return true;
                        }
                    }
                }
            }
        }
        if let Some(colon_idx) = name.find("::") {
            if colon_idx > 0 {
                let (receiver, member) = (&name[..colon_idx], &name[colon_idx + 2..]);
                if self.known_name(receiver) || self.known_name(member) {
                    return true;
                }
                if let Some(last_colon) = name.rfind("::") {
                    if last_colon > colon_idx {
                        let tail = &name[last_colon + 2..];
                        if !tail.is_empty() && self.known_name(tail) {
                            return true;
                        }
                    }
                }
            }
        }
        for sep in [':', '$'] {
            if sep == ':' && name.contains("::") {
                continue;
            }
            if let Some(sep_idx) = name.find(sep) {
                if sep_idx > 0 {
                    let (receiver, member) = (&name[..sep_idx], &name[sep_idx + 1..]);
                    if self.known_name(member) || self.known_name(receiver) {
                        return true;
                    }
                    let capitalized = capitalize_first(receiver);
                    if self.known_name(capitalized.as_str()) {
                        return true;
                    }
                }
            }
        }
        if let Some(slash_idx) = path_name.rfind('/') {
            if slash_idx > 0 && self.known_name(&path_name[slash_idx + 1..]) {
                return true;
            }
        }
        if !path_name.contains('/')
            && thread_regex(&EXT_TAIL_RE).is_match(path_name)
            && self.known_name(path_name)
        {
            return true;
        }
        false
    }

    /// matchesAnyImport — `localName === name`, or the `localName.` prefix
    /// arm for member names.
    pub(super) fn matches_any_import(&mut self, r: &ResolveRefIn) -> Result<bool> {
        let imports = self.import_mappings(&r.file_path)?;
        Ok(imports.iter().any(|i| {
            i.local_name == r.reference_name
                || r.reference_name
                    .strip_prefix(&i.local_name)
                    .is_some_and(|rest| rest.starts_with('.'))
        }))
    }

    /// `frameworks.some(f => f.claimsReference?.(name))` — the prefilter's
    /// fourth arm, evaluated over the detected resolver names. A config
    /// without `framework_names` predates the refinement: every miss is
    /// conservatively claimed (the old `frameworks_active` behavior).
    pub(super) fn framework_claims(&self, name: &str) -> bool {
        if !self.frameworks_active {
            return false;
        }
        match &self.framework_names {
            None => true,
            Some(names) => names.iter().any(|f| framework_claims_reference(f, name)),
        }
    }

    // -----------------------------------------------------------------------
    // innermostBinding / isBoundToBareImport / isLocallyBoundJsName
    // (name-matcher.ts)
    // -----------------------------------------------------------------------

    /// innermostBinding: the row for `name` whose scope contains `line`,
    /// narrowest scope first (first-wins on ties, matching the TS loop).
    pub(super) fn innermost_binding<'a>(
        rows: &'a [KBinding],
        name: &str,
        line: Option<i64>,
    ) -> Option<&'a KBinding> {
        let mut best: Option<&KBinding> = None;
        for r in rows {
            if r.name != name {
                continue;
            }
            if let Some(l) = line {
                if l < r.scope_start || l > r.scope_end {
                    continue;
                }
            }
            if best.is_none_or(|b| r.scope_end - r.scope_start < b.scope_end - b.scope_start) {
                best = Some(r);
            }
        }
        best
    }

    /// packageNameOf — `@scope/pkg/sub` → `@scope/pkg`, `pkg/sub` → `pkg`.
    pub(super) fn package_name_of(source: &str) -> &str {
        let mut parts = source.split('/');
        match (source.starts_with('@'), parts.next(), parts.next()) {
            (true, Some(scope), Some(pkg)) => &source[..scope.len() + 1 + pkg.len()],
            (_, Some(first), _) => first,
            _ => source,
        }
    }

    /// isBoundToBareImport (name-matcher.ts) — ESM languages only; the
    /// call-site name resolves to a bare external specifier, so no project
    /// node may claim it.
    pub(super) fn is_bound_to_bare_import(&mut self, r: &ResolveRefIn) -> Result<bool> {
        if !is_esm_family(&r.language) {
            return Ok(false);
        }
        let rows = self.bindings(&r.file_path)?;
        let source: Option<String> = if !rows.is_empty() {
            match Self::innermost_binding(&rows, &r.reference_name, Some(r.line)) {
                Some(b) if b.kind == "import" => b.target_spec.clone(),
                _ => None,
            }
        } else {
            self.import_mappings(&r.file_path)?
                .iter()
                .find(|i| i.local_name == r.reference_name)
                .map(|i| i.source.clone())
        };
        let Some(source) = source else { return Ok(false) };
        if source.starts_with('.') || source.starts_with('/') {
            return Ok(false);
        }
        if source.starts_with('~') || source.starts_with('#') || source.starts_with('$') {
            return Ok(false);
        }
        if source.starts_with("@/") || source.starts_with("src/") {
            return Ok(false);
        }
        if let Some(aliases) = &self.aliases {
            if aliases.patterns.iter().any(|p| source.starts_with(&p.prefix)) {
                return Ok(false);
            }
        }
        if let Some(ws) = &self.workspaces {
            if self.resolve_workspace_import(&source).is_some() {
                return Ok(false);
            }
            if ws.local_link_names.contains(Self::package_name_of(&source)) {
                return Ok(false);
            }
        }
        if !source.starts_with("node:") && !self.node_builtins.contains(&source) {
            let head = Self::package_name_of(&source).to_string();
            let local = match self.root_import_memo.get(&head) {
                Some(&v) => v,
                None => {
                    let v = self.file_exists(&head);
                    self.root_import_memo.insert(head, v);
                    v
                }
            };
            if local {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// isLocallyBoundJsName — a `decl`/`local`/`param` row whose scope holds
    /// the reference.
    pub(super) fn is_locally_bound_js_name(
        &mut self,
        name: &str,
        file_path: &str,
        line: Option<i64>,
    ) -> Result<bool> {
        let rows = self.bindings(file_path)?;
        if rows.is_empty() {
            return Ok(false);
        }
        Ok(match Self::innermost_binding(&rows, name, line) {
            Some(b) => matches!(b.kind.as_str(), "decl" | "local" | "param"),
            None => false,
        })
    }
}
