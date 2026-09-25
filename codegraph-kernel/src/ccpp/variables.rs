//! Variables and `#include`s.

use super::*;

impl<'t> Walker<'t> {
    /// extractVariable: C takes the dedicated branch (file-scope declarators,
    /// tree-sitter.ts:2795); cpp takes the TS GENERIC fallback (direct
    /// identifier children).
    pub(super) fn extract_variable(&mut self, node: Node<'t>) {
        let is_const = self.variant == Variant::C && self.is_const_declaration(node);
        let kind: &'static str = if is_const { "constant" } else { "variable" };
        let docstring = preceding_docstring(node, self.src);
        // isExported?.() ?? false — EXPLICIT false (tri-state flag set).
        let is_exported = Some(false);

        if self.variant == Variant::C {
            if has_function_ancestor(node) {
                return;
            }
            for i in 0..node.named_child_count() {
                let Some(child) = node.named_child(i) else { continue };
                if !matches!(
                    child.kind(),
                    "init_declarator" | "pointer_declarator" | "array_declarator"
                ) {
                    continue;
                }
                let Some(name_node) = c_declarator_identifier(child) else { continue };
                let name = self.text(name_node);
                if name.is_empty() {
                    continue;
                }
                let value_node = if child.kind() == "init_declarator" {
                    child.child_by_field_name("value")
                } else {
                    None
                };
                let signature = value_node.map(|v| util::init_signature(self.text(v)));
                self.create_node(
                    kind,
                    name,
                    child,
                    Extra {
                        docstring: docstring.clone(),
                        signature,
                        is_exported,
                        ..Extra::default()
                    },
                );
            }
        } else {
            // Generic fallback: direct identifier children only (`int x;`
            // extracts; `int x = 5;` nests in an init_declarator and does not).
            for i in 0..node.named_child_count() {
                let Some(child) = node.named_child(i) else { continue };
                if child.kind() != "identifier" {
                    continue;
                }
                let name = self.text(child).to_string();
                if !name.is_empty() && name != "<anonymous>" {
                    self.create_node(
                        kind,
                        &name,
                        child,
                        Extra { docstring: docstring.clone(), is_exported, ..Extra::default() },
                    );
                }
            }
        }
    }

    /// extractImport via the c/cpp extractImport hook: `#include <sys.h>` /
    /// `#include "local.h"`. A hook miss (`#include MACRO`) extracts nothing.
    pub(super) fn extract_import(&mut self, node: Node<'t>) {
        let import_text = self.text(node).trim().to_string();
        let module_name: Option<String> =
            if let Some(sys) = self.find_child_by_kind(node, "system_lib_string") {
                let t = self.text(sys);
                let t = t.strip_prefix('<').unwrap_or(t);
                let t = t.strip_suffix('>').unwrap_or(t);
                Some(t.to_string())
            } else if let Some(lit) = self.find_child_by_kind(node, "string_literal") {
                self.find_child_by_kind(lit, "string_content").map(|sc| self.text(sc).to_string())
            } else {
                None
            };
        let Some(module_name) = module_name else { return };
        self.create_node(
            "import",
            &module_name,
            node,
            Extra { signature: Some(import_text), ..Extra::default() },
        );
        if !module_name.is_empty() {
            let parent = self.top_row();
            self.push_ref_at(parent, &module_name, edge_kind_index("imports").unwrap(), node);
            let local = include_local_name(&module_name);
            self.emit_import_binding(&local, &module_name, node);
        }
    }
}
