//! PHP extraction — a faithful Rust port of `TreeSitterExtractor`'s PHP paths
//! (src/extraction/tree-sitter.ts) plus languages/php.ts.
//!
//! Same porting contract as the other walkers: behavior parity, bug-for-bug.
//! The authoritative quirk list is docs/design/php-kernel-port-checklist.md —
//! including what this file preserves on purpose: the visitNode hook consumes
//! const_declaration (constants at ANY scope, values never walked) and
//! trait-`use` (implements refs WITH filePath — the v2 ref-flag wire slot,
//! shipped with ruby) before the ladder; the FIRST file-level namespace scopes
//! the whole walk (braced namespaces scope nothing); anonymous classes on the
//! v0.24.2 grammar mint NO anon-class node (top-level methods become file-level
//! functions, in-body methods vanish) and their instantiates ref is the whole
//! anon-class text run through the suffix logic; scoped calls are DOT-joined
//! (`UserModel.query`); `$this->prop->m()` emits `this->prop.m` (the #1251
//! machinery is resolution-side); nullsafe `?->` emits nothing; literal
//! receivers are not suppressed; interface multi-extends drops all but the
//! first base; property type-hints emit no refs from field nodes. Positions in
//! UTF-16 code units. Files with parse errors are walked like any other (tree-sitter's recovery is canonical; buffers::parse_collapse_warning reports a collapsed parse).

mod imports;
mod calls;
mod bindings;
mod refs;
use crate::buffers::{
    BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, node_kind_index, Arena, BoolFlags, EmitOut, NodeRow,
    RefRow, Tables, FLAG_IS_STATIC, NONE, NONE_STR,
    REF_FLAG_FILE_PATH,
};
use crate::walker::named_kids;
use crate::walker::{Cand, Scope, ValueScope, scope_qualified_name};
use crate::textutil::{strip_generic_and_qualifier, capitalized_re};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tree_sitter::Node;



/// PHP_NON_CLASS_RETURN (languages/php.ts:37).
fn is_php_non_class_return(lc: &str) -> bool {
    matches!(
        lc,
        "array" | "string" | "int" | "integer" | "float" | "double" | "bool" | "boolean"
            | "void" | "mixed" | "never" | "null" | "false" | "true" | "object" | "callable"
            | "iterable" | "resource"
    )
}

/// PHP_PSEUDO_TYPES (tree-sitter.ts:5760).
fn is_php_pseudo_type(name: &str) -> bool {
    matches!(
        name,
        "self" | "static" | "parent" | "mixed" | "object" | "iterable" | "callable" | "void"
            | "null" | "false" | "true" | "never" | "array" | "int" | "float" | "string" | "bool"
    )
}

/// PHP_TYPE_NODES (tree-sitter.ts:310).
fn is_php_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "named_type" | "optional_type" | "nullable_type" | "union_type" | "intersection_type"
            | "disjunctive_normal_form_type" | "primitive_type"
    )
}

/// PHP_CALLABLE_HOFS (function-ref.ts:347).
fn is_php_callable_hof(name: &str) -> bool {
    matches!(
        name,
        "array_map" | "array_filter" | "array_walk" | "array_walk_recursive" | "array_reduce"
            | "usort" | "uasort" | "uksort"
            | "array_udiff" | "array_udiff_assoc" | "array_uintersect" | "array_uintersect_assoc"
            | "call_user_func" | "call_user_func_array"
            | "forward_static_call" | "forward_static_call_array"
            | "preg_replace_callback" | "preg_replace_callback_array"
            | "register_shutdown_function" | "register_tick_function"
            | "set_error_handler" | "set_exception_handler" | "spl_autoload_register"
            | "ob_start" | "iterator_apply" | "header_register_callback"
            | "is_callable"
    )
}

/// String-callable qualified shape (`/^\w+::\w+$/`, JS ASCII `\w`).
fn qualified_callable_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[0-9A-Za-z_]+::[0-9A-Za-z_]+$").unwrap())
}


#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<u8>,
    is_static: Option<bool>,
    return_type: Option<String>,
}

pub struct Walker<'t> {
    src: &'t str,
    file_path: &'t str,
    cols: util::Cols,
    arena: Arena,
    tables: Tables,
    md_ref_keys: HashSet<String>,
    stack: Vec<Scope>,
    node_ids: Vec<String>,
    defined_fn_names: HashSet<String>,
    imported_names: HashSet<String>,
    fn_ref_cands: Vec<Cand>,
    fs_values: HashMap<String, u32>,
    value_scopes: Vec<ValueScope<'t>>,
    line_count: u32,
}

pub fn extract(file_path: &str, source: &str) -> Result<EmitOut, String> {
    let t0 = std::time::Instant::now();
    let tree = crate::langs::parse("php", source)?;

    let mut w = Walker::new(source, file_path);

    let line_count = w.line_count;
    let base_name = crate::buffers::push_file_node(&mut w.arena, &mut w.tables, file_path, line_count);
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });

    // extractFilePackage: the FIRST namespace_definition among the root's
    // direct namedChildren; braced namespaces (a compound_statement /
    // declaration_list child) make NO node and scope NOTHING. The node stays
    // pushed for the whole walk — QNs become `App\Services::Name` and import
    // nodes/refs hang off it.
    let root = tree.root_node();
    let mut pkg_pushed = false;
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else { continue };
        if child.kind() != "namespace_definition" {
            continue;
        }
        let ns_name = named_kids(child)
            .find(|c| c.kind() == "namespace_name");
        let has_body = named_kids(child)
            .any(|c| matches!(c.kind(), "compound_statement" | "declaration_list"));
        if let Some(ns_name) = ns_name {
            if !has_body {
                let pkg = w.text(ns_name).to_string();
                if !pkg.is_empty() {
                    if let Some(row) = w.create_node("namespace", &pkg, child, Extra::default()) {
                        w.stack.push(Scope { row, kind: "namespace", name: pkg });
                        pkg_pushed = true;
                    }
                }
            }
        }
        break;
    }

    w.visit_node(root);
    w.flush_fn_ref_candidates();
    w.flush_value_refs();
    if pkg_pushed {
        w.stack.pop();
    }
    w.stack.pop();

    Ok(crate::buffers::finish(w.arena, w.tables, tree.root_node().has_error(), file_path, t0))
}

impl<'t> Walker<'t> {
    fn new(source: &'t str, file_path: &'t str) -> Walker<'t> {
        Walker {
            src: source,
            file_path,
            cols: util::Cols::new(source),
            arena: Arena::default(),
            tables: Tables::default(),
            md_ref_keys: HashSet::new(),
            stack: Vec::new(),
            node_ids: Vec::new(),
            defined_fn_names: HashSet::new(),
            imported_names: HashSet::new(),
            fn_ref_cands: Vec::new(),
            fs_values: HashMap::new(),
            value_scopes: Vec::new(),
            line_count: source.bytes().filter(|b| *b == b'\n').count() as u32 + 1,
        }
    }
    markdown_refs_impl!();

    walker_pos_impl!();
    inside_class_like_impl!("class" | "struct" | "interface" | "trait" | "enum" | "module");

    push_ref_impl!();


    // --- createNode ------------------------------------------------------------

    fn create_node(&mut self, kind: &'static str, name: &str, node: Node<'t>, extra: Extra) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let start_line = self.line_of(node);
        let id = ids::node_id(self.file_path, kind, name, start_line);
        let end_line = node.end_position().row as u32 + 1; // no resolveBody for php

        let qualified = scope_qualified_name(&self.stack, name);

        let mut flags = BoolFlags::default();
        if let Some(v) = extra.is_static {
            flags.set(FLAG_IS_STATIC, v);
        }
        let name_ref = self.arena.put(name);
        let qn_ref = self.arena.put(&qualified);
        let id_ref = self.arena.put(&id);
        let doc_ref = self.arena.put_opt(extra.docstring.as_deref());
        let sig_ref = self.arena.put_opt(extra.signature.as_deref());
        let ret_ref = self.arena.put_opt(extra.return_type.as_deref());
        let row = self.tables.push_node(&NodeRow {
            kind: node_kind_index(kind).unwrap(),
            visibility: extra.visibility.unwrap_or(0),
            flags,
            start_line,
            end_line,
            start_column: self.col_of(node),
            end_column: self.end_col_of(node),
            name: name_ref,
            qualified_name: qn_ref,
            id: id_ref,
            docstring: doc_ref,
            signature: sig_ref,
            decorators: NONE_STR, // php attributes never emit decorates refs
            type_parameters: NONE_STR,
            return_type: ret_ref,
            extra_json: NONE_STR,
        });
        self.node_ids.push(id);

        let parent_row = self.top_row();
        self.tables.push_contains(parent_row, row);

        if kind == "function" || kind == "method" {
            self.defined_fn_names.insert(name.to_string());
        }
        // captureValueRefScope — with a namespace pushed, top-level constants
        // have a `namespace` parent (NOT in the accepted set) and are dropped
        // as targets; class/enum consts qualify, interface/trait ones don't.
        let target_kind_ok = kind == "constant" || kind == "variable";
        if target_kind_ok
            && util::utf16_len(name) >= 3
            && util::has_upper_or_underscore().is_match(name)
        {
            let parent_ok = self
                .stack
                .last()
                .map(|s| matches!(s.kind, "file" | "class" | "module" | "struct" | "enum"))
                .unwrap_or(false);
            if parent_ok {
                self.fs_values.insert(name.to_string(), row);
            }
        }
        if matches!(kind, "function" | "method" | "constant" | "variable") {
            self.value_scopes.push(ValueScope { row, node, name: name.to_string() });
        }
        self.emit_decl_binding(kind, name, row, node, extra.visibility);
        if kind == "function" || kind == "method" {
            self.emit_param_bindings(node);
        }
        Some(row)
    }

    extract_name_impl!();

    // --- hooks (languages/php.ts) ------------------------------------------------

    /// getVisibility: any `visibility_modifier` child with one of the three
    /// texts; none → public (the php default).
    fn visibility_of(&self, node: Node) -> u8 {
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            if child.kind() == "visibility_modifier" {
                match self.text(child) {
                    "public" => return 1,
                    "private" => return 2,
                    "protected" => return 3,
                    _ => {}
                }
            }
        }
        1 // PHP defaults to public
    }

    fn is_static(&self, node: Node) -> bool {
        (0..node.child_count())
            .filter_map(|i| node.child(i))
            .any(|c| c.kind() == "static_modifier")
    }

    /// extractPhpReturnType — `self`/`static` collapse to the `'self'` marker
    /// (#608 chained-call fuel); primitives/unions → None.
    fn return_type_of(&self, node: Node) -> Option<String> {
        let mut rt = node.child_by_field_name("return_type")?;
        if rt.kind() == "optional_type" {
            rt = rt.named_child(0).unwrap_or(rt);
        }
        if rt.kind() == "primitive_type" {
            return None;
        }
        let name_node = if rt.kind() == "named_type" { rt.named_child(0).unwrap_or(rt) } else { rt };
        let text = self.text(name_node).trim();
        let text = text.strip_prefix('\\').unwrap_or(text);
        if text.is_empty() {
            return None;
        }
        let last = text.rsplit('\\').next().unwrap_or(text);
        let lc = last.to_lowercase();
        if matches!(lc.as_str(), "self" | "static" | "this" | "$this") {
            return Some("self".to_string());
        }
        if is_php_non_class_return(&lc) {
            return None;
        }
        if !crate::textutil::ascii_ident_re().is_match(last) {
            return None; // unions/intersections/complex
        }
        Some(last.to_string())
    }

    // --- the visitNode hook (php.ts:108) ------------------------------------------

    fn try_visit_hook(&mut self, node: Node<'t>) -> bool {
        match node.kind() {
            // Class/interface/trait/enum/top-level constants: one `constant`
            // node per const_element, NO extras, values never walked.
            "const_declaration" => {
                let elements: Vec<Node> = named_kids(node)
                    .filter(|c| c.kind() == "const_element")
                    .collect();
                for elem in elements {
                    let name_node = named_kids(elem)
                        .find(|c| c.kind() == "name");
                    let Some(name_node) = name_node else { continue };
                    let name = self.text(name_node).to_string();
                    self.create_node("constant", &name, elem, Extra::default());
                }
                true
            }
            // Trait use inside a class-like body: one `implements` ref per
            // used name (full qualified text), all at the use_declaration's
            // position — WITH filePath (the hook sets ctx.filePath; v2 flag).
            "use_declaration" => {
                let names: Vec<Node> = named_kids(node)
                    .filter(|c| matches!(c.kind(), "name" | "qualified_name"))
                    .collect();
                let parent = self.top_row();
                let implements = crate::buffers::EDGE_IMPLEMENTS;
                let line = self.line_of(node);
                let col = self.col_of(node);
                for n in names {
                    let name_ref = self.arena.put(self.text(n));
                    self.tables.push_ref_flagged(
                        &RefRow {
                            from_idx: parent,
                            kind: implements,
                            line,
                            column: col,
                            reference_name: name_ref,
                            candidates: NONE_STR,
                            from_id_str: NONE_STR,
                        },
                        REF_FLAG_FILE_PATH,
                    );
                }
                true
            }
            _ => false,
        }
    }

    // --- the dispatcher (visitNode, PHP-relevant branches) ------------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        if self.try_visit_hook(node) {
            self.scan_fn_ref_subtree(node, 0);
            return;
        }

        let kind = node.kind();
        let mut skip_children = false;

        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "function_definition" {
            // functionTypes; method_declaration is not in it, so this is
            // always extractFunction (php functions can't be class members).
            self.extract_function(node);
            skip_children = true;
        } else if kind == "class_declaration" {
            self.extract_class(node, "class");
            skip_children = true;
        } else if kind == "trait_declaration" {
            // classifyClassNode → 'trait'.
            self.extract_class(node, "trait");
            skip_children = true;
        } else if kind == "method_declaration" {
            // Inside a class-like → method; outside (an anonymous class's
            // members at TOP level — grammar-bump delta #1) the 1747 gate
            // bounces to extractFunction: a file-level `function` node.
            if self.inside_class_like() {
                self.extract_method(node);
            } else {
                self.extract_function(node);
            }
            skip_children = true;
        } else if kind == "interface_declaration" {
            self.extract_interface(node);
            skip_children = true;
        } else if kind == "enum_declaration" {
            self.extract_enum(node);
            skip_children = true;
        } else if kind == "property_declaration" && self.inside_class_like() {
            self.extract_field(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if matches!(
            kind,
            "namespace_use_declaration" | "include_expression" | "include_once_expression"
                | "require_expression" | "require_once_expression"
        ) {
            self.extract_import(node);
            // children still visited (importTypes sets no skipChildren)
        } else if matches!(
            kind,
            "function_call_expression" | "member_call_expression" | "scoped_call_expression"
        ) {
            self.extract_call(node);
        } else if kind == "object_creation_expression" {
            self.extract_instantiation(node);
            if let Some(anon_body) = find_anonymous_class_body(node) {
                // v0.24.2 nests the declaration_list in `anonymous_class`, so
                // this never fires — mirrored for shape.
                self.extract_anonymous_class(node, anon_body);
                skip_children = true;
            }
        }
        // text / php_tag / text_interpolation / namespace_definition /
        // nullsafe_member_call_expression / expression_statement / closures /
        // match / attributes: no branch — children visited.

        if !skip_children {
            for c in named_kids(node) {
                self.visit_node(c);
            }
        }
    }

    // --- visitFunctionBody --------------------------------------------------------


    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        if node.kind() == "assignment_expression" {
            self.emit_local_rows(node);
        }
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if matches!(
            kind,
            "function_call_expression" | "member_call_expression" | "scoped_call_expression"
        ) {
            self.extract_call(node);
        } else if kind == "object_creation_expression" {
            self.extract_instantiation(node);
            if let Some(anon_body) = find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                return;
            }
        }

        // Static value reads (`Cls::CONST`, `Cls::$prop`, `Cls::class`).
        self.extract_static_member_ref(node);

        // Nested NAMED functions; body-level class/trait/enum/interface
        // declarations (the polyfill idiom). NOTE: no method_declaration
        // branch — in-body anonymous-class methods vanish (delta #1), and the
        // visitNode hook does NOT run here (a const_declaration in a body-level
        // class still extracts via extractClass's own visitNode body walk).
        if kind == "function_definition" {
            let name = self.extract_name(node);
            if name != "<anonymous>" {
                self.extract_function(node);
                return;
            }
        }
        if kind == "class_declaration" {
            self.extract_class(node, "class");
            return;
        }
        if kind == "trait_declaration" {
            self.extract_class(node, "trait");
            return;
        }
        if kind == "enum_declaration" {
            self.extract_enum(node);
            return;
        }
        if kind == "interface_declaration" {
            self.extract_interface(node);
            return;
        }

        for c in named_kids(node) {
            self.visit_for_calls_and_structure(c);
        }
    }

    // --- extractors ----------------------------------------------------------------

    fn extract_function(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            if let Some(body) = node.child_by_field_name("body") {
                self.visit_for_calls_and_structure(body);
            }
            return;
        }
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: None, // no getSignature hook
            visibility: Some(self.visibility_of(node)),
            is_static: Some(self.is_static(node)),
            return_type: self.return_type_of(node),
        };
        let Some(row) = self.create_node("function", &name, node, extra) else { return };
        self.extract_php_type_refs(node, row);
        // decorators: none.
        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: None,
            visibility: Some(self.visibility_of(node)),
            is_static: Some(self.is_static(node)),
            return_type: self.return_type_of(node),
        };
        let Some(row) = self.create_node("method", &name, node, extra) else { return };
        self.extract_php_type_refs(node, row);
        self.stack.push(Scope { row, kind: "method", name });
        // Bodiless (interface/abstract) methods still mint nodes, no walk.
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_class(&mut self, node: Node<'t>, kind: &'static str) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node(kind, &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        // primary-ctor refs: csharp-only (needs a parameter_list child type);
        // decorators: none.
        self.stack.push(Scope { row, kind, name });
        let body = node.child_by_field_name("body").unwrap_or(node);
        for c in named_kids(body) {
            self.visit_node(c);
        }
        self.stack.pop();
    }

    fn extract_interface(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default() // NO visibility — extractInterface never asks
        };
        let Some(row) = self.create_node("interface", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "interface", name });
        let body = node.child_by_field_name("body").unwrap_or(node);
        for c in named_kids(body) {
            self.visit_node(c);
        }
        self.stack.pop();
    }

    fn extract_enum(&mut self, node: Node<'t>) {
        stack_guard!();
        let Some(body) = node.child_by_field_name("body") else { return };
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("enum", &name, node, extra) else { return };
        // class_interface_clause → implements refs; the backing type is never
        // read (it's not in a base_clause).
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "enum", name });
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else { continue };
            if child.kind() == "enum_case" {
                self.extract_enum_members(child);
            } else {
                self.visit_node(child);
            }
        }
        self.stack.pop();
    }

    fn extract_enum_members(&mut self, node: Node<'t>) {
        // name-field path: one enum_member at the enum_case; backed values
        // (`= 'H'`) never walked.
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.text(name_node).to_string();
            self.create_node("enum_member", &name, node, Extra::default());
        }
    }

    /// extractField — the php property_element branch (2077-2104): one `field`
    /// node per element, `$` re-added in the signature only, then RETURN — no
    /// decorators, no type-annotation refs from fields.
    fn extract_field(&mut self, node: Node<'t>) {
        let docstring = preceding_docstring(node, self.src);
        let visibility = Some(self.visibility_of(node));
        let is_static = Some(self.is_static(node));

        let prop_elements: Vec<Node> = named_kids(node)
            .filter(|c| c.kind() == "property_element")
            .collect();
        if prop_elements.is_empty() {
            // The declarator/bare fallbacks find nothing on php shapes.
            return;
        }
        // The type node: first namedChild that isn't a modifier or element.
        // QUIRK: final_modifier/abstract_modifier are NOT excluded — a
        // `final public Foo $x` takes `final` as the type text. PRESERVE.
        let type_node = named_kids(node)
            .find(|c| {
                !matches!(
                    c.kind(),
                    "visibility_modifier" | "static_modifier" | "readonly_modifier"
                        | "property_element" | "var_modifier"
                )
            });
        let type_text = type_node.map(|t| self.text(t).to_string());

        for elem in prop_elements {
            let var_name = named_kids(elem)
                .find(|c| c.kind() == "variable_name");
            let Some(var_name) = var_name else { continue };
            let name_node = named_kids(var_name)
                .find(|c| c.kind() == "name");
            let Some(name_node) = name_node else { continue };
            let name = self.text(name_node).to_string();
            let signature = match &type_text {
                Some(t) => format!("{t} ${name}"),
                None => format!("${name}"),
            };
            self.create_node(
                "field",
                &name,
                elem,
                Extra {
                    docstring: docstring.clone(),
                    signature: Some(signature),
                    visibility,
                    is_static,
                    ..Extra::default()
                },
            );
        }
    }

    // --- imports -------------------------------------------------------------------

    // --- bindings (resolution-binding-model-plan.md, Phase 3: PHP) --------------------

    enclosing_scope_impl!("file" | "namespace");

    push_binding_row_impl!();

    // --- calls ---------------------------------------------------------------------

    // --- php type refs (extractPhpTypeRefs, 6022) ----------------------------------

    // --- function-as-value refs (PHP_SPEC, function-ref.ts:360) --------------------

    flush_fn_ref_candidates_impl!();

    // --- value references ------------------------------------------------------------

}

/// The function name node of the php call whose arguments contain `node` —
/// ≤4 parent hops to a function_call_expression; member/scoped calls abort
/// (method-call HOFs never qualify). (function-ref.ts:822)
fn php_enclosing_call_name(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    for _ in 0..4 {
        let c = cur?;
        if c.kind() == "function_call_expression" {
            return c.child_by_field_name("function");
        }
        if matches!(c.kind(), "member_call_expression" | "scoped_call_expression") {
            return None;
        }
        cur = c.parent();
    }
    None
}

fn find_anonymous_class_body(node: Node) -> Option<Node> {
    for child in named_kids(node) {
        if matches!(child.kind(), "class_body" | "declaration_list") {
            return Some(child);
        }
    }
    None
}


