//! Kotlin extraction — a faithful Rust port of `TreeSitterExtractor`'s Kotlin
//! paths (src/extraction/tree-sitter.ts) plus languages/kotlin.ts.
//!
//! Same porting contract as the other walkers: behavior parity, bug-for-bug.
//! The authoritative quirk list is docs/design/kotlin-kernel-port-checklist.md.
//! Two surfaces are FIRSTS for the kernel: extension-function receivers
//! (getReceiverType → `Type::method` qualified-name OVERRIDE with no package
//! prefix + the owner-contains fallback that excludes `interface` kinds and
//! is source-order dependent) and extractModifiers (expect/actual platform
//! modifiers → the node DECORATORS wire field, on every created node — the
//! KMP synthesizer's input). Preserved on purpose: the FIELD_COUNT-0 dead
//! cluster (no signatures, ZERO type-annotation refs), the bodiless-class
//! header re-walk asymmetry, enum-entry bodies being invisible, KDoc
//! (`multiline_comment`) never being a docstring AND chain-breaking,
//! comment-gluing into import/package extents,
//! `@Anno(args)` emitting nothing while `@Anno` emits decorates, zero
//! instantiates refs (constructors are capitalized `calls`), the qualified-
//! receiver `com::qext` bug, the paren-then-lambda `trailing()` garbage
//! callee, and the packaged-file value-ref target drop (namespace parents are
//! not accepted). The fun-interface misparse-recovery hook branches are
//! ported below (erroring files are extracted natively since Phase 1 of
//! kernel-only-extraction-plan.md; phantom hasError files were always clean).
//! Positions in UTF-16 code units.

mod hooks;
mod calls;
mod bindings;
mod refs;
use crate::buffers::{
    BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, node_kind_index, Arena, BoolFlags, EmitOut, NodeRow,
    RefRow, StrRef, Tables, FLAG_IS_ASYNC, FLAG_IS_STATIC,
    NONE, NONE_STR,
};
use crate::walker::named_kids;
use crate::walker::{Scope, ValueScope, Cand, scope_qualified_name};
use crate::textutil::{is_literal_receiver, strip_generic_and_qualifier, capitalized_re};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;






/// A property's CODE children: the named child right after the `=` token, a
/// `property_delegate` (`by lazy { … }`), and an accessor the grammar nested
/// under the declaration (`val x: Int get() = compute()` — written on ONE line;
/// an accessor on its own line parses as a SIBLING of the property and is not
/// reachable from here). What stays unwalked is the declaration itself —
/// modifiers, the `val`/`var` keyword, the name+type, and an extension
/// receiver's type and type parameters. (Go's #693 fix walks the `value` field
/// for the same reason; this grammar exposes no fields at all, hence the `=`
/// anchor.)
fn property_initializers<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut out: Vec<Node<'t>> = Vec::new();
    let mut after_eq = false;
    for i in 0..node.child_count() {
        let Some(c) = node.child(i) else { continue };
        if !c.is_named() {
            if c.kind() == "=" {
                after_eq = true;
            }
            continue;
        }
        if after_eq {
            out.push(c);
            after_eq = false;
        } else if matches!(c.kind(), "property_delegate" | "getter" | "setter") {
            out.push(c);
        }
    }
    out
}

/// Accessors written on their OWN line parse as SIBLINGS of the property, not
/// as children of it (same-line ones nest — see property_initializers). Walking
/// back over any accessors between us and the declaration finds the property an
/// accessor belongs to; None when this accessor stands alone.
fn accessor_owner<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut p = node.prev_named_sibling();
    while let Some(n) = p {
        if matches!(n.kind(), "getter" | "setter") {
            p = n.prev_named_sibling();
            continue;
        }
        return if n.kind() == "property_declaration" { Some(n) } else { None };
    }
    None
}

/// The sibling accessors that follow a property declaration, in source order.
fn following_accessors<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut n = node.next_named_sibling();
    while let Some(c) = n {
        if !matches!(c.kind(), "getter" | "setter") {
            break;
        }
        out.push(c);
        n = c.next_named_sibling();
    }
    out
}



#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<u8>,
    is_static: Option<bool>,
    is_async: Option<bool>,
    return_type: Option<String>,
    /// composeReceiverQualifiedName override (extension methods) — the id
    /// still hashes the bare NAME; only the qualifiedName column changes.
    qualified_override: Option<String>,
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
    /// Type-like rows (struct/union/class/enum/trait) by name, in creation
    /// order: the TS owner lookups scan `this.nodes` for the FIRST
    /// earlier-in-file match of a kind set.
    type_rows: HashMap<String, Vec<(u32, &'static str)>>,
    defined_fn_names: HashSet<String>,
    imported_names: HashSet<String>,
    fn_ref_cands: Vec<Cand>,
    fs_values: HashMap<String, u32>,
    fs_value_counts: HashMap<String, u32>,
    value_scopes: Vec<ValueScope<'t>>,
    line_count: u32,
}

pub fn extract(file_path: &str, source: &str) -> Result<EmitOut, String> {
    let t0 = std::time::Instant::now();
    let tree = crate::langs::parse("kotlin", source)?;

    let mut w = Walker::new(source, file_path);

    let line_count = w.line_count;
    let base_name = crate::buffers::push_file_node(&mut w.arena, &mut w.tables, file_path, line_count);
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });

    // extractFilePackage: the FIRST package_header among root's direct named
    // children → namespace node (comment-glued extents included), pushed for
    // the whole walk.
    let root = tree.root_node();
    let mut pkg_pushed = false;
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else { continue };
        if child.kind() != "package_header" {
            continue;
        }
        let id_node = named_kids(child)
            .find(|c| c.kind() == "identifier");
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
    /// The first row named `name` whose kind is in `kinds`.
    fn first_type_row(&self, name: &str, kinds: &[&str]) -> Option<u32> {
        self.type_rows.get(name)?.iter().find(|(_, k)| kinds.contains(k)).map(|(row, _)| *row)
    }

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
            type_rows: HashMap::new(),
            defined_fn_names: HashSet::new(),
            imported_names: HashSet::new(),
            fn_ref_cands: Vec::new(),
            fs_values: HashMap::new(),
            fs_value_counts: HashMap::new(),
            value_scopes: Vec::new(),
            line_count: source.bytes().filter(|b| *b == b'\n').count() as u32 + 1,
        }
    }
    markdown_refs_impl!();

    walker_pos_impl!();
    inside_class_like_impl!("class" | "struct" | "interface" | "trait" | "enum" | "module");

    push_ref_impl!();


    /// resolveBody (kotlin.ts:219): first ERROR child whose child(0) is `{`
    /// (fun-interface parent body — unreachable post-defer, kept for
    /// contract), else first function_body | class_body | enum_class_body.
    fn resolve_body(&self, node: Node<'t>) -> Option<Node<'t>> {
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() == "ERROR" {
                if let Some(first) = child.child(0) {
                    if first.kind() == "{" {
                        return Some(child);
                    }
                }
            }
            if matches!(child.kind(), "function_body" | "class_body" | "enum_class_body") {
                return Some(child);
            }
        }
        None
    }

    // --- createNode ------------------------------------------------------------

    fn create_node(&mut self, kind: &'static str, name: &str, node: Node<'t>, extra: Extra) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let start_line = self.line_of(node);
        let id = ids::node_id(self.file_path, kind, name, start_line);
        // endLine extension via resolveBody — LIVE for kotlin function/method
        // kinds (in-range for this grammar, so practically a no-op — but the
        // hook is part of the contract).
        let mut end_line = node.end_position().row as u32 + 1;
        if kind == "function" || kind == "method" {
            if let Some(body) = self.resolve_body(node) {
                let be = body.end_position().row as u32 + 1;
                if be > end_line {
                    end_line = be;
                }
            }
        }

        let qualified = match &extra.qualified_override {
            Some(qn) => qn.clone(),
            None => scope_qualified_name(&self.stack, name)
        };

        let mut flags = BoolFlags::default();
        if let Some(v) = extra.is_async {
            flags.set(FLAG_IS_ASYNC, v);
        }
        if let Some(v) = extra.is_static {
            flags.set(FLAG_IS_STATIC, v);
        }
        // extractModifiers merge (tree-sitter.ts:1355) — runs for EVERY
        // created node: expect/actual platform modifiers → decorators.
        let mods = self.extract_modifiers(node);
        let dec_ref: StrRef = match &mods {
            Some(list) if !list.is_empty() => self.arena.put_list(list),
            _ => NONE_STR,
        };
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
            decorators: dec_ref,
            type_parameters: NONE_STR,
            return_type: ret_ref,
            extra_json: NONE_STR,
        });
        self.node_ids.push(id);
        if matches!(kind, "struct" | "union" | "class" | "enum" | "trait") {
            self.type_rows.entry(name.to_string()).or_default().push((row, kind));
        }

        let parent_row = self.top_row();
        self.tables.push_contains(parent_row, row);

        if kind == "function" || kind == "method" {
            self.defined_fn_names.insert(name.to_string());
        }
        // captureValueRefScope — namespace parents are NOT accepted, so
        // packaged files' top-level constants are never targets (quirk).
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
        self.emit_decl_binding(kind, name, row, node, extra.visibility);
        if kind == "function" || kind == "method" {
            self.emit_param_bindings(node);
        }
        Some(row)
    }

    // --- hooks (languages/kotlin.ts) ----------------------------------------------

    /// extractName — the zero-field grammar means the nameField lookup always
    /// misses; names come from the shared fallback scan (first direct
    /// identifier-family child; backtick names keep their backticks).
    fn extract_name(&self, node: Node) -> String {
        if let Some(name_node) = node.child_by_field_name("simple_identifier") {
            // nameField is a TYPE name used as a FIELD name — never resolves
            // (mirrored for shape; the grammar has zero fields).
            return self.text(name_node).to_string();
        }
        for c in named_kids(node) {
            if matches!(c.kind(), "identifier" | "type_identifier" | "simple_identifier" | "constant") {
                return self.text(c).to_string();
            }
        }
        "<anonymous>".to_string()
    }

    /// getVisibility: modifiers text includes public/private/protected/
    /// internal in that order; default PUBLIC. Text-includes semantics —
    /// annotation text inside modifiers can flip it (bug-for-bug).
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
                if text.contains("protected") {
                    return 3;
                }
                if text.contains("internal") {
                    return 4;
                }
            }
        }
        1 // Kotlin defaults to public
    }

    /// isAsync: modifiers text includes 'suspend' (text-includes false
    /// positive on `@suspendMarker` annotations — preserve).
    fn is_async(&self, node: Node) -> bool {
        (0..node.child_count())
            .filter_map(|i| node.child(i))
            .any(|c| c.kind() == "modifiers" && self.text(c).contains("suspend"))
    }

    /// `(params): ReturnType` — the positional read TreeSitterExtractor's
    /// kotlin getSignature does (#1495): the `function_value_parameters` child,
    /// then the type node that follows it before the body. Verbatim source text,
    /// so it round-trips through parity byte-for-byte.
    fn signature_of(&self, node: Node) -> Option<String> {
        let mut params: Option<Node> = None;
        let mut return_type: Option<Node> = None;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() == "function_value_parameters" {
                params = Some(child);
                continue;
            }
            if params.is_none() {
                continue;
            }
            if matches!(child.kind(), "function_body" | "type_constraints") {
                break;
            }
            if matches!(child.kind(), "user_type" | "nullable_type" | "function_type") {
                return_type = Some(child);
                break;
            }
        }
        let params = params?;
        let mut sig = self.text(params).to_string();
        if let Some(rt) = return_type {
            sig.push_str(": ");
            sig.push_str(self.text(rt));
        }
        Some(sig)
    }

    /// extractKotlinReturnType — positional: the first user_type/nullable_type
    /// AFTER function_value_parameters; function_body/type_constraints first →
    /// None; Unit/Nothing → None; `: T` generic params leak (preserve).
    fn return_type_of(&self, node: Node) -> Option<String> {
        let mut seen_params = false;
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() == "function_value_parameters" {
                seen_params = true;
                continue;
            }
            if !seen_params {
                continue;
            }
            if matches!(child.kind(), "function_body" | "type_constraints") {
                return None;
            }
            if matches!(child.kind(), "user_type" | "nullable_type") {
                let ut = if child.kind() == "nullable_type" {
                    named_kids(child)
                        .find(|c| c.kind() == "user_type")
                        .unwrap_or(child)
                } else {
                    child
                };
                let type_id = named_kids(ut)
                    .find(|c| c.kind() == "type_identifier");
                let name = self.text(type_id.unwrap_or(ut)).trim();
                if name.is_empty() || !crate::textutil::ascii_ident_re().is_match(name) {
                    return None;
                }
                if matches!(name, "Unit" | "Nothing") {
                    return None;
                }
                return Some(name.to_string());
            }
        }
        None
    }

    /// getReceiverType — extension functions: the last user_type BEFORE a `.`
    /// child; its FIRST type_identifier's text (qualified receivers take the
    /// FIRST segment — the `com::qext` bug, preserve).
    fn receiver_type_of(&self, node: Node<'t>) -> Option<String> {
        let mut found_user_type: Option<Node> = None;
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            match child.kind() {
                "user_type" => found_user_type = Some(child),
                "." => {
                    if let Some(ut) = found_user_type {
                        let type_id = named_kids(ut)
                            .find(|c| c.kind() == "type_identifier");
                        return Some(self.text(type_id.unwrap_or(ut)).to_string());
                    }
                }
                "simple_identifier" | "function_value_parameters" => break,
                _ => {}
            }
        }
        None
    }

    // --- the visitNode hook (fun-interface recovery + property branch) ----------------

    // --- the dispatcher (visitNode, Kotlin-relevant branches) -----------------------

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

        if kind == "function_declaration" {
            if self.inside_class_like() {
                self.extract_method(node);
            } else {
                self.extract_function(node);
            }
            skip_children = true;
        } else if kind == "class_declaration" {
            // classifyClassNode: `interface`/`enum` keyword children.
            let mut classified = "class";
            for i in 0..node.child_count() {
                if let Some(c) = node.child(i) {
                    if c.kind() == "interface" {
                        classified = "interface";
                        break;
                    }
                    if c.kind() == "enum" {
                        classified = "enum";
                        break;
                    }
                }
            }
            match classified {
                "interface" => self.extract_interface(node),
                "enum" => self.extract_enum(node),
                _ => self.extract_class(node),
            }
            skip_children = true;
        } else if kind == "object_declaration" {
            // extraClassNodeTypes → extractClass → kind `class`.
            self.extract_class(node);
            skip_children = true;
        } else if kind == "type_alias" {
            skip_children = self.extract_type_alias(node);
        } else if kind == "property_declaration" {
            // Hook-declined destructuring: extractField/extractVariable both
            // find no matching children for kotlin — NOTHING minted, RHS
            // invisible; candidates-only scan.
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "import_header" {
            self.extract_import(node);
        } else if kind == "call_expression" {
            self.extract_call(node);
        }
        // companion_object, anonymous_initializer, secondary_constructor,
        // getter/setter siblings, file_annotation, object_literal, if/when at
        // top level: no branch — recursed (calls attribute to the stack top).

        if !skip_children {
            for c in named_kids(node) {
                self.visit_node(c);
            }
        }
    }

    // --- visitFunctionBody ----------------------------------------------------------


    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        if node.kind() == "property_declaration" {
            self.emit_local_rows(node);
        }
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "call_expression" {
            self.extract_call(node);
        }
        // (INSTANTIATION_KINDS has no kotlin members; extractBareCall absent.)

        self.extract_static_member_ref(node);

        if kind == "function_declaration" {
            let name = self.extract_name(node);
            if name != "<anonymous>" {
                // extractFunction diverts receiver-bearing nested fns to
                // extractMethod itself.
                self.extract_function(node);
                return;
            }
        }
        if kind == "class_declaration" {
            let mut classified = "class";
            for i in 0..node.child_count() {
                if let Some(c) = node.child(i) {
                    if c.kind() == "interface" {
                        classified = "interface";
                        break;
                    }
                    if c.kind() == "enum" {
                        classified = "enum";
                        break;
                    }
                }
            }
            match classified {
                "interface" => self.extract_interface(node),
                "enum" => self.extract_enum(node),
                _ => self.extract_class(node),
            }
            return;
        }
        // object_declaration is NOT dispatched here — a body-local object's
        // `fun`s hit the function branch above and leak out as FUNCTIONS
        // under the enclosing fn; its properties mint nothing (quirk).

        for c in named_kids(node) {
            self.visit_for_calls_and_structure(c);
        }
    }

    // --- extractors ------------------------------------------------------------------

    fn extract_function(&mut self, node: Node<'t>) {
        stack_guard!();
        // getReceiverType short-circuit (1522) — extension fns at any scope.
        if self.receiver_type_of(node).is_some() {
            self.extract_method(node);
            return;
        }
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            if let Some(body) = self.resolve_body(node) {
                self.visit_for_calls_and_structure(body);
            }
            return;
        }
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: self.signature_of(node),
            visibility: Some(self.visibility_of(node)),
            is_async: Some(self.is_async(node)),
            is_static: Some(false), // kotlin isStatic is always false
            return_type: self.return_type_of(node),
            ..Extra::default()
        };
        let Some(row) = self.create_node("function", &name, node, extra) else { return };
        // extractTypeAnnotations: the generic path's field lookups all miss
        // (zero fields) — kotlin emits ZERO type-annotation refs.
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = self.resolve_body(node) {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        let receiver = self.receiver_type_of(node);
        let name = self.extract_name(node);
        let qualified_override = receiver.as_ref().map(|r| format!("{r}::{name}"));
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: self.signature_of(node),
            visibility: Some(self.visibility_of(node)),
            is_async: Some(self.is_async(node)),
            is_static: Some(false),
            return_type: self.return_type_of(node),
            qualified_override,
        };
        let Some(row) = self.create_node("method", &name, node, extra) else { return };
        // Owner-contains fallback (1799): receiver present, not class-like →
        // the FIRST same-file node named like the receiver with kind ∈
        // {struct, class, enum, trait} (interface EXCLUDED; source-order
        // dependent — both quirks preserved). Additive to the normal edge.
        if let Some(recv) = &receiver {
            if !self.inside_class_like() {
                let owner = self.first_type_row(recv, &["struct", "class", "enum", "trait"]);
                if let Some(owner_row) = owner {
                    self.tables.push_contains(owner_row, row);
                }
            }
        }
        // Type annotations: dead. Decorators: live.
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "method", name });
        if let Some(body) = self.resolve_body(node) {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        let resolved_body = self.resolve_body(node);
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("class", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        // primaryCtor refs: csharp-gated no-op.
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "class", name });
        // Bodied: ONLY class_body children (primary-ctor properties/defaults
        // invisible). Bodiless: the class node itself → header children
        // visited → ctor default-value + super-arg calls attribute to the
        // CLASS (the asymmetry, pinned).
        let body = resolved_body.unwrap_or(node);
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
            ..Extra::default() // NO visibility
        };
        let Some(row) = self.create_node("interface", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "interface", name });
        let body = self.resolve_body(node).unwrap_or(node);
        for c in named_kids(body) {
            self.visit_node(c);
        }
        self.stack.pop();
    }

    fn extract_enum(&mut self, node: Node<'t>) {
        stack_guard!();
        let Some(body) = self.resolve_body(node) else { return };
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
        // name field → null (zero fields) → the identifier-children scan: one
        // enum_member per direct simple_identifier, positioned AT the
        // identifier. Entry value_arguments and entry class_bodies (override
        // methods!) are never visited — invisible (quirk).
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if matches!(child.kind(), "simple_identifier" | "identifier" | "property_identifier") {
                let name = self.text(child).to_string();
                self.create_node("enum_member", &name, child, Extra::default());
            }
        }
    }

    /// extractTypeAlias — plain node; the alias-value ref walk reads the
    /// `value` FIELD → null (zero fields) → NO refs. Returns false →
    /// children re-visited (harmless).
    fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default()
        };
        self.create_node("type_alias", &name, node, extra);
        false
    }

    fn extract_import(&mut self, node: Node<'t>) {
        // Comment-gluing: the header's extent (and thus the signature) can
        // include trailing comment lines — the trimmed FULL text is the
        // signature; the ref stays at the header start.
        let import_text = self.text(node).trim().to_string();
        let identifier = named_kids(node)
            .find(|c| c.kind() == "identifier");
        let Some(identifier) = identifier else { return };
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
        self.import_row_of(node, &module_name);
    }

    // --- bindings (resolution-binding-model-plan.md, Phase 3: JVM) --------------------

    enclosing_scope_impl!("file" | "namespace");

    push_binding_row_impl!();

    decorators_impl!();


    // --- function-as-value refs (KOTLIN_SPEC, function-ref.ts:240) ------------------

    flush_fn_ref_candidates_impl!();

    // --- value references --------------------------------------------------------------

}


