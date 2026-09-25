//! The extract_* family — continuation of the Walker impl (see mod.rs for the
//! porting contract). Each function mirrors its namesake in
//! src/extraction/tree-sitter.ts; TS-file line references are as of the R2
//! port. Bug-for-bug fidelity is deliberate — fix the TS side first.

use crate::textutil as util;
use super::{
    body_of, is_react_hoc,
    is_vue_collection_name, Extra, Scope, Walker,
};
use crate::buffers::edge_kind_index;
use tree_sitter::Node;

impl<'t> Walker<'t> {
    // --- extractFunction --------------------------------------------------------

    pub(super) fn extract_function(&mut self, node: Node<'t>, name_override: Option<String>) {
        stack_guard!();
        let mut name = name_override
            .clone()
            .unwrap_or_else(|| self.extract_name(node));

        // Arrow/function-expression values: resolve the name from the parent
        // variable_declarator (`export const useAuth = () => {}`), or from a
        // CommonJS export assignment (`exports.getItems = async () => {}`,
        // #1675). Mirrors TreeSitterExtractor.extractFunction.
        let mut common_js_export = false;
        if name_override.is_none()
            && name == "<anonymous>"
            && matches!(node.kind(), "arrow_function" | "function_expression" | "generator_function")
        {
            if let Some(parent) = node.parent() {
                if parent.kind() == "variable_declarator" {
                    if let Some(var_name) = parent.child_by_field_name("name") {
                        name = self.text(var_name).to_string();
                    }
                } else if parent.kind() == "assignment_expression" {
                    if let Some(export_name) = self.common_js_export_name(parent, node) {
                        name = export_name;
                        common_js_export = true;
                    }
                }
            }
        }
        if name == "<anonymous>" {
            // Still walk the body: module wrappers hold named inner functions
            // and calls that would otherwise be lost (#528).
            if let Some(body) = body_of(node) {
                self.visit_for_calls_and_structure(body);
            }
            return;
        }

        let extra = Extra {
            docstring: crate::docstring::preceding_docstring(node, self.src),
            signature: self.signature_of(node),
            visibility: self.visibility_of(node),
            is_exported: Some(common_js_export || self.is_exported(node)),
            is_async: Some(self.is_async(node)),
            is_static: self.is_static(node),
            ..Extra::default()
        };
        let Some(row) = self.create_node("function", &name, node, extra) else {
            return;
        };

        self.extract_type_annotations(node, row);
        self.extract_decorators_for(node, row);

        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = body_of(node) {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    /// The property a CommonJS export assignment binds a function to —
    /// `exports.NAME = <node>` / `module.exports.NAME = <node>` — or None for
    /// any other assignment. The node must be the assignment's whole
    /// right-hand side. Mirrors TreeSitterExtractor.commonJsExportName.
    fn common_js_export_name(&self, assignment: Node<'t>, value: Node<'t>) -> Option<String> {
        let right = assignment.child_by_field_name("right")?;
        if right.start_byte() != value.start_byte() || right.end_byte() != value.end_byte() {
            return None;
        }
        let left = assignment.child_by_field_name("left")?;
        if left.kind() != "member_expression" {
            return None;
        }
        let object = left.child_by_field_name("object")?;
        let property = left.child_by_field_name("property")?;
        if property.kind() != "property_identifier" {
            return None;
        }
        if !matches!(self.text(object), "exports" | "module.exports") {
            return None;
        }
        Some(self.text(property).to_string())
    }

    // --- reactComponentHoc / extractReactComponentNode (#841) --------------------

    /// Some(inner) when the initializer is a recognized component wrapper —
    /// inner is the inline render function, or None for `styled.x`/`memo(Ref)`.
    /// Outer None = not a component wrapper.
    fn react_component_hoc(&self, value: Node<'t>) -> Option<Option<Node<'t>>> {
        if value.kind() != "call_expression" {
            return None;
        }
        let callee = value.child_by_field_name("function")?;
        let callee_text = self.text(callee);
        if util::styled_callee().is_match(callee_text) {
            return Some(None);
        }
        if !is_react_hoc(callee_text) {
            return None;
        }
        let mut inner: Option<Node> = None;
        if let Some(args) = value.child_by_field_name("arguments") {
            for i in 0..args.named_child_count() {
                if let Some(a) = args.named_child(i) {
                    if matches!(a.kind(), "arrow_function" | "function_expression") {
                        inner = Some(a);
                        break;
                    }
                }
            }
        }
        Some(inner)
    }

    fn extract_react_component_node(
        &mut self,
        name: &str,
        declarator: Node<'t>,
        inner_fn: Option<Node<'t>>,
        extra: Extra,
    ) {
        let Some(row) = self.create_node("component", name, declarator, extra) else {
            return;
        };
        let Some(inner) = inner_fn else { return };
        self.stack.push(Scope { row, kind: "component", name: name.to_string() });
        if let Some(body) = body_of(inner) {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    // --- extractClass ------------------------------------------------------------

    pub(super) fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        let resolved_body = body_of(node); // skipBodilessClass unset for TS/JS
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: crate::docstring::preceding_docstring(node, self.src),
            visibility: self.visibility_of(node),
            is_exported: Some(self.is_exported(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("class", &name, node, extra) else {
            return;
        };

        self.extract_inheritance(node, row);
        self.extract_decorators_for(node, row);

        self.stack.push(Scope { row, kind: "class", name });
        let body = resolved_body.unwrap_or(node);
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    // --- extractMethod -------------------------------------------------------------

    pub(super) fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        if !self.inside_class_like() {
            // Object-literal methods are ephemeral: walk the body only.
            if let Some(parent) = node.parent() {
                if matches!(parent.kind(), "object" | "object_expression") {
                    if let Some(body) = body_of(node) {
                        self.visit_for_calls_and_structure(body);
                    }
                    return;
                }
            }
            self.extract_function(node, None);
            return;
        }

        let name = self.extract_name(node);
        let extra = Extra {
            docstring: crate::docstring::preceding_docstring(node, self.src),
            signature: self.signature_of(node),
            visibility: self.visibility_of(node),
            is_async: Some(self.is_async(node)),
            is_static: self.is_static(node),
            ..Extra::default() // methods carry no isExported (mirrors extractMethod)
        };
        let Some(row) = self.create_node("method", &name, node, extra) else {
            return;
        };

        self.extract_type_annotations(node, row);
        self.extract_decorators_for(node, row);

        self.stack.push(Scope { row, kind: "method", name });
        if let Some(body) = body_of(node) {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    // --- extractInterface / extractEnum / members -----------------------------------

    pub(super) fn extract_interface(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: crate::docstring::preceding_docstring(node, self.src),
            is_exported: Some(self.is_exported(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("interface", &name, node, extra) else {
            return;
        };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "interface", name });
        let body = body_of(node).unwrap_or(node);
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    pub(super) fn extract_enum(&mut self, node: Node<'t>) {
        stack_guard!();
        let Some(body) = body_of(node) else { return };
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: crate::docstring::preceding_docstring(node, self.src),
            visibility: self.visibility_of(node),
            is_exported: Some(self.is_exported(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("enum", &name, node, extra) else {
            return;
        };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "enum", name });
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else { continue };
            if matches!(child.kind(), "property_identifier" | "enum_assignment") {
                self.extract_enum_members(child);
            } else {
                self.visit_node(child);
            }
        }
        self.stack.pop();
    }

    fn extract_enum_members(&mut self, node: Node<'t>) {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.text(name_node).to_string();
            self.create_node("enum_member", &name, node, Extra::default());
            return;
        }
        let mut found = false;
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                if matches!(child.kind(), "simple_identifier" | "identifier" | "property_identifier") {
                    let name = self.text(child).to_string();
                    self.create_node("enum_member", &name, child, Extra::default());
                    found = true;
                }
            }
        }
        if !found && node.named_child_count() == 0 {
            let name = self.text(node).to_string();
            self.create_node("enum_member", &name, node, Extra::default());
        }
    }

    // --- extractProperty (#808 property-classified class fields) ---------------------

    pub(super) fn extract_property(&mut self, node: Node<'t>) -> Option<(u32, String)> {
        let docstring = crate::docstring::preceding_docstring(node, self.src);
        let visibility = self.visibility_of(node);
        let is_static = Some(self.is_static(node).unwrap_or(false)); // `?? false` — always present

        let name_node = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("property"))
            .or_else(|| {
                (0..node.named_child_count())
                    .filter_map(|i| node.named_child(i))
                    .find(|c| c.kind() == "identifier")
            })?;
        let name = self.text(name_node).to_string();

        // TS/JS field definitions carry an explicit `type` field; the generic
        // scan is for other languages (#808). A `property_signature` (an
        // interface member, #1638) carries a `type` field and no value, so it
        // reads the type field too: the generic scan's exclusion list covers
        // `identifier` but not the `property_identifier` an interface member is
        // named with, so it would stop on the name and make the signature repeat
        // it (`counts counts`) instead of naming the type. Mirrors
        // extractProperty's isTsJsField.
        let is_ts_js_field = matches!(
            node.kind(),
            "public_field_definition" | "field_definition" | "property_signature"
        );
        let type_node = if is_ts_js_field {
            node.child_by_field_name("type")
        } else {
            (0..node.named_child_count()).filter_map(|i| node.named_child(i)).find(|c| {
                !matches!(
                    c.kind(),
                    "modifier"
                        | "modifiers"
                        | "identifier"
                        | "accessor_list"
                        | "accessors"
                        | "equals_value_clause"
                )
            })
        };
        let type_text = type_node.map(|t| {
            let raw = self.text(t);
            raw.strip_prefix(':').unwrap_or(raw).trim_start().to_string()
        });
        let signature = match &type_text {
            Some(t) => format!("{t} {name}"),
            None => name.clone(),
        };

        let row = self.create_node(
            "property",
            &name,
            node,
            Extra { docstring, signature: Some(signature), visibility, is_static, ..Extra::default() },
        )?;
        self.extract_decorators_for(node, row);
        self.extract_type_annotations(node, row);
        self.markdown_refs_from_subtree(node, row);
        Some((row, name))
    }

    // --- extractVariable (TS/JS branch) ------------------------------------------------


    pub(super) fn extract_variable(&mut self, node: Node<'t>) {
        let is_const = self.is_const_decl(node);
        let kind: &'static str = if is_const { "constant" } else { "variable" };
        let docstring = crate::docstring::preceding_docstring(node, self.src);
        let is_exported = self.is_exported(node); // `?? false` — always present

        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "variable_declarator" {
                continue;
            }
            let Some(name_node) = child.child_by_field_name("name") else { continue };
            let value = child.child_by_field_name("value");

            // Destructured patterns are skipped — except RTK Query generated
            // hooks (`export const { useGetXQuery } = api`), and exported
            // bindings destructured off a factory call (`export const {
            // selectRouteData } = getRouterSelectors()`).
            if matches!(name_node.kind(), "object_pattern" | "array_pattern") {
                if name_node.kind() == "object_pattern"
                    && value.map(|v| v.kind() == "identifier").unwrap_or(false)
                {
                    self.extract_rtk_hook_bindings(name_node, is_exported);
                } else if name_node.kind() == "object_pattern"
                    && is_exported
                    && value.map(|v| v.kind() == "call_expression").unwrap_or(false)
                {
                    self.extract_factory_binding_nodes(name_node, value.unwrap(), kind, is_exported);
                }
                continue;
            }
            let name = self.text(name_node).to_string();

            // Arrow/function/generator values extract as functions, named by the declarator.
            if let Some(v) = value {
                if matches!(v.kind(), "arrow_function" | "function_expression" | "generator_function") {
                    self.extract_function(v, None);
                    continue;
                }
            }

            let init_signature = value.map(|v| util::init_signature(self.text(v)));

            // React HOC-wrapped components (#841), PascalCase-gated.
            if let Some(v) = value {
                if util::pascal_case().is_match(&name) {
                    if let Some(inner) = self.react_component_hoc(v) {
                        self.extract_react_component_node(
                            &name,
                            child,
                            inner,
                            Extra {
                                docstring: docstring.clone(),
                                signature: init_signature.clone(),
                                is_exported: Some(is_exported),
                                ..Extra::default()
                            },
                        );
                        continue;
                    }
                }
            }

            let var_row = self.create_node(
                kind,
                &name,
                child,
                Extra {
                    docstring: docstring.clone(),
                    signature: init_signature.clone(),
                    is_exported: Some(is_exported),
                    ..Extra::default()
                },
            );
            if let Some(row) = var_row {
                self.extract_variable_type_annotation(child, row);
                // The declarator's walk stops here (visit_node skips a
                // variable's children), so the initializer's string literals
                // are reached from this side, owned by the declared symbol.
                if let Some(v) = value {
                    self.markdown_refs_from_subtree(v, row);
                }
            }

            // Exported const object-of-functions / store shapes.
            let object_of_fns: Option<Node> = match value {
                Some(v) if matches!(v.kind(), "object" | "object_expression") => Some(v),
                Some(v) if v.kind() == "call_expression" => self.find_initializer_returned_object(v, 0),
                _ => None,
            };
            let has_inline_fns = object_of_fns
                .map(|o| self.object_has_inline_functions(o))
                .unwrap_or(false);
            // "Exported" includes the two-statement form `const useStore =
            // create(…)` … `export default useStore` (is_exported_later), the
            // shape most React Native stores are written in. Mirrors
            // TreeSitterExtractor.isExportedLater.
            let extract_object_methods =
                (is_exported || self.is_exported_later(&name)) && object_of_fns.is_some() && has_inline_fns;

            let rtk_endpoints = match value {
                Some(v) if v.kind() == "call_expression" => self.find_rtk_endpoints_object(v),
                _ => None,
            };
            let pinia_setup = match value {
                Some(v) if v.kind() == "call_expression" => self.find_pinia_setup_fn(v),
                _ => None,
            };
            let mut store_collections: Vec<Node> = Vec::new();
            if let Some(v) = value {
                if matches!(v.kind(), "call_expression" | "new_expression") {
                    store_collections.extend(self.find_vue_store_collection_objects(v));
                }
            }
            if let Some(obj) = object_of_fns {
                if !extract_object_methods
                    && is_vue_collection_name(&name)
                    && self.looks_like_vue_store_file()
                {
                    store_collections.push(obj);
                }
            }

            // Walk the initializer for calls, ATTRIBUTED to the declared symbol
            // (#693) — except the object/store shapes whose members are
            // extracted method-by-method below (walking those too would
            // double-count each member arrow's calls). Before this the walk ran
            // with only the FILE on the stack (`const cfg = load()` recorded the
            // file as load's caller) and object literals were skipped outright.
            let members_extracted_separately = extract_object_methods
                || rtk_endpoints.is_some()
                || pinia_setup.is_some()
                || !store_collections.is_empty();
            if let Some(v) = value {
                if !members_extracted_separately {
                    match var_row {
                        Some(row) => {
                            self.stack.push(Scope { row, kind, name: name.clone() });
                            self.visit_for_calls_and_structure(v);
                            self.stack.pop();
                        }
                        None => self.visit_for_calls_and_structure(v),
                    }
                }
            }

            if extract_object_methods {
                if let Some(obj) = object_of_fns {
                    self.extract_object_literal_functions(obj);
                }
            }
            if let Some(rtk) = rtk_endpoints {
                self.extract_rtk_endpoints(rtk);
            }
            if let Some(setup) = pinia_setup {
                self.extract_pinia_setup_body(setup);
            }
            for coll in store_collections {
                self.extract_object_literal_functions(coll);
            }
        }
    }

    // --- object-literal / store helpers -------------------------------------------------

    // --- extractTypeAlias + members (#359, #634) -------------------------------------

    // --- extractImport + binding refs ---------------------------------------------------

    pub(super) fn extract_import(&mut self, node: Node<'t>) {
        let import_text = self.text(node).trim().to_string();
        // typescriptExtractor.extractImport: the `source` field, quotes stripped
        // globally. A missing/empty module means the hook declined — no node.
        let Some(source_field) = node.child_by_field_name("source") else { return };
        let module_name: String = self
            .text(source_field)
            .chars()
            .filter(|c| *c != '\'' && *c != '"')
            .collect();
        if module_name.is_empty() {
            return;
        }
        self.create_node(
            "import",
            &module_name,
            node,
            Extra { signature: Some(import_text), ..Extra::default() },
        );
        let parent = self.top_row();
        self.push_ref(parent, &module_name, edge_kind_index("imports").unwrap(), node);
        self.emit_import_binding_refs(node, parent);
    }

    fn emit_import_binding_refs(&mut self, node: Node<'t>, from_row: u32) {
        let clause = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "import_clause");
        let Some(clause) = clause else { return }; // side-effect import

        let imports_kind = edge_kind_index("imports").unwrap();
        let spec_text: String = node
            .child_by_field_name("source")
            .map(|s| self.text(s).chars().filter(|c| *c != '\'' && *c != '"').collect())
            .unwrap_or_default();
        let push = |w: &mut Self, name_node: Option<Node<'t>>, imported: &str| {
            let Some(n) = name_node else { return };
            let name = w.text(n).to_string();
            if name.is_empty() {
                return;
            }
            w.push_ref(from_row, &name, imports_kind, n);
            w.emit_import_binding(&name, &spec_text, imported, n);
        };

        for i in 0..clause.named_child_count() {
            let Some(child) = clause.named_child(i) else { continue };
            match child.kind() {
                "identifier" => push(self, Some(child), "default"),
                "named_imports" => {
                    for j in 0..child.named_child_count() {
                        let Some(spec) = child.named_child(j) else { continue };
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let original = spec.child_by_field_name("name").or_else(|| spec.named_child(0));
                        let imported = original.map(|o| self.text(o).to_string()).unwrap_or_default();
                        let n = spec.child_by_field_name("alias").or(original);
                        push(self, n, &imported);
                    }
                }
                "namespace_import" => {
                    let n = (0..child.named_child_count())
                        .filter_map(|k| child.named_child(k))
                        .find(|c| c.kind() == "identifier")
                        .or_else(|| child.named_child(0));
                    push(self, n, "*");
                }
                _ => {}
            }
        }
    }

    pub(super) fn emit_re_export_refs(&mut self, node: Node<'t>) {
        let from_row = self.top_row();
        let clause = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "export_clause");
        let spec_text: String = node
            .child_by_field_name("source")
            .map(|s| self.text(s).chars().filter(|c| *c != '\'' && *c != '"').collect())
            .unwrap_or_default();
        let Some(clause) = clause else {
            // `export * from './y'` / `export * as ns from './y'`: a wildcard
            // `reexport` row, exported as `*` or as the namespace name.
            if !spec_text.is_empty() {
                let ns = (0..node.named_child_count())
                    .filter_map(|i| node.named_child(i))
                    .find(|c| c.kind() == "namespace_export")
                    .and_then(|ne| (0..ne.named_child_count()).filter_map(|k| ne.named_child(k)).find(|c| c.kind() == "identifier"))
                    .map(|id| self.text(id).to_string());
                self.emit_reexport_binding("*", ns.as_deref().unwrap_or("*"), &spec_text, node);
            }
            return;
        };
        let imports_kind = edge_kind_index("imports").unwrap();
        for i in 0..clause.named_child_count() {
            let Some(spec) = clause.named_child(i) else { continue };
            if spec.kind() != "export_specifier" {
                continue;
            }
            let name_node = spec.child_by_field_name("name").or_else(|| spec.named_child(0));
            let Some(n) = name_node else { continue };
            let name = self.text(n).to_string();
            if name.is_empty() {
                continue;
            }
            // `export { default as a } from './a'` forwards a default export:
            // no `imports` ref (nothing is named `default`), but a row so the
            // barrel chase can follow it.
            if name != "default" {
                self.push_ref(from_row, &name, imports_kind, n);
            }
            let exported_as = spec.child_by_field_name("alias").map(|a| self.text(a).to_string()).unwrap_or_else(|| name.clone());
            self.emit_reexport_binding(&name, &exported_as, &spec_text, n);
        }
    }

    // --- extractCall (TS/JS generic tail) -------------------------------------------------

    // --- extractInstantiation -----------------------------------------------------------

    // --- extractDecoratorsFor --------------------------------------------------------------

    // --- extractInheritance (TS/JS clauses) ---------------------------------------------------

    // --- type annotations (#381 — TS family only) ----------------------------------------------

}

/// `.replace(/\s+/g, ' ')` for the tuple-contract signature.
pub(super) fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}
