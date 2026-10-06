//! Fields shadow inherited getters and extension getters without creating call edges.
use super::*;

impl KernelResolver {
    pub(super) fn dart_field_depth(&mut self, type_name: &str, name: &str, r: &ResolveRefIn) -> Res<Option<u32>> {
        let mut queue: VecDeque<_> = self.dart_visible_declarations(type_name, &r.file_path)?.into_iter()
            .map(|declaration| (declaration, 0u32)).collect();
        let mut seen = HashSet::new();
        while seen.len() < 40 {
            let Some((declaration, depth)) = queue.pop_front() else { break };
            if !seen.insert(declaration.id.clone()) { continue; }
            if self.dart_declares_field(&declaration, name, r) { return Ok(Some(depth)); }
            for parent in self.dart_head_of(&declaration).supers {
                queue.extend(self.dart_visible_declarations(&parent, &declaration.file_path)?.into_iter()
                    .map(|declaration| (declaration, depth + 1)));
            }
        }
        Ok(None)
    }

    fn dart_visible_declarations(&mut self, name: &str, file: &str) -> Res<Vec<Arc<KNode>>> {
        let declarations: Vec<_> = self.nodes_by_name(name)?.iter()
            .filter(|n| n.language == "dart" && matches!(n.kind.as_str(), "class" | "enum" | "interface"))
            .cloned().collect();
        let local: Vec<_> = declarations.iter().filter(|n| n.file_path == file).cloned().collect();
        if !local.is_empty() { return Ok(local); }
        let mut visible = HashSet::new();
        let mut queue = VecDeque::from([file.to_string()]);
        while visible.len() < 40 {
            let Some(library) = queue.pop_front() else { break };
            if !visible.insert(library.clone()) { continue; }
            let imports: Vec<_> = self.nodes_in_file(&library)?.iter()
                .filter(|n| n.language == "dart" && n.kind == "import").cloned().collect();
            for import in imports {
                let signature = import.signature.as_deref().unwrap_or("");
                if library != file && !signature.trim_start().starts_with("export ") { continue; }
                if re!(r"\bas\s+[A-Za-z_$]").is_match(signature) { continue; }
                if let Some(show) = re!(r"\bshow\s+([^;]+)").captures(signature) {
                    if !show[1].split(',').any(|item| item.trim() == name) { continue; }
                }
                if re!(r"\bhide\s+([^;]+)").captures(signature)
                    .is_some_and(|hide| hide[1].split(',').any(|item| item.trim() == name)) { continue; }
                let target = if let Some(package) = import.name.strip_prefix("package:") {
                    let Some((package, path)) = package.split_once('/') else { continue };
                    let mut directory = pos_dirname(&library).to_string();
                    let mut target = None;
                    loop {
                        let manifest = if directory.is_empty() { "pubspec.yaml".to_string() }
                            else { format!("{directory}/pubspec.yaml") };
                        if let Some(source) = self.read_file(&manifest) {
                            if re!(r#"(?m)^name:\s*['\"]?([A-Za-z0-9_]+)"#).captures(source.text())
                                .is_some_and(|found| &found[1] == package) {
                                let candidate = if directory.is_empty() { format!("lib/{path}") }
                                    else { format!("{directory}/lib/{path}") };
                                target = self.probe_extensions(&candidate, "dart");
                            }
                            break;
                        }
                        if directory.is_empty() { break; }
                        directory = pos_dirname(&directory).to_string();
                    }
                    target
                } else if import.name.contains(':') { None }
                else {
                    let directory = pos_resolve(&self.project_root, pos_dirname(&library));
                    self.resolve_relative_import(&import.name, &directory, "dart")?
                };
                if let Some(target) = target {
                    queue.push_back(target);
                }
            }
        }
        Ok(declarations.into_iter().filter(|n| visible.contains(&n.file_path)).collect())
    }

    fn dart_declares_field(&mut self, declaration: &KNode, name: &str, r: &ResolveRefIn) -> bool {
        let Some(source) = self.read_file(&declaration.file_path) else { return false };
        if !self.dart_fields_memo.get(&declaration.id).is_some_and(|(old, _)| Rc::ptr_eq(old, &source)) {
            let site = ResolveRefIn { column: declaration.start_column, ..r.clone().at(declaration) };
            let Some(tree) = self.parsed_tree(&source, &site) else { return false };
            let point = ((declaration.start_line - 1).max(0) as usize, declaration.start_column.max(0) as usize);
            let mut node = super::iteration::descendant_for_position(tree.root_node(), source.text(), point);
            while !matches!(node.kind(), "class_definition" | "enum_declaration") {
                let Some(parent) = node.parent() else { return false };
                node = parent;
            }
            let mut fields = HashSet::new();
            if let Some(body) = super::iteration::named_children(node).into_iter()
                .find(|n| matches!(n.kind(), "class_body" | "enum_body")) {
                for member in super::iteration::named_children(body).into_iter().filter(|n| n.kind() == "declaration") {
                    for list in super::iteration::named_children(member).into_iter()
                        .filter(|n| matches!(n.kind(), "initialized_identifier_list" | "static_final_declaration_list")) {
                        for entry in super::iteration::named_children(list) {
                            if let Some(identifier) = super::iteration::named_children(entry).into_iter().find(|n| n.kind() == "identifier") {
                                fields.insert(source.text()[identifier.start_byte()..identifier.end_byte()].to_string());
                            }
                        }
                    }
                }
            }
            self.dart_fields_memo.insert(declaration.id.clone(), (source, fields));
        }
        self.dart_fields_memo.get(&declaration.id).is_some_and(|(_, fields)| fields.contains(name))
    }
}
