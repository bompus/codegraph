//! C / C++ extraction — a faithful Rust port of `TreeSitterExtractor`'s c/cpp
//! paths (src/extraction/tree-sitter.ts) plus languages/c-cpp.ts, one dual-
//! language module flagged like tsjs/ (checklist:
//! docs/design/ccpp-kernel-port-checklist.md — read it before editing).
//!
//! The seven preParse blanking passes are NOT here: the TS route point
//! (src/extraction/kernel/index.ts) applies `extractor.preParse` before the
//! kernel call, so this walker receives the SAME blanked bytes the wasm
//! extractor parses (all blanks are equal-length-space replacements — every
//! offset survives). `.metal`/`.cu`/`.cuh` arrive as language 'cpp' with their
//! dialect blanks already applied.
//!
//! Quirks mirrored bug-for-bug (each pinned by the parity gates):
//!  - cpp namespace prefix stack (#1291): named `namespace a::b {` pushes the
//!    name AS WRITTEN onto the qualifiedName prefix; anonymous falls through.
//!    No namespace NODE is minted (#1093 crowd-out).
//!  - out-of-line `Cls::method` defs: name = LAST `::` segment of the
//!    declarator's qualified_identifier (BFS that skips parameter_list +
//!    trailing_return_type), receiver = the template-stripped qualifier,
//!    qualifiedName composed against the namespace prefix with the re-spelled-
//!    prefix anchor rule; owner `contains` edge to the FIRST earlier
//!    struct/class/enum/trait of the receiver's bare name.
//!  - macro-name salvage: recoverCppMacroDefinedName (ALL-CAPS macro def whose
//!    real name is the lone first argument) at resolveName, and
//!    recoverMangledCppName (glued "Ret name" → last token) as the universal
//!    post-hoc net for BOTH c and cpp.
//!  - `class MACRO Name` misparse residue: isMisparsedFunction drops the
//!    phantom function (name starts `namespace`, C++ keywords, or the bodyless
//!    class/struct `type` + non-function_declarator shape, #946/#1061) but
//!    still walks the body.
//!  - C file-scope variables: init/pointer/array declarators only — a BARE
//!    identifier declarator is the macro-prototype misparse and is skipped
//!    (loses uninit scalars by design); cpp declarations instead take the TS
//!    GENERIC fallback (direct identifier children only → `int x;` extracts,
//!    `int x = 5;` does not — bug-for-bug).
//!  - inheritance quirk: extractInheritance recurses into
//!    field_declaration_list, where a field_declaration with no DIRECT
//!    field_identifier child (pointer/array/method members) but a direct
//!    type_identifier emits an `extends` ref to that type (the Go-embedding
//!    branch matching c/cpp shapes). Kept: the parity gate pins today's graph.
//!  - static-member/value-read pass (cpp only): `field_expression` is in
//!    MEMBER_ACCESS_TYPES (listed for Scala, same node kind in cpp), so
//!    `Capitalized.member` / `Capitalized->member` VALUE reads emit
//!    `references` refs; qualified_identifier is checked too but its scope
//!    child is namespace_identifier/template_type/…, never a plain
//!    identifier, so it can't emit.
//!  - explicit operator calls (#1247) ride an ERROR child; erroring files
//!    are extracted natively, so this branch is live.
//!  - local fn-pointer fan-out (#932-adjacent): `auto k = &fn<…>;` records
//!    per-caller targets (insertion-ordered, branch reassignments accumulate);
//!    a later bare `k(args)` emits one `calls` ref PER target and suppresses
//!    the local name. Template args stripped like base-class refs (#1043).
//!  - pure-virtual methods (#1727): cpp in-class `virtual T f(...) = 0;` is a
//!    `field_declaration` (not `function_definition`); mint a method node so
//!    abstract-base calls and cpp-override synthesis have a target. Mirrors
//!    TS `methodTypes` + `classifyMethodNode` / `isAbstract`.
//!  - callable members (c-fnptr-field-nodes): a `field_declaration` whose
//!    declarator is a fn-pointer — `int (*read)(int)`, a fn-ptr typedef
//!    member `hook_fn read`, or a `typedef void cb_t(void)` member behind
//!    `*` — mints a `field` node qualified `Owner::name`, the graph target
//!    `s->fp(...)`/`x.fp(...)` calls resolve to. Scalar/array/plain members
//!    mint nothing; the typedef registries are file-local.
//!  - stack construction (#1035): cpp `declaration` with class-like named
//!    `type` and an init_declarator whose value is argument_list /
//!    initializer_list → `instantiates` (most-vexing-parse excluded).
//!  - value-reference edges: C only (VALUE_REF_LANGS has 'c', not 'cpp') —
//!    shadow prune via init_declarator counts, crate::walker::MAX_VALUE_REF_NODES cap,
//!    CODEGRAPH_VALUE_REFS=0 kill switch.
//!  - fn-ref capture (#756): cFamilySpec for both; cpp adds addressOfOnly
//!    (bare identifiers only qualify in file-scope value/list positions).
//!
//! Files with parse errors are extracted natively; the kernel's error recovery
//! is canonical (kernel-only-extraction-plan.md, Phase 1).

mod variables;
mod types;
mod names;
mod calls;
mod fnptr_fields;
mod bindings;
mod refs;
use crate::buffers::{
    BINDING_DECL, BINDING_IMPORT, BINDING_LOCAL, BINDING_PARAM, node_kind_index, Arena, BoolFlags, EmitOut, NodeRow,
    RefRow, Tables, FLAG_IS_ABSTRACT, FLAG_IS_EXPORTED, FUNCTION_REF_CODE, NONE, NONE_STR,
};
use crate::walker::named_kids;
use crate::walker::{Scope, ValueScope};
use crate::textutil::{is_stoplisted, is_literal_receiver, capitalized_re};
use crate::docstring::preceding_docstring;
use crate::ids;
use crate::textutil as util;
use regex::Regex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
use tree_sitter::Node;


// --- compiled regexes (JS \w/\s spelled as ASCII classes for parity) ---------

/// recoverCppMacroDefinedName: macro-shaped parsed name.
fn macro_shaped_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$").unwrap())
}
fn has_lower_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z]").unwrap())
}
/// normalizeCppReturnType: smart-pointer/optional unwrap.
fn ret_wrapper_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?-u:\b)(?:std\s*::\s*)?(?:unique_ptr|shared_ptr|weak_ptr|optional)\s*<\s*([^,>]+?)\s*>")
            .unwrap()
    })
}
fn ret_keyword_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?-u:\b)(?:const|volatile|typename|struct|class|enum)(?-u:\b)").unwrap())
}
fn ptr_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[*&]+").unwrap())
}
fn ws_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}
/// recoverMangledCppName's `Ret (name)` idiom guard.
fn ret_paren_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\S+\s+\([A-Za-z_][A-Za-z0-9_]*\)").unwrap())
}
/// Operator-call receiver: simple identifier / dotted member chain.
fn operator_receiver_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_.]*$").unwrap())
}
/// Symbolic operator tail (`/^[^\w\s]/` in JS).
fn symbolic_op_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Za-z0-9_\s]").unwrap())
}
/// normalizeValue's qualified `&Cls::m` member-pointer test (`/^[A-Za-z_][\w:]*$/`).
fn qualified_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_:]*$").unwrap())
}

/// CPP_NON_CLASS_RETURN (languages/c-cpp.ts).
fn is_non_class_return(name: &str) -> bool {
    matches!(
        name,
        "void" | "bool" | "char" | "short" | "int" | "long" | "float" | "double" | "unsigned"
            | "signed" | "size_t" | "ssize_t" | "auto" | "wchar_t" | "char8_t" | "char16_t"
            | "char32_t" | "int8_t" | "int16_t" | "int32_t" | "int64_t" | "uint8_t" | "uint16_t"
            | "uint32_t" | "uint64_t" | "intptr_t" | "uintptr_t" | "nullptr_t"
    )
}

/// CPP_PRIMITIVE_NAMES (languages/c-cpp.ts) — recoverMangledCppName's guard.
fn is_cpp_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "bool" | "void" | "int" | "char" | "short" | "long" | "float" | "double" | "unsigned"
            | "signed" | "wchar_t" | "char8_t" | "char16_t" | "char32_t" | "char_t" | "size_t"
            | "auto" | "const" | "struct" | "class" | "enum" | "union" | "typename"
    )
}



/// stripCppTemplateArgs (languages/c-cpp.ts): depth-counted removal of every
/// balanced `<…>` group; `<` and `>` never reach the output.
fn strip_cpp_template_args(name: &str) -> String {
    if !name.contains('<') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut depth = 0u32;
    for ch in name.chars() {
        if ch == '<' {
            depth += 1;
        } else if ch == '>' {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// recoverMangledCppName (languages/c-cpp.ts) — universal post-hoc salvage for
/// a name still mangled by an unblanked macro ("Ret name" → "name").
fn recover_mangled_cpp_name(name: String) -> String {
    if !name.chars().any(|c| c.is_whitespace())
        || name.starts_with("operator")
        || name.starts_with('~')
    {
        return name;
    }
    if ret_paren_name_re().is_match(&name) {
        return name; // `Ret (name)` idiom — leave alone
    }
    let before_params = match name.find('(') {
        Some(i) => &name[..i],
        None => &name[..],
    };
    // (JS: `beforeParams.trim().split(/\s+/)` — split_whitespace already
    // ignores leading/trailing whitespace, so no explicit trim.)
    let candidate = before_params.split_whitespace().last().unwrap_or("");
    if candidate.is_empty()
        || !crate::textutil::ascii_ident_re().is_match(candidate)
        || is_cpp_primitive_name(candidate)
    {
        return name;
    }
    candidate.to_string()
}

/// normalizeCppReturnType (languages/c-cpp.ts).
fn normalize_cpp_return_type(raw: &str) -> Option<String> {
    let mut t = raw.trim().to_string();
    if t.is_empty() {
        return None;
    }
    if let Some(c) = ret_wrapper_re().captures(&t) {
        if let Some(inner) = c.get(1) {
            t = inner.as_str().to_string();
        }
    }
    let t = ret_keyword_re().replace_all(&t, " ");
    let t = crate::textutil::generic_args_re().replace_all(&t, " ");
    let t = ptr_ref_re().replace_all(&t, " ");
    let t = ws_run_re().replace_all(&t, " ");
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split("::").filter(|p| !p.is_empty()).collect();
    let last = *parts.last()?;
    if is_non_class_return(last) || !crate::textutil::ascii_ident_re().is_match(last) {
        return None;
    }
    Some(last.to_string())
}

/// JS `String.replace(/->/g,'.').replace(/\s+/g,'')` used on receivers.
fn arrow_dot_no_ws(s: &str) -> String {
    s.replace("->", ".").chars().filter(|c| !c.is_whitespace()).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    C,
    Cpp,
}


#[derive(Default)]
struct Extra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<u8>,
    is_exported: Option<bool>,
    is_abstract: Option<bool>,
    return_type: Option<String>,
    qualified_name: Option<String>,
}


/// Capture mode for a fn-ref candidate (gate policy keys on it).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Args,
    Rhs,
    Value,
    List,
    Varinit,
}

struct Cand {
    from: u32,
    name: String,
    mode: Mode,
    explicit_ref: bool,
    line: u32,
    column_byte: usize,
    row: usize,
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
    variant: Variant,
    cols: util::Cols,
    arena: Arena,
    tables: Tables,
    stack: Vec<Scope>,
    nodes_meta: Vec<NodeMeta>,
    node_ids: Vec<String>,
    /// C/C++ enclosing `namespace ns { … }` names (cpp only ever non-empty).
    namespace_prefix: Vec<String>,
    /// cppLocalFnPtrs: caller row → local name → insertion-ordered targets.
    local_fn_ptrs: HashMap<u32, HashMap<String, Vec<String>>>,
    /// Function-pointer typedef names seen in this file — `typedef int
    /// (*hook_fn)(int)` — so a member declared `hook_fn read;` mints a
    /// `field` node. File-local only: a typedef living in another header is
    /// unknown here, and the member then mints nothing (never a guess).
    fn_ptr_typedefs: HashSet<String>,
    /// Function-TYPE typedefs — `typedef void cb_t(void)` — callable only
    /// through an explicit `*` member declarator (`cb_t *cbp`).
    fn_type_typedefs: HashSet<String>,
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

pub fn extract(file_path: &str, source: &str, language: &str) -> Result<EmitOut, String> {
    let variant = match language {
        "c" => Variant::C,
        "cpp" => Variant::Cpp,
        other => return Err(format!("ccpp walker got language '{other}'")),
    };
    let t0 = std::time::Instant::now();
    let tree = crate::langs::parse(language, source)?;
    let mut w = Walker::new(source, file_path, variant);

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
    fn new(source: &'t str, file_path: &'t str, variant: Variant) -> Walker<'t> {
        Walker {
            src: source,
            file_path,
            variant,
            cols: util::Cols::new(source),
            arena: Arena::default(),
            tables: Tables::default(),
            stack: Vec::new(),
            nodes_meta: Vec::new(),
            node_ids: Vec::new(),
            namespace_prefix: Vec::new(),
            local_fn_ptrs: HashMap::new(),
            fn_ptr_typedefs: HashSet::new(),
            fn_type_typedefs: HashSet::new(),
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

    inside_class_like_impl!("class" | "struct" | "union" | "interface" | "trait" | "enum" | "module");

    push_ref_impl!();

    fn create_node(&mut self, kind: &'static str, name: &str, node: Node<'t>, extra: Extra) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let start_line = self.line_of(node);
        let id = ids::node_id(self.file_path, kind, name, start_line);
        // (c/cpp define no resolveBody hook, so createNode's endLine extension
        // for sibling-body grammars never fires — endLine is the node's own.)
        let end_line = node.end_position().row as u32 + 1;

        let qualified = extra.qualified_name.unwrap_or_else(|| {
            let mut parts: Vec<&str> = self.namespace_prefix.iter().map(|s| s.as_str()).collect();
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
        if let Some(v) = extra.is_abstract {
            flags.set(FLAG_IS_ABSTRACT, v);
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
        self.tables.push_contains(parent_row, row);

        if kind == "function" || kind == "method" {
            self.defined_fn_names.insert(name.to_string());
        }
        // captureValueRefScope (capture is variant-agnostic like the TS side;
        // flushValueRefs gates on the language — C only).
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
        self.emit_decl_binding(kind, name, row, node);
        if kind == "function" || kind == "method" {
            self.emit_param_bindings(node);
        }
        Some(row)
    }

    // --- name extraction -----------------------------------------------------

    /// extractName: extractNameRaw + the universal recoverMangledName net
    /// (wired for BOTH c and cpp in languages/c-cpp.ts).
    fn extract_name(&self, node: Node) -> String {
        recover_mangled_cpp_name(self.extract_name_raw(node))
    }

    /// cppExtractor.getVisibility: the FIRST access_specifier among the
    /// parent's children decides (document order, not nearest-preceding —
    /// bug-for-bug with the TS loop).
    fn visibility_of(&self, node: Node) -> Option<u8> {
        let parent = node.parent()?;
        for i in 0..parent.child_count() {
            let Some(child) = parent.child(i) else { continue };
            if child.kind() == "access_specifier" {
                let text = self.text(child);
                if text.contains("public") {
                    return Some(1);
                }
                if text.contains("private") {
                    return Some(2);
                }
                if text.contains("protected") {
                    return Some(3);
                }
            }
        }
        None
    }


    // --- visitNode -----------------------------------------------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        // C++ namespace blocks: prefix-only, no node (#1291/#1093). Anonymous
        // namespaces fall through to the generic walk. (No markdown scan
        // before this early return: a namespace node is never a string.)
        if self.variant == Variant::Cpp && kind == "namespace_definition" {
            let ns_name = node
                .child_by_field_name("name")
                .map(|n| self.text(n).to_string())
                .unwrap_or_default();
            if !ns_name.is_empty() {
                self.namespace_prefix.push(ns_name);
                for c in named_kids(node) {
                    self.visit_node(c);
                }
                self.namespace_prefix.pop();
                return;
            }
        }

        self.maybe_capture_fn_refs(node);
        let md_owner = self.top_row();
        self.markdown_refs_from_string(node, md_owner);

        if kind == "function_definition" {
            // functionTypes for both; cpp's methodTypes also lists it, so
            // inside a class-like scope it extracts as a method.
            if self.inside_class_like() && self.variant == Variant::Cpp {
                self.extract_method(node);
            } else {
                self.extract_function(node);
            }
            skip_children = true;
        } else if self.variant == Variant::Cpp && kind == "class_specifier" {
            self.extract_class(node);
            skip_children = true;
        } else if kind == "struct_specifier" {
            self.extract_aggregate(node, "struct");
            skip_children = true;
        } else if kind == "union_specifier" {
            self.extract_aggregate(node, "union");
            skip_children = true;
        } else if kind == "enum_specifier" {
            self.extract_enum(node);
            skip_children = true;
        } else if kind == "type_definition"
            || (self.variant == Variant::Cpp && kind == "alias_declaration")
        {
            // Register fn-pointer typedefs BEFORE extract_type_alias consumes
            // the node — a later struct member declared `hook_fn read;` is a
            // callable field only if the typedef is known.
            self.register_fn_typedefs(node);
            skip_children = self.extract_type_alias(node);
        } else if kind == "declaration" && !self.inside_class_like() {
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if kind == "field_declaration" && self.inside_class_like() {
            if self.variant == Variant::Cpp && self.is_cpp_pure_virtual_method_decl(node) {
                // Pure-virtual methods have no `function_definition` body — mint the
                // method node so calls through the abstract base and cpp-override
                // synthesis have a target (#1727). Non-pure field_declarations fall
                // through to the children walk (data members / prototypes).
                self.extract_method(node);
                skip_children = true;
            } else {
                // Callable function-pointer members mint `field` nodes so
                // `s->fp(...)`/`x.fp(...)` calls have a graph target. Children
                // still walk — a nested struct/union specifier lives there.
                self.extract_callable_fields(node);
            }
        } else if kind == "preproc_include" {
            self.extract_import(node);
        } else if kind == "call_expression" {
            self.extract_call(node);
        } else if kind == "new_expression" {
            // INSTANTIATION_KINDS: cpp `new Foo(...)`. (No anonymous-class
            // body exists under new_expression in this grammar; children are
            // still walked for nested calls.)
            self.extract_instantiation(node);
        }

        if !skip_children {
            for c in named_kids(node) {
                self.visit_node(c);
            }
        }
    }

    // --- extractors ----------------------------------------------------------

    fn extract_function(&mut self, node: Node<'t>) {
        stack_guard!();
        // Receiver present (out-of-line `Cls::method` def) → method instead.
        if self.variant == Variant::Cpp && self.receiver_type_of(node).is_some() {
            self.extract_method(node);
            return;
        }

        let name = self.extract_name(node);
        if name == "<anonymous>" {
            if let Some(body) = node.child_by_field_name("body") {
                self.visit_for_calls_and_structure(body);
            }
            return;
        }
        // Misparse artifacts: drop the node, still walk the body (#946/#1061).
        if self.is_misparsed_function(&name, node) {
            if let Some(body) = node.child_by_field_name("body") {
                self.visit_for_calls_and_structure(body);
            }
            return;
        }

        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: if self.variant == Variant::Cpp { self.visibility_of(node) } else { None },
            return_type: self.return_type_of(node),
            ..Extra::default()
        };
        let Some(row) = self.create_node("function", &name, node, extra) else { return };
        // (extractTypeAnnotations + extractDecoratorsFor are structural no-ops
        // for c/cpp: not in TYPE_ANNOTATION_LANGUAGES, and the decorator node
        // kinds never appear as direct children/preceding siblings in these
        // grammars — `attribute` only occurs under attribute_declaration.)
        self.stack.push(Scope { row, kind: "function", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    fn extract_method(&mut self, node: Node<'t>) {
        stack_guard!();
        let receiver_type = if self.variant == Variant::Cpp { self.receiver_type_of(node) } else { None };

        if !self.inside_class_like() && receiver_type.is_none() {
            // (object-literal parents don't occur in c/cpp) — treat as function.
            self.extract_function(node);
            return;
        }

        let name = self.extract_name(node);
        if self.is_misparsed_function(&name, node) {
            if let Some(body) = node.child_by_field_name("body") {
                self.visit_for_calls_and_structure(body);
            }
            return;
        }

        let extra = Extra {
            docstring: preceding_docstring(node, self.src),
            visibility: if self.variant == Variant::Cpp { self.visibility_of(node) } else { None },
            is_abstract: if self.variant == Variant::Cpp && self.is_cpp_pure_virtual_method_decl(node) {
                Some(true)
            } else {
                None
            },
            return_type: self.return_type_of(node),
            qualified_name: receiver_type
                .as_ref()
                .map(|r| self.compose_receiver_qualified_name(r, &name)),
            ..Extra::default() // extractMethod passes no isExported
        };
        let Some(row) = self.create_node("method", &name, node, extra) else { return };

        // Out-of-line def: contains edge from the FIRST earlier-in-file
        // struct/class/enum/trait node of the receiver's name.
        if let Some(receiver_type) = &receiver_type {
            if !self.inside_class_like() {
                let owner_row = self
                    .nodes_meta
                    .iter()
                    .position(|m| {
                        m.name == *receiver_type
                            && matches!(m.kind, "struct" | "union" | "class" | "enum" | "trait")
                    })
                    .map(|i| i as u32);
                if let Some(owner_row) = owner_row {
                    self.tables.push_contains(owner_row, row);
                }
            }
        }

        self.stack.push(Scope { row, kind: "method", name });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_for_calls_and_structure(body);
        }
        self.stack.pop();
    }

    // --- callable fields (fn-pointer members → `field` nodes) ----------------
    //
    // A struct/union member that can be CALLED — `ops->read(...)` — gets a
    // `field` node qualified `Owner::name`, giving the resolver's member-call
    // arm a target for the ~12k `recv->fn(args)` refs a kernel-scale corpus
    // leaves dangling. Only three declarator shapes qualify:
    //   1. `int (*read)(int);`            — direct fn-pointer declarator
    //   2. `hook_fn read;`                — member of a `typedef int
    //      (*hook_fn)(int)` known in this file
    //   3. `cb_t *cbp;`                   — pointer to a `typedef void
    //      cb_t(void)` function type
    // Scalar, array-of-non-fnptr, and plain members mint nothing — Linux has
    // millions of those, and an edge can only be as good as the call site.

    // --- bindings (resolution-binding-model-plan.md, Phase 3: C/C++) --------------------

    enclosing_scope_impl!("file");

    push_binding_row_impl!();

    // --- calls / instantiation ----------------------------------------------

    // --- function bodies -----------------------------------------------------


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
        if kind == "declaration" {
            self.emit_local_rows(node);
        }
        // A typedef inside a function body still registers fn-pointer names —
        // a local struct declared after it may use them (`register_fn_typedefs`
        // otherwise runs only from visit_node's top-level arm).
        if kind == "type_definition"
            || (self.variant == Variant::Cpp && kind == "alias_declaration")
        {
            self.register_fn_typedefs(node);
        }

        // C++ stack construction `Calculator calc(0)` / `Widget w{1,2}` (#1035).
        if kind == "declaration"
            && self.variant == Variant::Cpp
            && self.is_cpp_stack_construction(node)
        {
            self.extract_instantiation(node);
        }

        // C++ local fn-pointer bindings: declarations and branch reassignments.
        if self.variant == Variant::Cpp {
            if kind == "declaration" {
                for i in 0..node.named_child_count() {
                    let Some(child) = node.named_child(i) else { continue };
                    if child.kind() != "init_declarator" {
                        continue;
                    }
                    let Some(decl) = child.child_by_field_name("declarator") else { continue };
                    if decl.kind() != "identifier" {
                        continue;
                    }
                    let local = self.text(decl).to_string();
                    self.record_cpp_fn_ptr_binding(&local, child.child_by_field_name("value"));
                }
            } else if kind == "assignment_expression" {
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "identifier" {
                        let local = self.text(left).to_string();
                        self.record_cpp_fn_ptr_binding(&local, node.child_by_field_name("right"));
                    }
                }
            }
        }

        // Static-member / value-read: `Foo.BAR`, `Foo->x` (cpp).
        self.extract_static_member_ref(node);

        // Nested NAMED functions become their own nodes.
        if kind == "function_definition" {
            let nested_name = self.extract_name(node);
            if !nested_name.is_empty() && nested_name != "<anonymous>" {
                self.extract_function(node);
                return;
            }
        }

        // Structural nodes inside bodies (local classes; macro-misparse rescue).
        if self.variant == Variant::Cpp && kind == "class_specifier" {
            self.extract_class(node);
            return;
        }
        if kind == "struct_specifier" {
            self.extract_aggregate(node, "struct");
            return;
        }
        if kind == "union_specifier" {
            self.extract_aggregate(node, "union");
            return;
        }
        if kind == "enum_specifier" {
            self.extract_enum(node);
            return;
        }

        for c in named_kids(node) {
            self.visit_for_calls_and_structure(c);
        }
    }

    // --- inheritance ---------------------------------------------------------

    // --- fn-ref capture (#756, cFamilySpec) ----------------------------------

    // --- value refs (C only: VALUE_REF_LANGS has 'c', not 'cpp') -------------

}

// --- free helpers ------------------------------------------------------------

/// findDeclaratorQualifiedId (languages/c-cpp.ts:13): BFS for the declarator's
/// `qualified_identifier`, skipping parameter_list + trailing_return_type so a
/// qualified PARAMETER type can't be mistaken for the method name.
fn find_declarator_qualified_id(declarator: Node) -> Option<Node> {
    let mut queue: VecDeque<Node> = VecDeque::new();
    queue.push_back(declarator);
    while let Some(current) = queue.pop_front() {
        if current.kind() == "qualified_identifier" {
            return Some(current);
        }
        for child in named_kids(current) {
            if child.kind() != "parameter_list" && child.kind() != "trailing_return_type" {
                queue.push_back(child);
            }
        }
    }
    None
}

/// cDeclaratorIdentifier (tree-sitter.ts:234): resolve the declared identifier
/// through init/pointer/array/parenthesized declarator wrappers; a
/// function_declarator means prototype/fn-ptr — null. (The C grammar's
/// parenthesized_declarator exposes no `declarator` field, so that arm always
/// terminates — bug-for-bug with getChildByField returning null there.)
fn c_declarator_identifier(node: Node) -> Option<Node> {
    let mut cur = Some(node);
    let mut guard = 0;
    while let Some(n) = cur {
        guard += 1;
        if guard > 12 {
            return None;
        }
        match n.kind() {
            "identifier" => return Some(n),
            "function_declarator" => return None,
            "init_declarator" | "pointer_declarator" | "array_declarator"
            | "parenthesized_declarator" => {
                cur = n.child_by_field_name("declarator");
            }
            _ => return None,
        }
    }
    None
}

/// isMacroMisparsedTypeDecl (languages/c-cpp.ts:261): `class MACRO Name {…}`
/// misparse residue — bodyless class/struct specifier in `type` + a
/// non-function_declarator declarator.
fn is_macro_misparsed_type_decl(node: Node) -> bool {
    let Some(type_node) = node.child_by_field_name("type") else { return false };
    if type_node.kind() != "class_specifier" && type_node.kind() != "struct_specifier" {
        return false;
    }
    let has_body = named_kids(type_node)
        .any(|c| c.kind() == "field_declaration_list");
    if has_body {
        return false;
    }
    if let Some(declarator) = node.child_by_field_name("declarator") {
        if declarator.kind() == "function_declarator" {
            return false;
        }
    }
    true
}

/// hasFunctionAncestor (tree-sitter.ts:295).
fn has_function_ancestor(node: Node) -> bool {
    let mut p = node.parent();
    while let Some(n) = p {
        if n.kind() == "function_definition" {
            return true;
        }
        p = n.parent();
    }
    false
}


/// The header's basename without its extension — the resolver's local name
/// for an include (`#include "utils/helpers.hpp"` → `helpers`).
fn include_local_name(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stem = match base.rsplit_once('.') {
        Some((stem, "h" | "hpp" | "hxx" | "hh" | "inl" | "ipp" | "cxx" | "cc" | "cpp")) => stem,
        _ => base,
    };
    if stem.is_empty() { path.to_string() } else { stem.to_string() }
}

/// The identifier a (possibly wrapped) declarator names.
fn declarator_identifier<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut cur = node;
    loop {
        match cur.kind() {
            "identifier" | "field_identifier" | "type_identifier" => return Some(cur),
            "init_declarator" | "pointer_declarator" | "reference_declarator" | "array_declarator" | "parenthesized_declarator" | "attributed_declarator" => {
                cur = cur.child_by_field_name("declarator").or_else(|| cur.named_child(0))?;
            }
            // A function_declarator is a prototype or a function pointer: as the
            // walk's cDeclaratorIdentifier, it names nothing.
            "function_declarator" => return None,
            _ => return None,
        }
    }
}
