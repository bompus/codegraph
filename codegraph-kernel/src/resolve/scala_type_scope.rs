//! Scala live blocks and imported implicit owners.
use super::*;

#[derive(Default)]
pub(crate) struct ScalaImports {
    owners: HashSet<String>,
    members: HashSet<String>,
    values: HashSet<String>,
}
impl KernelResolver {
    pub(super) fn scala_imported_member(
        &mut self,
        owner: &str,
        name: &str,
        file: &str,
    ) -> Res<bool> {
        let imports = self.scala_imports(file)?;
        if imports.owners.contains(owner)
            || imports.members.contains(&format!("{owner}.{name}"))
            || !imports.values.is_empty()
        {
            return Ok(true);
        }
        if let Some(seen) = self.scala_imported_ancestors_memo.get(file) {
            return Ok(seen.contains(owner));
        }
        if self.scala_package_object_supers.is_none() {
            let files: Vec<_> = self
                .table()?
                .files
                .iter()
                .filter(|f| f.ends_with(".scala"))
                .cloned()
                .collect();
            let mut supers: HashMap<String, Vec<String>> = HashMap::new();
            for path in files {
                let Some(source) = self.read_file(&path) else {
                    continue;
                };
                for m in re!(r"\bpackage\s+object\s+([\w$]+)\s+extends\s+([^\{\n]+)")
                    .captures_iter(&super::awaited::strip_ts_comments(source.text()))
                {
                    supers
                        .entry(m[1].to_string())
                        .or_default()
                        .extend(scala_supers(&m[2]));
                }
            }
            self.scala_package_object_supers = Some(supers);
        }
        let mut queue: VecDeque<_> = imports.owners.iter().cloned().collect();
        if let Some(source) = self.read_file(file) {
            queue.extend(
                re!(r"(?m)^\s*package\s+([\w.]+)\s*$")
                    .captures_iter(source.text())
                    .flat_map(|m| m[1].split('.').map(str::to_string).collect::<Vec<_>>()),
            );
        }
        let mut visited = HashSet::new();
        let mut seen = HashSet::new();
        while visited.len() < 120 {
            let Some(name) = queue.pop_front() else { break };
            if !visited.insert(name.clone()) {
                continue;
            }
            let mut bases = self
                .scala_package_object_supers
                .as_ref()
                .unwrap()
                .get(&name)
                .cloned()
                .unwrap_or_default();
            for decl in self
                .nodes_by_name(&name)?
                .iter()
                .filter(|n| n.language == "scala" && super::call_shape::type_kind(&n.kind))
            {
                let Some(source) = self.read_file(&decl.file_path) else {
                    continue;
                };
                let from = (decl.start_line - 1).max(0) as usize;
                let to = ((decl.start_line + 12).max(0) as usize).min(source.len());
                let head = super::call_shape::flat_head(&source[from.min(to)..to].join(" "), true);
                if let Some((_, clause)) = head.split_once("extends") {
                    bases.extend(scala_supers(clause));
                }
            }
            for base in bases {
                if seen.insert(base.clone()) {
                    queue.push_back(base);
                }
            }
        }
        let found = seen.contains(owner);
        self.scala_imported_ancestors_memo
            .insert(file.to_string(), seen);
        Ok(found)
    }

    fn scala_imports(&mut self, file: &str) -> Res<Rc<ScalaImports>> {
        if let Some(hit) = self.scala_imports_memo.get(file) {
            return Ok(hit.clone());
        }
        let source = self
            .read_file(file)
            .map(|f| super::awaited::strip_ts_comments(f.text()))
            .unwrap_or_default();
        let mut imports = ScalaImports::default();
        for m in re!(r"(?m)^\s*import\s+([\w.$]+?)\.(?:([_*])|\{([^}]*)\}|([\w$]+))\s*$")
            .captures_iter(&source)
        {
            let owner = m[1].rsplit('.').next().unwrap_or("");
            let root = m[1].split('.').next().unwrap_or("");
            if root.starts_with(|c: char| c.is_ascii_lowercase())
                && (root == &m[1]
                    || Self::cached_regex(&format!(
                        r"\b(?:val|var|lazy\s+val)\s+{}\b|[(,]\s*{}\s*:",
                        regex::escape(root),
                        regex::escape(root)
                    ))?
                    .is_match(&source))
            {
                imports.values.insert(owner.to_string());
            }
            if m.get(2).is_some() {
                imports.owners.insert(owner.to_string());
            } else {
                for part in m
                    .get(3)
                    .or_else(|| m.get(4))
                    .map(|p| p.as_str())
                    .unwrap_or("")
                    .split(',')
                {
                    let name = part.trim().split("=>").next().unwrap_or("").trim();
                    if matches!(name, "_" | "*") {
                        imports.owners.insert(owner.to_string());
                    } else {
                        imports.members.insert(format!("{owner}.{name}"));
                    }
                }
            }
        }
        let imports = Rc::new(imports);
        self.scala_imports_memo
            .insert(file.to_string(), imports.clone());
        Ok(imports)
    }

    /// A binder in the innermost function, or in a still-open class-body block.
    pub(super) fn scala_local_binder(&mut self, r: &ResolveRefIn) -> Res<Option<(i64, i64)>> {
        let Some(source) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let Some(tree) = self.parsed_tree(&source, r) else {
            return Ok(None);
        };
        let mut at = super::iteration::descendant_for_position(
            tree.root_node(),
            source.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1),
        );
        let mut scopes = Vec::new();
        loop {
            if matches!(
                at.kind(),
                "function_definition" | "lambda_expression" | "block"
            ) {
                scopes.push(at);
            }
            if matches!(
                at.kind(),
                "class_definition" | "object_definition" | "trait_definition"
            ) {
                break;
            }
            let Some(parent) = at.parent() else { break };
            at = parent;
        }
        let name = regex::escape(&r.reference_name);
        let binder = Self::cached_regex(&format!(
            r"(?:[(,\[]\s*(?:implicit\s+|using\s+)?{name}\s*:)|(?:\b(?:val|var|def|lazy\s+val)\s+{name}\b)|(?:^|[^\w$.]){name}\s*(?:=>|<-)|\(\s*{name}\s*(?:,[^)]*)?\)\s*=>"
        ))?;
        for scope in scopes {
            let mut snippets = String::new();
            // Exclude sibling blocks: their binders cannot shadow this call.
            for child in super::iteration::named_children(scope) {
                if child.start_position().row as i64 + 1 > r.line {
                    break;
                }
                if child.end_position().row as i64 + 1 < r.line
                    && matches!(
                        child.kind(),
                        "block" | "function_definition" | "lambda_expression" | "call_expression"
                    )
                {
                    continue;
                }
                let end = child.end_byte().min(
                    super::iteration::descendant_for_position(
                        tree.root_node(),
                        source.text(),
                        ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1),
                    )
                    .end_byte(),
                );
                if child.start_byte() < end {
                    snippets.push_str(&source.text()[child.start_byte()..end]);
                    snippets.push('\n');
                }
            }
            if binder.is_match(&snippets) {
                return Ok(Some((
                    scope.start_position().row as i64 + 1,
                    scope.end_position().row as i64 + 1,
                )));
            }
        }
        Ok(None)
    }
}
fn scala_supers(clause: &str) -> Vec<String> {
    let flat = re!(r"\[[^\]]*\]|\([^)]*\)").replace_all(clause, "");
    flat.split("with")
        .filter_map(|s| {
            re!(r"([A-Za-z_$][\w$]*)\s*$")
                .captures(s.trim())
                .map(|m| m[1].to_string())
        })
        .collect()
}
