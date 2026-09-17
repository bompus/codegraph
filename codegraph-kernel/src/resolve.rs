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
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Tables — ports of the TS sets in src/resolution/{index,name-matcher,
// import-resolver}.ts.
// ---------------------------------------------------------------------------

/// BINDINGS_LANGUAGES (src/extraction/kernel/index.ts): languages whose
/// extractors emit `bindings` rows.
fn is_migrated_language(lang: &str) -> bool {
    matches!(
        lang,
        "typescript"
            | "tsx"
            | "javascript"
            | "jsx"
            | "arkts"
            | "python"
            | "go"
            | "java"
            | "kotlin"
            | "php"
            | "c"
            | "cpp"
    )
}

// ---------------------------------------------------------------------------
// Framework `claimsReference` predicates (src/resolution/frameworks/*.ts).
// Each is a pure name-shape function — no context — so the kernel can
// evaluate the prefilter's last arm natively instead of deferring every
// prefilter miss to TypeScript whenever ANY framework is detected. JS `\w`
// is ASCII without /u, so the ports spell it `[A-Za-z0-9_]`. Registered
// resolvers without a claimsReference claim nothing; an unlisted name
// (a custom registerFrameworkResolver) claims everything — a conservative
// passthrough, never a wrong verdict.
// ---------------------------------------------------------------------------

static DRUPAL_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*::?[A-Za-z0-9_]+$").unwrap());
static EXPO_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\.)(?:push|replace|navigate|dismissTo)$|^[a-z][A-Za-z]*(?:Push|Replace|Navigate)$")
        .unwrap()
});
static LARAVEL_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*Controller@[A-Za-z0-9_]+$").unwrap());
static NEXT_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\.)(?:push|replace|prefetch)$|^(?:redirect|permanentRedirect)$|^(?:NextResponse|Response)\.redirect$").unwrap()
});
static PLAY_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*\.[A-Za-z_][A-Za-z0-9_]*$").unwrap());
static RR_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:history|navigate|router)\.(?:push|replace|navigate)$|^(?:navigate|redirect)$")
        .unwrap()
});
static RAILS_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_/]+#[A-Za-z0-9_]+$").unwrap());
static TANSTACK_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:navigate|redirect)$|^(?:router|Route)\.navigate$").unwrap()
});
static TERRA_CLAIM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^module\.[^.:\s]+:(?:file$|var\.|output\.|remote-output\.)").unwrap()
});
static VUE_NAV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$?router\.(?:push|replace)$|^navigateTo$").unwrap());

/// `f.claimsReference(name)` for the resolver registered as `framework`
/// (`f.name`), or the conservative claim for names the kernel does not know.
fn framework_claims_reference(framework: &str, name: &str) -> bool {
    match framework {
        "cics" => name.starts_with("cics-transid:"),
        "django" => name == "_iterable_class" || name.ends_with(".urls"),
        "drupal" => {
            name.starts_with("hook_") || name.contains('\\') || DRUPAL_CLAIM_RE.is_match(name)
        }
        "expo-router" => EXPO_NAV_RE.is_match(name),
        "laravel" => LARAVEL_CLAIM_RE.is_match(name),
        "nextjs" => NEXT_NAV_RE.is_match(name),
        "play" => PLAY_CLAIM_RE.is_match(name),
        // react-native-bridge's claimsReference returns false — JS-visible
        // method names reach the resolver through the name-exists arm.
        "react-native-bridge" => false,
        "react-router" => RR_NAV_RE.is_match(name),
        "rails" => RAILS_CLAIM_RE.is_match(name),
        "spring" => name.ends_with(":prefix"),
        "sveltekit-router" => matches!(name, "goto" | "redirect"),
        "swift-objc-bridge" => name.contains(':'),
        "tanstack-router" => TANSTACK_NAV_RE.is_match(name),
        "terraform" => TERRA_CLAIM_RE.is_match(name),
        "vue-router" => VUE_NAV_RE.is_match(name),
        "aspnet" | "astro" | "express" | "expo-modules" | "fabric-view" | "fastapi" | "flask"
        | "go" | "goframe" | "nestjs" | "react" | "rust" | "svelte" | "swiftui" | "uikit"
        | "vapor" | "vue" => false,
        _ => true,
    }
}

/// Diagnostic/differential probe: `f.claimsReference(name)` for the resolver
/// registered as `framework` — the same table `framework_claims` iterates,
/// including the conservative claim for names the kernel does not know.
#[napi]
pub fn framework_claims_name(framework: String, name: String) -> bool {
    framework_claims_reference(&framework, &name)
}

/// LANGUAGE_FAMILY (name-matcher.ts).
fn language_family(lang: &str) -> Option<&'static str> {
    match lang {
        "java" | "kotlin" | "scala" => Some("jvm"),
        "swift" | "objc" => Some("apple"),
        "typescript" | "tsx" | "javascript" | "jsx" | "arkts" => Some("web"),
        "c" | "cpp" => Some("c"),
        "csharp" | "razor" => Some("dotnet"),
        _ => None,
    }
}

fn same_language_family(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    matches!(language_family(a), Some(fa) if Some(fa) == language_family(b))
}

fn is_known_language_family(lang: &str) -> bool {
    language_family(lang).is_some()
}

fn crosses_known_family(a: &str, b: &str) -> bool {
    is_known_language_family(a) && is_known_language_family(b) && !same_language_family(a, b)
}

/// ESM_FAMILY (name-matcher.ts).
fn is_esm_family(lang: &str) -> bool {
    matches!(lang, "typescript" | "tsx" | "javascript" | "jsx" | "arkts")
}

/// ESM_IMPORT_LANGUAGES (import-resolver.ts): imports are ES specifiers.
fn is_esm_import_language(lang: &str) -> bool {
    matches!(
        lang,
        "typescript" | "tsx" | "javascript" | "jsx" | "arkts" | "svelte" | "vue" | "astro"
    )
}

/// SUPERTYPE_TARGET_KINDS (resolution/types.ts).
fn is_supertype_target_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class"
            | "struct"
            | "interface"
            | "trait"
            | "protocol"
            | "enum"
            | "union"
            | "type_alias"
            | "component"
            | "module"
            | "namespace"
    )
}

/// isInheritanceRef (resolution/types.ts).
fn is_inheritance_ref(kind: &str) -> bool {
    kind == "extends" || kind == "implements"
}

/// isImportableKind (resolution/types.ts).
fn is_importable_kind(kind: &str) -> bool {
    !matches!(kind, "property" | "field" | "method" | "enum_member" | "parameter")
}

/// PRIVATE_IS_FILE_LOCAL (name-matcher.ts).
fn private_is_file_local(lang: &str) -> bool {
    matches!(lang, "kotlin" | "java" | "csharp" | "swift" | "scala" | "dart" | "php")
}

/// NO_NESTED_FUNCTIONS (name-matcher.ts).
fn no_nested_functions(lang: &str) -> bool {
    lang == "c" || lang == "cpp"
}

/// JS_FAMILY (name-matcher.ts): the bare-call method guard applies to these.
fn is_js_family(lang: &str) -> bool {
    matches!(lang, "typescript" | "tsx" | "javascript" | "jsx")
}

/// BARE_CALL_TARGET_KINDS (name-matcher.ts).
fn is_bare_call_target_kind(kind: &str) -> bool {
    matches!(kind, "function" | "class" | "component" | "constant" | "variable")
}

/// CPP_ADL_RANGE_NAMES (name-matcher.ts).
fn is_cpp_adl_name(name: &str) -> bool {
    matches!(name, "begin" | "end" | "rbegin" | "rend" | "cbegin" | "cend")
}

/// DEFAULT_BINDING_KINDS (import-resolver.ts).
fn is_default_binding_kind(kind: &str) -> bool {
    matches!(kind, "function" | "class" | "component" | "constant" | "variable")
}

/// ALIAS_BINDING_KINDS (alias-binding.ts).
fn is_alias_binding_kind(kind: &str) -> bool {
    matches!(kind, "constant" | "variable" | "property")
}

/// CALLABLE_KINDS (alias-binding.ts).
fn is_callable_kind(kind: &str) -> bool {
    matches!(kind, "function" | "method" | "class" | "component")
}

/// EXTENSION_RESOLUTION (import-resolver.ts).
fn extension_resolution(language: &str) -> &'static [&'static str] {
    match language {
        "typescript" => &[
            ".ts", ".tsx", ".d.ts", ".js", ".jsx", "/index.ts", "/index.tsx", "/index.js",
        ],
        "arkts" => &[
            ".ets", ".ts", ".d.ts", ".js", "/Index.ets", "/index.ets", "/index.ts", "/index.js",
        ],
        "javascript" => &[
            ".js", ".jsx", ".mjs", ".cjs", ".xsjs", ".xsjslib", "/index.js", "/index.jsx",
        ],
        "tsx" => &[
            ".tsx", ".ts", ".d.ts", ".js", ".jsx", "/index.tsx", "/index.ts", "/index.js",
        ],
        "jsx" => &[".jsx", ".js", "/index.jsx", "/index.js"],
        "svelte" => &[
            ".ts", ".js", ".svelte", ".tsx", ".jsx", "/index.ts", "/index.js", "/index.svelte",
        ],
        "vue" => &[
            ".ts", ".js", ".vue", ".tsx", ".jsx", "/index.ts", "/index.js", "/index.vue",
        ],
        "astro" => &[
            ".ts", ".js", ".astro", ".tsx", ".jsx", "/index.ts", "/index.js", "/index.astro",
        ],
        "python" => &[".py", "/__init__.py"],
        "go" => &[".go"],
        "rust" => &[".rs", "/mod.rs"],
        "java" => &[".java"],
        "c" => &[".h", ".c"],
        "cpp" => &[".h", ".hpp", ".hxx", ".cpp", ".cc", ".cxx"],
        "csharp" => &[".cs"],
        "php" => &[".php"],
        "ruby" => &[".rb"],
        "objc" => &[".h", ".m", ".mm"],
        "nix" => &[".nix", "/default.nix"],
        _ => &[],
    }
}

/// EMITTED_TO_SOURCE_EXTENSIONS gated by EMITTED_SPECIFIER_LANGUAGES
/// (import-resolver.ts): `.js`→`.ts` remap for node16/nodenext/bundler
/// specifiers.
fn emitted_to_source(rel: &str, language: &str) -> Option<&'static [&'static str]> {
    if !matches!(
        language,
        "typescript" | "tsx" | "javascript" | "jsx" | "vue" | "svelte" | "astro" | "arkts"
    ) {
        return None;
    }
    if rel.ends_with(".js") {
        Some(&[".ts", ".tsx", ".d.ts"])
    } else if rel.ends_with(".jsx") {
        Some(&[".tsx"])
    } else if rel.ends_with(".mjs") {
        Some(&[".mts", ".d.mts"])
    } else if rel.ends_with(".cjs") {
        Some(&[".cts", ".d.cts"])
    } else {
        None
    }
}

/// The 12 Node built-ins isExternalImport special-cases for ESM languages
/// (import-resolver.ts) — deliberately the SHORT list, not builtinModules.
const ESM_BUILTIN_MODULES: &[&str] = &[
    "fs", "path", "os", "crypto", "http", "https", "url", "util", "events", "stream",
    "child_process", "buffer",
];

/// The stdlib head check isExternalImport applies to Python imports.
const PYTHON_STDLIB_HEADS: &[&str] = &[
    "os", "sys", "json", "re", "math", "datetime", "collections", "typing", "pathlib", "logging",
];

/// The hardcoded alias fallback map (import-resolver.ts resolveAliasedImport
/// step 2): prefix → replacement, tried in declaration order.
const FALLBACK_ALIASES: &[(&str, &str)] = &[
    ("@/", "src/"),
    ("~/", "src/"),
    ("@src/", "src/"),
    ("src/", "src/"),
    ("@app/", "app/"),
    ("app/", "app/"),
];

/// MARKDOWN_PATH_REF (resolution/index.ts isBuiltInOrExternal).
static MARKDOWN_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?i:md|markdown)(?:$|[#?]|::)").unwrap());

/// C_SOURCE_EXT (name-matcher.ts).
static C_SOURCE_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?i:c|cc|cpp|cxx|c\+\+|m|mm)$").unwrap());

/// BARE_ALIAS_RE (alias-binding.ts): `= foo` / `= foo as T` / `= foo;`.
static BARE_ALIAS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^=\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:as\s+[A-Za-z0-9_.<>\[\]]+\s*)?;?$").unwrap()
});

static IMPL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(pub(\([^)]*\))?\s+)?(unsafe\s+)?impl\b").unwrap()
});
static IMPL_FOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\sfor\s").unwrap());
static ITEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(pub(\([^)]*\))?\s+)?(fn|struct|enum|mod|trait|const|static|type)\b").unwrap()
});
static JS_CALL_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.A-Za-z0-9_$\]\)]\s*$").unwrap());
static JS_CALL_KEYWORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:return|await|yield|typeof|void|new|else|case|throw|in|of|instanceof)\s*$")
        .unwrap()
});
static CPP_THIS_DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^A-Za-z0-9_])this\.$").unwrap());
static CPP_THIS_ARROW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^A-Za-z0-9_])this->$").unwrap());
static AFTER_NAME_PAREN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\)").unwrap());

// ---------------------------------------------------------------------------
// Row types (mirror src/types.ts Node + db/schema.sql bindings).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct KNode {
    id: String,
    kind: String,
    name: String,
    qualified_name: String,
    file_path: String,
    language: String,
    start_line: i64,
    end_line: i64,
    start_column: i64,
    #[allow(dead_code)]
    end_column: i64,
    signature: Option<String>,
    visibility: Option<String>,
    is_exported: bool,
}

impl KNode {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(KNode {
            id: row.get("id")?,
            kind: row.get("kind")?,
            name: row.get("name")?,
            qualified_name: row.get("qualified_name")?,
            file_path: row.get("file_path")?,
            language: row.get("language")?,
            start_line: row.get("start_line")?,
            end_line: row.get("end_line")?,
            start_column: row.get("start_column")?,
            end_column: row.get("end_column")?,
            signature: row.get("signature")?,
            visibility: row.get("visibility")?,
            is_exported: row.get::<_, i64>("is_exported")? != 0,
        })
    }
}

#[derive(Clone, Debug)]
struct KBinding {
    name: String,
    kind: String,
    node_id: Option<String>,
    target_spec: Option<String>,
    target_name: Option<String>,
    exported_as: Option<String>,
    #[allow(dead_code)]
    export_form: Option<String>,
    scope_start: i64,
    scope_end: i64,
    storage: Option<String>,
    line: i64,
}

/// ImportMapping (import-resolver.ts).
#[derive(Clone, Debug)]
struct KImport {
    local_name: String,
    exported_name: String,
    source: String,
    is_default: bool,
    is_namespace: bool,
}

/// ReExport (import-resolver.ts).
#[derive(Clone, Debug)]
struct KReExport {
    kind: &'static str, // 'named' | 'wildcard'
    exported_name: Option<String>,
    original_name: Option<String>,
    source: String,
}

/// FileExportIndex (import-resolver.ts): per-file export lookup —
/// first-wins `by_name` over `isExported` nodes plus binding-row aliases,
/// and the three default-export guesses. (The TS `declared` map is dead
/// code there and not ported.)
#[derive(Debug)]
struct FileExportIndexK {
    by_name: HashMap<String, KNode>,
    default_component: Option<KNode>,
    default_fn_class: Option<KNode>,
    default_binding: Option<KNode>,
}

/// AliasPattern (project-aliases.ts).
#[derive(Clone, Debug)]
struct AliasPatternK {
    prefix: String,
    suffix: String,
    has_wildcard: bool,
    replacements: Vec<String>,
}



/// AliasMap (project-aliases.ts).
#[derive(Clone, Debug)]
struct AliasMapK {
    base_url: Option<String>,
    patterns: Vec<AliasPatternK>,
}

/// WorkspacePackages (workspace-packages.ts).
#[derive(Clone, Debug, Default)]
struct WorkspaceK {
    source_entries: Vec<(String, String)>,
    by_name: HashMap<String, String>,
    entry_by_name: Option<HashMap<String, String>>,
    local_link_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Path helpers — DB paths are always '/'-joined relative; project_root is
// absolute. On the supported platforms path == path.posix.
// ---------------------------------------------------------------------------

fn pos_dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[..i],
        None => "",
    }
}

fn pos_basename(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

/// path.posix.normalize semantics for a joined path: collapse `.`, `..`,
/// duplicate slashes. `keep_relative` mirrors normalize on relative inputs
/// (leading `..` segments are preserved).
fn pos_normalize(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{}", joined)
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// path.resolve(dir, p): join + normalize; absolute `p` wins.
fn pos_resolve(dir: &str, p: &str) -> String {
    if p.starts_with('/') {
        pos_normalize(p)
    } else {
        pos_normalize(&format!("{}/{}", dir.trim_end_matches('/'), p))
    }
}

/// path.relative(root, abs): `abs` must be inside `root` in every call site
/// that reaches here; return the forward-slash suffix, "" when equal.
fn pos_relative(root: &str, abs: &str) -> String {
    let root = root.trim_end_matches('/');
    if abs == root {
        return String::new();
    }
    if let Some(rest) = abs.strip_prefix(root) {
        if let Some(rest) = rest.strip_prefix('/') {
            return rest.to_string();
        }
    }
    // Fallback: mirror path.relative by counting root segments — never hit in
    // practice because callers gate on containment.
    let mut root_dir = root;
    let mut ups = String::new();
    while !abs.starts_with(&format!("{}/", root_dir)) && abs != root_dir {
        match pos_dirname(root_dir) {
            "" => break,
            d => {
                root_dir = d;
                ups.push_str("../");
            }
        }
    }
    format!("{}{}", ups, abs.strip_prefix(root_dir).unwrap_or(abs).trim_start_matches('/'))
}

fn is_within_dir(root_abs: &str, target_abs: &str) -> bool {
    target_abs == root_abs || target_abs.starts_with(&format!("{}/", root_abs.trim_end_matches('/')))
}

/// lexicalPathWithinRoot (sync/scanners/files.ts).
fn lexical_path_within_root(root_abs: &str, rel: &str) -> bool {
    let resolved = pos_resolve(root_abs, rel);
    is_within_dir(root_abs, &resolved)
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
}

/// One unresolved_refs row — mirrors UnresolvedReference/rowId shape so the
/// TS side can feed passthrough refs straight into the existing pipeline.
#[napi(object)]
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
    node: Rc<KNode>,
    confidence: f64,
    resolved_by: &'static str,
}

// ---------------------------------------------------------------------------
// Tiny LRU for file contents (queries.fileCache equivalent).
// ---------------------------------------------------------------------------

struct FileCache {
    map: HashMap<String, Option<Rc<Vec<String>>>>,
    order: VecDeque<String>,
    cap: usize,
}

impl FileCache {
    fn new(cap: usize) -> Self {
        FileCache { map: HashMap::new(), order: VecDeque::new(), cap }
    }
    fn get(&self, k: &str) -> Option<&Option<Rc<Vec<String>>>> {
        self.map.get(k)
    }
    fn put(&mut self, k: String, v: Option<Rc<Vec<String>>>) {
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

#[napi]
pub struct KernelResolver {
    // Option so close() can drop the Connection deterministically — see
    // close() for why GC-timed teardown is unsafe on a shared -shm.
    conn: Option<Connection>,
    project_root: String,
    root_abs: String,
    aliases: Option<AliasMapK>,
    workspaces: Option<WorkspaceK>,
    go_module_path: Option<String>,
    cpp_include_dirs: Vec<String>,
    node_builtins: HashSet<String>,
    frameworks_active: bool,
    framework_names: Option<Vec<String>>,
    ambiguous_ceiling: i64,

    known_names: HashSet<String>,
    known_files: HashSet<String>,

    name_cache: HashMap<String, Rc<Vec<KNode>>>,
    lower_cache: HashMap<String, Rc<Vec<KNode>>>,
    qname_cache: HashMap<String, Rc<Vec<KNode>>>,
    file_nodes: HashMap<String, Rc<Vec<KNode>>>,
    node_by_id: HashMap<String, Option<Rc<KNode>>>,
    bindings_cache: HashMap<String, Rc<Vec<KBinding>>>,
    import_map_cache: HashMap<String, Rc<Vec<KImport>>>,
    reexport_cache: HashMap<String, Rc<Vec<KReExport>>>,
    export_index: HashMap<String, Option<Rc<FileExportIndexK>>>,
    import_path_memo: HashMap<String, Option<String>>,
    exported_symbol_memo: HashMap<String, Option<Rc<KNode>>>,
    sealed_memo: HashMap<String, bool>,
    c_static_memo: HashMap<String, bool>,
    rust_trait_memo: HashMap<String, bool>,
    root_import_memo: HashMap<String, bool>,
    file_cache: FileCache,
    regex_cache: HashMap<String, Rc<Regex>>,
}

#[napi]
impl KernelResolver {
    #[napi(constructor)]
    pub fn new(config: KernelResolverConfig) -> Result<Self> {
        let conn = Connection::open_with_flags(&config.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
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

        let mut r = KernelResolver {
            conn: Some(conn),
            project_root: config.project_root,
            root_abs,
            aliases: config.aliases.map(|a| AliasMapK {
                base_url: a.base_url,
                patterns: a
                    .patterns
                    .into_iter()
                    .map(|p| AliasPatternK {
                        prefix: p.prefix,
                        suffix: p.suffix,
                        has_wildcard: p.has_wildcard,
                        replacements: p.replacements,
                    })
                    .collect(),
            }),
            workspaces,
            go_module_path: config.go_module_path,
            cpp_include_dirs,
            node_builtins: config.node_builtin_specifiers.into_iter().collect(),
            frameworks_active: config.frameworks_active,
            framework_names: config.framework_names,
            ambiguous_ceiling: config.ambiguous_name_ceiling.unwrap_or(500) as i64,
            known_names: HashSet::new(),
            known_files: HashSet::new(),
            name_cache: HashMap::new(),
            lower_cache: HashMap::new(),
            qname_cache: HashMap::new(),
            file_nodes: HashMap::new(),
            node_by_id: HashMap::new(),
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
            file_cache: FileCache::new(1024),
            regex_cache: HashMap::new(),
        };
        r.warm_caches()?;
        Ok(r)
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
        self.conn.take();
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
            out.push(self.resolve_ref(&r)?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Query layer — every statement reproduces the TypeScript ORDER BY verbatim
// (src/db/queries.ts): candidate order is observable through findBestMatch's
// first-max scoring.
// ---------------------------------------------------------------------------

const NODE_COLS: &str = "id, kind, name, qualified_name, file_path, language, \
                         start_line, end_line, start_column, end_column, \
                         signature, visibility, is_exported";

impl KernelResolver {
    /// The live connection — errors once close() has run. Statements borrow
    /// it, so the Option indirection stays inside this accessor.
    fn conn(&self) -> Result<&Connection> {
        self.conn
            .as_ref()
            .ok_or_else(|| Error::from_reason("KernelResolver is closed"))
    }

    /// warmCaches (ReferenceResolver): the known-file and known-symbol sets.
    fn warm_caches(&mut self) -> Result<()> {
        // Field-level borrow (not self.conn()) so the statement and the
        // cache-field writes below stay disjoint borrows.
        let conn = self
            .conn
            .as_ref()
            .ok_or_else(|| Error::from_reason("KernelResolver is closed"))?;
        let mut stmt = conn
            .prepare("SELECT path FROM files")
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut files = Vec::new();
        for p in rows {
            files.push(p.map_err(|e| Error::from_reason(e.to_string()))?);
        }
        self.known_files.extend(files);
        // getAllNodeNames: `SELECT DISTINCT name FROM nodes` — no kind filter.
        let mut stmt = conn
            .prepare("SELECT DISTINCT name FROM nodes")
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut names = Vec::new();
        for n in rows {
            names.push(n.map_err(|e| Error::from_reason(e.to_string()))?);
        }
        self.known_names.extend(names);
        Ok(())
    }

    fn query_nodes(&self, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<KNode>> {
        let mut stmt = self
            .conn()?
            .prepare(sql)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let rows = stmt
            .query_map(params, KNode::from_row)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut out = Vec::new();
        for n in rows {
            out.push(n.map_err(|e| Error::from_reason(e.to_string()))?);
        }
        Ok(out)
    }

    /// queries.getNodesByName — ORDER BY file_path, start_line.
    fn nodes_by_name(&mut self, name: &str) -> Result<Rc<Vec<KNode>>> {
        if let Some(v) = self.name_cache.get(name) {
            return Ok(v.clone());
        }
        let sql = format!("SELECT {} FROM nodes WHERE name = ?1 ORDER BY file_path, start_line", NODE_COLS);
        let v = Rc::new(self.query_nodes(&sql, &[&name])?);
        self.name_cache.insert(name.to_string(), v.clone());
        Ok(v)
    }

    /// queries.getNodesByLowerName — `WHERE lower(name) = lower(?)`, no
    /// ORDER BY (rowid order, same as the TS reader).
    fn nodes_by_lower_name(&mut self, name: &str) -> Result<Rc<Vec<KNode>>> {
        if let Some(v) = self.lower_cache.get(name) {
            return Ok(v.clone());
        }
        let sql = format!(
            "SELECT {} FROM nodes WHERE lower(name) = lower(?1)",
            NODE_COLS
        );
        let v = Rc::new(self.query_nodes(&sql, &[&name])?);
        self.lower_cache.insert(name.to_string(), v.clone());
        Ok(v)
    }

    /// queries.getNodesByQualifiedName — no ORDER BY (TS uses rowid order).
    fn nodes_by_qualified_name(&mut self, qname: &str) -> Result<Rc<Vec<KNode>>> {
        if let Some(v) = self.qname_cache.get(qname) {
            return Ok(v.clone());
        }
        let sql = format!("SELECT {} FROM nodes WHERE qualified_name = ?1", NODE_COLS);
        let v = Rc::new(self.query_nodes(&sql, &[&qname])?);
        self.qname_cache.insert(qname.to_string(), v.clone());
        Ok(v)
    }

    /// queries.getNodesInFile — ORDER BY start_line.
    fn nodes_in_file(&mut self, file_path: &str) -> Result<Rc<Vec<KNode>>> {
        if let Some(v) = self.file_nodes.get(file_path) {
            return Ok(v.clone());
        }
        let sql = format!("SELECT {} FROM nodes WHERE file_path = ?1 ORDER BY start_line", NODE_COLS);
        let v = Rc::new(self.query_nodes(&sql, &[&file_path])?);
        self.file_nodes.insert(file_path.to_string(), v.clone());
        Ok(v)
    }

    /// queries.getNodeById.
    fn node_by_id(&mut self, id: &str) -> Result<Option<Rc<KNode>>> {
        if let Some(v) = self.node_by_id.get(id) {
            return Ok(v.clone());
        }
        let sql = format!("SELECT {} FROM nodes WHERE id = ?1", NODE_COLS);
        let v = self.query_nodes(&sql, &[&id])?.into_iter().next().map(Rc::new);
        self.node_by_id.insert(id.to_string(), v.clone());
        Ok(v)
    }

    /// queries.getBindings — ORDER BY rowid.
    fn bindings(&mut self, file_path: &str) -> Result<Rc<Vec<KBinding>>> {
        if let Some(v) = self.bindings_cache.get(file_path) {
            return Ok(v.clone());
        }
        let mut stmt = self
            .conn()?
            .prepare(
                "SELECT file_path, name, kind, node_id, target_spec, target_name, \
                 exported_as, export_form, scope_start, scope_end, storage, line \
                 FROM bindings WHERE file_path = ?1 ORDER BY rowid",
            )
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok(KBinding {
                    name: row.get("name")?,
                    kind: row.get("kind")?,
                    node_id: row.get("node_id")?,
                    target_spec: row.get("target_spec")?,
                    target_name: row.get("target_name")?,
                    exported_as: row.get("exported_as")?,
                    export_form: row.get("export_form")?,
                    scope_start: row.get("scope_start")?,
                    scope_end: row.get("scope_end")?,
                    storage: row.get("storage")?,
                    line: row.get("line")?,
                })
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut v = Vec::new();
        for b in rows {
            v.push(b.map_err(|e| Error::from_reason(e.to_string()))?);
        }
        drop(stmt); // release the self.conn borrow before the cache write
        let v = Rc::new(v);
        self.bindings_cache.insert(file_path.to_string(), v.clone());
        Ok(v)
    }

    /// context.fileExists — the knownFiles set answers for the exact and the
    /// `\`-normalized spelling, then lexical containment + existsSync
    /// fallback for files not yet indexed (queries' per-candidate hot path).
    fn file_exists(&self, rel: &str) -> bool {
        let normalized = rel.replace('\\', "/");
        if self.known_files.contains(rel) || self.known_files.contains(normalized.as_str()) {
            return true;
        }
        lexical_path_within_root(&self.root_abs, rel)
            && std::fs::metadata(pos_resolve(&self.root_abs, rel)).is_ok()
    }

    /// context.getFileLines — `readFile` content split on /\r?\n/,
    /// LRU-cached (nulls cached too).
    fn read_file(&mut self, rel: &str) -> Option<Rc<Vec<String>>> {
        if let Some(v) = self.file_cache.get(rel) {
            return v.clone();
        }
        let v = std::fs::read_to_string(pos_resolve(&self.root_abs, rel))
            .ok()
            .map(|s| {
                let normalized = s.replace("\r\n", "\n");
                Rc::new(normalized.split('\n').map(|l| l.to_string()).collect::<Vec<String>>())
            });
        self.file_cache.put(rel.to_string(), v.clone());
        v
    }

    /// Compile-once-per-pattern regexes — the bare-call and store-bind
    /// matchers build name-parameterized patterns per ref, and identical
    /// names recur across thousands of refs.
    fn cached_regex(&mut self, pattern: &str) -> Result<Rc<Regex>> {
        if let Some(re) = self.regex_cache.get(pattern) {
            return Ok(re.clone());
        }
        let re = Rc::new(Regex::new(pattern).map_err(|e| Error::from_reason(e.to_string()))?);
        self.regex_cache.insert(pattern.to_string(), re.clone());
        Ok(re)
    }

    // -----------------------------------------------------------------------
    // Bindings → import mappings / re-exports (import-resolver.ts)
    // -----------------------------------------------------------------------

    /// importMappingsFromBindings. Returns None when the file has no binding
    /// rows — same as TS, where a null result falls through to
    /// extractImportMappings, which now always yields `[]`.
    fn import_mappings_from_bindings(&self, rows: &[KBinding]) -> Option<Vec<KImport>> {
        if rows.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for r in rows {
            if r.kind != "import" || r.target_spec.is_none() {
                continue;
            }
            let exported_name = r.target_name.clone().unwrap_or_else(|| r.name.clone());
            out.push(KImport {
                local_name: r.name.clone(),
                is_default: exported_name == "default",
                is_namespace: exported_name == "*",
                exported_name,
                source: r.target_spec.clone().unwrap(),
            });
        }
        Some(out)
    }

    /// getImportMappings equivalent — bindings-backed, no source fallback
    /// needed (extractImportMappings is a stub returning []).
    fn import_mappings(&mut self, file_path: &str) -> Result<Rc<Vec<KImport>>> {
        if let Some(v) = self.import_map_cache.get(file_path) {
            return Ok(v.clone());
        }
        let rows = self.bindings(file_path)?;
        let v = Rc::new(self.import_mappings_from_bindings(&rows).unwrap_or_default());
        self.import_map_cache.insert(file_path.to_string(), v.clone());
        Ok(v)
    }

    /// reExportsFromBindings.
    fn reexports(&mut self, file_path: &str) -> Result<Rc<Vec<KReExport>>> {
        if let Some(v) = self.reexport_cache.get(file_path) {
            return Ok(v.clone());
        }
        let rows = self.bindings(file_path)?;
        let mut out = Vec::new();
        for r in rows.iter() {
            if r.kind != "reexport" || r.target_spec.is_none() {
                continue;
            }
            if r.name == "*" {
                out.push(KReExport {
                    kind: "wildcard",
                    exported_name: None,
                    original_name: None,
                    source: r.target_spec.clone().unwrap(),
                });
            } else {
                out.push(KReExport {
                    kind: "named",
                    exported_name: Some(r.exported_as.clone().unwrap_or_else(|| r.name.clone())),
                    original_name: Some(r.name.clone()),
                    source: r.target_spec.clone().unwrap(),
                });
            }
        }
        let v = Rc::new(out);
        self.reexport_cache.insert(file_path.to_string(), v.clone());
        Ok(v)
    }

    /// defaultExportBinding (import-resolver.ts): the identifier
    /// `export default NAME` names, from the file's binding rows.
    fn default_export_binding(rows: &[KBinding]) -> Option<String> {
        rows.iter()
            .find(|r| r.exported_as.as_deref() == Some("default") && r.node_id.is_some())
            .map(|r| r.name.clone())
    }

    /// getFileExportIndex (import-resolver.ts).
    fn file_export_index(&mut self, file_path: &str) -> Result<Rc<FileExportIndexK>> {
        if let Some(Some(v)) = self.export_index.get(file_path) {
            return Ok(v.clone());
        }
        let nodes = self.nodes_in_file(file_path)?;
        let mut by_name: HashMap<String, KNode> = HashMap::new();
        let mut default_component: Option<KNode> = None;
        let mut default_fn_class: Option<KNode> = None;
        for n in nodes.iter() {
            if !n.is_exported {
                continue;
            }
            by_name.entry(n.name.clone()).or_insert_with(|| n.clone());
            if default_component.is_none() && n.kind == "component" {
                default_component = Some(n.clone());
            }
            if default_fn_class.is_none() && (n.kind == "function" || n.kind == "class") {
                default_fn_class = Some(n.clone());
            }
        }
        let rows = self.bindings(file_path)?;
        let mut default_binding: Option<KNode> = None;
        if let Some(bound) = Self::default_export_binding(&rows) {
            let mut candidates: Vec<&KNode> = nodes
                .iter()
                .filter(|n| n.name == bound && is_default_binding_kind(&n.kind))
                .collect();
            candidates.sort_by(|a, b| {
                a.start_line
                    .cmp(&b.start_line)
                    .then(a.start_column.cmp(&b.start_column))
            });
            default_binding = candidates.first().map(|n| (*n).clone());
        }
        // Local export clauses: `export { impl as alias }` binds the renamed
        // name to the real declaration.
        if !rows.is_empty() {
            let by_id: HashMap<&str, &KNode> =
                nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            for r in rows.iter() {
                let (Some(exported), Some(node_id)) = (r.exported_as.as_deref(), r.node_id.as_deref())
                else {
                    continue;
                };
                if by_name.contains_key(exported) {
                    continue;
                }
                if let Some(decl) = by_id.get(node_id) {
                    by_name.insert(exported.to_string(), (*decl).clone());
                }
            }
        }
        let idx = Rc::new(FileExportIndexK {
            by_name,
            default_component,
            default_fn_class,
            default_binding,
        });
        self.export_index.insert(file_path.to_string(), Some(idx.clone()));
        Ok(idx)
    }

    /// findExportedSymbol (import-resolver.ts) — bounded, cycle-safe export
    /// walk over direct exports, named re-exports and wildcard barrels.
    /// Top-level (fresh-visited) results are memoized like the TS WeakMap.
    fn find_exported_symbol(
        &mut self,
        file_path: &str,
        want: &ExportWant,
        language: &str,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> Result<Option<Rc<KNode>>> {
        const REEXPORT_MAX_DEPTH: usize = 8;
        if depth > REEXPORT_MAX_DEPTH {
            return Ok(None);
        }
        let fresh = depth == 0 && visited.is_empty();
        let memo_key = if fresh {
            Some(format!(
                "{}\0{}{}\0{}\0{}\0{}",
                file_path,
                if want.is_default { 1 } else { 0 },
                if want.is_namespace { 1 } else { 0 },
                want.exported_name,
                want.member_name.as_deref().unwrap_or(""),
                language
            ))
        } else {
            None
        };
        if let Some(k) = &memo_key {
            if let Some(hit) = self.exported_symbol_memo.get(k) {
                return Ok(hit.clone());
            }
        }
        if visited.contains(file_path) {
            return Ok(None);
        }
        visited.insert(file_path.to_string());

        let export_index = self.file_export_index(file_path)?;
        // 1. Direct hit.
        if want.is_default {
            let direct = export_index
                .default_component
                .clone()
                .or_else(|| export_index.default_binding.clone())
                .or_else(|| export_index.default_fn_class.clone());
            if let Some(d) = direct {
                return self.memo_symbol(memo_key, d);
            }
        } else if want.is_namespace && want.member_name.is_some() {
            if let Some(d) = export_index.by_name.get(want.member_name.as_deref().unwrap()) {
                return self.memo_symbol(memo_key, d.clone());
            }
        } else if let Some(d) = export_index.by_name.get(&want.exported_name) {
            return self.memo_symbol(memo_key, d.clone());
        }

        // Python module-level imports are public bindings — same walk.
        if language == "python" {
            let name = if want.is_namespace {
                want.member_name.clone()
            } else {
                Some(want.exported_name.clone())
            };
            if let Some(name) = name {
                let rows = self.bindings(file_path)?;
                let file_nodes = self.nodes_in_file(file_path)?;
                let matching: Vec<&KBinding> = rows
                    .iter()
                    .filter(|row| {
                        row.name == name
                            && row.scope_start == 1
                            && !file_nodes.iter().any(|node| {
                                matches!(node.kind.as_str(), "function" | "method" | "class")
                                    && node.start_line <= row.line
                                    && node.end_line >= row.line
                            })
                    })
                    .collect();
                if matching.len() == 1 {
                    let binding = matching[0];
                    if binding.kind == "import"
                        && binding.target_spec.is_some()
                        && binding.target_name.as_deref() != Some("*")
                    {
                        let spec = binding.target_spec.clone().unwrap();
                        let mut next = self.resolve_import_path(&spec, file_path, language)?;
                        if next.is_none() {
                            next = self
                                .find_python_module_file(&spec, file_path)?
                                .map(|n| n.file_path.clone());
                        }
                        if let Some(next) = next {
                            let chained = self.find_exported_symbol(
                                &next,
                                &ExportWant {
                                    is_default: false,
                                    is_namespace: false,
                                    exported_name: binding
                                        .target_name
                                        .clone()
                                        .unwrap_or_else(|| binding.name.clone()),
                                    member_name: None,
                                },
                                language,
                                visited,
                                depth + 1,
                            )?;
                            if chained.is_some() {
                                return self.memo_symbol_opt(memo_key, chained);
                            }
                        }
                    }
                }
            }
        }

        // 2. Named re-exports.
        let reexports = self.reexports(file_path)?;
        if reexports.is_empty() {
            return self.memo_symbol_opt(memo_key, None);
        }
        let target_name = if want.is_default {
            "default".to_string()
        } else {
            want.exported_name.clone()
        };
        for rex in reexports.iter() {
            if rex.kind == "named" && rex.exported_name.as_deref() == Some(target_name.as_str()) {
                let next = self.resolve_import_path(&rex.source, file_path, language)?;
                let Some(next) = next else { continue };
                let chained = self.find_exported_symbol(
                    &next,
                    &ExportWant {
                        is_default: rex.original_name.as_deref() == Some("default"),
                        is_namespace: false,
                        exported_name: rex.original_name.clone().unwrap_or_default(),
                        member_name: None,
                    },
                    language,
                    visited,
                    depth + 1,
                )?;
                if chained.is_some() {
                    return self.memo_symbol_opt(memo_key, chained);
                }
            }
        }
        // 3. Wildcard re-exports.
        for rex in reexports.iter() {
            if rex.kind == "wildcard" {
                let next = self.resolve_import_path(&rex.source, file_path, language)?;
                let Some(next) = next else { continue };
                let chained =
                    self.find_exported_symbol(&next, want, language, visited, depth + 1)?;
                if chained.is_some() {
                    return self.memo_symbol_opt(memo_key, chained);
                }
            }
        }
        self.memo_symbol_opt(memo_key, None)
    }

    fn memo_symbol(&mut self, key: Option<String>, n: KNode) -> Result<Option<Rc<KNode>>> {
        self.memo_symbol_opt(key, Some(Rc::new(n)))
    }
    fn memo_symbol_opt(
        &mut self,
        key: Option<String>,
        v: Option<Rc<KNode>>,
    ) -> Result<Option<Rc<KNode>>> {
        if let Some(k) = key {
            self.exported_symbol_memo.insert(k, v.clone());
        }
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Built-in tables (index.ts isBuiltInOrExternal) — verbatim ports.
// ---------------------------------------------------------------------------

static JS_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "console", "window", "document", "global", "process", "Promise", "Array", "Object",
        "String", "Number", "Boolean", "Date", "Math", "JSON", "RegExp", "Error", "Map", "Set",
        "WeakMap", "WeakSet", "setTimeout", "setInterval", "clearTimeout", "clearInterval",
        "fetch", "require", "module", "exports", "__dirname", "__filename",
    ]
    .into_iter()
    .collect()
});

static REACT_HOOKS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "useState", "useEffect", "useContext", "useReducer", "useCallback", "useMemo", "useRef",
        "useLayoutEffect", "useImperativeHandle", "useDebugValue",
    ]
    .into_iter()
    .collect()
});

static PYTHON_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "print", "len", "range", "str", "int", "float", "list", "dict", "set", "tuple", "open",
        "input", "type", "isinstance", "hasattr", "getattr", "setattr", "super", "self", "cls",
        "None", "True", "False",
    ]
    .into_iter()
    .collect()
});

static PYTHON_BUILT_IN_METHODS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "append", "extend", "insert", "remove", "pop", "clear", "sort", "reverse", "copy", "update",
        "keys", "values", "items", "get", "add", "discard", "union", "intersection", "difference",
        "split", "join", "strip", "lstrip", "rstrip", "replace", "lower", "upper", "startswith",
        "endswith", "find", "index", "count", "encode", "decode", "format", "isdigit", "isalpha",
        "isalnum", "read", "write", "readline", "readlines", "close", "flush", "seek",
    ]
    .into_iter()
    .collect()
});

// GO_STDLIB_PACKAGES (index.ts isBuiltInOrExternal `pkg.member` arm) is not
// ported: that arm needs a '.' in the name, which kernel eligibility excludes.

static GO_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "make", "new", "len", "cap", "append", "copy", "delete", "close", "panic", "recover",
        "print", "println", "complex", "real", "imag", "error", "nil", "true", "false", "iota",
        "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
        "uintptr", "float32", "float64", "complex64", "complex128", "string", "bool", "byte",
        "rune", "any",
    ]
    .into_iter()
    .collect()
});

static C_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "printf", "fprintf", "sprintf", "snprintf", "scanf", "fscanf", "sscanf", "malloc", "calloc",
        "realloc", "free", "memcpy", "memmove", "memset", "memcmp", "memchr", "strlen", "strcpy",
        "strncpy", "strcat", "strncat", "strcmp", "strncmp", "strstr", "strchr", "strrchr",
        "strtok", "strdup", "fopen", "fclose", "fread", "fwrite", "fgets", "fputs", "fputc",
        "fgetc", "feof", "ferror", "fflush", "fseek", "ftell", "rewind", "exit", "abort", "atexit",
        "atoi", "atol", "atof", "strtol", "strtoul", "strtod", "qsort", "bsearch", "abs", "labs",
        "rand", "srand", "sin", "cos", "tan", "sqrt", "pow", "log", "log10", "exp", "ceil",
        "floor", "fabs", "time", "clock", "difftime", "mktime", "localtime", "gmtime", "strftime",
        "asctime", "assert", "errno", "perror", "remove", "rename", "tmpfile", "tmpnam", "getenv",
        "system", "signal", "raise", "setjmp", "longjmp", "va_start", "va_end", "va_arg",
        "va_copy", "NULL", "EOF", "BUFSIZ", "FILENAME_MAX", "RAND_MAX", "EXIT_SUCCESS",
        "EXIT_FAILURE", "size_t", "ptrdiff_t", "wchar_t", "intptr_t", "uintptr_t", "int8_t",
        "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "FILE",
        "stat", "lstat", "fstat", "open", "close", "read", "write", "pipe", "fork", "exec",
        "waitpid", "getpid", "getppid", "kill", "sleep", "usleep", "pthread_create",
        "pthread_join", "pthread_mutex_lock", "pthread_mutex_unlock", "dlopen", "dlsym", "dlclose",
    ]
    .into_iter()
    .collect()
});

static CPP_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "cout", "cin", "cerr", "clog", "endl", "flush", "ws", "std", "nullptr", "true", "false",
        "this", "sizeof", "alignof", "typeid", "static_cast", "dynamic_cast", "reinterpret_cast",
        "const_cast", "make_unique", "make_shared", "make_pair", "move", "forward", "swap",
    ]
    .into_iter()
    .collect()
});

/// C_CPP_STDLIB_HEADERS (import-resolver.ts isExternalImport).
static C_CPP_STDLIB_HEADERS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "assert.h", "complex.h", "ctype.h", "errno.h", "fenv.h", "float.h", "inttypes.h",
        "iso646.h", "limits.h", "locale.h", "math.h", "setjmp.h", "signal.h", "stdalign.h",
        "stdarg.h", "stdatomic.h", "stdbool.h", "stddef.h", "stdint.h", "stdio.h", "stdlib.h",
        "stdnoreturn.h", "string.h", "tgmath.h", "threads.h", "time.h", "uchar.h", "wchar.h",
        "wctype.h", "cassert", "ccomplex", "cctype", "cerrno", "cfenv", "cfloat", "cinttypes",
        "ciso646", "climits", "clocale", "cmath", "csetjmp", "csignal", "cstdalign", "cstdarg",
        "cstdbool", "cstddef", "cstdint", "cstdio", "cstdlib", "cstring", "ctgmath", "ctime",
        "cuchar", "cwchar", "cwctype", "algorithm", "any", "array", "atomic", "barrier", "bit",
        "bitset", "charconv", "chrono", "codecvt", "compare", "complex", "concepts",
        "condition_variable", "coroutine", "deque", "exception", "execution", "expected",
        "filesystem", "format", "forward_list", "fstream", "functional", "future", "generator",
        "initializer_list", "iomanip", "ios", "iosfwd", "iostream", "istream", "iterator", "latch",
        "limits", "list", "locale", "map", "mdspan", "memory", "memory_resource", "mutex", "new",
        "numbers", "numeric", "optional", "ostream", "print", "queue", "random", "ranges",
        "ratio", "regex", "scoped_allocator", "semaphore", "set", "shared_mutex",
        "source_location", "span", "spanstream", "sstream", "stack", "stacktrace", "stdexcept",
        "stdfloat", "stop_token", "streambuf", "string", "string_view", "strstream", "syncstream",
        "system_error", "thread", "tuple", "type_traits", "typeindex", "typeinfo",
        "unordered_map", "unordered_set", "utility", "valarray", "variant", "vector", "version",
    ]
    .into_iter()
    .collect()
});

/// The `want` of findExportedSymbol.
struct ExportWant {
    is_default: bool,
    is_namespace: bool,
    exported_name: String,
    member_name: Option<String>,
}

impl KernelResolver {
    // -----------------------------------------------------------------------
    // isBuiltInOrExternal + the fast pre-filter (index.ts)
    // -----------------------------------------------------------------------

    /// isBuiltInOrExternal restricted to migrated languages and bare names —
    /// every arm the bare slice can reach, in the same order.
    fn is_built_in_or_external(&self, r: &ResolveRefIn) -> bool {
        let name = r.reference_name.as_str();
        let is_js_ts = is_esm_family(&r.language);
        if is_js_ts && JS_BUILT_INS.contains(name) {
            return true;
        }
        if r.language == "arkts" && (name == "$r" || name == "$rawfile") {
            return true;
        }
        if is_js_ts && REACT_HOOKS.contains(name) {
            return true;
        }
        if r.language == "python" && PYTHON_BUILT_INS.contains(name) {
            return true;
        }
        if r.language == "python" && PYTHON_BUILT_IN_METHODS.contains(name) {
            // A bare name colliding with a builtin method is only a builtin
            // when nothing declares it.
            return !self.known_names.contains(name);
        }
        // Go: `!isBindingReceiverCall` is implied — a bare name never carries
        // a receiver — so the bare arm applies. The `pkg.member` stdlib arm
        // needs a '.', dead here.
        if r.language == "go" && GO_BUILT_INS.contains(name) {
            return true;
        }
        if r.language == "c" || r.language == "cpp" {
            // `std::` prefix needs ':' — dead for bare names.
            if C_BUILT_INS.contains(name) || CPP_BUILT_INS.contains(name) {
                return !self.has_any_possible_match(name);
            }
        }
        false
    }

    /// hasAnyPossibleMatch reduced to the bare-name form: every separator-
    /// dependent arm (dot/colon/slash/`$`/path) is dead by definition of
    /// `is_bare_name`, leaving the direct known-name check.
    fn has_any_possible_match(&self, name: &str) -> bool {
        self.known_names.contains(name)
    }

    /// matchesAnyImport — bare names can only satisfy the `localName ===
    /// name` arm.
    fn matches_any_import(&mut self, r: &ResolveRefIn) -> Result<bool> {
        let imports = self.import_mappings(&r.file_path)?;
        Ok(imports.iter().any(|i| i.local_name == r.reference_name))
    }

    /// `frameworks.some(f => f.claimsReference?.(name))` — the prefilter's
    /// fourth arm, evaluated over the detected resolver names. A config
    /// without `framework_names` predates the refinement: every miss is
    /// conservatively claimed (the old `frameworks_active` behavior).
    fn framework_claims(&self, name: &str) -> bool {
        if !self.frameworks_active {
            return false;
        }
        match &self.framework_names {
            None => true,
            Some(names) => names.iter().any(|f| framework_claims_reference(f, name)),
        }
    }

    // -----------------------------------------------------------------------
    // innermostBinding / isBoundToBareImport / isLocallyBoundJsName
    // (name-matcher.ts)
    // -----------------------------------------------------------------------

    /// innermostBinding: the row for `name` whose scope contains `line`,
    /// narrowest scope first (first-wins on ties, matching the TS loop).
    fn innermost_binding<'a>(
        rows: &'a [KBinding],
        name: &str,
        line: Option<i64>,
    ) -> Option<&'a KBinding> {
        let mut best: Option<&KBinding> = None;
        for r in rows {
            if r.name != name {
                continue;
            }
            if let Some(l) = line {
                if l < r.scope_start || l > r.scope_end {
                    continue;
                }
            }
            if best.is_none_or(|b| r.scope_end - r.scope_start < b.scope_end - b.scope_start) {
                best = Some(r);
            }
        }
        best
    }

    /// packageNameOf — `@scope/pkg/sub` → `@scope/pkg`, `pkg/sub` → `pkg`.
    fn package_name_of(source: &str) -> &str {
        let mut parts = source.split('/');
        match (source.starts_with('@'), parts.next(), parts.next()) {
            (true, Some(scope), Some(pkg)) => &source[..scope.len() + 1 + pkg.len()],
            (_, Some(first), _) => first,
            _ => source,
        }
    }

    /// isBoundToBareImport (name-matcher.ts) — ESM languages only; the
    /// call-site name resolves to a bare external specifier, so no project
    /// node may claim it.
    fn is_bound_to_bare_import(&mut self, r: &ResolveRefIn) -> Result<bool> {
        if !is_esm_family(&r.language) {
            return Ok(false);
        }
        let rows = self.bindings(&r.file_path)?;
        let source: Option<String> = if !rows.is_empty() {
            match Self::innermost_binding(&rows, &r.reference_name, Some(r.line)) {
                Some(b) if b.kind == "import" => b.target_spec.clone(),
                _ => None,
            }
        } else {
            self.import_mappings(&r.file_path)?
                .iter()
                .find(|i| i.local_name == r.reference_name)
                .map(|i| i.source.clone())
        };
        let Some(source) = source else { return Ok(false) };
        if source.starts_with('.') || source.starts_with('/') {
            return Ok(false);
        }
        if source.starts_with('~') || source.starts_with('#') || source.starts_with('$') {
            return Ok(false);
        }
        if source.starts_with("@/") || source.starts_with("src/") {
            return Ok(false);
        }
        if let Some(aliases) = &self.aliases {
            if aliases.patterns.iter().any(|p| source.starts_with(&p.prefix)) {
                return Ok(false);
            }
        }
        if let Some(ws) = &self.workspaces {
            if self.resolve_workspace_import(&source).is_some() {
                return Ok(false);
            }
            if ws.local_link_names.contains(Self::package_name_of(&source)) {
                return Ok(false);
            }
        }
        if !source.starts_with("node:") && !self.node_builtins.contains(&source) {
            let head = Self::package_name_of(&source).to_string();
            let local = match self.root_import_memo.get(&head) {
                Some(&v) => v,
                None => {
                    let v = self.file_exists(&head);
                    self.root_import_memo.insert(head, v);
                    v
                }
            };
            if local {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// isLocallyBoundJsName — a `decl`/`local`/`param` row whose scope holds
    /// the reference.
    fn is_locally_bound_js_name(
        &mut self,
        name: &str,
        file_path: &str,
        line: Option<i64>,
    ) -> Result<bool> {
        let rows = self.bindings(file_path)?;
        if rows.is_empty() {
            return Ok(false);
        }
        Ok(match Self::innermost_binding(&rows, name, line) {
            Some(b) => matches!(b.kind.as_str(), "decl" | "local" | "param"),
            None => false,
        })
    }

    // -----------------------------------------------------------------------
    // Import machinery (import-resolver.ts)
    // -----------------------------------------------------------------------

    /// isExternalImport (import-resolver.ts), context-aware.
    fn is_external_import(&self, import_path: &str, language: &str) -> bool {
        if import_path.starts_with('.') {
            return false;
        }
        if self.workspaces.is_some() && self.resolve_workspace_import(import_path).is_some() {
            return false;
        }
        if is_esm_import_language(language) {
            if ESM_BUILTIN_MODULES.contains(&import_path) {
                return true;
            }
            if let Some(aliases) = &self.aliases {
                if aliases.patterns.iter().any(|p| import_path.starts_with(&p.prefix)) {
                    return false;
                }
            }
            if !import_path.starts_with("@/")
                && !import_path.starts_with("~/")
                && !import_path.starts_with("src/")
            {
                return true;
            }
        }
        if language == "python" {
            let head = import_path.split('.').next().unwrap_or("");
            if PYTHON_STDLIB_HEADS.contains(&head) {
                return true;
            }
        }
        if language == "go" {
            if import_path.starts_with('.') {
                return false;
            }
            if let Some(mod_path) = &self.go_module_path {
                if import_path == mod_path || import_path.starts_with(&format!("{}/", mod_path)) {
                    return false;
                }
            }
            if import_path.contains("/internal/") {
                return false;
            }
            return true;
        }
        if language == "c" || language == "cpp" {
            if C_CPP_STDLIB_HEADERS.contains(import_path) {
                return true;
            }
            let without_ext = import_path.strip_suffix(".h").unwrap_or(import_path);
            if C_CPP_STDLIB_HEADERS.contains(without_ext) {
                return true;
            }
        }
        false
    }

    /// resolveWorkspaceImport (workspace-packages.ts): sourceEntries first,
    /// then longest matching package name; a bare member import resolves to
    /// its declared entry when the manifest names one.
    fn resolve_workspace_import(&self, import_path: &str) -> Option<String> {
        let ws = self.workspaces.as_ref()?;
        if let Some((_, src)) = ws.source_entries.iter().find(|(k, _)| k == import_path) {
            return Some(src.clone());
        }
        let mut best: Option<(&str, &str)> = None;
        for (name, dir) in &ws.by_name {
            if (import_path == name.as_str() || import_path.starts_with(&format!("{}/", name)))
                && best.is_none_or(|(b, _)| name.len() > b.len())
            {
                best = Some((name, dir));
            }
        }
        let (name, dir) = best?;
        let subpath = &import_path[name.len()..]; // '' or '/widgets'
        if subpath.is_empty() {
            if let Some(entry_by_name) = &ws.entry_by_name {
                if let Some(entry) = entry_by_name.get(name) {
                    return Some(entry.clone());
                }
            }
        }
        let joined = format!("{}{}", dir, subpath);
        static MULTI_SLASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/{2,}").unwrap());
        Some(MULTI_SLASH.replace_all(&joined, "/").to_string())
    }

    /// applyAliases (path-aliases.ts): candidate paths relative to
    /// projectRoot in tsconfig priority order.
    fn apply_aliases(&self, import_path: &str) -> Vec<String> {
        let Some(alias_map) = &self.aliases else { return Vec::new() };
        for pat in &alias_map.patterns {
            if !import_path.starts_with(&pat.prefix) {
                continue;
            }
            if !pat.suffix.is_empty() && !import_path.ends_with(&pat.suffix) {
                continue;
            }
            let captured = if pat.has_wildcard {
                &import_path[pat.prefix.len()..import_path.len() - pat.suffix.len()]
            } else if import_path != pat.prefix {
                continue;
            } else {
                ""
            };
            let mut out = Vec::new();
            for target in &pat.replacements {
                let filled = if pat.has_wildcard {
                    target.replacen('*', captured, 1)
                } else {
                    target.clone()
                };
                let base = alias_map.base_url.as_deref().unwrap_or(&self.project_root);
                let absolute = pos_resolve(base, &filled);
                let relative = pos_relative(&self.root_abs, &absolute).replace('\\', "/");
                if relative.starts_with("..") {
                    continue;
                }
                out.push(relative);
            }
            return out;
        }
        Vec::new()
    }

    /// findSourceForEmittedSpecifier (import-resolver.ts).
    fn find_source_for_emitted(&self, relative_path: &str, language: &str) -> Option<String> {
        let sources = emitted_to_source(relative_path, language)?;
        let stem_len = relative_path.rfind('.')?; // safe: an emitted suffix matched
        let stem = &relative_path[..stem_len];
        for ext in sources {
            let candidate = format!("{}{}", stem, ext);
            if self.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// resolveRelativeImport (import-resolver.ts), including the Python
    /// dotted-relative translation.
    fn resolve_relative_import(
        &mut self,
        import_path: &str,
        from_dir: &str,
        language: &str,
    ) -> Result<Option<String>> {
        let extensions = extension_resolution(language);
        if language == "python" && import_path.starts_with('.') {
            let dots = import_path.len() - import_path.trim_start_matches('.').len();
            let up = "../".repeat(dots.saturating_sub(1));
            let rest = import_path[dots..].replace('.', "/");
            let py_base = pos_resolve(from_dir, &format!("{}{}", up, rest));
            let py_rel = pos_relative(&self.root_abs, &py_base);
            for ext in extensions {
                let candidate = format!("{}{}", py_rel, ext);
                if self.file_exists(&candidate) {
                    return Ok(Some(candidate));
                }
            }
            if !py_rel.is_empty() && self.file_exists(&py_rel) {
                return Ok(Some(py_rel));
            }
            return Ok(None);
        }
        let base_path = pos_resolve(from_dir, import_path);
        let relative_path = pos_relative(&self.root_abs, &base_path);
        for ext in extensions {
            let candidate = format!("{}{}", relative_path, ext);
            if self.file_exists(&candidate) {
                return Ok(Some(candidate));
            }
        }
        if self.file_exists(&relative_path) {
            return Ok(Some(relative_path));
        }
        Ok(self.find_source_for_emitted(&relative_path, language))
    }

    /// resolveAliasedImport (import-resolver.ts): tsconfig paths → workspace
    /// → hardcoded fallbacks → direct path.
    fn resolve_aliased_import(
        &mut self,
        import_path: &str,
        language: &str,
    ) -> Result<Option<String>> {
        let extensions = extension_resolution(language);
        macro_rules! try_with_ext {
            ($base:expr) => {{
                let base: &str = $base;
                let mut hit: Option<String> = None;
                for ext in extensions {
                    let candidate = format!("{}{}", base, ext);
                    if self.file_exists(&candidate) {
                        hit = Some(candidate);
                        break;
                    }
                }
                if hit.is_none() && self.file_exists(base) {
                    hit = Some(base.to_string());
                }
                if hit.is_none() {
                    hit = self.find_source_for_emitted(base, language);
                }
                hit
            }};
        }

        if self.aliases.is_some() {
            for c in self.apply_aliases(import_path) {
                if let Some(hit) = try_with_ext!(&c) {
                    return Ok(Some(hit));
                }
            }
        }
        if self.workspaces.is_some() {
            if let Some(base) = self.resolve_workspace_import(import_path) {
                if let Some(hit) = try_with_ext!(&base) {
                    return Ok(Some(hit));
                }
            }
        }
        for (alias, replacement) in FALLBACK_ALIASES {
            if let Some(rest) = import_path.strip_prefix(alias) {
                let rewritten = format!("{}{}", replacement, rest);
                if let Some(hit) = try_with_ext!(&rewritten) {
                    return Ok(Some(hit));
                }
            }
        }
        Ok(try_with_ext!(import_path))
    }

    /// resolveCppIncludePath (import-resolver.ts): -I dir scan with the
    /// language's extension list, then the path as-is.
    fn resolve_cpp_include_path(&mut self, import_path: &str, language: &str) -> Option<String> {
        let extensions = extension_resolution(language);
        for dir in self.cpp_include_dirs.clone() {
            let normalized_dir = dir.replace('\\', "/");
            for ext in extensions {
                let candidate = format!("{}/{}{}", normalized_dir, import_path, ext);
                if self.file_exists(&candidate) {
                    return Some(candidate);
                }
            }
            let candidate = format!("{}/{}", normalized_dir, import_path);
            if self.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// resolveImportPath (import-resolver.ts) — memoized exactly like the TS
    /// per-context WeakMap memo (key: language\0fromFile\0importPath).
    fn resolve_import_path(
        &mut self,
        import_path: &str,
        from_file: &str,
        language: &str,
    ) -> Result<Option<String>> {
        let key = format!("{}\0{}\0{}", language, from_file, import_path);
        if let Some(hit) = self.import_path_memo.get(&key) {
            return Ok(hit.clone());
        }
        let resolved = self.resolve_import_path_uncached(import_path, from_file, language)?;
        self.import_path_memo.insert(key, resolved.clone());
        Ok(resolved)
    }

    fn resolve_import_path_uncached(
        &mut self,
        import_path: &str,
        from_file: &str,
        language: &str,
    ) -> Result<Option<String>> {
        // (COBOL copybook arm omitted: `cobol` is not a migrated language.)
        if self.is_external_import(import_path, language) {
            return Ok(None);
        }
        let from_dir = pos_dirname(&pos_resolve(&self.root_abs, from_file)).to_string();
        if import_path.starts_with('.') {
            return self.resolve_relative_import(import_path, &from_dir, language);
        }
        if let Some(aliased) = self.resolve_aliased_import(import_path, language)? {
            return Ok(Some(aliased));
        }
        if language == "c" || language == "cpp" {
            return Ok(self.resolve_cpp_include_path(import_path, language));
        }
        Ok(None)
    }

    /// findPythonModuleFile (import-resolver.ts): `a.b.c` → `a/b/c.py` or
    /// `a/b/c/__init__.py`, suffix-matched; root/`src/` prefixes win, then a
    /// unique candidate.
    fn find_python_module_file(
        &mut self,
        module: &str,
        exclude_file: &str,
    ) -> Result<Option<Rc<KNode>>> {
        if module.is_empty() || module.starts_with('.') {
            return Ok(None);
        }
        let rel = module.replace('.', "/");
        let last_seg = module.split('.').next_back().unwrap_or(module);
        let mut candidates: Vec<Rc<KNode>> = Vec::new();
        for n in self.nodes_by_name(&format!("{}.py", last_seg))?.iter() {
            let want = format!("{}.py", rel);
            if n.kind == "file"
                && n.file_path != exclude_file
                && (n.file_path == want || n.file_path.ends_with(&format!("/{}", want)))
            {
                candidates.push(Rc::new(n.clone()));
            }
        }
        for n in self.nodes_by_name("__init__.py")?.iter() {
            let want = format!("{}/__init__.py", rel);
            if n.kind == "file"
                && n.file_path != exclude_file
                && (n.file_path == want || n.file_path.ends_with(&format!("/{}", want)))
            {
                candidates.push(Rc::new(n.clone()));
            }
        }
        for root in ["", "src/"] {
            for suffix in [format!("{}/__init__.py", rel), format!("{}.py", rel)] {
                let want = format!("{}{}", root, suffix);
                if let Some(hit) = candidates.iter().find(|n| n.file_path == want) {
                    return Ok(Some(hit.clone()));
                }
            }
        }
        Ok(if candidates.len() == 1 { candidates.pop() } else { None })
    }

    /// resolveModuleImportToFile (import-resolver.ts): a whole-module import
    /// (`import * as ns`, `import def`, Python `from . import mod`) links to
    /// the module FILE node.
    fn resolve_module_import_to_file(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Result<Option<Rc<KNode>>> {
        if r.reference_kind != "imports" {
            return Ok(None);
        }
        // `ref.referenceName.includes('.')` — dead for a bare name.
        for imp in imports {
            if imp.local_name != r.reference_name {
                continue;
            }
            let module_path = if imp.is_namespace || imp.is_default {
                imp.source.clone()
            } else if r.language == "python" {
                let module_name = if imp.exported_name == "*" {
                    imp.local_name.clone()
                } else {
                    imp.exported_name.clone()
                };
                if imp.source.ends_with('.') {
                    format!("{}{}", imp.source, module_name)
                } else {
                    format!("{}.{}", imp.source, module_name)
                }
            } else {
                // A named TS/JS import binds a symbol, not a module.
                continue;
            };
            if let Some(resolved) =
                self.resolve_import_path(&module_path, &r.file_path, &r.language)?
            {
                if resolved != r.file_path {
                    if let Some(file_node) = self
                        .nodes_in_file(&resolved)?
                        .iter()
                        .find(|n| n.kind == "file")
                    {
                        return Ok(Some(Rc::new(file_node.clone())));
                    }
                }
            }
            if r.language == "python" {
                if let Some(mod_file) = self.find_python_module_file(&module_path, &r.file_path)? {
                    return Ok(Some(mod_file));
                }
            }
        }
        Ok(None)
    }

    /// resolveJavaImportedReference (import-resolver.ts) — the bare arm
    /// (`matchesBare` ⇒ memberName = localName) plus the static-import owner
    /// fallback, both over the FQN→file-suffix disambiguation.
    fn resolve_java_imported_reference(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Result<Option<Rc<KNode>>> {
        if imports.is_empty() {
            return Ok(None);
        }
        let ext = if r.language == "kotlin" { ".kt" } else { ".java" };
        for imp in imports {
            // `matchesQualified` (name startsWith localName+'.') is dead for a
            // bare name — only `matchesBare` remains.
            if imp.local_name != r.reference_name {
                continue;
            }
            let fqn_path = format!("{}{}", imp.source.replace('.', "/"), ext);
            let candidates = self.nodes_by_name(&imp.local_name)?;
            for node in candidates.iter() {
                if node.language != r.language {
                    continue;
                }
                let fp = node.file_path.replace('\\', "/");
                if fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{}", fqn_path)) {
                    return Ok(Some(Rc::new(node.clone())));
                }
            }
            // `import static com.example.Foo.bar;` — the FQN tail is the
            // member, the part before is the owner class.
            if let Some(dot) = imp.source.rfind('.') {
                if dot > 0 {
                    let owner_path =
                        format!("{}{}", imp.source[..dot].replace('.', "/"), ext);
                    for node in candidates.iter() {
                        if node.language != r.language {
                            continue;
                        }
                        let fp = node.file_path.replace('\\', "/");
                        if fp.ends_with(&owner_path)
                            || fp.ends_with(&format!("/{}", owner_path))
                        {
                            return Ok(Some(Rc::new(node.clone())));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// resolveViaImport restricted to the bare-name slice.
    fn resolve_via_import(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        if (r.language == "c" || r.language == "cpp") && r.reference_kind == "imports" {
            // Quoted-include search order: the including file's own directory
            // first, via a same-named file NODE (not just existence).
            let from_dir = pos_dirname(&r.file_path);
            let sibling_path = if from_dir.is_empty() {
                pos_normalize(&r.reference_name)
            } else {
                pos_normalize(&format!("{}/{}", from_dir, r.reference_name))
            };
            let sibling_base = pos_basename(&sibling_path).to_string();
            if let Some(sibling) = self
                .nodes_by_name(&sibling_base)?
                .iter()
                .find(|n| n.kind == "file" && n.file_path == sibling_path)
            {
                return Ok(Some(KCand {
                    node: Rc::new(sibling.clone()),
                    confidence: 0.92,
                    resolved_by: "import",
                }));
            }
            let Some(resolved_path) =
                self.resolve_import_path(&r.reference_name, &r.file_path, &r.language)?
            else {
                return Ok(None);
            };
            let basename = pos_basename(&resolved_path).to_string();
            if let Some(file_node) = self
                .nodes_by_name(&basename)?
                .iter()
                .find(|n| n.kind == "file" && n.file_path == resolved_path)
            {
                return Ok(Some(KCand {
                    node: Rc::new(file_node.clone()),
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
            return Ok(None);
        }
        // isPhpIncludePathRef / isCobolCopybookRef / isNixPathImportRef are
        // dead here: the first needs '/' or '.' in the name (non-bare), the
        // other two name unmigrated languages.

        let imports = self.import_mappings(&r.file_path)?;
        if imports.is_empty() && self.read_file(&r.file_path).is_none() {
            return Ok(None);
        }

        // resolveGoCrossPackageReference needs `pkg.Member` — non-bare, dead.
        if r.language == "java" || r.language == "kotlin" {
            if let Some(node) = self.resolve_java_imported_reference(r, &imports)? {
                return Ok(Some(KCand {
                    node,
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }
        // resolvePythonModuleMember / resolvePythonAbsoluteModule need a '.'
        // in the name — dead. Rust path refs: rust is unmigrated.
        if matches!(
            r.language.as_str(),
            "python" | "typescript" | "tsx" | "javascript" | "jsx" | "arkts"
        ) {
            if let Some(node) = self.resolve_module_import_to_file(r, &imports)? {
                return Ok(Some(KCand {
                    node,
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }

        for imp in imports.iter() {
            // `name.startsWith(localName + '.')` is dead for a bare name.
            if imp.local_name != r.reference_name {
                continue;
            }
            let mut resolved_path =
                self.resolve_import_path(&imp.source, &r.file_path, &r.language)?;
            if resolved_path.is_none() && r.language == "python" {
                resolved_path = self
                    .find_python_module_file(&imp.source, &r.file_path)?
                    .map(|n| n.file_path.clone());
            }
            let Some(resolved_path) = resolved_path else { continue };
            let want = ExportWant {
                is_default: imp.is_default,
                is_namespace: imp.is_namespace,
                exported_name: if imp.is_default {
                    "default".to_string()
                } else {
                    imp.exported_name.clone()
                },
                // Namespace import + bare localName: the TS `.replace` finds
                // no `localName.` in a bare name, so memberName = the name.
                member_name: if imp.is_namespace {
                    Some(r.reference_name.replacen(&format!("{}.", imp.local_name), "", 1))
                } else {
                    None
                },
            };
            let mut visited = HashSet::new();
            if let Some(target) =
                self.find_exported_symbol(&resolved_path, &want, &r.language, &mut visited, 0)?
            {
                // The member-descent block is dead for a bare name.
                return Ok(Some(KCand {
                    node: target,
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }
        Ok(None)
    }

    /// isBoundToOutOfRepoImport (import-resolver.ts): for a bare name in a
    /// migrated language only the ESM arm can fire.
    fn is_bound_to_out_of_repo_import(&mut self, r: &ResolveRefIn) -> Result<bool> {
        if !is_esm_import_language(&r.language) {
            return Ok(false);
        }
        for imp in self.import_mappings(&r.file_path)?.iter() {
            if imp.local_name != r.reference_name {
                continue;
            }
            return Ok(self.is_external_import(&imp.source, &r.language));
        }
        Ok(false)
    }
}
impl KernelResolver {
    // -----------------------------------------------------------------------
    // Name machinery (name-matcher.ts)
    // -----------------------------------------------------------------------

    /// JS `string.slice(i)` over UTF-16 code units — `column` values are
    /// extraction offsets consumed as JS string indices, so replicate that
    /// indexing (round a mid-surrogate boundary up to the next char).
    fn js_slice(s: &str, start: usize) -> &str {
        &s[Self::js_unit_to_byte(s, start)..]
    }

    /// JS `string.slice(0, i)` over UTF-16 code units.
    fn js_prefix(s: &str, end: usize) -> &str {
        &s[..Self::js_unit_to_byte(s, end)]
    }

    /// UTF-16 unit offset → byte offset (clamped to the next char boundary).
    fn js_unit_to_byte(s: &str, units: usize) -> usize {
        let mut seen = 0usize;
        for (byte_idx, ch) in s.char_indices() {
            if seen >= units {
                return byte_idx;
            }
            seen += ch.len_utf16();
        }
        s.len()
    }

    /// UTF-16 length — the `.length` JS sees.
    fn utf16_len(s: &str) -> usize {
        s.chars().map(|c| c.len_utf16()).sum()
    }

    /// applyLanguageGate (name-matcher.ts).
    fn apply_language_gate(&self, candidates: Vec<Rc<KNode>>, r: &ResolveRefIn) -> Vec<Rc<KNode>> {
        if r.reference_kind == "references" || r.reference_kind == "function_ref" {
            return candidates
                .into_iter()
                .filter(|c| same_language_family(&c.language, &r.language))
                .collect();
        }
        if r.reference_kind == "imports" {
            return candidates
                .into_iter()
                .filter(|c| !crosses_known_family(&c.language, &r.language))
                .collect();
        }
        candidates
    }

    /// isLexicallyReachable (name-matcher.ts): a function nested in a
    /// same-file function/method is reachable only from inside the parent.
    fn is_lexically_reachable(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Result<bool> {
        if candidate.kind != "function" {
            return Ok(true);
        }
        if no_nested_functions(&candidate.language) {
            return Ok(true);
        }
        let qn = &candidate.qualified_name;
        let Some(sep) = qn.rfind("::") else { return Ok(true) };
        let parent_qn = &qn[..sep];
        let containers: Vec<KNode> = self
            .nodes_by_qualified_name(parent_qn)?
            .iter()
            .filter(|p| {
                p.file_path == candidate.file_path
                    && (p.kind == "function" || p.kind == "method")
                    && p.start_line <= candidate.start_line
                    && p.end_line >= candidate.end_line
            })
            .cloned()
            .collect();
        if containers.is_empty() {
            return Ok(true);
        }
        Ok(r.file_path == candidate.file_path
            && containers
                .iter()
                .any(|p| r.line >= p.start_line && r.line <= p.end_line))
    }

    /// isSealedModule (name-matcher.ts): an ESM file with import statements
    /// and no export of any form offers nothing cross-file.
    fn is_sealed_module(&mut self, file_path: &str) -> Result<bool> {
        if let Some(&hit) = self.sealed_memo.get(file_path) {
            return Ok(hit);
        }
        let rows = self.bindings(file_path)?;
        let nodes = self.nodes_in_file(file_path)?;
        let sealed = !rows.is_empty()
            && nodes.iter().any(|n| n.kind == "import")
            && !rows.iter().any(|r| r.exported_as.is_some())
            && !nodes.iter().any(|n| n.is_exported);
        self.sealed_memo.insert(file_path.to_string(), sealed);
        Ok(sealed)
    }

    /// isStaticCFunction (name-matcher.ts): binding row storage === 'static'.
    fn is_static_c_function(&mut self, candidate: &KNode) -> Result<bool> {
        if let Some(&hit) = self.c_static_memo.get(&candidate.id) {
            return Ok(hit);
        }
        let rows = self.bindings(&candidate.file_path)?;
        let is_static = rows
            .iter()
            .find(|r| r.node_id.as_deref() == Some(candidate.id.as_str()))
            .is_some_and(|r| r.storage.as_deref() == Some("static"));
        self.c_static_memo.insert(candidate.id.clone(), is_static);
        Ok(is_static)
    }

    /// rustModuleDir (name-matcher.ts).
    fn rust_module_dir(file_path: &str) -> String {
        let base = pos_basename(file_path);
        let dir = pos_dirname(file_path);
        if base == "mod.rs" || base == "lib.rs" || base == "main.rs" {
            return dir.to_string();
        }
        let stem = base.strip_suffix(".rs").unwrap_or(base);
        if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{}/{}", dir, stem)
        }
    }

    /// isRustTraitImplMethod (name-matcher.ts) — scan upward for the nearest
    /// `impl` header; `impl Trait for` wins, a top-level item ends the scan.
    fn is_rust_trait_impl_method(&mut self, candidate: &KNode) -> Result<bool> {
        if candidate.kind != "method" {
            return Ok(false);
        }
        if let Some(&hit) = self.rust_trait_memo.get(&candidate.id) {
            return Ok(hit);
        }
        let lines = self
            .read_file(&candidate.file_path)
            .unwrap_or_else(|| Rc::new(Vec::new()));
        let mut is_trait = false;
        let mut i = candidate.start_line.saturating_sub(2);
        loop {
            if i < 0 {
                break;
            }
            let line = lines.get(i as usize).map(|s| s.as_str()).unwrap_or("");
            if IMPL_RE.is_match(line) {
                let stripped = line.split("//").next().unwrap_or("");
                is_trait = IMPL_FOR_RE.is_match(stripped);
                break;
            }
            if ITEM_RE.is_match(line) {
                break;
            }
            i -= 1;
            if i < 0 {
                break;
            }
        }
        self.rust_trait_memo.insert(candidate.id.clone(), is_trait);
        Ok(is_trait)
    }

    /// The `= require("….json")` signature guard (isCrossFileReachable).
    /// Hand-rolled because the JS pattern uses a backreference.
    fn is_json_require_signature(sig: &str) -> bool {
        // ^=\s*require\s*\(\s*(['"])[^'"]+\.json\1\s*\)\s*;?\s*$
        let s = sig.trim();
        let Some(s) = s.strip_prefix('=') else { return false };
        let s = s.trim_start();
        let Some(s) = s.strip_prefix("require") else { return false };
        let s = s.trim_start();
        let Some(s) = s.strip_prefix('(') else { return false };
        let s = s.trim_start();
        let Some(q) = s.chars().next() else { return false };
        if q != '\'' && q != '"' {
            return false;
        }
        let Some(end) = s[1..].find(q) else { return false };
        let content = &s[1..1 + end];
        // `[^'"]+\.json` — at least one non-quote char, then literal `.json`.
        if content.len() <= ".json".len()
            || !content.ends_with(".json")
            || content.contains(['\'', '"'])
        {
            return false;
        }
        let s = s[1 + end + 1..].trim_start();
        let Some(s) = s.strip_prefix(')') else { return false };
        let s = s.trim_start();
        let s = s.strip_prefix(';').unwrap_or(s);
        s.trim().is_empty()
    }

    /// isCrossFileReachable (name-matcher.ts).
    fn is_cross_file_reachable(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Result<bool> {
        if r.language != "markdown"
            && candidate.language == "markdown"
            && !MARKDOWN_PATH_RE.is_match(&r.reference_name.replace('\\', "/"))
        {
            return Ok(false);
        }
        if r.reference_kind == "calls"
            && is_esm_family(&candidate.language)
            && (candidate.kind == "constant" || candidate.kind == "variable")
            && Self::is_json_require_signature(candidate.signature.as_deref().unwrap_or(""))
        {
            return Ok(false);
        }
        if candidate.file_path == r.file_path {
            return Ok(true);
        }
        if !is_esm_family(&candidate.language) {
            return Ok(true);
        }
        self.is_sealed_module(&candidate.file_path).map(|s| !s)
    }

    /// isVisibleAcrossFiles (name-matcher.ts).
    fn is_visible_across_files(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Result<bool> {
        if candidate.file_path == r.file_path {
            return Ok(true);
        }
        let lang = candidate.language.as_str();
        if lang == "c" || lang == "cpp" {
            return Ok(candidate.kind != "function"
                || !C_SOURCE_EXT_RE.is_match(&candidate.file_path)
                || !self.is_static_c_function(candidate)?);
        }
        if lang == "go" {
            let first = candidate.name.chars().next().unwrap_or('_');
            return Ok(first.is_ascii_uppercase()
                || pos_dirname(&candidate.file_path) == pos_dirname(&r.file_path));
        }
        if lang == "rust" {
            if candidate.visibility.as_deref() != Some("private") {
                return Ok(true);
            }
            if self.is_rust_trait_impl_method(candidate)? {
                return Ok(true);
            }
            let owner = Self::rust_module_dir(&candidate.file_path);
            return Ok(r.file_path.starts_with(&format!("{}/", owner)));
        }
        if private_is_file_local(lang) {
            return Ok(candidate.visibility.as_deref() != Some("private"));
        }
        self.is_cross_file_reachable(candidate, r)
    }

    /// isBareJsCall (name-matcher.ts): a receiver-less JS/TS `calls` ref.
    fn is_bare_js_call(&mut self, r: &ResolveRefIn) -> Result<bool> {
        if r.reference_kind != "calls" || !is_js_family(&r.language) {
            return Ok(false);
        }
        // `!name.includes('.')` — implied by bare eligibility.
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(false) };
        let at = Self::js_slice(line, r.column as usize);
        // `new RegExp('^' + nameEsc + '\\s*[(<]')` — nameEsc is the JS regex
        // escape, which `regex::escape` reproduces for our charset.
        let call_re = self.cached_regex(&format!("^{}\\s*[(<]", regex::escape(&r.reference_name)))?;
        if !call_re.is_match(at) {
            return Ok(false);
        }
        let before = Self::js_prefix(line, r.column as usize);
        Ok(!JS_CALL_PREFIX_RE.is_match(before) || JS_CALL_KEYWORD_RE.is_match(before))
    }

    /// cppBareCallForm (name-matcher.ts) — only the ADL range names.
    fn cpp_bare_call_form(&mut self, r: &ResolveRefIn) -> Result<Option<&'static str>> {
        if r.reference_kind != "calls" {
            return Ok(None);
        }
        if r.language != "c" && r.language != "cpp" {
            return Ok(None);
        }
        if !is_cpp_adl_name(&r.reference_name) {
            return Ok(None);
        }
        // `name.includes('.') || includes('::')` — dead for bare names.
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(None) };
        let from = (r.column as usize).min(Self::utf16_len(line));
        let hay = Self::js_slice(line, from);
        let re = self.cached_regex(&format!(
            "(^|[^A-Za-z0-9_]){}\\s*\\(",
            regex::escape(&r.reference_name)
        ))?;
        let Some(m) = re.captures(hay) else { return Ok(None) };
        // m.index / m[1].length are UTF-16 units in TS; convert the haystack
        // byte offsets back to units before re-anchoring on `line`.
        let name_at = from
            + Self::utf16_len(&hay[..m.get(0).unwrap().start()])
            + Self::utf16_len(m.get(1).unwrap().as_str());
        let before: String = Self::js_prefix(line, name_at)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if before.ends_with("::") {
            return Ok(Some("qualified"));
        }
        if CPP_THIS_ARROW_RE.is_match(&before) || CPP_THIS_DOT_RE.is_match(&before) {
            return Ok(Some("this-member"));
        }
        if before.ends_with("->") || before.ends_with('.') {
            return Ok(Some("explicit-member"));
        }
        let after_name = Self::js_slice(line, name_at + Self::utf16_len(&r.reference_name));
        let Some(open) = after_name.find('(') else { return Ok(Some("implicit-this")) };
        Ok(Some(
            if AFTER_NAME_PAREN_RE.is_match(&after_name[open + 1..]) {
                "implicit-this"
            } else {
                "free-args"
            },
        ))
    }

    /// enclosingTypePrefix + callableOnType + applyCppCallSiteForm
    /// (name-matcher.ts).
    fn enclosing_type_prefix(node: Option<&KNode>) -> Option<String> {
        let node = node?;
        let sep = node.qualified_name.rfind("::")?;
        if sep == 0 {
            return None;
        }
        Some(node.qualified_name[..sep].to_string())
    }

    fn callable_on_type(candidate: &KNode, type_prefix: &str) -> bool {
        if candidate.kind != "method" && candidate.kind != "function" {
            return false;
        }
        let qn = &candidate.qualified_name;
        if !qn.contains("::") {
            return false;
        }
        let want = format!("{}::{}", type_prefix, candidate.name);
        *qn == want || qn.ends_with(&format!("::{}", want)) || want.ends_with(&format!("::{}", qn))
    }

    /// Returns `Ok(None)` when the call-site form vetoes every candidate
    /// (the TS `null` — reference stays unresolved).
    fn apply_cpp_call_site_form(
        &mut self,
        r: &ResolveRefIn,
        candidates: Vec<Rc<KNode>>,
    ) -> Result<Option<Vec<Rc<KNode>>>> {
        let Some(form) = self.cpp_bare_call_form(r)? else {
            return Ok(Some(candidates));
        };
        match form {
            "qualified" | "explicit-member" => Ok(None),
            "this-member" => {
                let origin = self.node_by_id(&r.from_node_id)?;
                let Some(prefix) = Self::enclosing_type_prefix(origin.as_deref()) else {
                    return Ok(None);
                };
                Ok(Some(
                    candidates
                        .into_iter()
                        .filter(|n| Self::callable_on_type(n, &prefix))
                        .collect(),
                ))
            }
            "free-args" => {
                let local: Vec<Rc<KNode>> = candidates
                    .into_iter()
                    .filter(|n| n.kind == "function" && n.file_path == r.file_path)
                    .collect();
                Ok(Some(if local.len() == 1 { local } else { Vec::new() }))
            }
            _ => Ok(Some(candidates)),
        }
    }

    /// pathProximityFromDirs + computePathProximity (name-matcher.ts).
    fn path_proximity_from_dirs(dir1: &[String], file_path2: &str) -> i64 {
        let mut dir2: Vec<&str> = file_path2.split('/').collect();
        dir2.pop();
        let mut shared = 0i64;
        for i in 0..dir1.len().min(dir2.len()) {
            if dir1[i] == dir2[i] {
                shared += 1;
            } else {
                break;
            }
        }
        (shared * 15).min(80)
    }

    fn compute_path_proximity(file_path1: &str, file_path2: &str) -> i64 {
        let mut dir1: Vec<String> = file_path1.split('/').map(|s| s.to_string()).collect();
        dir1.pop();
        Self::path_proximity_from_dirs(&dir1, file_path2)
    }

    /// findBestMatch (name-matcher.ts) — strict `>` first-max scoring.
    fn find_best_match(&self, r: &ResolveRefIn, candidates: &[Rc<KNode>]) -> Option<Rc<KNode>> {
        let mut best_score = -1f64;
        let mut best: Option<Rc<KNode>> = None;
        let mut ref_dirs: Vec<String> =
            r.file_path.split('/').map(|s| s.to_string()).collect();
        ref_dirs.pop();
        let has_same_language = candidates.iter().any(|c| c.language == r.language);
        for candidate in candidates {
            if has_same_language && candidate.language != r.language {
                continue;
            }
            let mut score = 0f64;
            if candidate.file_path == r.file_path {
                score += 100.0;
            }
            score += Self::path_proximity_from_dirs(&ref_dirs, &candidate.file_path) as f64;
            if candidate.language == r.language {
                score += 50.0;
            } else {
                score -= 80.0;
            }
            if r.reference_kind == "calls"
                && (candidate.kind == "function" || candidate.kind == "method")
            {
                score += 25.0;
            }
            if r.reference_kind == "instantiates"
                && matches!(candidate.kind.as_str(), "class" | "struct" | "union" | "interface")
            {
                score += 25.0;
            }
            if r.reference_kind == "decorates" {
                if candidate.kind == "function" || candidate.kind == "method" {
                    score += 25.0;
                } else if candidate.kind == "class" || candidate.kind == "interface" {
                    score += 15.0;
                }
            }
            if candidate.is_exported {
                score += 10.0;
            }
            if candidate.file_path == r.file_path && candidate.start_line > 0 {
                let distance = (candidate.start_line - r.line).abs() as f64;
                score += (20.0 - distance / 10.0).max(0.0);
            }
            if score > best_score {
                best_score = score;
                best = Some(candidate.clone());
            }
        }
        best
    }

    /// matchByExactName (name-matcher.ts).
    fn match_by_exact_name(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let bare_js = self.is_bare_js_call(r)?;
        let all_named: Vec<Rc<KNode>> = self
            .nodes_by_name(&r.reference_name)?
            .iter()
            .cloned()
            .map(Rc::new)
            .collect();
        let mut candidates = self.apply_language_gate(all_named, r);
        candidates.retain(|n| n.kind != "import");
        // Nested locals reachable only from inside their container (#1230).
        let mut kept: Vec<Rc<KNode>> = Vec::with_capacity(candidates.len());
        for n in candidates.into_iter() {
            if self.is_lexically_reachable(&n, r)? {
                kept.push(n);
            }
        }
        let mut candidates = kept;
        candidates.retain(|n| !is_inheritance_ref(&r.reference_kind) || is_supertype_target_kind(&n.kind));
        candidates.retain(|n| r.reference_kind != "imports" || is_importable_kind(&n.kind));
        {
            let mut kept: Vec<Rc<KNode>> = Vec::with_capacity(candidates.len());
            for n in candidates.into_iter() {
                if r.reference_kind != "imports"
                    || n.file_path == r.file_path
                    || !is_esm_family(&n.language)
                    || !self.is_sealed_module(&n.file_path)?
                {
                    kept.push(n);
                }
            }
            candidates = kept;
        }
        candidates.retain(|n| !(bare_js && n.kind == "method"));
        if bare_js {
            let locally_bound =
                self.is_locally_bound_js_name(&r.reference_name, &r.file_path, Some(r.line))?;
            if locally_bound {
                candidates.retain(|n| n.file_path == r.file_path);
            }
        }

        // A name bound to a bare external import can land only on a same-file
        // definition (#1709).
        if candidates.iter().any(|n| n.file_path != r.file_path)
            && self.is_bound_to_bare_import(r)?
        {
            candidates.retain(|n| n.file_path == r.file_path);
        }

        // C/C++ call-site form.
        let Some(cpp_form) = self.apply_cpp_call_site_form(r, candidates)? else {
            return Ok(None);
        };
        let candidates = cpp_form;
        if candidates.is_empty() {
            return Ok(None);
        }
        if candidates.len() == 1 {
            if !self.is_cross_file_reachable(&candidates[0], r)? {
                return Ok(None);
            }
            if bare_js && !is_bare_call_target_kind(&candidates[0].kind) {
                return Ok(None);
            }
            let cross = candidates[0].language != r.language;
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: if cross { 0.5 } else { 0.9 },
                resolved_by: "exact-match",
            }));
        }
        if candidates.len() as i64 > self.ambiguous_ceiling {
            return Ok(None);
        }
        let Some(best) = self.find_best_match(r, &candidates) else {
            return Ok(None);
        };
        if bare_js && !is_bare_call_target_kind(&best.kind) {
            return Ok(None);
        }
        if self.is_cross_file_reachable(&best, r)? {
            let proximity = Self::compute_path_proximity(&r.file_path, &best.file_path);
            return Ok(Some(KCand {
                node: best,
                confidence: if proximity >= 30 { 0.7 } else { 0.4 },
                resolved_by: "exact-match",
            }));
        }
        Ok(None)
    }

    /// matchFuzzy (name-matcher.ts).
    fn match_fuzzy(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        if self.is_bound_to_bare_import(r)? {
            return Ok(None);
        }
        if let Some(form) = self.cpp_bare_call_form(r)? {
            if matches!(form, "qualified" | "explicit-member" | "this-member" | "free-args") {
                return Ok(None);
            }
        }
        let candidates = self.nodes_by_lower_name(&r.reference_name)?;
        let callable: Vec<Rc<KNode>> = candidates
            .iter()
            .filter(|n| matches!(n.kind.as_str(), "function" | "method" | "class"))
            .cloned()
            .map(Rc::new)
            .collect();
        let gated = self.apply_language_gate(callable, r);
        let same_language: Vec<Rc<KNode>> = gated
            .iter()
            .filter(|n| n.language == r.language)
            .cloned()
            .collect();
        let final_candidates = if same_language.is_empty() {
            gated
        } else {
            same_language
        };
        if final_candidates.len() == 1 {
            let only = final_candidates[0].clone();
            let reachable = self.is_lexically_reachable(&only, r)?
                && self.is_visible_across_files(&only, r)?
                && self.is_cross_file_reachable(&only, r)?;
            let bare_decline = self.is_bare_js_call(r)?
                && (only.kind == "method"
                    || (only.file_path != r.file_path
                        && self.is_locally_bound_js_name(
                            &r.reference_name,
                            &r.file_path,
                            Some(r.line),
                        )?));
            let reachable = reachable && !bare_decline && self.is_lexically_reachable(&only, r)?;
            if reachable {
                let cross = only.language != r.language;
                return Ok(Some(KCand {
                    node: only,
                    confidence: if cross { 0.3 } else { 0.5 },
                    resolved_by: "fuzzy",
                }));
            }
        }
        Ok(None)
    }

    /// matchReference restricted to the bare-name slice: every strategy
    /// before exact-name keys on a separator a bare name cannot carry, so
    /// the pipeline is exact → fuzzy.
    fn match_reference_bare(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        if let Some(c) = self.match_by_exact_name(r)? {
            return Ok(Some(c));
        }
        self.match_fuzzy(r)
    }

    // -----------------------------------------------------------------------
    // Gates (index.ts) + alias forwarding (alias-binding.ts)
    // -----------------------------------------------------------------------

    /// gateLanguage (index.ts): drop import/name results crossing a family.
    fn gate_language(&self, cand: Option<KCand>, r: &ResolveRefIn) -> Option<KCand> {
        let cand = cand?;
        let tgt = cand.node.language.as_str();
        // getLanguageFromNodeId is the node's language — already in hand.
        if tgt == "markdown" || r.language == "markdown" {
            return Some(cand);
        }
        if (r.reference_kind == "references" || r.reference_kind == "function_ref")
            && !same_language_family(tgt, &r.language)
        {
            return None;
        }
        if r.reference_kind == "imports" && crosses_known_family(tgt, &r.language) {
            return None;
        }
        Some(cand)
    }

    /// gateTargetKind (index.ts): the imports/inheritance target-kind gates
    /// plus the out-of-repo import check.
    fn gate_target_kind(&mut self, cand: KCand, r: &ResolveRefIn) -> Result<Option<KCand>> {
        if r.reference_kind == "imports" {
            return Ok(if is_importable_kind(&cand.node.kind) {
                Some(cand)
            } else {
                None
            });
        }
        if !is_inheritance_ref(&r.reference_kind) {
            return Ok(Some(cand));
        }
        if !is_supertype_target_kind(&cand.node.kind) {
            return Ok(None);
        }
        if self.is_bound_to_out_of_repo_import(r)? {
            return Ok(None);
        }
        Ok(Some(cand))
    }

    /// aliasTargetName + resolveAliasBinding (alias-binding.ts), memberName
    /// fixed to null (bare names carry no member segment).
    fn resolve_alias_binding(&mut self, alias_node: &KNode) -> Result<Option<Rc<KNode>>> {
        if !is_alias_binding_kind(&alias_node.kind) {
            return Ok(None);
        }
        let sig = alias_node.signature.as_deref().unwrap_or("").trim();
        let target_name = BARE_ALIAS_RE
            .captures(sig)
            .map(|c| c.get(1).unwrap().as_str().to_string());
        let Some(target_name) = target_name else { return Ok(None) };
        if target_name == alias_node.name {
            return Ok(None);
        }
        let candidates: Vec<KNode> = self
            .nodes_by_name(&target_name)?
            .iter()
            .filter(|n| is_callable_kind(&n.kind))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        let same_file: Vec<&KNode> = candidates
            .iter()
            .filter(|n| n.file_path == alias_node.file_path)
            .collect();
        if same_file.len() == 1 {
            return Ok(Some(Rc::new(same_file[0].clone())));
        }
        if same_file.len() > 1 {
            return Ok(None);
        }
        Ok(if candidates.len() == 1 {
            Some(Rc::new(candidates[0].clone()))
        } else {
            None
        })
    }

    // -----------------------------------------------------------------------
    // The pipeline — resolveOneInner restricted to migrated+bare refs, then
    // resolveOne's gateTargetKind + calls alias-forward.
    // -----------------------------------------------------------------------

    /// is_bare_name: the eligibility shape — no separator any skipped
    /// strategy keys on. Leading `$` stays (arkts `$r` builtins are handled);
    /// a `$` elsewhere could feed the R method pattern's `[\w.]+\$` arm.
    fn name_is_bare(name: &str) -> bool {
        !name.is_empty()
            && !name
                .char_indices()
                .any(|(i, c)| matches!(c, '.' | ':' | '/' | '\\' | '#' | '(' | ')') || (c == '$' && i > 0))
    }

    /// Necessary condition for matchJsStoreBindingCall (name-matcher.ts):
    /// both store-binding arms bind the ref's own name through `const` — a
    /// destructure (`const {a} = X.getState()`) or a selector alias
    /// (`const a = useStore(s => s.a)`). Absent that shape the matcher cannot
    /// fire, so the kernel may adjudicate the ref itself. The whole file is
    /// scanned (the destructure can span lines); false positives only cost a
    /// TS fallback, never a wrong verdict.
    fn file_could_store_bind(&mut self, r: &ResolveRefIn) -> Result<bool> {
        if r.reference_kind != "calls" || !is_js_family(&r.language) {
            return Ok(false);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let text = lines.join("\n");
        let name = regex::escape(&r.reference_name);
        let re = self.cached_regex(&format!(
            "\\bconst\\s*(?:\\{{[^{{}}]*\\b{}\\b|{}\\b)", name, name
        ))?;
        Ok(re.is_match(&text))
    }

    /// The `function_ref` block of resolveOneInner (index.ts). TS order:
    /// prefilter → this.-member arm (non-bare, unreachable here) →
    /// resolveViaImport with a target-kind gate → matchFunctionRef. A
    /// prefilter miss routes to matchJsStoreBindingCall, which is
    /// `calls`-gated — dead for function_ref — and frameworks never run on
    /// this path, so the miss is terminal either way.
    fn resolve_function_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
        let pre_pass =
            self.has_any_possible_match(&r.reference_name) || self.matches_any_import(r)?;
        if !pre_pass {
            return Ok(ResolveOutcome::unresolved());
        }
        // An import hit resolves only when its target is a callable value —
        // function/method, or a Python class (bareClassOk). A gated-out
        // import is discarded, not pooled, before the name matcher runs.
        let import_cand = self.resolve_via_import(r)?;
        let import_result = self.gate_language(import_cand, r);
        if let Some(c) = import_result {
            if c.node.kind == "function"
                || c.node.kind == "method"
                || (r.language == "python" && c.node.kind == "class")
            {
                return self.finish(r, c, None, true);
            }
        }
        let name_cand = self.match_function_ref_bare(r)?;
        match self.gate_language(name_cand, r) {
            Some(c) => self.finish(r, c, None, true),
            None => Ok(ResolveOutcome::unresolved()),
        }
    }

    /// matchFunctionRef, bare arm (name-matcher.ts): name-exact
    /// function/method nodes in the ref's language family — plus Python
    /// classes — excluding the origin node; JS/TS/ArkTS/C++/Python/PHP match
    /// functions only (a bare identifier there is never a method value).
    /// Same-file wins by earliest line; cross-file is unique-or-drop.
    fn match_function_ref_bare(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let bare_fn_only = matches!(
            r.language.as_str(),
            "typescript" | "tsx" | "javascript" | "jsx" | "arkts" | "cpp" | "python" | "php"
        );
        let bare_class_ok = r.language == "python";
        let mut candidates: Vec<Rc<KNode>> = self
            .nodes_by_name(&r.reference_name)?
            .iter()
            .filter(|n| {
                (n.kind == "function"
                    || (!bare_fn_only && n.kind == "method")
                    || (bare_class_ok && n.kind == "class"))
                    && same_language_family(&n.language, &r.language)
                    && n.id != r.from_node_id
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        // Swift implicit-self: a bare identifier names a method only of the
        // enclosing type; same-named methods elsewhere are parameter
        // collisions. Free functions are unaffected; top-level code has no
        // implicit self, so method targets drop entirely there.
        if r.language == "swift" && candidates.iter().any(|n| n.kind == "method") {
            let class_prefix = match self.node_by_id(&r.from_node_id)? {
                Some(from) => match from.qualified_name.rfind("::") {
                    Some(sep) if sep > 0 => Some(from.qualified_name[..sep].to_string()),
                    _ => None,
                },
                None => None,
            };
            candidates.retain(|n| {
                if n.kind != "method" {
                    return true;
                }
                let Some(cp) = &class_prefix else { return false };
                match n.qualified_name.rfind("::") {
                    Some(msep) if msep > 0 => {
                        let mp = &n.qualified_name[..msep];
                        mp == cp.as_str()
                            || mp.ends_with(&format!("::{cp}"))
                            || cp.ends_with(&format!("::{mp}"))
                    }
                    _ => false,
                }
            });
            if candidates.is_empty() {
                return Ok(None);
            }
        }
        // Same-file definition wins; same-name overloads in one file are the
        // same conceptual symbol — first by position for determinism
        // (min_by_key keeps the first minimum, matching TS's `<=` reduce).
        let same_file: Vec<Rc<KNode>> = candidates
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        if !same_file.is_empty() {
            // Swift: several same-named METHODS in one file are an overload
            // family — a bare identifier is a same-named parameter, not a
            // method value. A single method still resolves.
            if r.language == "swift"
                && same_file.len() > 1
                && same_file.iter().all(|n| n.kind == "method")
            {
                return Ok(None);
            }
            let target = same_file.iter().min_by_key(|n| n.start_line).unwrap().clone();
            return Ok(Some(KCand {
                node: target,
                confidence: if same_file.len() == 1 { 0.95 } else { 0.9 },
                resolved_by: "function-ref",
            }));
        }
        // Cross-file: only an unambiguous match resolves.
        if candidates.len() == 1 {
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: 0.8,
                resolved_by: "function-ref",
            }));
        }
        Ok(None)
    }

    fn resolve_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
        // ref_is_eligible, split so the passthrough reason names the gate.
        if !is_migrated_language(&r.language) {
            return Ok(ResolveOutcome::passthrough("ineligible:lang"));
        }
        if !Self::name_is_bare(&r.reference_name) {
            return Ok(ResolveOutcome::passthrough("ineligible:name"));
        }
        if r.file_path.is_empty() {
            return Ok(ResolveOutcome::passthrough("ineligible:path"));
        }

        // resolveOneInner, bare slice:
        //   builtin/external → CFML/jvm/razor/phpStatic arms all dead →
        //   prefilter → frameworks (TS) → boundReceiver (dead) → chain guard
        //   (dead) → viaImport → name-match → post-checks → first-max.
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        // The store-binding matcher stays in TS (source-reading): it can fire
        // on a prefilter miss AND short-circuits matchByExactName's candidate
        // list, so a JS bare call whose file const-binds its name must
        // passthrough wherever it would otherwise settle.
        if self.is_bare_js_call(r)? && self.file_could_store_bind(r)? {
            return Ok(ResolveOutcome::passthrough("store-bind"));
        }
        // `function_ref` (#756) has a dedicated, strictly-gated TS path that
        // never reaches frameworks or the fuzzy matchers — resolve it here
        // the same way (import, then the name-matcher's bare arm). The
        // `Cls::member` and `this.member` shapes are non-bare and stay on
        // the TS side via the eligibility gate.
        if r.reference_kind == "function_ref" {
            return self.resolve_function_ref(r);
        }
        // nix-path/arkts-dot/erlang-arity arms are dead for migrated bare
        // names; the claimsReference arm is evaluated natively — a claimed
        // name still reaches the framework resolvers through the TS path.
        let pre_pass =
            self.has_any_possible_match(&r.reference_name) || self.matches_any_import(r)?;
        if !pre_pass {
            return Ok(if self.framework_claims(&r.reference_name) {
                ResolveOutcome::passthrough("claimed")
            } else {
                ResolveOutcome::unresolved()
            });
        }

        let mut cands: Vec<KCand> = Vec::new();
        let import_cand = self.resolve_via_import(r)?;
        let import_result = self.gate_language(import_cand, r);
        let mut final_import: Option<KCand> = None;
        if let Some(c) = import_result {
            if c.confidence >= 0.9 {
                final_import = Some(c);
            } else {
                cands.push(c);
            }
        }

        if let Some(c) = final_import {
            // ≥0.9 import wins over everything below (and over framework
            // candidates < 0.9; a ≥0.9 framework hit still displaces it on
            // the TS side when frameworks are active).
            let winner = match self.gate_target_kind(c, r)? {
                Some(w) => w,
                None => {
                    // A gated-out ≥0.9 import resolves to nothing in the TS
                    // spine — its <0.9 framework candidates were discarded by
                    // the early return — but a ≥0.9 framework hit would have
                    // pre-empted the import entirely. Only the full TS spine
                    // can distinguish; hand it back when frameworks are live.
                    return Ok(if self.frameworks_active {
                        ResolveOutcome::passthrough("gated-import")
                    } else {
                        ResolveOutcome::unresolved()
                    });
                }
            };
            return self.finish(r, winner, None, true);
        }

        let name_cand = self.match_reference_bare(r)?;
        let name_result = self.gate_language(name_cand, r);
        if let Some(c) = name_result {
            // Post-pipeline visibility check on the committed target — a
            // rejection must NOT promote a runner-up (#1730/#1745).
            if self.is_visible_across_files(&c.node, r)? {
                cands.push(c);
            }
        }

        if cands.is_empty() {
            // The chain/php-prop defer arms need `().`/`this->` — dead.
            return Ok(if self.frameworks_active {
                // Framework candidates may still exist on the TS side —
                // report an empty candidate list rather than a verdict.
                ResolveOutcome::no_candidates()
            } else {
                ResolveOutcome::unresolved()
            });
        }

        // First-max on strict `>` — matches the TS candidates.reduce. The
        // reported list keeps the ORIGINAL candidate order [import?, name?]
        // so the TS merge can re-run the reduce with framework candidates
        // prepended.
        let reported = self.frameworks_active.then(|| {
            cands
                .iter()
                .map(|c| KernelCandidateOut {
                    target_node_id: c.node.id.clone(),
                    confidence: c.confidence,
                    resolved_by: c.resolved_by.to_string(),
                })
                .collect::<Vec<_>>()
        });
        let mut bi = 0usize;
        for i in 1..cands.len() {
            if cands[i].confidence > cands[bi].confidence {
                bi = i;
            }
        }
        let winner = cands.remove(bi);
        let winner = match self.gate_target_kind(winner, r)? {
            Some(w) => w,
            None => {
                // A gated-out kernel winner does not preclude a framework
                // candidate winning the merged first-max on the TS side.
                return Ok(ResolveOutcome {
                    candidates: reported,
                    ..ResolveOutcome::unresolved()
                });
            }
        };
        self.finish(r, winner, reported, false)
    }

    /// Apply resolveOne's tail — the `calls` alias forward — and emit the
    /// outcome. `candidates` is the reported kernel list (original order)
    /// for the TS framework merge when frameworks are active.
    fn finish(
        &mut self,
        r: &ResolveRefIn,
        winner: KCand,
        candidates: Option<Vec<KernelCandidateOut>>,
        is_final: bool,
    ) -> Result<ResolveOutcome> {
        let mut winner = winner;
        if r.reference_kind == "calls" {
            // memberName is null for a bare name (no '.').
            if let Some(forwarded) = self.resolve_alias_binding(&winner.node)? {
                if forwarded.id != winner.node.id {
                    winner = KCand {
                        node: forwarded,
                        confidence: winner.confidence.min(0.85),
                        resolved_by: winner.resolved_by,
                    };
                }
            }
        }
        Ok(ResolveOutcome::resolved(
            &winner.node,
            winner.confidence,
            winner.resolved_by,
            is_final,
            candidates,
        ))
    }
}
