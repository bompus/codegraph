//! Java extraction — a faithful Rust port of `TreeSitterExtractor`'s Java
//! paths (src/extraction/tree-sitter.ts) plus languages/java.ts, including
//! the Lombok member synthesizer (#912).
//!
//! Same porting contract as tsjs/: behavior parity with the wasm path,
//! bug-for-bug, verified by scripts/kernel-parity.mjs and the full-index
//! dump-diff gate. Positions in UTF-16 code units. Files with parse errors
//! are walked like any other (tree-sitter's recovery is canonical).

mod lombok;
mod bindings;
mod refs;
use crate::buffers::{
    BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, edge_kind_index, node_kind_index, Arena, BoolFlags, EdgeRow, EmitOut, NodeRow,
    RefRow, StrRef, Tables, FLAG_IS_STATIC, NONE, NONE_STR,
};
use crate::walker::{Scope, ValueScope, Cand};
use crate::textutil::{is_builtin_type, strip_generic_and_qualifier, capitalized_re};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tree_sitter::Node;


fn is_method_type(kind: &str) -> bool {
    matches!(kind, "method_declaration" | "constructor_declaration")
}
fn is_interface_type(kind: &str) -> bool {
    matches!(kind, "interface_declaration" | "annotation_type_declaration")
}

/// JAVA_NON_CLASS_RETURN_NODES (languages/java.ts).
fn is_non_class_return(kind: &str) -> bool {
    matches!(kind, "void_type" | "integral_type" | "floating_point_type" | "boolean_type")
}


/// LOMBOK_LOG_ANNOTATIONS (languages/java.ts).
fn has_ann(anns: &[String], name: &str) -> bool {
    anns.iter().any(|a| a == name)
}

fn is_lombok_log_annotation(name: &str) -> bool {
    matches!(
        name,
        "Slf4j" | "Log4j" | "Log4j2" | "Log" | "CommonsLog" | "JBossLog" | "Flogger" | "XSlf4j"
            | "CustomLog"
    )
}

fn method_ref_type_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([A-Z][A-Za-z0-9_]*)\s*::").unwrap())
}
fn is_prefix_re(word: &str) -> bool {
    // /^is[A-Z]/ for Lombok boolean getters.
    word.len() > 2 && word.starts_with("is") && word.as_bytes()[2].is_ascii_uppercase()
}


/// Per-node metadata kept for the Lombok synthesizer's taken-member scan
/// (mirrors its walk over ctx.nodes by qualifiedName).
struct NodeMeta {
    kind: &'static str,
    name: String,
    qualified_name: String,
}

#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<u8>,
    is_static: Option<bool>,
    return_type: Option<String>,
    decorators: Option<Vec<String>>,
}



pub struct Walker<'t> {
    src: &'t str,
    file_path: &'t str,
    cols: util::Cols,
    arena: Arena,
    tables: Tables,
    stack: Vec<Scope>,
    nodes_meta: Vec<NodeMeta>,
    /// Node id string per row — ids COLLIDE for same-(kind, name, line) nodes
    /// and the TS side's fn-ref dedupe / value-ref self-checks key on the id.
    node_ids: Vec<String>,
    defined_fn_names: HashSet<String>,
    imported_names: HashSet<String>,
    fn_ref_cands: Vec<Cand>,
    fs_values: HashMap<String, u32>,
    fs_value_counts: HashMap<String, u32>,
    value_scopes: Vec<ValueScope<'t>>,
    /// Markdown path refs already emitted — see markdown_refs_impl! (lib.rs).
    md_ref_keys: HashSet<String>,
    line_count: u32,
}

pub fn extract(file_path: &str, source: &str) -> Result<EmitOut, String> {
    let t0 = std::time::Instant::now();
    let tree = crate::langs::parse("java", source)?;

    let mut w = Walker::new(source, file_path);

    // File node (TreeSitterExtractor.extract).
    let line_count = w.line_count;
    let base_name = crate::buffers::push_file_node(&mut w.arena, &mut w.tables, file_path, line_count);
    w.nodes_meta.push(NodeMeta {
        kind: "file",
        name: base_name.to_string(),
        qualified_name: file_path.to_string(),
    });
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });

    // extractFilePackage: wrap top-level declarations in a `namespace` node
    // carrying the package FQN.
    let root = tree.root_node();
    let mut pkg_pushed = false;
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else { continue };
        if child.kind() != "package_declaration" {
            continue;
        }
        let id_node = (0..child.named_child_count())
            .filter_map(|j| child.named_child(j))
            .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"));
        if let Some(id_node) = id_node {
            let pkg = w.text(id_node).trim().to_string();
            if !pkg.is_empty() {
                if let Some(row) = w.create_node("namespace", &pkg, child, Extra::default()) {
                    w.stack.push(Scope { row, kind: "namespace", name: pkg });
                    pkg_pushed = true;
                }
            }
        }
        break;
    }

    w.visit_node(root);
    w.flush_fn_ref_candidates();
    w.flush_value_refs(root);
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
            stack: Vec::new(),
            nodes_meta: Vec::new(),
            node_ids: Vec::new(),
            defined_fn_names: HashSet::new(),
            imported_names: HashSet::new(),
            fn_ref_cands: Vec::new(),
            fs_values: HashMap::new(),
            fs_value_counts: HashMap::new(),
            value_scopes: Vec::new(),
            md_ref_keys: HashSet::new(),
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
        let end_line = node.end_position().row as u32 + 1; // no resolveBody for java

        let qualified = {
            let mut parts: Vec<&str> = Vec::new();
            for s in &self.stack {
                if s.kind != "file" {
                    parts.push(&s.name);
                }
            }
            let mut qn = parts.join("::");
            if !qn.is_empty() {
                qn.push_str("::");
            }
            qn.push_str(name);
            qn
        };

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
        let dec_ref: StrRef = match &extra.decorators {
            Some(list) if !list.is_empty() => self.arena.put_list(list),
            _ => NONE_STR,
        };
        let type_params: Vec<String> = node.child_by_field_name("type_parameters")
            .map(|params| {
                let mut cursor = params.walk();
                params.named_children(&mut cursor)
                    .filter(|param| param.kind() == "type_parameter")
                    .map(|param| self.text(param).to_string()).collect()
            }).unwrap_or_default();
        let type_params_ref = if type_params.is_empty() { NONE_STR } else { self.arena.put_list(&type_params) };
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
            decorators: dec_ref,
            type_parameters: type_params_ref,
            return_type: ret_ref,
            extra_json: NONE_STR,
        });
        self.nodes_meta.push(NodeMeta { kind, name: name.to_string(), qualified_name: qualified });
        self.node_ids.push(id);

        let parent_row = self.top_row();
        self.tables.push_edge(&EdgeRow {
            source_idx: parent_row,
            target_idx: row,
            kind: edge_kind_index("contains").unwrap(),
            provenance: 0,
            line: NONE,
            column: NONE,
            metadata_json: NONE_STR,
            source_id_str: NONE_STR,
            target_id_str: NONE_STR,
        });

        if kind == "function" || kind == "method" {
            self.defined_fn_names.insert(name.to_string());
        }
        self.capture_value_ref_scope(kind, name, row, node);
        self.emit_decl_binding(kind, name, row, node, extra.visibility);
        if kind == "function" || kind == "method" {
            self.emit_param_bindings(node);
        }
        Some(row)
    }

    // --- modifiers / hooks (languages/java.ts) -----------------------------------

    fn modifiers_child(&self, node: Node<'t>) -> Option<Node<'t>> {
        (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "modifiers")
    }

    fn visibility_of(&self, node: Node) -> Option<u8> {
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            if child.kind() == "modifiers" {
                let text = self.text(child);
                if text.contains("public") {
                    return Some(1);
                }
                if text.contains("private") {
                    return Some(2);
                }
                if text.contains("protected") {
                    return Some(3);
                }
            }
        }
        None
    }

    fn is_static(&self, node: Node) -> bool {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" && self.text(child).contains("static") {
                    return true;
                }
            }
        }
        false
    }

    /// javaExtractor.isConst: `static final` field → constant.
    fn is_const(&self, node: Node) -> bool {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" {
                    let text = self.text(child);
                    return word_re("static").is_match(text) && word_re("final").is_match(text);
                }
            }
        }
        false
    }

    fn signature_of(&self, node: Node) -> Option<String> {
        let params = node.child_by_field_name("parameters")?;
        let params_text = self.text(params);
        match node.child_by_field_name("type") {
            Some(ret) => Some(format!("{} {}", self.text(ret), params_text)),
            None => Some(params_text.to_string()),
        }
    }

    /// normalizeJavaType (languages/java.ts).
    fn normalize_java_type(&self, type_node: Option<Node>) -> Option<String> {
        let t = type_node?;
        if is_non_class_return(t.kind()) || t.kind() == "array_type" {
            return None;
        }
        let raw = crate::textutil::generic_args_re().replace_all(self.text(t).trim(), "").into_owned();
        let last = raw.rsplit('.').next().unwrap_or("").trim().to_string();
        if last.is_empty() || !crate::textutil::ascii_ident_re().is_match(&last) {
            return None;
        }
        Some(last)
    }

    extract_name_impl!();

    // --- the dispatcher (visitNode, Java-relevant branches) -----------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "class_declaration" {
            self.extract_class(node);
            skip_children = true;
        } else if is_method_type(kind) {
            self.extract_method(node);
            skip_children = true;
        } else if is_interface_type(kind) {
            self.extract_interface(node);
            skip_children = true;
        } else if kind == "enum_declaration" {
            self.extract_enum(node);
            skip_children = true;
        } else if kind == "field_declaration" && self.inside_class_like() {
            self.extract_field(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "local_variable_declaration" && !self.inside_class_like() {
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "import_declaration" {
            self.extract_import(node);
        } else if kind == "method_invocation" {
            self.extract_call(node);
        } else if kind == "object_creation_expression" {
            self.extract_instantiation(node);
            if let Some(anon_body) = find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                skip_children = true;
            }
        }

        if !skip_children {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    self.visit_node(c);
                }
            }
        }
    }

    // --- visitFunctionBody ----------------------------------------------------------


    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);
        if kind == "local_variable_declaration" {
            self.emit_local_rows(node);
        }

        if kind == "method_invocation" {
            self.extract_call(node);
        } else if kind == "object_creation_expression" {
            self.extract_instantiation(node);
            if let Some(anon_body) = find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                return;
            }
        }

        // Static-member / value-read (`Type.CONST`) — self-gates on field_access.
        self.extract_static_member_ref(node);

        if kind == "class_declaration" {
            self.extract_class(node);
            return;
        }
        if kind == "enum_declaration" {
            self.extract_enum(node);
            return;
        }
        if is_interface_type(kind) {
            self.extract_interface(node);
            return;
        }

        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.visit_for_calls_and_structure(c);
            }
        }
    }

    // --- extractors --------------------------------------------------------------

    fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: self.visibility_of(node),
            ..Extra::default() // java has no isExported hook
        };
        let Some(row) = self.create_node("class", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.extract_decorators_for(node, row);

        self.stack.push(Scope { row, kind: "class", name });
        let body = node.child_by_field_name("body").unwrap_or(node);
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        // Lombok member synthesis (#912) — class still on the stack.
        self.synthesize_lombok_members(node, row);
        self.stack.pop();
    }

    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        if !self.inside_class_like() {
            // (object-literal parents don't exist in Java; a stray top-level
            // method extracts as a function, mirroring extractMethod's tail)
            self.extract_function(node);
            return;
        }
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: self.signature_of(node),
            visibility: self.visibility_of(node),
            is_static: Some(self.is_static(node)),
            return_type: self.normalize_java_type(node.child_by_field_name("type")),
            ..Extra::default()
        };
        let Some(row) = self.create_node("method", &name, node, extra) else { return };
        self.extract_type_annotations(node, row);
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "method", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    /// extractFunction — only reachable for a method outside any class.
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
            signature: self.signature_of(node),
            visibility: self.visibility_of(node),
            is_static: Some(self.is_static(node)),
            return_type: self.normalize_java_type(node.child_by_field_name("type")),
            ..Extra::default()
        };
        let Some(row) = self.create_node("function", &name, node, extra) else { return };
        self.extract_type_annotations(node, row);
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_interface(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default()
        };
        let Some(row) = self.create_node("interface", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "interface", name });
        let body = node.child_by_field_name("body").unwrap_or(node);
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    fn extract_enum(&mut self, node: Node<'t>) {
        stack_guard!();
        let Some(body) = node.child_by_field_name("body") else { return };
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: self.visibility_of(node),
            ..Extra::default()
        };
        let Some(row) = self.create_node("enum", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "enum", name });
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else { continue };
            if child.kind() == "enum_constant" {
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
        }
        // (identifier-children / leaf fallbacks are other grammars' shapes)
    }

    /// extractField — each declarator becomes a field/constant node.
    fn extract_field(&mut self, node: Node<'t>) {
        stack_guard!();
        let docstring = preceding_docstring(node, self.src);
        let visibility = self.visibility_of(node);
        let is_static = Some(self.is_static(node));
        let field_kind: &'static str = if self.is_const(node) { "constant" } else { "field" };

        let declarators: Vec<Node> = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .filter(|c| c.kind() == "variable_declarator")
            .collect();

        if !declarators.is_empty() {
            let type_node = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| {
                    !matches!(
                        c.kind(),
                        "modifiers" | "modifier" | "variable_declarator" | "variable_declaration"
                            | "marker_annotation" | "annotation"
                    )
                });
            let type_text = type_node.map(|t| self.text(t).to_string());

            for decl in declarators {
                let name_node = decl.child_by_field_name("name").or_else(|| {
                    (0..decl.named_child_count())
                        .filter_map(|i| decl.named_child(i))
                        .find(|c| c.kind() == "identifier")
                });
                let Some(name_node) = name_node else { continue };
                let name = self.text(name_node).to_string();
                let signature = match &type_text {
                    Some(t) => format!("{t} {name}"),
                    None => name.clone(),
                };
                let row = self.create_node(
                    field_kind,
                    &name,
                    decl,
                    Extra {
                        docstring: docstring.clone(),
                        signature: Some(signature),
                        visibility,
                        is_static,
                        ..Extra::default()
                    },
                );
                if let Some(row) = row {
                    self.extract_decorators_for(node, row);
                    self.extract_type_annotations(node, row);
                    // Walk the initializer ATTRIBUTED to the declared field
                    // (#693, the Go fix): the dispatcher only fn-ref-scans this
                    // subtree, so a lambda / method reference / anonymous class
                    // in `private final Runnable r = () -> target();` emitted no
                    // call edge at all.
                    if let Some(value) = decl.child_by_field_name("value") {
                        self.stack.push(Scope { row, kind: field_kind, name: name.clone() });
                        self.visit_for_calls_and_structure(value);
                        self.stack.pop();
                    }
                }
            }
        } else {
            let name_node = node.child_by_field_name("name").or_else(|| {
                (0..node.named_child_count())
                    .filter_map(|i| node.named_child(i))
                    .find(|c| c.kind() == "identifier")
            });
            if let Some(name_node) = name_node {
                let name = self.text(name_node).to_string();
                let row = self.create_node(
                    field_kind,
                    &name,
                    node,
                    Extra { docstring, visibility, is_static, ..Extra::default() },
                );
                if let Some(row) = row {
                    self.markdown_refs_from_subtree(node, row);
                }
            }
        }
    }

    /// extractVariable's generic fallback (top-level locals — rare in Java).
    fn extract_variable(&mut self, node: Node<'t>) {
        let kind: &'static str = if self.is_const(node) { "constant" } else { "variable" };
        let docstring = preceding_docstring(node, self.src);
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            let name = match child.kind() {
                "identifier" => self.text(child).to_string(),
                "variable_declarator" => self.extract_name(child),
                _ => continue,
            };
            if name.is_empty() || name == "<anonymous>" {
                continue;
            }
            self.create_node(
                kind,
                &name,
                child,
                Extra { docstring: docstring.clone(), ..Extra::default() },
            );
        }
    }

    fn extract_import(&mut self, node: Node<'t>) {
        let import_text = self.text(node).trim().to_string();
        let scoped = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "scoped_identifier");
        let Some(scoped) = scoped else { return }; // hook declined
        let module_name = self.text(scoped).to_string();
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
        self.push_ref_at(parent, &module_name, edge_kind_index("imports").unwrap(), node);
        self.import_row_of(node, &module_name);
    }

    // --- bindings (resolution-binding-model-plan.md, Phase 3: JVM) --------------------

    enclosing_scope_impl!("file" | "namespace");

    push_binding_row_impl!();

    /// extractCall — the Java method_invocation paths.
    fn extract_call(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let caller = self.top_row();
        let name_field = node.child_by_field_name("name");
        let object_field = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("scope"));

        let mut callee_name = String::new();
        if let (Some(name_field), Some(object_field)) = (name_field, object_field) {
            let method_name = self.text(name_field);

            // Static-factory / fluent chain: `Foo.getInstance().bar()` →
            // `<inner-receiver>.<inner-method>().<method>` (#645/#608).
            if !method_name.is_empty() && object_field.kind() == "method_invocation" {
                let inner_obj = object_field.child_by_field_name("object");
                let inner_name = object_field.child_by_field_name("name");
                if let (Some(io), Some(inm)) = (inner_obj, inner_name) {
                    let callee = format!("{}.{}().{}", self.text(io), self.text(inm), method_name);
                    self.push_ref_at(caller, &callee, edge_kind_index("calls").unwrap(), node);
                    return;
                }
            }

            // `this.userbo.toLogin2()` — unwrap the field after `this.`.
            let receiver_name = if object_field.kind() == "field_access" {
                let inner = object_field.child_by_field_name("object");
                let fld = object_field.child_by_field_name("field");
                match (inner, fld) {
                    (Some(inner), Some(fld))
                        if matches!(inner.kind(), "this" | "this_expression") =>
                    {
                        self.text(fld).to_string()
                    }
                    _ => self.text(object_field).to_string(),
                }
            } else {
                self.text(object_field).to_string()
            };
            let receiver_name = receiver_name.strip_prefix('$').unwrap_or(&receiver_name);

            if !method_name.is_empty() {
                if matches!(receiver_name, "self" | "this" | "cls" | "super" | "parent" | "static") {
                    callee_name = method_name.to_string();
                } else {
                    callee_name = format!("{receiver_name}.{method_name}");
                }
            }
        } else {
            // Bare call `foo()` — the generic tail: function field ?? first child.
            let func = node
                .child_by_field_name("function")
                .or_else(|| node.named_child(0));
            if let Some(func) = func {
                callee_name = self.text(func).to_string();
            }
        }

        if !callee_name.is_empty() {
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
            self.push_ref_at(caller, &callee_name, edge_kind_index("calls").unwrap(), node);
        }
    }

    fn extract_instantiation(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let ctor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };
        let class_name = strip_generic_and_qualifier(self.text(ctor));
        if !class_name.is_empty() {
            let from = self.top_row();
            self.push_ref_at(from, &class_name, edge_kind_index("instantiates").unwrap(), node);
        }
    }

    /// extractAnonymousClass — `new T() { ... }`.
    fn extract_anonymous_class(&mut self, node: Node<'t>, body: Node<'t>) {
        stack_guard!();
        let type_node = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let mut type_name = type_node.map(|t| self.text(t).to_string()).unwrap_or_else(|| "Object".to_string());
        type_name = strip_generic_and_qualifier(&type_name);
        if type_name.is_empty() {
            type_name = "Object".to_string();
        }

        let anon_name = format!("<{type_name}$anon@{}>", node.start_position().row + 1);
        let Some(row) = self.create_node("class", &anon_name, node, Extra::default()) else {
            return;
        };
        // Bug-for-bug: the TS code uses `startPosition.row` (0-based) as the
        // LINE here — the one place it forgets the +1.
        let (line, column) = match type_node {
            Some(t) => (t.start_position().row as u32, self.col_of(t)),
            None => (node.start_position().row as u32, self.col_of(node)),
        };
        self.push_ref(row, &type_name, edge_kind_index("extends").unwrap(), line, column);

        self.stack.push(Scope { row, kind: "class", name: anon_name });
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    /// extractStaticMemberRef — `Type.CONST` value reads (java: field_access).
    fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if node.kind() != "field_access" {
            return;
        }
        if self.stack.is_empty() {
            return;
        }
        let owner = self.top_row();
        // Skip `Type.method()` — the access is a call's callee, already linked.
        if let Some(parent) = node.parent() {
            if parent.kind() == "method_invocation" {
                let callee = parent
                    .child_by_field_name("function")
                    .or_else(|| parent.child_by_field_name("method"))
                    .or_else(|| parent.named_child(0));
                if let Some(callee) = callee {
                    if callee.start_byte() == node.start_byte() {
                        return;
                    }
                }
            }
        }
        let recv = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("expression"))
            .or_else(|| node.child_by_field_name("scope"))
            .or_else(|| node.named_child(0));
        let Some(recv) = recv else { return };
        if matches!(
            recv.kind(),
            "identifier" | "type_identifier" | "simple_identifier" | "name" | "scoped_type_identifier"
        ) {
            let text = self.text(recv);
            if capitalized_re().is_match(text) {
                self.push_ref_at(owner, text, edge_kind_index("references").unwrap(), recv);
            }
        }
    }

    /// extractInheritance — the Java clauses (type_list-aware).
    fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let extends_kind = edge_kind_index("extends").unwrap();
        let implements_kind = edge_kind_index("implements").unwrap();
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                "superclass" | "extends_interfaces" => {
                    let type_list = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .find(|c| c.kind() == "type_list");
                    let targets: Vec<Node> = match type_list {
                        Some(tl) => (0..tl.named_child_count()).filter_map(|j| tl.named_child(j)).collect(),
                        None => child.named_child(0).into_iter().collect(),
                    };
                    for target in targets {
                        let name = self.text(target).to_string();
                        self.push_ref_at(class_row, &name, extends_kind, target);
                    }
                }
                "super_interfaces" => {
                    let type_list = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .find(|c| c.kind() == "type_list");
                    let targets: Vec<Node> = match type_list {
                        Some(tl) => (0..tl.named_child_count()).filter_map(|j| tl.named_child(j)).collect(),
                        None => (0..child.named_child_count()).filter_map(|j| child.named_child(j)).collect(),
                    };
                    for iface in targets {
                        let name = self.text(iface).to_string();
                        self.push_ref_at(class_row, &name, implements_kind, iface);
                    }
                }
                _ => {}
            }
        }
    }

    decorators_impl!();


    /// extractTypeAnnotations — Java's returnField is `type`.
    fn extract_type_annotations(&mut self, node: Node<'t>, from_row: u32) {
        if let Some(params) = node.child_by_field_name("parameters") {
            self.extract_type_refs_from_subtree(params, from_row);
        }
        if let Some(ret) = node.child_by_field_name("type") {
            self.extract_type_refs_from_subtree(ret, from_row);
        }
        let type_annotation = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = type_annotation {
            self.extract_type_refs_from_subtree(ta, from_row);
        }
    }

    type_refs_from_subtree_impl!();

    // --- function-as-value refs (JAVA_SPEC: method references only) ----------------

    flush_fn_ref_candidates_impl!();

    // --- value references ------------------------------------------------------------

    // --- Lombok synthesis (#912, languages/java.ts synthesizeLombokMembers) ------------

}

fn find_anonymous_class_body(node: Node) -> Option<Node> {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if matches!(child.kind(), "class_body" | "declaration_list") {
                return Some(child);
            }
        }
    }
    None
}


fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `\bword\b` matcher (modifier keyword tests in languages/java.ts).
fn word_re(word: &'static str) -> &'static Regex {
    static STATIC_RE: OnceLock<Regex> = OnceLock::new();
    static FINAL_RE: OnceLock<Regex> = OnceLock::new();
    match word {
        "static" => STATIC_RE.get_or_init(|| Regex::new(r"(?-u:\b)static(?-u:\b)").unwrap()),
        "final" => FINAL_RE.get_or_init(|| Regex::new(r"(?-u:\b)final(?-u:\b)").unwrap()),
        _ => unreachable!("word_re only supports static/final"),
    }
}

