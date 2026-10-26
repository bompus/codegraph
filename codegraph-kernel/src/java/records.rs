//! Record component synthesis (languages/java.ts synthesizeRecordComponents, #2639):
//! a `record` declares a private field and a public accessor per component.

use super::*;

impl<'t> Walker<'t> {
    /// Members already declared directly in a class (exact `classQN::name`
    /// matches), split into the method and field namespaces.
    fn declared_member_names(&self, class_row: u32) -> (HashSet<String>, HashSet<String>) {
        let class_qn = self.nodes_meta[class_row as usize].qualified_name.clone();
        let mut taken_methods: HashSet<String> = HashSet::new();
        let mut taken_fields: HashSet<String> = HashSet::new();
        for m in &self.nodes_meta {
            if m.qualified_name == format!("{class_qn}::{}", m.name) {
                match m.kind {
                    "method" | "function" => {
                        taken_methods.insert(m.name.clone());
                    }
                    "field" | "variable" | "constant" | "property" => {
                        taken_fields.insert(m.name.clone());
                    }
                    _ => {}
                }
            }
        }
        (taken_methods, taken_fields)
    }

    /// synthesizeRecordComponents (languages/java.ts): per record component a
    /// private field and a public accessor, anchored on the component's name.
    pub(super) fn synthesize_record_components(&mut self, record_node: Node<'t>, record_row: u32) {
        let Some(params) = record_node.child_by_field_name("parameters") else { return };
        let (mut taken_methods, mut taken_fields) = self.declared_member_names(record_row);
        for i in 0..params.named_child_count() {
            let Some(param) = params.named_child(i) else { continue };
            if param.kind() != "formal_parameter" {
                continue;
            }
            let (Some(name_node), Some(type_node)) =
                (param.child_by_field_name("name"), param.child_by_field_name("type"))
            else {
                continue;
            };
            let name = self.text(name_node).trim().to_string();
            let type_text = self.text(type_node).trim().to_string();
            if name.is_empty() {
                continue;
            }
            if !taken_fields.contains(&name) {
                taken_fields.insert(name.clone());
                self.create_node(
                    "field",
                    &name,
                    name_node,
                    Extra {
                        visibility: Some(2),
                        is_static: Some(false),
                        signature: Some(format!("{type_text} {name}")),
                        ..Extra::default()
                    },
                );
            }
            if !taken_methods.contains(&name) {
                taken_methods.insert(name.clone());
                let return_type = self.normalize_java_type(Some(type_node));
                self.create_node(
                    "method",
                    &name,
                    name_node,
                    Extra {
                        visibility: Some(1),
                        is_static: Some(false),
                        signature: Some(format!("{type_text} {name}()")),
                        docstring: Some("Implicit record component accessor".to_string()),
                        return_type,
                        ..Extra::default()
                    },
                );
            }
        }
    }
}
