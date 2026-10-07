//! Go extraction — a faithful Rust port of `TreeSitterExtractor`'s Go paths
//! (src/extraction/tree-sitter.ts) plus languages/go.ts.
//!
//! Go's shape quirks, mirrored exactly: methods are top-level with a receiver
//! (qualifiedName override `Recv::name` + a contains edge to the FIRST
//! earlier-in-file struct of that name), structs/interfaces arrive as
//! `type_spec` and classify via the inner type node (struct and interface
//! embedding → extends; interface method_elems become method nodes), composite literals
//! (`pkga.Widget{}`) keep their package qualifier as `instantiates` refs,
//! top-level var/const specs walk their initializers ATTRIBUTED to the
//! declared symbol (#693), 2-hop field chains (`t.conn.Exec`) keep the chain
//! (#1276), and `New().Method()` re-encodes as `New().Method` (#645/#608)
//! only for bare-identifier factories. Files with parse errors are walked like any other (tree-sitter's recovery is canonical; buffers::parse_collapse_warning reports a collapsed parse).

mod bindings;
mod refs;
use crate::buffers::{
    node_kind_index, Arena, BoolFlags, EmitOut, NodeRow,
    RefRow, Tables, BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, FLAG_IS_EXPORTED, NONE, NONE_STR,
};
use crate::walker::named_kids;
use crate::walker::{Scope, ValueScope, Cand, scope_qualified_name};
use crate::textutil::{is_builtin_type, is_literal_receiver};
use crate::docstring::preceding_docstring_skipping_trailing;
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
            specs.extend(named_kids(child)
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
    node_id_allocator: ids::NodeIdAllocator,
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

    let assumed_imports = w.assumed_imports(tree.root_node());
    w.imported_names.extend(assumed_imports.iter().map(|(name, _, _)| name.clone()));
    w.visit_node(tree.root_node());
    for (name, path, spec) in assumed_imports { w.emit_import_binding(&name, &path, spec); }
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
            node_id_allocator: ids::NodeIdAllocator::default(),
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
        let column = self.col_of(node);
        let id = self.node_id_allocator.generate(self.file_path, kind, name, start_line, column);
        let end_line = node.end_position().row as u32 + 1;

        let qualified = extra.qualified_name.unwrap_or_else(|| scope_qualified_name(&self.stack, name));

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
        self.tables.push_contains(parent_row, row);

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
            let first = named_kids(result)
                .find(|c| c.kind() == "parameter_declaration")?;
            result = first.child_by_field_name("type")?;
        }
        if result.kind() == "pointer_type" {
            result = named_kids(result).find(|c| {
                matches!(c.kind(), "type_identifier" | "qualified_type" | "generic_type")
            })?;
        }
        if !matches!(result.kind(), "type_identifier" | "qualified_type" | "generic_type") {
            return None;
        }
        let text = self.text(result).trim();
        let base = text.split_once('[').map_or(text, |(base, _)| base).trim();
        let valid = base.split('.').all(|part| {
            !part.is_empty() && crate::textutil::ascii_ident_re().is_match(part)
        });
        (valid && base.split('.').count() <= 2).then(|| base.to_string())
    }

    /// goExtractor.getReceiverType: the regex over the receiver's text.
    fn receiver_type_of(&self, node: Node) -> Option<String> {
        let receiver = node.child_by_field_name("receiver")?;
        let text = self.text(receiver);
        receiver_re().captures(text).map(|c| c[1].to_string())
    }

    // --- visitNode ------------------------------------------------------------

    fn declaration_wrapper(&self, node: Node<'t>) -> Option<Node<'t>> {
        let parent = node.parent()?;
        if parent.kind() != "type_declaration" || parent.named_child(0)? != node {
            return None;
        }
        let specs = (0..parent.named_child_count())
            .filter_map(|i| parent.named_child(i))
            .filter(|c| matches!(c.kind(), "type_spec" | "type_alias"))
            .count();
        (specs == 1).then_some(parent)
    }

    fn docstring_of(&self, node: Node<'t>) -> Option<String> {
        preceding_docstring_skipping_trailing(self.declaration_wrapper(node).unwrap_or(node), self.src)
    }

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
        } else if matches!(kind, "type_spec" | "type_alias") {
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
            for c in named_kids(node) {
                self.visit_node(c);
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
        } else if kind == "func_literal" {
            // A literal's parameters shadow the enclosing function's names
            // for its body, as a named function's do.
            self.emit_param_bindings(node);
        }

        if kind == "function_declaration" {
            let name = self.extract_name(node);
            if name != "<anonymous>" {
                self.extract_function(node);
                return;
            }
        }

        for c in named_kids(node) {
            self.visit_for_calls_and_structure(c);
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
            docstring: self.docstring_of(node),
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
            docstring: self.docstring_of(node),
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
                    self.tables.push_contains(owner_row, row);
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

    /// extractTypeAlias for Go: type_spec (`type A B`) and type_alias
    /// (`type A = B`) → struct / interface / plain alias.
    fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let docstring = self.docstring_of(node);
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
                for c in named_kids(body) {
                    self.visit_node(c);
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

        let row = self.create_node(
            "type_alias",
            &name,
            node,
            Extra { docstring, is_exported, ..Extra::default() },
        );
        // (go has no `value` field — no TS-style type-ref walk or member
        // extraction.) An alias references what its `type` field names; a
        // defined type (`type_spec`) declares a type of its own.
        if let Some(row) = row {
            let references = crate::buffers::EDGE_REFERENCES;
            for ty in self.alias_type_names(node) {
                let text = self.text(ty).to_string();
                self.push_ref_at(row, &text, references, ty);
            }
        }
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
                self.create_node("method", &mname, m, Extra { signature, return_type: self.return_type_of(m), ..Extra::default() });
            }
        }
        self.stack.pop();
    }

    /// extractVariable's Go branch: var/const specs + short_var_declaration.
    fn extract_variable(&mut self, node: Node<'t>) {
        let docstring = self.docstring_of(node);
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
                    named_kids(left)
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
        let imports_kind = crate::buffers::EDGE_IMPORTS;
        let handle_spec = |w: &mut Self, spec: Node<'t>| {
            let lit = named_kids(spec)
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
            w.imported_names.insert(local);
        };

        let spec_list = named_kids(node)
            .find(|c| c.kind() == "import_spec_list");
        if let Some(list) = spec_list {
            for spec in named_kids(list) {
                if spec.kind() == "import_spec" {
                    handle_spec(self, spec);
                }
            }
        } else {
            let spec = named_kids(node)
                .find(|c| c.kind() == "import_spec");
            if let Some(spec) = spec {
                handle_spec(self, spec);
            }
        }
    }

    /// Keep the written/path name and add the conventional package name only
    /// when no other import in the file binds or assumes that name.
    fn assumed_imports(&self, root: Node<'t>) -> Vec<(String, String, Node<'t>)> {
        let mut imports = Vec::new();
        // Imports are top-level declarations. Do not revisit unrelated deep
        // function bodies after the guarded walker has deferred the file.
        let mut pending: Vec<_> = named_kids(root).filter(|n| n.kind() == "import_declaration").collect();
        while let Some(node) = pending.pop() {
            if node.kind() == "import_spec" { imports.push(node); }
            else { pending.extend(named_kids(node)); }
        }
        imports.sort_by_key(|node| node.start_byte());
        let mut bound = HashSet::new();
        let mut order = Vec::new();
        let mut assumed: HashMap<String, Vec<(String, Node<'t>)>> = HashMap::new();
        for spec in imports {
            let Some(lit) = spec.child_by_field_name("path")
                .or_else(|| named_kids(spec).find(|c| c.kind() == "interpreted_string_literal")) else { continue };
            let path = self.text(lit).trim_matches('"').to_string();
            let local = self.import_local_name(spec, &path);
            bound.insert(local);
            if spec.child_by_field_name("name").is_some() { continue; }
            let mut parts = path.rsplit('/');
            let last = parts.next().unwrap_or("");
            let base = if last.starts_with('v') && last.len() > 1
                && last[1..].bytes().all(|b| b.is_ascii_digit()) {
                parts.next().unwrap_or(last)
            } else { last };
            let base = base.strip_prefix("go-").unwrap_or(base);
            let name: String = base.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
                let candidates = assumed.entry(name.clone()).or_default();
                if candidates.is_empty() { order.push(name); }
                candidates.push((path, spec));
            }
        }
        order.into_iter().filter_map(|name| {
            let candidates = &assumed[&name];
            if bound.contains(&name) || candidates.len() != 1 { return None; }
            let (path, spec) = &candidates[0];
            Some((name, path.clone(), *spec))
        }).collect()
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

    /// extractCall — Go's generic-tail paths (selector_expression callees).
    fn extract_call(&mut self, node: Node<'t>) {
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
                                // A package-level factory chain, bare `New().Method()`
                                // or through an import `pkg.New().Method()`,
                                // re-encodes; instance chains keep the bare name.
                                let inner_fn = r.child_by_field_name("function");
                                let reencode = inner_fn.is_some_and(|f| match f.kind() {
                                    "identifier" => true,
                                    "selector_expression" => f
                                        .child_by_field_name("operand")
                                        .is_some_and(|o| {
                                            o.kind() == "identifier"
                                                && self.imported_names.contains(self.text(o))
                                        }),
                                    _ => false,
                                });
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
            self.push_ref_at(from, &callee_name, crate::buffers::EDGE_CALLS, node);
        }
    }

    /// extractInstantiation's composite_literal branch: named struct types
    /// only; the package qualifier is KEPT.
    fn extract_instantiation(&mut self, node: Node<'t>) {
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
            self.push_ref_at(from, &go_type, crate::buffers::EDGE_INSTANTIATES, node);
        }
    }

    /// goEmbeddedTypeName (languages/go.ts): the name node of the type an
    /// embedding names — `T`, `*T` (the `*` is a sibling token), `pkg.T` and
    /// `T[X]` all embed `T`. A qualified type yields its name; resolution reads
    /// the package back from the source at that position. None for any other
    /// type and for a predeclared one.
    fn embedded_type_name(&self, ty: Option<Node<'t>>) -> Option<Node<'t>> {
        let mut ty = ty?;
        if ty.kind() == "generic_type" {
            ty = ty.child_by_field_name("type")?;
        }
        match ty.kind() {
            "qualified_type" => ty.child_by_field_name("name"),
            "type_identifier" if !is_go_predeclared_type(self.text(ty)) => Some(ty),
            _ => None,
        }
    }

    /// goAliasTypeNames (languages/go.ts): the name nodes of the types an
    /// alias's `type` field names, in source order, but its own type
    /// parameters and the predeclared types. Generic aliases parse as a
    /// `type_spec` with an error child containing `=` in this grammar.
    fn alias_type_names(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut names = Vec::new();
        let Some(ty) = node.child_by_field_name("type") else { return names };
        let own = node.child_by_field_name("name").map(|n| self.text(n));
        let mut params: HashSet<&str> = HashSet::new();
        if let Some(list) = node.child_by_field_name("type_parameters") {
            for decl in (0..list.named_child_count()).filter_map(|i| list.named_child(i)) {
                for c in (0..decl.named_child_count()).filter_map(|j| decl.named_child(j)) {
                    if c.kind() == "identifier" {
                        params.insert(self.text(c));
                    }
                }
            }
        }
        self.collect_alias_type_names(ty, &params, own, false, &mut names);
        names
    }

    fn collect_alias_type_names(
        &self,
        node: Node<'t>,
        params: &HashSet<&str>,
        own: Option<&str>,
        qualified: bool,
        out: &mut Vec<Node<'t>>,
    ) {
        stack_guard!();
        if node.kind() == "type_identifier" {
            let text = self.text(node);
            if !is_go_predeclared_type(text) && (qualified || (!params.contains(text) && own != Some(text))) {
                out.push(node);
            }
            return;
        }
        let qualified = node.kind() == "qualified_type";
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.collect_alias_type_names(c, params, own, qualified, out);
            }
        }
    }

    /// extractInheritance — the Go branches: interface embedding (a type_elem
    /// holding one named type; a union, `~T` or basic type is a constraint)
    /// and struct embedding (field_declaration without a field_identifier),
    /// plus the field_declaration_list recursion.
    fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        stack_guard!();
        let extends_kind = crate::buffers::EDGE_EXTENDS;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                "type_elem" => {
                    let mut terms = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .filter(|c| c.kind() != "comment");
                    let only = match (terms.next(), terms.next()) {
                        (Some(term), None) => Some(term),
                        _ => None,
                    };
                    if let Some(type_id) = self.embedded_type_name(only) {
                        let name = self.text(type_id).to_string();
                        self.push_ref_at(class_row, &name, extends_kind, type_id);
                    }
                }
                "field_declaration" => {
                    let has_field_identifier = named_kids(child)
                        .any(|c| c.kind() == "field_identifier");
                    if !has_field_identifier {
                        if let Some(type_id) = self.embedded_type_name(child.child_by_field_name("type")) {
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
        let type_annotation = named_kids(node)
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = type_annotation {
            self.extract_type_refs_from_subtree(ta, from_row);
        }
    }

    type_refs_from_subtree_impl!();

    // --- fn refs (GO_SPEC, with the literal_element/expression_list layers) --------

    flush_fn_ref_candidates_impl!();

    // --- value refs -------------------------------------------------------------------

}




/// GO_PREDECLARED_TYPES (languages/go.ts): never a node in the graph, and as
/// the lone term of an interface a basic type is a constraint, not an embedding.
fn is_go_predeclared_type(name: &str) -> bool {
    matches!(
        name,
        "any" | "bool" | "byte" | "comparable" | "complex64" | "complex128" | "error" | "float32"
            | "float64" | "int" | "int8" | "int16" | "int32" | "int64" | "rune" | "string" | "uint"
            | "uint8" | "uint16" | "uint32" | "uint64" | "uintptr"
    )
}
