//! Native batch reference resolver — Phase 4 of the binding-model plan
//! (docs/design/resolution-binding-model-plan.md §"Phase 4").
//!
//! With every source-reading predicate moved onto the `bindings` table, the
//! resolve step for a bare identifier is a join over `bindings`, `nodes` and
//! `unresolved_refs`. `KernelResolver` performs that join natively on a
//! read-only connection, mirroring the `resolver-worker` chunk contract:
//! `readPendingBatch` is the `read` stage (the keyset cursor of
//! `getUnresolvedReferencesBatchAfter`) and `resolveChunk` is the `settle`
//! stage — one verdict per ref, admitted by TypeScript in input order.
//!
//! Eligibility (kernel v1): `referenceName` carries none of the separators a
//! skipped strategy keys on (`.`, `:`, `/`, `\`, `#`, `$`, `(`, `)`) and the
//! language emits binding rows (BINDINGS_LANGUAGES in
//! src/extraction/kernel/index.ts). Bare `function_ref` refs resolve
//! natively on their dedicated arm; `this.`/`Cls::m` shapes carry a
//! separator and stay behind. Everything else — receivers, chains, include
//! paths, qualified names — returns `passthrough` and the TypeScript path
//! resolves it unchanged.
//!
//! Determinism is part of the contract: every query reproduces its
//! TypeScript ORDER BY verbatim because `findBestMatch` is first-max, and
//! per-statement reads see the same committed edge state the sequential
//! baseline's reads would (WAL readers never pin a snapshot here — there is
//! no spanning transaction).

use napi::bindgen_prelude::*;
use napi_derive::napi;
use regex::Regex;
use rusqlite::{Connection, OpenFlags};
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};

/// This thread's own clone of a shared, statically compiled regex. A clone
/// shares the compiled program but gets its own search-cache pool, and a
/// pool's owner thread skips the pool's mutex; searched straight from the
/// static, every pool worker but one runs on the slow path (regex docs,
/// "Sharing a regex across threads can result in contention" — the case here
/// exactly: short haystacks, one search after another).
fn thread_regex(re: &'static Regex) -> Rc<Regex> {
    thread_local! {
        static LOCAL: RefCell<HashMap<usize, Rc<Regex>>> = RefCell::new(HashMap::new());
    }
    LOCAL.with(|map| {
        map.borrow_mut()
            .entry(re as *const Regex as usize)
            .or_insert_with(|| Rc::new(re.clone()))
            .clone()
    })
}

/// A fixed pattern as this thread's own regex (see thread_regex for why
/// per thread): compiled once on first use, cloned once per thread.
macro_rules! re {
    ($pattern:expr) => {{
        static SHARED: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
        thread_local! {
            static RE: Rc<Regex> = Rc::new(SHARED.clone());
        }
        RE.with(Rc::clone)
    }};
}

// ---------------------------------------------------------------------------
// napi input/output shapes
// ---------------------------------------------------------------------------

#[napi(object)]
pub struct KernelKv {
    pub key: String,
    pub value: String,
}

#[napi(object)]
pub struct KernelAliasPatternIn {
    pub prefix: String,
    pub suffix: String,
    pub has_wildcard: bool,
    pub replacements: Vec<String>,
}

#[napi(object)]
pub struct KernelAliasMapIn {
    pub base_url: Option<String>,
    pub patterns: Vec<KernelAliasPatternIn>,
}

#[napi(object)]
pub struct KernelWorkspaceIn {
    /// Import-source path prefix → workspace package name (longest prefix wins).
    pub source_entries: Vec<KernelKv>,
    /// Package name → directory containing its manifest.
    pub by_name: Vec<KernelKv>,
    /// Package name → entry file, when the manifest declares one.
    pub entry_by_name: Option<Vec<KernelKv>>,
    /// Link-source package names skipped during directory checks.
    pub local_link_names: Option<Vec<String>>,
}

#[napi(object)]
pub struct KernelResolverConfig {
    pub db_path: String,
    pub project_root: String,
    pub aliases: Option<KernelAliasMapIn>,
    pub workspaces: Option<KernelWorkspaceIn>,
    pub go_module_path: Option<String>,
    /// Already-resolved compile_commands include dirs (may be empty →
    /// the kernel applies the same hardcoded fallback as the TS path).
    pub cpp_include_dirs: Option<Vec<String>>,
    /// node:module builtinModules + their node: prefixed spellings.
    pub node_builtin_specifiers: Vec<String>,
    /// When true the TS orchestrator still runs framework resolvers; the
    /// kernel then reports its candidate list instead of a bare verdict.
    pub frameworks_active: bool,
    /// Names (`f.name`) of the detected framework resolvers, for the
    /// kernel's `claimsReference` evaluation. `None` means "not supplied"
    /// — the kernel then treats every prefilter miss as claimed, matching
    /// the old `frameworks_active`-only behavior.
    pub framework_names: Option<Vec<String>>,
    /// Max distinct-name count for the fuzzy matcher (queries.fuzzyMatchCeiling).
    pub ambiguous_name_ceiling: Option<u32>,
    /// Run token the pool hands every worker of one resolution run. Resolvers
    /// with the same `db_path` and generation share one in-memory node table
    /// (see NodeTable); without it the table is private to the instance.
    pub generation: Option<String>,
    /// True when this connection sees every `extends`/`implements` edge the
    /// current resolution run will read: the live db, or a snapshot taken
    /// after the prerequisite phase. Enables the supertype walks; otherwise
    /// they punt (`btm-supers`/`rmot-supers`) to TS, which reads live edges.
    pub supertypes_complete: Option<bool>,
    /// `db_path` is a private checkpointed copy nothing writes (a pool
    /// worker's snapshot). Opened `immutable=1`: no locks and no `-wal`/`-shm`
    /// — a copy has no `-shm`, and `readonly_shm=1` can't open one without it.
    pub snapshot: Option<bool>,
    /// Answer node lookups with indexed queries instead of loading the whole
    /// node table. For small batches (an incremental sync) the table load
    /// dominates; the queries return the same rows in the same order.
    pub query_lookups: Option<bool>,
}

/// One unresolved_refs row — mirrors UnresolvedReference/rowId shape so the
/// TS side can feed passthrough refs straight into the existing pipeline.
#[napi(object)]
#[derive(Clone)]
pub struct ResolveRefIn {
    pub row_id: Option<i64>,
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
    pub line: i64,
    pub column: i64,
    pub candidates: Option<String>,
    pub file_path: String,
    pub language: String,
    pub failure_reason: Option<String>,
}

impl ResolveRefIn {
    /// This ref re-sited at `node`'s declaration (file and first line).
    fn at(mut self, node: &KNode) -> Self {
        self.file_path = node.file_path.clone();
        self.line = node.start_line;
        self
    }

    /// This ref asking for `name` as a `kind` reference instead.
    fn naming(mut self, name: &str, kind: &str) -> Self {
        self.reference_name = name.to_string();
        self.reference_kind = kind.to_string();
        self
    }
}

#[napi(object)]
pub struct KernelCandidateOut {
    pub target_node_id: String,
    pub confidence: f64,
    pub resolved_by: String,
}

/// One verdict per input ref. `status`:
///   - `resolved`    — verdict (gates + alias forwarding applied). `isFinal`
///                     marks the import early-win; under frameworks it can
///                     still be displaced by a ≥0.9 framework hit.
///   - `unresolved`  — terminal miss (do not let TS "rescue" it).
///   - `passthrough` — kernel declined; run the full TS pipeline. `reason`
///                     names the gate that punted (diagnostics only).
/// `candidates` is populated only when `frameworksActive` and the kernel
/// produced a non-final verdict — the raw [import?, name?] list for the TS
/// first-max merge.
#[napi(object)]
pub struct ResolveOutcome {
    pub status: String,
    pub target_node_id: Option<String>,
    pub confidence: Option<f64>,
    pub resolved_by: Option<String>,
    pub is_final: bool,
    pub candidates: Option<Vec<KernelCandidateOut>>,
    pub reason: Option<String>,
}

impl ResolveOutcome {
    fn passthrough(reason: &'static str) -> Self {
        ResolveOutcome {
            status: "passthrough".into(),
            target_node_id: None,
            confidence: None,
            resolved_by: None,
            is_final: false,
            candidates: None,
            reason: Some(reason.into()),
        }
    }
    fn unresolved() -> Self {
        ResolveOutcome {
            status: "unresolved".into(),
            target_node_id: None,
            confidence: None,
            resolved_by: None,
            is_final: false,
            candidates: None,
            reason: None,
        }
    }
    /// Frameworks are active and the kernel found no name/import candidate —
    /// the TS merge must still run over framework candidates alone.
    fn no_candidates() -> Self {
        ResolveOutcome {
            candidates: Some(Vec::new()),
            ..Self::unresolved()
        }
    }
    /// A chain call nothing matched: the framework loop still runs, and when
    /// it finds nothing either the ref waits for the conformance pass
    /// (resolveOneInner's deferral) instead of failing.
    fn deferred() -> Self {
        ResolveOutcome {
            reason: Some("defer".into()),
            ..Self::no_candidates()
        }
    }
    /// A miss the framework loop may still overturn, but only with a ≥0.9 hit:
    /// lower framework candidates are discarded, not merged (the TS chain
    /// branch returns before the merge).
    fn final_miss() -> Self {
        ResolveOutcome {
            candidates: Some(Vec::new()),
            is_final: true,
            ..Self::unresolved()
        }
    }
    fn resolved(target: &KNode, confidence: f64, by: &str, is_final: bool, cands: Option<Vec<KernelCandidateOut>>) -> Self {
        ResolveOutcome {
            status: "resolved".into(),
            target_node_id: Some(target.id.clone()),
            confidence: Some(confidence),
            resolved_by: Some(by.to_string()),
            is_final,
            candidates: cands,
            reason: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Small internal verdict type — the kernel-side ResolvedRef.
// ---------------------------------------------------------------------------

struct KCand {
    node: Arc<KNode>,
    confidence: f64,
    resolved_by: &'static str,
}

/// Why a ref leaves the native resolver before a verdict: a punt (the next
/// step needs state the snapshot can't see — a source read, live supertype
/// edges, tree-sitter parsing, an unported arm — so the TS spine takes the
/// ref, with the reason for the profile), or a napi error. The punt is the
/// only concept the kernel adds to the TS matchers, which just return null:
/// every native matcher returns `Res<Option<_>>`, where `Ok(None)` is TS's
/// provable null and `?` carries a punt up to `resolve_chunk`, which turns
/// it into a `passthrough` outcome.
enum Halt {
    Punt(&'static str),
    Napi(Error),
}

type Res<T> = std::result::Result<T, Halt>;

impl From<Error> for Halt {
    fn from(e: Error) -> Self {
        Halt::Napi(e)
    }
}

impl From<Halt> for Error {
    fn from(h: Halt) -> Self {
        match h {
            Halt::Napi(e) => e,
            Halt::Punt(reason) => Error::from_reason(format!("unexpected punt outside resolve_ref: {reason}")),
        }
    }
}

/// method_call_shape's answer: settled already, or the parsed receiver
/// shape for the caller's own arm.
enum McShape {
    Done(Option<KCand>),
    /// `dotted`: the `recv.method` shape (dotMatch) matched.
    Parsed { receiver: String, method: String, inferable: bool, dotted: bool },
}

// ---------------------------------------------------------------------------
// Tiny LRU for file contents (queries.fileCache equivalent).
// ---------------------------------------------------------------------------

/// A cached source file: its lines (what the TS fileCache holds), plus
/// whole-file derivations computed on first use instead of once per ref.
pub(super) struct SourceFile {
    lines: Vec<String>,
    text: OnceCell<String>,
    rust_uses: OnceCell<HashMap<String, String>>,
    /// `lines_containing` memo, by needle.
    needle_lines: RefCell<HashMap<String, Rc<[u32]>>>,
}

impl SourceFile {
    fn new(lines: Vec<String>) -> Self {
        SourceFile {
            lines,
            text: OnceCell::new(),
            rust_uses: OnceCell::new(),
            needle_lines: RefCell::new(HashMap::new()),
        }
    }

    /// The lines rejoined with `\n` (CRLF already normalized).
    pub(super) fn text(&self) -> &str {
        self.text.get_or_init(|| self.lines.join("\n"))
    }

    /// Indices of the lines containing `needle`, ascending — one SIMD pass
    /// over the file per distinct needle instead of a search per line per
    /// query. A pattern that needs the literal can only match these.
    pub(super) fn lines_containing(&self, needle: &str) -> Rc<[u32]> {
        if let Some(hit) = self.needle_lines.borrow().get(needle) {
            return hit.clone();
        }
        let finder = memchr::memmem::Finder::new(needle.as_bytes());
        let hits: Rc<[u32]> = (0..self.lines.len() as u32)
            .filter(|&i| finder.find(self.lines[i as usize].as_bytes()).is_some())
            .collect();
        self.needle_lines.borrow_mut().insert(needle.to_string(), hits.clone());
        hits
    }

    /// The file's Rust `use` bindings (collectRustUseBindings).
    pub(super) fn rust_uses(&self) -> &HashMap<String, String> {
        self.rust_uses.get_or_init(|| collect_rust_use_bindings(self.text()))
    }
}

impl std::ops::Deref for SourceFile {
    type Target = Vec<String>;
    fn deref(&self) -> &Vec<String> {
        &self.lines
    }
}

struct FileCache {
    map: HashMap<String, Option<Rc<SourceFile>>>,
    order: VecDeque<String>,
    cap: usize,
}

impl FileCache {
    fn new(cap: usize) -> Self {
        FileCache { map: HashMap::new(), order: VecDeque::new(), cap }
    }
    fn get(&self, k: &str) -> Option<&Option<Rc<SourceFile>>> {
        self.map.get(k)
    }
    fn put(&mut self, k: String, v: Option<Rc<SourceFile>>) {
        if self.map.contains_key(&k) {
            self.order.retain(|x| x != &k);
        } else if self.map.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(k.clone());
        self.map.insert(k, v);
    }
}

// ---------------------------------------------------------------------------
// KernelResolver
// ---------------------------------------------------------------------------

thread_local! {
    static PROF: std::cell::RefCell<HashMap<String, (u64, u64)>> = std::cell::RefCell::new(HashMap::new());
}
static PROF_ON: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("CODEGRAPH_KERNEL_PROF").is_some_and(|v| v == "1"));
/// `CODEGRAPH_KERNEL_PROF=1`: attribute a step's time to `sub:<label>`;
/// otherwise the expression alone.
macro_rules! probe {
    ($r:expr, $label:expr, $e:expr) => {{
        if *PROF_ON {
            let t0 = std::time::Instant::now();
            let v = $e;
            prof_add(format!("sub:{}|{}|{}", $label, $r.reference_kind, $r.language), t0.elapsed().as_nanos() as u64);
            v
        } else {
            $e
        }
    }};
}
fn prof_add(key: String, ns: u64) {
    PROF.with(|p| {
        let mut p = p.borrow_mut();
        let e = p.entry(key).or_insert((0, 0));
        e.0 += 1;
        e.1 += ns;
    });
}
fn prof_dump(label: &str) {
    PROF.with(|p| {
        let p = p.borrow();
        for (k, (n, ns)) in p.iter() {
            eprintln!("[kernel-prof] {label} {k} n={n} ns={ns}");
        }
    });
}

mod tables;
mod affix;
mod node_table;
mod paths;
mod query;
mod prefilter;
mod imports;
mod names;
mod receivers;
mod bound;
mod fields;
mod method_call;
mod pipeline;
mod file_refs;
mod rust_modules;
mod awaited;
mod iteration;
use self::tables::*;
use self::affix::*;
use self::node_table::*;
use self::paths::*;
use self::names::*;
use self::prefilter::*;

#[napi]
pub struct KernelResolver {
    // Option so close() can drop the Connection deterministically — see
    // close() for why GC-timed teardown is unsafe on a shared -shm.
    conn: Option<Connection>,
    project_root: String,
    root_abs: String,
    /// AliasMap (project-aliases.ts), as the TS side passes it.
    aliases: Option<KernelAliasMapIn>,
    workspaces: Option<WorkspaceK>,
    go_module_path: Option<String>,
    cpp_include_dirs: Vec<String>,
    node_builtins: HashSet<String>,
    frameworks_active: bool,
    supertypes_complete: bool,
    framework_names: Option<Vec<String>>,
    ambiguous_ceiling: i64,

    /// The run's node table, loaded on first use (a worker reports ready
    /// before it) and dropped by close().
    table: std::cell::OnceCell<Arc<NodeTable>>,
    /// Set when `query_lookups`: per-key query results (no table is loaded).
    lookups: Option<RefCell<node_table::QueryLookups>>,
    db_path: String,
    /// The run token the table is shared under (see NodeTable); None keeps
    /// the table private.
    generation: Option<String>,
    bindings_cache: HashMap<String, Rc<Vec<KBinding>>>,
    import_map_cache: HashMap<String, Rc<Vec<KImport>>>,
    reexport_cache: HashMap<String, Rc<Vec<KReExport>>>,
    export_index: HashMap<String, Rc<FileExportIndexK>>,
    import_path_memo: HashMap<String, Option<String>>,
    exported_symbol_memo: HashMap<String, Option<Arc<KNode>>>,
    sealed_memo: HashMap<String, bool>,
    c_static_memo: HashMap<String, bool>,
    rust_trait_memo: HashMap<String, bool>,
    root_import_memo: HashMap<String, bool>,
    /// matchSelectedStoreCall's per-file selector names (`const a = f((s) =>`).
    selector_names_memo: HashMap<String, Rc<HashSet<String>>>,
    /// inferEsmAwaitedCallType's per-file index (`None`: no awaited binding).
    awaited_files: HashMap<String, Option<Rc<awaited::AwaitedFile>>>,
    rust_crate_root_memo: HashMap<String, Option<String>>,
    /// factory_initializer memo: (file, binding line, root, binding node).
    factory_init_memo: HashMap<(String, i64, String, Option<String>), Rc<method_call::FactoryInit>>,
    file_cache: FileCache,
}

/// Open `db_path` read-only without ever writing its `-shm` wal-index.
///
/// The kernel links its own SQLite build, and POSIX locks never conflict
/// within one process, so this connection cannot see node:sqlite's locks on
/// the same file. Opened normally it takes itself for the first connection
/// and truncates the `-shm` file node:sqlite still has mapped: the next
/// node:sqlite write into a region past the new end of file is a SIGBUS
/// (a sync after a large checkout crashed 3 runs in 4). `readonly_shm=1`
/// opens the `-shm` read-only, and SQLite then reads the WAL through its
/// heap-memory wal-index instead of the shared one.
fn open_read_only_shm(db_path: &str) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?readonly_shm=1", uri_path(db_path));
    Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
        .or_else(|_| Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY))
}

/// Open a snapshot copy that no connection writes. A copied WAL-mode file has
/// no `-shm`; `readonly_shm=1` then fails at the first query ("unable to open
/// database file" — the open itself is lazy, so the fallback above never
/// runs), and a plain read-only open would create one. `immutable=1` reads
/// the file as-is.
fn open_immutable(db_path: &str) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?immutable=1", uri_path(db_path));
    Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
}

/// A filesystem path as a SQLite URI path: `/` separators, a leading `/`
/// before a Windows drive letter, and the characters a URI reserves escaped.
fn uri_path(db_path: &str) -> String {
    let path = db_path.replace('\\', "/");
    let mut out = String::with_capacity(path.len() + 8);
    if path.as_bytes().get(1) == Some(&b':') {
        out.push('/');
    }
    for c in path.chars() {
        match c {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            ' ' => out.push_str("%20"),
            _ => out.push(c),
        }
    }
    out
}

#[napi]
impl KernelResolver {
    #[napi(constructor)]
    pub fn new(config: KernelResolverConfig) -> Result<Self> {
        let conn = if config.snapshot.unwrap_or(false) {
            open_immutable(&config.db_path)
        } else {
            open_read_only_shm(&config.db_path)
        }
            .map_err(|e| Error::from_reason(format!("KernelResolver open {}: {e}", config.db_path)))?;
        let root_abs = pos_normalize(&config.project_root);
        let workspaces = config.workspaces.map(|w| WorkspaceK {
            source_entries: w.source_entries.into_iter().map(|kv| (kv.key, kv.value)).collect(),
            by_name: w.by_name.into_iter().map(|kv| (kv.key, kv.value)).collect(),
            entry_by_name: w
                .entry_by_name
                .map(|v| v.into_iter().map(|kv| (kv.key, kv.value)).collect()),
            local_link_names: w
                .local_link_names
                .map(|v| v.into_iter().collect())
                .unwrap_or_default(),
        });
        // context.getCppIncludeDirs?.() ?? [] — the TS side resolves
        // compile_commands.json + convention heuristics and passes the list;
        // empty means no -I search, not a fallback.
        let cpp_include_dirs = config.cpp_include_dirs.unwrap_or_default();
        Ok(KernelResolver {
            conn: Some(conn),
            table: std::cell::OnceCell::new(),
            lookups: config.query_lookups.unwrap_or(false).then(Default::default),
            db_path: config.db_path,
            generation: config.generation,
            project_root: config.project_root,
            root_abs,
            aliases: config.aliases,
            workspaces,
            go_module_path: config.go_module_path,
            cpp_include_dirs,
            node_builtins: config.node_builtin_specifiers.into_iter().collect(),
            frameworks_active: config.frameworks_active,
            supertypes_complete: config.supertypes_complete.unwrap_or(false),
            framework_names: config.framework_names,
            ambiguous_ceiling: config.ambiguous_name_ceiling.unwrap_or(500) as i64,
            bindings_cache: HashMap::new(),
            import_map_cache: HashMap::new(),
            reexport_cache: HashMap::new(),
            export_index: HashMap::new(),
            import_path_memo: HashMap::new(),
            exported_symbol_memo: HashMap::new(),
            sealed_memo: HashMap::new(),
            c_static_memo: HashMap::new(),
            rust_trait_memo: HashMap::new(),
            root_import_memo: HashMap::new(),
            selector_names_memo: HashMap::new(),
            awaited_files: HashMap::new(),
            rust_crate_root_memo: HashMap::new(),
            factory_init_memo: HashMap::new(),
            file_cache: FileCache::new(1024),
        })
    }

    /// Deterministic connection teardown. Without it the rusqlite Connection
    /// closes whenever V8 GCs the JS wrapper — at an arbitrary later moment,
    /// possibly while resolver-pool workers' node:sqlite conns are mid-WAL
    /// I/O on the live -shm. The bundled SQLite's intra-process wal-index
    /// locks can't see node:sqlite's (POSIX fcntl is per-process), so a
    /// GC-timed close's shm teardown races them (SIGBUS / wal-index
    /// corruption on the linux corpus). Callers must close() only while no
    /// other-build conn can be doing shm work — before pool workers spawn or
    /// after they die.
    #[napi]
    pub fn close(&mut self) {
        self.debug_stats("close");
        self.conn.take();
        self.table.take();
    }

    /// `CODEGRAPH_KERNEL_STATS=1`: the per-instance cache sizes, to stderr —
    /// what a resolver holds by the end of a run.
    fn debug_stats(&self, at: &str) {
        if !std::env::var_os("CODEGRAPH_KERNEL_STATS").is_some_and(|v| v == "1") {
            return;
        }
        let file_lines: usize = self.file_cache.map.values().map(|v| v.as_ref().map_or(0, |l| l.len())).sum();
        let file_bytes: usize = self
            .file_cache
            .map
            .values()
            .map(|v| v.as_ref().map_or(0, |l| l.iter().map(|s| s.capacity() + 24).sum::<usize>()))
            .sum();
        eprintln!(
            "[kernel-stats] {at}: regex(shared)={} files={} (lines={file_lines} bytes={file_bytes}) bindings={} imports={} reexports={} export_index={} memos: symbol={} import_path={} sealed={} c_static={} rust_trait={} root_import={} crate_root={} factory_init={}",
            shared_regex_count(),
            self.file_cache.map.len(),
            self.bindings_cache.len(),
            self.import_map_cache.len(),
            self.reexport_cache.len(),
            self.export_index.len(),
            self.exported_symbol_memo.len(),
            self.import_path_memo.len(),
            self.sealed_memo.len(),
            self.c_static_memo.len(),
            self.rust_trait_memo.len(),
            self.root_import_memo.len(),
            self.rust_crate_root_memo.len(),
            self.factory_init_memo.len(),
        );
    }

    /// The `read` stage — same keyset cursor and prerequisite split as
    /// queries.getUnresolvedReferencesBatchAfter, plus the denormalized
    /// file/language fallback via the origin node that resolveOne applies.
    #[napi]
    pub fn read_pending_batch(
        &mut self,
        after_row_id: i64,
        limit: i64,
        prerequisites: bool,
    ) -> Result<Vec<ResolveRefIn>> {
        const PREREQ_KINDS: &str = "('imports','extends','implements')";
        let sql = format!(
            "SELECT id, from_node_id, reference_name, reference_kind, line, col, \
             candidates, file_path, language, failure_reason FROM unresolved_refs \
             WHERE status = 'pending' AND id > ?1 \
             AND reference_kind {} {} ORDER BY id LIMIT ?2",
            if prerequisites { "IN" } else { "NOT IN" },
            PREREQ_KINDS
        );
        let mut stmt = self
            .conn()?
            .prepare(&sql)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let rows = stmt
            .query_map([after_row_id, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut collected = Vec::new();
        for r in rows {
            collected.push(r.map_err(|e| Error::from_reason(e.to_string()))?);
        }
        drop(stmt);
        let mut out = Vec::new();
        for (row_id, from_node_id, reference_name, reference_kind, line, column, candidates, mut file_path, mut language, failure_reason) in collected
        {
            // Denormalized fallback — mirrors UnresolvedRef construction in
            // resolveBatchYielding: `raw.filePath || nodePath`,
            // `raw.language || nodeLang` — falsy (empty) only; a stored
            // 'unknown' passes through unchanged.
            if file_path.is_empty() || language.is_empty() {
                if let Some(node) = self.node_by_id(&from_node_id)? {
                    if file_path.is_empty() {
                        file_path = node.file_path.clone();
                    }
                    if language.is_empty() {
                        language = node.language.clone();
                    }
                }
            }
            out.push(ResolveRefIn {
                row_id: Some(row_id),
                from_node_id,
                reference_name,
                reference_kind,
                line,
                column,
                candidates,
                file_path,
                language,
                failure_reason,
            });
        }
        Ok(out)
    }

    /// The `settle` stage — one verdict per ref in input order.
    #[napi]
    pub fn resolve_chunk(&mut self, refs: Vec<ResolveRefIn>) -> Result<Vec<ResolveOutcome>> {
        let mut out = Vec::with_capacity(refs.len());
        for r in refs {
            let t0 = PROF_ON.then(std::time::Instant::now);
            let o = match self.resolve_ref(&r) {
                Ok(o) => o,
                Err(Halt::Punt(reason)) => ResolveOutcome::passthrough(reason),
                Err(Halt::Napi(e)) => return Err(e),
            };
            if let Some(t0) = t0 {
                let key = match o.status.as_str() {
                    "passthrough" => format!("punt:{}|{}|{}", o.reason.as_deref().unwrap_or(""), r.reference_kind, r.language),
                    "resolved" => format!("hit:{}|{}|{}", o.resolved_by.as_deref().unwrap_or(""), r.reference_kind, r.language),
                    st => format!("{st}|{}|{}", r.reference_kind, r.language),
                };
                prof_add(key, t0.elapsed().as_nanos() as u64);
            }
            out.push(o);
        }
        Ok(out)
    }

    /// resolveChainedCallsViaConformance's per-ref match, run after the main
    /// pass wrote every implements/extends edge: PHP `this->prop.method`
    /// through the method-call arm, Rust's `::` chains through the scoped
    /// arm, every other chain through the dotted arm, then the language gate.
    #[napi]
    pub fn resolve_deferred_chains(&mut self, refs: Vec<ResolveRefIn>) -> Result<Vec<ResolveOutcome>> {
        let mut out = Vec::with_capacity(refs.len());
        for r in refs {
            if !is_migrated_language(&r.language) {
                out.push(ResolveOutcome::passthrough("ineligible:lang"));
                continue;
            }
            let hit = if r.language == "php" && php_prop_shape_re().is_match(&r.reference_name) {
                self.match_method_call_free(&r)
            } else if r.language == "rust" {
                self.match_scoped_call_chain(&r)
            } else {
                self.match_dotted_call_chain(&r)
            };
            out.push(match hit {
                Ok(c) => match self.gate_language(c, &r) {
                    Some(c) => ResolveOutcome::resolved(&c.node, c.confidence, c.resolved_by, true, None),
                    None => ResolveOutcome::unresolved(),
                },
                Err(Halt::Punt(reason)) => ResolveOutcome::passthrough(reason),
                Err(Halt::Napi(e)) => return Err(e),
            });
        }
        Ok(out)
    }
}


impl From<&KCand> for KernelCandidateOut {
    fn from(c: &KCand) -> Self {
        KernelCandidateOut {
            target_node_id: c.node.id.clone(),
            confidence: c.confidence,
            resolved_by: c.resolved_by.to_string(),
        }
    }
}

impl Drop for KernelResolver {
    fn drop(&mut self) {
        self.debug_stats("drop");
        prof_dump("drop");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_len_counts_units_from_bytes() {
        for s in ["", "abc", "é", "€", "😀", "a😀é€x"] {
            assert_eq!(utf16_len(s), s.encode_utf16().count(), "{s:?}");
        }
        let wide = "€".repeat(4); // 12 bytes, 4 units
        assert!(!utf16_len_exceeds(&wide, 4) && utf16_len_exceeds(&wide, 3));
    }

    #[test]
    fn path_or_tail_needs_a_segment_boundary() {
        assert!(is_path_or_tail("pkg/mod.py", "pkg/mod.py"));
        assert!(is_path_or_tail("src/pkg/mod.py", "pkg/mod.py"));
        assert!(!is_path_or_tail("src/xpkg/mod.py", "pkg/mod.py"));
        assert!(!is_path_or_tail("mod.py", "pkg/mod.py"));
    }

    /// Rows inserted out of every query's order, with ties on each ORDER BY
    /// key, so the table's stable sorts are checked against SQLite's own
    /// answers (the `verify` pass) and against the documented sequences.
    #[test]
    fn node_table_orders_match_sqlite() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, \
             qualified_name TEXT NOT NULL, file_path TEXT NOT NULL, language TEXT NOT NULL, \
             start_line INTEGER NOT NULL, end_line INTEGER NOT NULL, start_column INTEGER NOT NULL, \
             end_column INTEGER NOT NULL, signature TEXT, visibility TEXT, \
             is_exported INTEGER NOT NULL DEFAULT 0, return_type TEXT, type_parameters TEXT, decorators TEXT); \
             CREATE INDEX idx_nodes_name ON nodes(name); \
             CREATE INDEX idx_nodes_qualified_name ON nodes(qualified_name); \
             CREATE INDEX idx_nodes_file_path ON nodes(file_path); \
             CREATE INDEX idx_nodes_file_line ON nodes(file_path, start_line); \
             CREATE INDEX idx_nodes_lower_name ON nodes(lower(name)); \
             CREATE TABLE files (path TEXT PRIMARY KEY); \
             INSERT INTO files(path) VALUES ('b.ts'), ('a.ts');",
        )
        .unwrap();
        // (id, name, qualified_name, file_path, start_line): the files arrive in
        // reverse order, n2/n4 tie on every key, n3 differs from them in case only.
        let rows = [
            ("n1", "run", "B::run", "b.ts", 5),
            ("n2", "run", "A::run", "a.ts", 9),
            ("n3", "Run", "run", "a.ts", 9),
            ("n4", "run", "A::run", "a.ts", 9),
            ("n5", "run", "A::run", "a.ts", 2),
        ];
        for (id, name, qn, file, line) in rows {
            conn.execute(
                "INSERT INTO nodes(id, kind, name, qualified_name, file_path, language, start_line, \
                 end_line, start_column, end_column) VALUES (?1, 'function', ?2, ?3, ?4, 'typescript', ?5, ?5, 0, 0)",
                rusqlite::params![id, name, qn, file, line],
            )
            .unwrap();
        }
        let t = NodeTable::load(&conn).unwrap();
        assert_eq!(t.verify(&conn).unwrap(), 0);
        fn ids(l: &NodeList) -> Vec<&str> {
            l.iter().map(|n| n.id.as_str()).collect()
        }
        // getNodesByName: file_path, start_line, then rowid for the n2/n4 tie.
        assert_eq!(ids(&t.by_name["run"]), ["n5", "n2", "n4", "n1"]);
        // getNodesByLowerName: rowid order across both spellings.
        assert_eq!(ids(&t.by_lower()["run"]), ["n1", "n2", "n3", "n4", "n5"]);
        assert_eq!(ids(&t.by_qname["A::run"]), ["n2", "n4", "n5"]);
        // getNodesInFile: start_line, rowid on ties.
        assert_eq!(ids(&t.by_file["a.ts"]), ["n5", "n2", "n3", "n4"]);
        assert_eq!(t.by_id["n3"].qualified_name, "run");
        assert!(t.files.contains("a.ts") && !t.by_name.contains_key("missing"));
    }

    /// The match sequence (group 1, match end) an `Affix` yields for `word`
    /// on `line`, the way infer_match_line walks it.
    fn affix_matches(affix: &'static Affix, line: &str, word: &str) -> Vec<(String, usize)> {
        let mut got = Vec::new();
        let mut from = 0;
        while let Some(m) = affix.find_from(line, word, from) {
            got.push((m.group.map_or(String::new(), |(s, e)| line[s..e].to_string()), m.end));
            from = m.end;
        }
        got
    }

    /// `captures_iter` of the original pattern with `word` formatted in for
    /// `R`, in the same shape.
    fn regex_matches(pattern_with_r: &str, line: &str, word: &str) -> Vec<(String, usize)> {
        let re = Regex::new(&pattern_with_r.replace('R', &regex::escape(word))).unwrap();
        re.captures_iter(line)
            .map(|c| (c.get(1).map_or(String::new(), |g| g.as_str().to_string()), c.get(0).unwrap().end()))
            .collect()
    }

    const RECEIVERS: [&str; 7] = ["user", "x", "my_var", "über", "Foo", "this", "req"];
    const LINES: [&str; 20] = [
        "const user = new UserStore();",
        "let user: Foo<Bar> | null = x;",
        "user = Foo.new; user: Bar",
        "  user := &Store{}",
        "var user *pkg.Client",
        "func f(user pkg.Client, x int)",
        "Foo user = new Foo(); Bar user;",
        "private ?Foo $user;  $user = new Bar;",
        "struct ops *user, x;",
        "let mut user: &Foo = Bar::new();",
        "user : Foo = Bar()",
        "user <- Foo$new()",
        "x = x = Foo.new",
        "userx user\u{a0}=\u{a0}new Baz",
        "user user = Foo(); Foo user user = Bar",
        "über: Über.new  über = Straße(",
        "a.user: Foo;this.user: Bar",
        "user: Foo<Bar>[]",
        "type Foo = Bar; mod user; pub(crate) mod user ;",
        "",
    ];

    /// Every receiver-type pattern, split (`Affix`) against the original
    /// regex with the receiver formatted in: identical match sequences on
    /// lines carrying each language's shapes plus repeats, ties and
    /// non-ASCII whitespace.
    #[test]
    fn affix_receiver_patterns_match_the_formatted_regexes() {
        let originals: &[(&str, &[&str])] = &[
            ("typescript", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*new\s+([A-Za-z_$][A-Za-z0-9_.$]*)",
                r"(?-u:\b)R(?-u:\b)\s*:\s*([A-Z][A-Za-z0-9_.$]*)",
            ]),
            ("python", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(",
                r#"(?-u:\b)R(?-u:\b)\s*:\s*["']([A-Z][A-Za-z0-9_.]*)["']"#,
                r"(?-u:\b)R(?-u:\b)\s*:\s*([A-Z][A-Za-z0-9_.]*)",
            ]),
            ("java", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*new\s+([A-Za-z_][A-Za-z0-9_.]*)",
                r"(?-u:\b)([A-Z][A-Za-z0-9_.]*)\s+R(?-u:\b)\s*[=;,:)]",
            ]),
            ("kotlin", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(",
                r"(?-u:\b)R(?-u:\b)\s*:\s*([A-Z][A-Za-z0-9_.]*)",
            ]),
            ("rust", &[
                r"(?-u:\b)let\s+(?:mut\s+)?R(?-u:\b)(?:\s*:[^=]+)?=\s*&?(?:mut\s+)?([A-Z][A-Za-z0-9_]*)",
                r"(?-u:\b)R\s*:\s*&?(?:mut\s+)?([A-Z][A-Za-z0-9_]*)",
            ]),
            ("go", &[
                r"(?-u:\b)R\s+\*?([a-z_][A-Za-z0-9_]*\.[A-Z][A-Za-z0-9_]*)(?:\s*[,)]|\s*$)",
                r"(?-u:\b)R(?-u:\b)\s*:=\s*&?([A-Za-z_][A-Za-z0-9_.]*)\s*\{",
                r"(?-u:\b)var\s+R\s+\*?([A-Za-z_][A-Za-z0-9_.]*)",
                r"(?-u:\b)R\s+\*?([A-Z][A-Za-z0-9_.]*)",
            ]),
            ("php", &[
                r"\$?R(?-u:\b)\s*=\s*new\s+([A-Za-z_\\][A-Za-z0-9_\\]*)",
                r"(?-u:\b)([A-Za-z_\\][A-Za-z0-9_\\]*)\s+&?\$R(?-u:\b)",
            ]),
            ("c", &[
                r"(?-u:\b)(?:(?:struct|union)\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\*?\s*(?-u:\b)R(?-u:\b)\s*(?:[,)=;]|\[)",
            ]),
            ("csharp", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*new\s+([A-Za-z_][A-Za-z0-9_.]*)",
                r"(?-u:\b)([A-Z][A-Za-z0-9_.]*)\s+R(?-u:\b)\s*[=;,)]",
            ]),
            ("swift", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(",
                r"(?-u:\b)R(?-u:\b)\s*:\s*([A-Z][A-Za-z0-9_.]*)",
            ]),
            ("ruby", &[r"(?-u:\b)R(?-u:\b)\s*=\s*([A-Z][A-Za-z0-9_:]*)\.new(?-u:\b)"]),
            ("scala", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*(?:new\s+)?([A-Z][A-Za-z0-9_.]*)",
                r"(?-u:\b)R(?-u:\b)\s*:\s*([A-Z][A-Za-z0-9_.]*)",
            ]),
            ("dart", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(",
                r"(?-u:\b)([A-Z][A-Za-z0-9_.]*)\s+R(?-u:\b)\s*[=;,)]",
            ]),
            ("lua", &[
                r"(?-u:\b)R(?-u:\b)\s*=\s*([A-Z][A-Za-z0-9_]*)\.new(?-u:\b)",
                r"(?-u:\b)R(?-u:\b)\s*=\s*([A-Z][A-Za-z0-9_]*)\s*\(",
                r"(?-u:\b)R(?-u:\b)\s*:\s*([A-Z][A-Za-z0-9_.]*)",
            ]),
            ("r", &[r"(?-u:\b)R(?-u:\b)\s*(?:<-|<<-|=)\s*([A-Z][A-Za-z0-9_.]*)\$new(?-u:\b)"]),
        ];
        let php_property: &[&str] = &[
            r"(?-u:\b)(?:(?:private|protected|public|readonly|static|final)(?:\(set\))?\s+)+\??([A-Za-z_\\][A-Za-z0-9_\\]*)\s+&?\$R(?-u:\b)",
            r"\$this->R(?-u:\b)\s*=\s*new\s+([A-Za-z_\\][A-Za-z0-9_\\]*)",
        ];
        let mut cases = 0usize;
        let mut check = |label: &str, pats: &[&str], affixes: &'static [ReceiverPattern]| {
            assert_eq!(affixes.len(), pats.len(), "{label}");
            for (pat, rp) in pats.iter().zip(affixes) {
                for recv in RECEIVERS {
                    for line in LINES {
                        assert_eq!(
                            affix_matches(&rp.affix, line, recv),
                            regex_matches(pat, line, recv),
                            "{label} {pat:?} recv={recv:?} line={line:?}"
                        );
                        cases += 1;
                    }
                }
            }
        };
        for (lang, pats) in originals {
            check(lang, pats, local_receiver_type_patterns(lang));
        }
        check("php property", php_property, &PHP_PROPERTY_TYPE_PATTERNS);
        assert!(cases > 3000);
    }

    /// The other split sites — declaration and member shapes — against
    /// their original patterns.
    #[test]
    fn affix_site_patterns_match_the_formatted_regexes() {
        let sites: &'static [(&str, Affix)] = Vec::leak(vec![
            (r"(?-u:\b)(?:const|let|var)\s+R\s*=", Affix::new(r"(?-u:\b)(?:const|let|var)\s+", r"\s*=", false, false, false)),
            (r"(?-u:\b)(?:const|let|var)\s+R\s*(=[\s\S]+)", Affix::new(r"(?-u:\b)(?:const|let|var)\s+", r"\s*(=[\s\S]+)", false, false, false)),
            (
                r"(?-u:\b)(?:const|let|var)\s+R\s*=\s*await\s+[A-Za-z0-9_$]+\s*\(",
                Affix::new(r"(?-u:\b)(?:const|let|var)\s+", r"\s*=\s*await\s+[A-Za-z0-9_$]+\s*\(", false, false, false),
            ),
            (r"[{,]\s*R\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[,}]", Affix::new(r"[{,]\s*", r"\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[,}]", false, false, false)),
            (r"(?-u:\b)type\s+R\s*=\s*([^;]+);", Affix::new(r"(?-u:\b)type\s+", r"\s*=\s*([^;]+);", false, false, false)),
            (r"(?-u:\b)R\s*:\s*([^,{}]+)", Affix::new("", r"\s*:\s*([^,{}]+)", true, false, false).lead(b":")),
            (r"(?-u:\b)R(?-u:\b)\s*=\s*([^;]+)", Affix::new("", r"\s*=\s*([^;]+)", true, true, false).lead(b"=")),
            (r"(?-u:\b)R\s+\*?([A-Za-z0-9_.]+)(?:\s*[,)]|\s*$)", Affix::new("", r"\s+\*?([A-Za-z0-9_.]+)(?:\s*[,)]|\s*$)", true, false, false)),
            (r"(?-u:\b)R\s+\*?\[?\]?([A-Za-z_][A-Za-z0-9_.]*)", Affix::new("", r"\s+\*?\[?\]?([A-Za-z_][A-Za-z0-9_.]*)", true, false, false)),
            (r"^\s*(?:pub\s*(?:\([^)]*\))?\s+)?mod\s+R\s*;", Affix::new(r"^\s*(?:pub\s*(?:\([^)]*\))?\s+)?mod\s+", r"\s*;", false, false, false)),
            (r"\$this->R(?-u:\b)\s*=\s*\$([A-Za-z0-9_]+)(?-u:\b)", Affix::new(r"\$this->", r"\s*=\s*\$([A-Za-z0-9_]+)(?-u:\b)", false, true, false)),
            (
                r"(?-u:\b)R(?-u:\b)\s*[?!]?\s*:\s*(?:readonly\s+)?typeof\s+([A-Za-z_$][A-Za-z0-9_.$]*)",
                Affix::new("", r"\s*[?!]?\s*:\s*(?:readonly\s+)?typeof\s+([A-Za-z_$][A-Za-z0-9_.$]*)", true, true, false).lead(b"?!:"),
            ),
            (
                r"(?-u:\b)R(?-u:\b)\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Za-z_$][A-Za-z0-9_.$]*)",
                Affix::new("", r"\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Za-z_$][A-Za-z0-9_.$]*)", true, true, false).lead(b"?!:"),
            ),
        ]);
        let extra = [
            "const user = await fetchUser(id);",
            "{ user: makeUser, x }",
            "{user,x}",
            "$this->user = $repo; $this->user = new Foo",
            "var user = (a, b) => a + b\n  , x = 1",
            "let  user  =  user  ",
            "  private user?: typeof Store; readonly x!: Foo.Bar<T>[]",
        ];
        for (pat, affix) in sites {
            for word in RECEIVERS {
                for line in LINES.iter().chain(extra.iter()) {
                    assert_eq!(affix_matches(affix, line, word), regex_matches(pat, line, word), "{pat:?} word={word:?} line={line:?}");
                }
            }
        }
    }

    /// `js_destructure_names` against the pattern it implements, with ASCII
    /// word boundaries: a `const {…}` run naming the ref.
    #[test]
    fn js_destructure_names_matches_the_regex() {
        let names = ["a", "user", "über", "b$"];
        let texts = [
            "const user = 1;",
            "constuser = 2",
            "const {a, user} = X.getState();",
            "const {\n  a,\n  user\n} = s;",
            "const {a: {user}} = x",
            "let user = 1; const b$ = 2",
            "x.const user",
            "const\u{a0}user",
            "const { über } = 1",
            "const {a} = {user}",
            "aconst user",
            "const {} user",
            "const user_1 = 0",
            "const {a} = 1; const {b, name} = x; const {\n c: user } = y",
            "xconst user; const\n{ user }",
            "",
        ];
        for name in names {
            // The oracle: the pattern the function replaced, per name.
            let pattern = format!(
                r"(?-u:\b)const\s*\{{[^{{}}]*(?-u:\b){0}(?-u:\b)",
                regex::escape(name)
            );
            let re = Regex::new(&pattern).unwrap();
            for text in texts {
                assert_eq!(js_destructure_names(text, name), re.is_match(text), "name={name:?} text={text:?}");
            }
        }
    }
}
