//! C# type references: declared types, primary-constructor parameters and generic positions.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    pub(super) fn extract_csharp_type_refs(&mut self, node: Node<'t>, from_row: u32) {
        // Property `type` / method `returns` (a node carries only one).
        let direct = node
            .child_by_field_name("type")
            .or_else(|| node.child_by_field_name("returns"));
        if let Some(t) = direct {
            self.walk_type_position(t, from_row);
        }
        // Field declarations: the variable_declaration wrapper's `type` field.
        let var_decl = named_kids(node)
            .find(|c| c.kind() == "variable_declaration");
        if let Some(vd) = var_decl {
            if let Some(t) = vd.child_by_field_name("type") {
                self.walk_type_position(t, from_row);
            }
        }
        // Method/constructor parameters: ONLY each `parameter`'s `type` field.
        if let Some(params) = node.child_by_field_name("parameters") {
            for i in 0..params.named_child_count() {
                let Some(p) = params.named_child(i) else { continue };
                if p.kind() != "parameter" {
                    continue;
                }
                if let Some(t) = p.child_by_field_name("type") {
                    self.walk_type_position(t, from_row);
                }
            }
        }
    }

    /// extractCsharpPrimaryCtorParamRefs (5938) — the class/struct/record
    /// primary constructor's parameter_list (an unnamed-field child).
    pub(super) fn extract_primary_ctor_param_refs(&mut self, node: Node<'t>, owner_row: u32) {
        let param_list = named_kids(node)
            .find(|c| c.kind() == "parameter_list");
        let Some(param_list) = param_list else { return };
        for i in 0..param_list.named_child_count() {
            let Some(p) = param_list.named_child(i) else { continue };
            if p.kind() != "parameter" {
                continue;
            }
            if let Some(t) = p.child_by_field_name("type") {
                self.walk_type_position(t, owner_row);
            }
        }
    }

    /// walkCsharpTypePosition (5955).
    pub(super) fn walk_type_position(&mut self, node: Node<'t>, from_row: u32) {
        stack_guard!();
        match node.kind() {
            "predefined_type" => {}
            "identifier" => {
                let name = self.text(node);
                if !name.is_empty() && !is_builtin_type(name) {
                    self.push_ref_at(from_row, name, crate::buffers::EDGE_REFERENCES, node);
                }
            }
            "qualified_name" => {
                // Rightmost segment is the type; position = the whole node.
                let text = self.text(node);
                let last = text.rsplit('.').next().unwrap_or(text);
                if !last.is_empty() && !is_builtin_type(last) {
                    self.push_ref_at(from_row, last, crate::buffers::EDGE_REFERENCES, node);
                }
            }
            "tuple_element" => {
                // Walk the type field only — never the element NAME.
                if let Some(t) = node.child_by_field_name("type") {
                    self.walk_type_position(t, from_row);
                }
            }
            _ => {
                for c in named_kids(node) {
                    self.walk_type_position(c, from_row);
                }
            }
        }
    }
}
