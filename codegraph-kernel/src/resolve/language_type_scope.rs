//! Visibility of types and library-owned declarations reached by bare names.
use super::*;

#[derive(Default)]
pub(crate) struct LanguageTypeScope {
    pub(super) package: String,
    namespaces: Vec<String>,
    imports: HashSet<String>,
    demand: HashSet<String>,
    package_object: Option<String>,
    has_package_object: bool,
}
fn visible_type(kind: &str) -> bool {
    matches!(
        kind,
        "class"
            | "struct"
            | "interface"
            | "trait"
            | "enum"
            | "record"
            | "annotation"
            | "type_alias"
            | "protocol"
            | "union"
    )
}
pub(super) type ScalaBlockScope = (Rc<SourceFile>, Option<(i64, i64)>);

impl KernelResolver {
    fn language_file_scope(&mut self, file: &str, language: &str) -> Res<Rc<LanguageTypeScope>> {
        if let Some(hit) = self.language_type_scope_memo.get(file) {
            return Ok(hit.clone());
        }
        let source = self
            .read_file(file)
            .map(|f| super::awaited::strip_ts_comments(f.text()))
            .unwrap_or_default();
        let mut scope = LanguageTypeScope::default();
        let packages: Vec<_> = re!(r"(?m)^\s*package\s+([\w.]+)\s*;?\s*$")
            .captures_iter(&source)
            .map(|m| m[1].to_string())
            .collect();
        scope.package = packages.join(".");
        if language == "scala" {
            scope.has_package_object = re!(r"\bpackage\s+object\b").is_match(&source);
            scope.package_object = re!(r"(?m)^\s*package\s+object\s+([\w$]+)")
                .captures(&source)
                .map(|m| {
                    if scope.package.is_empty() {
                        m[1].to_string()
                    } else {
                        format!("{}.{}", scope.package, &m[1])
                    }
                });
        }
        for m in re!(r"(?m)^\s*import\s+(?:static\s+)?([\w.$]+?)(\.\*)?\s*;").captures_iter(&source)
        {
            if m.get(2).is_some() {
                scope.demand.insert(m[1].to_string());
            } else {
                scope.imports.insert(m[1].to_string());
            }
        }
        if language == "csharp" {
            scope.namespaces = re!(r"(?m)^\s*namespace\s+([\w.]+)")
                .captures_iter(&source)
                .map(|m| m[1].to_string())
                .collect();
            scope.demand.extend(self.csharp_namespace_imports(file)?);
        }
        let scope = Rc::new(scope);
        self.language_type_scope_memo
            .insert(file.to_string(), scope.clone());
        Ok(scope)
    }

    fn csharp_namespace_imports(&mut self, file: &str) -> Res<HashSet<String>> {
        let mut imports = HashSet::new();
        let mut dir = pos_dirname(file).to_string();
        let project = self.csharp_project_of(&dir);
        loop {
            for entry in self.csharp_dir_entries(&dir) {
                if !entry.ends_with(".csproj") && !entry.ends_with(".props") {
                    continue;
                }
                let path = if dir.is_empty() {
                    entry
                } else {
                    format!("{dir}/{entry}")
                };
                let Some(source) = self.read_file(&path) else {
                    continue;
                };
                for m in re!(r#"(?i)<Using\s+Include\s*=\s*"([\w.]+)"([^>]*)>"#)
                    .captures_iter(source.text())
                {
                    if !re!(r#"(?i)\bStatic\s*=\s*"true""#).is_match(&m[2]) {
                        imports.insert(m[1].to_string());
                    }
                }
                if re!(r"(?i)<ImplicitUsings>\s*(?:enable|true)\s*</ImplicitUsings>")
                    .is_match(source.text())
                {
                    imports.extend(
                        [
                            "System",
                            "System.Collections.Generic",
                            "System.IO",
                            "System.Linq",
                            "System.Net.Http",
                            "System.Threading",
                            "System.Threading.Tasks",
                        ]
                        .into_iter()
                        .map(str::to_string),
                    );
                }
            }
            if dir.is_empty() {
                break;
            }
            dir = pos_dirname(&dir).to_string();
        }
        if let Some(globals) = self.csharp_namespace_globals_memo.get(&project) {
            imports.extend(globals.iter().cloned());
            return Ok(imports);
        }
        let files: Vec<_> = self
            .table()?
            .files
            .iter()
            .filter(|f| f.ends_with(".cs"))
            .cloned()
            .collect();
        let mut globals = HashSet::new();
        for candidate in files {
            if self.csharp_project_of(pos_dirname(&candidate)) != project {
                continue;
            }
            let Some(source) = self.read_file(&candidate) else {
                continue;
            };
            for m in re!(r"(?m)^\s*global\s+using\s+([\w.]+)\s*;")
                .captures_iter(&super::awaited::strip_ts_comments(source.text()))
            {
                globals.insert(m[1].to_string());
            }
        }
        imports.extend(globals.iter().cloned());
        self.csharp_namespace_globals_memo
            .insert(project, Rc::new(globals));
        Ok(imports)
    }

    /// Native type visibility guard shared by exact, fuzzy and final-hit paths.
    pub(super) fn language_type_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.language != r.language {
            return Ok(true);
        }
        if r.language == "scala" {
            if r.reference_kind == "references"
                && !r
                    .reference_name
                    .chars()
                    .any(|c| c.is_alphanumeric() || c == '_')
            {
                return Ok(
                    visible_type(&n.kind) || matches!(n.kind.as_str(), "module" | "namespace")
                );
            }
            if let Some((package, start, end)) = self.scala_package_object_of(n, r) {
                if n.file_path == r.file_path && start <= r.line && r.line <= end {
                    return Ok(true);
                }
                let here = self.language_file_scope(&r.file_path, "scala")?;
                if here.package == package || here.package.starts_with(&format!("{package}.")) {
                    return Ok(true);
                }
                let source = self
                    .read_file(&r.file_path)
                    .map(|f| super::awaited::strip_ts_comments(f.text()))
                    .unwrap_or_default();
                return Ok(re!(r"(?m)^\s*import\s+([\w.$]+)")
                    .captures_iter(&source)
                    .any(|m| m[1].starts_with(&format!("{package}."))));
            }
        }
        if r.language == "dart" && n.kind == "class" {
            let source = self.read_file(&n.file_path);
            if source
                .as_ref()
                .and_then(|f| f.get((n.start_line - 1).max(0) as usize))
                .is_some_and(|line| re!(r"^\s*extension\s+on\b").is_match(line))
            {
                return Ok(false);
            }
        }
        if r.language == "dart" && n.kind == "method" && n.file_path != r.file_path {
            if let Some((owner, _)) = n.qualified_name.rsplit_once("::") {
                let decl = self
                    .nodes_in_file(&n.file_path)?
                    .iter()
                    .find(|p| p.kind == "class" && p.qualified_name == owner)
                    .cloned();
                if let Some(decl) = decl {
                    let source = self.read_file(&decl.file_path);
                    if source
                        .as_ref()
                        .and_then(|f| f.get((decl.start_line - 1).max(0) as usize))
                        .is_some_and(|line| re!(r"^\s*extension\s+on\b").is_match(line))
                    {
                        return Ok(false);
                    }
                }
            }
        }
        if !matches!(r.language.as_str(), "java" | "csharp")
            || !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name)
        {
            return Ok(true);
        }
        if r.language == "java" {
            return self.upstream_java_type_visible(n, r);
        }
        if !visible_type(&n.kind) {
            return Ok(true);
        }
        let scope = self.language_file_scope(&r.file_path, "csharp")?;
        let candidate_scope = self.language_file_scope(&n.file_path, "csharp")?;
        let fqn = n.qualified_name.replace("::", ".");
        if let Some(alias) = self.csharp_alias_at(&r.reference_name, r) {
            return Ok(alias == fqn);
        }
        let mut declaration_site = r.clone().at(n);
        declaration_site.column = n.start_column;
        let namespace = self
            .csharp_namespaces_at(&declaration_site)
            .unwrap_or_else(|| candidate_scope.namespaces.clone())
            .join(".");
        let own_namespaces = self
            .csharp_namespaces_at(r)
            .unwrap_or_else(|| scope.namespaces.clone())
            .join(".");
        let namespace_usings = self.csharp_namespace_usings_at(r);
        if !namespace.is_empty()
            && !scope.demand.contains(&namespace)
            && !namespace_usings.contains(&namespace)
            && own_namespaces != namespace
            && !own_namespaces.starts_with(&format!("{namespace}."))
        {
            return Ok(false);
        }
        self.nested_type_visible(n, r)
    }

    fn scala_package_object_of(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<(String, i64, i64)> {
        if let Some(hit) = self.scala_package_object_membership_memo.get(&n.id) {
            return hit.clone();
        }
        let membership = self.scala_package_object_membership(n, r);
        self.scala_package_object_membership_memo
            .insert(n.id.clone(), membership.clone());
        membership
    }

    fn scala_package_object_membership(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<(String, i64, i64)> {
        if !self
            .language_file_scope(&n.file_path, "scala")
            .ok()?
            .has_package_object
        {
            return None;
        }
        let source = self.read_file(&n.file_path)?;
        let mut site = r.clone().at(n);
        site.column = n.start_column;
        let tree = self.parsed_tree(&source, &site)?;
        let mut node = super::iteration::descendant_for_position(
            tree.root_node(),
            source.text(),
            (
                (n.start_line - 1).max(0) as usize,
                n.start_column.max(0) as usize + 1,
            ),
        );
        loop {
            if node.kind() == "package_object" {
                let body = node.child_by_field_name("body")?;
                if (body.start_position().row as i64) < n.start_line
                    && n.end_line <= body.end_position().row as i64 + 1
                {
                    let package = self
                        .language_file_scope(&n.file_path, "scala")
                        .ok()?
                        .package_object
                        .clone()?;
                    return Some((
                        package,
                        body.start_position().row as i64 + 1,
                        body.end_position().row as i64 + 1,
                    ));
                }
                return None;
            }
            let parent = node.parent()?;
            node = parent;
        }
    }

    /// Directives at each enclosing namespace body, innermost first, then the file scope.
    fn csharp_directive_scopes(&mut self, r: &ResolveRefIn) -> Option<Vec<Vec<String>>> {
        let source = self.read_file(&r.file_path)?;
        let tree = self.parsed_tree(&source, r)?;
        let mut node = super::iteration::descendant_for_position(
            tree.root_node(),
            source.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1),
        );
        let mut scopes = Vec::new();
        loop {
            if matches!(
                node.kind(),
                "declaration_list" | "compilation_unit" | "file_scoped_namespace_declaration"
            ) {
                scopes.push(
                    super::iteration::named_children(node)
                        .into_iter()
                        .filter(|n| n.kind() == "using_directive")
                        .map(|directive| {
                            source.text()[directive.start_byte()..directive.end_byte()].to_string()
                        })
                        .collect(),
                );
            }
            let Some(parent) = node.parent() else { break };
            node = parent;
        }
        Some(scopes)
    }

    pub(super) fn csharp_namespace_usings_at(&mut self, r: &ResolveRefIn) -> HashSet<String> {
        let mut namespaces = HashSet::new();
        for scope in self.csharp_directive_scopes(r).unwrap_or_default() {
            for text in scope {
                if let Some(imported) =
                    re!(r"^\s*(?:global\s+)?using\s+(?:global::)?([\w.]+)\s*;").captures(&text)
                {
                    namespaces.insert(imported[1].to_string());
                }
            }
        }
        namespaces
    }

    /// Using aliases belong to their enclosing namespace body, then the file scope.
    pub(super) fn csharp_alias_at(&mut self, name: &str, r: &ResolveRefIn) -> Option<String> {
        for scope in self.csharp_directive_scopes(r)? {
            let mut targets = HashSet::new();
            for text in scope {
                if let Some(alias) = re!(r"^\s*(?:global\s+)?using\s+([A-Za-z_]\w*)\s*=\s*(?:global::)?([\w.]+)\s*(?:<[^;>]*>)?\s*;").captures(&text) {
                    if &alias[1] == name { targets.insert(alias[2].to_string()); }
                }
            }
            if targets.len() == 1 {
                return targets.into_iter().next();
            }
            if !targets.is_empty() {
                return None;
            }
        }
        None
    }

    pub(super) fn csharp_alias_type_target(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.language != "csharp"
            || !(self.is_dotnet_type_ref(r)
                || matches!(
                    r.reference_kind.as_str(),
                    "extends" | "implements" | "type_of" | "returns"
                ))
        {
            return Ok(None);
        }
        let Some(target) = self.csharp_alias_at(&r.reference_name, r) else {
            return Ok(None);
        };
        let leaf = target.rsplit('.').next().unwrap_or(&target);
        let candidates = self.nodes_by_name(leaf)?;
        let node = candidates
            .iter()
            .find(|n| {
                n.language == "csharp"
                    && visible_type(&n.kind)
                    && n.qualified_name.replace("::", ".") == target
            })
            .cloned();
        Ok(node.map(|node| KCand {
            node,
            confidence: 0.9,
            resolved_by: "import",
        }))
    }

    pub(super) fn csharp_namespaces_at(&mut self, r: &ResolveRefIn) -> Option<Vec<String>> {
        let source = self.read_file(&r.file_path)?;
        let tree = self.parsed_tree(&source, r)?;
        let mut node = super::iteration::descendant_for_position(
            tree.root_node(),
            source.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1),
        );
        let mut names = Vec::new();
        loop {
            if matches!(
                node.kind(),
                "namespace_declaration" | "file_scoped_namespace_declaration"
            ) {
                if let Some(name) = node.child_by_field_name("name") {
                    names.push(source.text()[name.start_byte()..name.end_byte()].to_string());
                }
            }
            let Some(parent) = node.parent() else { break };
            node = parent;
        }
        names.reverse();
        if names.is_empty() {
            for declaration in super::iteration::named_children(tree.root_node()) {
                if declaration.kind() == "file_scoped_namespace_declaration" {
                    if let Some(name) = declaration.child_by_field_name("name") {
                        names.push(source.text()[name.start_byte()..name.end_byte()].to_string());
                    }
                }
            }
        }
        Some(names)
    }

    fn type_ancestor_names(&mut self, n: &KNode) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = self.language_type_ancestors_memo.get(&n.qualified_name) {
            return Ok(hit.clone());
        }
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([n.qualified_name.clone()]);
        while seen.len() < 60 {
            let Some(qn) = queue.pop_front() else { break };
            let decls: Vec<_> = self
                .nodes_by_qualified_name(&qn)?
                .iter()
                .filter(|d| d.language == n.language && visible_type(&d.kind))
                .cloned()
                .collect();
            for decl in decls {
                let Some(source) = self.read_file(&decl.file_path) else {
                    continue;
                };
                let from = (decl.start_line - 1).max(0) as usize;
                let to = ((decl.start_line + 6).max(0) as usize).min(source.len());
                let header =
                    super::call_shape::flat_head(&source[from.min(to)..to].join(" "), false);
                let clause = if n.language == "java" {
                    re!(r"\b(?:extends|implements)\b(.*)")
                        .captures(&header)
                        .map(|m| m[1].to_string())
                } else {
                    header
                        .split_once(':')
                        .map(|(_, tail)| tail.split("where").next().unwrap_or("").to_string())
                };
                for base in clause.unwrap_or_default().split(',') {
                    for name in re!(r"[A-Za-z_$][\w$]*")
                        .find_iter(base)
                        .map(|m| m.as_str())
                        .filter(|name| !matches!(*name, "extends" | "implements"))
                    {
                        if !seen.insert(name.to_string()) {
                            continue;
                        }
                        queue.extend(
                            self.nodes_by_name(name)?
                                .iter()
                                .filter(|d| d.language == n.language && visible_type(&d.kind))
                                .map(|d| d.qualified_name.clone()),
                        );
                    }
                }
            }
        }
        let seen = Rc::new(seen);
        self.language_type_ancestors_memo
            .insert(n.qualified_name.clone(), seen.clone());
        Ok(seen)
    }

    fn nested_type_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some((owner, _)) = n.qualified_name.rsplit_once("::") else {
            return Ok(true);
        };
        let owner_node = self
            .nodes_in_file(&n.file_path)?
            .iter()
            .find(|p| p.qualified_name == owner && visible_type(&p.kind))
            .cloned();
        let Some(owner_node) = owner_node else {
            return Ok(true);
        };
        let enclosing: Vec<_> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|p| visible_type(&p.kind) && p.start_line <= r.line && p.end_line >= r.line)
            .cloned()
            .collect();
        for parent in enclosing {
            if parent.qualified_name == owner
                || parent.qualified_name.starts_with(&format!("{owner}::"))
                || self
                    .type_ancestor_names(&parent)?
                    .contains(&owner_node.name)
            {
                return Ok(true);
            }
            if let Some(anonymous) = re!(r"<([A-Za-z_$][\w$]*)\$anon@\d+>").captures(&parent.name) {
                if anonymous[1] == owner_node.name {
                    return Ok(true);
                }
                for base in self
                    .nodes_by_name(&anonymous[1])?
                    .iter()
                    .filter(|n| n.language == "java" && visible_type(&n.kind))
                {
                    if self.type_ancestor_names(base)?.contains(&owner_node.name) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    fn upstream_java_type_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.kind == "method" && r.reference_kind != "calls" {
            if let Some((owner, _)) = n.qualified_name.rsplit_once("::") {
                if super::call_shape::owner(n) == Some(n.name.as_str()) {
                    let ty = self
                        .nodes_in_file(&n.file_path)?
                        .iter()
                        .find(|p| p.qualified_name == owner && visible_type(&p.kind))
                        .cloned();
                    if let Some(ty) = ty {
                        return self.upstream_java_type_visible(&ty, r);
                    }
                }
            }
        }
        if !visible_type(&n.kind) && n.kind != "enum_member" {
            return Ok(true);
        }
        let here = self.language_file_scope(&r.file_path, "java")?;
        let candidate = self.language_file_scope(&n.file_path, "java")?;
        let mut fqn = n.qualified_name.replace("::", ".");
        if !candidate.package.is_empty() && !fqn.starts_with(&format!("{}.", candidate.package)) {
            fqn = format!("{}.{}", candidate.package, fqn);
        }
        let owner_fqn = fqn.rsplit_once('.').map(|(owner, _)| owner).unwrap_or("");
        let line = self
            .read_file(&r.file_path)
            .and_then(|f| f.get((r.line - 1).max(0) as usize).cloned())
            .unwrap_or_default();
        let qualifier = Self::cached_regex(&format!(
            r"([A-Za-z_$][\w$.]*)\s*\.\s*(?:@[\w.]+(?:\([^)]*\))?\s+)*{}\b",
            regex::escape(&r.reference_name)
        ))?;
        if qualifier
            .captures_iter(&line)
            .any(|m| &m[1] == owner_fqn || owner_fqn.ends_with(&format!(".{}", &m[1])))
        {
            return Ok(true);
        }
        if n.kind == "enum_member" {
            return Ok(n.file_path == r.file_path
                || Self::cached_regex(&format!(
                    r"\bcase\b[^:;]*\b{}\b",
                    regex::escape(&r.reference_name)
                ))?
                .is_match(&line)
                || here.imports.contains(&fqn)
                || here.demand.contains(owner_fqn));
        }
        if here.imports.contains(&fqn) || here.demand.contains(owner_fqn) {
            return Ok(true);
        }
        let owner = n.qualified_name.rsplit_once("::").map(|(owner, _)| owner);
        let nested = owner.is_some_and(|owner| {
            self.nodes_in_file(&n.file_path).is_ok_and(|nodes| {
                nodes
                    .iter()
                    .any(|p| p.qualified_name == owner && visible_type(&p.kind))
            })
        });
        if !nested {
            return Ok(n.file_path == r.file_path
                || candidate.package == here.package
                || here.demand.contains(&candidate.package));
        }
        if re!(r"\.\s*new\s+").is_match(&line) {
            return Ok(true);
        }
        self.nested_type_visible(n, r)
    }

    pub(super) fn java_outside_import(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if !matches!(r.language.as_str(), "java" | "kotlin") || r.reference_kind == "imports" {
            return Ok(false);
        }
        let name = r.reference_name.split('.').next().unwrap_or("");
        let mapping = self
            .import_mappings(&r.file_path)?
            .iter()
            .find(|m| m.local_name == name)
            .cloned();
        let Some(mapping) = mapping else {
            return Ok(false);
        };
        let call = r.reference_kind == "calls" && name == r.reference_name;
        if self.nodes_in_file(&r.file_path)?.iter().any(|n| {
            n.name == name
                && if call {
                    matches!(n.kind.as_str(), "method" | "function")
                } else {
                    visible_type(&n.kind)
                }
        }) {
            return Ok(false);
        }
        // A static import from a default-package project type has no namespace row.
        let root = mapping.source.split('.').next().unwrap_or("");
        if self.nodes_by_name(root)?.iter().any(|n| {
            matches!(n.language.as_str(), "java" | "kotlin" | "scala") && visible_type(&n.kind) && {
                let qn = n.qualified_name.replace("::", ".");
                mapping.source == qn || mapping.source.starts_with(&format!("{qn}."))
            }
        }) {
            return Ok(false);
        }
        if self.language_jvm_packages.is_none() {
            let packages = {
                let mut stmt = self.conn()?.prepare("SELECT DISTINCT qualified_name FROM nodes WHERE kind = 'namespace' AND language IN ('java', 'kotlin', 'scala')").map_err(|e| Error::from_reason(e.to_string()))?;
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                rows.collect::<rusqlite::Result<HashSet<_>>>()
                    .map_err(|e| Error::from_reason(e.to_string()))?
            };
            self.language_jvm_packages = Some(packages);
        }
        Ok(!self
            .language_jvm_packages
            .as_ref()
            .unwrap()
            .iter()
            .any(|package| mapping.source.starts_with(&format!("{package}."))))
    }

    pub(super) fn dart_receiver_names_owner(
        &mut self,
        receiver: &str,
        method: &KNode,
    ) -> Res<bool> {
        if method.language == "dart" {
            if let Some((owner, _)) = method.qualified_name.rsplit_once("::") {
                let declaration = self
                    .nodes_in_file(&method.file_path)?
                    .iter()
                    .find(|n| n.qualified_name == owner && n.kind == "class")
                    .cloned();
                if let Some(declaration) = declaration {
                    if let Some(source) = self.read_file(&declaration.file_path) {
                        let from = (declaration.start_line - 1).max(0) as usize;
                        let to = ((declaration.start_line + 12).max(0) as usize).min(source.len());
                        let text = source[from.min(to)..to].join(" ");
                        if let Some(on) =
                            re!(r"\bextension\s+\w*\s*(?:<[^>]*>)?\s*on\s+([\w<>, ?]+?)\s*\{")
                                .captures(&text)
                        {
                            let ty: String =
                                on[1].chars().filter(|c| !"<>, ?".contains(*c)).collect();
                            let mut synthetic = method.clone();
                            synthetic.qualified_name = format!("{ty}::{}", method.name);
                            return Ok(super::method_call::shares_receiver_word(
                                receiver, &synthetic,
                            ));
                        }
                    }
                }
            }
        }
        Ok(super::method_call::shares_receiver_word(receiver, method))
    }

    /// Scala block declarations are visible only while their owning AST block is live.
    pub(super) fn scala_block_reachable(&mut self, n: &KNode, r: &ResolveRefIn) -> bool {
        if n.language != "scala" || !matches!(n.kind.as_str(), "field" | "variable" | "constant") {
            return true;
        }
        let Some(source) = self.read_file(&n.file_path) else {
            return true;
        };
        if let Some((cached_source, scope)) = self.scala_block_scope_memo.get(&n.id) {
            if Rc::ptr_eq(cached_source, &source) {
                return scope.is_none_or(|(start, end)| {
                    n.file_path == r.file_path && start <= r.line && r.line <= end
                });
            }
        }
        let scope = self.scala_block_scope(n, r, &source);
        self.scala_block_scope_memo
            .insert(n.id.clone(), (source, scope));
        scope.is_none_or(|(start, end)| {
            n.file_path == r.file_path && start <= r.line && r.line <= end
        })
    }

    fn scala_block_scope(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
        source: &Rc<SourceFile>,
    ) -> Option<(i64, i64)> {
        let mut site = r.clone().at(n);
        site.column = n.start_column;
        site.language = n.language.clone();
        let tree = self.parsed_tree(source, &site)?;
        let mut node = super::iteration::descendant_for_position(
            tree.root_node(),
            source.text(),
            (
                (n.start_line - 1).max(0) as usize,
                n.start_column.max(0) as usize + 1,
            ),
        );
        while let Some(parent) = node.parent() {
            if parent.kind() == "block"
                || matches!(parent.kind(), "function_definition" | "lambda_expression")
            {
                let start = parent.start_position().row as i64 + 1;
                let end = parent.end_position().row as i64 + 1;
                return Some((start, end));
            }
            if matches!(
                parent.kind(),
                "class_definition" | "object_definition" | "trait_definition"
            ) {
                return None;
            }
            node = parent;
        }
        None
    }
}
