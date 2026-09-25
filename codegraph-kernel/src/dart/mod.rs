//! Dart extraction — a faithful Rust port of the dart paths of
//! `TreeSitterExtractor` (src/extraction/tree-sitter.ts) plus
//! languages/dart.ts.
//!
//! Same porting contract as the other walkers: behavior parity, bug-for-bug.
//! The authoritative quirk list is docs/design/dart-kernel-port-checklist.md.
//! The center of gravity is THE SIBLING-BODY DOUBLE-WALK: dart attaches every
//! function/method body as a NEXT SIBLING of its signature node, and the TS
//! walkers consume the body TWICE — once via resolveBody (attributed to the
//! function/method) and once via the enclosing generic walk (attributed to
//! the file/class). The deterministic result — duplicate local-function
//! nodes with the SAME id under different parents, duplicated
//! calls/instantiates refs, file/class-attributed fn-ref twins — must be
//! reproduced byte-for-byte in the observed interleave; a "helpful" dedupe
//! breaks parity. Other load-bearing oddities preserved on purpose:
//! callTypes is EMPTY (all call refs ride extractBareCall's selector
//! walking in the body walker — cascades are invisible, `?.` encodes like
//! `.`); `ConfigT.load()` double-emits (calls + a static-member references
//! ref — no callee-of-call skip in the dart branch); operator methods mint
//! `method "<anonymous>"`; the unnamed constructor is skipped
//! (isMisparsedFunction) while named ctors/factories are named by the CTOR
//! name with the class as returnType; instance fields mint NO nodes (only
//! static_final_declaration → constant, via the hook); prefixed return
//! types keep the PREFIX (`other.OtherClass f()` → returnType `other` —
//! bug, preserved); enum `with` mixins emit nothing while enum `implements`
//! works; deferred imports are invisible; named-argument callbacks are NOT
//! fn-ref-captured; `async*`/`sync*` are NOT async. Positions in UTF-16
//! code units. Files with parse errors are walked like any other (tree-sitter's recovery is canonical; buffers::parse_collapse_warning reports a collapsed parse).

mod calls;
mod refs;
use crate::buffers::{
    edge_kind_index, node_kind_index, Arena, BoolFlags, EdgeRow, EmitOut, NodeRow,
    RefRow, StrRef, Tables, FLAG_IS_ASYNC, FLAG_IS_STATIC,
    NONE, NONE_STR,
};
use crate::walker::{Scope, ValueScope, Cand};
use crate::textutil::{is_stoplisted, is_builtin_type};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;








#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    /// 0 = absent; 1 public, 2 private.
    visibility: u8,
    is_async: Option<bool>,
    is_static: Option<bool>,
    return_type: Option<String>,
    /// resolveBody-driven endLine extension (LIVE for dart sibling bodies).
    end_line_override: Option<u32>,
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
    let tree = crate::langs::parse("dart", source)?;

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

    // File node (tree-sitter.ts:508-521).
    let line_count = w.cols.line_count();
    let base_name = crate::buffers::push_file_node(&mut w.arena, &mut w.tables, file_path, line_count);
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });

    w.visit(tree.root_node());
    w.flush_fn_ref_candidates();
    w.flush_value_refs(tree.root_node());
    w.stack.pop();

    Ok(crate::buffers::finish(w.arena, w.tables, tree.root_node().has_error(), file_path, t0))
}

impl<'t> Walker<'t> {
    markdown_refs_impl!();

    walker_pos_impl!();
    inside_class_like_impl!("class" | "struct" | "interface" | "trait" | "enum" | "module");

    fn push_ref_at(&mut self, from_row: u32, name: &str, kind: &str, node: Node) {
        let name_ref = self.arena.put(name);
        self.tables.push_ref(&RefRow {
            from_idx: from_row,
            kind: edge_kind_index(kind).unwrap(),
            line: self.line_of(node),
            column: self.col_of(node),
            reference_name: name_ref,
            candidates: NONE_STR,
            from_id_str: NONE_STR,
        });
        // Dart import names are URIs (`package:x/y.dart`) — they match neither
        // SIMPLE_NAME nor QUALIFIED_IMPORT, so importedNames stays empty in
        // practice; ported for fidelity.
        if kind == "imports" {
            if util::simple_name().is_match(name) {
                self.imported_names.insert(name.to_string());
            } else if let Some(c) = util::qualified_import().captures(name) {
                self.imported_names.insert(c[1].to_string());
            }
        }
    }

    // --- createNode (tree-sitter.ts:1308) ---------------------------------

    fn create_node(&mut self, kind: &'static str, name: &str, node: Node<'t>, extra: Extra) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let start_line = self.line_of(node);
        let id = ids::node_id(self.file_path, kind, name, start_line);

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

        // endLine extension (:1322-1334) — LIVE for dart: a function/method
        // node's endLine extends to its sibling function_body's end.
        let mut end_line = node.end_position().row as u32 + 1;
        if let Some(ext) = extra.end_line_override {
            if ext > end_line {
                end_line = ext;
            }
        }

        let name_ref = self.arena.put(name);
        let qn_ref = self.arena.put(&qualified);
        let id_ref = self.arena.put(&id);
        let doc_ref = self.arena.put_opt(extra.docstring.as_deref());
        let sig_ref = self.arena.put_opt(extra.signature.as_deref());
        let ret_ref = self.arena.put_opt(extra.return_type.as_deref());
        let mut flags = BoolFlags::default();
        if let Some(v) = extra.is_async {
            flags.set(FLAG_IS_ASYNC, v);
        }
        if let Some(v) = extra.is_static {
            flags.set(FLAG_IS_STATIC, v);
        }
        let row = self.tables.push_node(&NodeRow {
            kind: node_kind_index(kind).unwrap(),
            visibility: extra.visibility,
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
        self.node_ids.push(id.clone());
        if kind == "function" || kind == "method" {
            self.defined_fn_names.insert(name.to_string());
        }

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

        // captureValueRefScope (:735-767). Dart mints only `constant` targets.
        if (kind == "constant" || kind == "variable")
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

    // --- languages/dart.ts helper transcriptions --------------------------

    /// dartInnerSignature (dart.ts:9-17).
    fn inner_signature(&self, node: Node<'t>) -> Node<'t> {
        if node.kind() == "method_signature" {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).find(|c| {
                matches!(c.kind(), "function_signature" | "getter_signature" | "setter_signature")
            });
            if let Some(inner) = inner {
                return inner;
            }
        }
        node
    }

    /// dartConstructorSignature (dart.ts:25-35).
    fn constructor_signature(&self, node: Node<'t>) -> Option<Node<'t>> {
        if matches!(node.kind(), "factory_constructor_signature" | "constructor_signature") {
            return Some(node);
        }
        if node.kind() == "method_signature" {
            let mut cursor = node.walk();
            return node.named_children(&mut cursor).find(|c| {
                matches!(c.kind(), "factory_constructor_signature" | "constructor_signature")
            });
        }
        None
    }

    /// dartEnclosingTypeName (dart.ts:38-50).
    fn enclosing_type_name(&self, node: Node<'t>) -> Option<&'t str> {
        let mut p = node.parent();
        while let Some(parent) = p {
            if matches!(
                parent.kind(),
                "class_definition" | "mixin_declaration" | "extension_declaration" | "enum_declaration"
            ) {
                return parent.child_by_field_name("name").map(|n| self.text(n));
            }
            p = parent.parent();
        }
        None
    }

    /// dartCtorInfo (dart.ts:61-70).
    fn ctor_info(&self, node: Node<'t>) -> Option<(String, String)> {
        let ctor = self.constructor_signature(node)?;
        let mut cursor = ctor.walk();
        let ids: Vec<Node<'t>> = ctor
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "identifier")
            .collect();
        let class_name = self.enclosing_type_name(node)?;
        let first = ids.first()?;
        if self.text(*first) != class_name {
            return None; // misparsed method, not a ctor
        }
        let ctor_name = ids.get(1).map(|n| self.text(*n)).unwrap_or(class_name);
        Some((class_name.to_string(), ctor_name.to_string()))
    }

    /// extractDartReturnType (dart.ts:80-92).
    fn return_type_of(&self, node: Node<'t>) -> Option<String> {
        if let Some((class_name, _)) = self.ctor_info(node) {
            return Some(class_name);
        }
        let sig = self.inner_signature(node);
        let mut cursor = sig.walk();
        let ret = sig
            .named_children(&mut cursor)
            .find(|c| c.kind() == "type_identifier")?;
        let text = crate::textutil::generic_args_re().replace_all(self.text(ret), "");
        let text = text.trim();
        let last = text.split('.').next_back()?;
        if last.is_empty() || !crate::textutil::ascii_ident_re().is_match(last) {
            return None;
        }
        Some(last.to_string())
    }

    /// isMisparsedFunction (dart.ts:177-188) — skip the UNNAMED constructor.
    fn is_unnamed_ctor(&self, node: Node<'t>) -> bool {
        match self.ctor_info(node) {
            Some((class_name, ctor_name)) => ctor_name == class_name,
            None => false,
        }
    }

    /// getSignature (dart.ts:189-208).
    fn signature_of(&self, node: Node<'t>) -> Option<String> {
        let sig = self.inner_signature(node);
        let mut c1 = sig.walk();
        let params = sig
            .named_children(&mut c1)
            .find(|c| c.kind() == "formal_parameter_list");
        let mut c2 = sig.walk();
        let ret = sig
            .named_children(&mut c2)
            .find(|c| matches!(c.kind(), "type_identifier" | "void_type"));
        if params.is_none() && ret.is_none() {
            return None;
        }
        let mut result = String::new();
        if let Some(r) = ret {
            result.push_str(self.text(r));
            result.push(' ');
        }
        if let Some(p) = params {
            result.push_str(self.text(p));
        }
        let trimmed = result.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// getVisibility (dart.ts:209-222) — `_` prefix = private; every
    /// constructor is public (the unwrap misses ctor signatures / the name
    /// FIELD is the class identifier).
    fn visibility_of(&self, node: Node<'t>) -> u8 {
        let name_node = if node.kind() == "method_signature" {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).find(|c| {
                matches!(c.kind(), "function_signature" | "getter_signature" | "setter_signature")
            });
            inner.and_then(|i| {
                let mut ic = i.walk();
                let found = i.named_children(&mut ic).find(|c| c.kind() == "identifier");
                found
            })
        } else {
            node.child_by_field_name("name")
        };
        match name_node {
            Some(n) if self.text(n).starts_with('_') => 2,
            _ => 1,
        }
    }

    /// isAsync (dart.ts:223-233) — the `async` anon child of the SIBLING
    /// function_body; `async*`/`sync*` are different token types → false.
    fn is_async_of(&self, node: Node<'t>) -> bool {
        if let Some(next) = node.next_named_sibling() {
            if next.kind() == "function_body" {
                for i in 0..next.child_count() {
                    if let Some(c) = next.child(i) {
                        if c.kind() == "async" {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// isStatic (dart.ts:234-243).
    fn is_static_of(&self, node: Node<'t>) -> bool {
        if node.kind() == "method_signature" {
            for i in 0..node.child_count() {
                if let Some(c) = node.child(i) {
                    if c.kind() == "static" {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// resolveBody (dart.ts:158-171).
    fn resolve_body(&self, node: Node<'t>) -> Option<Node<'t>> {
        if matches!(node.kind(), "function_signature" | "method_signature") {
            let next = node.next_named_sibling()?;
            if next.kind() == "function_body" {
                return Some(next);
            }
            return None;
        }
        if let Some(standard) = node.child_by_field_name("body") {
            return Some(standard);
        }
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .find(|c| matches!(c.kind(), "class_body" | "extension_body"));
        found
    }

    /// extractName (tree-sitter.ts:90-192) — resolveName (ctor names) →
    /// name field → the method_signature inner unwrap → identifier-ish
    /// child → `<anonymous>` (operators land here).
    fn extract_name(&self, node: Node<'t>) -> String {
        // resolveName hook (dart.ts:244-260): named ctor/factory → ctor name.
        if let Some((class_name, ctor_name)) = self.ctor_info(node) {
            if ctor_name != class_name {
                return ctor_name;
            }
        }
        if let Some(name_node) = node.child_by_field_name("name") {
            return self.text(name_node).to_string();
        }
        if node.kind() == "method_signature" {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).find(|c| {
                matches!(
                    c.kind(),
                    "function_signature" | "getter_signature" | "setter_signature"
                        | "constructor_signature" | "factory_constructor_signature"
                )
            });
            if let Some(inner) = inner {
                let mut ic = inner.walk();
                let id = inner.named_children(&mut ic).find(|c| c.kind() == "identifier");
                if let Some(id) = id {
                    return self.text(id).to_string();
                }
            }
        }
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if matches!(c.kind(), "identifier" | "type_identifier" | "simple_identifier" | "constant") {
                return self.text(c).to_string();
            }
        }
        "<anonymous>".to_string()
    }

    // --- the main walk (visitNode, tree-sitter.ts:936-1303) ---------------

    fn visit(&mut self, node: Node<'t>) {
        stack_guard!();
        // The visitNode hook (dart.ts:144-157) — the constants branch.
        if node.kind() == "static_final_declaration" {
            let mut cursor = node.walk();
            let name_node = node.named_children(&mut cursor).find(|c| c.kind() == "identifier");
            if let Some(name_node) = name_node {
                // signature = first value sibling's text, sliced to 100
                // UTF-16 units (a flattened chain captures just its head).
                let signature = name_node.next_named_sibling().map(|v| {
                    let sliced = util::slice_utf16(self.text(v), 100);
                    if util::utf16_len(&sliced) >= 100 {
                        format!("= {sliced}...")
                    } else {
                        format!("= {sliced}")
                    }
                });
                let name = self.text(name_node).to_string();
                self.create_node("constant", &name, node, Extra { signature, ..Default::default() });
            }
            self.scan_fn_ref_subtree(node, 0);
            return;
        }

        // maybeCaptureFnRefs (:990) — the double-walk fn-ref twin source.
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        match node.kind() {
            "function_signature" => {
                // functionTypes row — method_signature does NOT include it →
                // always extractFunction, even inside a class (abstract
                // members become kind `function` contained by the class).
                self.extract_function(node);
                return;
            }
            "class_definition" | "mixin_declaration" | "extension_declaration" => {
                self.extract_class(node);
                return;
            }
            "method_signature" | "constructor_signature" => {
                self.extract_method(node);
                return;
            }
            "enum_declaration" => {
                self.extract_enum(node);
                return;
            }
            "type_alias" => {
                let skip = self.extract_type_alias(node);
                if skip {
                    return;
                }
            }
            "import_or_export" => {
                self.extract_import(node);
                return;
            }
            "new_expression" => {
                // INSTANTIATION_KINDS row — from the FILE/CLASS on the
                // sibling revisit (the double-walk's pass 2a).
                self.extract_instantiation(node);
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child);
        }
    }

    // --- extractFunction / extractMethod (:1517 / :1737) ------------------

    fn extract_function(&mut self, node: Node<'t>) {
        stack_guard!();
        // No receiver hook. Name first (resolveName inside extract_name).
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            // :1549 — body-only walk (nothing pushed). Dart signatures always
            // name; preserved for fidelity.
            if let Some(body) = self.resolve_body(node) {
                self.visit_body(body);
            }
            return;
        }
        // isMisparsedFunction: the unnamed constructor is skipped — node
        // suppressed, body still walked (attributed to the current stack top).
        if self.is_unnamed_ctor(node) {
            if let Some(body) = self.resolve_body(node) {
                self.visit_body(body);
            }
            return;
        }
        let docstring = preceding_docstring(node, self.src);
        let signature = self.signature_of(node);
        let visibility = self.visibility_of(node);
        let is_async = self.is_async_of(node);
        let is_static = self.is_static_of(node);
        let return_type = self.return_type_of(node);
        let body = self.resolve_body(node);
        let end_line_override = body.map(|b| b.end_position().row as u32 + 1);
        let row = self.create_node(
            "function",
            &name,
            node,
            Extra {
                docstring,
                signature,
                visibility,
                is_async: Some(is_async),
                is_static: Some(is_static),
                return_type,
                end_line_override,
            },
        );
        let Some(row) = row else { return };
        self.extract_type_annotations(node, row);
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = body {
            self.visit_body(body);
        }
        self.stack.pop();
    }

    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        // Gate (:1747): not inside class-like (no methodsAreTopLevel, no
        // receiver, parent never object/object_expression) → extractFunction.
        if !self.inside_class_like() {
            self.extract_function(node);
            return;
        }
        let name = self.extract_name(node);
        // isMisparsedFunction — the unnamed ctor: body-only walk.
        if self.is_unnamed_ctor(node) {
            if let Some(body) = self.resolve_body(node) {
                self.visit_body(body);
            }
            return;
        }
        let docstring = preceding_docstring(node, self.src);
        let signature = self.signature_of(node);
        let visibility = self.visibility_of(node);
        let is_async = self.is_async_of(node);
        let is_static = self.is_static_of(node);
        let return_type = self.return_type_of(node);
        let body = self.resolve_body(node);
        let end_line_override = body.map(|b| b.end_position().row as u32 + 1);
        // Operators mint method "<anonymous>" — extractMethod has NO skip.
        let row = self.create_node(
            "method",
            &name,
            node,
            Extra {
                docstring,
                signature,
                visibility,
                is_async: Some(is_async),
                is_static: Some(is_static),
                return_type,
                end_line_override,
            },
        );
        let Some(row) = row else { return };
        self.extract_type_annotations(node, row);
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "method", name });
        if let Some(body) = body {
            self.visit_body(body);
        }
        self.stack.pop();
    }

    // --- extractClass (:1679) — classes, mixins, extensions ---------------

    fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        let resolved_body = self.resolve_body(node);
        // No skipBodilessClass. Anonymous `extension on String` → the name
        // fallback finds the ON type's type_identifier — a class named after
        // the extended type (preserved).
        let name = self.extract_name(node);
        let docstring = preceding_docstring(node, self.src);
        let visibility = self.visibility_of(node);
        let row = self.create_node(
            "class",
            &name,
            node,
            Extra { docstring, visibility, ..Default::default() },
        );
        let Some(row) = row else { return };
        self.extract_inheritance(node, row);
        // extractCsharpPrimaryCtorParamRefs — csharp-gated no-op.
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: "class", name });
        let body = resolved_body.unwrap_or(node);
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            self.visit(child);
        }
        self.stack.pop();
    }

    // --- extractEnum (:1914) ----------------------------------------------

    fn extract_enum(&mut self, node: Node<'t>) {
        stack_guard!();
        let body = match self.resolve_body(node) {
            Some(b) => b,
            None => return,
        };
        let name = self.extract_name(node);
        let docstring = preceding_docstring(node, self.src);
        let visibility = self.visibility_of(node);
        let row = self.create_node(
            "enum",
            &name,
            node,
            Extra { docstring, visibility, ..Default::default() },
        );
        let Some(row) = row else { return };
        // Enum `with` mixins are a DIRECT child (no superclass wrapper) →
        // no clause matches; `interfaces` DOES → implements only.
        self.extract_inheritance(node, row);
        // No extractDecoratorsFor on the enum path.
        self.stack.push(Scope { row, kind: "enum", name });
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "enum_constant" {
                self.extract_enum_members(child);
            } else {
                self.visit(child);
            }
        }
        self.stack.pop();
    }

    /// extractEnumMembers (:1958) — one enum_member per constant, positioned
    /// at the enum_constant node; ctor arguments never walked.
    fn extract_enum_members(&mut self, node: Node<'t>) {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.text(name_node).to_string();
            self.create_node("enum_member", &name, node, Extra::default());
        }
    }

    // --- extractTypeAlias (:2890, plain path) -----------------------------

    fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let docstring = preceding_docstring(node, self.src);
        // `value` field is null (type_alias has no fields) → no refs from
        // the aliased type; returns false → children re-visited.
        self.create_node("type_alias", &name, node, Extra { docstring, ..Default::default() });
        false
    }

    // --- extractImport (:3170; hook dart.ts:261-304) ----------------------

    fn extract_import(&mut self, node: Node<'t>) {
        let find_child = |parent: Node<'t>, kind: &str| -> Option<Node<'t>> {
            let mut cursor = parent.walk();
            let found = parent.named_children(&mut cursor).find(|c| c.kind() == kind);
            found
        };
        let uri_of = |spec: Node<'t>| -> Option<Node<'t>> {
            let configurable = find_child(spec, "configurable_uri")?;
            let uri = find_child(configurable, "uri")?;
            find_child(uri, "string_literal")
        };
        let mut module: Option<String> = None;
        if let Some(li) = find_child(node, "library_import") {
            if let Some(spec) = find_child(li, "import_specification") {
                if let Some(sl) = uri_of(spec) {
                    module = Some(self.text(sl).replace(['\'', '"'], ""));
                }
            }
        }
        if module.is_none() {
            if let Some(le) = find_child(node, "library_export") {
                if let Some(sl) = uri_of(le) {
                    module = Some(self.text(sl).replace(['\'', '"'], ""));
                }
            }
        }
        // Deferred imports (bare `uri`, no configurable_uri) → hook null →
        // nothing at all (invisible).
        let Some(module) = module.filter(|m| !m.is_empty()) else { return };
        let signature = self.text(node).trim().to_string();
        let created = self.create_node(
            "import",
            &module,
            node,
            Extra { signature: Some(signature), ..Default::default() },
        );
        if created.is_some() {
            let parent_row = self.top_row();
            self.push_ref_at(parent_row, &module, "imports", node);
        }
    }

    // --- extractInstantiation (:4610, generic tail) -----------------------

    // --- extractBareCall (dart.ts:305-379) --------------------------------

    // --- extractStaticMemberRef — the dart branch (:4759-4767) ------------

    // --- extractDecoratorsFor (:4897-5024) — the sibling scan -------------

    fn extract_decorators_for(&mut self, decl: Node<'t>, decorated_row: u32) {
        // Scan 1: direct children (+ modifiers descent) — inert for dart
        // (annotations are preceding siblings), ported for fidelity.
        let mut cursor = decl.walk();
        for child in decl.named_children(&mut cursor) {
            self.consider_decorator(child, decorated_row);
            if child.kind() == "modifiers" {
                let mut mc = child.walk();
                let inner: Vec<Node<'t>> = child.named_children(&mut mc).collect();
                for m in inner {
                    self.consider_decorator(m, decorated_row);
                }
            }
        }
        // Scan 2: preceding siblings, backward, stop at the first
        // non-annotation — stacked annotations emit in REVERSE source order.
        if let Some(parent) = decl.parent() {
            let decl_start = decl.start_byte();
            let mut decl_idx: Option<usize> = None;
            for i in 0..parent.named_child_count() {
                if let Some(sib) = parent.named_child(i) {
                    if sib.start_byte() == decl_start {
                        decl_idx = Some(i);
                        break;
                    }
                }
            }
            if let Some(di) = decl_idx {
                for j in (0..di).rev() {
                    let Some(sib) = parent.named_child(j) else { continue };
                    if !matches!(sib.kind(), "decorator" | "annotation" | "marker_annotation") {
                        break;
                    }
                    self.consider_decorator(sib, decorated_row);
                }
            }
        }
    }

    fn consider_decorator(&mut self, n: Node<'t>, decorated_row: u32) {
        if !matches!(n.kind(), "decorator" | "annotation" | "marker_annotation" | "attribute") {
            return;
        }
        let mut target: Option<Node<'t>> = None;
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            if child.kind() == "call_expression" {
                let fnn = child.child_by_field_name("function").or_else(|| child.named_child(0));
                if let Some(f) = fnn {
                    target = Some(f);
                }
                if target.is_some() {
                    break;
                }
            }
            if matches!(
                child.kind(),
                "identifier" | "member_expression" | "scoped_identifier" | "navigation_expression"
                    | "user_type" | "type_identifier"
            ) {
                target = Some(child);
                break;
            }
        }
        let Some(target) = target else { return };
        let name = crate::textutil::strip_generic_and_qualifier(self.text(target));
        if name.is_empty() {
            return;
        }
        self.push_ref_at(decorated_row, &name, "decorates", n);
    }

    // --- extractInheritance — the dart rows (:5368-5393, :5437-5459) ------

    fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "superclass" {
                // extends type + `with` mixins (implements) — dart branch.
                let mut cc = child.walk();
                let targets: Vec<Node<'t>> = child.named_children(&mut cc).collect();
                for t in targets {
                    if t.kind() == "mixins" {
                        let mut mc = t.walk();
                        let mixins: Vec<Node<'t>> = t.named_children(&mut mc).collect();
                        for m in mixins {
                            if m.kind() == "type_identifier" {
                                let name = self.text(m).to_string();
                                self.push_ref_at(class_row, &name, "implements", m);
                            }
                        }
                    } else if t.kind() == "type_identifier" {
                        let name = self.text(t).to_string();
                        self.push_ref_at(class_row, &name, "extends", t);
                    }
                }
            } else if child.kind() == "interfaces" {
                // implements — one per named child, FULL child text.
                let mut cc = child.walk();
                let targets: Vec<Node<'t>> = child.named_children(&mut cc).collect();
                for iface in targets {
                    let name = self.text(iface).to_string();
                    self.push_ref_at(class_row, &name, "implements", iface);
                }
            }
        }
    }

    // --- extractTypeAnnotations — the dart path (:5819-5833) --------------

    fn extract_type_annotations(&mut self, node: Node<'t>, row: u32) {
        let sig = if node.kind() == "method_signature" {
            let mut cursor = node.walk();
            let found = node.named_children(&mut cursor).find(|c| {
                matches!(
                    c.kind(),
                    "function_signature" | "getter_signature" | "setter_signature"
                        | "constructor_signature" | "factory_constructor_signature"
                )
            });
            found.unwrap_or(node) // operators fall back to the wrapper itself
        } else {
            node
        };
        self.type_refs_from_subtree(sig, row);
    }

    fn type_refs_from_subtree(&mut self, node: Node<'t>, from_row: u32) {
        stack_guard!();
        if node.kind() == "type_identifier" {
            let name = self.text(node);
            if !name.is_empty() && !is_builtin_type(name) {
                let name = name.to_string();
                self.push_ref_at(from_row, &name, "references", node);
            }
            return;
        }
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            self.type_refs_from_subtree(c, from_row);
        }
    }

    // --- visitFunctionBody (:5129-5286) — dart rows -----------------------

    fn visit_body(&mut self, node: Node<'t>) {
        stack_guard!();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        let kind = node.kind();
        if kind == "new_expression" {
            // INSTANTIATION branch fires first — extractBareCall's
            // new_expression arm is dead. Children still recursed.
            self.extract_instantiation(node);
        } else if let Some(callee) = self.bare_call_name(node) {
            // extractBareCall (:5159-5173) — ref at the MATCHED node.
            let caller_row = self.top_row();
            self.push_ref_at(caller_row, &callee, "calls", node);
        }

        self.extract_static_member_ref(node);

        if kind == "function_signature" {
            // Nested named functions (:5245) — extractFunction walks the
            // nested body itself; the enclosing walker ALSO revisits the
            // sibling function_body (double-walk pass 2b) via recursion.
            self.extract_function(node);
            return;
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit_body(child);
        }
    }

    // --- function-as-value capture (#756) — DART_SPEC ---------------------

    flush_fn_ref_candidates_impl!();

    // --- value-reference edges (:398-931) ---------------------------------

}

