//! Shared visibility and binding guards for native name matching.
use super::*;

pub(super) fn test_suite_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    name.starts_with("test_")
        || name == "conftest.py"
        || re!(r"[._-](?:tests?|specs?)\.[a-z0-9]+$").is_match(name)
        || re!(r"(?:Test|Tests|TestCase)\.(?:java|kt|kts|swift|cs|scala|groovy|m|mm|vb|fs)$")
            .is_match(path)
        || re!(r"(?:^|/)(?:tests?|__tests__|specs?|e2e)/").is_match(&lower)
        || re!(r"(?:^|/)[A-Za-z0-9]*(?:Test|Tests|Spec)/").is_match(path)
}

/// The binding patterns `python_binds_name` tests for a name.
#[derive(Clone, Copy)]
enum PyBinding {
    /// An assignment or annotation inside the enclosing function.
    Assign,
    /// A `for` or `as` target inside the enclosing function.
    Target,
    /// A parameter of the enclosing function.
    Params,
    /// An assignment or annotation anywhere in the file.
    Top,
}

impl PyBinding {
    fn pattern(self, name: &str) -> String {
        let name = regex::escape(name);
        match self {
            PyBinding::Assign => format!(
                r"^\s*(?:[\w\s,*()\[\]]*,\s*)?\(?\*?{name}\)?\s*(?:,[\w\s,*()\[\]]*)?(?::[^=]+)?=(?:[^=]|$)"
            ),
            PyBinding::Target => format!(r"\bfor\s+[\w\s,()]*\b{name}\b[\w\s,()]*\s+in\b|\bas\s+{name}\b"),
            PyBinding::Params => format!(r"[(,]\s*\*{{0,2}}{name}\s*(?:[:=,)]|$)"),
            PyBinding::Top => format!(r"^(?:[\w,\s]*,\s*)?{name}\s*(?:,[\w\s,]*)?(?::[^=]+)?=(?:[^=]|$)"),
        }
    }

    /// A necessary condition for `pattern` to match `line`, checked without a
    /// regex: the name followed by the characters the pattern requires next.
    fn may_match(self, line: &str, name: &str) -> bool {
        match self {
            PyBinding::Assign => name_then(line, name, true, &[',', ':', '='], false),
            PyBinding::Target => line.contains(name),
            PyBinding::Params => name_then(line, name, false, &[':', '=', ',', ')'], true),
            PyBinding::Top => name_then(line, name, false, &[',', ':', '='], false),
        }
    }
}

/// Whether `name` occurs in `line` followed by optional whitespace (after one
/// optional `)` when `close_paren`) and then a character of `next`, or by the
/// end of `line` when `at_end`.
fn name_then(line: &str, name: &str, close_paren: bool, next: &[char], at_end: bool) -> bool {
    line.match_indices(name).any(|(at, _)| {
        let mut rest = &line[at + name.len()..];
        if close_paren {
            rest = rest.strip_prefix(')').unwrap_or(rest);
        }
        match rest.trim_start().chars().next() {
            Some(c) => next.contains(&c),
            None => at_end,
        }
    })
}

impl KernelResolver {
    pub(super) fn local_declaration_scope(&mut self, n: &KNode) -> Res<Option<(i64, i64)>> {
        if no_nested_functions(&n.language)
            || (matches!(n.language.as_str(), "lua" | "luau") && !self.is_lua_local(n))
        {
            return Ok(None);
        }
        let key = format!("local:{}", n.id);
        if let Some(hit) = self.lexical_scope_memo.get(&key) {
            return Ok(*hit);
        }
        let scope = self
            .nodes_in_file(&n.file_path)?
            .iter()
            .filter(|f| {
                matches!(f.kind.as_str(), "function" | "method")
                    && f.id != n.id
                    && f.start_line <= n.start_line
                    && f.end_line >= n.start_line
                    && f.start_line != f.end_line
                    && (f.start_line != n.start_line || f.start_column < n.start_column)
            })
            .min_by_key(|f| f.end_line - f.start_line)
            .map(|f| (f.start_line, f.end_line));
        self.lexical_scope_memo.insert(key, scope);
        Ok(scope)
    }

    fn is_minified_script_uncached(&mut self, file: &str) -> bool {
        if !re!(r"(?i)\.(?:m?js|cjs)$").is_match(file) {
            return false;
        }
        if re!(r"(?i)[.-]min\.m?js$").is_match(file) {
            return true;
        }
        self.read_file(file).is_some_and(|s| {
            let text = s.text();
            if text.len() < 4000 {
                return false;
            }
            if re!(r"\bfunction __webpack_require__\s*\(").is_match(text) {
                return true;
            }
            let mut long = 0;
            let mut punctuation = 0;
            for line in text.lines().filter(|l| l.len() >= 1000) {
                long += line.len();
                punctuation += line.bytes().filter(|b| b";{}(),".contains(b)).count();
            }
            long * 2 >= text.len() && punctuation * 100 >= long * 3
        })
    }

    fn sfc_private_uncached(&mut self, n: &KNode) -> bool {
        let svelte = n.file_path.ends_with(".svelte");
        if (!svelte && !n.file_path.ends_with(".vue"))
            || matches!(n.kind.as_str(), "component" | "file")
        {
            return false;
        }
        let Some(lines) = self.read_file(&n.file_path) else {
            return false;
        };
        let mut open = None;
        for (i, line) in lines.iter().enumerate() {
            if let Some(tag) = re!(r"(?i)<script\b([^>]*)>").captures(line) {
                if open.is_none() {
                    open = Some((
                        i as i64 + 1,
                        if svelte {
                            re!(r#"\bmodule\b|context\s*=\s*["']module["']"#).is_match(&tag[1])
                        } else {
                            !re!(r"\bsetup\b").is_match(&tag[1])
                        },
                    ));
                }
            }
            if re!(r"(?i)</script\s*>").is_match(line) {
                if let Some((start, exported)) = open.take() {
                    if n.start_line >= start && n.start_line <= i as i64 + 1 {
                        return !exported
                            && (svelte
                                || !lines.get((n.start_line - 1).max(0) as usize).is_some_and(
                                    |l| {
                                        re!(
                                            r"^\s*export\s+(?:declare\s+)?(?:interface|type|enum)\b"
                                        )
                                        .is_match(l)
                                    },
                                ));
                    }
                }
            }
        }
        true
    }

    pub(super) fn unnamed_test_double(
        &mut self,
        method: &KNode,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> bool {
        let owner = method
            .qualified_name
            .rsplit_once("::")
            .map(|(p, _)| p.rsplit([':', '.']).next().unwrap_or(""))
            .unwrap_or("");
        let double = re!(r"(?i)\b(?:fake|mock|mocked|stub|dummy|spy)\b");
        double.is_match(&split_camel_case(owner).join(" "))
            && !double.is_match(&split_camel_case(receiver).join(" "))
            && !self
                .read_file(&r.file_path)
                .is_some_and(|s| s.text().contains(owner))
    }

    fn python_fixture_decorated(&mut self, n: &KNode) -> bool {
        let Some(lines) = self.read_file(&n.file_path) else {
            return false;
        };
        let mut fixture = false;
        let mut open = 0i64;
        for line in lines
            .iter()
            .take((n.start_line - 1).max(0) as usize)
            .rev()
            .take(16)
        {
            let text = line.trim();
            open += text.matches(')').count() as i64 - text.matches('(').count() as i64;
            if open > 0 {
                continue;
            }
            if !text.starts_with('@') {
                break;
            }
            if re!(r"^@(?:\w+\.)*fixture\b").is_match(text) {
                fixture = true;
                break;
            }
        }
        fixture
    }

    pub(super) fn python_fixture_reachable(&mut self, n: &KNode, file: &str) -> bool {
        if n.language != "python" || n.kind != "function" {
            return false;
        }
        let conftest = n.file_path.strip_suffix("conftest.py");
        if let Some(dir) = conftest {
            return dir.is_empty() || file.starts_with(dir);
        }
        if !self.python_fixture_decorated(n) {
            return false;
        }
        if n.file_path == file {
            return true;
        }
        let mut dir = pos_dirname(file);
        loop {
            let conftest = if dir == "." || dir.is_empty() {
                "conftest.py".to_string()
            } else {
                format!("{dir}/conftest.py")
            };
            if let Some(s) = self.read_file(&conftest) {
                let text = s.text();
                let dotted = n.file_path.trim_end_matches(".py").replace('/', ".");
                if re!(r#"(?m)^\s*from\s+([\w.]+)\s+import\s+\*|["']([\w.]+)["']"#)
                    .captures_iter(text)
                    .any(|m| {
                        m.get(1).or_else(|| m.get(2)).is_some_and(|x| {
                            dotted == x.as_str() || dotted.ends_with(&format!(".{}", x.as_str()))
                        })
                    })
                {
                    return true;
                }
            }
            if dir == "." || dir.is_empty() {
                break;
            }
            let next = pos_dirname(dir);
            if next == dir {
                break;
            }
            dir = next;
        }
        false
    }

    pub(super) fn python_locally_bound(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "python" || !re!(r"^[A-Za-z_]\w*$").is_match(name) {
            return Ok(false);
        }
        // The answer depends only on the file, the line and the name; the
        // name-match candidate filter asks it once per cross-file candidate.
        let key = (r.file_path.clone(), r.line, name.to_string());
        if let Some(&bound) = self.python_bound_memo.get(&key) {
            return Ok(bound);
        }
        let bound = self.python_binds_name(name, r)?;
        self.python_bound_memo.insert(key, bound);
        Ok(bound)
    }

    /// `python_locally_bound` uncached. Every pattern here needs the name
    /// followed by particular characters, so `name_then` rejects most lines
    /// (a call such as `x = name(...)`) before a pattern is compiled or run;
    /// on CPython a pattern per distinct name and a whole-file scan per
    /// candidate had made this most of the resolve time.
    fn python_binds_name(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        if self
            .import_mappings(&r.file_path)?
            .iter()
            .any(|m| m.local_name == name && !m.is_namespace)
        {
            return Ok(false);
        }
        let function = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "function" | "method")
                    && n.start_line <= r.line
                    && n.end_line >= r.line
            })
            .min_by_key(|n| n.end_line - n.start_line)
            .cloned();
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        if let Some(f) = function {
            let lo = (f.start_line - 1).max(0) as usize;
            let mut i = lo;
            let mut signature = String::new();
            while i < lines.len().min(lo + 20) {
                signature.push_str(&lines[i]);
                if re!(r"\)\s*(?:->[^:]*)?:\s*(?:#.*)?$").is_match(&lines[i]) {
                    break;
                }
                i += 1;
            }
            if let Some((_, rest)) = signature.split_once('(') {
                if Self::py_binding_in(PyBinding::Params, &format!("({rest}"), name)? {
                    return Ok(true);
                }
            }
            let finder = memchr::memmem::Finder::new(name);
            for line in lines.iter().take((r.line - 1).max(0) as usize).skip(i + 1) {
                if finder.find(line.as_bytes()).is_some()
                    && (Self::py_binding_in(PyBinding::Assign, line, name)?
                        || Self::py_binding_in(PyBinding::Target, line, name)?)
                {
                    return Ok(true);
                }
            }
        }
        // A binding anywhere in the file: the same answer for every line.
        let key = (r.file_path.clone(), name.to_string());
        if let Some(&bound) = self.python_file_binds_memo.get(&key) {
            return Ok(bound);
        }
        let finder = memchr::memmem::Finder::new(name);
        let mut bound = false;
        for line in lines.iter() {
            if finder.find(line.as_bytes()).is_some() && Self::py_binding_in(PyBinding::Top, line, name)? {
                bound = true;
                break;
            }
        }
        self.python_file_binds_memo.insert(key, bound);
        Ok(bound)
    }

    /// Whether `line` holds `kind`'s binding of `name`. The pattern is
    /// compiled and run only for a line that passes `may_match`.
    fn py_binding_in(kind: PyBinding, line: &str, name: &str) -> Res<bool> {
        Ok(kind.may_match(line, name) && Self::cached_regex(&kind.pattern(name))?.is_match(line))
    }

    pub(super) fn python_module_symbol(
        &mut self,
        file: &str,
        name: &str,
        depth: usize,
        visited: &mut HashSet<String>,
    ) -> Res<Option<Arc<KNode>>> {
        if !visited.insert(format!("{file}\0{name}")) {
            return Ok(None);
        }
        if let Some(n) = self.nodes_in_file(file)?.iter().find(|n| {
            n.name == name
                && !n.qualified_name.contains("::")
                && matches!(
                    n.kind.as_str(),
                    "class" | "function" | "variable" | "constant"
                )
        }) {
            return Ok(Some(n.clone()));
        }
        if depth >= 3 {
            return Ok(None);
        }
        let top_sources: HashSet<_> = self
            .read_file(file)
            .map(|s| {
                re!(r"(?m)^from\s+([\w.]+)\s+import\b")
                    .captures_iter(s.text())
                    .map(|m| m[1].to_string())
                    .collect()
            })
            .unwrap_or_default();
        let mut sources: Vec<_> = self
            .import_mappings(file)?
            .iter()
            .filter(|m| !m.is_namespace && m.local_name == name && top_sources.contains(&m.source))
            .map(|m| (m.source.clone(), m.exported_name.clone()))
            .collect();
        if let Some(lines) = self.read_file(file) {
            for m in re!(r"(?m)^from\s+([\w.]+)\s+import\s+\*").captures_iter(lines.text()) {
                sources.push((m[1].to_string(), name.to_string()));
            }
        }
        for (source, exported) in sources {
            let path = self.resolve_import_path(&source, file, "python")?.or(self
                .find_python_module_file(&source, file)?
                .map(|n| n.file_path.clone()));
            if let Some(path) = path {
                if path != file {
                    if let Some(n) =
                        self.python_module_symbol(&path, &exported, depth + 1, visited)?
                    {
                        return Ok(Some(n));
                    }
                }
            }
        }
        Ok(None)
    }
}

impl KernelResolver {
    pub(super) fn outside_js_local(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if !is_js_family(&r.language) || !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name) {
            return Ok(false);
        }
        if r.reference_kind == "calls" && !self.is_receiver_less_call(r)? {
            return Ok(false);
        }
        let Some(f) = self.node_by_id(&r.from_node_id)? else {
            return Ok(false);
        };
        if !matches!(f.kind.as_str(), "function" | "method")
            || f.start_line > r.line
            || f.end_line < r.line
        {
            return Ok(false);
        }
        let bindings = self.bindings(&r.file_path)?;
        let bound = innermost_binding(&bindings, &r.reference_name, Some(r.line));
        let local = bound.is_some_and(|b| b.kind == "param" && b.scope_start >= f.start_line)
            || self.js_source_local_binding_within(r, Some(&f));
        if !local {
            return Ok(false);
        }
        if self
            .js_typed_destructured_member(r)?
            .is_some_and(|member| member.id == n.id)
        {
            return Ok(false);
        }
        Ok(n.file_path != r.file_path || n.start_line < f.start_line || n.start_line > f.end_line)
    }

    pub(super) fn go_external_qualified(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "go" || r.reference_kind == "imports" {
            return Ok(false);
        }
        let name = r
            .reference_name
            .rsplit('.')
            .next()
            .unwrap_or(&r.reference_name);
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else {
            return Ok(false);
        };
        let at = super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len());
        let root = if line[at..].starts_with(name) {
            re!(r"(?:^|[^\w.])([A-Za-z_]\w*)\.$")
                .captures(&line[..at])
                .map(|m| m[1].to_string())
        } else {
            let re = Self::cached_regex(&format!(
                r"(?:^|[^\w.])([A-Za-z_]\w*)\.{}\b",
                regex::escape(name)
            ))?;
            re.captures(line).map(|m| m[1].to_string())
        };
        let Some(root) = root else {
            return Ok(false);
        };
        let imports = self.import_mappings(&r.file_path)?;
        let Some(imp) = imports.iter().find(|m| m.local_name == root) else {
            return Ok(false);
        };
        Ok(!imp.source.starts_with('.')
            && !imp.source.contains("/internal/")
            && !self
                .go_module_path
                .as_ref()
                .is_some_and(|m| imp.source == *m || imp.source.starts_with(&format!("{m}/"))))
    }

    pub(super) fn python_fixture_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        if r.language != "python"
            || !re!(r"^[a-z_]\w*$").is_match(receiver)
            || matches!(receiver, "self" | "cls")
        {
            return Ok(None);
        }
        let caller = self.node_by_id(&r.from_node_id)?;
        let Some(caller) = caller.filter(|n| matches!(n.kind.as_str(), "function" | "method"))
        else {
            return Ok(None);
        };
        let default_test = caller.name.starts_with("test_")
            && re!(r"(?:^|/)(?:test_[^/]*|[^/]*_test)\.py$").is_match(&r.file_path);
        if !default_test && !self.python_fixture_decorated(&caller) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let signature = lines
            .iter()
            .skip((caller.start_line - 1).max(0) as usize)
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let signature = signature.split("):").next().unwrap_or(&signature);
        if !Self::cached_regex(&format!(
            r"[(,]\s*{}\s*(?:[:=,)]|$)",
            regex::escape(receiver)
        ))?
        .is_match(signature)
        {
            return Ok(None);
        }
        let mut fixtures = Vec::new();
        for n in self.nodes_by_name(receiver)?.iter() {
            if self.python_fixture_reachable(n, &r.file_path) {
                fixtures.push(n.clone());
            }
        }
        fixtures.sort_by_key(|n| {
            (
                n.file_path != r.file_path,
                std::cmp::Reverse(n.file_path.len()),
            )
        });
        let Some(fixture) = fixtures.first() else {
            return Ok(None);
        };
        let Some(lines) = self.read_file(&fixture.file_path) else {
            return Ok(None);
        };
        let body = lines
            .iter()
            .skip(fixture.start_line.max(0) as usize)
            .take((fixture.end_line - fixture.start_line).max(0) as usize)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let Some(returned) =
            re!(r"(?m)^\s*(?:return|yield)\s+([A-Za-z_][\w.]*)\s*(\()?").captures(&body)
        else {
            return Ok(None);
        };
        let direct = if returned.get(2).is_some() {
            Some(returned[1].to_string())
        } else {
            Self::cached_regex(&format!(
                r"(?m)^\s*{}\s*=\s*([A-Za-z_][\w.]*)\s*\(",
                regex::escape(&returned[1])
            ))?
            .captures(&body)
            .map(|m| m[1].to_string())
        };
        Ok(direct
            .and_then(|s| s.rsplit('.').next().map(str::to_string))
            .filter(|s| s.starts_with(|c: char| c.is_ascii_uppercase())))
    }

    /// Additional scope evidence for Rust paths and imported leaves; None
    /// leaves the existing prelude and glob rules in charge.
    pub(super) fn rust_upstream_name_visible(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<bool> {
        if r.language != "rust" {
            return None;
        }
        let lines = self.read_file(&r.file_path)?;
        let line = lines.get((r.line - 1).max(0) as usize)?;
        let name = r
            .reference_name
            .rsplit("::")
            .next()
            .unwrap_or(&r.reference_name);
        let at = super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len());
        let path_re = Self::cached_regex(&format!(
            r"((?:[A-Za-z_]\w*\s*::\s*)+){name}\b",
            name = regex::escape(name)
        ))
        .ok()?;
        let path = if line[at..].starts_with(name) {
            re!(r"((?:[A-Za-z_]\w*\s*::\s*)+)$")
                .captures(&line[..at])
                .map(|m| m[1].to_string())
        } else {
            path_re.captures(line).map(|m| m[1].to_string())
        };
        let (external, bound) = if let Some(hit) = self.upstream_rust_uses.get(&r.file_path) {
            hit.clone()
        } else {
            let mut external = HashSet::new();
            let mut bound = HashSet::new();
            let mut deps = self.upstream_rust_deps.clone().unwrap_or_default();
            if self.upstream_rust_deps.is_none() {
                let mut manifests = HashSet::from(["Cargo.toml".to_string()]);
                if let Some(files) = self.sorted_files() {
                    for file in files.iter().filter(|f| f.ends_with(".rs")) {
                        let mut dir = pos_dirname(file);
                        while dir != "." && !dir.is_empty() {
                            manifests.insert(format!("{dir}/Cargo.toml"));
                            let next = pos_dirname(dir);
                            if next == dir {
                                break;
                            }
                            dir = next;
                        }
                    }
                }
                let mut own = HashSet::new();
                for manifest in manifests {
                    if let Some(source) = self.read_file(&manifest) {
                        let text = source.text();
                        if let Some(m) =
                            re!(r#"(?s)\[package\][^\[]*?\bname\s*=\s*"([^"]+)""#).captures(text)
                        {
                            own.insert(m[1].replace('-', "_"));
                        }
                        let mut in_deps = false;
                        for line in source.iter() {
                            let line = line.split('#').next().unwrap_or("").trim();
                            if let Some(header) = re!(r"^\[([^\]]+)\]$").captures(line) {
                                if let Some(named) =
                                    re!(r"(?:^|\.)(?:dev-|build-)?dependencies\.([A-Za-z0-9_-]+)$")
                                        .captures(&header[1])
                                {
                                    deps.insert(named[1].replace('-', "_"));
                                }
                                in_deps = re!(r"(?:^|\.)(?:dev-|build-)?dependencies$")
                                    .is_match(&header[1]);
                                continue;
                            }
                            if in_deps {
                                if let Some(m) = re!(r"^([A-Za-z0-9_-]+)\s*=").captures(line) {
                                    deps.insert(m[1].replace('-', "_"));
                                }
                            }
                        }
                    }
                }
                for name in &own {
                    deps.remove(name);
                    self.upstream_scope_memo
                        .insert(format!("rust-crate:{name}"), true);
                }
                self.upstream_rust_deps = Some(deps.clone());
            }
            for m in re!(r"(?:^|[;{}\s])use\s+([^;]+);")
                .captures_iter(&super::name_scope::strip_rust_comments(lines.text()))
            {
                let tree = &m[1];
                let root = tree
                    .trim_start_matches(':')
                    .trim_start()
                    .split("::")
                    .next()
                    .unwrap_or("")
                    .trim();
                let outside = matches!(root, "std" | "core" | "alloc") || deps.contains(root);
                for leaf in
                    re!(r"([A-Za-z_]\w*)\s*(?:[,}]|$)|\bas\s+([A-Za-z_]\w*)").captures_iter(tree)
                {
                    let name = leaf.get(2).or_else(|| leaf.get(1)).unwrap().as_str();
                    if name != "self" && name != "as" {
                        if outside {
                            external.insert(name.to_string());
                        } else {
                            bound.insert(name.to_string());
                        }
                    }
                }
                for owner in re!(r"([A-Za-z_]\w*)\s*::\s*\{[^{}]*\bself\b").captures_iter(tree) {
                    if outside {
                        external.insert(owner[1].to_string());
                    } else {
                        bound.insert(owner[1].to_string());
                    }
                }
            }
            self.upstream_rust_uses
                .insert(r.file_path.clone(), (external.clone(), bound.clone()));
            (external, bound)
        };
        if let Some(path) = path {
            let segments: Vec<_> = path
                .split("::")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let root = segments.first().copied().unwrap_or("");
            let seg = segments.last().copied().unwrap_or("");
            if r.reference_kind == "references" && segments.len() == 1 {
                if seg == "Self" {
                    let header = lines
                        .iter()
                        .take(r.line.max(0) as usize)
                        .enumerate()
                        .rev()
                        .find(|(_, l)| {
                            re!(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:impl|trait)\b")
                                .is_match(l)
                        })
                        .map(|(i, _)| i as i64 + 1)
                        .unwrap_or(0);
                    return Some(
                        n.kind == "type_alias"
                            && n.file_path == r.file_path
                            && header > 0
                            && n.start_line >= header
                            && n.start_line <= r.line,
                    );
                }
                if re!(r"^[A-Z]\w*$").is_match(seg) {
                    let generic =
                        Self::cached_regex(&format!(r"<[^<>]*\b{}\b\s*[:,>=]", regex::escape(seg)))
                            .ok()?;
                    if lines
                        .iter()
                        .take(r.line.max(0) as usize)
                        .any(|l| generic.is_match(l))
                    {
                        return Some(false);
                    }
                }
            }
            if matches!(root, "std" | "core" | "alloc") && n.file_path != r.file_path {
                return Some(false);
            }
            if external.contains(seg) && !bound.contains(seg) {
                return Some(false);
            }
            if matches!(seg, "crate" | "self" | "super" | "Self")
                || n.file_path == r.file_path
                || self.upstream_scope_memo.get(&format!("rust-crate:{seg}")) == Some(&true)
            {
                return Some(true);
            }
            return Some(
                n.file_path
                    .rsplit('/')
                    .next()
                    .is_some_and(|f| f.trim_end_matches(".rs") == seg)
                    || n.file_path.contains(&format!("/{seg}/"))
                    || n.qualified_name.split("::").any(|x| x == seg),
            );
        }
        if n.file_path == r.file_path {
            return Some(true);
        }
        if external.contains(name) && !bound.contains(name) && !line[..at].trim_end().ends_with('.')
        {
            return Some(false);
        }
        if !matches!(
            n.kind.as_str(),
            "method" | "property" | "field" | "enum_member" | "file" | "module" | "namespace"
        ) && r.reference_kind != "imports"
        {
            let glob = lines.rust_scope_uses().globs.iter().any(|g| {
                g == "super"
                    || n.file_path.contains(&format!("/{g}/"))
                    || n.file_path.ends_with(&format!("/{g}.rs"))
            });
            return Some(bound.contains(name) || glob);
        }
        None
    }
}

impl KernelResolver {
    pub(super) fn is_minified_script(&mut self, file: &str) -> bool {
        let key = format!("minified:{file}");
        if let Some(hit) = self.upstream_scope_memo.get(&key) {
            return *hit;
        }
        let hit = self.is_minified_script_uncached(file);
        self.upstream_scope_memo.insert(key, hit);
        hit
    }
}

impl KernelResolver {
    pub(super) fn sfc_private(&mut self, n: &KNode) -> bool {
        let key = format!("sfc:{}", n.id);
        if let Some(hit) = self.upstream_scope_memo.get(&key) {
            return *hit;
        }
        let hit = self.sfc_private_uncached(n);
        self.upstream_scope_memo.insert(key, hit);
        hit
    }
}

impl KernelResolver {
    pub(super) fn python_fixture_member(
        &mut self,
        receiver: &str,
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<KCand>>> {
        let Some(ty) = self.python_fixture_type(receiver, r)? else {
            return Ok(None);
        };
        let mut fixtures = Vec::new();
        for n in self.nodes_by_name(receiver)?.iter() {
            if self.python_fixture_reachable(n, &r.file_path) {
                fixtures.push(n.clone());
            }
        }
        fixtures.sort_by_key(|n| {
            (
                n.file_path != r.file_path,
                std::cmp::Reverse(n.file_path.len()),
            )
        });
        let Some(fixture) = fixtures.first() else {
            return Ok(None);
        };
        // Resolve the returned type where the fixture imports or declares it.
        let site = r.clone().at(fixture);
        Ok(Some(self.match_bound_type_member(&ty, member, &site)?))
    }
}

#[cfg(test)]
mod tests {
    use super::PyBinding;
    use regex::Regex;

    /// `python_binds_name` runs a binding pattern only on lines that pass its
    /// `may_match` filter, so every line a pattern matches must pass it.
    #[test]
    fn may_match_admits_every_binding_match() {
        let lines = [
            "{n} = 1", "a, {n} = f()", "{n}: int = 3", "{n} : int", "  ({n}) = g()", "*{n}, b = xs",
            "a, *{n} = xs", "{n}, b = 1, 2", "(a, {n}) = t", "[a, {n}] = t", "{n} == 1", "{n}=1",
            "{n}\u{3000}= 1", "{n}\t= 2", "f({n}=1)", "def f(self, {n}, y):", "def f({n}: int = 0)",
            "def f(*{n})", "def f(**{n}", "def f({n}", "y = {n}(x)", "{n}.attr = 1", "x{n} = 1",
            "{n}x = 1", "{n}x, {n} = t", "for {n} in xs:", "with open() as {n}:",
        ];
        let kinds = [PyBinding::Assign, PyBinding::Target, PyBinding::Params, PyBinding::Top];
        for name in ["x", "foo", "über"] {
            let mut matched = [0; 4];
            for (k, kind) in kinds.into_iter().enumerate() {
                let re = Regex::new(&kind.pattern(name)).unwrap();
                for line in lines.map(|l| l.replace("{n}", name)) {
                    if re.is_match(&line) {
                        matched[k] += 1;
                        assert!(kind.may_match(&line, name), "{} {line:?}", kind.pattern(name));
                    }
                }
            }
            assert!(matched.iter().all(|&n| n > 0), "every pattern matched some line for {name}: {matched:?}");
            // The filter is what saves the work: a plain call compiles no pattern.
            let call = format!("y = {name}(z)");
            assert!(!kinds.into_iter().any(|k| !matches!(k, PyBinding::Target) && k.may_match(&call, name)));
        }
    }
}
