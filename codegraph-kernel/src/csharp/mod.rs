//! C# extraction — a faithful Rust port of `TreeSitterExtractor`'s C# paths
//! (src/extraction/tree-sitter.ts) plus languages/csharp.ts.
//!
//! Same porting contract as the other walkers: behavior parity with the wasm
//! path, bug-for-bug, verified by scripts/kernel-parity.mjs and the full-index
//! dump-diff gate. The authoritative quirk list is
//! docs/design/csharp-kernel-port-checklist.md — including every deliberate
//! emission hole (property/accessor bodies, constructor initializers,
//! delegates/events/operators/indexers, top-level locals) and garbage ref
//! (`(repo)` primary-ctor extends, `: byte` enum extends, `nameof` calls)
//! this file preserves on purpose. Positions in UTF-16 code units.
//! Files with parse errors are walked like any other (tree-sitter's recovery is canonical; buffers::parse_collapse_warning reports a collapsed parse).
//!
//! preParse (#237 `#if` blanking) stays TS-side: the route point hoists it, so
//! the kernel receives pre-blanked bytes — port NOTHING of it here (its regex
//! carries JS `(?m)`/CRLF semantics that must not be re-implemented).

mod calls;
mod types;
mod refs;
use crate::buffers::{
    node_kind_index, Arena, BoolFlags, EdgeRow, EmitOut, NodeRow,
    RefRow, Tables, FLAG_IS_ASYNC, FLAG_IS_STATIC,
    NONE, NONE_STR,
};
use crate::walker::named_kids;
use crate::walker::{Scope, ValueScope, Cand};
use crate::textutil::{is_builtin_type, strip_generic_and_qualifier, capitalized_re};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tree_sitter::Node;




/// extractCsharpReturnType's trailing-nullable strip (`/\?+$/`).
fn trailing_nullable_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\?+$").unwrap())
}



#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<u8>,
    is_static: Option<bool>,
    is_async: Option<bool>,
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
    /// Node id string per row — ids COLLIDE for same-(kind, name, line) nodes
    /// and the TS side's fn-ref dedupe / value-ref self-checks key on the id.
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
    let tree = crate::langs::parse("csharp", source)?;

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

    // File node (TreeSitterExtractor.extract). Source here is the pre-blanked
    // text (the route point hoists preParse), identical bytes on both arms.
    let line_count = w.cols.line_count();
    let base_name = crate::buffers::push_file_node(&mut w.arena, &mut w.tables, file_path, line_count);
    w.node_ids.push(ids::file_node_id(file_path));
    w.stack.push(Scope { row: 0, kind: "file", name: base_name.to_string() });

    // extractFilePackage: the FIRST top-level namespace declaration mints ONE
    // `namespace` node that stays pushed for the ENTIRE file — a second
    // top-level namespace's types nest under the first's node/QN, nested
    // namespaces leave no trace, and every import ref in a namespaced file
    // hangs off this node (checklist §namespace).
    let root = tree.root_node();
    let mut pkg_pushed = false;
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else { continue };
        if child.kind() != "namespace_declaration"
            && child.kind() != "file_scoped_namespace_declaration"
        {
            continue;
        }
        // csharpExtractor.extractPackage: `name` field ?? first
        // qualified_name/identifier named child. No trim.
        let name_node = child.child_by_field_name("name").or_else(|| {
            named_kids(child)
                .find(|c| matches!(c.kind(), "qualified_name" | "identifier"))
        });
        if let Some(name_node) = name_node {
            let pkg = w.text(name_node).to_string();
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

/// classifyClassNode (languages/csharp.ts): a record_declaration with an
/// anonymous `struct` keyword child is a record struct.
fn record_is_struct(node: Node) -> bool {
    (0..node.child_count())
        .filter_map(|i| node.child(i))
        .any(|c| c.kind() == "struct")
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
        let end_line = node.end_position().row as u32 + 1; // no resolveBody for csharp

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
            decorators: NONE_STR, // C# extraction emits no decorators (checklist §decorators)
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
        self.capture_value_ref_scope(kind, name, row, node);
        Some(row)
    }

    // --- hooks (languages/csharp.ts) --------------------------------------------
    //
    // C# modifiers are individual named `modifier` children — there is NO
    // Java-style `modifiers` wrapper (probed).

    /// getVisibility: FIRST `modifier` child whose text is one of the four
    /// levels wins; none → private (the C# default).
    fn visibility_of(&self, node: Node) -> u8 {
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            if child.kind() == "modifier" {
                match self.text(child) {
                    "public" => return 1,
                    "private" => return 2,
                    "protected" => return 3,
                    "internal" => return 4,
                    _ => {}
                }
            }
        }
        2 // C# defaults to private
    }

    fn is_static(&self, node: Node) -> bool {
        (0..node.child_count())
            .filter_map(|i| node.child(i))
            .any(|c| c.kind() == "modifier" && self.text(c) == "static")
    }

    fn is_async(&self, node: Node) -> bool {
        (0..node.child_count())
            .filter_map(|i| node.child(i))
            .any(|c| c.kind() == "modifier" && self.text(c) == "async")
    }

    /// isConst: `const` → true; else `static` AND `readonly` both present.
    fn is_const(&self, node: Node) -> bool {
        let mut has_static = false;
        let mut has_readonly = false;
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            if child.kind() != "modifier" {
                continue;
            }
            match self.text(child) {
                "const" => return true,
                "static" => has_static = true,
                "readonly" => has_readonly = true,
                _ => {}
            }
        }
        has_static && has_readonly
    }

    /// extractCsharpReturnType — reads the `returns` field; feeds the
    /// #645/#608 chained-call resolution. Constructors have no `returns`.
    fn return_type_of(&self, node: Node) -> Option<String> {
        let t = node.child_by_field_name("returns")?;
        if matches!(t.kind(), "predefined_type" | "array_type") {
            return None;
        }
        let mut s = self.text(t).trim().to_string();
        s = trailing_nullable_re().replace(&s, "").into_owned();
        s = crate::textutil::generic_args_re().replace_all(&s, "").into_owned();
        let last = s.rsplit('.').next().unwrap_or("").trim().to_string();
        if last.is_empty() || !crate::textutil::ascii_ident_re().is_match(&last) {
            return None;
        }
        Some(last)
    }

    extract_name_impl!();

    // --- the dispatcher (visitNode, C#-relevant branches) -----------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if self.extract_type_decl(node) {
            skip_children = true;
        } else if kind == "method_declaration" || kind == "constructor_declaration" {
            self.extract_method(node);
            skip_children = true;
        } else if kind == "property_declaration" && self.inside_class_like() {
            // Property accessor/expression bodies are NEVER walked (calls
            // inside are lost by design) — candidates-only scan.
            self.extract_property(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "field_declaration" && self.inside_class_like() {
            self.extract_field(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "local_declaration_statement" && !self.inside_class_like() {
            // Top-level statements: extractVariable's generic fallback finds no
            // direct identifier/variable_declarator children (C# nests them in
            // variable_declaration) → ZERO nodes, zero refs. Candidates only.
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "using_directive" {
            self.extract_import(node);
            // no skipChildren (TS importTypes branch) — children visited below
        } else if kind == "invocation_expression" {
            self.extract_call(node);
        } else if kind == "object_creation_expression" {
            self.extract_instantiation(node);
            if let Some(anon_body) = find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                skip_children = true;
            }
        }
        // Everything else (namespace_declaration, global_statement, delegates,
        // events, operators, indexers, destructors, local functions, preproc_*)
        // falls through: no node minted, children visited — their bodies' calls
        // attribute to the enclosing scope (checklist §dispatch).

        if !skip_children {
            for c in named_kids(node) {
                self.visit_node(c);
            }
        }
    }

    // --- visitFunctionBody ------------------------------------------------------


    fn visit_for_calls_and_structure(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "invocation_expression" {
            self.extract_call(node);
        } else if kind == "object_creation_expression" {
            self.extract_instantiation(node);
            if let Some(anon_body) = find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                return;
            }
        }

        // Static value reads (`ReadType.ReadAsDouble`) — body walker only.
        self.extract_static_member_ref(node);

        // (variable_declarator type-annotation branch: C# has no
        // `type_annotation` child node — structurally inert, not ported.
        // functionTypes is empty — no nested-function branch.)

        if self.extract_type_decl(node) {
            return;
        }

        for c in named_kids(node) {
            self.visit_for_calls_and_structure(c);
        }
    }

    // --- extractors --------------------------------------------------------------

    /// A class/record/struct/interface/enum declaration, extracted fully
    /// (children skipped); false for any other node. classifyClassNode: a
    /// `record struct` is a struct.
    fn extract_type_decl(&mut self, node: Node<'t>) -> bool {
        match node.kind() {
            "record_declaration" if record_is_struct(node) => self.extract_struct(node),
            "class_declaration" | "record_declaration" => self.extract_class(node),
            "struct_declaration" | "record_struct_declaration" => self.extract_struct(node),
            "interface_declaration" => self.extract_interface(node),
            "enum_declaration" => self.extract_enum(node),
            _ => return false,
        }
        true
    }

    fn extract_class(&mut self, node: Node<'t>) {
        stack_guard!();
        // skipBodilessClass unset: a bodiless `record Empty;` still mints a node.
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default() // isExported hook absent → flag not present
        };
        let Some(row) = self.create_node("class", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.extract_primary_ctor_param_refs(node, row);
        // extractDecoratorsFor: C# attributes never match its accepted node
        // types (attribute_list is skipped, its children never reached) —
        // zero `decorates` refs; the call slot emits nothing.

        self.stack.push(Scope { row, kind: "class", name });
        // body ?? node: a bodiless record's own children are iterated
        // "harmlessly" — visit_node on identifier/parameter_list/base_list
        // children falls through (base-arg identifiers still feed fn-ref
        // capture, mirroring the TS walk).
        let body = node.child_by_field_name("body").unwrap_or(node);
        for c in named_kids(body) {
            self.visit_node(c);
        }
        // no synthesizeMembers for C#
        self.stack.pop();
    }

    fn extract_struct(&mut self, node: Node<'t>) {
        stack_guard!();
        // Body gate — EXCEPT C# positional records (`record struct M(…);`,
        // node type record_declaration), complete definitions with no body.
        // A bodiless `struct Fwd;` mints NO node. (#831)
        let body = node.child_by_field_name("body");
        if body.is_none() && node.kind() != "record_declaration" {
            return;
        }
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: Some(self.visibility_of(node)),
            ..Extra::default()
        };
        let Some(row) = self.create_node("struct", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.extract_primary_ctor_param_refs(node, row);
        // NOTE: extractStruct does NOT call extractDecoratorsFor (TS parity).
        if let Some(body) = body {
            self.stack.push(Scope { row, kind: "struct", name });
            for c in named_kids(body) {
                self.visit_node(c);
            }
            self.stack.pop();
        }
    }

    fn extract_interface(&mut self, node: Node<'t>) {
        stack_guard!();
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            ..Extra::default() // NO visibility — extractInterface never asks
        };
        let Some(row) = self.create_node("interface", &name, node, extra) else { return };
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "interface", name });
        let body = node.child_by_field_name("body").unwrap_or(node);
        for c in named_kids(body) {
            self.visit_node(c);
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
        // The underlying type (`enum ReadType : byte`) sits in base_list →
        // an `extends` ref named `byte` (garbage, PRESERVE).
        self.extract_inheritance(node, row);
        self.stack.push(Scope { row, kind: "enum", name });
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else { continue };
            if child.kind() == "enum_member_declaration" {
                self.extract_enum_members(child);
            } else {
                self.visit_node(child);
            }
        }
        self.stack.pop();
    }

    fn extract_enum_members(&mut self, node: Node<'t>) {
        // name-field path: one enum_member node positioned at the MEMBER node
        // (attributes included in its span); values/attributes ignored.
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.text(name_node).to_string();
            self.create_node("enum_member", &name, node, Extra::default());
        }
        // (identifier-children / leaf fallbacks are other grammars' shapes)
    }

    /// extractProperty (1986) — property_declaration only (dispatch-gated to
    /// class-like scopes). Accessor bodies and `=>` value clauses are never
    /// walked; type refs DO come from the `type` field.
    fn extract_property(&mut self, node: Node<'t>) {
        let docstring = preceding_docstring(node, self.src);
        let visibility = Some(self.visibility_of(node));
        let is_static = Some(self.is_static(node)); // ?? false — always concrete

        let name_node = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("property"))
            .or_else(|| {
                named_kids(node)
                    .find(|c| c.kind() == "identifier")
            });
        let Some(name_node) = name_node else { return };
        let name = self.text(name_node).to_string();
        if name.is_empty() {
            return;
        }

        // Generic scan (isTsJsField=false): FIRST namedChild that isn't a
        // modifier/name/accessor/initializer. A BARE-identifier declared type
        // (`public Widget Parent {get;}`) is excluded by the `identifier`
        // filter → the signature loses its type (QUIRK, preserve); the type
        // ref below still fires via the `type` FIELD.
        let type_node = named_kids(node)
            .find(|c| {
                !matches!(
                    c.kind(),
                    "modifier" | "modifiers" | "identifier" | "accessor_list" | "accessors"
                        | "equals_value_clause"
                )
            });
        let type_text = type_node.map(|t| {
            let raw = self.text(t);
            // TS `.replace(/^:\s*/, '')` — inert for C# type text; mirrored.
            match raw.strip_prefix(':') {
                Some(rest) => rest.trim_start_matches(crate::textutil::is_js_space).to_string(),
                None => raw.to_string(),
            }
        });
        let signature = match &type_text {
            Some(t) => format!("{t} {name}"),
            None => name.clone(),
        };

        let row = self.create_node(
            "property",
            &name,
            node,
            Extra { docstring, signature: Some(signature), visibility, is_static, ..Extra::default() },
        );
        if let Some(row) = row {
            // decorators: none for C#; then the csharp type-ref path.
            self.extract_csharp_type_refs(node, row);
        }
    }

    /// extractField (2046) — field_declaration; each declarator becomes a
    /// field/constant node anchored at the DECLARATOR.
    fn extract_field(&mut self, node: Node<'t>) {
        let docstring = preceding_docstring(node, self.src);
        let visibility = Some(self.visibility_of(node));
        let is_static = Some(self.is_static(node));
        // `const` / `static readonly` → constant (value-ref targets).
        let field_kind: &'static str = if self.is_const(node) { "constant" } else { "field" };

        // Direct declarators (Java shape) — none for C#; the wrapper path:
        let mut declarators: Vec<Node> = named_kids(node)
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        let var_decl = named_kids(node)
            .find(|c| c.kind() == "variable_declaration");
        if declarators.is_empty() {
            if let Some(vd) = var_decl {
                declarators = named_kids(vd)
                    .filter(|c| c.kind() == "variable_declarator")
                    .collect();
            }
        }
        // (PHP property_element branch: unreachable for C#.)

        if !declarators.is_empty() {
            let type_search = var_decl.unwrap_or(node);
            let type_node = named_kids(type_search)
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
                    named_kids(decl)
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
                    // decorators: none; type refs from the OUTER declaration —
                    // multi-declarator fields emit the type refs once PER
                    // declarator, each from its own field node.
                    self.extract_csharp_type_refs(node, row);
                    // The ladder skips a field's children, so the declarator's
                    // string literals are reached here, owned by the field.
                    self.markdown_refs_from_subtree(decl, row);
                }
            }
        } else {
            // Bare fallback (unreachable on non-erroring C#; ported for shape).
            let name_node = node.child_by_field_name("name").or_else(|| {
                named_kids(node)
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

    /// extractMethod (1737) — method_declaration + constructor_declaration.
    /// Signature is ALWAYS undefined (no getSignature hook); isAsync is real.
    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        if !self.inside_class_like() {
            // Unreachable on non-erroring C# (top-level `void M(){}` parses as
            // local_function_statement; erroring files defer) — mirror the TS
            // treat-as-function tail for shape.
            self.extract_function(node);
            return;
        }
        let name = self.extract_name(node);
        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            signature: None,
            visibility: Some(self.visibility_of(node)),
            is_async: Some(self.is_async(node)),
            is_static: Some(self.is_static(node)),
            return_type: self.return_type_of(node),
        };
        let Some(row) = self.create_node("method", &name, node, extra) else { return };
        // extractTypeAnnotations short-circuits into the csharp path:
        // `returns`-field refs FIRST, then per-parameter type refs.
        self.extract_csharp_type_refs(node, row);
        // decorators: none.
        self.stack.push(Scope { row, kind: "method", name });
        // The `body` FIELD only (block or arrow_expression_clause). A
        // constructor_initializer (`: base(args)`) is NOT the body → its
        // argument calls are LOST (quirk, preserve).
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    /// extractFunction — only reachable for a method outside any class
    /// (unreachable on non-erroring C#; kept faithful to the generic tail).
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
            signature: None,
            visibility: Some(self.visibility_of(node)),
            is_async: Some(self.is_async(node)),
            is_static: Some(self.is_static(node)),
            return_type: self.return_type_of(node),
        };
        let Some(row) = self.create_node("function", &name, node, extra) else { return };
        self.extract_csharp_type_refs(node, row);
        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_variable(&mut self, node: Node<'t>) {
        // extractVariable's generic fallback: direct identifier /
        // variable_declarator children only — C# nests declarators inside
        // variable_declaration, so this NEVER fires (`var x = F();` at top
        // level produces no node, no calls ref, no instantiates — preserve).
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

    /// extractImport via csharpExtractor.extractImport: moduleName = first
    /// qualified_name child's text, else first identifier's — with the alias
    /// quirks (alias-to-qualified keeps generic args on the TARGET text;
    /// alias-to-identifier captures the ALIAS name) preserved verbatim.
    fn extract_import(&mut self, node: Node<'t>) {
        let import_text = self.text(node).trim().to_string();
        let target = named_kids(node)
            .find(|c| c.kind() == "qualified_name")
            .or_else(|| {
                named_kids(node)
                    .find(|c| c.kind() == "identifier")
            });
        let Some(target) = target else { return }; // hook declined → no node, no ref
        let module_name = self.text(target).to_string();
        if module_name.is_empty() {
            return;
        }
        self.create_node(
            "import",
            &module_name,
            node,
            Extra { signature: Some(import_text), ..Extra::default() },
        );
        // One generic `imports` ref from the stack top (the namespace node in
        // a namespaced file, else the file node). No per-binding emitter.
        let parent = self.top_row();
        self.push_ref_at(parent, &module_name, crate::buffers::EDGE_IMPORTS, node);
    }

    // --- C# type-reference engine (extractCsharpTypeRefs, 5893) -----------------

    // --- function-as-value refs (CSHARP_SPEC, function-ref.ts:250) --------------

    flush_fn_ref_candidates_impl!();

    // --- value references --------------------------------------------------------

}

fn find_anonymous_class_body(node: Node) -> Option<Node> {
    for child in named_kids(node) {
        if matches!(child.kind(), "class_body" | "declaration_list") {
            return Some(child);
        }
    }
    None
}


