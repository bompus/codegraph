//! C/C++ names: declarator unwrapping, qualified method names, macro-defined names and receiver types.

use crate::walker::named_kids;
use super::*;

impl<'t> Walker<'t> {
    /// extractNameRaw for the c/cpp extractor configs (nameField 'declarator';
    /// cpp resolveName = extractCppQualifiedMethodName).
    pub(super) fn extract_name_raw(&self, node: Node) -> String {
        if let Some(name) = self.recover_single_arg_macro_defined_name(node) {
            return name;
        }
        if let Some((type_name, argument)) = self.paren_declarator_shape(node) {
            return if self.attribute_declaration_before(node).is_some() {
                type_name.to_string()
            } else {
                format!("{type_name}({argument})")
            };
        }
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
        for c in named_kids(node) {
            if matches!(c.kind(), "identifier" | "type_identifier" | "simple_identifier" | "constant") {
                return self.text(c).to_string();
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

    /// recoverSingleArgMacroDefinedName: only a local macro definition proves
    /// that its argument is the function name, in either parser shape (#1373).
    pub(super) fn recover_single_arg_macro_defined_name(&self, node: Node) -> Option<String> {
        if node.kind() != "function_definition" {
            return None;
        }
        let declarator = node.child_by_field_name("declarator")?;
        let (macro_node, argument) = if declarator.kind() == "parenthesized_declarator"
            && declarator.named_child_count() == 1
        {
            let m = node.child_by_field_name("type")?;
            let a = declarator.named_child(0)?;
            if m.kind() != "type_identifier" || a.kind() != "identifier" {
                return None;
            }
            (m, a)
        } else if declarator.kind() == "function_declarator"
            && node.child_by_field_name("type").is_none()
        {
            let m = declarator.child_by_field_name("declarator")?;
            let params = declarator.child_by_field_name("parameters")?;
            let param = params.named_child(0)?;
            if m.kind() != "identifier" || params.named_child_count() != 1
                || param.kind() != "parameter_declaration" || param.named_child_count() != 1
            {
                return None;
            }
            let a = param.named_child(0)?;
            if a.kind() != "type_identifier" {
                return None;
            }
            (m, a)
        } else {
            return None;
        };
        let macro_name = self.text(macro_node);
        let mut scope = Some(node);
        while let Some(current) = scope {
            if current.kind() == "preproc_else" || current.kind().starts_with("preproc_elif") {
                return None;
            }
            let mut previous = current.prev_named_sibling();
            while let Some(prev) = previous {
                previous = prev.prev_named_sibling();
                if prev.kind().starts_with("preproc_if") {
                    return None;
                }
                if prev.kind() == "preproc_call"
                    && prev.child_by_field_name("directive").map(|n| self.text(n)) == Some("#undef")
                    && prev.child_by_field_name("argument").map(|n| self.text(n).trim()) == Some(macro_name)
                {
                    return None;
                }
                if !matches!(prev.kind(), "preproc_function_def" | "preproc_def")
                    || prev.child_by_field_name("name").map(|n| self.text(n)) != Some(macro_name)
                {
                    continue;
                }
                let params = prev.child_by_field_name("parameters")?;
                let param = params.named_child(0)?;
                let value = prev.child_by_field_name("value")?;
                if params.named_child_count() != 1 || param.kind() != "identifier" {
                    return None;
                }
                let replacement = self.text(value).replace("\\\r\n", " ").replace("\\\n", " ");
                let captures = single_arg_macro_replacement_re().captures(replacement.trim())?;
                if replacement.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|s| s == "typedef")
                    || captures.get(1)?.as_str() != self.text(param)
                {
                    return None;
                }
                return Some(self.text(argument).to_string());
            }
            scope = current.parent();
        }
        None
    }

    /// `T(X) { … }` with no parameter list: a definition whose signature a
    /// macro supplies, parsed as type `T` and declarator `(X)`. Two sources:
    /// a macro that wraps the name (`ENCODER(hz)`, `TARGET(BINARY_OP)`,
    /// `SYSCALL_DEFINE0(sync)`), named `T(X)` like `STRINGLIB(fn)(…)` already
    /// is; and an attribute macro that took the type's place, see
    /// attribute_declaration_before.
    pub(super) fn paren_declarator_shape(&self, node: Node) -> Option<(&'t str, &'t str)> {
        if node.kind() != "function_definition" {
            return None;
        }
        let type_node = node.child_by_field_name("type")?;
        let declarator = node.child_by_field_name("declarator")?;
        if type_node.kind() != "type_identifier"
            || declarator.kind() != "parenthesized_declarator"
            || declarator.named_child_count() != 1
        {
            return None;
        }
        let argument = declarator.named_child(0)?;
        if argument.kind() != "identifier" {
            return None;
        }
        Some((self.text(type_node), self.text(argument)))
    }

    /// `bool mi_decl_noinline _mi_preloading(void)`: the attribute macro reads
    /// as a variable in a declaration with no `;`, which leaves the real name
    /// in the definition's type slot and its parameters as `(void)`. The
    /// unterminated declaration ends on the definition's line or the one
    /// before.
    pub(super) fn attribute_declaration_before<'n>(&self, node: Node<'n>) -> Option<Node<'n>> {
        let prev = node.prev_named_sibling()?;
        let adjacent = node.start_position().row - prev.end_position().row <= 1;
        (prev.kind() == "declaration" && adjacent && !self.text(prev).trim_end().ends_with(';'))
            .then_some(prev)
    }

    /// `static PyObject *Py_PRESERVE_NONE_CC _TAIL_CALL_error(TAIL_CALL_PARAMS);`
    /// is the prototype form of attribute_declaration_before: the attribute
    /// macro ends an unterminated declaration, and the name and parameters
    /// read as a file-level call. True for either half; neither is a variable
    /// or a call.
    pub(super) fn is_attribute_prototype_part(&self, node: Node) -> bool {
        let (decl, stmt) = match node.kind() {
            "declaration" => (Some(node), node.next_named_sibling()),
            "expression_statement" => (node.prev_named_sibling(), Some(node)),
            _ => return false,
        };
        let (Some(decl), Some(stmt)) = (decl, stmt) else { return false };
        decl.kind() == "declaration"
            && stmt.kind() == "expression_statement"
            && stmt.named_child(0).is_some_and(|c| c.kind() == "call_expression")
            && stmt.start_position().row.saturating_sub(decl.end_position().row) <= 1
            && !self.text(decl).trim_end().ends_with(';')
    }

    /// A condition a macro misparsed at file level: `catch (e) {` in an
    /// EM_JS body, or `if mi_unlikely(x) {` whose `if` became an ERROR.
    fn is_macro_condition(&self, node: Node) -> bool {
        let Some((type_name, _)) = self.paren_declarator_shape(node) else { return false };
        is_statement_keyword(type_name)
            || node.prev_sibling().is_some_and(|p| {
                self.text(p).rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_').next().is_some_and(is_statement_keyword)
            })
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
        if self.paren_declarator_shape(node).is_some() {
            let decl = self.attribute_declaration_before(node)?;
            return normalize_cpp_return_type(self.text(decl.child_by_field_name("type")?));
        }
        let type_node = node.child_by_field_name("type")?;
        normalize_cpp_return_type(self.text(type_node))
    }

    /// cExtractor.isConst: any named `type_qualifier` child reading "const".
    pub(super) fn is_const_declaration(&self, node: Node) -> bool {
        named_kids(node)
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
        named_kids(node)
            .any(|c| c.kind() == "number_literal" && self.text(c) == "0")
    }

    /// cppExtractor.isMisparsedFunction (languages/c-cpp.ts:811). cpp only.
    pub(super) fn is_misparsed_function(&self, name: &str, node: Node) -> bool {
        if self.is_macro_condition(node) {
            return true;
        }
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
