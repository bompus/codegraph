//! Go extraction — a faithful Rust port of `TreeSitterExtractor`'s Go paths
//! (src/extraction/tree-sitter.ts) plus languages/go.ts.
//!
//! Go's shape quirks, mirrored exactly: methods are top-level with a receiver
//! (qualifiedName override `Recv::name` + a contains edge to the FIRST
//! earlier-in-file struct of that name), structs/interfaces arrive as
//! `type_spec` and classify via the inner type node (struct embedding →
//! extends; interface method_elems become method nodes), composite literals
//! (`pkga.Widget{}`) keep their package qualifier as `instantiates` refs,
//! top-level var/const specs walk their initializers ATTRIBUTED to the
//! declared symbol (#693), 2-hop field chains (`t.conn.Exec`) keep the chain
//! (#1276), and `New().Method()` re-encodes as `New().Method` (#645/#608)
//! only for bare-identifier factories. Files with parse errors are walked like any other (tree-sitter's recovery is canonical; buffers::parse_collapse_warning reports a collapsed parse).

use crate::buffers::{
    edge_kind_index, node_kind_index, Arena, BoolFlags, EdgeRow, EmitOut, NodeRow,
    RefRow, Tables, BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, FLAG_IS_EXPORTED, NONE, NONE_STR,
};
use crate::walker::{Scope, ValueScope, Cand};
use crate::textutil::{is_stoplisted, is_builtin_type, is_literal_receiver};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tree_sitter::Node;


fn receiver_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(\s*(?:[A-Za-z_][0-9A-Za-z_]*\s+)?\*?\s*([A-Za-z_][0-9A-Za-z_]*)").unwrap())
}
fn go_two_hop_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][0-9A-Za-z_]*\.[A-Za-z_][0-9A-Za-z_]*$").unwrap())
}
fn bracket_args_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[[^\]]*\]").unwrap())
}

// Grouped var declarations wrap their specs in var_spec_list; constants and
// ungrouped declarations expose specs directly. Share that grammar boundary
// between symbol extraction and binding collection.
fn declaration_specs(node: Node<'_>) -> Vec<Node<'_>> {
    let mut specs = Vec::new();
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else { continue };
        if matches!(child.kind(), "var_spec" | "const_spec") {
            specs.push(child);
        } else if child.kind() == "var_spec_list" {
            specs.extend((0..child.named_child_count()).filter_map(|j| child.named_child(j))
                .filter(|n| n.kind() == "var_spec"));
        }
    }
    specs
}


#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    is_exported: Option<bool>,
    return_type: Option<String>,
    qualified_name: Option<String>,
}



/// Per-node metadata for the receiver-method owner lookup (mirrors the TS
/// side's scan over `this.nodes` — FIRST match wins, earlier-in-file only).
struct NodeMeta {
    kind: &'static str,
    name: String,
}

pub struct Walker<'t> {
    src: &'t str,
    file_path: &'t str,
    cols: util::Cols,
    arena: Arena,
    tables: Tables,
    stack: Vec<Scope>,
    nodes_meta: Vec<NodeMeta>,
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
    let tree = crate::langs::parse("go", source)?;

    let mut w = Walker::new(source, file_path);

    let line_count = w.line_count;
    let base_name = crate::buffers::push_file_node(&mut w.arena, &mut w.tables, file_path, line_count);
    w.nodes_meta.push(NodeMeta { kind: "file", name: base_name.to_string() });
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });

    w.visit_node(tree.root_node());
    w.flush_fn_ref_candidates();
    w.flush_value_refs(tree.root_node());
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

    fn create_node(&mut self, kind: &'static str, name: &str, node: Node<'t>, extra: Extra) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let start_line = self.line_of(node);
        let id = ids::node_id(self.file_path, kind, name, start_line);
        let end_line = node.end_position().row as u32 + 1;

        let qualified = extra.qualified_name.unwrap_or_else(|| {
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
        });

        let mut flags = BoolFlags::default();
        if let Some(v) = extra.is_exported {
            flags.set(FLAG_IS_EXPORTED, v);
        }
        let name_ref = self.arena.put(name);
        let qn_ref = self.arena.put(&qualified);
        let id_ref = self.arena.put(&id);
        let doc_ref = self.arena.put_opt(extra.docstring.as_deref());
        let sig_ref = self.arena.put_opt(extra.signature.as_deref());
        let ret_ref = self.arena.put_opt(extra.return_type.as_deref());
        let row = self.tables.push_node(&NodeRow {
            kind: node_kind_index(kind).unwrap(),
            visibility: 0,
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
            decorators: NONE_STR,
            type_parameters: NONE_STR,
            return_type: ret_ref,
            extra_json: NONE_STR,
        });
        self.nodes_meta.push(NodeMeta { kind, name: name.to_string() });
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
                *self.fs_value_counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
        if matches!(kind, "function" | "method" | "constant" | "variable") {
            self.value_scopes.push(ValueScope { row, node, name: name.to_string() });
        }
        self.emit_decl_binding(kind, name, row, node);
        if kind == "function" || kind == "method" {
            self.emit_param_bindings(node);
        }
        Some(row)
    }

    extract_name_impl!();

    /// goExtractor.getSignature: params + ' ' + result.
    fn signature_of(&self, node: Node) -> Option<String> {
        let params = node.child_by_field_name("parameters")?;
        let mut sig = self.text(params).to_string();
        if let Some(result) = node.child_by_field_name("result") {
            sig.push(' ');
            sig.push_str(self.text(result));
        }
        Some(sig)
    }

    /// goExtractor.isExported: uppercase first letter of the name field.
    fn is_exported(&self, node: Node) -> bool {
        if let Some(name_node) = node.child_by_field_name("name") {
            let text = self.text(name_node);
            return text.as_bytes().first().map(|b| b.is_ascii_uppercase()).unwrap_or(false);
        }
        false
    }

    /// extractGoReturnType (languages/go.ts).
    fn return_type_of(&self, node: Node) -> Option<String> {
        let mut result = node.child_by_field_name("result")?;
        if result.kind() == "parameter_list" {
            let first = (0..result.named_child_count())
                .filter_map(|i| result.named_child(i))
                .find(|c| c.kind() == "parameter_declaration")?;
            result = first.child_by_field_name("type").unwrap_or(first);
        }
        if result.kind() == "pointer_type" {
            result = (0..result.named_child_count())
                .filter_map(|i| result.named_child(i))
                .find(|c| matches!(c.kind(), "type_identifier" | "qualified_type" | "generic_type"))
                .unwrap_or(result);
        }
        let text = self.text(result).trim();
        let text = text.strip_prefix('*').unwrap_or(text);
        let text = crate::textutil::generic_args_re().replace_all(text, "");
        let text = bracket_args_re().replace_all(&text, "");
        let last = text.rsplit('.').next().unwrap_or("").trim().to_string();
        if last.is_empty() || !crate::textutil::ascii_ident_re().is_match(&last) {
            return None;
        }
        Some(last)
    }

    /// goExtractor.getReceiverType: the regex over the receiver's text.
    fn receiver_type_of(&self, node: Node) -> Option<String> {
        let receiver = node.child_by_field_name("receiver")?;
        let text = self.text(receiver);
        receiver_re().captures(text).map(|c| c[1].to_string())
    }

    // --- visitNode ------------------------------------------------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "function_declaration" {
            self.extract_function(node);
            skip_children = true;
        } else if kind == "method_declaration" {
            self.extract_method(node);
            skip_children = true;
        } else if kind == "type_spec" {
            skip_children = self.extract_type_alias(node);
        } else if matches!(kind, "var_declaration" | "short_var_declaration" | "const_declaration")
            && !self.inside_class_like()
        {
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "import_declaration" {
            self.extract_import(node);
        } else if kind == "call_expression" {
            self.extract_call(node);
        } else if kind == "composite_literal" {
            self.extract_instantiation(node);
        }

        if !skip_children {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    self.visit_node(c);
                }
            }
        }
    }


    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "call_expression" {
            self.extract_call(node);
        } else if kind == "composite_literal" {
            self.extract_instantiation(node);
        }
        if matches!(kind, "short_var_declaration" | "var_declaration" | "range_clause") {
            self.emit_local_rows(node);
        }

        if kind == "function_declaration" {
            let name = self.extract_name(node);
            if name != "<anonymous>" {
                self.extract_function(node);
                return;
            }
        }

        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.visit_for_calls_and_structure(c);
            }
        }
    }

    // --- extractors --------------------------------------------------------------

    fn extract_function(&mut self, node: Node<'t>) {
        stack_guard!();
        // (getReceiverType only matches method_declaration's receiver field —
        // function_declaration has none, so no reroute happens here)
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
            is_exported: Some(self.is_exported(node)),
            return_type: self.return_type_of(node),
            ..Extra::default()
        };
        let Some(row) = self.create_node("function", &name, node, extra) else { return };
        self.extract_type_annotations(node, row);
        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        // methodsAreTopLevel: always a method. Receiver-qualified name +
        // a contains edge from the FIRST earlier struct/class/enum/trait
        // node of the receiver's name (mirrors the this.nodes.find scan).
        let receiver_type = self.receiver_type_of(node);
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: self.signature_of(node),
            return_type: self.return_type_of(node),
            qualified_name: receiver_type.as_ref().map(|r| format!("{r}::{name}")),
            ..Extra::default() // extractMethod passes no isExported
        };
        let Some(row) = self.create_node("method", &name, node, extra) else { return };

        if let Some(receiver_type) = &receiver_type {
            if !self.inside_class_like() {
                let owner_row = self
                    .nodes_meta
                    .iter()
                    .position(|m| {
                        m.name == *receiver_type
                            && matches!(m.kind, "struct" | "class" | "enum" | "trait")
                    })
                    .map(|i| i as u32);
                if let Some(owner_row) = owner_row {
                    self.tables.push_edge(&EdgeRow {
                        source_idx: owner_row,
                        target_idx: row,
                        kind: edge_kind_index("contains").unwrap(),
                        provenance: 0,
                        line: NONE,
                        column: NONE,
                        metadata_json: NONE_STR,
                        source_id_str: NONE_STR,
                        target_id_str: NONE_STR,
                    });
                }
            }
        }

        self.extract_type_annotations(node, row);
        self.stack.push(Scope { row, kind: "method", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    /// extractTypeAlias for Go: type_spec → struct / interface / plain alias.
    fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let docstring = preceding_docstring(node, self.src);
        let is_exported = Some(self.is_exported(node));
        let type_child = node.child_by_field_name("type");
        let resolved = type_child.map(|t| t.kind());

        if resolved == Some("struct_type") {
            let Some(row) = self.create_node(
                "struct",
                &name,
                node,
                Extra { docstring, is_exported, ..Extra::default() },
            ) else {
                return true;
            };
            self.stack.push(Scope { row, kind: "struct", name });
            if let Some(type_child) = type_child {
                // Struct embedding → extends (field_declaration without a
                // field_identifier), reached via the inheritance recursion.
                self.extract_inheritance(type_child, row);
                let body = type_child.child_by_field_name("body").unwrap_or(type_child);
                for i in 0..body.named_child_count() {
                    if let Some(c) = body.named_child(i) {
                        self.visit_node(c);
                    }
                }
            }
            self.stack.pop();
            return true;
        }

        if resolved == Some("interface_type") {
            let Some(row) = self.create_node(
                "interface",
                &name,
                node,
                Extra { docstring, is_exported, ..Extra::default() },
            ) else {
                return true;
            };
            if let Some(type_child) = type_child {
                self.extract_inheritance(type_child, row);
                self.extract_go_interface_methods(type_child, row, &name);
            }
            return true;
        }

        self.create_node(
            "type_alias",
            &name,
            node,
            Extra { docstring, is_exported, ..Extra::default() },
        );
        // (go type_spec has no `value` field — no type-ref walk; TS/tsx member
        // extraction is TS-family-only)
        false
    }

    /// extractGoInterfaceMethods: method_elem/method_spec → method nodes.
    fn extract_go_interface_methods(&mut self, interface_type: Node<'t>, iface_row: u32, iface_name: &str) {
        self.stack.push(Scope { row: iface_row, kind: "interface", name: iface_name.to_string() });
        for i in 0..interface_type.named_child_count() {
            let Some(m) = interface_type.named_child(i) else { continue };
            if !matches!(m.kind(), "method_elem" | "method_spec") {
                continue;
            }
            let name_node = m.child_by_field_name("name").or_else(|| m.named_child(0));
            let Some(name_node) = name_node else { continue };
            let mname = self.text(name_node).to_string();
            if !mname.is_empty() {
                let signature = self.signature_of(m);
                self.create_node("method", &mname, m, Extra { signature, ..Extra::default() });
            }
        }
        self.stack.pop();
    }

    /// extractVariable's Go branch: var/const specs + short_var_declaration.
    fn extract_variable(&mut self, node: Node<'t>) {
        let docstring = preceding_docstring(node, self.src);
        let is_const_decl = node.kind() == "const_declaration";

        for spec in declaration_specs(node) {
            let mut var_row: Option<u32> = None;
            if let Some(name_node) = spec.named_child(0) {
                if name_node.kind() == "identifier" {
                    let name = self.text(name_node).to_string();
                    let value_node = if spec.named_child_count() > 1 {
                        spec.named_child(spec.named_child_count() - 1)
                    } else {
                        None
                    };
                    let signature = value_node.map(|v| util::init_signature(self.text(v)));
                    var_row = self.create_node(
                        if is_const_decl { "constant" } else { "variable" },
                        &name,
                        spec,
                        Extra { docstring: docstring.clone(), signature, ..Extra::default() },
                    );
                }
            }
            // Walk the initializer ATTRIBUTED to the declared symbol (#693).
            if let Some(value_field) = spec.child_by_field_name("value") {
                if let Some(row) = var_row {
                    let name = self.nodes_meta[row as usize].name.clone();
                    self.stack.push(Scope { row, kind: "variable", name });
                    self.visit_for_calls_and_structure(value_field);
                    self.stack.pop();
                } else {
                    self.visit_for_calls_and_structure(value_field);
                }
            }
        }

        if node.kind() == "short_var_declaration" {
            let left = node.child_by_field_name("left");
            let right = node.child_by_field_name("right");
            if let Some(left) = left {
                let identifiers: Vec<Node> = if left.kind() == "expression_list" {
                    (0..left.named_child_count())
                        .filter_map(|i| left.named_child(i))
                        .filter(|c| c.kind() == "identifier")
                        .collect()
                } else {
                    vec![left]
                };
                for id in identifiers {
                    let name = self.text(id).to_string();
                    let signature = right.map(|r| util::init_signature(self.text(r)));
                    self.create_node(
                        "variable",
                        &name,
                        node,
                        Extra { docstring: docstring.clone(), signature, ..Extra::default() },
                    );
                }
            }
        }
    }

    /// extractImport's Go branch: one import node + ref per import_spec.
    fn extract_import(&mut self, node: Node<'t>) {
        let parent = self.top_row();
        let imports_kind = edge_kind_index("imports").unwrap();
        let handle_spec = |w: &mut Self, spec: Node<'t>| {
            let lit = (0..spec.named_child_count())
                .filter_map(|i| spec.named_child(i))
                .find(|c| c.kind() == "interpreted_string_literal");
            let Some(lit) = lit else { return };
            let import_path: String = w
                .text(lit)
                .chars()
                .filter(|c| *c != '\'' && *c != '"')
                .collect();
            if import_path.is_empty() {
                return;
            }
            let signature = w.text(spec).trim().to_string();
            w.create_node(
                "import",
                &import_path,
                spec,
                Extra { signature: Some(signature), ..Extra::default() },
            );
            w.push_ref_at(parent, &import_path, imports_kind, spec);
            let local = w.import_local_name(spec, &import_path);
            w.emit_import_binding(&local, &import_path, spec);
        };

        let spec_list = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "import_spec_list");
        if let Some(list) = spec_list {
            for i in 0..list.named_child_count() {
                if let Some(spec) = list.named_child(i) {
                    if spec.kind() == "import_spec" {
                        handle_spec(self, spec);
                    }
                }
            }
        } else {
            let spec = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| c.kind() == "import_spec");
            if let Some(spec) = spec {
                handle_spec(self, spec);
            }
        }
    }

    // --- bindings (resolution-binding-model-plan.md, Phase 3: Go) --------------------

    /// The local name an import binds: the alias when written (`f "fmt"`, a
    /// dot or blank import as spelled), else the path's last segment — the
    /// resolver's long-standing reading (a package's declared name can differ).
    fn import_local_name(&self, spec: Node<'t>, import_path: &str) -> String {
        if let Some(alias) = spec.child_by_field_name("name") {
            return self.text(alias).to_string();
        }
        import_path.rsplit('/').next().unwrap_or(import_path).to_string()
    }

    enclosing_scope_impl!("file");

    push_binding_row_impl!();

    /// Go exports by case: a capitalized package-level name is `public`, a
    /// lowercase one is `storage = package` (visible to its package only).
    fn is_exported_name(name: &str) -> bool {
        name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
    }

    fn package_level_decl_row(&mut self, name: &str, node_idx: u32, line: u32) {
        let exported = Self::is_exported_name(name);
        let storage = if exported { None } else { Some("package") };
        self.push_binding_row(BINDING_DECL, name, node_idx, (1, self.line_count), line, None, exported, storage);
    }

    fn emit_decl_binding(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>) {
        if matches!(kind, "file" | "import") {
            return;
        }
        let line = self.line_of(node);
        match self.enclosing_scope() {
            None => self.package_level_decl_row(name, row, line),
            Some(scope) => self.push_binding_row(BINDING_LOCAL, name, row, scope, line, None, false, None),
        }
    }

    /// One `param` row per parameter and receiver name, scoped to the function.
    fn emit_param_bindings(&mut self, node: Node<'t>) {
        let scope = (self.line_of(node), node.end_position().row as u32 + 1);
        for field in ["receiver", "parameters"] {
            let Some(list) = node.child_by_field_name(field) else { continue };
            for i in 0..list.named_child_count() {
                let Some(p) = list.named_child(i) else { continue };
                if !matches!(p.kind(), "parameter_declaration" | "variadic_parameter_declaration") {
                    continue;
                }
                for j in 0..p.named_child_count() {
                    let Some(c) = p.named_child(j) else { continue };
                    if c.kind() == "identifier" {
                        let name = self.text(c).to_string();
                        let line = self.line_of(c);
                        self.push_binding_row(BINDING_PARAM, &name, NONE, scope, line, None, false, None);
                    }
                }
            }
        }
    }

    /// Nodeless `local` rows for names a function body binds: `x := …`,
    /// `var x T` and `for i, v := range …`.
    fn emit_local_rows(&mut self, node: Node<'t>) {
        let Some(scope) = self.enclosing_scope() else { return };
        self.emit_local_rows_scoped(node, scope);
    }

    fn emit_local_rows_scoped(&mut self, node: Node<'t>, scope: (u32, u32)) {
        let mut names: Vec<Node<'t>> = Vec::new();
        match node.kind() {
            "short_var_declaration" | "range_clause" => {
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "expression_list" {
                        names.extend((0..left.named_child_count()).filter_map(|i| left.named_child(i)).filter(|c| c.kind() == "identifier"));
                    } else if left.kind() == "identifier" {
                        names.push(left);
                    }
                }
            }
            "var_declaration" => {
                for spec in declaration_specs(node) {
                    names.extend((0..spec.named_child_count()).filter_map(|j| spec.named_child(j)).filter(|c| c.kind() == "identifier"));
                }
            }
            _ => {}
        }
        for n in names {
            let name = self.text(n).to_string();
            let line = self.line_of(n);
            self.push_binding_row(BINDING_LOCAL, &name, NONE, scope, line, None, false, None);
        }
    }

    fn emit_import_binding(&mut self, local: &str, import_path: &str, node: Node<'t>) {
        let line = self.line_of(node);
        let line_count = self.line_count;
        self.push_binding_row(BINDING_IMPORT, local, NONE, (1, line_count), line, Some((import_path, "*")), false, None);
    }

    /// extractCall — Go's generic-tail paths (selector_expression callees).
    fn extract_call(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let mut callee_name = String::new();

        if let Some(func) = func {
            if func.kind() == "selector_expression" {
                let property = func
                    .child_by_field_name("property")
                    .or_else(|| func.child_by_field_name("field"));
                if let Some(property) = property {
                    let method_name = self.text(property);
                    let receiver = func
                        .child_by_field_name("object")
                        .or_else(|| func.child_by_field_name("operand"))
                        .or_else(|| func.child_by_field_name("argument"))
                        .or_else(|| func.named_child(0));
                    if let Some(r) = receiver {
                        if is_literal_receiver(r.kind()) {
                            return;
                        }
                    }
                    if let Some(r) = receiver {
                        match r.kind() {
                            "identifier" | "simple_identifier" | "field_identifier" => {
                                let receiver_name = self.text(r);
                                if !matches!(receiver_name, "self" | "this" | "cls" | "super") {
                                    callee_name = format!("{receiver_name}.{method_name}");
                                } else {
                                    callee_name = method_name.to_string();
                                }
                            }
                            "call_expression" => {
                                // Bare package-level factory chain `New().Method()`
                                // re-encodes; instance chains keep the bare name.
                                let inner_fn = r.child_by_field_name("function");
                                let reencode =
                                    inner_fn.map(|f| f.kind() == "identifier").unwrap_or(false);
                                if reencode {
                                    let inner: String = self
                                        .text(inner_fn.unwrap())
                                        .replace("->", ".")
                                        .chars()
                                        .filter(|c| !c.is_whitespace())
                                        .collect();
                                    callee_name = format!("{inner}().{method_name}");
                                } else {
                                    callee_name = method_name.to_string();
                                }
                            }
                            "composite_literal" => {
                                if let Some(ty) = r.child_by_field_name("type") {
                                    callee_name = format!("{}.{}", self.text(ty), method_name);
                                }
                            }
                            "selector_expression" => {
                                // 2-hop field chain `t.conn.Exec` (#1276).
                                let chain: String = self
                                    .text(r)
                                    .chars()
                                    .filter(|c| !c.is_whitespace())
                                    .collect();
                                if go_two_hop_re().is_match(&chain) {
                                    callee_name = format!("{chain}.{method_name}");
                                } else {
                                    callee_name = method_name.to_string();
                                }
                            }
                            _ => {
                                callee_name = method_name.to_string();
                            }
                        }
                    } else {
                        callee_name = method_name.to_string();
                    }
                }
            } else {
                callee_name = self.text(func).to_string();
            }
        }

        if !callee_name.is_empty() {
            // `(*T)(x)` conversions normalize to `T`.
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
            let from = self.top_row();
            self.push_ref_at(from, &callee_name, edge_kind_index("calls").unwrap(), node);
        }
    }

    /// extractInstantiation's composite_literal branch: named struct types
    /// only; the package qualifier is KEPT.
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
        if !matches!(ctor.kind(), "type_identifier" | "qualified_type") {
            return;
        }
        let mut go_type = self.text(ctor).trim().to_string();
        if let Some(br) = go_type.find('[') {
            if br > 0 {
                go_type.truncate(br);
                go_type = go_type.trim().to_string();
            }
        }
        if !go_type.is_empty() {
            let from = self.top_row();
            self.push_ref_at(from, &go_type, edge_kind_index("instantiates").unwrap(), node);
        }
    }

    /// extractInheritance — the Go branches: interface embedding
    /// (constraint_elem) and struct embedding (field_declaration without a
    /// field_identifier), plus the field_declaration_list recursion.
    fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        stack_guard!();
        let extends_kind = edge_kind_index("extends").unwrap();
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                "constraint_elem" => {
                    let type_id = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .find(|c| c.kind() == "type_identifier");
                    if let Some(type_id) = type_id {
                        let name = self.text(type_id).to_string();
                        self.push_ref_at(class_row, &name, extends_kind, type_id);
                    }
                }
                "field_declaration" => {
                    let has_field_identifier = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .any(|c| c.kind() == "field_identifier");
                    if !has_field_identifier {
                        let type_id = (0..child.named_child_count())
                            .filter_map(|j| child.named_child(j))
                            .find(|c| c.kind() == "type_identifier");
                        if let Some(type_id) = type_id {
                            let name = self.text(type_id).to_string();
                            self.push_ref_at(class_row, &name, extends_kind, type_id);
                        }
                    }
                }
                "field_declaration_list" | "class_heritage" => {
                    self.extract_inheritance(child, class_row);
                }
                _ => {}
            }
        }
    }

    /// extractTypeAnnotations — Go's returnField is `result`.
    fn extract_type_annotations(&mut self, node: Node<'t>, from_row: u32) {
        if let Some(params) = node.child_by_field_name("parameters") {
            self.extract_type_refs_from_subtree(params, from_row);
        }
        if let Some(ret) = node.child_by_field_name("result") {
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

    // --- fn refs (GO_SPEC, with the literal_element/expression_list layers) --------

    fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        let (mode, field): (&str, &str) = match node.kind() {
            "argument_list" => ("args", ""),
            "assignment_statement" => ("rhs", "right"),
            "short_var_declaration" => ("rhs", "right"),
            "var_spec" => ("varinit", "value"),
            "keyed_element" => ("value", ""), // value = LAST named child
            "literal_value" => ("list", ""),
            _ => return,
        };
        if self.stack.is_empty() {
            return;
        }
        let from = self.top_row();

        let mut values: Vec<Node> = Vec::new();
        match mode {
            "args" | "list" => {
                for i in 0..node.named_child_count() {
                    if let Some(c) = node.named_child(i) {
                        values.push(c);
                    }
                }
            }
            "rhs" => {
                if let Some(rhs) = node.child_by_field_name(field) {
                    let lhs_text = node
                        .child_by_field_name("left")
                        .map(|l| self.text(l))
                        .unwrap_or("");
                    let lhs_last = util::lhs_last_name()
                        .captures(lhs_text)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str());
                    if !(lhs_last.is_some() && lhs_last == Some(self.text(rhs).trim())) {
                        values.push(rhs);
                    }
                }
            }
            "value" => {
                let v = node
                    .child_by_field_name("value")
                    .or_else(|| {
                        if node.named_child_count() > 0 {
                            node.named_child(node.named_child_count() - 1)
                        } else {
                            None
                        }
                    });
                if let Some(v) = v {
                    values.push(v);
                }
            }
            _ => {
                // varinit — Go var_spec names are plain identifiers (no
                // destructuring patterns to skip).
                if let Some(v) = node.child_by_field_name(field) {
                    values.push(v);
                }
            }
        }

        for v in values {
            self.normalize_fn_ref_value(v, from, 0);
        }
    }

    /// normalizeValue with GO_SPEC's transparent layers (literal_element,
    /// expression_list — both fan out to named children).
    fn normalize_fn_ref_value(&mut self, v: Node<'t>, from: u32, depth: u32) {
        stack_guard!();
        if depth > 4 {
            return;
        }
        match v.kind() {
            "identifier" => {
                let name = self.text(v).to_string();
                if name.is_empty() || is_stoplisted(&name) {
                    return;
                }
                let p = v.start_position();
                self.fn_ref_cands.push(Cand {
                    from,
                    name,
                    line: p.row as u32 + 1,
                    column_byte: v.start_byte(),
                    row: p.row,
                });
            }
            "literal_element" | "expression_list" => {
                for i in 0..v.named_child_count() {
                    if let Some(c) = v.named_child(i) {
                        self.normalize_fn_ref_value(c, from, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }

    fn scan_fn_ref_subtree(&mut self, node: Node<'t>, depth: u32) {
        stack_guard!();
        if depth > 12 {
            return;
        }
        if depth > 0
            && matches!(
                node.kind(),
                "function_declaration" | "arrow_function" | "function_expression"
                    | "lambda_literal" | "lambda_expression"
            )
        {
            return;
        }
        self.maybe_capture_fn_refs(node);
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.scan_fn_ref_subtree(c, depth + 1);
            }
        }
    }

    flush_fn_ref_candidates_impl!();

    // --- value refs -------------------------------------------------------------------

    fn flush_value_refs(&mut self, root: Node<'t>) {
        let scopes = std::mem::take(&mut self.value_scopes);
        let mut targets = std::mem::take(&mut self.fs_values);
        let counts = std::mem::take(&mut self.fs_value_counts);
        if std::env::var("CODEGRAPH_VALUE_REFS").as_deref() == Ok("0") {
            return;
        }
        if targets.is_empty() || scopes.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }

        // Shadow prune — Go declarator shapes: const_spec/var_spec (name =
        // first child) and short_var_declaration (left / expression_list).
        let mut decl_counts: HashMap<&str, u32> = HashMap::new();
        let bump = |decl_counts: &mut HashMap<&'t str, u32>, name_node: Option<Node<'t>>, src: &'t str, targets: &HashMap<String, u32>| {
            if let Some(n) = name_node {
                if matches!(n.kind(), "identifier" | "simple_identifier") {
                    let nm = &src[n.byte_range()];
                    if targets.contains_key(nm) {
                        *decl_counts.entry(nm).or_insert(0) += 1;
                    }
                }
            }
        };
        let mut dstack: Vec<Node> = vec![root];
        let mut dvisited = 0usize;
        while let Some(n) = dstack.pop() {
            if dvisited >= crate::walker::MAX_VALUE_REF_NODES {
                break;
            }
            dvisited += 1;
            match n.kind() {
                "const_spec" | "var_spec" => bump(&mut decl_counts, n.named_child(0), self.src, &targets),
                "short_var_declaration" => {
                    let left = n
                        .child_by_field_name("left")
                        .or_else(|| n.child_by_field_name("pattern"))
                        .or_else(|| n.named_child(0));
                    if let Some(left) = left {
                        if left.kind() == "identifier" {
                            bump(&mut decl_counts, Some(left), self.src, &targets);
                        } else {
                            for i in 0..left.named_child_count() {
                                bump(&mut decl_counts, left.named_child(i), self.src, &targets);
                            }
                        }
                    }
                }
                _ => {}
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i) {
                    dstack.push(c);
                }
            }
        }
        let shadowed: Vec<String> = decl_counts
            .iter()
            .filter(|(nm, c)| **c > counts.get(**nm).copied().unwrap_or(1))
            .map(|(nm, _)| nm.to_string())
            .collect();
        for nm in shadowed {
            targets.remove(&nm);
        }
        if targets.is_empty() {
            return;
        }

        crate::walker::emit_value_refs(self.src, &self.node_ids, &mut self.arena, &mut self.tables, &scopes, &targets);
    }
}




