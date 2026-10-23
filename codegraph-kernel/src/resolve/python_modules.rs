//! Python modules are found under import roots, never by a path suffix.
//! Root discovery and sync invalidation use the same indexed-file snapshot.
use super::*;

#[cfg_attr(feature = "addon", napi(object))]
pub struct PythonReopenScope {
    pub target_files: Vec<String>,
    pub source_dirs: Vec<String>,
    pub module_leaves: Vec<String>,
}
#[cfg_attr(feature = "addon", napi(object))]
pub struct PythonImportBinding {
    pub file_path: String,
    pub local_name: String,
    pub binding: Option<String>,
}

#[derive(Default)]
pub(super) struct PythonModules {
    files: HashSet<String>,
    packages: HashSet<String>,
    dirs: HashSet<String>,
    project: HashSet<String>,
    local: HashMap<String, String>,
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}
fn join(dir: &str, leaf: &str) -> String {
    if dir.is_empty() {
        leaf.to_string()
    } else {
        format!("{dir}/{leaf}")
    }
}
fn within(dir: &str, scope: &str) -> bool {
    scope.is_empty() || dir == scope || dir.starts_with(&format!("{scope}/"))
}
fn non_project(dir: &str) -> bool {
    matches!(
        dir.rsplit('/').next().unwrap_or(dir),
        "tests"
            | "test"
            | "testing"
            | "fixtures"
            | "testdata"
            | "examples"
            | "example"
            | "samples"
            | "vendor"
            | "third_party"
    )
}

impl PythonModules {
    fn at(&self, base: &str) -> Option<String> {
        let init = join(base, "__init__.py");
        if self.files.contains(&init) {
            Some(init)
        } else {
            let file = format!("{base}.py");
            self.files.contains(&file).then_some(file)
        }
    }
    fn under(&self, root: &str, rel: &str) -> Option<String> {
        let mut base = root.to_string();
        let parts: Vec<_> = rel.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            base = join(&base, part);
            if self.packages.contains(&base) {
                continue;
            }
            if self.files.contains(&format!("{base}.py")) || !self.dirs.contains(&base) {
                return None;
            }
        }
        self.at(&join(root, rel))
    }
    fn add_local(&mut self, root: String, reach: String) {
        if self.local.get(&root).is_none_or(|old| within(old, &reach)) {
            self.local.insert(root, reach);
        }
    }
    fn resolve(&self, spec: &str, from: &str) -> Option<String> {
        if spec.is_empty()
            || !spec
                .chars()
                .all(|c| c == '.' || c == '_' || c.is_alphanumeric())
        {
            return None;
        }
        let importer = parent(from);
        let dots = spec.bytes().take_while(|b| *b == b'.').count();
        if dots > 0 {
            let mut base = importer;
            for _ in 1..dots {
                if base.is_empty()
                    || self.packages.contains(base) && !self.packages.contains(parent(base))
                {
                    return None;
                }
                base = parent(base);
            }
            let rest = &spec[dots..];
            return if rest.is_empty() {
                let init = join(base, "__init__.py");
                self.files.contains(&init).then_some(init)
            } else {
                self.under(base, &rest.replace('.', "/"))
            };
        }
        let rel = spec.replace('.', "/");
        let head = spec.split('.').next()?;
        let mut containing = Vec::new();
        let mut dir = importer;
        loop {
            if self.project.contains(dir)
                || self.local.contains_key(dir)
                || !self.packages.contains(dir)
                    && (dir == importer || within(importer, &join(dir, head)))
            {
                containing.push(dir.to_string());
            }
            if dir.is_empty() {
                break;
            }
            dir = parent(dir);
        }
        let mut concrete_local = false;
        for root in &containing {
            if let Some(hit) = self.under(root, &rel) {
                return Some(hit);
            }
            concrete_local |= self.at(&join(root, head)).is_some();
        }
        if concrete_local {
            return None;
        }
        let eligible = |root: &String| {
            !containing.contains(root)
                && (self.project.contains(root)
                    || self
                        .local
                        .get(root)
                        .is_some_and(|reach| within(importer, reach)))
        };
        let roots: Vec<_> = self
            .project
            .iter()
            .chain(self.local.keys())
            .filter(|r| eligible(r))
            .collect();
        let concrete: Vec<_> = roots
            .iter()
            .filter(|r| self.at(&join(r, head)).is_some())
            .collect();
        if !concrete.is_empty() {
            return if concrete.len() == 1 {
                self.under(concrete[0], &rel)
            } else {
                None
            };
        }
        let hits: HashSet<_> = roots
            .iter()
            .filter(|r| self.dirs.contains(&join(r, head)))
            .filter_map(|r| self.under(r, &rel))
            .collect();
        if hits.len() == 1 {
            hits.into_iter().next()
        } else {
            None
        }
    }
}

/// Literal package-directory declarations from supported build backends.
fn declared_roots(content: &str, cfg: bool) -> Vec<String> {
    let header = re!(r"(?m)^[ \t]*\[([^\[\]\n]+)\][ \t]*(?:#.*)?$");
    let sections: Vec<_> = header.captures_iter(content).collect();
    let mut out = Vec::new();
    for (i, section) in sections.iter().enumerate() {
        let table = section[1].trim().replace(['\'', '"'], "");
        let body = &content[section.get(0).unwrap().end()
            ..sections
                .get(i + 1)
                .map_or(content.len(), |m| m.get(0).unwrap().start())];
        let mut add = |raw: &str| {
            let clean = raw
                .trim()
                .trim_matches(['\'', '"'])
                .trim_start_matches("./")
                .trim_end_matches('/');
            if !clean.is_empty()
                && !clean.starts_with("..")
                && !clean.starts_with('/')
                && !clean.contains(':')
            {
                out.push(clean.to_string());
            }
        };
        if cfg {
            let pattern = if table == "options" {
                Some(r"(?m)^\s*package_dir\s*=\s*(?:\n\s+)?=\s*(\S+)\s*$")
            } else if table == "options.packages.find" {
                Some(r"(?m)^\s*where\s*=\s*(\S+)\s*$")
            } else {
                None
            };
            if let Some(pattern) = pattern {
                if let Some(m) = Regex::new(pattern).unwrap().captures(body) {
                    add(&m[1]);
                }
            }
        } else {
            match table.as_str() {
                "tool.setuptools.packages.find" => {
                    if let Some(m) = re!(r"(?m)^\s*where\s*=\s*\[([^\]]*)\]").captures(body) {
                        for item in m[1].split(',') {
                            add(item);
                        }
                    }
                }
                "tool.setuptools.package-dir" | "tool.setuptools" => {
                    let pattern = if table == "tool.setuptools" {
                        r#"package-dir\s*=\s*\{[^}]*?["']{2}\s*=\s*["']([^"']+)["']"#
                    } else {
                        r#"(?m)^\s*["']{2}\s*=\s*["']([^"']+)["']"#
                    };
                    if let Some(m) = Regex::new(pattern).unwrap().captures(body) {
                        add(&m[1]);
                    }
                }
                "tool.poetry" => {
                    if let Some(packages) =
                        re!(r"(?m)^\s*packages\s*=\s*\[([\s\S]*?)\]\s*$").captures(body)
                    {
                        for m in re!(r#"\bfrom\s*=\s*["']([^"']+)["']"#).captures_iter(&packages[1])
                        {
                            add(&m[1]);
                        }
                    }
                }
                "tool.hatch.build.targets.wheel" => {
                    if let Some(m) = re!(r"(?m)^\s*packages\s*=\s*\[([^\]]*)\]").captures(body) {
                        for item in m[1].split(',') {
                            let item = item.trim().trim_matches(['\'', '"']);
                            if item.contains('/') {
                                add(parent(item));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

impl KernelResolver {
    fn build_python_modules(&mut self) -> Res<PythonModules> {
        let mut index = PythonModules::default();
        let files = if let Some(files) = self.sorted_files() {
            files
        } else {
            Arc::new(self.table()?.files.iter().cloned().collect())
        };
        index.dirs.insert(String::new());
        index.project.insert(String::new());
        for file in files.iter().filter(|f| f.ends_with(".py")) {
            let file = file.replace('\\', "/");
            if file == "__init__.py" || file.ends_with("/__init__.py") {
                index.packages.insert(parent(&file).to_string());
            }
            let mut dir = parent(&file);
            while !dir.is_empty() {
                index.dirs.insert(dir.to_string());
                dir = parent(dir);
            }
            index.files.insert(file);
        }
        for dir in index.packages.clone() {
            let mut top = dir.as_str();
            while !top.is_empty() && index.packages.contains(parent(top)) {
                top = parent(top);
            }
            if !top.is_empty() {
                index.add_local(parent(top).to_string(), parent(top).to_string());
            }
        }
        for service in index.dirs.clone() {
            if !service.is_empty()
                && !["setup.py", "pyproject.toml", "setup.cfg"].iter().any(|f| {
                    std::path::Path::new(&pos_resolve(&self.root_abs, &join(&service, f))).is_file()
                })
            {
                continue;
            }
            let mut reach = None;
            if index.packages.contains(&service) {
                reach = Some(service.clone());
            }
            let mut ancestor = parent(&service);
            while reach.is_none() && !ancestor.is_empty() {
                if index.packages.contains(ancestor) {
                    reach = Some(service.clone());
                } else if non_project(ancestor) {
                    reach = Some(ancestor.to_string());
                }
                ancestor = parent(ancestor);
            }
            let mut roots = vec![service.clone()];
            let src = join(&service, "src");
            if index.dirs.contains(&src) && !index.packages.contains(&src) {
                roots.push(src);
            }
            for (file, cfg) in [("pyproject.toml", false), ("setup.cfg", true)] {
                if let Some(content) =
                    super::query::read_source(pos_resolve(&self.root_abs, &join(&service, file)))
                {
                    roots.extend(
                        declared_roots(&content, cfg)
                            .into_iter()
                            .map(|p| join(&service, &p))
                            .filter(|p| index.dirs.contains(p)),
                    );
                }
            }
            for root in roots {
                if let Some(reach) = &reach {
                    index.add_local(root, reach.clone());
                } else {
                    index.project.insert(root);
                }
            }
        }
        index.local.retain(|r, _| !index.project.contains(r));
        Ok(index)
    }
    fn python_module_index(&mut self) -> Res<Rc<PythonModules>> {
        if let Some(index) = &self.py_globals.modules {
            return Ok(index.clone());
        }
        let index = Rc::new(self.build_python_modules()?);
        self.py_globals.modules = Some(index.clone());
        Ok(index)
    }
    pub(in crate::resolve) fn python_module_path(
        &mut self,
        spec: &str,
        from: &str,
    ) -> Res<Option<String>> {
        Ok(self.python_module_index()?.resolve(spec, from))
    }
}

#[cfg_attr(feature = "addon", napi)]
impl KernelResolver {
    #[cfg_attr(feature = "addon", napi)]
    pub fn resolve_python_module(&mut self, spec: String, from: String) -> Result<Option<String>> {
        self.python_module_path(&spec, &from).map_err(Error::from)
    }
    /// Stable root-set serialization. The caller may hash it; sync compares it
    /// before/after extraction, using a fresh resolver snapshot each time.
    #[cfg_attr(feature = "addon", napi)]
    pub fn python_package_roots(&mut self) -> Result<String> {
        let index = self.build_python_modules().map_err(Error::from)?;
        if index.files.is_empty() {
            return Ok(String::new());
        }
        let mut project: Vec<_> = index.project.iter().collect();
        project.sort();
        let mut local: Vec<_> = index
            .local
            .iter()
            .map(|(r, s)| format!("{r}\t{s}"))
            .collect();
        local.sort();
        Ok(format!(
            "{}\0{}",
            project.into_iter().cloned().collect::<Vec<_>>().join("\n"),
            local.join("\n")
        ))
    }
    #[cfg_attr(feature = "addon", napi)]
    pub fn clear_python_module_caches(&mut self) {
        self.py_globals = Default::default();
    }
    #[cfg_attr(feature = "addon", napi)]
    pub fn python_package_reopen_scope(
        &mut self,
        changed_files: Vec<String>,
    ) -> Result<PythonReopenScope> {
        let index = self.build_python_modules().map_err(Error::from)?;
        let roots: Vec<_> = index.project.iter().chain(index.local.keys()).collect();
        let names = |file: &str| -> Vec<String> {
            let module = if file == "__init__.py" {
                ""
            } else {
                file.strip_suffix("/__init__.py")
                    .unwrap_or_else(|| file.strip_suffix(".py").unwrap_or(file))
            };
            roots
                .iter()
                .filter_map(|r| {
                    if r.is_empty() {
                        Some(module)
                    } else {
                        module.strip_prefix(&format!("{r}/"))
                    }
                })
                .filter(|p| !p.is_empty())
                .map(|p| p.replace('/', "."))
                .collect()
        };
        let mut affected = HashSet::new();
        let mut dirs = HashSet::new();
        for file in changed_files
            .iter()
            .map(|f| f.replace('\\', "/"))
            .filter(|f| f.ends_with(".py"))
        {
            affected.extend(names(&file));
            if file == "__init__.py" || file.ends_with("/__init__.py") {
                dirs.insert(parent(&file).to_string());
            }
        }
        let mut targets: Vec<_> = index
            .files
            .iter()
            .filter(|f| {
                names(f).iter().any(|m| {
                    affected
                        .iter()
                        .any(|a| m == a || m.starts_with(&format!("{a}.")))
                })
            })
            .cloned()
            .collect();
        targets.sort();
        let mut dirs: Vec<_> = dirs.into_iter().collect();
        dirs.sort();
        let mut leaves: Vec<_> = affected
            .iter()
            .filter_map(|a| a.rsplit('.').next())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        leaves.sort();
        Ok(PythonReopenScope {
            target_files: targets,
            source_dirs: dirs,
            module_leaves: leaves.into_iter().map(str::to_string).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn index(files: &[&str], project: &[&str], local: &[(&str, &str)]) -> PythonModules {
        let mut i = PythonModules::default();
        for f in files {
            i.files.insert(f.to_string());
            let mut d = parent(f);
            while !d.is_empty() {
                i.dirs.insert(d.to_string());
                d = parent(d);
            }
            if f.ends_with("/__init__.py") {
                i.packages.insert(parent(f).to_string());
            }
        }
        i.project.extend(project.iter().map(|s| s.to_string()));
        i.local
            .extend(local.iter().map(|(r, s)| (r.to_string(), s.to_string())));
        i
    }
    #[test]
    fn root_lookup_does_not_capture_fixture_or_nested_namesake() {
        let i = index(
            &[
                "src/pkg/__init__.py",
                "src/pkg/f.py",
                "tests/fixtures/json.py",
                "app/utils/json.py",
            ],
            &["", "src"],
            &[("tests/fixtures", "tests/fixtures")],
        );
        assert_eq!(
            i.resolve("pkg.f", "app/main.py"),
            Some("src/pkg/f.py".into())
        );
        assert_eq!(i.resolve("json", "app/main.py"), None);
        assert_eq!(
            i.resolve("json", "tests/fixtures/main.py"),
            Some("tests/fixtures/json.py".into())
        );
    }
    #[test]
    fn relative_imports_and_regular_packages_follow_python_search_order() {
        let i = index(
            &[
                "pkg/__init__.py",
                "pkg/sub/__init__.py",
                "pkg/sub.py",
                "pkg/sub/f.py",
                "pkg/plain.py",
                "pkg/plain/f.py",
            ],
            &[""],
            &[],
        );
        assert_eq!(
            i.resolve(".f", "pkg/sub/main.py"),
            Some("pkg/sub/f.py".into())
        );
        assert_eq!(i.resolve("...x", "pkg/sub/main.py"), None);
        assert_eq!(
            i.resolve("pkg.sub", "app.py"),
            Some("pkg/sub/__init__.py".into())
        );
        assert_eq!(i.resolve("pkg.plain.f", "app.py"), None);
    }
    #[test]
    fn ambiguous_services_do_not_pick_an_arbitrary_module() {
        let i = index(
            &[
                "a/core/__init__.py",
                "a/core/f.py",
                "b/core/__init__.py",
                "b/core/f.py",
            ],
            &["", "a", "b"],
            &[],
        );
        assert_eq!(i.resolve("core.f", "a/main.py"), Some("a/core/f.py".into()));
        assert_eq!(i.resolve("core.f", "elsewhere/main.py"), None);
    }
    #[test]
    fn build_backend_roots_are_literal_and_relative() {
        assert_eq!(
            declared_roots(
                "[tool.setuptools.packages.find]\nwhere = [\"lib\", \"../outside\"]\n",
                false
            ),
            vec!["lib"]
        );
        assert_eq!(
            declared_roots("[options]\npackage_dir =\n    = lib\n", true),
            vec!["lib"]
        );
    }
}

#[cfg_attr(feature = "addon", napi)]
impl KernelResolver {
    /// Flat importer entries for grouping by file and local name. A binding is
    /// `file\0symbol`, `file\0` for a submodule, or null for a namespace.
    #[cfg_attr(feature = "addon", napi)]
    pub fn python_package_import_bindings(
        &mut self,
        init_files: Vec<String>,
    ) -> Result<Vec<PythonImportBinding>> {
        let mut packages: HashSet<_> = init_files
            .into_iter()
            .map(|f| f.replace('\\', "/"))
            .collect();
        let index = self.python_module_index().map_err(Error::from)?;
        let mut out: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, Option<String>>,
        > = Default::default();
        loop {
            let before = packages.len();
            for file in &index.files {
                let imports = self.import_mappings(file).map_err(Error::from)?;
                for imp in imports.iter() {
                    let Some(module) = index
                        .resolve(&imp.source, file)
                        .filter(|p| packages.contains(p) && p != file)
                    else {
                        continue;
                    };
                    let binding = if imp.is_namespace || imp.exported_name == "*" {
                        None
                    } else {
                        match self
                            .python_module_symbol(
                                &module,
                                &imp.exported_name,
                                0,
                                &mut HashSet::new(),
                            )
                            .map_err(Error::from)?
                        {
                            Some(n) => Some(format!("{}\0{}", n.file_path, n.name)),
                            None => {
                                let spec = if imp.source.ends_with('.') {
                                    format!("{}{}", imp.source, imp.exported_name)
                                } else {
                                    format!("{}.{}", imp.source, imp.exported_name)
                                };
                                Some(
                                    index
                                        .resolve(&spec, file)
                                        .map_or("\0".into(), |f| format!("{f}\0")),
                                )
                            }
                        }
                    };
                    let names = out.entry(file.clone()).or_default();
                    names.insert(imp.local_name.clone(), binding);
                    if imp.is_namespace {
                        names.insert(imp.source.split('.').next().unwrap_or("").to_string(), None);
                    }
                    if file == "__init__.py" || file.ends_with("/__init__.py") {
                        packages.insert(file.clone());
                    }
                }
            }
            if packages.len() == before {
                break;
            }
        }
        Ok(out
            .into_iter()
            .flat_map(|(file, names)| {
                names
                    .into_iter()
                    .map(move |(local_name, binding)| PythonImportBinding {
                        file_path: file.clone(),
                        local_name,
                        binding,
                    })
            })
            .collect())
    }
}

#[cfg_attr(feature = "addon", napi)]
impl KernelResolver {
    /// The module ref of `from X import y` is emitted at the statement's
    /// start. Imported names have their own columns, even when one is also X.
    #[cfg_attr(feature = "addon", napi)]
    pub fn is_python_from_import_module(&mut self, r: ResolveRefIn) -> bool {
        if r.language != "python" || r.reference_kind != "imports" {
            return false;
        }
        let Some(file) = self.read_file(&r.file_path) else {
            return false;
        };
        let Some(tree) = self.parsed_tree(&file, &r) else {
            return false;
        };
        let text = file.text();
        let mut scope = Some(crate::resolve::iteration::descendant_for_position(
            tree.root_node(),
            text,
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        ));
        while let Some(node) = scope {
            if node.kind() == "import_from_statement" {
                return node.start_position().row as i64 + 1 == r.line
                    && node.start_position().column as i64 == r.column
                    && node.child_by_field_name("module_name").is_some_and(|m| {
                        text.get(m.start_byte()..m.end_byte()) == Some(r.reference_name.as_str())
                    });
            }
            scope = node.parent();
        }
        false
    }
}
