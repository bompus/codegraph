//! codegraph-kernel — native extraction kernel (napi-rs).
//!
//! Replaces ONLY the parse+extract walk inside the parse workers, behind the
//! existing `ExtractionResult` contract. Input `(filePath, content, language)`
//! per file; output flat typed buffers — one boundary crossing per file.
//! Everything downstream (resolution, synthesis, frameworks, MCP) is
//! untouched and consumes the decoded result exactly as before.
//!
//! Calls are synchronous by design: the existing `ParseWorkerPool` workers
//! already parallelize per-file, so each worker thread drives its own kernel
//! call (do NOT rebuild the pool on the Rust side — see the migration plan §3).
//!
//! Per-language extraction lives in a dedicated walker module (tsjs/ for
//! typescript/tsx/javascript/jsx) that mirrors the TS extractor for behavioral
//! parity — verified by scripts/kernel-parity.mjs and the §5 gate.

#![deny(clippy::all)]

/// First statement of every recursive walker function (see stack.rs, #1581):
/// once the stack pointer is inside the red zone, stop descending — the
/// latched flag makes `stack::run_guarded` discard the walk and defer the
/// file to wasm. `Default::default()` covers every walker return type in use
/// (`()`, `bool`, `Option<_>`, `String`); a hook returning `false` just sends
/// its caller down the generic child walk, whose own guard returns at once.
macro_rules! stack_guard {
    () => {
        if $crate::stack::exhausted() {
            return ::core::default::Default::default();
        }
    };
}

/// The position helpers every walker shares: source text of a node, its
/// 1-based line, its UTF-16 start/end columns (via the walker's `cols`), and
/// the row of the innermost scope (0 = the file node). Expects `src: &'t str`,
/// `cols: textutil::Cols` and `stack: Vec<Scope>` with a `row: u32`.
macro_rules! walker_pos_impl {
    () => {
        fn text(&self, node: Node) -> &'t str {
            &self.src[node.byte_range()]
        }
        fn line_of(&self, node: Node) -> u32 {
            node.start_position().row as u32 + 1
        }
        fn col_of(&self, node: Node) -> u32 {
            self.cols.col(self.src, node.start_position().row, node.start_byte())
        }
        fn end_col_of(&self, node: Node) -> u32 {
            self.cols.col(self.src, node.end_position().row, node.end_byte())
        }
        fn top_row(&self) -> u32 {
            self.stack.last().map(|s| s.row).unwrap_or(0)
        }
    };
}

/// isInsideClassLikeNode — the innermost scope only (the file never
/// counts), over the walker's class-like kinds (C/C++ and Rust add `union`).
macro_rules! inside_class_like_impl {
    ($($kind:literal)|+) => {
        fn inside_class_like(&self) -> bool {
            self.stack.last().map(|s| matches!(s.kind, $($kind)|+)).unwrap_or(false)
        }
    };
}

/// Emits the function-reference candidates held during the walk, now that
/// the file's defined and imported names are known: a candidate survives
/// when it is `this.`-rooted, `::`-qualified, or names a function this file
/// defines or imports; one ref per (owner node id, name). Expects
/// `fn_ref_cands: Vec<walker::Cand>`, `defined_fn_names`, `imported_names`,
/// `node_ids`, `file_path`, `cols`, `src`, `arena`, `tables`.
macro_rules! flush_fn_ref_candidates_impl {
    () => {
        fn flush_fn_ref_candidates(&mut self) {
            let cands = std::mem::take(&mut self.fn_ref_cands);
            if cands.is_empty() || $crate::textutil::is_generated_file(self.file_path) {
                return;
            }
            // The TS side keys on `${fromNodeId}|${name}`; ids can collide
            // across rows, so the key is the id string, not the row.
            let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
            for c in cands {
                if !c.name.starts_with("this.")
                    && !c.name.contains("::")
                    && !self.defined_fn_names.contains(&c.name)
                    && !self.imported_names.contains(&c.name)
                {
                    continue;
                }
                if !seen.insert((self.node_ids[c.from as usize].clone(), c.name.clone())) {
                    continue;
                }
                let column = self.cols.col(self.src, c.row, c.column_byte);
                let name_ref = self.arena.put(&c.name);
                self.tables.push_ref(&$crate::buffers::RefRow {
                    from_idx: c.from,
                    kind: $crate::buffers::FUNCTION_REF_CODE,
                    line: c.line,
                    column,
                    reference_name: name_ref,
                    candidates: $crate::buffers::NONE_STR,
                    from_id_str: $crate::buffers::NONE_STR,
                });
            }
        }
    };
}

/// One binding row from the walker's arena and tables; an `exported` row is
/// `EXPORT_PUBLIC` under its own name.
macro_rules! push_binding_row_impl {
    () => {
        #[allow(clippy::too_many_arguments)]
        fn push_binding_row(
            &mut self,
            kind: u8,
            name: &str,
            node_idx: u32,
            scope: (u32, u32),
            line: u32,
            target: Option<(&str, &str)>,
            exported: bool,
            storage: Option<&str>,
        ) {
            use $crate::buffers::{BindingRow, EXPORT_NONE, EXPORT_PUBLIC, NONE_STR};
            let name_ref = self.arena.put(name);
            let (target_spec, target_name) = match target {
                Some((spec, imported)) => (self.arena.put(spec), self.arena.put(imported)),
                None => (NONE_STR, NONE_STR),
            };
            let storage_ref = self.arena.put_opt(storage);
            self.tables.push_binding(&BindingRow {
                kind,
                export_form: if exported { EXPORT_PUBLIC } else { EXPORT_NONE },
                node_idx,
                scope_start: scope.0,
                scope_end: scope.1,
                name: name_ref,
                target_spec,
                target_name,
                exported_as: if exported { name_ref } else { NONE_STR },
                storage: storage_ref,
                line,
            });
        }
    };
}

/// The markdown path-reference pair, for a walker with the usual shape
/// (`text`, `line_of`, `col_of`, `arena`, `tables`, `file_path`). Every routed
/// language needs the same two methods, and the wasm arm they must match is
/// one implementation, so this is one implementation too — see markdown.rs for
/// what it mirrors and why the two ref flags are set only here.
macro_rules! markdown_refs_impl {
    () => {
        /// extractMarkdownPathReferencesFromStringNode.
        fn markdown_refs_from_string(&mut self, node: Node<'t>, owner_row: u32) {
            if !$crate::markdown::is_markdown_path_string_kind(node.kind()) {
                return;
            }
            let found = $crate::markdown::markdown_path_refs(self.text(node), self.file_path);
            if found.is_empty() {
                return;
            }
            let kind_code = $crate::buffers::edge_kind_index("references").unwrap();
            let line = self.line_of(node);
            let column = self.col_of(node);
            for (name, offset) in found {
                let column = column + offset as u32;
                // Declaration scans and the general walk can reach the same string.
                if !self.md_ref_keys.insert(format!("{owner_row}|{name}|{line}|{column}")) {
                    continue;
                }
                let name_ref = self.arena.put(&name);
                // addReference denormalizes filePath and language onto the ref
                // where the ordinary ref path does not, so these two flags are
                // set here and nowhere else.
                self.tables.push_ref_flagged(
                    &$crate::buffers::RefRow {
                        from_idx: owner_row,
                        kind: kind_code,
                        line,
                        column,
                        reference_name: name_ref,
                        candidates: $crate::buffers::NONE_STR,
                        from_id_str: $crate::buffers::NONE_STR,
                    },
                    $crate::buffers::REF_FLAG_FILE_PATH | $crate::buffers::REF_FLAG_LANGUAGE,
                );
            }
        }

        /// extractMarkdownPathReferencesFromSubtree — for constructs whose walk
        /// stops before their value, where the owner is the declared symbol
        /// rather than the enclosing scope.
        ///
        /// Only some walkers stop that way (tsjs, python, java, csharp, ruby,
        /// lua); in the rest a declaration's children are walked normally and
        /// the string method above already covers them, so the pair is one
        /// macro and this half goes unused there. The parity fixtures decide
        /// which is which — every language's torture file carries the same
        /// eight markdown shapes.
        #[allow(dead_code)]
        fn markdown_refs_from_subtree(&mut self, node: Node<'t>, owner_row: u32) {
            stack_guard!();
            self.markdown_refs_from_string(node, owner_row);
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    self.markdown_refs_from_subtree(c, owner_row);
                }
            }
        }
    };
}

mod buffers;
mod ccpp;
mod cfnptr;
mod csharp;
mod dart;
mod docstring;
mod ids;
mod go;
mod java;
mod kotlin;
mod langs;
mod lua;
mod markdown;
mod php;
mod resolve;
mod rlang;
mod ruby;
mod rustlang;
mod scala;
mod stack;
mod swift;
mod textutil;
mod tree;
mod walker;
mod python;
mod tsjs;

use napi::bindgen_prelude::*;
use napi_derive::napi;

/// The six flat tables for one file. See buffers.rs for the byte layout;
/// `src/extraction/kernel/layout.ts` is the TS mirror.
#[napi(object)]
pub struct ExtractBuffers {
    pub meta: Buffer,
    pub nodes: Buffer,
    pub edges: Buffer,
    pub refs: Buffer,
    /// v3: per-file bindings (docs/design/resolution-binding-model-plan.md).
    pub bindings: Buffer,
    pub arena: Buffer,
}

/// Wire-contract description — the TS loader verifies this against
/// src/types.ts before routing anything to the kernel, so an out-of-date
/// `.node` degrades to the wasm path instead of mis-decoding.
#[napi(object)]
pub struct ContractInfo {
    pub abi_version: u32,
    pub kernel_version: String,
    pub node_kinds: Vec<String>,
    pub edge_kinds: Vec<String>,
    /// Languages this binary can extract (routing is still TS-side policy).
    pub languages: Vec<String>,
}

/// Grammar identity for the grammar-source-parity gate: the wasm grammar and
/// the native grammar must expose identical node-kind/field tables, or
/// kernel-vs-fallback routing would be non-deterministic.
#[napi(object)]
pub struct GrammarInfo {
    pub abi_version: u32,
    pub node_kind_count: u32,
    pub field_count: u32,
    pub node_kinds: Vec<String>,
    pub field_names: Vec<String>,
}

#[napi]
pub fn contract_info() -> ContractInfo {
    ContractInfo {
        abi_version: buffers::KERNEL_ABI_VERSION as u32,
        kernel_version: env!("CARGO_PKG_VERSION").to_string(),
        node_kinds: buffers::NODE_KINDS.iter().map(|s| s.to_string()).collect(),
        edge_kinds: buffers::EDGE_KINDS.iter().map(|s| s.to_string()).collect(),
        languages: langs::LANGUAGES.iter().map(|s| s.to_string()).collect(),
    }
}

#[napi]
pub fn grammar_info(language: String) -> Option<GrammarInfo> {
    let lang = langs::grammar_for(&language)?;
    let node_kind_count = lang.node_kind_count();
    let field_count = lang.field_count();
    let node_kinds = (0..node_kind_count)
        .map(|i| lang.node_kind_for_id(i as u16).unwrap_or("").to_string())
        .collect();
    // Field ids are 1-based in tree-sitter.
    let field_names = (1..=field_count)
        .map(|i| lang.field_name_for_id(i as u16).unwrap_or("").to_string())
        .collect();
    Some(GrammarInfo {
        abi_version: lang.abi_version() as u32,
        node_kind_count: node_kind_count as u32,
        field_count: field_count as u32,
        node_kinds,
        field_names,
    })
}

/// One struct node's extent for the cFnPtr sweep (mirror of the TS caller's
/// `{ id, startLine, endLine }`, with `endLine ?? startLine` applied TS-side).
#[napi(object)]
pub struct CfnptrStructIn {
    pub id: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[napi(object)]
pub struct CfnptrFileIn {
    /// RAW file text, exactly as the resolver's readFile returned it.
    pub text: String,
    pub structs: Vec<CfnptrStructIn>,
}

#[napi(object)]
pub struct CfnptrPathIn {
    /// Absolute file path — the kernel reads the text itself (utf-8-lossy, the
    /// same bytes JS `readFileSync(path, 'utf-8')` produces) instead of taking
    /// the whole corpus across the boundary.
    pub path: String,
    pub structs: Vec<CfnptrStructIn>,
}

#[napi(object)]
pub struct CfnptrField {
    pub name: String,
    pub index: u32,
    pub ptr: bool,
    #[napi(js_name = "type")]
    pub ty: String,
}

#[napi(object)]
pub struct CfnptrStructOut {
    pub id: String,
    pub parsed: bool,
    pub fields: Vec<CfnptrField>,
}

/// The cFnPtr extraction-sweep facts for one file — see cfnptr.rs (and the
/// TS synthesizer's `FileFacts`) for field semantics.
#[napi(object)]
pub struct CfnptrFacts {
    pub fn_ptr_typedefs: Vec<String>,
    pub fn_type_typedefs: Vec<String>,
    pub structs: Vec<CfnptrStructOut>,
    pub inline_ptr: bool,
    pub inline_types: Vec<String>,
    pub inline_tags: Vec<String>,
    pub init_tokens: Vec<String>,
    pub array_elems: Vec<String>,
    pub alias_names: Vec<String>,
    pub d_pairs: Vec<String>,
    /// Distinct LHS field names of `x->f = fn;` / `(*x)->f = fn;` (the
    /// bare-function-assignment registration filter). Absent on binaries
    /// built before this field existed — the TS side reads it optional.
    pub assign_fields: Vec<String>,
    pub dispatch_fields: Vec<String>,
    pub array_dispatch_names: Vec<String>,
    pub includes: Vec<String>,
}

fn facts_to_out(facts: cfnptr::FileFacts) -> CfnptrFacts {
    CfnptrFacts {
        fn_ptr_typedefs: facts.fn_ptr_typedefs,
        fn_type_typedefs: facts.fn_type_typedefs,
        structs: facts
            .structs
            .into_iter()
            .map(|s| CfnptrStructOut {
                id: s.id,
                parsed: s.parsed,
                fields: s
                    .fields
                    .into_iter()
                    .map(|fl| CfnptrField { name: fl.name, index: fl.index, ptr: fl.ptr, ty: fl.ty })
                    .collect(),
            })
            .collect(),
        inline_ptr: facts.inline_ptr,
        inline_types: facts.inline_types,
        inline_tags: facts.inline_tags,
        init_tokens: facts.init_tokens,
        array_elems: facts.array_elems,
        alias_names: facts.alias_names,
        d_pairs: facts.d_pairs,
        assign_fields: facts.assign_fields,
        dispatch_fields: facts.dispatch_fields,
        array_dispatch_names: facts.array_dispatch_names,
        includes: facts.includes,
    }
}

/// Empty facts — an unreadable file (or a scan panic) merges to nothing, the
/// same as the JS sweep's `if (!rawText) continue`.
fn empty_cfnptr_facts() -> CfnptrFacts {
    CfnptrFacts {
        fn_ptr_typedefs: Vec::new(),
        fn_type_typedefs: Vec::new(),
        structs: Vec::new(),
        inline_ptr: false,
        inline_types: Vec::new(),
        inline_tags: Vec::new(),
        init_tokens: Vec::new(),
        array_elems: Vec::new(),
        alias_names: Vec::new(),
        d_pairs: Vec::new(),
        assign_fields: Vec::new(),
        dispatch_fields: Vec::new(),
        array_dispatch_names: Vec::new(),
        includes: Vec::new(),
    }
}

/// Batched cFnPtr extraction sweep (task #5 step 2): one call scans a batch
/// of files and returns their collected facts, amortizing the NAPI boundary.
/// Feature-detected by the TS loader — absent on older binaries, where the
/// synthesizer keeps its JS sweep.
#[napi]
pub fn cfnptr_scan_files(files: Vec<CfnptrFileIn>) -> Vec<CfnptrFacts> {
    files
        .into_iter()
        .map(|f| {
            let structs: Vec<cfnptr::StructExtent> = f
                .structs
                .into_iter()
                .map(|s| cfnptr::StructExtent { id: s.id, start_line: s.start_line, end_line: s.end_line })
                .collect();
            facts_to_out(cfnptr::scan_file(&f.text, &structs))
        })
        .collect()
}

/// Read a file the way JS `readFileSync(path, 'utf-8')` does: valid UTF-8
/// moves in without a copy; invalid bytes take the lossy path.
fn read_lossy(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// Fan `f` over `items` on scoped threads (≤16, ≤ items), output 1:1 with
/// input order. Per-item work is independent, so the batch scales with
/// cores instead of riding one worker thread. A chunk thread that panics pads
/// its slots with `pad()` so alignment survives; per-item panics are the
/// caller's to catch when it wants finer padding.
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync, pad: impl Fn() -> R) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16)
        .min(items.len().max(1));
    if threads <= 1 {
        return items.iter().map(&f).collect();
    }
    let chunk_len = items.len().div_ceil(threads);
    let chunks: Vec<&[T]> = items.chunks(chunk_len).collect();
    let mut parts: Vec<Vec<R>> = Vec::with_capacity(chunks.len());
    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| s.spawn(|| chunk.iter().map(&f).collect::<Vec<_>>()))
            .collect();
        for (i, h) in handles.into_iter().enumerate() {
            match h.join() {
                Ok(v) => parts.push(v),
                Err(_) => parts.push(chunks[i].iter().map(|_| pad()).collect()),
            }
        }
    });
    parts.into_iter().flatten().collect()
}

/// Path-driven variant of `cfnptr_scan_files`: the kernel reads each file
/// itself and fans the batch across scoped threads. Output stays 1:1 with
/// input order: an unreadable file or a per-file panic yields empty facts,
/// and a chunk thread's own failure pads its outputs the same way.
#[napi]
pub fn cfnptr_scan_paths(files: Vec<CfnptrPathIn>) -> Vec<CfnptrFacts> {
    fn scan_one(f: &CfnptrPathIn) -> CfnptrFacts {
        let Some(text) = read_lossy(&f.path) else {
            return empty_cfnptr_facts();
        };
        let structs: Vec<cfnptr::StructExtent> = f
            .structs
            .iter()
            .map(|s| cfnptr::StructExtent { id: s.id.clone(), start_line: s.start_line, end_line: s.end_line })
            .collect();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cfnptr::scan_file(&text, &structs))) {
            Ok(facts) => facts_to_out(facts),
            Err(_) => empty_cfnptr_facts(),
        }
    }
    par_map(&files, scan_one, empty_cfnptr_facts)
}

/// Debug/differential hook: the native `stripCommentsForRegex(text, 'c')`.
/// Exists so the strip differential oracle can pin the Rust stripper against
/// the TS reference directly.
#[napi]
pub fn cfnptr_strip_c(text: String) -> String {
    String::from_utf8(cfnptr::strip_c(text.as_bytes())).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

// ---- cFnPtr stage C env extraction + stages D/E link ----

#[napi(object)]
pub struct CfnptrFnMacro {
    pub name: String,
    pub params: Vec<String>,
    pub expansion: String,
}

#[napi(object)]
pub struct CfnptrObjMacro {
    pub name: String,
    pub value: String,
}

/// One file's `buildEnv` inputs — see `cfnptr::file_env`.
#[napi(object)]
pub struct CfnptrFileEnv {
    pub fn_macros: Vec<CfnptrFnMacro>,
    pub obj_macros: Vec<CfnptrObjMacro>,
    pub defined: Vec<String>,
    /// Raw `#include "…"` captures — extension filtering and path resolution
    /// stay TS-side (filesystem access).
    pub includes: Vec<String>,
    /// The comment-stripped text — the JS `src(file)` result. The TS side
    /// pushes it into `srcCache` so stage C's `processUnit` doesn't pay a
    /// second read+strip (the old `fileFnMacros`→`src()` warmed that cache).
    pub stripped: String,
}

/// Path-driven per-file env extraction for stage C's `buildEnv`, internally
/// threaded like `cfnptr_scan_paths` — output is index-aligned with input,
/// `null` per unreadable path (the caller then falls back to `ctx.readFile`,
/// so a virtual FS still resolves) and per slot of a chunk thread that
/// panicked. OPTIONAL: absent on older binaries, where the synthesizer keeps
/// its lazy LRU-cached extractor path.
#[napi]
pub fn cfnptr_file_envs(paths: Vec<String>) -> Vec<Option<CfnptrFileEnv>> {
    fn one(path: &str) -> Option<CfnptrFileEnv> {
        let text = read_lossy(path)?;
        let e = cfnptr::file_env(&text);
        Some(CfnptrFileEnv {
            fn_macros: e
                .fn_macros
                .into_iter()
                .map(|m| CfnptrFnMacro { name: m.name, params: m.params, expansion: m.expansion })
                .collect(),
            obj_macros: e
                .obj_macros
                .into_iter()
                .map(|(name, value)| CfnptrObjMacro { name, value })
                .collect(),
            defined: e.defined,
            includes: e.includes,
            stripped: e.stripped,
        })
    }
    par_map(&paths, |p| one(p), || None)
}

/// `{name, type, isFnPtr}` — the FieldInfo members stages D/E consult; `type`
/// arrives null for fn-pointer fields (the TS side stores `''` there).
#[napi(object)]
pub struct CfnptrLinkField {
    pub name: String,
    #[napi(js_name = "type")]
    pub ty: Option<String>,
    pub is_fn_ptr: bool,
}

#[napi(object)]
pub struct CfnptrLinkFn {
    pub id: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[napi(object)]
pub struct CfnptrLinkFile {
    /// Project-relative path (the node filePath — `registeredAt` uses it).
    pub rel: String,
    /// Absolute path the kernel reads.
    pub abs: String,
    /// Stage-D survivor flag (facts pre-gate).
    pub prop: bool,
    /// Stage-E survivor flag.
    pub dispatch: bool,
    /// The file's function/method extents, in `getNodesInFile` order.
    pub fns: Vec<CfnptrLinkFn>,
}

#[napi(object)]
pub struct CfnptrFieldStructs {
    pub field: String,
    pub structs: Vec<String>,
}

#[napi(object)]
pub struct CfnptrLayout {
    pub name: String,
    pub fields: Vec<CfnptrLinkField>,
}

#[napi(object)]
pub struct CfnptrLayoutVariants {
    pub name: String,
    pub variants: Vec<Vec<CfnptrLinkField>>,
}

#[napi(object)]
pub struct CfnptrVarType {
    pub var: String,
    #[napi(js_name = "type")]
    pub ty: String,
}

#[napi(object)]
pub struct CfnptrRegEntry {
    pub key: String,
    pub ids: Vec<String>,
}

#[napi(object)]
pub struct CfnptrArrEntry {
    pub file: String,
    pub ids: Vec<String>,
}

#[napi(object)]
pub struct CfnptrArrReg {
    pub name: String,
    pub entries: Vec<CfnptrArrEntry>,
}

/// The registration tables verbatim — see `cfnptr::LinkTables`.
#[napi(object)]
pub struct CfnptrLinkTables {
    pub field_to_structs: Vec<CfnptrFieldStructs>,
    pub struct_layout: Vec<CfnptrLayout>,
    pub all_struct_fields: Vec<CfnptrLayoutVariants>,
    pub global_var_type: Vec<CfnptrVarType>,
    pub reg: Vec<CfnptrRegEntry>,
    pub array_reg: Vec<CfnptrArrReg>,
}

#[napi(object)]
pub struct CfnptrLinkEdge {
    pub source: String,
    pub target: String,
    pub line: u32,
    pub via: String,
    pub registered_at: String,
}

#[napi(object)]
pub struct CfnptrLinkOut {
    pub edges: Vec<CfnptrLinkEdge>,
}

/// Stages D+E of the fn-pointer synthesis — field←field propagation to a
/// fixpoint, then dispatch-site edges — internally threaded, file-order
/// deterministic. OPTIONAL: absent on older binaries.
#[napi]
pub fn cfnptr_link(files: Vec<CfnptrLinkFile>, tables: CfnptrLinkTables) -> CfnptrLinkOut {
    let files: Vec<cfnptr::LinkFile> = files
        .into_iter()
        .map(|f| cfnptr::LinkFile {
            rel: f.rel,
            abs: f.abs,
            prop: f.prop,
            dispatch: f.dispatch,
            fns: f
                .fns
                .into_iter()
                .map(|n| cfnptr::LinkFn {
                    id: n.id,
                    start_line: n.start_line as i64,
                    end_line: n.end_line as i64,
                })
                .collect(),
        })
        .collect();
    let tables = cfnptr::LinkTables {
        field_to_structs: tables
            .field_to_structs
            .into_iter()
            .map(|e| (e.field, e.structs))
            .collect(),
        struct_layout: tables
            .struct_layout
            .into_iter()
            .map(|e| (e.name, e.fields.into_iter().map(field_in).collect()))
            .collect(),
        all_struct_fields: tables
            .all_struct_fields
            .into_iter()
            .map(|e| {
                (
                    e.name,
                    e.variants
                        .into_iter()
                        .map(|v| v.into_iter().map(field_in).collect())
                        .collect(),
                )
            })
            .collect(),
        global_var_type: tables.global_var_type.into_iter().map(|e| (e.var, e.ty)).collect(),
        reg: tables.reg.into_iter().map(|e| (e.key, e.ids)).collect(),
        array_reg: tables
            .array_reg
            .into_iter()
            .map(|e| {
                (
                    e.name,
                    e.entries
                        .into_iter()
                        .map(|en| cfnptr::LinkArrEntry { file: en.file, ids: en.ids })
                        .collect(),
                )
            })
            .collect(),
    };
    CfnptrLinkOut {
        edges: cfnptr::cfnptr_link(&files, &tables)
            .into_iter()
            .map(|e| CfnptrLinkEdge {
                source: e.source,
                target: e.target,
                line: e.line as u32,
                via: e.via,
                registered_at: e.registered_at,
            })
            .collect(),
    }
}

fn field_in(f: CfnptrLinkField) -> cfnptr::LinkField {
    cfnptr::LinkField { name: f.name, ftype: f.ty, is_fn_ptr: f.is_fn_ptr }
}

/// The per-language walk, under the stack guard (stack.rs, #1581): a file
/// nested deeply enough to overflow this thread's stack comes back as a
/// `defer:` error — the TS side's routine "serve this one file another way"
/// signal — instead of a SIGSEGV that kills the entire indexer process.
fn walk_file(file_path: &str, content: &str, language: &str) -> std::result::Result<buffers::EmitOut, String> {
    stack::run_guarded(|| match language {
        "java" => java::extract(file_path, content),
        "python" => python::extract(file_path, content),
        "go" => go::extract(file_path, content),
        "c" | "cpp" => ccpp::extract(file_path, content, language),
        "rust" => rustlang::extract(file_path, content),
        "csharp" => csharp::extract(file_path, content),
        "ruby" => ruby::extract(file_path, content),
        "php" => php::extract(file_path, content),
        "swift" => swift::extract(file_path, content),
        "kotlin" => kotlin::extract(file_path, content),
        "r" => rlang::extract(file_path, content),
        "lua" | "luau" => lua::extract(file_path, content, language),
        "scala" => scala::extract(file_path, content),
        "dart" => dart::extract(file_path, content),
        _ => tsjs::extract(file_path, content, language),
    })
}

fn to_buffers(out: buffers::EmitOut) -> ExtractBuffers {
    ExtractBuffers {
        meta: out.meta.into(),
        nodes: out.nodes.into(),
        edges: out.edges.into(),
        refs: out.refs.into(),
        bindings: out.bindings.into(),
        arena: out.arena.into(),
    }
}

/// Binding rows only, for a file the generic extractor extracts (a
/// stack-guard defer, ArkTS, the kernel kill switch —
/// resolution-binding-model-plan.md §2.4). The same walk as `extract_file`
/// produces them, so the two paths can never disagree on a row; nodes, edges
/// and refs are dropped and the rows come back nodeless for the TS side to
/// attach by name and line. A file too deep for the walk yields `defer:` and
/// no rows, like its nodes.
#[napi]
pub fn bindings_file(file_path: String, content: String, language: String) -> Result<ExtractBuffers> {
    let out = walk_file(&file_path, &content, &language).map_err(Error::from_reason)?;
    Ok(to_buffers(out.bindings_only()))
}

#[napi]
pub fn extract_file(file_path: String, content: String, language: String) -> Result<ExtractBuffers> {
    let out = walk_file(&file_path, &content, &language).map_err(Error::from_reason)?;
    Ok(to_buffers(out))
}
