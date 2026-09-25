//! Scala extraction — a faithful Rust port of the scala paths of
//! `TreeSitterExtractor` (src/extraction/tree-sitter.ts) plus
//! languages/scala.ts.
//!
//! Same porting contract as the other walkers: behavior parity, bug-for-bug.
//! The authoritative quirk list is docs/design/scala-kernel-port-checklist.md —
//! including the load-bearing oddities this file preserves on purpose:
//! functionTypes is EMPTY so every def routes through extractMethod (top level
//! falls back to a `function` node); NO namespace node ever (package headers
//! ignored, QNs bare); imports are named the FIRST path segment (`import
//! com.example.C` → `com`); the val/var hook keys on the enclosing-definition
//! NODE TYPE (object vals → constants, class/trait/enum/given vals → fields)
//! and consumes the initializer (no calls/instantiates from hook-consumed
//! initializers); extension methods mint NO nodes (the first def's body calls
//! leak to the enclosing scope, every later def is invisible, and the braced
//! form resolves its `body` field to the `{` TOKEN — whole extension
//! invisible); anonymous `new T { … }` bodies leak their defs to the
//! enclosing scope (findAnonymousClassBody misses template_body); nested
//! defs in bodies mint NOTHING (inverse of kotlin); the bodied-vs-bodiless
//! class asymmetry (bodiless headers walk class_parameters → default-value
//! calls emit from the class; bodied ones never see them); curried signatures
//! keep only the FIRST parameter list and type params win the `parameters`
//! field; static-member WRITES emit (unlike kotlin); infix calls are
//! invisible; `derives` emits nothing; value-ref same-name targets take the
//! LAST registration. Positions in UTF-16 code units.
//! Files with parse errors are walked like any other (tree-sitter's recovery is canonical; buffers::parse_collapse_warning reports a collapsed parse).

mod calls;
mod refs;
use crate::buffers::{
    edge_kind_index, node_kind_index, Arena, BoolFlags, EdgeRow, EmitOut, NodeRow,
    RefRow, StrRef, Tables, FLAG_IS_ASYNC, FLAG_IS_STATIC,
    NONE, NONE_STR,
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





/// SCALA_BUILTIN_TYPES (languages/scala.ts:14-17) — the hook's OWN smaller set.
fn is_scala_builtin(name: &str) -> bool {
    matches!(
        name,
        "Int" | "Long" | "Short" | "Byte" | "Float" | "Double" | "Boolean" | "Char" | "Unit"
            | "String" | "Any" | "AnyRef" | "AnyVal" | "Nothing" | "Null"
    )
}

/// extractScalaReturnType's generic-args strip (`/\[[^\]]*\]/g`).
fn bracket_args_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[[^\]]*\]").unwrap())
}
/// JS `\s+` for the re-encode/return-type strips (Unicode whitespace).
fn ws_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}




#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    /// 0 = absent; 1 public, 2 private, 3 protected.
    visibility: u8,
    /// (present, value) — isAsync/isStatic are literal-false hooks for scala.
    is_async: Option<bool>,
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
    fs_value_counts: HashMap<String, u32>,
    value_scopes: Vec<ValueScope<'t>>,
}

pub fn extract(file_path: &str, source: &str) -> Result<EmitOut, String> {
    let t0 = std::time::Instant::now();
    let tree = crate::langs::parse("scala", source)?;

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

    // No packageTypes → no namespace node, ever.
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
        // flushFnRefCandidates' importedNames (tree-sitter.ts:661-675). Scala
        // import refs are named the FIRST path segment — always SIMPLE_NAME.
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

        // buildQualifiedName (:1447-1460) — non-file stack names, `::`-joined;
        // namespacePrefix always empty (no C++ namespaces, no scala namespace).
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
            end_line: node.end_position().row as u32 + 1, // no resolveBody hook
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

        // captureValueRefScope (:735-767).
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
                self.fs_values.insert(name.to_string(), row); // LAST wins
                *self.fs_value_counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
        if matches!(kind, "function" | "method" | "constant" | "variable") {
            self.value_scopes.push(ValueScope { row, node, name: name.to_string() });
        }

        Some(row)
    }

    // --- languages/scala.ts helper transcriptions -------------------------

    /// getValVarName (scala.ts:5-11).
    fn val_var_name(&self, node: Node<'t>) -> Option<&'t str> {
        let pattern = node.child_by_field_name("pattern")?;
        if pattern.kind() == "identifier" {
            return Some(self.text(pattern));
        }
        let mut cursor = pattern.walk();
        for c in pattern.named_children(&mut cursor) {
            if c.kind() == "identifier" {
                return Some(self.text(c));
            }
        }
        None
    }

    /// extractVisibility (scala.ts:69-80) → wire byte (1 public default).
    fn visibility_of(&self, node: Node<'t>) -> u8 {
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() == "modifiers" || c.kind() == "access_modifier" {
                let t = self.text(c);
                if t.contains("private") {
                    return 2;
                }
                if t.contains("protected") {
                    return 3;
                }
            }
        }
        1
    }

    /// isStatic (scala.ts:123-129) — text scan, effectively always false.
    fn is_static_of(&self, node: Node<'t>) -> bool {
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() == "modifiers" && self.text(c).contains("static") {
                return true;
            }
        }
        false
    }

    /// getSignature (scala.ts:110-117) — first-match-wins fields: curried
    /// defs keep only the first list; a type_parameters node carrying field
    /// `parameters` wins over the value list.
    fn signature_of(&self, node: Node<'t>) -> Option<String> {
        let params = node.child_by_field_name("parameters");
        let ret = node.child_by_field_name("return_type");
        if params.is_none() && ret.is_none() {
            return None;
        }
        let mut sig = params.map(|p| self.text(p).to_string()).unwrap_or_default();
        if let Some(r) = ret {
            sig.push_str(": ");
            sig.push_str(self.text(r));
        }
        if sig.is_empty() {
            None
        } else {
            Some(sig)
        }
    }

    /// extractScalaReturnType (scala.ts:56-67).
    fn return_type_of(&self, node: Node<'t>) -> Option<String> {
        let rt = node.child_by_field_name("return_type")?;
        let raw = self.text(rt).trim();
        if raw.starts_with("this.") {
            return None;
        }
        let base = bracket_args_re().replace_all(raw, "");
        let base = ws_re().replace_all(&base, "");
        let last = base.split('.').next_back()?;
        if last.is_empty() || !crate::textutil::ascii_ident_re().is_match(last) {
            return None;
        }
        Some(last.to_string())
    }

    /// scalaBaseTypeName (tree-sitter.ts:201-224).
    fn scala_base_type_name(&self, node: Option<Node<'t>>) -> Option<String> {
        stack_guard!();
        let node = node?;
        match node.kind() {
            "type_identifier" | "identifier" => Some(self.text(node).to_string()),
            "generic_type" => self.scala_base_type_name(node.named_child(0)),
            "stable_type_identifier" | "stable_identifier" => {
                let mut cursor = node.walk();
                let last = node
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "type_identifier" || c.kind() == "identifier")
                    .last();
                last.map(|n| self.text(n).to_string())
            }
            _ => {
                let mut cursor = node.walk();
                let id = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "type_identifier");
                id.map(|n| self.text(n).to_string())
            }
        }
    }

    /// emitScalaTypeRefs (scala.ts:27-45) — the hook's own builtin set.
    fn emit_scala_type_refs(&mut self, type_node: Node<'t>, from_row: u32) {
        stack_guard!();
        if type_node.kind() == "type_identifier" {
            let name = self.text(type_node);
            if !name.is_empty() && !is_scala_builtin(name) {
                let name = name.to_string();
                self.push_ref_at(from_row, &name, "references", type_node);
            }
            return;
        }
        let mut cursor = type_node.walk();
        for c in type_node.named_children(&mut cursor) {
            self.emit_scala_type_refs(c, from_row);
        }
    }

    /// extractName (tree-sitter.ts:98-192) — scala-reachable branches: the
    /// `name` field's raw text (operator glyphs and backticks kept), else the
    /// first identifier-ish child, else `<anonymous>`.
    fn extract_name(&self, node: Node<'t>) -> String {
        if let Some(name_node) = node.child_by_field_name("name") {
            return self.text(name_node).to_string();
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
        // The visitNode hook (scala.ts:131-198) runs FIRST.
        if self.hook(node) {
            self.scan_fn_ref_subtree(node, 0);
            return;
        }

        // maybeCaptureFnRefs (:990).
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        let kind = node.kind();
        match kind {
            // methodTypes (functionTypes is EMPTY — :994 never fires).
            "function_definition" | "function_declaration" => {
                self.extract_method_or_function(node);
                return; // skipChildren
            }
            "class_definition" | "object_definition" => {
                self.extract_class(node, "class");
                return;
            }
            "trait_definition" => {
                self.extract_class(node, "trait");
                return;
            }
            "enum_definition" => {
                self.extract_enum(node);
                return;
            }
            "type_definition" => {
                let skip = self.extract_type_alias(node);
                if skip {
                    return;
                }
                // plain path → false → children re-visited (nothing matches).
            }
            "import_declaration" => {
                self.extract_import(node);
                return; // skipChildren
            }
            "call_expression" => {
                self.extract_call(node);
                // no skipChildren — chains/args re-visited
            }
            "instance_expression" => {
                // INSTANTIATION_KINDS (:1255). findAnonymousClassBody looks
                // for class_body/declaration_list — scala's template_body is
                // neither → extractAnonymousClass never runs → children
                // recursed: anon-body defs LEAK to the enclosing scope.
                self.extract_instantiation(node);
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child);
        }
    }

    /// The visitNode hook (scala.ts:131-198). Returns true when consumed.
    fn hook(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        match node.kind() {
            "val_definition" | "var_definition" => {
                let is_val = node.kind() == "val_definition";
                // `if (!name) return false` — TS declines "" as well as null.
                let name = match self.val_var_name(node) {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => return false,
                };
                // Enclosing-definition NODE-TYPE walk (scala.ts:146-156).
                let mut enclosing: Option<&'static str> = None;
                let mut p = node.parent();
                while let Some(parent) = p {
                    match parent.kind() {
                        "class_definition" => {
                            enclosing = Some("class_definition");
                            break;
                        }
                        "trait_definition" => {
                            enclosing = Some("trait_definition");
                            break;
                        }
                        "enum_definition" => {
                            enclosing = Some("enum_definition");
                            break;
                        }
                        "given_definition" => {
                            enclosing = Some("given_definition");
                            break;
                        }
                        "object_definition" => {
                            enclosing = Some("object_definition");
                            break;
                        }
                        _ => p = parent.parent(),
                    }
                }
                let is_instance_field = matches!(
                    enclosing,
                    Some("class_definition") | Some("trait_definition") | Some("enum_definition")
                        | Some("given_definition")
                );
                let kind: &'static str = if is_instance_field {
                    "field"
                } else if is_val {
                    "constant"
                } else {
                    "variable"
                };
                let type_node = node.child_by_field_name("type");
                let signature = type_node.map(|t| {
                    format!("{} {}: {}", if is_val { "val" } else { "var" }, name, self.text(t))
                });
                let visibility = self.visibility_of(node);
                let created = self.create_node(
                    kind,
                    &name,
                    node,
                    Extra { signature, visibility, ..Default::default() },
                );
                if let (Some(row), Some(t)) = (created, type_node) {
                    self.emit_scala_type_refs(t, row);
                }
                // Walk the initializer ATTRIBUTED to the declared symbol
                // (#693, the Go fix): the hook consumes this subtree and the
                // dispatcher only fn-ref-scans it, so `val cb = () => target()`
                // — and even a plain `val x = compute()` — emitted no call edge
                // at all.
                if let Some(row) = created {
                    if let Some(value) = node.child_by_field_name("value") {
                        self.stack.push(Scope { row, kind, name: name.clone() });
                        self.visit_body(value);
                        self.stack.pop();
                    }
                }
                true
            }
            "enum_case_definitions" => {
                let mut cursor = node.walk();
                for case in node.named_children(&mut cursor) {
                    if case.kind() == "simple_enum_case" || case.kind() == "full_enum_case" {
                        if let Some(name_node) = case.child_by_field_name("name") {
                            let name = self.text(name_node).to_string();
                            // ctx.createNode('enum_member', name, child) — no
                            // extras: no docstring/visibility/flags.
                            self.create_node("enum_member", &name, case, Extra::default());
                        }
                    }
                }
                true
            }
            "extension_definition" => {
                // childForFieldName('body') is FIRST-MATCH-WINS over the full
                // (named + anonymous) child list: paren/indent form → the
                // first function_definition (its children visited — no node
                // minted, later defs invisible); braced form → the `{` TOKEN
                // (namedChildCount 0 — whole extension invisible).
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        self.visit(child);
                    }
                }
                true
            }
            _ => false,
        }
    }

    // --- extractMethod → extractFunction routing (:1737 / :1517) ----------

    fn extract_method_or_function(&mut self, node: Node<'t>) {
        stack_guard!();
        // No receiver hook, no methodsAreTopLevel: inside class-like → method,
        // else → function (the object/object_expression parent check never
        // matches scala node kinds).
        let is_method = self.inside_class_like();
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            // Unreachable for scala defs (name field required) — preserved:
            // walk the body with nothing pushed.
            if let Some(body) = node.child_by_field_name("body") {
                self.visit_body(body);
            }
            return;
        }
        let docstring = preceding_docstring(node, self.src);
        let signature = self.signature_of(node);
        let visibility = self.visibility_of(node);
        let is_static = self.is_static_of(node);
        let return_type = self.return_type_of(node);
        let row = self.create_node(
            if is_method { "method" } else { "function" },
            &name,
            node,
            Extra {
                docstring,
                signature,
                visibility,
                is_async: Some(false),
                is_static: Some(is_static),
                return_type,
            },
        );
        let Some(row) = row else { return };
        self.extract_type_annotations(node, row);
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind: if is_method { "method" } else { "function" }, name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_body(body);
        }
        self.stack.pop();
    }

    // --- extractClass (:1679) — classes, objects, traits ------------------

    fn extract_class(&mut self, node: Node<'t>, kind: &'static str) {
        stack_guard!();
        let resolved_body = node.child_by_field_name("body"); // template_body
        // No skipBodilessClass — bodiless mints (scala-complete).
        let name = self.extract_name(node);
        let docstring = preceding_docstring(node, self.src);
        let visibility = self.visibility_of(node);
        let row = self.create_node(
            kind,
            &name,
            node,
            Extra { docstring, visibility, ..Default::default() },
        );
        let Some(row) = row else { return };
        self.extract_inheritance(node, row);
        self.extract_decorators_for(node, row);
        self.stack.push(Scope { row, kind, name });
        // THE ASYMMETRY: bodiless classes walk the node ITSELF — header
        // children (class_parameters defaults, extends args) reach the
        // ladder; bodied classes walk only template_body children.
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
        let body = match node.child_by_field_name("body") {
            Some(b) => b,
            None => return, // bodiless enum mints nothing
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
        self.extract_inheritance(node, row);
        // No extractDecoratorsFor on the enum path (annotated enums emit no
        // decorates — shared-pipeline behavior).
        self.stack.push(Scope { row, kind: "enum", name });
        // enumMemberTypes is EMPTY → every body child goes through visitNode
        // (enum_case_definitions hits the hook; defs become methods).
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            self.visit(child);
        }
        self.stack.pop();
    }

    // --- extractTypeAlias (:2890, plain path :2967-2991) ------------------

    /// Returns skipChildren — always false on the scala plain path.
    fn extract_type_alias(&mut self, node: Node<'t>) -> bool {
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return false;
        }
        let docstring = preceding_docstring(node, self.src);
        // isExported hook absent; visibility not read on this path. The
        // alias-value ref walk reads field 'value' — scala's field is 'type'
        // → no reference to the aliased type, ever.
        self.create_node("type_alias", &name, node, Extra { docstring, ..Default::default() });
        false
    }

    // --- extractImport (:3170-3236) ---------------------------------------

    fn extract_import(&mut self, node: Node<'t>) {
        stack_guard!();
        let import_text = self.text(node).trim();
        // extractImport hook (scala.ts:200-211): `path` field is FIRST-MATCH-
        // WINS → the FIRST dotted segment names the import.
        let module = if let Some(path) = node.child_by_field_name("path") {
            Some(self.text(path))
        } else {
            let mut cursor = node.walk();
            let mut found = None;
            for c in node.named_children(&mut cursor) {
                if c.kind() == "identifier" || c.kind() == "stable_identifier" {
                    found = Some(self.text(c));
                    break;
                }
            }
            found
        };
        let Some(module) = module else { return };
        let module = module.to_string();
        let signature = import_text.to_string();
        let created = self.create_node(
            "import",
            &module,
            node,
            Extra { signature: Some(signature), ..Default::default() },
        );
        // Generic imports ref (:3183-3194) — hook sets no handledRefs.
        if created.is_some() && !module.is_empty() && !self.stack.is_empty() {
            let parent_row = self.top_row();
            self.push_ref_at(parent_row, &module, "imports", node);
        }
    }

    // --- extractCall (:3684) ----------------------------------------------

    // --- extractInstantiation (:4610, scala arm :4647-4662) ---------------

    // --- extractStaticMemberRef (:4750-4808) ------------------------------

    // --- extractDecoratorsFor (:4897-5024) --------------------------------

    // --- extractInheritance — the scala branch (:5339-5360) ---------------

    // --- extractTypeAnnotations (:5788-5880) ------------------------------

    // --- visitFunctionBody (:5129-5286) — scala rows ----------------------

    fn visit_body(&mut self, node: Node<'t>) {
        stack_guard!();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        let kind = node.kind();
        if kind == "call_expression" {
            self.extract_call(node);
            // falls through to recursion
        } else if kind == "instance_expression" {
            // instantiates + recursion (findAnonymousClassBody null): anon
            // template_body defs are NOT dispatched here (functionTypes
            // empty; methodTypes not checked in this walker) — their calls
            // attribute to the enclosing method.
            self.extract_instantiation(node);
        }

        self.extract_static_member_ref(node);

        // Nested named defs mint NOTHING (:5245 checks functionTypes — EMPTY;
        // the inverse of kotlin). Body-local classes/objects/traits/enums DO
        // extract fully.
        match kind {
            "class_definition" | "object_definition" => {
                self.extract_class(node, "class");
                return;
            }
            "trait_definition" => {
                self.extract_class(node, "trait");
                return;
            }
            "enum_definition" => {
                self.extract_enum(node);
                return;
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit_body(child);
        }
    }

    // --- function-as-value capture (#756) — SCALA_SPEC --------------------

    flush_fn_ref_candidates_impl!();

    // --- value-reference edges (:398-931) ---------------------------------

}

