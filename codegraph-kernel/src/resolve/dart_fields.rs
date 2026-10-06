//! Fields shadow inherited getters and extension getters without creating call edges.
use super::*;

impl KernelResolver {
    pub(super) fn dart_field_depth(&mut self, lineage: &HashMap<String, u32>, name: &str, r: &ResolveRefIn) -> Res<Option<u32>> {
        let mut depth = None;
        for (ty, rank) in lineage {
            let declarations: Vec<_> = self.nodes_by_name(ty)?.iter()
                .filter(|n| n.language == "dart" && matches!(n.kind.as_str(), "class" | "enum"))
                .cloned().collect();
            for declaration in declarations {
                if self.dart_declares_field(&declaration, name, r) {
                    depth = Some(depth.map_or(*rank, |old: u32| old.min(*rank)));
                }
            }
        }
        Ok(depth)
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
