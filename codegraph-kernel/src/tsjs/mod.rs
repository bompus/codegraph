//! TypeScript / TSX / JavaScript / JSX extraction — a faithful Rust port of
//! `TreeSitterExtractor`'s TS/JS paths (src/extraction/tree-sitter.ts) plus
//! the typescript/javascript LanguageExtractor configs.
//!
//! Porting contract (R2 of the migration plan): behavior parity with the wasm
//! path, verified by scripts/kernel-parity.mjs over real repos — including
//! bug-for-bug fidelity where the TS code has quirks. Every function notes the
//! TS function it mirrors; if you change one side, change the other or the
//! parity gate fails. Positions are emitted in UTF-16 code units (what
//! web-tree-sitter reports), see util::col16.

mod bindings;
mod extractors;
mod fnref;
use crate::textutil as util;

use crate::buffers::{
    BindingRow, BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_REEXPORT, EXPORT_CJS, EXPORT_CJS_OBJECT, EXPORT_ESM,
    EXPORT_ESM_DEFAULT, EXPORT_ESM_LATER, EXPORT_NONE, EXPORT_PUBLIC,
    build_meta, edge_kind_index, node_kind_index, Arena, BoolFlags, EdgeRow, EmitOut, NodeRow,
    RefRow, StrRef, Tables, FLAG_IS_ASYNC, FLAG_IS_EXPORTED, FLAG_IS_STATIC, FUNCTION_REF_CODE,
    NONE, NONE_STR,
};
use crate::ids;
use crate::langs;
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Parser};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Typescript,
    Tsx,
    Javascript,
    Jsx,
}

impl Variant {
    pub fn from_language(language: &str) -> Option<Variant> {
        match language {
            "typescript" => Some(Variant::Typescript),
            "tsx" => Some(Variant::Tsx),
            "javascript" => Some(Variant::Javascript),
            "jsx" => Some(Variant::Jsx),
            _ => None,
        }
    }
    /// TS-family (typescript/tsx): type annotations, interfaces, enums,
    /// aliases, visibility, isStatic. The JS family lacks all of those hooks.
    fn is_ts(self) -> bool {
        matches!(self, Variant::Typescript | Variant::Tsx)
    }
    /// VALUE_REF_LANGS includes typescript/tsx/javascript but NOT jsx.
    fn value_refs(self) -> bool {
        !matches!(self, Variant::Jsx)
    }
}

/// typescriptExtractor.methodTypes / javascriptExtractor.methodTypes.
fn is_method_type(v: Variant, kind: &str) -> bool {
    kind == "method_definition"
        || (v.is_ts() && matches!(kind, "public_field_definition" | "method_signature"))
        || (!v.is_ts() && kind == "field_definition")
}

/// typescriptExtractor.propertyTypes. The interface counterpart of
/// `public_field_definition`: it carries no value, so it is always a property
/// and never goes through classify_ts_class_member (#1638).
fn is_property_type(v: Variant, kind: &str) -> bool {
    v.is_ts() && kind == "property_signature"
}

/// Method node types that spell a SIGNATURE — a declaration with no body (#1638).
///
/// They are a method of whatever type declares them and nothing on their own, so
/// they must not take `extract_method`'s "no class-like parent, so treat it as a
/// free function" fallback. The other method types can: a `method_definition`
/// outside a class really is a function. This one appears outside a class only
/// inside a type literal (`type Handle = { stop(): void }`), whose members
/// `extract_ts_type_alias_members` already extracts and attaches to the alias
/// (#359) — take the fallback and the file gains a phantom top-level
/// `function stop` beside the real `Handle::stop`. Mirrors the TS extractor's
/// SIGNATURE_METHOD_NODE_TYPES (extraction/tree-sitter.ts).
fn is_signature_method_type(kind: &str) -> bool {
    kind == "method_signature"
}

pub(super) fn is_function_type(kind: &str) -> bool {
    matches!(kind, "function_declaration" | "generator_function_declaration" | "arrow_function" | "function_expression" | "generator_function")
}

fn is_class_type(v: Variant, kind: &str) -> bool {
    kind == "class_declaration" || (v.is_ts() && kind == "abstract_class_declaration")
}

fn is_variable_type(kind: &str) -> bool {
    matches!(kind, "lexical_declaration" | "variable_declaration")
}

/// LITERAL_RECEIVER_TYPES (tree-sitter.ts) — full set; only a handful occur in
/// TS/JS grammars but membership is what the TS code tests.
fn is_literal_receiver(kind: &str) -> bool {
    matches!(
        kind,
        "string" | "string_literal" | "interpreted_string_literal" | "raw_string_literal"
            | "template_string" | "concatenated_string" | "formatted_string" | "f_string"
            | "line_string_literal" | "string_content" | "heredoc_body"
            | "number" | "number_literal" | "integer" | "integer_literal" | "float"
            | "float_literal" | "int_literal" | "decimal_integer_literal" | "real_literal"
            | "char_literal" | "character_literal" | "rune_literal" | "regex" | "regex_literal"
            | "true" | "false" | "boolean_literal" | "bool_literal" | "none" | "null" | "nil"
            | "null_literal" | "undefined"
            | "list" | "list_literal" | "array" | "array_literal" | "array_creation_expression"
            | "dictionary" | "dict_literal" | "object" | "tuple" | "set"
    )
}

/// BUILTIN_TYPES (tree-sitter.ts) — names that never become type references.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "string" | "number" | "boolean" | "void" | "null" | "undefined" | "never" | "any"
            | "unknown" | "object" | "symbol" | "bigint" | "true" | "false"
            | "str" | "bool" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" | "char"
            | "int" | "long" | "short" | "byte" | "float" | "double"
            | "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64"
            | "float32" | "float64" | "complex64" | "complex128" | "rune" | "error"
            | "Int" | "Long" | "Short" | "Byte" | "Float" | "Double" | "Boolean" | "Char"
            | "Unit" | "String" | "Any" | "AnyRef" | "AnyVal" | "Nothing" | "Null"
    )
}

/// REACT_COMPONENT_HOCS (tree-sitter.ts, #841).
fn is_react_hoc(callee: &str) -> bool {
    matches!(callee, "forwardRef" | "memo" | "React.forwardRef" | "React.memo")
}

fn is_vue_collection_name(name: &str) -> bool {
    matches!(name, "actions" | "mutations" | "getters")
}

/// One scope-stack entry (TS keeps node IDs; rows are our equivalent).
struct Scope {
    row: u32,
    kind: &'static str,
    name: String,
}

/// Extra node properties, per-extract-site (mirrors createNode's `extra`).
#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<u8>,
    is_exported: Option<bool>,
    is_async: Option<bool>,
    is_static: Option<bool>,
    qualified_name: Option<String>,
}

struct ValueScope<'t> {
    row: u32,
    node: Node<'t>,
    name: String,
}

pub struct Walker<'t> {
    src: &'t str,
    file_path: &'t str,
    variant: Variant,
    line_starts: Vec<usize>,
    arena: Arena,
    tables: Tables,
    stack: Vec<Scope>,
    /// Node id string per row. Rows are unique but IDS COLLIDE for same
    /// (kind, name, line) nodes — routine in minified one-line files — and the
    /// TS extractor's fn-ref dedupe and value-ref self-checks key on the ID,
    /// so parity requires comparing ids, not rows.
    node_ids: Vec<String>,
    /// Function/method names defined in this file (fn-ref flush gate).
    defined_fn_names: HashSet<String>,
    /// Simple names from `imports` refs (fn-ref flush gate).
    imported_names: HashSet<String>,
    fn_ref_cands: Vec<(u32, fnref::Candidate)>,
    // Value-reference bookkeeping (flushValueRefs).
    fs_values: HashMap<String, u32>,
    fs_value_counts: HashMap<String, u32>,
    value_scopes: Vec<ValueScope<'t>>,
    vue_store_file: Option<bool>,
    /// `${from}|${name}|${line}|${column}` for markdown path refs already
    /// emitted. A string inside a declaration's value is reached twice — by the
    /// declaration's own subtree scan and by the general walk — and the TS side
    /// collapses that in `addReference`'s referenceKeys. Scoped to these refs
    /// because they are the only kernel path that reaches one node twice.
    md_ref_keys: HashSet<String>,
    /// Names exported by a LATER top-level statement (`export { a, b as c }`,
    /// `export default NAME`): name → (exported-as, form). Collected from the
    /// AST before the walk, replacing the old anchored-regex `is_exported_later`
    /// (docs/design/resolution-binding-model-plan.md, Phase 1).
    later_exports: HashMap<String, (String, u8)>,
    line_count: u32,
    /// (name, line) → specifier for a module-level `const x = require(..)`
    /// declarator: the walk emits its row as an `import` row (node-backed, so
    /// a later `module.exports = { x }` can still export it).
    import_decls: HashMap<(String, u32), String>,
    /// (name, line) of `param`/`local` rows the pre-walk emitted (dedupe).
    scoped_rows: HashSet<(String, u32)>,
}

const MAX_VALUE_REF_NODES: usize = 20_000;

pub fn extract(file_path: &str, source: &str, language: &str) -> Result<EmitOut, String> {
    let variant = Variant::from_language(language)
        .ok_or_else(|| format!("tsjs walker does not handle language: {language}"))?;
    let grammar = langs::grammar_for(language)
        .ok_or_else(|| format!("no grammar for language: {language}"))?;

    let t0 = std::time::Instant::now();
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|e| format!("set_language({language}) failed: {e}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "parser returned null tree".to_string())?;

    // Files with parse ERRORS are extracted natively like any other file. Error
    // RECOVERY differs between UTF-8 (native) and UTF-16 (web-tree-sitter)
    // parsing, so an erroring file's graph may differ from the wasm path's; the
    // kernel's recovery is canonical (kernel-only-extraction-plan.md, Phase 1).

    let mut w = Walker {
        src: source,
        file_path,
        variant,
        line_starts: util::line_starts(source),
        arena: Arena::default(),
        tables: Tables::default(),
        stack: Vec::new(),
        node_ids: Vec::new(),
        defined_fn_names: HashSet::new(),
        imported_names: HashSet::new(),
        fn_ref_cands: Vec::new(),
        fs_values: HashMap::new(),
        fs_value_counts: HashMap::new(),
        value_scopes: Vec::new(),
        vue_store_file: None,
        md_ref_keys: HashSet::new(),
        later_exports: HashMap::new(),
        line_count: 0,
        import_decls: HashMap::new(),
        scoped_rows: HashSet::new(),
    };

    // File node (TreeSitterExtractor.extract): id `file:<path>`, endLine =
    // newline count + 1, isExported explicitly false.
    let line_count = source.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
    let base_name = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    let mut flags = BoolFlags::default();
    flags.set(FLAG_IS_EXPORTED, false);
    let file_id = w.arena.put(&ids::file_node_id(file_path));
    let name_ref = w.arena.put(base_name);
    let qn_ref = w.arena.put(file_path);
    w.tables.push_node(&NodeRow {
        kind: node_kind_index("file").unwrap(),
        visibility: 0,
        flags,
        start_line: 1,
        end_line: line_count,
        start_column: 0,
        end_column: 0,
        name: name_ref,
        qualified_name: qn_ref,
        id: file_id,
        docstring: NONE_STR,
        signature: NONE_STR,
        decorators: NONE_STR,
        type_parameters: NONE_STR,
        return_type: NONE_STR,
        extra_json: NONE_STR,
    });
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });
    w.line_count = line_count;
    w.collect_later_exports(tree.root_node());
    w.collect_scoped_bindings(tree.root_node());

    w.visit_node(tree.root_node());

    // End-of-file passes, in the TS extract() order.
    w.flush_fn_ref_candidates();
    w.flush_value_refs(tree.root_node());
    w.stack.pop();

    let duration_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let errors_json = crate::buffers::parse_collapse_warning(
        &mut w.arena,
        &w.tables,
        tree.root_node().has_error(),
        file_path,
    );
    let meta = build_meta(&w.tables, w.arena.len(), errors_json, duration_ms);
    Ok(EmitOut {
        meta,
        nodes: w.tables.nodes,
        edges: w.tables.edges,
        refs: w.tables.refs,
        bindings: w.tables.bindings,
        arena: w.arena.into_vec(),
    })
}

impl<'t> Walker<'t> {
    // --- small helpers --------------------------------------------------------

    fn text(&self, node: Node) -> &'t str {
        &self.src[node.byte_range()]
    }

    fn line_of(&self, node: Node) -> u32 {
        node.start_position().row as u32 + 1
    }

    fn col_of(&self, node: Node) -> u32 {
        util::col16(self.src, &self.line_starts, node.start_position().row, node.start_byte())
    }

    fn end_col_of(&self, node: Node) -> u32 {
        util::col16(self.src, &self.line_starts, node.end_position().row, node.end_byte())
    }

    fn top_row(&self) -> u32 {
        self.stack.last().map(|s| s.row).unwrap_or(0)
    }

    /// isInsideClassLikeNode.
    fn inside_class_like(&self) -> bool {
        self.stack
            .last()
            .map(|s| matches!(s.kind, "class" | "struct" | "interface" | "trait" | "enum" | "module"))
            .unwrap_or(false)
    }

    fn push_ref(&mut self, from_row: u32, name: &str, kind_code: u8, node: Node) {
        let name_ref = self.arena.put(name);
        self.tables.push_ref(&RefRow {
            from_idx: from_row,
            kind: kind_code,
            line: self.line_of(node),
            column: self.col_of(node),
            reference_name: name_ref,
            candidates: NONE_STR,
            from_id_str: NONE_STR,
        });
        if kind_code == edge_kind_index("imports").unwrap() {
            // Feed the fn-ref flush gate the same way flushFnRefCandidates
            // derives importedNames from `imports` refs.
            if util::simple_name().is_match(name) {
                self.imported_names.insert(name.to_string());
            } else if let Some(c) = util::qualified_import().captures(name) {
                self.imported_names.insert(c[1].to_string());
            }
        }
    }


    fn push_call_ref(&mut self, name: &str, node: Node) {
        self.push_ref(self.top_row(), name, edge_kind_index("calls").unwrap(), node);
    }

    markdown_refs_impl!();

    // --- createNode -----------------------------------------------------------

    /// createNode (tree-sitter.ts): id, qualified name from the scope stack,
    /// contains edge from the parent scope, value-ref bookkeeping.
    fn create_node(&mut self, kind: &'static str, name: &str, node: Node<'t>, extra: Extra) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let start_line = self.line_of(node);
        let id = ids::node_id(self.file_path, kind, name, start_line);

        // endLine body extension: resolveBody only (TS/JS: function-valued
        // class fields whose body nests in the arrow / HOF-wrapped arrow).
        let mut end_line = node.end_position().row as u32 + 1;
        if (kind == "function" || kind == "method") && matches!(node.kind(), "public_field_definition" | "field_definition")
        {
            if let Some(body) = resolve_field_body(node) {
                let be = body.end_position().row as u32 + 1;
                if be > end_line {
                    end_line = be;
                }
            }
        }

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
        if let Some(v) = extra.is_static {
            flags.set(FLAG_IS_STATIC, v);
        }

        let name_ref = self.arena.put(name);
        let qn_ref = self.arena.put(&qualified);
        let id_ref = self.arena.put(&id);
        let doc_ref = opt_str(&mut self.arena, extra.docstring.as_deref());
        let sig_ref = opt_str(&mut self.arena, extra.signature.as_deref());
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
            return_type: NONE_STR,
            extra_json: NONE_STR,
        });

        // Containment edge from the current scope.
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

        self.node_ids.push(id);
        if kind == "function" || kind == "method" {
            self.defined_fn_names.insert(name.to_string());
        }
        self.emit_decl_binding(kind, name, row, node, extra.is_exported == Some(true));
        self.capture_value_ref_scope(kind, name, row, node);
        Some(row)
    }

    // --- bindings (resolution-binding-model-plan.md §2.1) --------------------------

    /// Pre-walk: the names a later top-level `export` statement exports.
    fn collect_later_exports(&mut self, root: Node<'t>) {
        for i in 0..root.named_child_count() {
            let Some(stmt) = root.named_child(i) else { continue };
            if stmt.kind() != "export_statement" || stmt.child_by_field_name("source").is_some() {
                continue;
            }
            // `export default NAME;` / `export = NAME;` name a declaration.
            // `export default <expression>` binds none: a nodeless `default`
            // row records that the module still exports something.
            if let Some(value) = stmt.child_by_field_name("value") {
                if value.kind() == "identifier" {
                    let name = self.text(value).to_string();
                    self.later_exports.entry(name).or_insert(("default".to_string(), EXPORT_ESM_DEFAULT));
                } else if !matches!(value.kind(), "function_declaration" | "class_declaration" | "generator_function_declaration") {
                    self.push_default_export_row(stmt);
                }
                continue;
            }
            if stmt.child_by_field_name("declaration").is_none() && self.has_keyword_child(stmt, "default") {
                self.push_default_export_row(stmt);
                continue;
            }
            // `export { a, b as c, d as default }`
            let clause = (0..stmt.named_child_count())
                .filter_map(|k| stmt.named_child(k))
                .find(|c| c.kind() == "export_clause");
            let Some(clause) = clause else { continue };
            for j in 0..clause.named_child_count() {
                let Some(spec) = clause.named_child(j) else { continue };
                if spec.kind() != "export_specifier" {
                    continue;
                }
                let Some(name_node) = spec.child_by_field_name("name").or_else(|| spec.named_child(0)) else { continue };
                let name = self.text(name_node).to_string();
                let exported_as = spec
                    .child_by_field_name("alias")
                    .map(|a| self.text(a).to_string())
                    .unwrap_or_else(|| name.clone());
                let form = if exported_as == "default" { EXPORT_ESM_DEFAULT } else { EXPORT_ESM_LATER };
                self.later_exports.entry(name).or_insert((exported_as, form));
            }
        }
    }

    /// CommonJS export assignments, keyed by the LOCAL name they
    /// export: `module.exports = { a, b: c }` (`cjs-object`), `module.exports =
    /// NAME` (as `default`), `exports.x = NAME` / `exports['x'] = NAME` /
    /// `module.exports.x = NAME` (`cjs`). A function-valued right side is the
    /// walk's own exported node and needs no entry here.
    pub(super) fn collect_cjs_export(&mut self, expr: Node<'t>) {
        let (Some(left), Some(right)) = (expr.child_by_field_name("left"), expr.child_by_field_name("right")) else { return };
        let left_text: String = self.text(left).chars().filter(|c| !c.is_whitespace()).collect();
        if left_text == "module.exports" {
            if right.kind() == "object" {
                for j in 0..right.named_child_count() {
                    let Some(prop) = right.named_child(j) else { continue };
                    match prop.kind() {
                        "shorthand_property_identifier" => {
                            let name = self.text(prop).to_string();
                            self.later_exports.entry(name.clone()).or_insert((name, EXPORT_CJS_OBJECT));
                        }
                        "pair" => {
                            let key = prop.child_by_field_name("key").map(|k| self.text(k).trim_matches(|c| c == '\'' || c == '"').to_string());
                            let value = prop.child_by_field_name("value").filter(|v| v.kind() == "identifier");
                            if let (Some(key), Some(value)) = (key, value) {
                                self.later_exports.entry(self.text(value).to_string()).or_insert((key, EXPORT_CJS_OBJECT));
                            }
                        }
                        _ => {}
                    }
                }
            } else if right.kind() == "identifier" {
                self.later_exports.entry(self.text(right).to_string()).or_insert(("default".to_string(), EXPORT_CJS));
            }
            return;
        }
        if right.kind() != "identifier" || !matches!(left.kind(), "member_expression" | "subscript_expression") {
            return;
        }
        let Some(obj) = left.child_by_field_name("object") else { return };
        let obj_text: String = self.text(obj).chars().filter(|c| !c.is_whitespace()).collect();
        if obj_text != "exports" && obj_text != "module.exports" {
            return;
        }
        let key = left
            .child_by_field_name("property")
            .or_else(|| left.child_by_field_name("index"))
            .map(|k| self.text(k).trim_matches(|c| c == '\'' || c == '"').to_string());
        if let Some(key) = key {
            if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
                self.later_exports.entry(self.text(right).to_string()).or_insert((key, EXPORT_CJS));
            }
        }
    }

    /// A `default` row with no node: `export default defineConfig({ … })`.
    fn push_default_export_row(&mut self, stmt: Node<'t>) {
        let name_ref = self.arena.put("default");
        let line = self.line_of(stmt);
        let line_count = self.line_count;
        self.tables.push_binding(&BindingRow {
            kind: BINDING_DECL,
            export_form: EXPORT_ESM_DEFAULT,
            node_idx: NONE,
            scope_start: 1,
            scope_end: line_count,
            name: name_ref,
            target_spec: NONE_STR,
            target_name: NONE_STR,
            exported_as: name_ref,
            storage: NONE_STR,
            line,
        });
    }

    /// The (exported-as, form) a later statement gives `name`, or none.
    pub(super) fn later_export_of(&mut self, name: &str) -> (StrRef, u8) {
        match self.later_exports.get(name).cloned() {
            Some((alias, form)) => (self.arena.put(&alias), form),
            None => (NONE_STR, EXPORT_NONE),
        }
    }

    /// Replaces the regex `is_exported_later`: same question, answered from the AST.
    pub(super) fn is_exported_later(&self, name: &str) -> bool {
        self.later_exports.contains_key(name)
    }

    /// A `decl` row for a module-scope declaration, a `local` row for one nested
    /// in a function/class body. `export_statement` ancestry is `esm`; a
    /// declaration exported by a later statement takes that statement's form;
    /// an exported flag with neither is the CommonJS assignment path.
    fn emit_decl_binding(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>, exported_flag: bool) {
        if matches!(kind, "file" | "import") {
            return;
        }
        let line = self.line_of(node);
        if self.scoped_rows.contains(&(name.to_string(), line)) {
            return;
        }
        let require_spec = self.import_decls.get(&(name.to_string(), line)).cloned();
        let scope_top = self.stack.last().map(|s| s.kind).unwrap_or("file");
        let at_module_scope = scope_top == "file";
        let (scope_start, scope_end) = if at_module_scope {
            (1, self.line_count)
        } else {
            let row_of_scope = self.stack.last().map(|s| s.row).unwrap_or(0);
            self.tables.node_lines(row_of_scope)
        };
        let mut exported_as: Option<String> = None;
        let mut form = EXPORT_NONE;
        if at_module_scope {
            if exported_flag && self.is_exported(node) {
                // `export default function NAME` / `class NAME` exports the name
                // as `default`: the declaration itself is the statement's value.
                // A member nested in a default-exported object literal is not.
                let is_default = self
                    .export_statement_of(node)
                    .map(|e| self.has_keyword_child(e, "default") && e.child_by_field_name("declaration") == Some(node))
                    .unwrap_or(false);
                exported_as = Some(if is_default { "default".to_string() } else { name.to_string() });
                form = if is_default { EXPORT_ESM_DEFAULT } else { EXPORT_ESM };
            } else if self.in_declare_global(node) {
                // `declare global { … }` contributes the name to every file.
                exported_as = Some(name.to_string());
                form = EXPORT_PUBLIC;
            } else if let Some((alias, f)) = self.later_exports.get(name) {
                exported_as = Some(alias.clone());
                form = *f;
            } else if exported_flag {
                exported_as = Some(name.to_string());
                form = EXPORT_CJS;
            }
        }
        let name_ref = self.arena.put(name);
        let exported_ref = opt_str(&mut self.arena, exported_as.as_deref());
        let (kind_code, target_spec, target_name) = match require_spec {
            Some(spec) => (BINDING_IMPORT, self.arena.put(&spec), self.arena.put("default")),
            None => (if at_module_scope { BINDING_DECL } else { BINDING_LOCAL }, NONE_STR, NONE_STR),
        };
        self.tables.push_binding(&BindingRow {
            kind: kind_code,
            export_form: form,
            node_idx: row,
            scope_start,
            scope_end,
            name: name_ref,
            target_spec,
            target_name,
            exported_as: exported_ref,
            storage: NONE_STR,
            line,
        });
    }

    /// An `import` row: the local name, the specifier, and the imported name.
    /// An imported name a later clause exports (`import X from './x'; export
    /// { X }`) carries that export like a declaration would.
    pub(super) fn emit_import_binding(&mut self, local: &str, spec: &str, imported: &str, node: Node<'t>) {
        let name_ref = self.arena.put(local);
        let spec_ref = self.arena.put(spec);
        let imported_ref = self.arena.put(imported);
        let line = self.line_of(node);
        let line_count = self.line_count;
        let (exported_as, form) = self.later_export_of(local);
        self.tables.push_binding(&BindingRow {
            kind: BINDING_IMPORT,
            export_form: form,
            node_idx: NONE,
            scope_start: 1,
            scope_end: line_count,
            name: name_ref,
            target_spec: spec_ref,
            target_name: imported_ref,
            exported_as,
            storage: NONE_STR,
            line,
        });
    }

    /// A `reexport` row: `export { name as alias } from spec`.
    pub(super) fn emit_reexport_binding(&mut self, name: &str, exported_as: &str, spec: &str, node: Node<'t>) {
        let name_ref = self.arena.put(name);
        let spec_ref = self.arena.put(spec);
        let exported_ref = self.arena.put(exported_as);
        let line = self.line_of(node);
        let line_count = self.line_count;
        self.tables.push_binding(&BindingRow {
            kind: BINDING_REEXPORT,
            export_form: EXPORT_ESM,
            node_idx: NONE,
            scope_start: 1,
            scope_end: line_count,
            name: name_ref,
            target_spec: spec_ref,
            target_name: name_ref,
            exported_as: exported_ref,
            storage: NONE_STR,
            line,
        });
    }

    // --- value references (captureValueRefScope / flushValueRefs) --------------

    fn capture_value_ref_scope(&mut self, kind: &'static str, name: &str, row: u32, node: Node<'t>) {
        if !self.variant.value_refs() {
            return;
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
    }

    fn flush_value_refs(&mut self, root: Node<'t>) {
        let scopes = std::mem::take(&mut self.value_scopes);
        let mut targets = std::mem::take(&mut self.fs_values);
        let counts = std::mem::take(&mut self.fs_value_counts);
        if !self.variant.value_refs() || std::env::var("CODEGRAPH_VALUE_REFS").as_deref() == Ok("0") {
            return;
        }
        if targets.is_empty() || scopes.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }

        // Shadow prune: count declarators of each target name across the whole
        // tree; more declarators than file-scope nodes ⇒ an inner re-binding
        // shadows the target. (TS/JS declarators are `variable_declarator`;
        // the other kinds in the TS switch belong to other grammars.)
        let mut decl_counts: HashMap<&str, u32> = HashMap::new();
        let mut dstack: Vec<Node> = vec![root];
        let mut dvisited = 0usize;
        while let Some(n) = dstack.pop() {
            if dvisited >= MAX_VALUE_REF_NODES {
                break;
            }
            dvisited += 1;
            if n.kind() == "variable_declarator" {
                if let Some(first) = n.named_child(0) {
                    if first.kind() == "identifier" {
                        let nm = self.text(first);
                        if targets.contains_key(nm) {
                            *decl_counts.entry(nm).or_insert(0) += 1;
                        }
                    }
                }
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

        let refs_kind = edge_kind_index("references").unwrap();
        for scope in &scopes {
            // Self-skip and per-scope dedupe compare node ID STRINGS (which
            // collide for same-(kind, name, line) nodes), matching the TS side.
            let mut seen: HashSet<&str> = HashSet::new();
            let mut stack: Vec<Node> = vec![scope.node];
            let mut visited = 0usize;
            while let Some(n) = stack.pop() {
                if visited >= MAX_VALUE_REF_NODES {
                    break;
                }
                visited += 1;
                if matches!(n.kind(), "identifier" | "constant" | "name" | "simple_identifier") {
                    let ref_name = self.text(n);
                    if let Some(&target_row) = targets.get(ref_name) {
                        let target_id = self.node_ids[target_row as usize].as_str();
                        if target_id != self.node_ids[scope.row as usize]
                            && ref_name != scope.name
                            && !seen.contains(&target_id)
                        {
                            seen.insert(target_id);
                            let meta = self.arena.put(r#"{"valueRef":true}"#);
                            self.tables.push_edge(&EdgeRow {
                                source_idx: scope.row,
                                target_idx: target_row,
                                kind: refs_kind,
                                provenance: 0,
                                line: NONE,
                                column: NONE,
                                metadata_json: meta,
                                source_id_str: NONE_STR,
                                target_id_str: NONE_STR,
                            });
                        }
                    }
                }
                for i in 0..n.named_child_count() {
                    if let Some(c) = n.named_child(i) {
                        stack.push(c);
                    }
                }
            }
        }
    }

    // --- function-as-value refs (#756) -----------------------------------------

    fn maybe_capture_fn_refs(&mut self, node: Node<'t>) {
        let Some(mode) = fnref::dispatch(node.kind()) else { return };
        if self.stack.is_empty() {
            return;
        }
        let from = self.top_row();
        for (cand, _mode) in fnref::capture(node, mode, self.src) {
            self.fn_ref_cands.push((from, cand));
        }
    }

    /// scanFnRefSubtree: capture-only walk of subtrees the main walkers skip.
    fn scan_fn_ref_subtree(&mut self, node: Node<'t>, depth: u32) {
        stack_guard!();
        if depth > 12 {
            return;
        }
        let kind = node.kind();
        if depth > 0
            && (is_function_type(kind) || matches!(kind, "lambda_literal" | "lambda_expression"))
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

    fn flush_fn_ref_candidates(&mut self) {
        let cands = std::mem::take(&mut self.fn_ref_cands);
        if cands.is_empty() || util::is_generated_file(self.file_path) {
            return;
        }
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (from, c) in cands {
            // Gate: `this.<member>` always flushes; everything else must match
            // a same-file function/method or an imported name. (The `::` and
            // ungated-mode policies belong to other languages' specs.)
            if !c.name.starts_with("this.")
                && !c.name.contains("::")
                && !self.defined_fn_names.contains(&c.name)
                && !self.imported_names.contains(&c.name)
            {
                continue;
            }
            // Dedupe on the node ID STRING, not the row — ids collide for
            // same-(kind, name, line) nodes (minified one-liners) and the TS
            // side keys its dedupe on `${fromNodeId}|${name}`.
            if !seen.insert((self.node_ids[from as usize].clone(), c.name.clone())) {
                continue;
            }
            let column = util::col16(self.src, &self.line_starts, c.row, c.column_byte);
            let name_ref = self.arena.put(&c.name);
            self.tables.push_ref(&RefRow {
                from_idx: from,
                kind: FUNCTION_REF_CODE,
                line: c.line,
                column,
                reference_name: name_ref,
                candidates: NONE_STR,
                from_id_str: NONE_STR,
            });
        }
    }

    // --- the dispatcher (visitNode) --------------------------------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        // Function-as-value capture — independent of the dispatch ladder.
        self.maybe_capture_fn_refs(node);
        let owner = self.top_row();
        self.markdown_refs_from_string(node, owner);

        if is_function_type(kind) {
            // (the isInsideClassLike + methodTypes overlap is Python/Ruby-only)
            self.extract_function(node, None);
            skip_children = true;
        } else if is_class_type(self.variant, kind) {
            self.extract_class(node);
            skip_children = true;
        } else if is_method_type(self.variant, kind)
            && (!is_signature_method_type(kind) || self.inside_class_like())
        {
            if classify_ts_class_member(node) == Member::Property {
                let prop = self.extract_property(node);
                if let (Some((row, name)), Some(value)) = (prop, node.child_by_field_name("value")) {
                    self.stack.push(Scope { row, kind: "property", name });
                    self.visit_function_body(value);
                    self.stack.pop();
                }
                self.scan_fn_ref_subtree(node, 0);
            } else {
                self.extract_method(node);
            }
            skip_children = true;
        } else if self.variant.is_ts() && kind == "interface_declaration" {
            self.extract_interface(node);
            skip_children = true;
        } else if self.variant.is_ts() && kind == "enum_declaration" {
            self.extract_enum(node);
            skip_children = true;
        } else if self.variant.is_ts() && kind == "type_alias_declaration" {
            skip_children = self.extract_type_alias(node);
        } else if is_variable_type(kind) && !self.inside_class_like() {
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "import_statement" {
            self.extract_import(node);
        } else if kind == "export_statement" && node.child_by_field_name("source").is_some() {
            // Re-export: `export { X } from './y'`.
            self.emit_re_export_refs(node);
        } else if kind == "export_statement" && self.looks_like_vue_store_file() {
            // Vuex MODULE default export (`export default { actions: {…} }`).
            if let Some(exported) = node.child_by_field_name("value") {
                if matches!(exported.kind(), "object" | "object_expression") {
                    self.extract_store_collection_methods(exported);
                    skip_children = true;
                }
            }
        } else if kind == "call_expression" {
            self.extract_call(node);
        } else if kind == "new_expression" {
            self.extract_instantiation(node);
        } else if is_property_type(self.variant, kind) && self.inside_class_like() {
            // NOTE: `property_signature` / `method_signature` used to be handled
            // here together, hanging their type annotations off the ENCLOSING
            // INTERFACE — the only anchor available while the members themselves
            // went unextracted. Since #1638 `method_signature` is a method type
            // and `property_signature` a property type, so the method branch
            // above claims the first (under the same inside_class_like guard
            // this branch had) and this one extracts the second as a real node.
            // The `references` edges survive — extract_method and
            // extract_property each call extract_type_annotations — but now hang
            // off the member, the more precise anchor: `Api::fetch → PageId`
            // says which member wants the type, where `Api → PageId` only said
            // the file did.
            self.extract_property(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        }

        if !skip_children {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    self.visit_node(c);
                }
            }
        }
    }

    // --- visitFunctionBody ------------------------------------------------------

    fn visit_function_body(&mut self, body: Node<'t>) {
        self.visit_for_calls_and_structure(body);
    }

    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "call_expression" {
            self.extract_call(node);
        } else if kind == "new_expression" {
            self.extract_instantiation(node);
        }

        // Local variable type annotations (TS family only).
        if self.variant.is_ts() && kind == "variable_declarator" {
            let owner = self.top_row();
            self.extract_variable_type_annotation(node, owner);
        }

        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        // Nested NAMED functions become their own nodes — and so does the
        // function a React handler hook binds a name to (`const onPress =
        // useCallback(() => {…}, [])`). Mirrors TreeSitterExtractor's
        // reactHookBoundName.
        if is_function_type(kind) {
            let name = self.extract_name(node);
            if name != "<anonymous>" {
                self.extract_function(node, None);
                return;
            }
            if let Some(bound) = self.react_hook_bound_name(node) {
                self.extract_function(node, Some(bound));
                return;
            }
            // `const handleClear = () => {…}` inside a body (#1669): named by
            // its declarator, like at module scope. Mirrors
            // TreeSitterExtractor's declaratorBoundFunction.
            if self.declarator_bound_function(node) {
                self.extract_function(node, None);
                return;
            }
        }

        if is_class_type(self.variant, kind) {
            self.extract_class(node);
            return;
        }
        if self.variant.is_ts() && kind == "enum_declaration" {
            self.extract_enum(node);
            return;
        }
        if self.variant.is_ts() && kind == "interface_declaration" {
            self.extract_interface(node);
            return;
        }

        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                self.visit_for_calls_and_structure(c);
            }
        }
    }

    // --- name / signature / modifier helpers ------------------------------------

    /// Whether an anonymous function is the whole value of a
    /// `variable_declarator` with a plain identifier name —
    /// `const NAME = () => {…}` / `= function () {…}`.
    fn declarator_bound_function(&self, node: Node<'t>) -> bool {
        if !matches!(node.kind(), "arrow_function" | "function_expression") {
            return false;
        }
        let Some(declarator) = node.parent() else { return false };
        if declarator.kind() != "variable_declarator" {
            return false;
        }
        let Some(value) = declarator.child_by_field_name("value") else { return false };
        if value.start_byte() != node.start_byte() || value.end_byte() != node.end_byte() {
            return false;
        }
        declarator
            .child_by_field_name("name")
            .map(|n| n.kind() == "identifier")
            .unwrap_or(false)
    }

    /// The declarator name a React handler hook binds an anonymous function
    /// to — `const NAME = useCallback(<node>, [...])` (also `React.useCallback`,
    /// `useEffectEvent`, `useEvent`) — or None for any other shape. The node
    /// must be the call's FIRST argument and the call's value must be bound
    /// directly by a `variable_declarator`.
    fn react_hook_bound_name(&self, node: Node<'t>) -> Option<String> {
        if !matches!(node.kind(), "arrow_function" | "function_expression") {
            return None;
        }
        let args = node.parent()?;
        if args.kind() != "arguments" {
            return None;
        }
        let first = args.named_child(0)?;
        if first.start_byte() != node.start_byte() || first.end_byte() != node.end_byte() {
            return None;
        }
        let call = args.parent()?;
        if call.kind() != "call_expression" {
            return None;
        }
        let callee = call.child_by_field_name("function")?;
        let callee_text = self.text(callee);
        let hook = callee_text.strip_prefix("React.").unwrap_or(callee_text);
        if !matches!(hook, "useCallback" | "useEffectEvent" | "useEvent") {
            return None;
        }
        let declarator = call.parent()?;
        if declarator.kind() != "variable_declarator" {
            return None;
        }
        let name_node = declarator.child_by_field_name("name")?;
        if name_node.kind() != "identifier" {
            return None;
        }
        Some(self.text(name_node).to_string())
    }

    /// extractName / extractNameRaw for the TS/JS configs.
    fn extract_name(&self, node: Node) -> String {
        // javascriptExtractor.resolveName: field_definition names its key the
        // `property` field.
        if !self.variant.is_ts() && node.kind() == "field_definition" {
            if let Some(prop) = node.child_by_field_name("property") {
                return self.text(prop).to_string();
            }
        }
        if let Some(name_node) = node.child_by_field_name("name") {
            return self.text(name_node).to_string();
        }
        if matches!(node.kind(), "arrow_function" | "function_expression" | "generator_function") {
            return "<anonymous>".to_string();
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

    /// typescriptExtractor.getSignature / javascriptExtractor.getSignature.
    fn signature_of(&self, node: Node) -> Option<String> {
        let params = node.child_by_field_name("parameters")?;
        let mut sig = self.text(params).to_string();
        if self.variant.is_ts() {
            if let Some(ret) = node.child_by_field_name("return_type") {
                let ret_text = self.text(ret);
                let stripped = ret_text.strip_prefix(':').unwrap_or(ret_text).trim_start();
                sig.push_str(": ");
                sig.push_str(stripped);
            }
        }
        Some(sig)
    }

    /// typescriptExtractor.getVisibility (TS only — JS has no hook).
    fn visibility_of(&self, node: Node) -> Option<u8> {
        if !self.variant.is_ts() {
            return None;
        }
        for i in 0..node.child_count() {
            let child = node.child(i)?;
            if child.kind() == "accessibility_modifier" {
                return match self.text(child) {
                    "public" => Some(1),
                    "private" => Some(2),
                    "protected" => Some(3),
                    _ => None,
                };
            }
        }
        None
    }

    /// The `export_statement` ancestor, when there is one.
    fn export_statement_of(&self, node: Node<'t>) -> Option<Node<'t>> {
        let mut cur = node.parent();
        while let Some(p) = cur {
            if p.kind() == "export_statement" {
                return Some(p);
            }
            cur = p.parent();
        }
        None
    }

    /// Inside a `declare global { … }` ambient block.
    fn in_declare_global(&self, node: Node) -> bool {
        let mut cur = node.parent();
        while let Some(p) = cur {
            if p.kind() == "ambient_declaration" && self.has_keyword_child(p, "global") {
                return true;
            }
            cur = p.parent();
        }
        false
    }

    /// isExported: walk the parent chain for an export_statement.
    fn is_exported(&self, node: Node) -> bool {
        let mut cur = node.parent();
        while let Some(p) = cur {
            if p.kind() == "export_statement" {
                return true;
            }
            cur = p.parent();
        }
        false
    }

    fn has_keyword_child(&self, node: Node, kw: &str) -> bool {
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                if c.kind() == kw {
                    return true;
                }
            }
        }
        false
    }

    fn is_async(&self, node: Node) -> bool {
        self.has_keyword_child(node, "async")
    }

    /// TS has an isStatic hook; JS does not (None = field absent).
    fn is_static(&self, node: Node) -> Option<bool> {
        if self.variant.is_ts() {
            Some(self.has_keyword_child(node, "static"))
        } else {
            None
        }
    }

    fn is_const_decl(&self, node: Node) -> bool {
        node.kind() == "lexical_declaration" && self.has_keyword_child(node, "const")
    }

    // (extract_* functions continue in impl blocks below)
}

/// classifyTsClassMember (#808): a class field is a METHOD only when its value
/// is callable (arrow / function expression / HOF call wrapping one).
#[derive(PartialEq)]
enum Member {
    Method,
    Property,
}

fn classify_ts_class_member(node: Node) -> Member {
    if !matches!(node.kind(), "public_field_definition" | "field_definition") {
        return Member::Method;
    }
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else { continue };
        if matches!(child.kind(), "arrow_function" | "function_expression") {
            return Member::Method;
        }
        if child.kind() == "call_expression" {
            if let Some(args) = child.child_by_field_name("arguments") {
                for j in 0..args.named_child_count() {
                    if let Some(arg) = args.named_child(j) {
                        if matches!(arg.kind(), "arrow_function" | "function_expression") {
                            return Member::Method;
                        }
                    }
                }
            }
        }
    }
    Member::Property
}

/// typescriptExtractor.resolveBody / javascriptExtractor.resolveBody: the body
/// of a function-valued class field, nested in the arrow / HOF-wrapped arrow.
fn resolve_field_body(node: Node) -> Option<Node> {
    if !matches!(node.kind(), "public_field_definition" | "field_definition") {
        return None;
    }
    for i in 0..node.named_child_count() {
        let child = node.named_child(i)?;
        if matches!(child.kind(), "arrow_function" | "function_expression") {
            return child.child_by_field_name("body");
        }
        if child.kind() == "call_expression" {
            if let Some(args) = child.child_by_field_name("arguments") {
                for j in 0..args.named_child_count() {
                    if let Some(arg) = args.named_child(j) {
                        if matches!(arg.kind(), "arrow_function" | "function_expression") {
                            return arg.child_by_field_name("body");
                        }
                    }
                }
            }
        }
    }
    None
}

/// resolveBody ?? getChildByField(node, 'body') — the body-walk resolution.
fn body_of(node: Node) -> Option<Node> {
    resolve_field_body(node).or_else(|| node.child_by_field_name("body"))
}

fn opt_str(arena: &mut Arena, s: Option<&str>) -> StrRef {
    match s {
        Some(s) => arena.put(s),
        None => NONE_STR,
    }
}
