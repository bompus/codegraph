//! Swift extraction — a faithful Rust port of `TreeSitterExtractor`'s Swift
//! paths (src/extraction/tree-sitter.ts) plus languages/swift.ts.
//!
//! Same porting contract as the other walkers: behavior parity, bug-for-bug.
//! The authoritative quirk list is docs/design/swift-kernel-port-checklist.md.
//! The port's center of gravity is the DEDICATED in-class property branch
//! (#1020 — Alamofire's 348 `property` nodes): computed properties become
//! `property` nodes whose getter walks with the property pushed; stored
//! `static let/var` → constant/variable, instance stored → field; decorator/
//! type-annotation/attr-arg refs all attach to the ENCLOSING TYPE; stored
//! declarations descend so initializer calls attribute to the class. Also
//! preserved on purpose: `parameter` field never resolves (zero param type
//! refs, zero signatures), isAsync is present-false (dead hook), `open` →
//! internal visibility, everything-is-`extends` inheritance (first
//! type_identifier of each specifier), no instantiates refs ever (`Foo()` is
//! a plain call), subscript reads as `calls arr`, `defer` as `calls defer`,
//! multi-case enum entries minting only the first case, `/** */` block docs
//! ignored AND chain-breaking, init/deinit/subscript minting no nodes with
//! their bodies routed through visitNode (calls → class, static reads →
//! nothing). Positions in UTF-16 code units. Files with parse errors defer
//! to wasm (structurally high incidence, 9–27% — the sweep runs
//! --max-deferral 0.3 by measured both-arm reality).

mod calls;
mod refs;
use crate::buffers::{
    node_kind_index, Arena, BoolFlags, EdgeRow, EmitOut, NodeRow,
    RefRow, Tables, FLAG_IS_ASYNC, FLAG_IS_EXPORTED, FLAG_IS_STATIC,
    NONE, NONE_STR,
};
use crate::walker::{Scope, ValueScope, Cand};
use crate::textutil::{is_stoplisted, is_builtin_type, is_literal_receiver, strip_generic_and_qualifier, capitalized_re};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use std::collections::{HashMap, HashSet, VecDeque};
use tree_sitter::Node;








#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<u8>,
    is_static: Option<bool>,
    is_async: Option<bool>,
    is_exported: Option<bool>,
    return_type: Option<String>,
}



struct SwiftPropInfo<'t> {
    name_node: Option<Node<'t>>,
    is_let: bool,
    is_computed: bool,
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
    fs_value_counts: HashMap<String, u32>,
    value_scopes: Vec<ValueScope<'t>>,
}

pub fn extract(file_path: &str, source: &str) -> Result<EmitOut, String> {
    let t0 = std::time::Instant::now();
    let tree = crate::langs::parse("swift", source)?;

    let mut w = Walker {
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
        fs_value_counts: HashMap::new(),
        value_scopes: Vec::new(),
    };

    let line_count = w.cols.line_count();
    let base_name = crate::buffers::push_file_node(&mut w.arena, &mut w.tables, file_path, line_count);
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });

    // No packageTypes — swift has no namespace node; top-level QNs are bare.
    w.visit_node(tree.root_node());
    w.flush_fn_ref_candidates();
    w.flush_value_refs(tree.root_node());
    w.stack.pop();

    Ok(crate::buffers::finish(w.arena, w.tables, tree.root_node().has_error(), file_path, t0))
}

/// firstSimpleIdentifier (tree-sitter.ts:261): BFS (FIFO), at most 40 nodes
/// popped, first `simple_identifier` wins.
fn first_simple_identifier<'t>(node: Option<Node<'t>>) -> Option<Node<'t>> {
    let mut q: VecDeque<Node<'t>> = VecDeque::new();
    if let Some(n) = node {
        q.push_back(n);
    }
    let mut guard = 0;
    while guard < 40 {
        let Some(n) = q.pop_front() else { break };
        guard += 1;
        if n.kind() == "simple_identifier" {
            return Some(n);
        }
        for i in 0..n.named_child_count() {
            if let Some(c) = n.named_child(i) {
                q.push_back(c);
            }
        }
    }
    None
}

/// lastNamedOfType (function-ref.ts:600): rightmost matching DESCENDANT in
/// document order (deeper matches override).
fn last_simple_identifier<'t>(node: Node<'t>) -> Option<Node<'t>> {
    stack_guard!();
    let mut found: Option<Node<'t>> = None;
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else { continue };
        if child.kind() == "simple_identifier" {
            found = Some(child);
        }
        if let Some(deeper) = last_simple_identifier(child) {
            found = Some(deeper);
        }
    }
    found
}

impl<'t> Walker<'t> {
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
        let end_line = node.end_position().row as u32 + 1; // no resolveBody for swift

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
        if let Some(v) = extra.is_exported {
            flags.set(FLAG_IS_EXPORTED, v);
        }
        if let Some(v) = extra.is_async {
            flags.set(FLAG_IS_ASYNC, v);
        }
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
            decorators: NONE_STR, // extractModifiers absent — never set from modifiers
            type_parameters: NONE_STR,
            return_type: ret_ref,
            extra_json: NONE_STR,
        });
        self.node_ids.push(id);

        let parent_row = self.top_row();
        self.tables.push_edge(&EdgeRow {
            source_idx: parent_row,
            target_idx: row,
            kind: crate::buffers::EDGE_CONTAINS,
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
        // captureValueRefScope — struct:/enum: parents accepted (the swift
        // static-let-namespacing idiom).
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
        Some(row)
    }

    // --- hooks (languages/swift.ts) ----------------------------------------------

    /// extractName incl. the resolveName hook: a multi-segment extension name
    /// (`extension KF.Builder`) takes the LAST type_identifier's text.
    fn extract_name(&self, node: Node) -> String {
        if node.kind() == "class_declaration" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if name_node.kind() == "user_type" {
                    let ids: Vec<Node> = (0..name_node.named_child_count())
                        .filter_map(|i| name_node.named_child(i))
                        .filter(|c| c.kind() == "type_identifier")
                        .collect();
                    if ids.len() > 1 {
                        return self.text(ids[ids.len() - 1]).to_string();
                    }
                }
            }
        }
        if let Some(name_node) = node.child_by_field_name("name") {
            return self.text(name_node).to_string();
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

    /// getVisibility: whole-text substring matching over `modifiers` children;
    /// default INTERNAL. `open` → internal, `fileprivate` → private (via the
    /// 'private' substring), `public private(set)` → public (first match).
    fn visibility_of(&self, node: Node) -> u8 {
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            if child.kind() == "modifiers" {
                let text = self.text(child);
                if text.contains("public") {
                    return 1;
                }
                if text.contains("private") {
                    return 2;
                }
                if text.contains("internal") {
                    return 4;
                }
                // 'fileprivate' arm is dead — 'private' already matched.
            }
        }
        4 // Swift defaults to internal
    }

    /// isStatic: modifiers text contains 'static' OR 'class' (class members
    /// count — deliberate; substring semantics preserved).
    fn is_static(&self, node: Node) -> bool {
        (0..node.child_count())
            .filter_map(|i| node.child(i))
            .any(|c| {
                c.kind() == "modifiers" && {
                    let t = self.text(c);
                    t.contains("static") || t.contains("class")
                }
            })
    }

    /// isAsync: dead hook — `async` never sits inside `modifiers` (it's an
    /// anon child after the params) → effectively always false, but PRESENT.
    fn is_async(&self, node: Node) -> bool {
        (0..node.child_count())
            .filter_map(|i| node.child(i))
            .any(|c| c.kind() == "modifiers" && self.text(c).contains("async"))
    }

    /// extractSwiftReturnType — POSITIONAL: first user_type/optional_type after
    /// the name simple_identifier, before function_body; last dotted segment;
    /// generics stripped non-nesting; Void → None.
    fn return_type_of(&self, node: Node) -> Option<String> {
        let mut seen_name = false;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() == "simple_identifier" && !seen_name {
                seen_name = true;
                continue;
            }
            if !seen_name {
                continue;
            }
            if child.kind() == "function_body" {
                return None;
            }
            let type_node = match child.kind() {
                "user_type" => Some(child),
                "optional_type" => (0..child.named_child_count())
                    .filter_map(|j| child.named_child(j))
                    .find(|c| c.kind() == "user_type"),
                _ => None,
            };
            if child.kind() == "user_type" || child.kind() == "optional_type" {
                let t = type_node?;
                let name = crate::textutil::generic_args_re()
                    .replace_all(self.text(t).trim(), "")
                    .into_owned();
                let last = name.rsplit('.').next().unwrap_or("").trim();
                if last.is_empty() || !crate::textutil::ascii_ident_re().is_match(last) || last == "Void" {
                    return None;
                }
                return Some(last.to_string());
            }
        }
        None
    }

    /// swiftPropertyInfo (tree-sitter.ts:277).
    fn swift_property_info(&self, node: Node<'t>) -> SwiftPropInfo<'t> {
        let pattern = node.child_by_field_name("name").or_else(|| {
            (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| matches!(c.kind(), "value_binding_pattern" | "pattern"))
        });
        let binding = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "value_binding_pattern");
        let is_let = binding
            .map(|b| self.text(b).trim_start().starts_with("let"))
            .unwrap_or(false);
        let is_computed = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .any(|c| matches!(c.kind(), "computed_property" | "protocol_property_requirements"));
        SwiftPropInfo { name_node: first_simple_identifier(pattern), is_let, is_computed }
    }

    // --- the dispatcher (visitNode, Swift-relevant branches) -----------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "function_declaration" {
            if self.inside_class_like() {
                self.extract_method(node);
            } else {
                self.extract_function(node);
            }
            skip_children = true;
        } else if kind == "class_declaration" {
            // classifyClassNode: `struct`/`enum` keyword children; actor and
            // extension fall through to 'class'.
            let mut classified = "class";
            for i in 0..node.child_count() {
                if let Some(c) = node.child(i) {
                    if c.kind() == "struct" {
                        classified = "struct";
                        break;
                    }
                    if c.kind() == "enum" {
                        classified = "enum";
                        break;
                    }
                }
            }
            match classified {
                "struct" => self.extract_struct(node),
                "enum" => self.extract_enum(node),
                _ => self.extract_class(node),
            }
            skip_children = true;
        } else if kind == "protocol_declaration" {
            self.extract_interface(node);
            skip_children = true;
        } else if kind == "typealias_declaration" {
            skip_children = self.extract_type_alias(node);
        } else if kind == "property_declaration" && !self.inside_class_like() {
            // Top-level let/var (extractVariable's swift branch). Initializers
            // are NEVER walked — candidates-only scan.
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if matches!(kind, "property_declaration" | "protocol_property_declaration")
            && self.inside_class_like()
        {
            skip_children = self.dedicated_property_branch(node);
        } else if kind == "import_declaration" {
            self.extract_import(node);
        } else if kind == "call_expression" {
            self.extract_call(node);
        }
        // init/deinit/subscript declarations, macro_invocation, directive,
        // diagnostic, operator/precedence declarations, protocol function
        // requirements, associatedtype: no branch — recursed. Their calls
        // attribute to the enclosing scope; static-member reads inside them
        // emit NOTHING (the pass is body-walker-only).

        if !skip_children {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    self.visit_node(c);
                }
            }
        }
    }

    /// THE DEDICATED PROPERTY BRANCH (tree-sitter.ts:1113-1193, #1020).
    /// Returns skipChildren.
    fn dedicated_property_branch(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        let owner_row = self.top_row();
        let info = self.swift_property_info(node);
        let mut computed_prop: Option<(u32, String)> = None;

        if let Some(name_node) = info.name_node {
            let name = self.text(name_node).to_string();
            if info.is_computed {
                let row = self.create_node(
                    "property",
                    &name,
                    node,
                    Extra {
                        visibility: Some(self.visibility_of(node)),
                        is_static: Some(self.is_static(node)),
                        ..Extra::default()
                    },
                );
                if let Some(row) = row {
                    computed_prop = Some((row, name));
                }
            } else {
                let is_static = self.is_static(node);
                let kind: &'static str = if is_static {
                    if info.is_let { "constant" } else { "variable" }
                } else {
                    "field"
                };
                self.create_node(
                    kind,
                    &name,
                    node,
                    Extra {
                        visibility: Some(self.visibility_of(node)),
                        is_static: Some(is_static),
                        ..Extra::default()
                    },
                );
            }
        }

        // All three ref passes attach to the ENCLOSING TYPE (ownerId).
        self.extract_decorators_for(node, owner_row);
        // extractVariableTypeAnnotation: the direct type_annotation child.
        let ta = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = ta {
            self.extract_type_refs_from_subtree(ta, owner_row);
        }
        // walkAttrArgs: extractStaticMemberRef over the whole modifiers subtree
        // (`@Siblings(through: Pivot.self)` metatype args).
        let mods = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "modifiers");
        if let Some(mods) = mods {
            self.walk_attr_args(mods);
        }

        if let Some((row, name)) = computed_prop {
            let getter = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| matches!(c.kind(), "computed_property" | "protocol_property_requirements"));
            if let Some(getter) = getter {
                self.stack.push(Scope { row, kind: "property", name });
                self.visit_for_calls_and_structure(getter);
                self.stack.pop();
            }
            return true; // skipChildren — computed only
        }
        // Stored: descend generically — initializer calls attribute to the
        // CLASS; observers' bodies likewise; modifiers re-walk is harmless.
        false
    }

    fn walk_attr_args(&mut self, n: Node<'t>) {
        stack_guard!();
        self.extract_static_member_ref(n);
        for i in 0..n.named_child_count() {
            if let Some(c) = n.named_child(i) {
                self.walk_attr_args(c);
            }
        }
    }

    // --- visitFunctionBody ---------------------------------------------------------


    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "call_expression" {
            self.extract_call(node);
        }
        // (INSTANTIATION_KINDS has no swift types; extractBareCall absent.)

        self.extract_static_member_ref(node);

        if kind == "function_declaration" {
            let name = self.extract_name(node);
            if name != "<anonymous>" {
                self.extract_function(node);
                return;
            }
        }
        if kind == "class_declaration" {
            let mut classified = "class";
            for i in 0..node.child_count() {
                if let Some(c) = node.child(i) {
                    if c.kind() == "struct" {
                        classified = "struct";
                        break;
                    }
                    if c.kind() == "enum" {
                        classified = "enum";
                        break;
                    }
                }
            }
            match classified {
                "struct" => self.extract_struct(node),
                "enum" => self.extract_enum(node),
                _ => self.extract_class(node),
            }
            return;
        }
        if kind == "protocol_declaration" {
            self.extract_interface(node);
            return;
        }

        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.visit_for_calls_and_structure(c);
            }
        }
    }

    // --- extractors -----------------------------------------------------------------

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
            signature: None, // getSignature reads the never-resolving 'parameter' field
            visibility: Some(self.visibility_of(node)),
            is_async: Some(self.is_async(node)), // present-false (dead hook)
            is_static: Some(self.is_static(node)),
            return_type: self.return_type_of(node),
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

    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: None,
            visibility: Some(self.visibility_of(node)),
            is_async: Some(self.is_async(node)),
            is_static: Some(self.is_static(node)),
            return_type: self.return_type_of(node),
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

    fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("class", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        // primaryCtor refs: csharp-only (no parameter_list child type).
        // Classes DO get decorates (`@Observable class`), unlike struct/enum.
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "class", name });
        let body = node.child_by_field_name("body").unwrap_or(node);
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    fn extract_struct(&mut self, node: Node<'t>) {
        stack_guard!();
        // Body gate (:1876) — bodiless mints nothing (record exemption is C#).
        let Some(body) = node.child_by_field_name("body") else { return };
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("struct", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        // NO extractDecoratorsFor for structs (`@main struct` emits nothing).
        self.stack.push(Scope { row, kind: "struct", name });
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
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("enum", &name, node, extra) else { return };
        // Raw-value types ride inheritance (`enum Suit: String` → extends
        // String — extends refs have NO builtin filter). NO decorates.
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "enum", name });
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else { continue };
            if child.kind() == "enum_entry" {
                self.extract_enum_members(child);
            } else {
                self.visit_node(child);
            }
        }
        self.stack.pop();
    }

    fn extract_enum_members(&mut self, node: Node<'t>) {
        // `name` field = the FIRST case name only — `case put, delete` mints
        // ONLY `put` (the identifier-scan fallback is dead, the field always
        // resolves). Associated/raw values never walked.
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.text(name_node).to_string();
            self.create_node("enum_member", &name, node, Extra::default());
        }
    }

    fn extract_interface(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default() // NO visibility, NO decorates
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

    /// extractTypeAlias (:2890) — plain type_alias node + value-subtree type
    /// refs (`typealias Handler = (Data) -> Void` → refs Data + Void…Void is
    /// builtin-suppressed; `= KF.Builder` → refs KF AND Builder). Returns
    /// skipChildren=false (children also recursed, harmlessly).
    fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default()
        };
        let row = self.create_node("type_alias", &name, node, extra);
        if let Some(row) = row {
            if let Some(value) = node.child_by_field_name("value") {
                self.extract_type_refs_from_subtree(value, row);
            }
        }
        false
    }

    /// extractVariable — the swift top-level branch (:2851): let → constant /
    /// var → variable via swiftPropertyInfo; computed skipped; position = the
    /// whole declaration; extras = docstring + isExported literal FALSE.
    fn extract_variable(&mut self, node: Node<'t>) {
        let docstring = preceding_docstring(node, self.src);
        let info = self.swift_property_info(node);
        let Some(name_node) = info.name_node else { return };
        if info.is_computed {
            return;
        }
        let kind: &'static str = if info.is_let { "constant" } else { "variable" };
        let name = self.text(name_node).to_string();
        self.create_node(
            kind,
            &name,
            node,
            Extra { docstring, is_exported: Some(false), ..Extra::default() },
        );
    }

    fn extract_import(&mut self, node: Node<'t>) {
        let import_text = self.text(node).trim().to_string();
        let identifier = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "identifier");
        let Some(identifier) = identifier else { return }; // hook null → nothing
        let module_name = self.text(identifier).to_string();
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
        self.push_ref_at(parent, &module_name, crate::buffers::EDGE_IMPORTS, node);
    }

    type_refs_from_subtree_impl!();

    decorators_impl!();


    // --- function-as-value refs (SWIFT_SPEC, function-ref.ts:288) -------------------

    flush_fn_ref_candidates_impl!();

    // --- value references -------------------------------------------------------------

}


