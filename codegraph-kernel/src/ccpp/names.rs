//! C/C++ names: declarator unwrapping, qualified method names, macro-defined names and receiver types.

use super::*;

impl<'t> Walker<'t> {
    /// extractNameRaw for the c/cpp extractor configs (nameField 'declarator';
    /// cpp resolveName = extractCppQualifiedMethodName).
    pub(super) fn extract_name_raw(&self, node: Node) -> String {
        if self.variant == Variant::Cpp {
            if let Some(hook) = self.extract_cpp_qualified_method_name(node) {
                return hook;
            }
        }
        if let Some(name_node) = node.child_by_field_name("declarator") {
            let mut resolved = name_node;
            // Unwrap pointer/reference declarators (`int* f()`, `T& f()`).
            while matches!(resolved.kind(), "pointer_declarator" | "reference_declarator") {
                let inner = resolved
                    .child_by_field_name("declarator")
                    .or_else(|| resolved.named_child(0));
                match inner {
                    Some(i) => resolved = i,
                    None => break,
                }
            }
            // C++ conversion operator: `operator <type>`.
            if resolved.kind() == "operator_cast" {
                return match resolved.named_child(0) {
                    Some(t) => format!("operator {}", self.text(t).trim()),
                    None => self.text(resolved).to_string(),
                };
            }
            if resolved.kind() == "function_declarator" || resolved.kind() == "declarator" {
                let inner = resolved
                    .child_by_field_name("declarator")
                    .or_else(|| resolved.named_child(0));
                return match inner {
                    Some(i) => self.text(i).to_string(),
                    None => self.text(resolved).to_string(),
                };
            }
            return self.text(resolved).to_string();
        }
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                if matches!(c.kind(), "identifier" | "type_identifier" | "simple_identifier" | "constant") {
                    return self.text(c).to_string();
                }
            }
        }
        "<anonymous>".to_string()
    }

    /// extractCppQualifiedMethodName (languages/c-cpp.ts:75).
    pub(super) fn extract_cpp_qualified_method_name(&self, node: Node) -> Option<String> {
        if let Some(n) = self.recover_cpp_macro_defined_name(node) {
            return Some(n);
        }
        let declarator = node.child_by_field_name("declarator")?;
        let qid = find_declarator_qualified_id(declarator)?;
        let text = self.text(qid).trim();
        let parts: Vec<&str> = text.split("::").filter(|p| !p.is_empty()).collect();
        parts.last().map(|s| s.to_string())
    }

    /// recoverCppMacroDefinedName (languages/c-cpp.ts:49).
    pub(super) fn recover_cpp_macro_defined_name(&self, node: Node) -> Option<String> {
        if node.kind() != "function_definition" {
            return None;
        }
        let declarator = node.child_by_field_name("declarator")?;
        if declarator.kind() != "function_declarator" {
            return None;
        }
        let inner = declarator.child_by_field_name("declarator")?;
        if inner.kind() != "identifier" {
            return None;
        }
        let macro_name = self.text(inner);
        if !macro_shaped_re().is_match(macro_name) {
            return None;
        }
        let params = declarator.child_by_field_name("parameters")?;
        if params.named_child_count() < 2 {
            return None;
        }
        let lone_ident_text = |p: Node| -> Option<&'t str> {
            if p.kind() == "parameter_declaration"
                && p.named_child_count() == 1
                && p.named_child(0).map(|c| c.kind() == "type_identifier").unwrap_or(false)
            {
                Some(self.text(p.named_child(0).unwrap()))
            } else {
                None
            }
        };
        let name = params.named_child(0).and_then(lone_ident_text)?;
        if !has_lower_re().is_match(name) {
            return None;
        }
        for i in 1..params.named_child_count() {
            if let Some(p) = params.named_child(i) {
                if lone_ident_text(p).is_some() {
                    return None;
                }
            }
        }
        Some(name.to_string())
    }

    /// extractCppReceiverType (languages/c-cpp.ts:86).
    pub(super) fn receiver_type_of(&self, node: Node) -> Option<String> {
        let declarator = node.child_by_field_name("declarator")?;
        let qid = find_declarator_qualified_id(declarator)?;
        let text = self.text(qid).trim();
        let parts: Vec<&str> = text.split("::").filter(|p| !p.is_empty()).collect();
        if parts.len() <= 1 {
            return None;
        }
        let receiver = strip_cpp_template_args(&parts[..parts.len() - 1].join("::"));
        if receiver.is_empty() {
            None
        } else {
            Some(receiver)
        }
    }

    /// extractCppReturnType: the `type` field, normalized.
    pub(super) fn return_type_of(&self, node: Node) -> Option<String> {
        let type_node = node.child_by_field_name("type")?;
        normalize_cpp_return_type(self.text(type_node))
    }

    /// cExtractor.isConst: any named `type_qualifier` child reading "const".
    pub(super) fn is_const_declaration(&self, node: Node) -> bool {
        (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .any(|c| c.kind() == "type_qualifier" && self.text(c) == "const")
    }

    /// `#1727`: C++ pure-virtual method declaration (`virtual int read(int key) = 0;`).
    /// tree-sitter-cpp shapes these as `field_declaration` whose declarator unwraps
    /// to a `function_declarator`, with the pure-virtual `= 0` as a DIRECT
    /// `number_literal` "0" child (default-arg `= 0` lives inside
    /// `parameter_declaration` and must not match).
    pub(super) fn is_cpp_pure_virtual_method_decl(&self, node: Node<'_>) -> bool {
        if node.kind() != "field_declaration" {
            return false;
        }
        let Some(mut declarator) = node.child_by_field_name("declarator") else {
            return false;
        };
        while matches!(declarator.kind(), "pointer_declarator" | "reference_declarator") {
            let inner = declarator
                .child_by_field_name("declarator")
                .or_else(|| declarator.named_child(0));
            let Some(inner) = inner else {
                return false;
            };
            declarator = inner;
        }
        if declarator.kind() != "function_declarator" {
            return false;
        }
        (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .any(|c| c.kind() == "number_literal" && self.text(c) == "0")
    }

    /// cppExtractor.isMisparsedFunction (languages/c-cpp.ts:811). cpp only.
    pub(super) fn is_misparsed_function(&self, name: &str, node: Node) -> bool {
        if self.variant != Variant::Cpp {
            return false;
        }
        if name.starts_with("namespace") {
            return true;
        }
        if matches!(name, "switch" | "if" | "for" | "while" | "do" | "case" | "return") {
            return true;
        }
        is_macro_misparsed_type_decl(node)
    }

    /// composeReceiverQualifiedName (tree-sitter.ts:1424).
    pub(super) fn compose_receiver_qualified_name(&self, receiver_type: &str, name: &str) -> String {
        let base = format!("{receiver_type}::{name}");
        if self.namespace_prefix.is_empty() {
            return base;
        }
        let receiver_head = receiver_type.split("::").next().unwrap_or("");
        let anchor = self.namespace_prefix.iter().position(|p| p == receiver_head);
        let prefix: &[String] = match anchor {
            Some(i) => &self.namespace_prefix[..i],
            None => &self.namespace_prefix[..],
        };
        if prefix.is_empty() {
            base
        } else {
            format!("{}::{}", prefix.join("::"), base)
        }
    }
}
