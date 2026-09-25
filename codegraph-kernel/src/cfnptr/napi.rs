//! The napi surface of the cFnPtr synthesizer: the transfer types the TS side
//! passes and receives, and the entry points for the sweep (stages A/B),
//! the macro environment (stage C) and the link (stages D/E).

use napi_derive::napi;

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

fn facts_to_out(facts: super::FileFacts) -> CfnptrFacts {
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
            let structs: Vec<super::StructExtent> = f
                .structs
                .into_iter()
                .map(|s| super::StructExtent { id: s.id, start_line: s.start_line, end_line: s.end_line })
                .collect();
            facts_to_out(super::scan_file(&f.text, &structs))
        })
        .collect()
}

/// Read a file the way JS `readFileSync(path, 'utf-8')` does: valid UTF-8
/// moves in without a copy; invalid bytes take the lossy path.
fn read_lossy(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
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
        let structs: Vec<super::StructExtent> = f
            .structs
            .iter()
            .map(|s| super::StructExtent { id: s.id.clone(), start_line: s.start_line, end_line: s.end_line })
            .collect();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::scan_file(&text, &structs))) {
            Ok(facts) => facts_to_out(facts),
            Err(_) => empty_cfnptr_facts(),
        }
    }
    super::par_map(&files, scan_one, empty_cfnptr_facts)
}

/// Debug/differential hook: the native `stripCommentsForRegex(text, 'c')`.
/// Exists so the strip differential oracle can pin the Rust stripper against
/// the TS reference directly.
#[napi]
pub fn cfnptr_strip_c(text: String) -> String {
    String::from_utf8(super::strip_c(text.as_bytes())).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
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

/// One file's `buildEnv` inputs — see `super::file_env`.
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
        let e = super::file_env(&text);
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
    super::par_map(&paths, |p| one(p), || None)
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

/// The registration tables verbatim — see `super::LinkTables`.
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
    let files: Vec<super::LinkFile> = files
        .into_iter()
        .map(|f| super::LinkFile {
            rel: f.rel,
            abs: f.abs,
            prop: f.prop,
            dispatch: f.dispatch,
            fns: f
                .fns
                .into_iter()
                .map(|n| super::LinkFn {
                    id: n.id,
                    start_line: n.start_line as i64,
                    end_line: n.end_line as i64,
                })
                .collect(),
        })
        .collect();
    let tables = super::LinkTables {
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
                        .map(|en| super::LinkArrEntry { file: en.file, ids: en.ids })
                        .collect(),
                )
            })
            .collect(),
    };
    CfnptrLinkOut {
        edges: super::cfnptr_link(&files, &tables)
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

fn field_in(f: CfnptrLinkField) -> super::LinkField {
    super::LinkField { name: f.name, ftype: f.ty, is_fn_ptr: f.is_fn_ptr }
}
