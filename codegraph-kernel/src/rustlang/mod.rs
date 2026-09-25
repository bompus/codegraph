//! Rust-language extraction — a faithful port of `TreeSitterExtractor`'s rust
//! paths (src/extraction/tree-sitter.ts) plus languages/rust.ts. ("rustlang"
//! because `rust` alone collides with the kernel's own implementation
//! language.) Survey artifact: docs/design/rust-lang-kernel-port-checklist.md.
//!
//! Rust's shape quirks, mirrored exactly (bug-for-bug, all verified against
//! the TS reference):
//! - `isAsync` is dead code upstream: it scans DIRECT children for an `async`
//!   token, but the grammar nests it inside `function_modifiers` — every rust
//!   fn/method carries isAsync **false** (present-false, never absent).
//! - impl blocks push NO scope: members re-dispatch at file scope, so an impl
//!   associated `const` becomes a FILE-level `variable`, and the method↔owner
//!   `contains` edge is a source-order name scan (an impl ABOVE its struct
//!   gets no edge). The receiver (method QN prefix, `contains` owner,
//!   `implements` source) is the impl_item's `type` field via
//!   impl_type_name — both sides moved to the grammar's trait:/type: fields
//!   together in #1588 (the earlier positional scan qualified every
//!   parameterized impl's methods by the TRAIT).
//! - `const_item`/`static_item` ride the generic extractVariable fallback:
//!   kind is always `variable`, no signature, and EVERY direct `identifier`
//!   child mints a node (`const MAX: u32 = OTHER;` → two nodes, `MAX` + the
//!   phantom `OTHER`). Top-level initializer values are never body-walked.
//! - `mod_item` mints no module node and adds no QN prefix.
//! - Chained-call re-encode is scoped_identifier-gated (`Foo::new().bar()` →
//!   `Foo::new().bar`); a call through a field of the enclosing type keeps
//!   the owner-field shape (`self.inner.run()` → `self.inner.run`, #1585);
//!   instance chains, parens, `.await`, deeper/non-self field chains, and
//!   bare `self` receivers all collapse to the bare method name (`self` is
//!   node kind `self`, not `identifier`, so it dodges SKIP_RECEIVERS by
//!   falling through). Turbofish callees keep the raw `helper::<T>` text.
//! - `use` emits an import node named by the ROOT module (`crate`/`self`/…),
//!   one root `imports` ref, then one FULL-path `imports` ref per binding;
//!   `use x::*` (use_wildcard) emits nothing at all.
//! - Trait supertraits come only from `trait_bounds`; a scoped supertrait
//!   (`fmt::Debug`) matches no case and is silently dropped.
//! - Rocket `routes!`/`catchers!` are extracted ONLY inside function bodies,
//!   and only when the macro name is a bare identifier.
//! - A rust type alias emits NO ref to its aliased type (the shared code
//!   reads a `value` field; rust's field is `type`).
//! - An `attribute_item` between a doc comment and its item breaks the
//!   docstring sibling chain (`#[derive(..)]` kills the docstring).
//!
//! Files with parse errors are walked like any other (tree-sitter's recovery is canonical; buffers::parse_collapse_warning reports a collapsed parse).

mod uses;
mod refs;
use crate::buffers::{
    edge_kind_index, node_kind_index, Arena, BindingRow, BoolFlags, EdgeRow, EmitOut,
    NodeRow, RefRow, Tables, BINDING_IMPORT, EXPORT_NONE, EXPORT_PUBLIC, FLAG_IS_ASYNC,
    FLAG_IS_EXPORTED, NONE, NONE_STR,
};
use crate::walker::{Scope, ValueScope, Cand};
use crate::textutil::{is_stoplisted, is_builtin_type, is_literal_receiver};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;




#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    return_type: Option<String>,
    qualified_name: Option<String>,
    visibility: Option<u8>,
    is_exported: Option<bool>,
    is_async: Option<bool>,
}



/// Per-node metadata for the receiver-method owner lookup and
/// findNodeByName (mirrors the TS scans over `this.nodes` — FIRST match
/// wins, earlier-in-file only).
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
}

impl<'t> Walker<'t> {
    fn new(src: &'t str, file_path: &'t str) -> Self {
        Walker {
            src,
            file_path,
            cols: util::Cols::new(src),
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
        }
    }
}

pub fn extract(file_path: &str, source: &str) -> Result<EmitOut, String> {
    let t0 = std::time::Instant::now();
    let tree = crate::langs::parse("rust", source)?;

    let mut w = Walker::new(source, file_path);

    let line_count = source.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
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
    markdown_refs_impl!();

    walker_pos_impl!();

    inside_class_like_impl!("class" | "struct" | "union" | "interface" | "trait" | "enum" | "module");

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
        if let Some(v) = extra.is_async {
            flags.set(FLAG_IS_ASYNC, v);
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
        // captureValueRefScope: rust consts are kind `variable` — still targets.
        let target_kind_ok = kind == "constant" || kind == "variable";
        if target_kind_ok
            && util::utf16_len(name) >= 3
            && util::has_upper_or_underscore().is_match(name)
        {
            let parent_ok = self
                .stack
                .last()
                .map(|s| matches!(s.kind, "file" | "class" | "module" | "struct" | "union" | "enum"))
                .unwrap_or(false);
            if parent_ok {
                self.fs_values.insert(name.to_string(), row);
                *self.fs_value_counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
        if matches!(kind, "function" | "method" | "constant" | "variable") {
            self.value_scopes.push(ValueScope { row, node, name: name.to_string() });
        }
        Some(row)
    }

    extract_name_impl!();

    /// rustExtractor.getSignature: raw params text + ` -> ` + raw return type.
    fn signature_of(&self, node: Node) -> Option<String> {
        let params = node.child_by_field_name("parameters")?;
        let mut sig = self.text(params).to_string();
        if let Some(rt) = node.child_by_field_name("return_type") {
            sig.push_str(" -> ");
            sig.push_str(self.text(rt));
        }
        Some(sig)
    }

    /// rustExtractor.getVisibility: direct `visibility_modifier` child whose
    /// text contains `pub` → public, else private; none → private.
    fn visibility_of(&self, node: Node) -> u8 {
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                if c.kind() == "visibility_modifier" {
                    return if self.text(c).contains("pub") { 1 } else { 2 };
                }
            }
        }
        2 // private — Rust defaults to private
    }

    /// extractRustReturnType (languages/rust.ts:14).
    fn return_type_of(&self, node: Node) -> Option<String> {
        let mut rt = node.child_by_field_name("return_type")?;
        if rt.kind() == "reference_type" {
            rt = (0..rt.named_child_count())
                .filter_map(|i| rt.named_child(i))
                .find(|c| matches!(c.kind(), "type_identifier" | "scoped_type_identifier" | "generic_type"))
                .unwrap_or(rt);
        }
        if matches!(rt.kind(), "primitive_type" | "unit_type" | "tuple_type") {
            return None;
        }
        let text = self.text(rt).trim();
        let stripped = crate::textutil::generic_args_re().replace_all(text, "");
        let last = stripped.rsplit("::").next().unwrap_or("").trim();
        if last.is_empty() || !crate::textutil::ascii_ident_re().is_match(last) {
            return None;
        }
        Some(if last == "Self" { "self".to_string() } else { last.to_string() })
    }

    /// rustImplTypeName (languages/rust.ts) — the implementing type's simple
    /// name for an impl block, from the grammar's `type` field (#1588):
    /// `impl<T> Tr for G<T>` / `impl<'a> Iterator for Parents<'a>` /
    /// `impl Tr for &Foo` / `impl Tr for m::Foo` → `G` / `Parents` / `Foo` /
    /// `Foo`. Shapes naming no single type (tuple, `dyn Tr`, pointer,
    /// primitive, fn type…) → None. Mirrored byte-for-byte — change both.
    fn impl_type_name(&self, ty: Option<Node>) -> Option<String> {
        stack_guard!();
        let ty = ty?;
        match ty.kind() {
            "type_identifier" | "identifier" => Some(self.text(ty).to_string()),
            "generic_type" => self.impl_type_name(ty.child_by_field_name("type")),
            "scoped_type_identifier" | "scoped_identifier" => {
                self.impl_type_name(ty.child_by_field_name("name"))
            }
            "reference_type" => self.impl_type_name(ty.child_by_field_name("type")),
            _ => None,
        }
    }

    /// rustExtractor.getReceiverType: parent-walk to the nearest impl_item and
    /// read its `type` field (impl_type_name). The pre-#1588 rule took the
    /// LAST direct type_identifier child, which for `impl Trait for Generic<T>`
    /// was the TRAIT — so every parameterized impl's methods were qualified by
    /// the trait.
    fn receiver_type_of(&self, node: Node) -> Option<String> {
        let mut parent = node.parent();
        while let Some(p) = parent {
            if p.kind() == "impl_item" {
                return self.impl_type_name(p.child_by_field_name("type"));
            }
            parent = p.parent();
        }
        None
    }

    // --- visitNode ------------------------------------------------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if matches!(kind, "function_item" | "function_signature_item") {
            self.extract_fn_or_method(node);
            skip_children = true;
        } else if kind == "trait_item" {
            self.extract_interface(node);
            skip_children = true;
        } else if kind == "struct_item" {
            self.extract_aggregate(node, "struct");
            skip_children = true;
        } else if kind == "union_item" {
            self.extract_aggregate(node, "union");
            skip_children = true;
        } else if kind == "enum_item" {
            self.extract_enum(node);
            skip_children = true;
        } else if kind == "type_item" {
            self.extract_type_alias(node);
            // extractTypeAlias returns false for rust (plain alias) — children
            // are visited (nothing in them has a branch).
        } else if matches!(kind, "let_declaration" | "const_item" | "static_item")
            && !self.inside_class_like()
        {
            // Inside a class-like scope the gate fails and the else-ladder
            // falls through with children VISITED — a trait const's value
            // expression emits calls refs from the trait node.
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "use_declaration" {
            self.extract_import(node);
            self.emit_use_bindings(node);
            // importTypes branch never sets skipChildren.
        } else if kind == "call_expression" {
            self.extract_call(node);
        } else if kind == "struct_expression" {
            self.extract_instantiation(node);
        } else if kind == "impl_item" {
            // Emits the implements back-reference; skipChildren stays false so
            // the declaration_list is visited at FILE scope (impl pushes
            // nothing on the stack).
            self.extract_rust_impl_item(node);
        }

        if !skip_children {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    self.visit_node(c);
                }
            }
        }
    }

    // --- extractors --------------------------------------------------------------

    /// extractFunction/extractMethod, decision resolved once: method iff a
    /// receiver is found (fn inside an impl — including a NESTED fn inside an
    /// impl method's body, whose parent walk passes through the outer fn) or
    /// the stack top is class-like (trait members).
    fn extract_fn_or_method(&mut self, node: Node<'t>) {
        stack_guard!();
        let receiver = self.receiver_type_of(node);
        let as_method = receiver.is_some() || self.inside_class_like();

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
            visibility: Some(self.visibility_of(node)),
            // isAsync hook exists but never finds a direct `async` child (it
            // nests in function_modifiers) — present-false on every node.
            is_async: Some(false),
            return_type: self.return_type_of(node),
            qualified_name: receiver.as_ref().map(|r| format!("{r}::{name}")),
            ..Extra::default() // isExported hook absent → flag not set
        };
        let kind: &'static str = if as_method { "method" } else { "function" };
        let Some(row) = self.create_node(kind, &name, node, extra) else { return };

        // Contains edge from the owner: receiver present AND not class-like —
        // FIRST earlier-in-file struct/class/enum/trait of the receiver's name.
        if as_method && !self.inside_class_like() {
            if let Some(receiver) = &receiver {
                let owner_row = self
                    .nodes_meta
                    .iter()
                    .position(|m| {
                        m.name == *receiver
                            && matches!(m.kind, "struct" | "union" | "class" | "enum" | "trait")
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
        // extractDecoratorsFor: rust attribute_items are siblings, not
        // decorator/annotation/attribute node types — complete no-op.
        self.stack.push(Scope { row, kind, name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    /// extractInterface — kind `trait` (interfaceKind), inheritance from
    /// trait_bounds, body children visited with the trait pushed.
    fn extract_interface(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default() // no visibility/isExported on the interface path
        };
        let Some(row) = self.create_node("trait", &name, node, extra) else { return };
        self.extract_inheritance(node, row);

        self.stack.push(Scope { row, kind: "trait", name });
        let body = node.child_by_field_name("body").unwrap_or(node);
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    /// Extract a Rust struct or union — the body field is OPTIONAL. A unit
    /// struct (`struct U;`) has no body and is still a complete definition,
    /// so it mints a node with no members; tuple structs' ordered_field_declaration_list
    /// is a body. Mirrors the TS reference's `allowBodilessStruct`.
    fn extract_aggregate(&mut self, node: Node<'t>, kind: &'static str) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node(kind, &name, node, extra) else { return };
        self.extract_inheritance(node, row);

        // Unit structs have no body to walk — the node itself is the whole
        // definition.
        let Some(body) = node.child_by_field_name("body") else { return };

        self.stack.push(Scope { row, kind, name });
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    /// extractEnum — body required; enum_variant children → enum_member nodes
    /// (name field only, payloads never walked); other children re-dispatched.
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
        self.extract_inheritance(node, row);

        self.stack.push(Scope { row, kind: "enum", name });
        for i in 0..body.named_child_count() {
            let Some(c) = body.named_child(i) else { continue };
            if c.kind() == "enum_variant" {
                if let Some(name_node) = c.child_by_field_name("name") {
                    let vname = self.text(name_node).to_string();
                    self.create_node("enum_member", &vname, c, Extra::default());
                }
            } else {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    /// extractTypeAlias — plain `type_alias` node. QUIRK: the alias-value ref
    /// walk reads a `value` field; rust type_item's field is `type` → no ref
    /// to the aliased type. Returns children-visited (false) like the TS.
    fn extract_type_alias(&mut self, node: Node<'t>) {
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return;
        }
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default()
        };
        self.create_node("type_alias", &name, node, extra);
    }

    /// extractVariable's generic fallback: kind is ALWAYS `variable` (no
    /// isConst hook), every direct `identifier` child mints a node positioned
    /// at the CHILD, docstring shared, isExported present-false, no signature,
    /// and the initializer value is never body-walked.
    fn extract_variable(&mut self, node: Node<'t>) {
        let docstring = preceding_docstring(node, self.src);
        let name_field = node.child_by_field_name("name");
        let mut declared: Option<(u32, String)> = None;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() != "identifier" {
                continue;
            }
            let name = self.text(child).to_string();
            if !name.is_empty() {
                let row = self.create_node(
                    "variable",
                    &name,
                    child,
                    Extra {
                        docstring: docstring.clone(),
                        is_exported: Some(false),
                        ..Extra::default()
                    },
                );
                if let (Some(row), Some(nf)) = (row, name_field) {
                    if child.start_byte() == nf.start_byte() {
                        declared = Some((row, name));
                    }
                }
            }
        }
        // Walk the initializer ATTRIBUTED to the declared symbol (#693):
        // `const N: usize = compute()` and
        // `static REGISTRY: Lazy<T> = Lazy::new(|| build())` dropped every call
        // inside the initializer, so a handler table or a lazily-built
        // singleton linked to nothing.
        if let Some(value) = node.child_by_field_name("value") {
            match declared {
                Some((row, name)) => {
                    self.stack.push(Scope { row, kind: "variable", name });
                    self.visit_for_calls_and_structure(value);
                    self.stack.pop();
                }
                None => self.visit_for_calls_and_structure(value),
            }
        }
    }

    /// extractCall — the rust paths of the generic else-branch (4312+).
    fn extract_call(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let mut callee_name = String::new();

        if let Some(func) = func {
            if func.kind() == "field_expression" {
                let property = func
                    .child_by_field_name("property")
                    .or_else(|| func.child_by_field_name("field"))
                    .or_else(|| func.named_child(1));
                if let Some(property) = property {
                    let method_name = self.text(property);
                    let receiver = func
                        .child_by_field_name("object")
                        .or_else(|| func.child_by_field_name("operand"))
                        .or_else(|| func.child_by_field_name("argument"))
                        .or_else(|| func.named_child(0));
                    if let Some(r) = receiver {
                        if is_literal_receiver(r.kind()) {
                            return; // emit NOTHING (#1230)
                        }
                    }
                    if let Some(r) = receiver {
                        match r.kind() {
                            // rust `self` is node kind `self`, NOT `identifier` —
                            // it dodges this branch and falls to the bare-name
                            // fallthrough (same net effect as SKIP_RECEIVERS).
                            "identifier" | "simple_identifier" | "field_identifier" => {
                                let receiver_name = self.text(r);
                                if !matches!(receiver_name, "self" | "this" | "cls" | "super") {
                                    callee_name = format!("{receiver_name}.{method_name}");
                                } else {
                                    callee_name = method_name.to_string();
                                }
                            }
                            "call_expression" => {
                                // Chained-call re-encode: ONLY an associated-
                                // function chain (`Foo::new().bar()`, inner
                                // callee a scoped_identifier). Instance chains
                                // keep the bare method name.
                                let inner_fn = r.child_by_field_name("function");
                                let reencode =
                                    inner_fn.map(|f| f.kind() == "scoped_identifier").unwrap_or(false);
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
                            "field_expression" => {
                                // `self.<field>.<method>()` — a call through a
                                // field of the enclosing type (#1585): keep the
                                // `self.` prefix so the resolver can type the
                                // field from the owner struct's declaration
                                // (or leave it unresolved). Any other
                                // field_expression receiver — a deeper chain,
                                // a non-self base — keeps the bare name.
                                let base = r.child_by_field_name("value");
                                let field = r.child_by_field_name("field");
                                match (base, field) {
                                    (Some(b), Some(f))
                                        if b.kind() == "self" && f.kind() == "field_identifier" =>
                                    {
                                        let field_name = self.text(f);
                                        callee_name = format!("self.{field_name}.{method_name}");
                                    }
                                    _ => callee_name = method_name.to_string(),
                                }
                            }
                            // `self.method()` — keep the `self.` prefix so the
                            // resolver can read the owner off the calling
                            // method's qualified name and resolve the method on
                            // THAT type, instead of matching a bare name by file
                            // proximity (#1861). Mirrors the wasm extractor.
                            "self" => {
                                callee_name = format!("self.{method_name}");
                            }
                            _ => {
                                // parenthesized, await_expression — bare method
                                // name.
                                callee_name = method_name.to_string();
                            }
                        }
                    } else {
                        callee_name = method_name.to_string();
                    }
                }
            } else if matches!(func.kind(), "scoped_identifier" | "scoped_call_expression") {
                callee_name = self.text(func).to_string();
            } else {
                // identifier; generic_function keeps the raw turbofish text
                // (`helper::<T>` — unresolvable downstream, preserved).
                callee_name = self.text(func).to_string();
            }
        }

        if !callee_name.is_empty() {
            // Parenthesized-callee normalization — `(f)(x)` → `f`.
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
            let from = self.top_row();
            self.push_ref_at(from, &callee_name, edge_kind_index("calls").unwrap(), node);
        }
    }

    /// extractInstantiation — struct_expression via the GENERIC path: strip
    /// from the first `<`, keep the trailing `::`/`.` segment (JS slice
    /// semantics: slice(lastDot+1) after a `::` leaves one `:`, then ONE
    /// leading `[:.]` is stripped).
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

        let class_name = crate::textutil::strip_generic_and_qualifier(self.text(ctor));

        if !class_name.is_empty() {
            let from = self.top_row();
            self.push_ref_at(from, &class_name, edge_kind_index("instantiates").unwrap(), node);
        }
    }

    /// extractRustRouteMacro — body-walker-only; bare `routes`/`catchers`
    /// identifiers only (`rocket::routes![…]` is skipped); identifier runs in
    /// the token tree join with `::`, flushed on `,` and at end.
    fn extract_rust_route_macro(&mut self, node: Node<'t>) {
        let Some(macro_name) = node.named_child(0) else { return };
        let name = self.text(macro_name);
        if name != "routes" && name != "catchers" {
            return;
        }
        let token_tree = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "token_tree");
        let Some(token_tree) = token_tree else { return };
        if self.stack.is_empty() {
            return;
        }
        let from = self.top_row();
        let refs_kind = edge_kind_index("references").unwrap();

        let mut parts: Vec<&str> = Vec::new();
        let mut line = 0u32;
        let mut column_byte = 0usize;
        let mut row = 0usize;
        macro_rules! flush {
            () => {
                if !parts.is_empty() {
                    let joined = parts.join("::");
                    let column = self.cols.col(self.src, row, column_byte);
                    let name_ref = self.arena.put(&joined);
                    self.tables.push_ref(&RefRow {
                        from_idx: from,
                        kind: refs_kind,
                        line,
                        column,
                        reference_name: name_ref,
                        candidates: NONE_STR,
                        from_id_str: NONE_STR,
                    });
                    parts.clear();
                }
            };
        }
        for i in 0..token_tree.child_count() {
            let Some(t) = token_tree.child(i) else { continue };
            if t.kind() == "identifier" {
                if parts.is_empty() {
                    line = t.start_position().row as u32 + 1;
                    column_byte = t.start_byte();
                    row = t.start_position().row;
                }
                parts.push(self.text(t));
            } else if t.kind() == "," {
                flush!();
            }
        }
        flush!();
    }

    /// extractInheritance — the rust-reachable cases: trait_bounds
    /// (supertraits; a scoped `fmt::Debug` bound matches NO case and is
    /// dropped), the Go embedding check on field_declaration (inert in rust —
    /// every field has a field_identifier), and the field_declaration_list
    /// recursion that reaches it.
    fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        stack_guard!();
        let extends_kind = edge_kind_index("extends").unwrap();
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                "trait_bounds" => {
                    for j in 0..child.named_child_count() {
                        let Some(bound) = child.named_child(j) else { continue };
                        let type_node: Option<Node> = match bound.kind() {
                            "type_identifier" => Some(bound),
                            "generic_type" => (0..bound.named_child_count())
                                .filter_map(|k| bound.named_child(k))
                                .find(|c| c.kind() == "type_identifier"),
                            "higher_ranked_trait_bound" => {
                                let generic = (0..bound.named_child_count())
                                    .filter_map(|k| bound.named_child(k))
                                    .find(|c| c.kind() == "generic_type");
                                generic
                                    .and_then(|g| {
                                        (0..g.named_child_count())
                                            .filter_map(|k| g.named_child(k))
                                            .find(|c| c.kind() == "type_identifier")
                                    })
                                    .or_else(|| {
                                        (0..bound.named_child_count())
                                            .filter_map(|k| bound.named_child(k))
                                            .find(|c| c.kind() == "type_identifier")
                                    })
                            }
                            _ => None, // scoped_type_identifier: dropped (quirk)
                        };
                        if let Some(tn) = type_node {
                            let name = self.text(tn).to_string();
                            self.push_ref_at(class_row, &name, extends_kind, tn);
                        }
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

    /// extractRustImplItem — `impl Trait for Type` back-reference from the
    /// grammar's `trait` / `type` fields (#1588; an inherent impl has no
    /// `trait` field and emits nothing). Target = FIRST earlier node of kind
    /// struct/union/enum/class (never trait) named by impl_type_name; ref FROM
    /// the type's node, named by the trait's full text (scoped path / generic
    /// args kept), at the trait node's position.
    fn extract_rust_impl_item(&mut self, node: Node<'t>) {
        let Some(trait_node) = node.child_by_field_name("trait") else {
            return;
        };
        let trait_name = self.text(trait_node).to_string();
        let Some(type_name) = self.impl_type_name(node.child_by_field_name("type")) else {
            return;
        };

        let target_row = self
            .nodes_meta
            .iter()
            .position(|m| m.name == type_name && matches!(m.kind, "struct" | "union" | "enum" | "class"))
            .map(|i| i as u32);
        if let Some(target_row) = target_row {
            self.push_ref_at(target_row, &trait_name, edge_kind_index("implements").unwrap(), trait_node);
        }
    }

    /// extractTypeAnnotations — parameters + return_type subtrees, one
    /// `references` ref per type_identifier leaf not in BUILTIN_TYPES. The
    /// trailing `type_annotation` child lookup is included for fidelity (the
    /// rust grammar has no such node — always a no-op).
    fn extract_type_annotations(&mut self, node: Node<'t>, from_row: u32) {
        if let Some(params) = node.child_by_field_name("parameters") {
            self.extract_type_refs_from_subtree(params, from_row);
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
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

    // --- visitFunctionBody -----------------------------------------------------


    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        // Rocket route macros: handler paths live in a raw token tree.
        if kind == "macro_invocation" {
            self.extract_rust_route_macro(node);
        }

        if kind == "call_expression" {
            self.extract_call(node);
        } else if kind == "struct_expression" {
            self.extract_instantiation(node);
        }

        // Nested NAMED fns become their own nodes (a nested fn inside an impl
        // method walks up to the impl and indexes as a METHOD).
        if matches!(kind, "function_item" | "function_signature_item") {
            let name = self.extract_name(node);
            if name != "<anonymous>" {
                self.extract_fn_or_method(node);
                return;
            }
        }

        // Structural nodes inside bodies.
        if kind == "struct_item" {
            self.extract_aggregate(node, "struct");
            return;
        }
        if kind == "union_item" {
            self.extract_aggregate(node, "union");
            return;
        }
        if kind == "enum_item" {
            self.extract_enum(node);
            return;
        }
        if kind == "trait_item" {
            self.extract_interface(node);
            return;
        }
        // A `use` inside a body emits binding rows only — import nodes and
        // `imports` refs are module-level (visit_node) contributions.
        if kind == "use_declaration" {
            self.emit_use_bindings(node);
            return;
        }

        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.visit_for_calls_and_structure(c);
            }
        }
    }

    // --- fn refs (RUST_SPEC) ----------------------------------------------------

    flush_fn_ref_candidates_impl!();

    // --- value refs -------------------------------------------------------------

}




