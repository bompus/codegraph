//! C / C++ extraction — a faithful Rust port of `TreeSitterExtractor`'s c/cpp
//! paths (src/extraction/tree-sitter.ts) plus languages/c-cpp.ts, one dual-
//! language module flagged like tsjs/ (checklist:
//! docs/design/ccpp-kernel-port-checklist.md — read it before editing).
//!
//! The seven preParse blanking passes are NOT here: the TS route point
//! (src/extraction/kernel/index.ts) applies `extractor.preParse` before the
//! kernel call, so this walker receives the SAME blanked bytes the generic
//! extractor parses (all blanks are equal-length-space replacements — every
//! offset survives). `.metal`/`.cu`/`.cuh` arrive as language 'cpp' with their
//! dialect blanks already applied.
//!
//! Quirks mirrored bug-for-bug (each pinned by the parity gates):
//!  - cpp namespace prefix stack (#1291): named `namespace a::b {` pushes the
//!    name AS WRITTEN onto the qualifiedName prefix; anonymous falls through.
//!    No namespace NODE is minted (#1093 crowd-out).
//!  - cpp brace scopes (brace_scopes.rs): when the tree has errors, each node
//!    is walked in the namespaces, and at declaration level the classes, the
//!    source's braces put it in, not the tree's (error recovery closes scopes
//!    at the wrong `}`). Files with parse errors use this recovery path.
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
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
use tree_sitter::Node;

mod brace_scopes;
use brace_scopes::{BraceScopes, NestedIntervals};


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
fn single_arg_macro_replacement_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(
        r"^(?:[A-Za-z_][A-Za-z0-9_:]*[ \t\r\n]+)+[*& \t\r\n]*([A-Za-z_][A-Za-z0-9_]*)[ \t\r\n]*\([^(){};#]*\)[ \t\r\n]*$"
    ).unwrap())
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
    /// End line and column when the node is not one syntax node (a function
    /// rebuilt from the pieces of a file-level ERROR).
    end: Option<(u32, u32)>,
    /// `static` when the node cannot tell (see `end`).
    is_static: Option<bool>,
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
    node_id_allocator: ids::NodeIdAllocator,
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
    /// Each class body's access specifiers as (start byte, visibility), by
    /// body node id: members look up the nearest preceding one.
    access_specifiers: RefCell<HashMap<usize, Vec<(usize, u8)>>>,
    /// A cpp file whose tree has errors is walked in the scopes its braces
    /// open (visit_in_brace_scope; only when the parsed tree has errors): the
    /// scan, and the class-like nodes extracted so far by their bodies' braces.
    brace_scopes: Option<BraceScopes>,
    class_scopes: NestedIntervals<Scope>,
    class_scope_rows: HashSet<u32>,
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
    if variant == Variant::Cpp && tree.root_node().has_error() {
        w.brace_scopes = brace_scopes::scan(source);
    }

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
            node_id_allocator: ids::NodeIdAllocator::default(),
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
            access_specifiers: RefCell::new(HashMap::new()),
            brace_scopes: None,
            class_scopes: NestedIntervals::new(),
            class_scope_rows: HashSet::new(),
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
        let column = self.col_of(node);
        let id = self.node_id_allocator.generate(self.file_path, kind, name, start_line, column);
        // (c/cpp define no resolveBody hook, so createNode's endLine extension
        // for sibling-body grammars never fires — endLine is the node's own.)
        let (end_line, end_column) = extra.end.unwrap_or((node.end_position().row as u32 + 1, self.end_col_of(node)));

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
            end_column,
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
        self.emit_decl_binding(kind, name, row, node, extra.is_static);
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

    /// cppExtractor.getVisibility: the nearest `public:`, `private:` or
    /// `protected:` before the node among its parent's children. `None` when
    /// no specifier precedes it; the class or struct default is not applied.
    fn visibility_of(&self, node: Node) -> Option<u8> {
        let parent = node.parent()?;
        let mut cache = self.access_specifiers.borrow_mut();
        let specifiers = cache.entry(parent.id()).or_insert_with(|| {
            let mut out = Vec::new();
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() != "access_specifier" {
                    continue;
                }
                let text = self.text(child);
                let visibility = if text.contains("public") {
                    1
                } else if text.contains("private") {
                    2
                } else if text.contains("protected") {
                    3
                } else {
                    continue;
                };
                out.push((child.start_byte(), visibility));
            }
            out
        });
        let before = specifiers.partition_point(|&(at, _)| at < node.start_byte());
        before.checked_sub(1).map(|k| specifiers[k].1)
    }


    // --- visitNode -----------------------------------------------------------

    fn visit_node(&mut self, node: Node<'t>) {
        stack_guard!();
        if self.brace_scopes.is_some() {
            self.visit_in_brace_scope(node);
        } else {
            self.dispatch_node(node);
        }
    }

    /// visitInCppBraceScope: walk `node` in the namespaces and classes its
    /// source braces put it in (see brace_scopes.rs). The namespaces apply
    /// everywhere; the enclosing classes only at declaration level, where the
    /// stack above the file node holds nothing but class scopes.
    fn visit_in_brace_scope(&mut self, node: Node<'t>) {
        stack_guard!();
        let at = node.start_byte();
        let namespaces = self.brace_scopes.as_ref().map(|s| s.namespaces_at(at)).unwrap_or_default();
        let saved = std::mem::replace(&mut self.namespace_prefix, namespaces);
        let mut base = self.stack.len();
        while base > 1 && self.class_scope_rows.contains(&self.stack[base - 1].row) {
            base -= 1;
        }
        let mut walked: Option<Vec<Scope>> = None;
        if base == 1 {
            let classes = self.class_scopes.at(at);
            let same = classes.len() == self.stack.len() - base
                && classes.iter().zip(&self.stack[base..]).all(|(c, s)| c.row == s.row);
            if !same {
                walked = Some(self.stack.split_off(base));
                self.stack.extend(classes);
            }
        }
        self.dispatch_node(node);
        if let Some(walked) = walked {
            self.stack.truncate(base);
            self.stack.extend(walked);
        }
        self.namespace_prefix = saved;
    }

    /// cppBodyEnd: where a class-like node ends in brace scopes — the `}`
    /// that closes its body — or None to keep the tree's.
    fn brace_body_end(&self, body: Node<'t>) -> Option<(u32, u32)> {
        let close = self.brace_scopes.as_ref()?.close_of(body.start_byte())?;
        Some(self.cols.position(self.src, close + 1))
    }

    /// openCppClassScope: a class-like node's body as a scope for visit_in_brace_scope.
    fn open_class_scope(&mut self, scope: &Scope, body: Node<'t>) {
        let Some(close) = self.brace_scopes.as_ref().and_then(|s| s.close_of(body.start_byte())) else { return };
        if self.class_scopes.add(body.start_byte(), close, scope.clone()) {
            self.class_scope_rows.insert(scope.row);
        }
    }

    fn dispatch_node(&mut self, node: Node<'t>) {
        stack_guard!();
        let kind = node.kind();
        let mut skip_children = false;

        // C/C++ function-like macros become `constant` nodes carrying the
        // directive as their signature — a value, never a callee (#1838).
        // Mirrors tree-sitter.ts visitNode.
        if kind == "preproc_function_def" {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = self.text(name_node).to_string();
                let signature = Some(self.text(node).trim().to_string());
                self.create_node("constant", &name, node, Extra { signature, ..Extra::default() });
            }
            return;
        }

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

        if self.is_cpp_constructor_declaration(node) {
            self.extract_method(node);
            skip_children = true;
        } else if kind == "function_definition" {
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
        } else if self.is_attribute_prototype_part(node) {
            skip_children = true;
        } else if kind == "declaration" && !self.inside_class_like() {
            // In brace scopes, a class the tree reads as a declaration's type
            // (glued to the tokens after it by error recovery) is still a class.
            if self.brace_scopes.is_some() {
                if let Some(t) = node.child_by_field_name("type") {
                    let class_like = matches!(
                        t.kind(),
                        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
                    );
                    if class_like && t.child_by_field_name("body").is_some() {
                        self.visit_node(t);
                    }
                }
            }
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
        } else if kind == "ERROR" && self.variant == Variant::C && self.enclosing_scope().is_none() {
            self.visit_file_level_error(node);
            skip_children = true;
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
            signature: self.constructor_signature(node),
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
            signature: self.constructor_signature(node),
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
        // A function-like macro defined inside a body is still a macro (#1838).
        if kind == "preproc_function_def" {
            self.visit_node(node);
            return;
        }
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

        // C++ stack construction `Calculator calc(0)` / `Widget w{1,2}` (#1035),
        // plus one constructor ref `ns::T::T/arity` per constructed object (#1839).
        if kind == "declaration" && self.variant == Variant::Cpp {
            let (instantiates, arities) = self.cpp_stack_constructions(node);
            if instantiates {
                self.extract_instantiation(node);
            }
            if !arities.is_empty() && !self.stack.is_empty() {
                if let Some(type_node) = node.child_by_field_name("type") {
                    let from = self.top_row();
                    let class_name = strip_cpp_template_args(self.text(type_node));
                    if let Some(name) = class_name.split("::").filter(|s| !s.is_empty()).last() {
                        let calls = crate::buffers::EDGE_CALLS;
                        for arity in arities {
                            self.push_ref_at(from, &format!("{class_name}::{name}/{arity}"), calls, node);
                        }
                    }
                }
            }
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

        // Nested NAMED functions become their own nodes. A real one (GCC nested
        // function) declares through a function_declarator under a real type.
        // A macro that supplies a condition's parentheses misparses instead:
        // `if mi_likely(x) {` as type `mi_likely`, declarator `(x)`, and
        // `else if mi_unlikely(x) {` as type `else`, declarator `mi_unlikely(x)`,
        // and a statement macro without a semicolon (`Py_END_ALLOW_THREADS`)
        // before `if (x) {` as type `Py_END_ALLOW_THREADS`, declarator `if(x)`,
        // or, with a blank line in between, declarator `Py_END_ALLOW_THREADS(x)`
        // holding `if` as an ERROR. All stay part of the enclosing body.
        if kind == "function_definition"
            && declares_function(node)
            && !node.child_by_field_name("type").is_some_and(|t| is_statement_keyword(self.text(t)))
            && !self.declarator_swallows_keyword(node)
        {
            let nested_name = self.extract_name(node);
            if !nested_name.is_empty()
                && nested_name != "<anonymous>"
                && !is_statement_keyword(&nested_name)
            {
                if self.is_swallowed_file_level_definition(node) {
                    self.extract_outside_enclosing_function(node);
                } else {
                    self.extract_function(node);
                }
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

    /// `Py_END_ALLOW_THREADS` then a blank line then `if (err) {` parses as a
    /// definition whose function_declarator holds the `if` as an ERROR child.
    fn declarator_swallows_keyword(&self, node: Node) -> bool {
        function_declarator_of(node).is_some_and(|d| {
            named_kids(d).any(|c| c.kind() == "ERROR" && is_statement_keyword(self.text(c).trim()))
        })
    }

    /// A definition inside a function body that is really a file-level one: an
    /// `#ifdef` / `#else` pair that each open a brace for one `}` leaves the body
    /// unclosed, and every later function in the file parses inside it. GCC
    /// rejects a `static` nested function, and a real one is indented.
    fn is_swallowed_file_level_definition(&self, node: Node) -> bool {
        node.start_position().column == 0
            || named_kids(node).any(|c| c.kind() == "storage_class_specifier" && self.text(c) == "static")
    }

    /// Extract with the enclosing function scopes cut off, so the node's parent
    /// and qualified name are what they would be at file level.
    fn extract_outside_enclosing_function(&mut self, node: Node<'t>) {
        let depth = self.stack.iter().position(|s| matches!(s.kind, "function" | "method")).unwrap_or(self.stack.len());
        let enclosing = self.stack.split_off(depth);
        self.extract_function(node);
        self.stack.extend(enclosing);
    }

    /// An `#ifdef` whose branches each open a brace for one `}` can leave a
    /// file-level region unparsed. Tree-sitter returns an ERROR holding a
    /// definition's specifiers, its function_declarator and the `{`, with the
    /// body's statements as loose siblings; rebuild the function from them so
    /// it and its calls are not lost.
    fn visit_file_level_error(&mut self, node: Node<'t>) {
        let kids: Vec<Node<'t>> = {
            let mut c = node.walk();
            node.children(&mut c).collect()
        };
        let mut i = 0;
        while i < kids.len() {
            if let Some(next) = self.extract_swallowed_function(&kids, i) {
                i = next;
                continue;
            }
            if kids[i].is_named() {
                self.visit_node(kids[i]);
            }
            i += 1;
        }
    }

    /// When `kids[d]` declares a function whose `{` follows, extract it and
    /// return the index after its body. The body ends at the first `}` in
    /// column 0 outside a multi-line macro, at an indented `}` the next
    /// column-0 item follows (netlib style), or earlier where a column-0
    /// non-directive starts the next file-level item.
    /// The source text decides the `}` because a statement the parser left
    /// open can hold the rest of the file.
    fn extract_swallowed_function(&mut self, kids: &[Node<'t>], d: usize) -> Option<usize> {
        let decl = kids[d];
        let mut fd = decl;
        while fd.kind() == "pointer_declarator" {
            fd = fd.child_by_field_name("declarator")?;
        }
        if fd.kind() != "function_declarator" || kids.get(d + 1)?.kind() != "{" {
            return None;
        }
        let name_node = fd.child_by_field_name("declarator").filter(|n| n.kind() == "identifier")?;
        let name = self.text(name_node).to_string();
        let mut s = d;
        while s > 0 && is_c_specifier(kids[s - 1].kind()) {
            s -= 1;
        }
        // A macro loop inside a body (`list_for_each(p, head) {`) has the same
        // shape, but is indented and has no return type.
        if kids[s].start_position().column != 0 || is_statement_keyword(&name) {
            return None;
        }
        let open = kids[d + 1].end_byte();
        let close = self.src[open..]
            .match_indices("\n}")
            .map(|(o, _)| open + o)
            .find(|&nl| !self.src[..nl].trim_end_matches('\r').ends_with('\\'))
            .map(|nl| nl + 1);
        let close = match (close, indented_close_before_next_item(self.src, open)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let mut last = d + 1;
        let mut j = d + 2;
        while j < kids.len() {
            let k = kids[j];
            if close.is_some_and(|c| k.start_byte() > c)
                || (k.is_named() && k.start_position().column == 0 && !k.kind().starts_with("preproc"))
            {
                break;
            }
            last = j;
            j += 1;
            if !k.is_named() && k.kind() == "}" && k.start_position().column == 0 {
                break;
            }
        }
        let start = kids[s];
        let mut end = (kids[last].end_position().row as u32 + 1, self.end_col_of(kids[last]));
        if let Some(c) = close.filter(|c| *c < kids[last].end_byte()) {
            end = (self.src[..c].matches('\n').count() as u32 + 1, 1);
        }
        let specs = &kids[s..d];
        let extra = Extra {
            docstring: preceding_docstring(start, self.src),
            return_type: specs
                .iter()
                .find(|k| matches!(k.kind(), "primitive_type" | "type_identifier" | "sized_type_specifier"))
                .and_then(|k| normalize_cpp_return_type(self.text(*k))),
            end: Some(end),
            is_static: Some(specs.iter().any(|k| k.kind() == "storage_class_specifier" && self.text(*k) == "static")),
            ..Extra::default()
        };
        let row = self.create_node("function", &name, start, extra)?;
        self.emit_declarator_param_bindings(fd, (self.line_of(start), end.0));
        self.stack.push(Scope { row, kind: "function", name: name.clone() });
        for k in &kids[d + 2..=last.max(d + 1)] {
            if k.is_named() {
                // create_node scoped only the first specifier; the body is these siblings.
                self.value_scopes.push(ValueScope { row, node: *k, name: name.clone() });
                self.visit_for_calls_and_structure(*k);
            }
        }
        self.stack.pop();
        Some(j)
    }

    // --- inheritance ---------------------------------------------------------

    // --- fn-ref capture (#756, cFamilySpec) ----------------------------------

    // --- value refs (C only: VALUE_REF_LANGS has 'c', not 'cpp') -------------

}

// --- free helpers ------------------------------------------------------------

/// Netlib-style C closes a body with an indented `}`, so there is no column-0
/// `}` to find. Returns the byte of the last `}`-only line before the next
/// column-0 item, when only directives, comments, blank lines and bare
/// specifier words (` static int`) sit between the two. A column-0 label or a
/// macro continuation line does not start an item.
fn indented_close_before_next_item(src: &str, open: usize) -> Option<usize> {
    let mut brace: Option<usize> = None;
    let mut in_comment = false;
    let mut continued = false;
    let mut at = open + src[open..].find('\n')? + 1;
    while at < src.len() {
        let end = src[at..].find('\n').map_or(src.len(), |o| at + o);
        let line = src[at..end].trim_end_matches('\r');
        let was_continued = continued;
        continued = line.ends_with('\\');
        let trimmed = line.trim();
        if in_comment {
            in_comment = !trimmed.contains("*/");
        } else if was_continued || trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
        } else if trimmed.starts_with("/*") {
            in_comment = !trimmed.contains("*/");
        } else if trimmed == "}" {
            brace = Some(at + line.find('}')?);
        } else if brace.is_some()
            && trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '*' || c.is_whitespace())
            && !line.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        {
        } else if brace.is_some() && line.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            let word_end = line.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(line.len());
            let rest = line[word_end..].trim_start();
            if !(rest.starts_with(':') && !rest.starts_with("::")) {
                return brace;
            }
            brace = None;
        } else {
            brace = None;
        }
        at = end + 1;
    }
    None
}

/// A definition whose declarator, under pointer / reference / attribute
/// wrappers, is a function_declarator.
fn declares_function(node: Node) -> bool {
    function_declarator_of(node).is_some()
}

fn function_declarator_of(node: Node) -> Option<Node> {
    let mut decl = node.child_by_field_name("declarator");
    while let Some(d) = decl {
        match d.kind() {
            "function_declarator" => return Some(d),
            "pointer_declarator" | "reference_declarator" | "attributed_declarator" => {
                decl = d.child_by_field_name("declarator").or_else(|| d.named_child(0));
            }
            _ => return None,
        }
    }
    None
}

/// A declaration specifier that can precede a function's declarator.
fn is_c_specifier(kind: &str) -> bool {
    matches!(
        kind,
        "storage_class_specifier" | "type_qualifier" | "primitive_type" | "type_identifier" | "sized_type_specifier"
            | "struct_specifier" | "union_specifier" | "enum_specifier" | "attribute_specifier" | "ms_call_modifier"
    )
}

/// A statement keyword the parser took for a definition's type (`catch`
/// from JavaScript in an EM_JS body).
fn is_statement_keyword(text: &str) -> bool {
    matches!(text, "if" | "else" | "while" | "for" | "do" | "switch" | "return" | "catch")
}

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
