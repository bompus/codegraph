//! isBuiltInOrExternal, the prefilter, and the binding predicates (index.ts, name-matcher.ts).

use super::*;

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
                if dot > 0 && GO_STDLIB_PACKAGES.contains(&name[..dot]) {
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
        if !is_esm_family(&r.language) && !is_sfc_language(&r.language) {
            return Ok(false);
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
        let Some(source) = source else { return Ok(false) };
        if source.starts_with('.') || source.starts_with('/') {
            return Ok(false);
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
            return Ok(!self.is_repository_package(provider, &r.file_path));
        }
        if source.starts_with('~') || source.starts_with('#') || source.starts_with('$') {
            return Ok(false);
        }
        if source.starts_with("@/") || source.starts_with("src/") {
            return Ok(false);
        }
        if self.is_alias_prefix(&source, &r.file_path) {
            return Ok(false);
        }
        if self.is_repository_package(package_name_of(&source), &r.file_path) {
            return Ok(false);
        }
        if self.workspaces.is_some() && self.resolve_workspace_import(&source).is_some() {
            return Ok(false);
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
                return Ok(false);
            }
        }
        Ok(true)
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
    pub(super) fn is_repository_package(&mut self, package: &str, from_file: &str) -> bool {
        if let Some(ws) = &self.workspaces {
            if ws.local_link_names.contains(package) || self.resolve_workspace_import(package).is_some() {
                return true;
            }
        }
        let mut dir = pos_dirname(from_file).to_string();
        loop {
            if dir == "." {
                dir.clear();
            }
            if self.manifest_own_packages(&dir).contains(package) {
                return true;
            }
            if dir.is_empty() {
                return false;
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
        let at = js_slice(lines.get(first)?, r.column as usize);
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
        if k == 0 || b[k - 1] != b'.' {
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
}
