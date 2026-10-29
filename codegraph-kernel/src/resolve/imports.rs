//! Import machinery (import-resolver.ts): mappings, re-exports, path resolution and the member descent.

use super::*;

impl KernelResolver {
    // -----------------------------------------------------------------------
    // Import machinery (import-resolver.ts)
    // -----------------------------------------------------------------------

    /// scopedAliasesForFile (path-aliases.ts): the aliases of the deepest
    /// nested config enclosing `from_file`.
    fn scoped_aliases_for(&self, from_file: &str) -> Option<&KernelAliasMapIn> {
        self.scoped_aliases
            .iter()
            .find(|s| {
                from_file.len() > s.dir.len()
                    && from_file.starts_with(&s.dir)
                    && from_file.as_bytes()[s.dir.len()] == b'/'
            })
            .map(|s| &s.map)
    }

    /// Whether a `paths` pattern of the file's nested config or the root
    /// config claims `import_path`.
    pub(super) fn is_alias_prefix(&self, import_path: &str, from_file: &str) -> bool {
        self.scoped_aliases_for(from_file)
            .into_iter()
            .chain(self.aliases.as_ref())
            .any(|m| m.patterns.iter().any(|p| !p.prefix.is_empty() && import_path.starts_with(&p.prefix)))
    }

    /// isExternalImport (import-resolver.ts), context-aware.
    pub(super) fn is_external_import(&self, import_path: &str, language: &str, from_file: &str) -> bool {
        if import_path.starts_with('.') {
            return false;
        }
        if self.workspaces.is_some() && self.resolve_workspace_import(import_path, from_file).is_some() {
            return false;
        }
        if is_esm_import_language(language) {
            if ESM_BUILTIN_MODULES.contains(&import_path) {
                return true;
            }
            if self.is_alias_prefix(import_path, from_file) {
                return false;
            }
            if !import_path.starts_with("@/")
                && !import_path.starts_with("~/")
                && !import_path.starts_with("src/")
            {
                return true;
            }
        }
        if language == "python" {
            let head = import_path.split('.').next().unwrap_or("");
            if PYTHON_STDLIB_HEADS.contains(&head) {
                return true;
            }
        }
        if language == "go" {
            if import_path.starts_with('.') {
                return false;
            }
            if self.go_package_dir(import_path, from_file).is_some() { return false; }
            if import_path.contains("/internal/") {
                return false;
            }
            return true;
        }
        if language == "c" || language == "cpp" {
            if C_CPP_STDLIB_HEADERS.contains(import_path) {
                return true;
            }
            let without_ext = import_path.strip_suffix(".h").unwrap_or(import_path);
            if C_CPP_STDLIB_HEADERS.contains(without_ext) {
                return true;
            }
        }
        false
    }

    /// The longest package name `import_path` names among `names` — `name` or
    /// `name/…` (longestName, workspace-packages.ts).
    fn longest_package_name<'a>(import_path: &str, names: impl Iterator<Item = &'a String>) -> Option<&'a str> {
        let mut best = None;
        for name in names {
            if (import_path == name.as_str() || import_path.starts_with(&format!("{}/", name)))
                && best.is_none_or(|b: &str| name.len() > b.len())
            {
                best = Some(name.as_str());
            }
        }
        best
    }

    /// matchPackage (workspace-packages.ts): the path dependency the nearest
    /// manifest above `from_file` maps the import to, else a workspace member.
    fn match_workspace_package(&self, import_path: &str, from_file: &str) -> Option<(String, String)> {
        let ws = self.workspaces.as_ref()?;
        // scopedPackage: manifests above the importing file declare their own
        // `file:`/`link:`/`portal:` dependencies for the files under them.
        let mut dir = from_file.replace('\\', "/");
        while !dir.is_empty() {
            dir = dir.rfind('/').map_or(String::new(), |s| dir[..s].to_string());
            if let Some(names) = ws.scoped_by_name.get(&dir) {
                if let Some(name) = Self::longest_package_name(import_path, names.keys()) {
                    return Some((name.to_string(), names[name].clone()));
                }
            }
        }
        let name = Self::longest_package_name(import_path, ws.by_name.keys())?;
        Some((name.to_string(), ws.by_name[name].clone()))
    }

    /// resolveWorkspaceImport (workspace-packages.ts): sourceEntries first,
    /// then the matching package; a bare member import resolves to its
    /// declared entry when the manifest names one.
    pub(super) fn resolve_workspace_import(&self, import_path: &str, from_file: &str) -> Option<String> {
        let ws = self.workspaces.as_ref()?;
        if let Some((_, src)) = ws.source_entries.iter().find(|(k, _)| k == import_path) {
            return Some(src.clone());
        }
        let (name, dir) = self.match_workspace_package(import_path, from_file)?;
        let subpath = &import_path[name.len()..]; // '' or '/widgets'
        if subpath.is_empty() {
            if let Some(entry_by_name) = &ws.entry_by_name {
                if let Some(entry) = entry_by_name.get(&name) {
                    return Some(entry.clone());
                }
            }
        }
        let joined = format!("{}{}", dir, subpath);
        fn multi_slash() -> Rc<Regex> {
            re!(r"/{2,}")
        }
        Some(multi_slash().replace_all(&joined, "/").to_string())
    }

    /// workspaceImportTargets (workspace-packages.ts): what the package's
    /// manifest declares for the import, and its directory form.
    fn workspace_import_targets(&self, import_path: &str, from_file: &str) -> Option<WorkspaceTargets> {
        let ws = self.workspaces.as_ref()?;
        let (name, dir) = self.match_workspace_package(import_path, from_file)?;
        let subpath = &import_path[name.len()..]; // '' or '/widgets'
        let joined = format!("{}{}", dir, subpath);
        let directory = re!(r"/{2,}").replace_all(&joined, "/").to_string();
        let ohpm = if subpath.is_empty() {
            ws.entry_by_name.as_ref().and_then(|m| m.get(&name)).cloned()
        } else {
            None
        };
        let declared = match ohpm {
            Some(entry) => vec![entry],
            None => self.declared_workspace_entries(dir.as_str(), subpath),
        };
        Some(WorkspaceTargets { declared, directory, package_dir: dir })
    }

    /// declaredEntries: the targets a package's manifest declares for
    /// `subpath` (`''` or `/widgets`), best first: its `exports` entry, then
    /// the root's `main`-style fields.
    fn declared_workspace_entries(&self, package_dir: &str, subpath: &str) -> Vec<String> {
        let Some(entries) = self.workspaces.as_ref().and_then(|ws| ws.entries_by_dir.get(package_dir)) else {
            return vec![];
        };
        let exported = exported_targets(&entries.exports, &format!(".{subpath}"));
        if !subpath.is_empty() {
            return exported;
        }
        let mut out = exported;
        for m in &entries.main {
            if !out.contains(m) {
                out.push(m.clone());
            }
        }
        out
    }

    /// indexedTarget (import-resolver.ts): `target` as a file the index
    /// holds — as written, with the language's extensions or index files, or
    /// as the source of an emitted `.js`. Only the index counts: build output
    /// a local build left on disk is not linked.
    fn indexed_target(&self, target: &str, language: &str) -> Option<String> {
        let as_is = indexed_spelling(target, language);
        if self.known_file(&as_is) {
            return Some(as_is);
        }
        for ext in extension_resolution(language) {
            let candidate = indexed_spelling(&format!("{target}{ext}"), language);
            if self.known_file(&candidate) {
                return Some(candidate);
            }
        }
        // findSourceForEmittedSpecifier restricted to the index.
        let sources = emitted_to_source(&as_is, language)?;
        let stem_len = as_is.rfind('.')?;
        let stem = &as_is[..stem_len];
        for ext in sources {
            let candidate = format!("{}{}", stem, ext);
            if self.known_file(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// builtSourceOf: `target`, build output the index lacks, as the indexed
    /// source it is built from (`dist/index.js` → `src/index.ts`).
    fn built_source_of(&self, target: &str, package_dir: &str, language: &str) -> Option<String> {
        for source in built_sources_of(target, package_dir) {
            if let Some(hit) = self.indexed_target(&source, language) {
                return Some(hit);
            }
        }
        None
    }

    /// declaredWorkspaceEntry: the first declared target the index holds, or
    /// that is build output the index lacks read as its source.
    fn declared_workspace_entry(&self, targets: &WorkspaceTargets, language: &str) -> Option<String> {
        for target in &targets.declared {
            if let Some(hit) = self
                .indexed_target(target, language)
                .or_else(|| self.built_source_of(target, &targets.package_dir, language))
            {
                return Some(hit);
            }
        }
        None
    }

    /// applyAliases (path-aliases.ts): candidate paths relative to
    /// projectRoot in tsconfig priority order.
    pub(super) fn apply_aliases(&self, import_path: &str, alias_map: &KernelAliasMapIn) -> Vec<String> {
        for pat in &alias_map.patterns {
            if !import_path.starts_with(&pat.prefix) {
                continue;
            }
            if !pat.suffix.is_empty() && !import_path.ends_with(&pat.suffix) {
                continue;
            }
            // `foo*oo` does not match `foo`: the literals may not overlap
            // (TypeScript's isPatternMatch), and slicing them would panic.
            if pat.has_wildcard && import_path.len() < pat.prefix.len() + pat.suffix.len() {
                continue;
            }
            let captured = if pat.has_wildcard {
                &import_path[pat.prefix.len()..import_path.len() - pat.suffix.len()]
            } else if import_path != pat.prefix {
                continue;
            } else {
                ""
            };
            let mut out = Vec::new();
            for target in &pat.replacements {
                let filled = if pat.has_wildcard {
                    target.replacen('*', captured, 1)
                } else {
                    target.clone()
                };
                let base = alias_map.base_url.as_deref().unwrap_or(&self.project_root);
                let absolute = pos_resolve(base, &filled);
                let relative = pos_relative(&self.root_abs, &absolute).replace('\\', "/");
                if relative.starts_with("..") {
                    continue;
                }
                out.push(relative);
            }
            return out;
        }
        Vec::new()
    }

    /// findSourceForEmittedSpecifier (import-resolver.ts).
    pub(super) fn find_source_for_emitted(&self, relative_path: &str, language: &str) -> Option<String> {
        let sources = emitted_to_source(relative_path, language)?;
        let stem_len = relative_path.rfind('.')?; // safe: an emitted suffix matched
        let stem = &relative_path[..stem_len];
        for ext in sources {
            let candidate = format!("{}{}", stem, ext);
            if self.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// Resolve a non-Python relative import; Python uses its source-root reader.
    pub(super) fn resolve_relative_import(
        &mut self,
        import_path: &str,
        from_dir: &str,
        language: &str,
    ) -> Res<Option<String>> {
        let base_path = pos_resolve(from_dir, import_path);
        let relative_path = pos_relative(&self.root_abs, &base_path);
        Ok(self.probe_extensions(&relative_path, language))
    }

    /// The first existing file among `base` + each of the language's
    /// extensions, then `base` itself, then the source an emitted `.js`/`.d.ts`
    /// path was compiled from — each spelled as the index keys it
    /// (indexedSpelling, upstream 8651990a / #2577).
    pub(super) fn probe_extensions(&self, base: &str, language: &str) -> Option<String> {
        extension_resolution(language)
            .iter()
            .filter(|ext| !base.is_empty() || ext.starts_with('/'))
            .map(|ext| if base.is_empty() { ext.trim_start_matches('/').to_string() } else { format!("{base}{ext}") })
            .map(|candidate| indexed_spelling(&candidate, language))
            .find(|candidate| self.file_exists(candidate))
            .or_else(|| {
                let as_is = indexed_spelling(base, language);
                self.file_exists(&as_is).then_some(as_is)
            })
            .or_else(|| self.find_source_for_emitted(&indexed_spelling(base, language), language))
    }

    /// resolveAliasedImport (import-resolver.ts): tsconfig paths → workspace
    /// → hardcoded fallbacks → direct path.
    pub(super) fn resolve_aliased_import(
        &mut self,
        import_path: &str,
        language: &str,
        from_file: &str,
    ) -> Res<Option<String>> {
        // The nearest nested config first. Its targets can name build output
        // the checkout lacks (`"vitest": ["./dist/index.d.ts"]`), so a miss or
        // a declaration-only hit falls back to the root config's aliases and
        // the workspace package's source.
        let scoped = self
            .scoped_aliases_for(from_file)
            .map(|m| self.apply_aliases(import_path, m))
            .unwrap_or_default();
        let mut declaration = None;
        for c in scoped {
            if let Some(hit) = self.probe_extensions(&c, language) {
                if !is_declaration_file(&hit) {
                    return Ok(Some(hit));
                }
                declaration.get_or_insert(hit);
            }
        }
        let root = self
            .aliases
            .as_ref()
            .map(|m| self.apply_aliases(import_path, m))
            .unwrap_or_default();
        for c in root {
            if let Some(hit) = self.probe_extensions(&c, language) {
                return Ok(Some(hit));
            }
        }
        if self.workspaces.is_some() {
            // The entry the package's manifest declares for the import (#2502),
            // else the member's directory where extension/index permutations
            // find its barrel, else the source its build output mirrors.
            if let Some(targets) = self.workspace_import_targets(import_path, from_file) {
                if let Some(declared) = self.declared_workspace_entry(&targets, language) {
                    return Ok(Some(declared));
                }
                let direct = self.probe_extensions(&targets.directory, language);
                if direct.as_ref().is_some_and(|d| self.known_file(d)) {
                    return Ok(direct);
                }
                if let Some(built) = self.built_source_of(&targets.directory, &targets.package_dir, language) {
                    return Ok(Some(built));
                }
                if direct.is_some() {
                    return Ok(direct);
                }
            }
        }
        if declaration.is_some() {
            return Ok(declaration);
        }
        for (alias, replacement) in FALLBACK_ALIASES {
            if let Some(rest) = import_path.strip_prefix(alias) {
                let rewritten = format!("{}{}", replacement, rest);
                if let Some(hit) = self.probe_extensions(&rewritten, language) {
                    return Ok(Some(hit));
                }
            }
        }
        Ok(self.probe_extensions(import_path, language))
    }

    /// aliasAndBaseUrlCandidates (path-aliases.ts): where a bare specifier can
    /// live under `aliases`, in the order tsc tries them: the targets of the
    /// most specific pattern it matches, then the specifier read from
    /// `baseUrl` when the config declares one.
    fn alias_and_base_url_candidates(&self, import_path: &str, aliases: &KernelAliasMapIn) -> Vec<String> {
        let mut out = self.apply_aliases(import_path, aliases);
        if aliases.declares_base_url == Some(true) {
            let base = aliases.base_url.as_deref().unwrap_or(&self.project_root);
            let absolute = pos_resolve(base, import_path);
            let relative = pos_relative(&self.root_abs, &absolute).replace('\\', "/");
            if !relative.is_empty() && !relative.starts_with("..") && !relative.starts_with('/') {
                out.push(relative);
            }
        }
        out
    }

    /// isDeclaredOutsidePackage (index.ts): a Node built-in, or a package the
    /// importing file's package.json or an enclosing one lists as a
    /// dependency; a framework's virtual module unless this repository is
    /// that framework.
    pub(super) fn is_declared_outside_package(&mut self, source: &str, from_file: &str) -> bool {
        if ["node:", "bun:", "jsr:", "npm:", "http:", "https:"].iter().any(|p| source.starts_with(p))
            || source.starts_with("@std/")
            || self.node_builtins.contains(source)
        {
            return true;
        }
        let provider = if source.starts_with("astro:") {
            Some("astro")
        } else if re!(r"^\$(?:app|env|service-worker)(?:/|$)").is_match(source) {
            Some("@sveltejs/kit")
        } else {
            None
        };
        let package = package_name_of(source).to_string();
        match self.repository_package(&package, from_file) {
            RepositoryPackage::External => true,
            RepositoryPackage::Manifest | RepositoryPackage::Mapped => false,
            RepositoryPackage::No => provider.is_some_and(|p| matches!(self.repository_package(p, from_file), RepositoryPackage::No)),
        }
    }

    /// resolveFromOwnConfig (import-resolver.ts): a bare specifier the
    /// importing app's own `paths` or `baseUrl` map to a file the index holds.
    /// A package the app declares (or a Node built-in) never binds to a
    /// project module, only to a declaration file the project keeps for it.
    fn resolve_from_own_config(&mut self, import_path: &str, from_file: &str, language: &str) -> Res<Option<String>> {
        if !is_esm_import_language(language)
            || import_path.is_empty()
            || import_path.starts_with('/')
            || re!(r"^[A-Za-z][\w+.-]*:").is_match(import_path)
        {
            return Ok(None);
        }
        let candidates = match self.scoped_aliases_for(from_file).or(self.aliases.as_ref()) {
            Some(map) => self.alias_and_base_url_candidates(import_path, map),
            None => return Ok(None),
        };
        if candidates.is_empty() {
            return Ok(None);
        }
        if self.is_declared_outside_package(import_path, from_file) {
            if !extension_resolution(language).contains(&".d.ts") {
                return Ok(None);
            }
            for c in candidates {
                let declarations = format!("{c}.d.ts");
                if self.file_exists(&declarations) && self.known_file(&declarations) {
                    return Ok(Some(declarations));
                }
            }
            return Ok(None);
        }
        for c in candidates {
            if let Some(hit) = self.probe_extensions(&c, language) {
                if self.known_file(&hit) {
                    return Ok(Some(hit));
                }
            }
        }
        Ok(None)
    }

    /// resolveCppIncludePath (import-resolver.ts): -I dir scan with the
    /// language's extension list, then the path as-is.
    pub(super) fn resolve_cpp_include_path(&mut self, import_path: &str, language: &str) -> Option<String> {
        let extensions = extension_resolution(language);
        for dir in self.cpp_include_dirs.clone() {
            let normalized_dir = dir.replace('\\', "/");
            for ext in extensions {
                let candidate = format!("{}/{}{}", normalized_dir, import_path, ext);
                if self.file_exists(&candidate) {
                    return Some(candidate);
                }
            }
            let candidate = format!("{}/{}", normalized_dir, import_path);
            if self.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// resolveImportPath (import-resolver.ts) — memoized exactly like the TS
    /// per-context WeakMap memo (key: language\0fromFile\0importPath).
    pub(super) fn resolve_import_path(
        &mut self,
        import_path: &str,
        from_file: &str,
        language: &str,
    ) -> Res<Option<String>> {
        let key = format!("{}\0{}\0{}", language, from_file, import_path);
        if let Some(hit) = self.import_path_memo.get(&key) {
            return Ok(hit.clone());
        }
        let resolved = self.resolve_import_path_uncached(import_path, from_file, language)?;
        self.import_path_memo.insert(key, resolved.clone());
        Ok(resolved)
    }

    pub(super) fn resolve_import_path_uncached(
        &mut self,
        import_path: &str,
        from_file: &str,
        language: &str,
    ) -> Res<Option<String>> {
        if language == "python" {
            return self.python_module_path(import_path, from_file);
        }
        // A COBOL copybook member names a library member, not a path — ahead
        // of the external check, which would take a bare name for a package.
        if language == "cobol" {
            return self.resolve_cobol_copybook(import_path, from_file);
        }
        if self.is_external_import(import_path, language, from_file) {
            // What still looks like a package may name a file through the
            // importing app's own config.
            return self.resolve_from_own_config(import_path, from_file, language);
        }
        let from_dir = pos_dirname(&pos_resolve(&self.root_abs, from_file)).to_string();
        if import_path.starts_with('.') {
            return self.resolve_relative_import(import_path, &from_dir, language);
        }
        if let Some(aliased) = self.resolve_aliased_import(import_path, language, from_file)? {
            return Ok(Some(aliased));
        }
        if language == "c" || language == "cpp" {
            return Ok(self.resolve_cpp_include_path(import_path, language));
        }
        Ok(None)
    }

    /// Resolve a Python module only through the importing file's source roots.
    pub(super) fn find_python_module_file(
        &mut self, module: &str, exclude_file: &str,
    ) -> Res<Option<Arc<KNode>>> {
        let Some(file) = self.python_module_path(module, exclude_file)? else { return Ok(None); };
        if file == exclude_file { return Ok(None); }
        Ok(self.nodes_in_file(&file)?.iter().find(|n| n.kind == "file").cloned())
    }

    /// resolveModuleImportToFile (import-resolver.ts): a whole-module import
    /// (`import * as ns`, `import def`, Python `from . import mod`) links to
    /// the module FILE node.
    pub(super) fn resolve_module_import_to_file(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Res<Option<Arc<KNode>>> {
        if r.reference_kind != "imports" {
            return Ok(None);
        }
        // `ref.referenceName.includes('.')` — dead for a bare name.
        for imp in imports {
            if imp.local_name != r.reference_name {
                continue;
            }
            let module_path = if imp.is_namespace || imp.is_default {
                imp.source.clone()
            } else if r.language == "python" {
                let module_name = if imp.exported_name == "*" {
                    imp.local_name.clone()
                } else {
                    imp.exported_name.clone()
                };
                if imp.source.ends_with('.') {
                    format!("{}{}", imp.source, module_name)
                } else {
                    format!("{}.{}", imp.source, module_name)
                }
            } else {
                // A named TS/JS import binds a symbol, not a module.
                continue;
            };
            if let Some(resolved) =
                self.resolve_import_path(&module_path, &r.file_path, &r.language)?
            {
                if resolved != r.file_path {
                    if let Some(file_node) = self
                        .nodes_in_file(&resolved)?
                        .iter()
                        .find(|n| n.kind == "file")
                    {
                        return Ok(Some(file_node.clone()));
                    }
                }
            }
            if r.language == "python" {
                if let Some(mod_file) = self.find_python_module_file(&module_path, &r.file_path)? {
                    return Ok(Some(mod_file));
                }
            }
        }
        Ok(None)
    }

    /// resolveJavaImportedReference (import-resolver.ts) — the bare arm
    /// (`matchesBare` ⇒ memberName = localName) plus the static-import owner
    /// fallback, both over the FQN→file-suffix disambiguation.
    pub(super) fn resolve_java_imported_reference(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Res<Option<Arc<KNode>>> {
        if imports.is_empty() {
            return Ok(None);
        }
        // Java and Kotlin import each other's declarations (a Kotlin test's
        // `import …ApiBuilder.get` names a method in a .java file), so both
        // languages and both extensions count.
        let is_jvm = |language: &str| language == "java" || language == "kotlin";
        let ends_with_fqn = |file_path: &str, fqn: &str| {
            let base = fqn.replace('.', "/");
            [".java", ".kt"].iter().any(|ext| file_path.ends_with(&format!("{base}{ext}")))
        };
        // A bare import call names the callee directly, without a member dot before it.
        let bare_call_site = if r.reference_kind == "calls" {
            match self.read_file(&r.file_path) {
                Some(lines) => lines.get((r.line - 1) as usize).is_none_or(|line| {
                    !line[..js_unit_to_byte(line, r.column.max(0) as usize)].trim_end().ends_with('.')
                        && js_slice(line, r.column.max(0) as usize)
                        .strip_prefix(r.reference_name.as_str())
                        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                }),
                None => true,
            }
        } else {
            true
        };
        for imp in imports {
            let matches_bare = imp.local_name == r.reference_name;
            let matches_qualified = r
                .reference_name
                .starts_with(&format!("{}.", imp.local_name));
            if !matches_bare && !matches_qualified {
                continue;
            }
            if matches_bare && !bare_call_site {
                continue;
            }
            let member_name = if matches_bare {
                imp.local_name.clone()
            } else {
                js_slice(&r.reference_name, utf16_len(&imp.local_name) + 1)
                    .to_string()
            };
            let candidates = self.nodes_by_name(&member_name)?;
            for node in candidates.iter() {
                if is_jvm(&node.language) && ends_with_fqn(&node.file_path, &imp.source) {
                    return Ok(Some(node.clone()));
                }
            }
            // `import static com.example.Foo.bar;` — the FQN tail is the
            // member, the part before is the owner class. Bare matches only —
            // a qualified ref already named the member above.
            if matches_bare {
                if let Some(dot) = imp.source.rfind('.') {
                    if dot > 0 {
                        let owner = &imp.source[..dot];
                        for node in candidates.iter() {
                            if is_jvm(&node.language) && ends_with_fqn(&node.file_path, owner) {
                                return Ok(Some(node.clone()));
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// resolveViaImport for a name with no `.` or `/` (a bare name, a C++
    /// `::` type, a C/C++ include path): the member variant's `.`/`/`-gated
    /// arms cannot fire for it.
    pub(super) fn resolve_via_import(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        self.resolve_via_import_member(r)
    }

    /// isBoundToOutOfRepoImport (import-resolver.ts): for a bare name in a
    /// migrated language only the ESM arm can fire.
    pub(super) fn is_bound_to_out_of_repo_import(&mut self, r: &ResolveRefIn) -> Res<bool> {
        // Qualified refs resolve by path, not through a bare import binding.
        if r.reference_name.contains("::") || r.reference_name.contains('.') {
            return Ok(false);
        }
        // Rust: a `use` path rooted at a stdlib crate ships outside the repo
        // by definition. Deliberately NOT "the module path doesn't resolve":
        // `pub use other_crate::{ports}` re-exports have no file to walk yet
        // are in-repo — a 2015-edition crate-relative path that walks to a
        // real file shadows a stdlib root and stays local.
        if r.language == "rust" {
            let Some(content) = self.read_file(&r.file_path) else {
                return Ok(false);
            };
            let Some(use_path) = content.rust_uses().get(&r.reference_name) else {
                return Ok(false);
            };
            let segments: Vec<&str> = use_path.split("::").collect();
            if segments.len() < 2 || !RUST_STDLIB_ROOTS.contains(segments[0]) {
                return Ok(false);
            }
            return Ok(
                self.resolve_rust_module_file(&segments[..segments.len() - 1], &r.file_path)?
                    .is_none(),
            );
        }
        if !is_esm_import_language(&r.language) {
            return Ok(false);
        }
        for imp in self.import_mappings(&r.file_path)?.iter() {
            if imp.local_name != r.reference_name {
                continue;
            }
            let (source, language, file) = (imp.source.clone(), r.language.clone(), r.file_path.clone());
            if self.ts_ambient_lookup(&source, &imp.exported_name, imp.is_default, r)?.is_some() { return Ok(false); }
            return Ok(self.is_external_import(&source, &language, &file)
                && self.resolve_import_path(&source, &file, &language)?.is_none());
        }
        Ok(false)
    }

    /// isJsBareModuleRef (import-resolver.ts): the `imports` reference to the
    /// module of an import named by one bare segment. A name some import binds
    /// locally is that binding, not the module.
    pub(super) fn is_js_bare_module_ref(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.reference_kind != "imports" || !is_esm_import_language(&r.language) || is_js_path_import_ref(r) {
            return Ok(false);
        }
        let imports = self.import_mappings(&r.file_path)?;
        Ok(imports.iter().any(|m| m.source == r.reference_name) && !imports.iter().any(|m| m.local_name == r.reference_name))
    }

    /// resolveViaImport (import-resolver.ts): the C/C++ include arm, the ESM
    /// `import('./x')`-path arm, the go/java/python/lua/module-file arms, and
    /// the imports loop with the `localName.member` descent.
    pub(super) fn resolve_via_import_member(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if self.is_member_call_site(r) || self.is_shadowed_import_name(r)? {
            return Ok(None);
        }
        // C/C++ `#include` path refs: the including file's own directory first
        // (via a same-named file NODE, not just existence), then the include
        // search path. isCobolCopybookRef / isNixPathImportRef name
        // unmigrated languages; PHP includes follow this arm.
        if (r.language == "c" || r.language == "cpp") && r.reference_kind == "imports" {
            // Quoted-include search order: the including file's own directory
            // first, via a same-named file NODE (not just existence).
            let from_dir = pos_dirname(&r.file_path);
            let sibling_path = pos_join(from_dir, &r.reference_name);
            let sibling_base = pos_basename(&sibling_path).to_string();
            if let Some(sibling) = self
                .nodes_by_name(&sibling_base)?
                .iter()
                .find(|n| n.kind == "file" && n.file_path == sibling_path)
            {
                return Ok(Some(KCand {
                    node: sibling.clone(),
                    confidence: 0.92,
                    resolved_by: "import",
                }));
            }
            let Some(resolved_path) =
                self.resolve_import_path(&r.reference_name, &r.file_path, &r.language)?
            else {
                return Ok(None);
            };
            let basename = pos_basename(&resolved_path).to_string();
            if let Some(file_node) = self
                .nodes_by_name(&basename)?
                .iter()
                .find(|n| n.kind == "file" && n.file_path == resolved_path)
            {
                return Ok(Some(KCand {
                    node: file_node.clone(),
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
            return Ok(None);
        }
        // COBOL COPY / EXEC SQL INCLUDE: the copybook member to its file, or
        // nothing (compiler-supplied members like SQLCA stay unresolved).
        if is_cobol_copybook_ref(r) {
            return self.resolve_import_path_to_file_node(r);
        }
        // PHP include/require: the literal path resolves against the including
        // file's directory (php.ini `include_path` isn't modeled), with `.php`
        // tried when the literal omits it. A path that names no indexed file
        // is a dead end, never a name match.
        if is_php_include_path_ref(r) {
            let from_dir = pos_dirname(&pos_join(&self.root_abs, &r.file_path)).to_string();
            let rel = pos_relative(&self.root_abs, &pos_resolve(&from_dir, &r.reference_name));
            let resolved = if self.file_exists(&rel) {
                Some(rel)
            } else {
                let with_ext = format!("{rel}.php");
                self.file_exists(&with_ext).then_some(with_ext)
            };
            let Some(resolved) = resolved else { return Ok(None) };
            let basename = pos_basename(&resolved).to_string();
            return Ok(self
                .nodes_by_name(&basename)?
                .iter()
                .find(|n| n.kind == "file" && n.file_path == resolved)
                .map(|n| KCand { node: n.clone(), confidence: 0.9, resolved_by: "import" }));
        }
        // Nix static path imports resolve to file nodes only.
        if is_nix_path_import_ref(r) {
            return self.resolve_import_path_to_file_node(r);
        }
        // Module-path imports resolve to the exact indexed file or nothing,
        // including extensionless CommonJS paths and configured aliases.
        if is_js_path_import_ref(r) {
            let Some(resolved) =
                self.resolve_import_path(&r.reference_name, &r.file_path, &r.language)?
            else {
                return Ok(None);
            };
            if resolved == r.file_path {
                return Ok(None);
            }
            return Ok(self
                .nodes_in_file(&resolved)?
                .iter()
                .find(|n| n.kind == "file")
                .cloned()
                .map(|node| KCand { node, confidence: 0.9, resolved_by: "import" }));
        }
        // A module imported by one bare segment (`'config'` under
        // `"baseUrl": "src"`) is a file only when the importing app's config
        // finds one; a real package stays unresolved.
        if self.is_js_bare_module_ref(r)? {
            let Some(resolved) = self.resolve_import_path(&r.reference_name, &r.file_path, &r.language)? else {
                return Ok(None);
            };
            if resolved == r.file_path {
                return Ok(None);
            }
            return Ok(self
                .nodes_in_file(&resolved)?
                .iter()
                .find(|n| n.kind == "file")
                .cloned()
                .map(|node| KCand { node, confidence: 0.9, resolved_by: "import" }));
        }
        let imports = if r.language == "python" && r.reference_kind != "imports" {
            let root = r.reference_name.split('.').next().unwrap();
            match self.receiver_binding(root, r)? {
                Some(binding) if binding.kind == "import" => {
                    // Unaliased imports share the package object across scopes,
                    // so retain its file-wide submodule catalog. An explicit
                    // alias replaces its receiver value.
                    let mut selected = vec![binding.clone()];
                    if self.python_package_import(&binding, r) {
                        for row in self.bindings(&r.file_path)?.iter() {
                            if row.name == root && row.kind == "import" && row.target_spec != binding.target_spec
                                && self.python_package_import(row, r)
                            {
                                selected.push(row.clone());
                            }
                        }
                    }
                    Rc::new(self.import_mappings_from_bindings(&selected))
                }
                Some(_) => return Ok(None),
                None => self.import_mappings(&r.file_path)?,
            }
        } else {
            self.import_mappings(&r.file_path)?
        };
        if imports.is_empty() && self.read_file(&r.file_path).is_none() {
            return Ok(None);
        }

        if r.language == "go" {
            if let Some(c) = self.resolve_go_cross_package(r, &imports)? {
                return Ok(Some(c));
            }
        }
        if r.language == "kotlin" && r.reference_kind != "imports" {
            let (root,member)=r.reference_name.split_once('.').map(|(root,member)|(root,Some(member))).unwrap_or((&r.reference_name,None));
            if let Some(imp)=imports.iter().find(|m|m.local_name==root && m.local_name!=m.exported_name) {
                let parts:Vec<_>=imp.source.split('.').collect();
                for i in (1..parts.len()).rev() {
                    let qn=format!("{}::{}",parts[..i].join("."),parts[i..].join("::"));
                    if let Some(owner)=self.nodes_by_qualified_name(&qn)?.first().cloned() {
                        let target=if let Some(member)=member {if member.contains('.') {return Ok(None);}self.nodes_by_qualified_name(&format!("{}::{member}",owner.qualified_name))?.first().cloned()}else{Some(owner)};
                        return Ok(target.map(|node|KCand{node,confidence:0.9,resolved_by:"import"}));
                    }
                }
            }
        }
        if r.language == "java" || r.language == "kotlin" {
            if let Some(node) = self.resolve_java_imported_reference(r, &imports)? {
                return Ok(Some(KCand {
                    node,
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }
        if r.language == "python" {
            for imp in imports.iter().filter(|m| !m.is_namespace && m.local_name == r.reference_name) {
                if let Some(file) = self.python_module_path(&imp.source, &r.file_path)? {
                    if let Some(node) = self.python_module_symbol(&file, &imp.exported_name, 0, &mut HashSet::new())? {
                        return Ok(Some(KCand { node, confidence: 0.9, resolved_by: "import" }));
                    }
                }
            }
            if let Some(c) = self.resolve_python_module_member(r, &imports)? {
                return Ok(Some(c));
            }
            if let Some(c) = self.resolve_python_absolute_module(r)? {
                return Ok(Some(c));
            }
        }
        // (Rust `::` paths never reach here — resolve_rust_path_ref runs
        // ahead of the route.)
        if let Some(c) = self.resolve_lua_require(r)? {
            return Ok(Some(c));
        }
        if matches!(
            r.language.as_str(),
            "python" | "typescript" | "tsx" | "javascript" | "jsx" | "arkts"
        ) {
            if let Some(node) = self.resolve_module_import_to_file(r, &imports)? {
                return Ok(Some(KCand {
                    node,
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }

        for imp in imports.iter() {
            let is_member = r
                .reference_name
                .starts_with(&format!("{}.", imp.local_name));
            if imp.local_name != r.reference_name && !is_member {
                continue;
            }
            let mut resolved_path =
                self.resolve_import_path(&imp.source, &r.file_path, &r.language)?;
            if resolved_path.is_none() && r.language == "python" {
                resolved_path = self
                    .find_python_module_file(&imp.source, &r.file_path)?
                    .map(|n| n.file_path.clone());
            }
            let Some(resolved_path) = resolved_path else { continue };
            let want = ExportWant {
                is_default: imp.is_default,
                is_namespace: imp.is_namespace,
                exported_name: if imp.is_default {
                    "default".to_string()
                } else {
                    imp.exported_name.clone()
                },
                // JS String.replace(string) removes the FIRST occurrence
                // anywhere — replacen(.., 1) matches, including the no-match
                // case that leaves the whole name as the member.
                member_name: if imp.is_namespace {
                    Some(r.reference_name.replacen(
                        &format!("{}.", imp.local_name),
                        "",
                        1,
                    ))
                } else {
                    None
                },
            };
            let mut visited = HashSet::new();
            let mut target = None;
            if r.reference_kind == "calls" && is_js_family(&r.language) {
                let alternatives: &[&str] = if resolved_path.ends_with(".d.mts") { &[".mts", ".mjs"] }
                    else if resolved_path.ends_with(".d.cts") { &[".cts", ".cjs"] }
                    else { &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"] };
                if let Some(stem) = [".d.ts", ".d.mts", ".d.cts"].iter().find_map(|suffix| resolved_path.strip_suffix(suffix)) {
                    for extension in alternatives {
                        let implementation = format!("{stem}{extension}");
                        if !self.file_exists(&implementation) { continue; }
                        target = self.find_exported_symbol(&implementation, &want, &r.language, &mut HashSet::new(), 0)?;
                        if target.is_some() { break; }
                    }
                }
            }
            if target.is_none() {
                target = self.find_exported_symbol(&resolved_path, &want, &r.language, &mut visited, 0)?;
            }
            let target = if target.is_none() && r.language == "python" { self.python_module_symbol(&resolved_path,want.member_name.as_deref().unwrap_or(&want.exported_name),0,&mut HashSet::new())? } else { target };
            let Some(target) = target
            else {
                // `import api from './api'` where the module ends in
                // `export default new ApiClient()`: no node holds the value,
                // so type it from the expression and find the method there.
                if imp.is_default && is_member && r.reference_kind == "calls" && is_esm_family(&r.language) {
                    let member = &r.reference_name[imp.local_name.len() + 1..];
                    if !member.is_empty() && !member.contains('.') {
                        if let Some(c) = self.resolve_default_instance_member(&resolved_path, member, r)? {
                            return Ok(Some(c));
                        }
                    }
                }
                continue;
            };
            if !imp.is_namespace && is_member {
                if let Some(member_node) = self.resolve_static_member(&target, r, &imp.local_name)? {
                    return Ok(Some(KCand {
                        node: member_node,
                        confidence: 0.9,
                        resolved_by: "import",
                    }));
                }
                if target.kind == "constant" || target.kind == "variable" {
                    // `if (member)` — an empty first segment skips the
                    // literal/alias arms entirely in TS.
                    let member0 = js_slice(
                        &r.reference_name,
                        utf16_len(&imp.local_name) + 1,
                    )
                    .split('.')
                    .next()
                    .unwrap_or("");
                    if !member0.is_empty() {
                        if let Some(lit) = self
                            .resolve_object_literal_member(&target, member0, r, 0.9, "import")?
                        {
                            return Ok(Some(lit));
                        }
                        if let Some(alias) = self.resolve_object_literal_alias(&target, member0, r)? {
                            return Ok(Some(alias));
                        }
                        if let Some(inst) = self.resolve_imported_instance_member(&target, member0, r)? {
                            return Ok(Some(inst));
                        }
                    }
                }
                // A named Python import proves the receiver exists, not the
                // member: `task.delay()` enqueues work, it does not call the
                // task function (#2040). Unknown members stay unresolved.
                if r.language == "python" {
                    return Ok(None);
                }
                // resolveImportedInstanceMember returns null for non-const/var
                // targets before reading anything — for them the decline rule
                // is the only remaining arm.
                if r.reference_kind == "calls"
                    && (target.kind == "function" || target.kind == "method")
                {
                    return Ok(None);
                }
            }
            return Ok(Some(KCand {
                node: target,
                confidence: 0.9,
                resolved_by: "import",
            }));
        }
        // A barrel's own `export { A, B as C } from './x'` names `A` and `B` the
        // way the module calls them, and binds neither, so no import above holds
        // them. Each resolves through the module the statement names, as an
        // import does: looked up by its name alone, it reached a namesake
        // elsewhere in the project.
        if r.reference_kind == "imports" && is_esm_import_language(&r.language) {
            if let Some(source) = self.re_export_source(&r.reference_name, r)? {
                if let Some(resolved_path) = self.resolve_import_path(&source, &r.file_path, &r.language)? {
                    let want = ExportWant {
                        is_default: false,
                        is_namespace: false,
                        exported_name: r.reference_name.clone(),
                        member_name: None,
                    };
                    let mut visited = HashSet::new();
                    if let Some(node) = self.find_exported_symbol(&resolved_path, &want, &r.language, &mut visited, 0)? {
                        return Ok(Some(KCand { node, confidence: 0.9, resolved_by: "import" }));
                    }
                }
            }
        }
        Ok(None)
    }

    /// reExportSource (name-matcher.ts): the module the file passes `name` on
    /// from with `export { name } from './x'` (or `export { name as alias } from
    /// './x'`), `name` being what that module calls it, when exactly one
    /// statement of the file does. A re-export binds no local name, so the
    /// file's import bindings never hold it.
    pub(super) fn re_export_source(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        let mut source: Option<String> = None;
        for rex in self.reexports(&r.file_path)?.iter() {
            if rex.kind != "named" || rex.original_name.as_deref() != Some(name) {
                continue;
            }
            if source.as_deref().is_some_and(|s| s != rex.source) {
                return Ok(None);
            }
            source = Some(rex.source.clone());
        }
        Ok(source)
    }

    /// resolveObjectLiteralAlias (import-resolver.ts): `Api.upload()` where
    /// `Api` is `{ upload, other: impl }` — the binding a shorthand or
    /// `key: ident` property names in the object's file (#1932).
    fn resolve_object_literal_alias(
        &mut self,
        container: &Arc<KNode>,
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if container.kind != "constant" && container.kind != "variable" {
            return Ok(None);
        }
        if !re!(r"\.(?:[cm]?[jt]sx?)$").is_match(&container.file_path) {
            return Ok(None);
        }
        Ok(self
            .resolve_object_literal_binding(container, member, r)?
            .map(|hit| KCand { confidence: 0.9, resolved_by: "import", ..hit }))
    }

    /// resolveImportedInstanceMember (import-resolver.ts): `store.notify()`
    /// where `store` is `export const store = new Store()` — type the value
    /// from its own declaration lines (the local receiver patterns, over the
    /// joined extent) and validate the member on that type.
    fn resolve_imported_instance_member(
        &mut self,
        value: &Arc<KNode>,
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if r.reference_kind != "calls" {
            return Ok(None);
        }
        let pats = local_receiver_type_patterns(&value.language);
        if pats.is_empty() {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&value.file_path) else {
            return Ok(None);
        };
        let from = ((value.start_line - 1).max(0) as usize).min(lines.len());
        let to = (value.end_line.max(0) as usize).clamp(from, lines.len());
        let decl = lines[from..to].join("\n");
        for pat in pats {
            let Some(type_name) =
                self.infer_match_text(&decl, &value.name, std::slice::from_ref(pat), false)?
            else {
                continue;
            };
            if let Some(c) = self.method_on_module_class(
                &value.file_path,
                &value.language,
                value.start_line,
                &type_name,
                member,
                r,
            )? {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }

    /// The class, interface or type alias a module binds to `type_name`, the type of a
    /// value it exports (`new Store()`, `: ViteDevServer`): one it declares
    /// itself, or the one an import of it from a project file resolves to.
    /// A type imported from a package, or a name the module does not bind,
    /// is none.
    fn module_bound_class(
        &mut self,
        module_path: &str,
        language: &str,
        line: i64,
        type_name: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        if !type_name.contains('.') {
            if let Some(own) = self
                .nodes_in_file(module_path)?
                .iter()
                .find(|n| n.name == type_name && matches!(n.kind.as_str(), "class" | "interface" | "type_alias"))
            {
                return Ok(Some(own.clone()));
            }
        }
        let head = type_name.split('.').next().unwrap_or("");
        let Some(imp) = self
            .import_mappings(module_path)?
            .iter()
            .find(|m| m.local_name == head)
            .cloned()
        else {
            return Ok(None);
        };
        if self.is_external_import(&imp.source, language, module_path) {
            return Ok(None);
        }
        let mut type_ref = r.clone().naming(type_name, "references");
        type_ref.from_node_id = format!("file:{module_path}");
        type_ref.file_path = module_path.to_string();
        type_ref.language = language.to_string();
        type_ref.line = line;
        type_ref.column = 0;
        Ok(self
            .resolve_via_import(&type_ref)?
            .map(|c| c.node)
            .filter(|n| matches!(n.kind.as_str(), "class" | "interface" | "type_alias")))
    }

    /// `member` on the class `module_path` binds to `type_name`: the class's
    /// own method, else one it inherits. A same-named class in another file
    /// never answers for it.
    fn method_on_module_class(
        &mut self,
        module_path: &str,
        language: &str,
        line: i64,
        type_name: &str,
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let Some(class) = self.module_bound_class(module_path, language, line, type_name, r)? else {
            return Ok(None);
        };
        let mut site = r.clone();
        site.file_path = class.file_path.clone();
        // A single-file component's script names a TS class like a TS file does.
        if is_esm_import_language(&site.language) {
            site.language = class.language.clone();
        }
        let Some(c) = self.resolve_method_on_type(&class.name, member, &site, 0.85, "instance-method", is_esm_import_language(&class.language).then_some(class.file_path.as_str()))? else {
            return Ok(None);
        };
        let own = format!("{}::{member}", class.name);
        let same_named = c.node.qualified_name == own || c.node.qualified_name.ends_with(&format!("::{own}"));
        if same_named && c.node.file_path != class.file_path {
            return Ok(None);
        }
        Ok(Some(c))
    }

    /// A member call on a module's anonymous default instance,
    /// `export default new ApiClient()`: the method on the class the
    /// exporting module binds to that name.
    fn resolve_default_instance_member(
        &mut self,
        module_path: &str,
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let Some(lines) = self.read_file(module_path) else { return Ok(None) };
        let text = lines.text();
        let Some((type_name, at)) = re!(r"(?m)^[ \t]*export[ \t]+default[ \t]+new[ \t]+([A-Za-z_$][A-Za-z0-9_$]*)[ \t]*[(<]")
            .captures(text)
            .and_then(|c| c.get(1))
            .map(|m| (m.as_str().to_string(), m.start()))
        else {
            return Ok(None);
        };
        let line = text[..at].matches('\n').count() as i64 + 1;
        let language = r.language.clone();
        self.method_on_module_class(module_path, &language, line, &type_name, member, r)
    }

    /// getRazorUsings: the file's own `@using` namespaces, then each
    /// `_Imports.razor` from its directory up to the project root.
    fn razor_usings(&mut self, file_path: &str) -> Rc<Vec<String>> {
        if let Some(hit) = self.razor_usings_memo.get(file_path) {
            return hit.clone();
        }
        let using = re!(r"(?m)^\s*@using\s+(?:static\s+)?([A-Za-z_][A-Za-z0-9_.]*)");
        let mut out: Vec<String> = Vec::new();
        let add_from = |this: &mut Self, path: &str, out: &mut Vec<String>| {
            if let Some(lines) = this.read_file(path) {
                for c in using.captures_iter(lines.text()) {
                    let ns = c[1].to_string();
                    if !out.contains(&ns) {
                        out.push(ns);
                    }
                }
            }
        };
        add_from(self, file_path, &mut out);
        let mut dir = file_path.rfind('/').map_or("", |i| &file_path[..i]).to_string();
        loop {
            let imports = if dir.is_empty() { "_Imports.razor".to_string() } else { format!("{dir}/_Imports.razor") };
            add_from(self, &imports, &mut out);
            if dir.is_empty() {
                break;
            }
            dir = dir.rfind('/').map_or(String::new(), |i| dir[..i].to_string());
        }
        let out = Rc::new(out);
        self.razor_usings_memo.insert(file_path.to_string(), out.clone());
        out
    }

    /// resolveRazorUsing: a simple type resolves when exactly one node is
    /// `<using>::<name>` across the file's `@using` set.
    pub(super) fn resolve_razor_using(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.reference_name.contains('.') || r.reference_name.contains("::") {
            return Ok(None);
        }
        let usings = self.razor_usings(&r.file_path);
        let mut found: Vec<Arc<KNode>> = Vec::new();
        for ns in usings.iter() {
            for cand in self.nodes_by_qualified_name(&format!("{ns}::{}", r.reference_name))?.iter() {
                if !found.iter().any(|f| f.id == cand.id) {
                    found.push(cand.clone());
                }
            }
        }
        if found.len() != 1 {
            return Ok(None);
        }
        Ok(Some(KCand { node: found.pop().unwrap(), confidence: 0.9, resolved_by: "import" }))
    }

    /// resolveCfmlComponentPath: `extends="../base"` resolves against the
    /// referencing file's directory to an exact `.cfc` (case-insensitive);
    /// `extends="coldbox.system.web.Controller"` matches the class segment and
    /// corroborates its directories right to left — at least one must agree,
    /// and a tie yields nothing.
    pub(super) fn resolve_cfml_component_path(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let norm = |p: &str| p.replace('\\', "/").to_lowercase();
        let candidates = |this: &Self, name: &str| -> Res<Vec<Arc<KNode>>> {
            Ok(this
                .nodes_by_name(name)?
                .iter()
                .filter(|n| (n.kind == "class" || n.kind == "interface") && (n.language == "cfml" || n.language == "cfscript"))
                .cloned()
                .collect())
        };
        if r.reference_name.contains('/') {
            let rel = re!(r"(?i)\.cfc$").replace(&r.reference_name, "").into_owned();
            let from = r.file_path.replace('\\', "/");
            let mut parts: Vec<&str> = from.split('/').collect();
            parts.pop();
            for seg in rel.split('/') {
                match seg {
                    "" | "." => {}
                    ".." => {
                        if parts.pop().is_none() {
                            return Ok(None);
                        }
                    }
                    s => parts.push(s),
                }
            }
            let want = norm(&format!("{}.cfc", parts.join("/")));
            let Some(class_name) = parts.last().filter(|s| !s.is_empty()) else { return Ok(None) };
            return Ok(candidates(self, class_name)?
                .into_iter()
                .find(|c| norm(&c.file_path) == want)
                .map(|node| KCand { node, confidence: 0.95, resolved_by: "file-path" }));
        }
        let segments: Vec<&str> = r.reference_name.split('.').map(str::trim).filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return Ok(None);
        }
        let class_name = segments[segments.len() - 1];
        let dir_segments = &segments[..segments.len() - 1];
        // Production code never names a component in a test suite; a test can
        // name one, by the path to it.
        let from_test = super::resolver_upstream::test_suite_path(&r.file_path);
        let mut best_score = 0f64;
        let mut tied: Vec<Arc<KNode>> = Vec::new();
        for cand in candidates(self, class_name)? {
            if !from_test && super::resolver_upstream::is_test_suite_path(&cand.file_path) {
                continue;
            }
            let path = cand.file_path.replace('\\', "/");
            let mut dirs: Vec<&str> = path.split('/').collect();
            dirs.pop();
            let mut score = 0usize;
            while score < dir_segments.len()
                && score < dirs.len()
                && dir_segments[dir_segments.len() - 1 - score].to_lowercase() == dirs[dirs.len() - 1 - score].to_lowercase()
            {
                score += 1;
            }
            // The one exception to corroboration: a one-folder path naming a
            // component at the project root, `wheels` the mapping onto it
            // (#2652); the file must be named like the component.
            let mut score = score as f64;
            if score == 0.0 && dirs.is_empty() && dir_segments.len() == 1 {
                let stem = cand.file_path.replace('\\', "/").to_lowercase();
                if stem.strip_suffix(".cfc").is_some_and(|s| s == class_name.to_lowercase()) {
                    score = 0.5;
                }
            }
            if score > best_score {
                best_score = score;
                tied = vec![cand];
            } else if score == best_score && score > 0.0 {
                tied.push(cand);
            }
        }
        // A test's tie between a component and a suite mirroring its path
        // names the component, as it does from production code (#2650).
        if tied.len() > 1 {
            tied.retain(|c| !super::resolver_upstream::is_test_suite_path(&c.file_path));
        }
        if best_score == 0.0 || tied.len() != 1 {
            return Ok(None);
        }
        Ok(tied
            .into_iter()
            .next()
            .map(|node| KCand { node, confidence: if best_score >= 1.0 { 0.9 } else { 0.8 }, resolved_by: "qualified-name" }))
    }

    /// resolveCfmlConstructedCall: a call chained onto a construction
    /// (#2652) — `createObject("component", "a.b.Batch").init()` or `new
    /// a.b.Batch().init()`, encoded `a.b.Batch().init`. The component is the
    /// one its path names, by resolveCfmlComponentPath's rules, or for a bare
    /// name the one beside the calling file, else the only one of that name.
    /// The method is the component's own or one it inherits.
    pub(super) fn resolve_cfml_constructed_call(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(m) = re!(r"^([A-Za-z_$][\w$.-]*)\(\)\.([A-Za-z_$][\w$]*)$").captures(&r.reference_name) else {
            return Ok(None);
        };
        let component = m[1].to_string();
        let cls = if component.contains('.') {
            let as_instantiates = r.clone().naming(&component, "instantiates");
            match self.resolve_cfml_component_path(&as_instantiates)? {
                Some(c) => Some(c.node),
                None => None,
            }
        } else {
            self.cfml_component_by_name(&component, &r.file_path)?
        };
        let Some(cls) = cls else { return Ok(None) };
        let method = self.cfml_method_of(&cls, &m[2].to_lowercase(), &mut HashSet::new())?;
        Ok(method.map(|node| KCand { node, confidence: 0.9, resolved_by: "instance-method" }))
    }

    /// cfmlComponentByName: the CFML component a bare name reaches from
    /// `from_file` — the one in its directory, else the only one so named.
    fn cfml_component_by_name(&mut self, name: &str, from_file: &str) -> Res<Option<Arc<KNode>>> {
        let components: Vec<Arc<KNode>> = self
            .nodes_by_lower_name(&name.to_lowercase())?
            .iter()
            .filter(|n| (n.kind == "class" || n.kind == "interface") && (n.language == "cfml" || n.language == "cfscript"))
            .cloned()
            .collect();
        fn dir_of(p: &str) -> &str {
            p.rfind('/').map(|i| &p[..=i]).unwrap_or("")
        }
        let beside: Vec<&Arc<KNode>> = components.iter().filter(|n| dir_of(&n.file_path) == dir_of(from_file)).collect();
        if beside.len() == 1 {
            return Ok(Some(beside[0].clone()));
        }
        Ok((components.len() == 1).then(|| components[0].clone()))
    }

    /// cfmlMethodOf: a component's method by its lowercased name (CFML names
    /// ignore case) — its own, else one a supertype has.
    fn cfml_method_of(&mut self, cls: &KNode, lower_name: &str, seen: &mut HashSet<String>) -> Res<Option<Arc<KNode>>> {
        if !seen.insert(cls.id.clone()) || seen.len() > 8 {
            return Ok(None);
        }
        let own = self.nodes_in_file(&cls.file_path)?.iter().find(|n| {
            n.kind == "method"
                && n.name.to_lowercase() == lower_name
                && n.qualified_name.to_lowercase() == format!("{}::{}", cls.name, n.name).to_lowercase()
                && n.start_line >= cls.start_line
                && n.end_line <= cls.end_line
        }).cloned();
        if own.is_some() {
            return Ok(own);
        }
        for supertype in self.supertype_nodes(&cls.id)? {
            if let Some(inherited) = self.cfml_method_of(&supertype, lower_name, seen)? {
                return Ok(Some(inherited));
            }
        }
        Ok(None)
    }

    /// resolveCfmlIncludePath: the file a CFML include names (#2651) —
    /// `include "x.cfm";`, `<cfinclude template="x.cfm">`,
    /// `cfinclude(template="x.cfm")`. Relative to the including file's
    /// directory, or to the web (project) root when the path starts with
    /// `/`. One case-insensitive match counts when the exact case misses.
    pub(super) fn resolve_cfml_include_path(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let written = r.reference_name.trim().replace('\\', "/");
        let from_root = written.starts_with('/');
        let mut parts: Vec<String> = if from_root {
            Vec::new()
        } else {
            let mut v: Vec<String> = r.file_path.replace('\\', "/").split('/').map(String::from).collect();
            v.pop();
            v
        };
        for seg in written.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Ok(None);
                    }
                }
                s => parts.push(s.to_string()),
            }
        }
        let Some(file_name) = parts.last().filter(|s| !s.is_empty()).cloned() else {
            return Ok(None);
        };
        let wanted = parts.join("/");
        let files: Vec<Arc<KNode>> = self
            .nodes_by_lower_name(&file_name.to_lowercase())?
            .iter()
            .filter(|n| n.kind == "file")
            .cloned()
            .collect();
        if let Some(exact) = files.iter().find(|n| n.file_path == wanted) {
            return Ok(Some(KCand {
                node: exact.clone(),
                confidence: if from_root { 0.85 } else { 0.95 },
                resolved_by: "file-path",
            }));
        }
        let folded: Vec<&Arc<KNode>> = files.iter().filter(|n| n.file_path.eq_ignore_ascii_case(&wanted)).collect();
        Ok((folded.len() == 1).then(|| KCand {
            node: folded[0].clone(),
            confidence: if from_root { 0.8 } else { 0.9 },
            resolved_by: "file-path",
        }))
    }

    /// The resolved import path's file node @0.9, or nothing.
    fn resolve_import_path_to_file_node(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(resolved) = self.resolve_import_path(&r.reference_name, &r.file_path, &r.language)? else {
            return Ok(None);
        };
        let basename = pos_basename(&resolved).to_string();
        Ok(self
            .nodes_by_name(&basename)?
            .iter()
            .find(|n| n.kind == "file" && n.file_path == resolved)
            .map(|n| KCand { node: n.clone(), confidence: 0.9, resolved_by: "import" }))
    }

    /// resolveCobolCopybook: the member against indexed file stems,
    /// case-insensitively; `.cpy` outranks a program file, a same-directory
    /// hit breaks a tier, and the first of equals (file-node order) wins.
    fn resolve_cobol_copybook(&mut self, member: &str, from_file: &str) -> Res<Option<String>> {
        if self.cobol_copybooks.is_none() {
            let conn = self.conn()?;
            // getNodesByKind('file') verbatim, so the order matches TS.
            let mut stmt = conn
                .prepare("SELECT * FROM nodes WHERE kind = ?1")
                .map_err(|e| Error::from_reason(e.to_string()))?;
            let rows = stmt
                .query_map(["file"], |row| row.get::<_, String>("file_path"))
                .map_err(|e| Error::from_reason(e.to_string()))?;
            let mut index: HashMap<String, Vec<String>> = HashMap::new();
            for path in rows {
                let path = path.map_err(|e| Error::from_reason(e.to_string()))?;
                let normalized = path.replace('\\', "/");
                let base = normalized.rsplit('/').next().unwrap_or("");
                let stem = match base.rfind('.') {
                    Some(dot) if dot > 0 => &base[..dot],
                    _ => base,
                };
                index.entry(stem.to_lowercase()).or_default().push(path.clone());
            }
            drop(stmt);
            self.cobol_copybooks = Some(index);
        }
        let Some(candidates) = self.cobol_copybooks.as_ref().and_then(|i| i.get(&member.to_lowercase())) else {
            return Ok(None);
        };
        let from_norm = from_file.replace('\\', "/");
        let from_dir = match from_norm.rfind('/') {
            Some(i) => &from_norm[..i],
            None => "",
        };
        let mut best: Option<&String> = None;
        let mut best_score = -1;
        for candidate in candidates {
            let normalized = candidate.replace('\\', "/");
            let ext = normalized.rfind('.').map_or(normalized.as_str(), |i| &normalized[i..]).to_lowercase();
            let mut score = 0;
            if ext == ".cpy" {
                score += 4;
            } else if ext == ".cbl" || ext == ".cob" || ext == ".cobol" {
                score += 2;
            }
            let dir = match normalized.rfind('/') {
                Some(i) => &normalized[..i],
                None => "",
            };
            if dir == from_dir {
                score += 1;
            }
            if score > best_score {
                best_score = score;
                best = Some(candidate);
            }
        }
        Ok(best.cloned())
    }

    /// resolveLuaRequire (import-resolver.ts): a Lua/Luau `imports` ref is a
    /// dotted module path (`a.b.c` from `require("a.b.c")`) or an
    /// instance-path leaf (`Signal` from `require(script.Parent.Signal)`).
    /// No static import statement exists, so the path-matcher can't bridge
    /// the dot↔slash / leaf↔basename gap: try `<base>.lua`, `.luau`,
    /// `/init.lua`, `/init.luau` as path suffixes over the files sharing the
    /// basename, prefer the longest common prefix with the ref's file (a
    /// stable sort — `getAllFiles()` order breaks ties), and link the file
    /// node @0.9 so the deterministic match beats a same-name self-match.
    pub(super) fn resolve_lua_require(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if (r.language != "lua" && r.language != "luau") || r.reference_kind != "imports" {
            return Ok(None);
        }
        let name = r.reference_name.as_str();
        if name.is_empty() {
            return Ok(None);
        }
        let base = if name.contains('.') { name.replace('.', "/") } else { name.to_string() };
        // JS `a[i] === b[i]` — shared UTF-16 code units from the start.
        let shared = |a: &str, b: &str| -> usize {
            a.encode_utf16().zip(b.encode_utf16()).take_while(|(x, y)| x == y).count()
        };
        for suffix in [
            format!("{base}.lua"),
            format!("{base}.luau"),
            format!("{base}/init.lua"),
            format!("{base}/init.luau"),
        ] {
            let basename = suffix.rsplit('/').next().unwrap_or("");
            let bucket = self.lua_basename_bucket(basename);
            let mut matches: Vec<&String> = bucket
                .iter()
                .filter(|f| is_path_or_tail(f, &suffix))
                .collect();
            if matches.is_empty() {
                continue;
            }
            matches.sort_by_key(|f| std::cmp::Reverse(shared(f, &r.file_path)));
            let best = matches[0].clone();
            if best == r.file_path {
                continue;
            }
            if let Some(file_node) = self.nodes_in_file(&best)?.iter().find(|n| n.kind == "file") {
                return Ok(Some(KCand {
                    node: file_node.clone(),
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }
        Ok(None)
    }

    /// The `getAllFiles()`-ordered paths sharing `basename` (luaBasenameIndex).
    /// `ORDER BY path` is byte order — the same order `sort()` gives.
    pub(super) fn lua_basename_bucket(&self, basename: &str) -> Arc<Vec<String>> {
        if let Some(files) = self.sorted_files() {
            return Arc::new(
                files
                    .iter()
                    .filter(|f| f.rsplit('/').next().unwrap_or("") == basename)
                    .cloned()
                    .collect(),
            );
        }
        self.table().map(|t| t.lua_basename_bucket(basename)).unwrap_or_default()
    }

    /// resolveGoCrossPackageReference (import-resolver.ts): `pkg.Member` via
    /// an in-module import — the package directory owns the member.
    pub(super) fn resolve_go_cross_package(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Res<Option<KCand>> {
        let proven = self.go_ref_qualifier(r)?;
        let (receiver, member_name) = if let Some((receiver, member)) = r.reference_name.split_once('.') {
            (receiver.to_string(), member)
        } else if let Some(imp) = proven { (imp.local_name, r.reference_name.as_str()) }
        else { return Ok(None); };
        if member_name.is_empty() || self.is_shadowed_import(&receiver, r)? { return Ok(None); }
        for imp in imports {
            if imp.local_name != receiver { continue; }
            let Some(pkg_dir) = self.go_package_dir(&imp.source, &r.file_path) else { continue };
            let nodes: Vec<_> = self.nodes_by_name(member_name)?.iter()
                .filter(|n| n.language == "go" && n.is_exported && n.kind != "method"
                    && !n.qualified_name.contains("::") && pos_dirname(&n.file_path) == pkg_dir)
                .cloned().collect();
            if let [node] = nodes.as_slice() {
                return Ok(Some(KCand { node: node.clone(), confidence: 0.9, resolved_by: "import" }));
            }
            // A name the package defines once per build target: the twin the
            // reference's file builds with, or nothing (#2581).
            if let Some(node) = self.go_pick_twin(&nodes, r)? {
                return Ok(Some(KCand { node, confidence: 0.9, resolved_by: "import" }));
            }
        }
        Ok(None)
    }

    pub(super) fn python_package_import(&mut self, binding: &KBinding, r: &ResolveRefIn) -> bool {
        let Some(source) = binding.target_spec.as_deref() else { return false };
        if binding.target_name.as_deref() != Some("*")
            || (source != binding.name && !source.starts_with(&format!("{}.", binding.name)))
        {
            return false;
        }
        let Some(file) = self.read_file(&r.file_path) else { return false };
        let row = (binding.line - 1).max(0) as usize;
        let Some(line) = file.get(row) else { return false };
        // A plain one-line `import a.b, c` names its modules on the line;
        // reading the tree parses the whole file again for each class that
        // imports a package, and the tree cache holds only a few files.
        if let Some(named) = plain_import_names(line, source) {
            return named;
        }
        let Some(tree) = self.parsed_tree(&file, r) else { return false };
        let point = tree_sitter::Point::new(row, line.len() - line.trim_start().len());
        let mut node = tree.root_node().named_descendant_for_point_range(point, point);
        while let Some(current) = node {
            if current.kind() == "import_statement" {
                return super::iteration::named_children(current).into_iter().any(|child| {
                    child.kind() == "dotted_name"
                        && child.utf8_text(file.text().as_bytes()).is_ok_and(|text| text == source)
                });
            }
            node = current.parent();
        }
        false
    }

    /// resolvePythonModuleMember (import-resolver.ts): `mod.func` after
    /// `import mod` / `from pkg import mod` — the receiver is a submodule.
    pub(super) fn resolve_python_module_member(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Res<Option<KCand>> {
        let Some(dot_idx) = r.reference_name.find('.') else {
            return Ok(None);
        };
        if dot_idx == 0 {
            return Ok(None);
        }
        let receiver = &r.reference_name[..dot_idx];
        let members: Vec<&str> = r.reference_name[dot_idx + 1..].split('.').collect();
        if members.first().is_none_or(|m| m.is_empty()) {
            return Ok(None);
        }

        for imp in imports.iter() {
            if imp.local_name != receiver {
                continue;
            }
            // `from pkg import mod as alias` — join with the EXPORTED name.
            let module_name = if imp.exported_name == "*" {
                imp.local_name.clone()
            } else {
                imp.exported_name.clone()
            };
            let mut module_path = if imp.is_namespace {
                imp.source.clone()
            } else if imp.source.ends_with('.') {
                format!("{}{}", imp.source, module_name)
            } else {
                format!("{}.{}", imp.source, module_name)
            };
            let mut remaining: Vec<&str> = members.clone();
            if imp.is_namespace && imp.source.starts_with(&format!("{}.", receiver)) {
                // JS slice counts UTF-16 units — receiver may not be ASCII.
                let suffix: Vec<&str> =
                    js_slice(&imp.source, utf16_len(receiver) + 1)
                        .split('.')
                        .collect();
                if !suffix.iter().enumerate().all(|(i, s)| remaining.get(i) == Some(s)) {
                    continue;
                }
                remaining = remaining[suffix.len()..].to_vec();
            }
            if remaining.is_empty() { continue; }
            if remaining.len() > 1 {
                module_path = format!("{}.{}", module_path, remaining[..remaining.len() - 1].join("."));
            }
            let member = remaining[remaining.len() - 1];
            let mut resolved_path =
                self.resolve_import_path(&module_path, &r.file_path, &r.language)?;
            if resolved_path.is_none() {
                resolved_path = self
                    .find_python_module_file(&module_path, &r.file_path)?
                    .map(|n| n.file_path.clone());
            }
            let Some(resolved_path) = resolved_path else { continue };
            if resolved_path == r.file_path {
                continue;
            }
            let target = self.python_module_symbol(&resolved_path,member,0,&mut HashSet::new())?;
            if let Some(target) = target {
                return Ok(Some(KCand {
                    node: target.clone(),
                    confidence: 0.85,
                    resolved_by: "import",
                }));
            }
        }
        Ok(None)
    }

    /// resolvePythonAbsoluteModule (import-resolver.ts): a dotted `imports`
    /// ref is the full module path — resolve to its file node.
    pub(super) fn resolve_python_absolute_module(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if r.reference_kind != "imports"
            || (!r.reference_name.contains('.') && !self.is_python_from_import_module(r.clone()))
        {
            return Ok(None);
        }
        Ok(self
            .find_python_module_file(&r.reference_name, &r.file_path)?
            .map(|node| KCand {
                node,
                confidence: 0.9,
                resolved_by: "import",
            }))
    }

    /// resolveStaticMember (import-resolver.ts): `Container.member` on a
    /// named class import — the `Container::member` qualifiedName inside the
    /// container's own file.
    pub(super) fn resolve_static_member(
        &mut self,
        container: &KNode,
        r: &ResolveRefIn,
        local_name: &str,
    ) -> Res<Option<Arc<KNode>>> {
        if !is_static_member_container(&container.kind) {
            return Ok(None);
        }
        let member = js_slice(&r.reference_name, utf16_len(local_name) + 1)
            .split('.')
            .next()
            .unwrap_or("");
        if member.is_empty() {
            return Ok(None);
        }
        let member_qn = format!("{}::{}", container.qualified_name, member);
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&member_qn)?
            .iter()
            .filter(|n| n.file_path == container.file_path)
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        if r.reference_kind == "calls" {
            if let Some(callable) = candidates
                .iter()
                .find(|n| n.kind == "method" || n.kind == "function")
            {
                return Ok(Some(callable.clone()));
            }
        }
        Ok(Some(candidates[0].clone()))
    }
}

fn is_declaration_file(path: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"].iter().any(|ext| path.ends_with(ext))
}

/// isCfmlTemplatePath: the name a CFML include carries — a path with a
/// separator, or one ending in a template extension.
pub(super) fn is_cfml_template_path(name: &str) -> bool {
    !name.contains(':')
        && (name.contains('/') || name.contains('\\') || re!(r"(?i)\.(?:cfml?|cfc|cfs)$").is_match(name.trim()))
}

/// Whether the `import` statement on `line` names the module `source` as a
/// plain dotted name (`import a.b, c`, not `import a.b as c`). `None` when
/// the line is not a plain one-line `import` statement, so the caller reads
/// the tree.
fn plain_import_names(line: &str, source: &str) -> Option<bool> {
    let clause = line.trim_start().strip_prefix("import")?;
    if !clause.starts_with([' ', '\t']) {
        return None;
    }
    let clause = clause.split(['#', ';']).next().unwrap_or(clause);
    if clause.contains(['\\', '(', '"', '\'']) {
        return None;
    }
    Some(clause.split(',').any(|name| name.trim() == source))
}

/// indexedSpelling (import-resolver.ts, upstream 8651990a / #2577): a path as
/// the index keys its files. The alias, workspace and direct-path branches
/// join an import specifier as written, so `@/widgets/Chart/` made
/// `src/widgets/Chart//index.tsx` and `src/widgets/./Chart` made
/// `src/widgets/./Chart/index.tsx` — spellings no file node or `files` row is
/// keyed by. An ES-module specifier is read lexically, so `.` and empty
/// segments drop and `..` folds; a trailing slash still names a folder. Other
/// languages keep the path as found — a C include's `a/../b.h` follows the
/// folder `a` on disk, symlinks included.
fn indexed_spelling(file_path: &str, language: &str) -> String {
    let dirty = file_path.contains("//")
        || file_path.split('/').any(|seg| seg == "." || seg == "..");
    if is_esm_import_language(language) && dirty {
        pos_normalize(file_path)
    } else {
        file_path.to_string()
    }
}

/// WorkspaceImportTargets (workspace-packages.ts): where an import of a
/// project package lands before any file is probed.
struct WorkspaceTargets {
    /// The entries the package declares for the import, best first.
    declared: Vec<String>,
    /// The package's directory plus the import's subpath, extension-less.
    directory: String,
    /// The package's directory.
    package_dir: String,
}

/// exportedTargets (workspace-packages.ts): `exports`' targets for `key` —
/// the exact key's, else the `*` pattern's with the longest prefix, its `*`
/// filled in.
fn exported_targets(exports: &[(String, Vec<String>)], key: &str) -> Vec<String> {
    for (k, targets) in exports {
        if k == key {
            return targets.clone();
        }
    }
    let mut best: Option<(usize, String, &Vec<String>)> = None;
    for (pattern, targets) in exports {
        let Some(star) = pattern.find('*') else { continue };
        if pattern[star + 1..].contains('*') {
            continue;
        }
        let (prefix, suffix) = (&pattern[..star], &pattern[star + 1..]);
        if key.len() < prefix.len() + suffix.len() || !key.starts_with(prefix) || !key.ends_with(suffix) {
            continue;
        }
        if best.as_ref().is_some_and(|(b, _, _)| prefix.len() <= *b) {
            continue;
        }
        best = Some((
            prefix.len(),
            key[prefix.len()..key.len() - suffix.len()].to_string(),
            targets,
        ));
    }
    match best {
        Some((_, matched, targets)) => targets.iter().map(|t| t.replace('*', &matched)).collect(),
        None => vec![],
    }
}

/// builtSourcesOf (import-resolver.ts): `target` inside `package_dir`'s build
/// output (`dist/`, `lib/`, `esm/`…) as the `src/` sources it is built from,
/// best first.
fn built_sources_of(target: &str, package_dir: &str) -> Vec<String> {
    let prefix = if package_dir.is_empty() {
        String::new()
    } else {
        format!("{package_dir}/")
    };
    if !target.starts_with(&prefix) {
        return vec![];
    }
    let in_package = &target[prefix.len()..];
    fn build_root() -> Rc<Regex> {
        re!(r"^(?:dist|build|lib|out|output|es|esm|cjs|umd)/")
    }
    fn build_format() -> Rc<Regex> {
        re!(r"^(?:esm|cjs|es|mjs|module|modules|commonjs|types|typings|umd|node|browser|src)/")
    }
    fn emitted_extension() -> Rc<Regex> {
        re!(r"(?:\.d)?\.(?:[cm]?[jt]s|[jt]sx)$")
    }
    let Some(root) = build_root().find(in_package) else {
        return vec![];
    };
    let mirrored = &in_package[root.end()..];
    let format = build_format().find(mirrored);
    let mut stems = vec![mirrored.to_string()];
    if let Some(f) = format {
        stems.push(mirrored[f.end()..].to_string());
    }
    let mut seen = HashSet::new();
    stems
        .iter()
        .map(|rest| emitted_extension().replace(rest, "").to_string())
        .filter(|stem| !stem.is_empty() && !stem.starts_with("src/"))
        .filter(|stem| seen.insert(stem.clone()))
        .map(|stem| format!("{prefix}src/{stem}"))
        .collect()
}

#[cfg(test)]
mod plain_import_tests {
    use super::plain_import_names;

    #[test]
    fn names_a_dotted_module_on_a_plain_import_line() {
        assert_eq!(plain_import_names("import os.path", "os.path"), Some(true));
        assert_eq!(plain_import_names("    import os, os.path  # paths", "os.path"), Some(true));
        assert_eq!(plain_import_names("import os.path; x = 1", "os.path"), Some(true));
        assert_eq!(plain_import_names("import os", "os.path"), Some(false));
        assert_eq!(plain_import_names("import os.path as osp", "os.path"), Some(false));
    }

    #[test]
    fn leaves_other_shapes_to_the_tree() {
        assert_eq!(plain_import_names("from os import path", "os.path"), None);
        assert_eq!(plain_import_names("importlib = 1", "importlib"), None);
        assert_eq!(plain_import_names("x = 1; import os", "os"), None);
        assert_eq!(plain_import_names("import os, \\", "os"), None);
        assert_eq!(plain_import_names("import (os)", "os"), None);
    }
}
