//! isBuiltInOrExternal, the prefilter, and the binding predicates (index.ts, name-matcher.ts).

use super::*;
use super::iteration::{descendant_for_position, named_children};

impl KernelResolver {
    // -----------------------------------------------------------------------
    // isBuiltInOrExternal + the fast pre-filter (index.ts)
    // -----------------------------------------------------------------------

    /// isBuiltInOrExternal restricted to migrated languages and bare names —
    /// every arm the bare slice can reach, in the same order.
    pub(super) fn is_built_in_or_external(&mut self, r: &ResolveRefIn) -> bool {
        let name = r.reference_name.as_str();
        let is_js_ts = is_esm_family(&r.language) || is_sfc_language(&r.language);
        // A built-in's name the file imports its own binding of
        // (`import Map from './Map.svelte'`) is that import, and one a
        // declaration in scope at the site binds (a component's
        // `function process(…)`) is that declaration.
        if is_js_ts
            && JS_BUILT_INS.contains(name)
            && !self.import_mappings(&r.file_path).is_ok_and(|m| m.iter().any(|i| i.local_name == name))
            && !self.bindings(&r.file_path).is_ok_and(|rows| {
                match innermost_binding(&rows, name, Some(r.line)) {
                    Some(b) => matches!(b.kind.as_str(), "decl" | "local" | "param"),
                    // A component's template sits outside its script's
                    // scope, yet sees the script's top-level declarations.
                    None => is_sfc_language(&r.language) && is_script_top_level_decl(&rows, name),
                }
            })
        {
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
                if dot > 0 && GO_STDLIB_PACKAGES.contains(&name[..dot])
                    && !self.is_shadowed_import(&name[..dot], r).unwrap_or(false) {
                    return true;
                }
            }
            if GO_BUILT_INS.contains(name) {
                return true;
            }
        }
        // Pascal/Delphi built-ins and standard library units.
        if r.language == "pascal"
            && (PASCAL_UNIT_PREFIXES.iter().any(|p| name.starts_with(p)) || PASCAL_BUILT_INS.contains(name))
        {
            return true;
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

    /// The prefilter's existence check for a ref: `hasAnyPossibleMatch`, or
    /// for a language whose names ignore case (PHP, Pascal, CFML, COBOL,
    /// VB.NET) the name or any `.`/`::`/`->` part of it in any case —
    /// `formatprice()` calls `FormatPrice`, which the exact-name set never lists.
    pub(super) fn has_any_possible_match_in(&self, name: &str, language: &str) -> bool {
        if self.has_any_possible_match(name) {
            return true;
        }
        if !is_case_insensitive_language(language) {
            return false;
        }
        let known_lower = |n: &str| self.nodes_by_lower_name(n).is_ok_and(|list| !list.is_empty());
        known_lower(name)
            || re!(r"::|->|\.").split(name).any(|part| !part.is_empty() && known_lower(part))
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
            && ext_tail_re().is_match(path_name)
            && self.known_name(path_name)
        {
            return true;
        }
        false
    }

    /// matchesAnyImport — `localName === name`, or the `localName.` prefix
    /// arm for member names.
    pub(super) fn matches_any_import(&mut self, r: &ResolveRefIn) -> Res<bool> {
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

    /// The node a binding row names: an `import` row through the import
    /// resolver (`via_import`, the caller's ref shape), any other row through
    /// its own `node_id`, no row → none.
    pub(super) fn binding_target_id(
        &mut self,
        binding: Option<&KBinding>,
        via_import: impl FnOnce(&mut Self) -> Res<Option<KCand>>,
    ) -> Res<Option<String>> {
        match binding {
            Some(b) if b.kind == "import" => Ok(via_import(self)?.map(|c| c.node.id.clone())),
            Some(b) => Ok(b.node_id.clone()),
            None => Ok(None),
        }
    }

    /// isBoundToBareImport (name-matcher.ts) — ESM languages only; the
    /// call-site name resolves to a bare external specifier, so no project
    /// node may claim it.
    pub(super) fn is_bound_to_bare_import(&mut self, r: &ResolveRefIn) -> Res<bool> {
        Ok(self.bare_import_verdict(r)? == BareImport::External)
    }

    /// The call-site name is imported from a bare specifier that only
    /// manifest text places in this repository: no workspace mapping says
    /// which directory holds it, so a same-named symbol is a guess.
    pub(super) fn is_bound_to_unmapped_package(&mut self, r: &ResolveRefIn) -> Res<bool> {
        Ok(self.bare_import_verdict(r)? == BareImport::UnmappedLocal)
    }

    fn bare_import_verdict(&mut self, r: &ResolveRefIn) -> Res<BareImport> {
        let local = Ok(BareImport::Local);
        if !is_esm_family(&r.language) && !is_sfc_language(&r.language) {
            return local;
        }
        let rows = self.bindings(&r.file_path)?;
        let source: Option<String> = if !rows.is_empty() {
            match innermost_binding(&rows, &r.reference_name, Some(r.line)) {
                Some(b) if b.kind == "import" => b.target_spec.clone(),
                _ => None,
            }
        } else {
            self.import_mappings(&r.file_path)?
                .iter()
                .find(|i| i.local_name == r.reference_name)
                .map(|i| i.source.clone())
        };
        let Some(source) = source else { return local };
        if source.starts_with('.') || source.starts_with('/') {
            return local;
        }
        // SvelteKit's `$app/…` and Astro's `astro:…` are the framework's
        // virtual modules — unless this repository is that framework.
        let provider = if source.starts_with("astro:") {
            Some("astro")
        } else if re!(r"^\$(?:app|env|service-worker)(?:/|$)").is_match(&source) {
            Some("@sveltejs/kit")
        } else {
            None
        };
        if let Some(provider) = provider {
            return Ok(match self.repository_package(provider, &r.file_path) {
                RepositoryPackage::No => BareImport::External,
                _ => BareImport::Local,
            });
        }
        if source.starts_with('~') || source.starts_with('#') || source.starts_with('$') {
            return local;
        }
        if source.starts_with("@/") || source.starts_with("src/") {
            return local;
        }
        if self.is_alias_prefix(&source, &r.file_path) {
            return local;
        }
        if self.workspaces.is_some() && self.resolve_workspace_import(&source).is_some() {
            return local;
        }
        match self.repository_package(package_name_of(&source), &r.file_path) {
            RepositoryPackage::Mapped => return local,
            RepositoryPackage::Manifest => return Ok(BareImport::UnmappedLocal),
            RepositoryPackage::No => {}
        }
        if !source.starts_with("node:") && !self.node_builtins.contains(&source) {
            let head = package_name_of(&source).to_string();
            let local = match self.root_import_memo.get(&head) {
                Some(&v) => v,
                None => {
                    let v = self.file_exists(&head);
                    self.root_import_memo.insert(head, v);
                    v
                }
            };
            if local {
                return Ok(BareImport::Local);
            }
        }
        Ok(BareImport::External)
    }

    /// A Svelte or Astro receiver the file declares, used where no binding
    /// scope reaches: the markup, or a second `<script>` beside the one that
    /// declares it. The rows can't type it there, so the binding-receiver
    /// claim would refuse a call the name strategies still settle
    /// (`counter.double()` in markup on `const counter = new Counter()`).
    /// A script-level import stays with the claim, which reads it through
    /// `sfc_top_level_binding`, even when a function elsewhere declares a
    /// local of the same name.
    pub(super) fn is_component_receiver_out_of_scope(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if !is_sfc_scoped_script(&r.language) {
            return Ok(false);
        }
        let root = r.reference_name.split('.').next().unwrap_or("");
        let rows = self.bindings(&r.file_path)?;
        Ok(innermost_binding(&rows, root, Some(r.line)).is_none()
            && !sfc_top_level_binding(&rows, root).is_some_and(|b| b.kind == "import")
            && rows.iter().any(|b| b.name == root && b.kind != "import"))
    }

    /// Is `package` in this repository? A workspace member, a `link:`/`file:`
    /// dependency the workspace loader found, or — read from the importing
    /// file's package.json and every enclosing one — the manifest's own name
    /// (a package importing itself) or a dependency declared `workspace:`
    /// (isDeclaredOutsidePackage's `own` set, index.ts). A nested manifest's
    /// `link:`/`file:` dependency stays outside: it names a directory, and
    /// only resolving it there would be binding evidence; matching the
    /// imported name across the project instead guessed wrong on vite's
    /// playground fixtures.
    fn repository_package(&mut self, package: &str, from_file: &str) -> RepositoryPackage {
        if let Some(ws) = &self.workspaces {
            if ws.local_link_names.contains(package) || self.resolve_workspace_import(package).is_some() {
                return RepositoryPackage::Mapped;
            }
        }
        let mut dir = pos_dirname(from_file).to_string();
        loop {
            if dir == "." {
                dir.clear();
            }
            if self.manifest_own_packages(&dir).contains(package) {
                return RepositoryPackage::Manifest;
            }
            if dir.is_empty() {
                return RepositoryPackage::No;
            }
            dir = match dir.rfind('/') {
                Some(cut) => dir[..cut].to_string(),
                None => String::new(),
            };
        }
    }

    /// The package names `<dir>/package.json` owns: its `name`, and every
    /// dependency declared `workspace:`. Read with patterns, not a JSON
    /// parser: a key whose value starts `workspace:` names a package of this
    /// repository wherever it appears.
    fn manifest_own_packages(&mut self, dir: &str) -> Rc<HashSet<String>> {
        if let Some(hit) = self.manifest_own_memo.get(dir) {
            return hit.clone();
        }
        let path = if dir.is_empty() { "package.json".to_string() } else { format!("{dir}/package.json") };
        let mut own = HashSet::new();
        if let Some(file) = self.read_file(&path) {
            let text = file.text();
            if let Some(m) = re!(r#"^\s*\{\s*(?:"[^"]*"\s*:\s*(?:"[^"]*"|[^,{}\[\]]+)\s*,\s*)*"name"\s*:\s*"([^"]+)""#).captures(text) {
                own.insert(m[1].to_string());
            }
            for m in re!(r#""([^"]+)"\s*:\s*"workspace:"#).captures_iter(text) {
                own.insert(m[1].to_string());
            }
        }
        let own = Rc::new(own);
        self.manifest_own_memo.insert(dir.to_string(), own.clone());
        own
    }

    /// isLocallyBoundJsName — a `decl`/`local`/`param` row whose scope holds
    /// the reference.
    pub(super) fn is_locally_bound_js_name(
        &mut self,
        name: &str,
        file_path: &str,
        line: Option<i64>,
    ) -> Res<bool> {
        let rows = self.bindings(file_path)?;
        if rows.is_empty() {
            return Ok(false);
        }
        Ok(match innermost_binding(&rows, name, line) {
            Some(b) => matches!(b.kind.as_str(), "decl" | "local" | "param"),
            None => false,
        })
    }
}

/// innermostBinding: the row for `name` whose scope contains `line`,
/// narrowest scope first (first-wins on ties, matching the TS loop).
/// isShadowedImportName (import-resolver.ts): a parameter or lexical local
/// that binds the name's root at the ref line shadows a same-named import
/// there — in `toStore(get, set)`, `get()` calls the parameter. Class members
/// also sit in `local` rows scoped to the class body, but a bare name never
/// reaches them, so they do not shadow.
impl KernelResolver {
    pub(super) fn is_shadowed_import_name(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if !is_esm_family(&r.language) || self.is_member_call_site(r) {
            return Ok(false);
        }
        let rows = self.bindings(&r.file_path)?;
        let root = r.reference_name.split('.').next().unwrap_or("");
        let Some(b) = innermost_binding(&rows, root, Some(r.line)) else { return Ok(false) };
        match b.kind.as_str() {
            "param" => Ok(true),
            "local" => Ok(match self.node_by_opt_id(b.node_id.as_deref())? {
                Some(n) => !matches!(n.kind.as_str(), "method" | "property" | "field"),
                None => true,
            }),
            _ => Ok(false),
        }
    }
}

impl KernelResolver {
    /// A parameter binds the bare name at the ref line. It has no node, so
    /// the name has no target, not even a same-file declaration.
    pub(super) fn is_param_shadowed(&mut self, r: &ResolveRefIn) -> Res<bool> {
        let rows = self.bindings(&r.file_path)?;
        Ok(innermost_binding(&rows, &r.reference_name, Some(r.line)).is_some_and(|b| b.kind == "param"))
    }

    /// isMemberCallSite (import-resolver.ts): a bare-named JS/TS call that is
    /// really a member call — the extractor keeps only `save` for
    /// `this.save()` and `(a.b).save()`. The ref column is the start of the
    /// call expression, so the name's first call-shaped occurrence from
    /// there, preceded by `.`, marks a receiver. An import binds a lexical
    /// name and never answers one.
    pub(super) fn is_member_call_site(&mut self, r: &ResolveRefIn) -> bool {
        self.member_call_receiver(r).is_some()
    }

    /// memberCallReceiver (import-resolver.ts): `Some(true)` for
    /// `this.x()` / `super.x()`, whose target is the enclosing class's
    /// member; `Some(false)` for a receiver whose type is unknown here
    /// (`this.#out.push()`, `a.b.filter()`); `None` without a receiver.
    pub(super) fn member_call_receiver(&mut self, r: &ResolveRefIn) -> Option<bool> {
        if r.reference_kind != "calls" || !is_esm_family(&r.language) || r.reference_name.contains('.') {
            return None;
        }
        let lines = self.read_file(&r.file_path)?;
        let first = (r.line - 1) as usize;
        let line = lines.get(first)?;
        // Svelte markup columns run one byte short (the extractor counts from
        // the `{`, not the expression), so `{...spread()}` lands on the
        // spread's last dot: back over the dots so the spread, which names
        // no receiver, starts the text.
        let at = js_slice(line, r.column as usize);
        let at = &line[line[..line.len() - at.len()].trim_end_matches('.').len()..];
        // A chain wrapped onto the next lines (`this.a.b\n  .filter(x)`):
        // the ref sits where the call expression starts, the name on a line
        // that continues it with `.`/`?.` or after a line ending in `.`.
        let mut end = first + 1;
        let mut tail = at.trim_end();
        while end < lines.len() && end - first <= MAX_WRAPPED_CHAIN_LINES {
            let next = lines[end].trim_start();
            if !(next.starts_with('.') || tail.ends_with('.')) {
                break;
            }
            tail = next.trim_end();
            end += 1;
        }
        if end == first + 1 {
            return member_call_at(at, &r.reference_name);
        }
        let mut text = at.to_string();
        for line in &lines[first + 1..end] {
            text.push('\n');
            text.push_str(line);
        }
        member_call_at(&text, &r.reference_name)
    }

    /// isUnknownReceiverBuiltInCall (import-resolver.ts): a JS built-in method
    /// name called on a receiver of unknown type — no project method is it.
    pub(super) fn is_unknown_receiver_built_in_call(&mut self, r: &ResolveRefIn) -> bool {
        JS_BUILT_IN_METHODS.contains(r.reference_name.as_str()) && self.member_call_receiver(r) == Some(false)
    }
}

/// Continuation lines `member_call_receiver` reads past the ref's own line.
const MAX_WRAPPED_CHAIN_LINES: usize = 16;

/// `i` moved past whitespace and `/* … */` comments.
fn skip_gap(b: &[u8], mut i: usize) -> usize {
    loop {
        match b.get(i) {
            Some(b' ' | b'\t' | b'\n' | b'\r') => i += 1,
            Some(b'/') if b.get(i + 1) == Some(&b'*') => match memchr::memmem::find(&b[i + 2..], b"*/") {
                Some(e) => i += 2 + e + 2,
                None => return i,
            },
            _ => return i,
        }
    }
}

/// `k` (an end offset) moved back over whitespace and `/* … */` comments.
fn skip_gap_back(b: &[u8], mut k: usize) -> usize {
    loop {
        match k.checked_sub(1).map(|p| b[p]) {
            Some(b' ' | b'\t' | b'\n' | b'\r') => k -= 1,
            Some(b'/') if k >= 2 && b[k - 2] == b'*' => match memchr::memmem::rfind(&b[..k - 2], b"/*") {
                Some(s) => k = s,
                None => return k,
            },
            _ => return k,
        }
    }
}

fn member_call_at(at: &str, name: &str) -> Option<bool> {
    let b = at.as_bytes();
    let ident = |i: Option<usize>| i.and_then(|i| b.get(i)).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$');
    for i in occurrences(at, name, 0) {
        if ident(i.checked_sub(1)) || ident(Some(i + name.len())) {
            continue;
        }
        let mut j = skip_gap(b, i + name.len());
        if b.get(j) == Some(&b'?') && b.get(j + 1) == Some(&b'.') {
            j = skip_gap(b, j + 2);
        }
        if !matches!(b.get(j), Some(b'(' | b'<' | b'`')) {
            continue;
        }
        let mut k = skip_gap_back(b, i);
        // `...name()` spreads the call's result; it has no receiver.
        if k == 0 || b[k - 1] != b'.' || at[..k].ends_with("...") {
            return None;
        }
        k -= 1;
        if k > 0 && b[k - 1] == b'?' {
            k -= 1;
        }
        k = skip_gap_back(b, k);
        let receiver = &at[..k];
        let is_self = ["this", "super"].iter().any(|s| {
            receiver.ends_with(s)
                && !matches!(
                    receiver.as_bytes().get(receiver.len().wrapping_sub(s.len() + 1)),
                    Some(c) if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$' || *c == b'.' || *c == b'#'
                )
        });
        return Some(is_self);
    }
    None
}

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

impl KernelResolver {
    /// Python reassignments share one lexical scope. A bare annotation does
    /// not replace a value; a future local still supplies shadowing evidence.
    pub(super) fn receiver_binding(&mut self, name: &str, site: &ResolveRefIn) -> Res<Option<KBinding>> {
        let rows = self.bindings(&site.file_path)?;
        let Some(binding) = innermost_binding(&rows, name, Some(site.line)) else { return Ok(None) };
        if site.language != "python" {
            return Ok(Some(binding.clone()));
        }
        let mut latest: Option<&KBinding> = None;
        for row in rows.iter().filter(|row| row.name == name
            && row.scope_start == binding.scope_start && row.scope_end == binding.scope_end
            && row.line <= site.line)
        {
            if latest.is_none_or(|best| row.line > best.line) && !self.python_annotation_only(row, site) {
                latest = Some(row);
            }
        }
        Ok(Some(latest.unwrap_or(binding).clone()))
    }

    /// A closure reads its captured cell when invoked, not when defined.
    /// Without invocation-order evidence, a later replacement cannot supply
    /// one reliable receiver type. A single future initialization is retained.
    pub(super) fn python_captured_receiver_mutates(&mut self, site: &ResolveRefIn) -> Res<bool> {
        if site.language != "python" || !matches!(site.reference_kind.as_str(), "calls" | "references" | "function_ref") {
            return Ok(false);
        }
        let receiver = match site.reference_name.split_once('.') {
            Some((root, _)) => root.to_string(),
            None => match self.bare_call_receiver(site)? {
                Some((_, receiver)) if matches!(receiver.as_str(), "self" | "cls") => receiver,
                _ => return Ok(false),
            },
        };
        let root = receiver.as_str();
        let Some(binding) = self.receiver_binding(root, site)? else { return Ok(false) };
        let rows = self.bindings(&site.file_path)?;
        let mut lines = Vec::new();
        for row in rows.iter().filter(|row| row.name == root
            && row.scope_start == binding.scope_start && row.scope_end == binding.scope_end)
        {
            if self.python_annotation_only(row, site) { continue; }
            // Importing another submodule retains the same package object.
            if row.line != binding.line && binding.kind == "import" && row.kind == "import"
                && self.python_package_import(&binding, site) && self.python_package_import(row, site)
            {
                continue;
            }
            lines.push(row.line);
        }
        let selected = lines.iter().filter(|line| **line <= site.line).max()
            .or_else(|| lines.iter().min());
        if !selected.is_some_and(|line| lines.iter().any(|next| *next > site.line.max(*line))) {
            return Ok(false);
        }
        let mut captured = binding.scope_start < self.enclosing_scope_start_line(&site.file_path, &site.language, site.line)?;
        // Lambdas and generator expressions have no graph function node.
        // Their bodies are deferred, but defaults and the first iterable are not.
        if let Some(source) = self.read_file(&site.file_path) {
            let text = source.text();
            if let Some(tree) = self.parsed_tree(&source, site) {
                let leaf = descendant_for_position(tree.root_node(), text,
                    ((site.line - 1).max(0) as usize, site.column.max(0) as usize));
                let at = leaf.start_byte();
                let mut current = Some(leaf);
                while let Some(node) = current {
                    current = node.parent();
                    if node.kind() == "lambda" {
                        let in_body = node.child_by_field_name("body")
                            .is_some_and(|body| body.start_byte() <= at && at < body.end_byte());
                        if !in_body { continue; }
                        if node.child_by_field_name("parameters").is_some_and(|params|
                            named_children(params).into_iter().any(|param| {
                                let name = param.child_by_field_name("name")
                                    .or_else(|| matches!(param.kind(), "list_splat_pattern" | "dictionary_splat_pattern")
                                        .then(|| param.named_child(0)).flatten()).unwrap_or(param);
                                name.kind() == "identifier" && &text[name.start_byte()..name.end_byte()] == root
                            })) { return Ok(false); }
                        captured = true;
                    } else if node.kind() == "generator_expression" {
                        let clauses: Vec<_> = named_children(node).into_iter()
                            .filter(|child| child.kind() == "for_in_clause").collect();
                        if clauses.first().and_then(|clause| clause.child_by_field_name("right"))
                            .is_some_and(|iterable| iterable.start_byte() <= at && at < iterable.end_byte()) { continue; }
                        if clauses.iter().filter_map(|clause| clause.child_by_field_name("left"))
                            .any(|target| {
                                let mut targets = vec![target];
                                while let Some(target) = targets.pop() {
                                    if target.kind() == "identifier" && &text[target.start_byte()..target.end_byte()] == root { return true; }
                                    targets.extend(named_children(target));
                                }
                                false
                            })
                        { return Ok(false); }
                        captured = true;
                    }
                }
            }
        }
        Ok(captured)
    }

    fn python_annotation_only(&mut self, binding: &KBinding, site: &ResolveRefIn) -> bool {
        if !matches!(binding.kind.as_str(), "local" | "decl") { return false; }
        let Some(file) = self.read_file(&site.file_path) else { return false };
        let row = (binding.line - 1).max(0) as usize;
        let Some(line) = file.get(row) else { return false };
        let trimmed = line.trim_start();
        if trimmed.strip_prefix(&binding.name).is_some_and(|tail| tail.trim_start().starts_with('=')) {
            return false;
        }
        let Some(tree) = self.parsed_tree(&file, site) else { return false };
        let point = tree_sitter::Point::new(row, line.len() - trimmed.len());
        let mut node = tree.root_node().named_descendant_for_point_range(point, point);
        while let Some(current) = node {
            if current.kind() == "assignment" {
                return current.child_by_field_name("type").is_some()
                    && current.child_by_field_name("right").is_none();
            }
            node = current.parent();
        }
        false
    }
}

/// Svelte and Astro: languages whose binding rows cover only the script
/// blocks, while the markup (and, in Svelte, a second `<script>`) sees each
/// script's top-level names.
pub(super) fn is_sfc_scoped_script(language: &str) -> bool {
    matches!(language, "svelte" | "astro")
}

/// A component script's top-level row for `name`: one whose scope no other
/// row's scope strictly contains. An import wins over a declaration.
pub(super) fn sfc_top_level_binding<'a>(rows: &'a [KBinding], name: &str) -> Option<&'a KBinding> {
    let top_level = |r: &KBinding| {
        !rows.iter().any(|o| {
            o.scope_start <= r.scope_start
                && r.scope_end <= o.scope_end
                && (o.scope_start, o.scope_end) != (r.scope_start, r.scope_end)
        })
    };
    let mut found = rows.iter().filter(|r| r.name == name && top_level(r));
    let first = found.next()?;
    if first.kind == "import" {
        return Some(first);
    }
    found.find(|r| r.kind == "import").or(Some(first))
}

/// Does the file's outermost binding scope (a component's script) declare `name`?
fn is_script_top_level_decl(rows: &[KBinding], name: &str) -> bool {
    let Some(start) = rows.iter().map(|r| r.scope_start).min() else { return false };
    let end = rows.iter().map(|r| r.scope_end).max().unwrap_or(start);
    rows.iter().any(|r| r.name == name && r.kind == "decl" && r.scope_start == start && r.scope_end == end)
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

/// How a bare import specifier relates to this repository.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BareImport {
    /// Relative, aliased, mapped or otherwise the project's own.
    Local,
    /// In the repository by manifest text alone, with no directory for it.
    UnmappedLocal,
    /// A package from outside the repository.
    External,
}

/// What places a package in this repository.
enum RepositoryPackage {
    /// The workspace loader maps it, or links it by name.
    Mapped,
    /// A manifest's own `name` or `workspace:` dependency, nothing more.
    Manifest,
    No,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_call_at_steps_over_multibyte_names() {
        assert_eq!(member_call_at("数据.数据()", "数据"), Some(false));
        assert_eq!(member_call_at("this.数据()", "数据"), Some(true));
        assert_eq!(member_call_at("x数据数据()", "数据"), None);
    }

    #[test]
    fn member_call_at_crosses_comments_and_wrapped_lines() {
        assert_eq!(member_call_at("this.findAll /*x*/().length", "findAll"), Some(true));
        assert_eq!(member_call_at("this /* a */ . /* b */ findAll()", "findAll"), Some(true));
        assert_eq!(member_call_at("this\n      .findAll().length", "findAll"), Some(true));
        assert_eq!(member_call_at("this.#items.list\n      .add(n)", "add"), Some(false));
        assert_eq!(member_call_at("findAll /*x*/ ()", "findAll"), None);
    }

    #[test]
    fn member_call_at_reads_a_spread_as_no_receiver() {
        assert_eq!(member_call_at("<p {...spread()}>", "spread"), None);
        assert_eq!(member_call_at("f(... spread())", "spread"), None);
        assert_eq!(member_call_at("f(...a.spread())", "spread"), Some(false));
    }
}
