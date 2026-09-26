//! Import machinery (import-resolver.ts): mappings, re-exports, path resolution and the member descent.

use super::*;

impl KernelResolver {
    // -----------------------------------------------------------------------
    // Import machinery (import-resolver.ts)
    // -----------------------------------------------------------------------

    /// isExternalImport (import-resolver.ts), context-aware.
    pub(super) fn is_external_import(&self, import_path: &str, language: &str) -> bool {
        if import_path.starts_with('.') {
            return false;
        }
        if self.workspaces.is_some() && self.resolve_workspace_import(import_path).is_some() {
            return false;
        }
        if is_esm_import_language(language) {
            if ESM_BUILTIN_MODULES.contains(&import_path) {
                return true;
            }
            if let Some(aliases) = &self.aliases {
                if aliases.patterns.iter().any(|p| import_path.starts_with(&p.prefix)) {
                    return false;
                }
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
            if let Some(mod_path) = &self.go_module_path {
                if import_path == mod_path || import_path.starts_with(&format!("{}/", mod_path)) {
                    return false;
                }
            }
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

    /// resolveWorkspaceImport (workspace-packages.ts): sourceEntries first,
    /// then longest matching package name; a bare member import resolves to
    /// its declared entry when the manifest names one.
    pub(super) fn resolve_workspace_import(&self, import_path: &str) -> Option<String> {
        let ws = self.workspaces.as_ref()?;
        if let Some((_, src)) = ws.source_entries.iter().find(|(k, _)| k == import_path) {
            return Some(src.clone());
        }
        let mut best: Option<(&str, &str)> = None;
        for (name, dir) in &ws.by_name {
            if (import_path == name.as_str() || import_path.starts_with(&format!("{}/", name)))
                && best.is_none_or(|(b, _)| name.len() > b.len())
            {
                best = Some((name, dir));
            }
        }
        let (name, dir) = best?;
        let subpath = &import_path[name.len()..]; // '' or '/widgets'
        if subpath.is_empty() {
            if let Some(entry_by_name) = &ws.entry_by_name {
                if let Some(entry) = entry_by_name.get(name) {
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

    /// applyAliases (path-aliases.ts): candidate paths relative to
    /// projectRoot in tsconfig priority order.
    pub(super) fn apply_aliases(&self, import_path: &str) -> Vec<String> {
        let Some(alias_map) = &self.aliases else { return Vec::new() };
        for pat in &alias_map.patterns {
            if !import_path.starts_with(&pat.prefix) {
                continue;
            }
            if !pat.suffix.is_empty() && !import_path.ends_with(&pat.suffix) {
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

    /// resolveRelativeImport (import-resolver.ts), including the Python
    /// dotted-relative translation.
    pub(super) fn resolve_relative_import(
        &mut self,
        import_path: &str,
        from_dir: &str,
        language: &str,
    ) -> Res<Option<String>> {
        let extensions = extension_resolution(language);
        if language == "python" && import_path.starts_with('.') {
            let dots = import_path.len() - import_path.trim_start_matches('.').len();
            let up = "../".repeat(dots.saturating_sub(1));
            let rest = import_path[dots..].replace('.', "/");
            let py_base = pos_resolve(from_dir, &format!("{}{}", up, rest));
            let py_rel = pos_relative(&self.root_abs, &py_base);
            for ext in extensions {
                let candidate = format!("{}{}", py_rel, ext);
                if self.file_exists(&candidate) {
                    return Ok(Some(candidate));
                }
            }
            if !py_rel.is_empty() && self.file_exists(&py_rel) {
                return Ok(Some(py_rel));
            }
            return Ok(None);
        }
        let base_path = pos_resolve(from_dir, import_path);
        let relative_path = pos_relative(&self.root_abs, &base_path);
        Ok(self.probe_extensions(&relative_path, language))
    }

    /// The first existing file among `base` + each of the language's
    /// extensions, then `base` itself, then the source an emitted `.js`/`.d.ts`
    /// path was compiled from.
    pub(super) fn probe_extensions(&self, base: &str, language: &str) -> Option<String> {
        extension_resolution(language)
            .iter()
            .map(|ext| format!("{base}{ext}"))
            .find(|candidate| self.file_exists(candidate))
            .or_else(|| self.file_exists(base).then(|| base.to_string()))
            .or_else(|| self.find_source_for_emitted(base, language))
    }

    /// resolveAliasedImport (import-resolver.ts): tsconfig paths → workspace
    /// → hardcoded fallbacks → direct path.
    pub(super) fn resolve_aliased_import(
        &mut self,
        import_path: &str,
        language: &str,
    ) -> Res<Option<String>> {
        if self.aliases.is_some() {
            for c in self.apply_aliases(import_path) {
                if let Some(hit) = self.probe_extensions(&c, language) {
                    return Ok(Some(hit));
                }
            }
        }
        if self.workspaces.is_some() {
            if let Some(base) = self.resolve_workspace_import(import_path) {
                if let Some(hit) = self.probe_extensions(&base, language) {
                    return Ok(Some(hit));
                }
            }
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
        // (COBOL copybook arm omitted: `cobol` is not a migrated language.)
        if self.is_external_import(import_path, language) {
            return Ok(None);
        }
        let from_dir = pos_dirname(&pos_resolve(&self.root_abs, from_file)).to_string();
        if import_path.starts_with('.') {
            return self.resolve_relative_import(import_path, &from_dir, language);
        }
        if let Some(aliased) = self.resolve_aliased_import(import_path, language)? {
            return Ok(Some(aliased));
        }
        if language == "c" || language == "cpp" {
            return Ok(self.resolve_cpp_include_path(import_path, language));
        }
        Ok(None)
    }

    /// findPythonModuleFile (import-resolver.ts): `a.b.c` → `a/b/c.py` or
    /// `a/b/c/__init__.py`, suffix-matched; root/`src/` prefixes win, then a
    /// unique candidate.
    pub(super) fn find_python_module_file(
        &mut self,
        module: &str,
        exclude_file: &str,
    ) -> Res<Option<Arc<KNode>>> {
        if module.is_empty() || module.starts_with('.') {
            return Ok(None);
        }
        let rel = module.replace('.', "/");
        let last_seg = module.split('.').next_back().unwrap_or(module);
        let mut candidates: Vec<Arc<KNode>> = Vec::new();
        let want = format!("{}.py", rel);
        for n in self.nodes_by_name(&format!("{}.py", last_seg))?.iter() {
            if n.kind == "file" && n.file_path != exclude_file && is_path_or_tail(&n.file_path, &want) {
                candidates.push(n.clone());
            }
        }
        let want = format!("{}/__init__.py", rel);
        for n in self.nodes_by_name("__init__.py")?.iter() {
            if n.kind == "file" && n.file_path != exclude_file && is_path_or_tail(&n.file_path, &want) {
                candidates.push(n.clone());
            }
        }
        for root in ["", "src/"] {
            for suffix in [format!("{}/__init__.py", rel), format!("{}.py", rel)] {
                let want = format!("{}{}", root, suffix);
                if let Some(hit) = candidates.iter().find(|n| n.file_path == want) {
                    return Ok(Some(hit.clone()));
                }
            }
        }
        Ok(if candidates.len() == 1 { candidates.pop() } else { None })
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
        // A chained call reduced to its bare method name starts its column at
        // the chain, not the name: only a source that begins with the name is
        // the import's (import-resolver.ts isBareCallSite).
        let bare_call_site = if r.reference_kind == "calls" {
            match self.read_file(&r.file_path) {
                Some(lines) => lines.get((r.line - 1) as usize).is_none_or(|line| {
                    js_slice(line, r.column as usize)
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
    /// arms cannot fire for it, so this is that function without the punt,
    /// which only the member descent raises.
    pub(super) fn resolve_via_import(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        match self.resolve_via_import_member(r) {
            Err(Halt::Punt(_)) => Ok(None),
            other => other,
        }
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
            return Ok(self.is_external_import(&imp.source, &r.language));
        }
        Ok(false)
    }

    /// resolveViaImport (import-resolver.ts): the C/C++ include arm, the ESM
    /// `import('./x')`-path arm, the go/java/python/lua/module-file arms, and
    /// the imports loop with the `localName.member` descent. Returns Punt
    /// where TS would read source the kernel doesn't port (the member descent
    /// only).
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
        // TS/JS path-shaped `imports` ref whose referenceName IS the module
        // specifier — the module ref of `import … from './x'` or a dynamic
        // `import('./x')` call site. Resolve the specifier (extension- and
        // alias-aware) straight to the file node like the c/cpp include arm:
        // name-match would bind the file's own `import` STATEMENT node —
        // `./cmd.config` literally matches that node's name — or a same-named
        // file elsewhere. Needs no binding rows, so it precedes the empty-
        // imports early return, mirroring the TS-side arm's position ahead of
        // the mappings lookup (import-resolver.ts resolveViaImport).
        if r.reference_kind == "imports"
            && is_esm_import_language(&r.language)
            && r.reference_name.contains('/')
        {
            if let Some(resolved) =
                self.resolve_import_path(&r.reference_name, &r.file_path, &r.language)?
            {
                if let Some(file_node) = self
                    .nodes_in_file(&resolved)?
                    .iter()
                    .find(|n| n.kind == "file")
                    .cloned()
                {
                    return Ok(Some(KCand {
                        node: file_node,
                        confidence: 0.9,
                        resolved_by: "import",
                    }));
                }
            }
        }
        let imports = self.import_mappings(&r.file_path)?;
        if imports.is_empty() && self.read_file(&r.file_path).is_none() {
            return Ok(None);
        }

        if r.language == "go" {
            if let Some(c) = self.resolve_go_cross_package(r, &imports)? {
                return Ok(Some(c));
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
            if let Some(c) = self.resolve_python_module_member(r, &imports)? {
                return Ok(Some(c));
            }
            if let Some(c) = self.resolve_python_absolute_module(r)? {
                return Ok(Some(c));
            }
        }
        // (Rust `::` paths never reach here — resolve_rust_path_ref runs
        // ahead of the gate and `::`+`.` names punt.)
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
            let Some(target) = self.find_exported_symbol(
                &resolved_path,
                &want,
                &r.language,
                &mut visited,
                0,
            )?
            else {
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
        Ok(None)
    }

    /// resolveObjectLiteralAlias (import-resolver.ts): `Api.upload()` where
    /// `Api` is `{ upload, other: impl }` — a shorthand or `key: ident`
    /// property naming a binding of the object's file. The binding resolves
    /// to a symbol declared there, else through that file's own imports.
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
        if !re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(member) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&container.file_path) else {
            return Ok(None);
        };
        let from = ((container.start_line - 1).max(0) as usize).min(lines.len());
        let to = (container.end_line.max(0) as usize).clamp(from, lines.len());
        let extent = lines[from..to].join("\n");
        let Some(brace) = extent.find('{') else {
            return Ok(None);
        };
        let body = &extent[brace..];
        // TS splices `member` into its regexes unescaped, where an
        // identifier's `$` is an end anchor: such a member never matches.
        if member.contains('$') {
            return Ok(None);
        }
        // `[{,\s]MEMBER\s*:\s*(IDENT)\s*[,}]`, then `[{,\s]MEMBER\s*[,}]`.
        static KEYED: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new(r"[{,\s]", r"\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[,}]", false, false, false)
        });
        static SHORTHAND: LazyLock<Affix> =
            LazyLock::new(|| Affix::new(r"[{,\s]", r"\s*[,}]", false, false, false));
        let binding = match KEYED.capture(body, member) {
            Some(b) => b.to_string(),
            None if SHORTHAND.is_match(body, member) => member.to_string(),
            None => return Ok(None),
        };
        let calls = r.reference_kind == "calls";
        let accepts = |n: &KNode| {
            let callable = matches!(n.kind.as_str(), "function" | "method" | "class");
            callable || (!calls && matches!(n.kind.as_str(), "constant" | "variable" | "component"))
        };
        let cand = |node| KCand { node, confidence: 0.9, resolved_by: "import" };

        // Declared in the object's own file, outside the literal.
        let mut local: Vec<Arc<KNode>> = self
            .nodes_in_file(&container.file_path)?
            .iter()
            .filter(|n| n.name == binding && n.id != container.id && accepts(n))
            .cloned()
            .collect();
        local.sort_by_key(|n| (n.start_line, n.start_column));
        if let Some(n) = local.into_iter().next() {
            return Ok(Some(cand(n)));
        }

        // Imported into the object's file.
        let imports = self.import_mappings(&container.file_path)?;
        for imp in imports.iter() {
            if imp.local_name != binding || imp.is_namespace {
                continue;
            }
            let Some(path) =
                self.resolve_import_path(&imp.source, &container.file_path, &container.language)?
            else {
                continue;
            };
            let want = ExportWant {
                is_default: imp.is_default,
                is_namespace: false,
                exported_name: if imp.is_default { "default".to_string() } else { imp.exported_name.clone() },
                member_name: None,
            };
            let mut visited = HashSet::new();
            if let Some(target) =
                self.find_exported_symbol(&path, &want, &container.language, &mut visited, 0)?
            {
                if accepts(&target) {
                    return Ok(Some(cand(target)));
                }
            }
        }
        Ok(None)
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
            if let Some(c) =
                self.resolve_method_on_type(&type_name, member, r, 0.85, "instance-method", None)?
            {
                return Ok(Some(c));
            }
        }
        Ok(None)
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
        self.table().map(|t| t.lua_basename_bucket(basename)).unwrap_or_default()
    }

    /// resolveGoCrossPackageReference (import-resolver.ts): `pkg.Member` via
    /// an in-module import — the package directory owns the member.
    pub(super) fn resolve_go_cross_package(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Res<Option<KCand>> {
        let Some(mod_path) = self.go_module_path.clone() else {
            return Ok(None);
        };
        let Some(dot) = r.reference_name.find('.') else {
            return Ok(None);
        };
        if dot == 0 {
            return Ok(None);
        }
        let receiver = &r.reference_name[..dot];
        let member_name = &r.reference_name[dot + 1..];
        if member_name.is_empty() {
            return Ok(None);
        }
        for imp in imports.iter() {
            if imp.local_name != receiver {
                continue;
            }
            if imp.source != mod_path && !imp.source.starts_with(&format!("{}/", mod_path)) {
                continue;
            }
            let pkg_dir = if imp.source == mod_path {
                String::new()
            } else {
                imp.source[mod_path.len() + 1..].to_string()
            };
            for node in self.nodes_by_name(member_name)?.iter() {
                if node.language != "go" || !node.is_exported {
                    continue;
                }
                let fp = &node.file_path;
                let file_dir = fp.rfind('/').map(|i| &fp[..i]).unwrap_or("");
                if file_dir == pkg_dir {
                    return Ok(Some(KCand {
                        node: node.clone(),
                        confidence: 0.9,
                        resolved_by: "import",
                    }));
                }
            }
        }
        Ok(None)
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
            let module_path = if imp.is_namespace {
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
            if remaining.len() != 1 {
                continue;
            }
            let member = remaining[0];
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
            let nodes = self.nodes_in_file(&resolved_path)?;
            let target = nodes.iter().find(|n| {
                n.name == member
                    && matches!(
                        n.kind.as_str(),
                        "function" | "class" | "variable" | "constant"
                    )
            });
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
        if r.reference_kind != "imports" || !r.reference_name.contains('.') {
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
