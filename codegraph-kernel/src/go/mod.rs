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
    RefRow, Tables, BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, FLAG_IS_EXPORTED, FUNCTION_REF_CODE, NONE, NONE_STR,
};
use crate::walker::named_kids;
use crate::walker::{Scope, ValueScope, scope_qualified_name};
use crate::textutil::{is_builtin_type, is_literal_receiver};
use crate::docstring::preceding_go_doc_comment;
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


struct Cand<'t> {
    from: u32,
    name: String,
    line: u32,
    column_byte: usize,
    row: usize,
    /// The value container the candidate came from: the flush reads its
    /// scope from there when the function shadows the name (#2596).
    node: Node<'t>,
}

impl<'t> Cand<'t> {
    /// Cand::at (walker.rs) plus the value container (#2596).
    fn at(from: u32, name: impl Into<String>, node: Node<'t>, container: Node<'t>) -> Option<Cand<'t>> {
        let name = name.into();
        if name.is_empty() || crate::textutil::is_stoplisted(&name) {
            return None;
        }
        let p = node.start_position();
        Some(Cand { from, name, line: p.row as u32 + 1, column_byte: node.start_byte(), row: p.row, node: container })
    }

    /// Cand::member (walker.rs) plus the value container (#2596): a member
    /// value that keeps its receiver path, anchored at the member name.
    fn member(from: u32, path: &str, member: Node<'t>, container: Node<'t>) -> Option<Cand<'t>> {
        let path = path.split('.').map(str::trim).collect::<Vec<_>>().join(".");
        let named = path.contains('.')
            && path.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().enumerate().all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
            });
        if !named {
            return None;
        }
        let p = member.start_position();
        Some(Cand { from, name: path, line: p.row as u32 + 1, column_byte: member.start_byte(), row: p.row, node: container })
    }
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
    /// Names an import is given by an alias written in the file (`v3 "…/client/v3"`):
    /// Go binds a name to one import, so an unaliased import whose last path
    /// element is such a name (`"github.com/cheggaaa/pb/v3"`) does not bind it.
    explicit_import_names: HashSet<String>,
    fn_ref_cands: Vec<Cand<'t>>,
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

    w.explicit_import_names = w.explicit_import_names(tree.root_node());
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
            explicit_import_names: HashSet::new(),
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

    /// goExtractor.isExported: uppercase first letter of the name field (a
    /// var_spec / const_spec's first name included).
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

    /// docstringFor (tree-sitter.ts) — the preceding comment run, looked up
    /// from the declaration wrapper when there is one, without the comments
    /// that trail the code above it (docstringSkipsTrailingComments) or that
    /// don't end on the line right above it, past a blank line
    /// (docstringSkipsDetachedComments), and without directives like
    /// `//go:embed` in its text (docstringSkipsDirectives).
    fn docstring_of(&self, node: Node<'t>) -> Option<String> {
        preceding_go_doc_comment(self.declaration_wrapper(node).unwrap_or(node), self.src)
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
            let walked = self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0, &walked);
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
            // methodsAreTopLevel: a Go method is a top-level declaration, so
            // extractMethod gives it the function's exportedness (name case).
            is_exported: Some(self.is_exported(node)),
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
                // The types its fields are declared with are the struct's
                // references: Go fields mint no nodes.
                let references = crate::buffers::EDGE_REFERENCES;
                for ty in self.struct_field_type_names(node) {
                    let text = self.text(ty).to_string();
                    self.push_ref_at(row, &text, references, ty);
                }
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
                self.extract_go_interface_methods(type_child, row, &name, node);
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

    /// extractGoInterfaceMethods: method_elem/method_spec → method nodes, each
    /// referencing the types its signature names (`decl` declares the
    /// interface).
    fn extract_go_interface_methods(&mut self, interface_type: Node<'t>, iface_row: u32, iface_name: &str, decl: Node<'t>) {
        self.stack.push(Scope { row: iface_row, kind: "interface", name: iface_name.to_string() });
        let references = crate::buffers::EDGE_REFERENCES;
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
                let Some(row) = self.create_node("method", &mname, m, Extra { signature, return_type: self.return_type_of(m), ..Extra::default() }) else { continue };
                for ty in self.interface_method_type_names(m, decl) {
                    let text = self.text(ty).to_string();
                    self.push_ref_at(row, &text, references, ty);
                }
            }
        }
        self.stack.pop();
    }

    /// goInterfaceMethodTypeNames (languages/go.ts): the name nodes of the
    /// types an interface method's parameters and results name, but the
    /// predeclared types and the interface's own type parameters.
    fn interface_method_type_names(&self, method: Node<'t>, decl: Node<'t>) -> Vec<Node<'t>> {
        let mut names = Vec::new();
        let params = self.type_parameter_names(decl);
        for field in ["parameters", "result"] {
            if let Some(part) = method.child_by_field_name(field) {
                self.collect_type_names(part, &params, false, &mut names);
            }
        }
        names
    }

    /// extractVariable's Go branch: var/const specs + short_var_declaration.
    fn extract_variable(&mut self, node: Node<'t>) -> Vec<usize> {
        let mut walked = Vec::new();
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
                    // A member of a `const ( … )` or `var ( … )` group has its
                    // own doc comment beside it in the parentheses; one without
                    // keeps the group's.
                    let doc = self.docstring_of(spec).or_else(|| docstring.clone());
                    // Exported by its own name's case, read from the spec (the
                    // declaration around it has no name).
                    let is_exported = Some(self.is_exported(spec));
                    var_row = self.create_node(
                        if is_const_decl { "constant" } else { "variable" },
                        &name,
                        spec,
                        Extra { docstring: doc, signature, is_exported, ..Extra::default() },
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
                    walked.push(value_field.id());
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
        walked
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
            let aliased = spec.child_by_field_name("name").is_some_and(|alias| w.text(alias) != ".");
            if aliased || !w.explicit_import_names.contains(&local) {
                w.emit_import_binding(&local, &import_path, spec);
            }
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

    /// The import specs of the file, in source order. Imports are top-level
    /// declarations; the deferred function bodies are not revisited.
    fn import_specs(&self, root: Node<'t>) -> Vec<Node<'t>> {
        let mut imports = Vec::new();
        let mut pending: Vec<_> = named_kids(root).filter(|n| n.kind() == "import_declaration").collect();
        while let Some(node) = pending.pop() {
            if node.kind() == "import_spec" { imports.push(node); }
            else { pending.extend(named_kids(node)); }
        }
        imports.sort_by_key(|node| node.start_byte());
        imports
    }

    /// goExplicitImportNames (import-resolver.ts extractGoImports): the names
    /// the file's imports are given by an alias, but a dot or blank import.
    fn explicit_import_names(&self, root: Node<'t>) -> HashSet<String> {
        self.import_specs(root)
            .into_iter()
            .filter_map(|spec| spec.child_by_field_name("name"))
            .map(|alias| self.text(alias).to_string())
            .filter(|name| name != "." && name != "_")
            .collect()
    }

    /// Keep the written/path name and add the conventional package name only
    /// when no other import in the file binds or assumes that name.
    fn assumed_imports(&self, root: Node<'t>) -> Vec<(String, String, Node<'t>)> {
        let imports = self.import_specs(root);
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
    /// type and for a predeclared one but `error`, an interface with a method.
    fn embedded_type_name(&self, ty: Option<Node<'t>>) -> Option<Node<'t>> {
        let mut ty = ty?;
        if ty.kind() == "generic_type" {
            ty = ty.child_by_field_name("type")?;
        }
        match ty.kind() {
            "qualified_type" => ty.child_by_field_name("name"),
            "type_identifier" => {
                let name = self.text(ty);
                (name == "error" || !is_go_predeclared_type(name)).then_some(ty)
            }
            _ => None,
        }
    }

    /// goDeclaredNames (languages/go.ts): what a type name written bare
    /// inside a type declaration can mean without naming another type — the
    /// declaration's own type parameters, and its own name, which in a
    /// recursive type is the declaration itself.
    fn declared_names(&self, decl: Node<'t>) -> HashSet<&'t str> {
        let mut names = self.type_parameter_names(decl);
        if let Some(name) = decl.child_by_field_name("name") {
            names.insert(self.text(name));
        }
        names
    }

    /// goTypeParameterNames (languages/go.ts): the type parameters a type
    /// declaration declares.
    fn type_parameter_names(&self, decl: Node<'t>) -> HashSet<&'t str> {
        let mut names = HashSet::new();
        if let Some(list) = decl.child_by_field_name("type_parameters") {
            for param in (0..list.named_child_count()).filter_map(|i| list.named_child(i)) {
                for c in (0..param.named_child_count()).filter_map(|j| param.named_child(j)) {
                    if c.kind() == "identifier" {
                        names.insert(self.text(c));
                    }
                }
            }
        }
        names
    }

    /// goAliasTypeNames (languages/go.ts): the name nodes of the types an
    /// alias's `type` field names, in source order, but its own type
    /// parameters and the predeclared types. Generic aliases parse as a
    /// `type_spec` with an error child containing `=` in this grammar.
    fn alias_type_names(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut names = Vec::new();
        if let Some(ty) = node.child_by_field_name("type") {
            self.collect_type_names(ty, &self.declared_names(node), false, &mut names);
        }
        names
    }

    /// goStructFieldTypeNames (languages/go.ts): the name nodes of the types a
    /// struct's fields are declared with, in source order, skipped as in
    /// alias_type_names. An embedded field's type is a supertype
    /// (embedded_type_name), but the types it is instantiated with are not
    /// (`Pod` in client-go's `listers.ResourceIndexer[*v1.Pod]`).
    fn struct_field_type_names(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut names = Vec::new();
        let fields = node.child_by_field_name("type").and_then(|ty| {
            (0..ty.named_child_count())
                .filter_map(|i| ty.named_child(i))
                .find(|c| c.kind() == "field_declaration_list")
        });
        let Some(fields) = fields else { return names };
        let declared = self.declared_names(node);
        for field in (0..fields.named_child_count()).filter_map(|i| fields.named_child(i)) {
            if field.kind() != "field_declaration" {
                continue;
            }
            let mut ty = field.child_by_field_name("type");
            // Embedded: no field name (extract_inheritance's test).
            let embedded = !(0..field.named_child_count())
                .filter_map(|j| field.named_child(j))
                .any(|c| c.kind() == "field_identifier");
            if embedded {
                ty = ty
                    .filter(|t| t.kind() == "generic_type")
                    .and_then(|t| t.child_by_field_name("type_arguments"));
            }
            if let Some(ty) = ty {
                self.collect_type_names(ty, &declared, false, &mut names);
            }
        }
        names
    }

    /// collectGoTypeNames (languages/go.ts). `qualified`: the name of a
    /// `pkg.Name`, which is that package's whatever this one declares.
    fn collect_type_names(&self, node: Node<'t>, declared: &HashSet<&str>, qualified: bool, out: &mut Vec<Node<'t>>) {
        stack_guard!();
        if node.kind() == "type_identifier" {
            let text = self.text(node);
            if !is_go_predeclared_type(text) && (qualified || !declared.contains(text)) {
                out.push(node);
            }
            return;
        }
        let qualified = node.kind() == "qualified_type";
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.collect_type_names(c, declared, qualified, out);
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

    fn flush_fn_ref_candidates(&mut self) {
        let cands = std::mem::take(&mut self.fn_ref_cands);
        if cands.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }
        let survivors: Vec<Cand<'t>> = cands
            .into_iter()
            .filter(|c| {
                c.name.starts_with("this.")
                    || c.name.contains('.')
                    || c.name.contains("::")
                    || self.defined_fn_names.contains(&c.name)
                    || self.imported_names.contains(&c.name)
            })
            .collect();
        // One reference per function and name, at its first use; one per
        // scope where the function binds the name itself (#2596). Mirrors
        // flushFnRefCandidates / goScopedFnRefKeys.
        let scopes = go_scoped_fn_ref_scopes(&survivors, &self.node_ids, self.src);
        let mut seen: HashSet<(String, String, Option<usize>)> = HashSet::new();
        for (i, c) in survivors.into_iter().enumerate() {
            let scope = scopes.get(&i).copied();
            if !seen.insert((self.node_ids[c.from as usize].clone(), c.name.clone(), scope)) {
                continue;
            }
            let column = self.cols.col(self.src, c.row, c.column_byte);
            let name_ref = self.arena.put(&c.name);
            self.tables.push_ref(&RefRow {
                from_idx: c.from,
                kind: FUNCTION_REF_CODE,
                line: c.line,
                column,
                reference_name: name_ref,
                candidates: NONE_STR,
                from_id_str: NONE_STR,
            });
        }
    }


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

// --- a local that shadows the function a value names (#2596) ---------------------

/// Go nodes that open a scope a local can be declared in: a block, the
/// implicit block of an `if` / `for` / `switch` / `select` statement, and each
/// clause of a `switch` or `select`. Mirrors GO_SCOPE_TYPES (function-ref.ts).
const GO_SCOPE_TYPES: [&str; 10] = [
    "block",
    "if_statement",
    "for_statement",
    "expression_switch_statement",
    "type_switch_statement",
    "select_statement",
    "expression_case",
    "default_case",
    "type_case",
    "communication_case",
];

/// Where a function-value candidate sits: the innermost scope node's id
/// (usize::MAX when there is none) and the unit it is written in: the
/// function or method declaration, or for a func literal at file scope the
/// outermost literal, else None. Mirrors goFnRefSite.
fn go_fn_ref_site<'t>(node: Node<'t>) -> (usize, Option<Node<'t>>) {
    let mut scope = usize::MAX;
    let mut literal: Option<Node<'t>> = None;
    let mut cur = Some(node);
    while let Some(n) = cur {
        if scope == usize::MAX && GO_SCOPE_TYPES.contains(&n.kind()) {
            scope = n.id();
        }
        if matches!(n.kind(), "function_declaration" | "method_declaration") {
            return (scope, Some(n));
        }
        if n.kind() == "func_literal" {
            literal = Some(n);
        }
        cur = n.parent();
    }
    (scope, literal)
}

/// The names among `names` that a function binds itself, as (unit id, name):
/// its func literals' parameters, `:=`, `var` and `const`, range and
/// type-switch variables, and a select case's received value. A name the
/// function's own receiver, parameters or results bind is left out: those
/// cover its whole body. Mirrors goLocalBindings.
fn go_local_bindings<'t>(units: &[Node<'t>], names: &HashSet<&str>, src: &str) -> HashSet<(usize, String)> {
    let mut out: HashSet<(usize, String)> = HashSet::new();
    for unit in units {
        let mut bind = |id: Option<Node<'t>>| {
            if let Some(id) = id {
                if id.kind() == "identifier" {
                    let name = &src[id.start_byte()..id.end_byte()];
                    if names.contains(name) {
                        out.insert((unit.id(), name.to_string()));
                    }
                }
            }
        };
        let declares = |n: Node<'t>| {
            (0..n.child_count()).any(|i| n.child(i).is_some_and(|c| !c.is_named() && c.kind() == ":="))
        };
        let mut stack: Vec<Node<'t>> = vec![*unit];
        while let Some(n) = stack.pop() {
            let list = match n.kind() {
                "short_var_declaration" => n.child_by_field_name("left"),
                "range_clause" | "receive_statement" if declares(n) => n.child_by_field_name("left"),
                "type_switch_statement" => n.child_by_field_name("alias"),
                _ => None,
            };
            if let Some(list) = list {
                for i in 0..list.named_child_count() {
                    bind(list.named_child(i));
                }
            }
            if matches!(n.kind(), "var_spec" | "const_spec" | "parameter_declaration" | "variadic_parameter_declaration") {
                let mut cursor = n.walk();
                for c in n.children_by_field_name("name", &mut cursor) {
                    bind(Some(c));
                }
            }
            for i in (0..n.named_child_count()).rev() {
                if let Some(c) = n.named_child(i) {
                    stack.push(c);
                }
            }
        }
        for field in ["receiver", "parameters", "result"] {
            let Some(list) = unit.child_by_field_name(field) else { continue };
            if list.kind() != "parameter_list" {
                continue;
            }
            for i in 0..list.named_child_count() {
                let Some(p) = list.named_child(i) else { continue };
                if !matches!(p.kind(), "parameter_declaration" | "variadic_parameter_declaration") {
                    continue;
                }
                let mut cursor = p.walk();
                for c in p.children_by_field_name("name", &mut cursor) {
                    if c.kind() == "identifier" {
                        out.remove(&(unit.id(), src[c.start_byte()..c.end_byte()].to_string()));
                    }
                }
            }
        }
    }
    out
}

/// The candidates, by index, that get one reference per scope instead of
/// one per function, each with the id of the scope it sits in: names a
/// function uses at least twice and binds itself. Only those candidates'
/// positions are read from the tree. Mirrors goScopedFnRefScopes.
fn go_scoped_fn_ref_scopes<'t>(cands: &[Cand<'t>], node_ids: &[String], src: &str) -> HashMap<usize, usize> {
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        if c.name.contains('.') {
            continue;
        }
        groups.entry((node_ids[c.from as usize].as_str(), c.name.as_str())).or_default().push(i);
    }
    let mut sites: Vec<(usize, usize, Node<'t>)> = Vec::new();
    let mut units: Vec<Node<'t>> = Vec::new();
    let mut unit_ids: HashSet<usize> = HashSet::new();
    let mut names: HashSet<&str> = HashSet::new();
    for members in groups.values() {
        if members.len() < 2 {
            continue;
        }
        for &i in members {
            let (scope, unit) = go_fn_ref_site(cands[i].node);
            let Some(unit) = unit else { continue };
            sites.push((i, scope, unit));
            if unit_ids.insert(unit.id()) {
                units.push(unit);
            }
            names.insert(cands[i].name.as_str());
        }
    }
    let mut out: HashMap<usize, usize> = HashMap::new();
    if units.is_empty() {
        return out;
    }
    let bound = go_local_bindings(&units, &names, src);
    for (i, scope, unit) in sites {
        if bound.contains(&(unit.id(), cands[i].name.clone())) {
            out.insert(i, scope);
        }
    }
    out
}
