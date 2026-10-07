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
        let declarations = self.nodes_by_name(name)?.iter().filter(|n| n.language == "dart" && matches!(n.kind.as_str(), "class" | "enum" | "interface")).cloned().collect::<Vec<_>>();
        let mut visible = Vec::new();
        for n in declarations { if self.dart_visible(file, &n.file_path, &n.name, "") { visible.push(n); } }
        let own = visible.iter().filter(|n| self.dart_same_library(file, &n.file_path)).cloned().collect::<Vec<_>>();
        Ok(if own.is_empty() { visible } else { own })
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
