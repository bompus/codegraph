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

/// Kernel pipeline eligibility. Mirrors BINDINGS_LANGUAGES
/// (src/extraction/kernel/index.ts) — `rust` included since it joined the
/// binding-emitting languages; its `use`-path handling was ported ahead of
/// the gate while it still resolved bindings-free.
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
            | "rust"
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
/// matchByFilePath's shape gate — `\.ext` (1–4 chars) or `.markdown` tail.
static FILE_PATH_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\.[A-Za-z][A-Za-z0-9]{0,3}|\.markdown)$").unwrap());
/// hasAnyPossibleMatch's bare-filename tail check (`\.[A-Za-z0-9]+$`).
static EXT_TAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.[A-Za-z0-9]+$").unwrap());
/// isBindingReceiverCall's name shape — `^.+\.[\w$]+$` (JS `\w` is ASCII).
static BOUND_RECEIVER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.+\.[A-Za-z0-9_$]+$").unwrap());
/// isBindingReceiverCall's excluded receiver roots — `^(this|self|super|cls)(\.|$)`.
static BOUND_ROOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:this|self|super|cls)(?:\.|$)").unwrap());
/// isUnresolvedJsMemberCall's retained chain — `^[A-Za-z_$][\w$]*(\.[A-Za-z_$][\w$]*){2,}$`.
static JS_MEMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*){2,}$").unwrap()
});
/// isUnresolvedJsMemberCall's excluded roots — `^(?:this|window)\.`.
static JS_MEMBER_ROOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:this|window)\.").unwrap());
/// CHAIN_SHAPE (index.ts) — `^(.+)\(\)\.(\w+)$`: a call-receiver chain.
static CHAIN_SHAPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.+\(\)\.[A-Za-z0-9_]+$").unwrap());
/// The chain arms' `<inner>().<method>` capture — `^(.+)\(\)\.(\w+)$`
/// with TS's ASCII `\w`.
static CALL_CHAIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+)\(\)\.([A-Za-z0-9_]+)$").unwrap());
/// CONSTRUCTS_VIA_BARE_CALL (name-matcher.ts) — languages where an
/// unprefixed capitalized `Foo(args)` constructs the class.
static CONSTRUCTS_VIA_BARE_CALL: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        ["kotlin", "swift", "scala", "dart", "pascal"].into_iter().collect()
    });
/// resolvePhpImportedStaticCall's receiver shape — `^(\w+)\.(\w+)$`.
static PHP_STATIC_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_]+)\.([A-Za-z0-9_]+)$").unwrap());

/// RUST_NON_PROJECT_FIELD_TYPES (name-matcher.ts): primitives and prelude
/// types — a field of one never names a project type.
static RUST_NON_PROJECT_FIELD_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "bool", "char", "str", "String", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16",
        "u32", "u64", "u128", "usize", "f32", "f64", "Self", "self",
    ]
    .into_iter()
    .collect()
});
/// Per-line comment stripper for rust decl scans — `//.*$` and `/\*.*?\*/`.
static RUST_LINE_COMMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"//.*$|/\*.*?\*/").unwrap());
/// RUST_STDLIB_ROOTS (import-resolver.ts): `use` roots that by definition
/// ship outside the repository.
static RUST_STDLIB_ROOTS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["std", "core", "alloc", "proc_macro"].into_iter().collect());
/// collectRustUseBindings' `use` statement matcher —
/// `(^|\n)\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);`.
static RUST_USE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\n)\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);").unwrap()
});
/// `use` alias tail — `^(.*?)\s+as\s+([A-Za-z_]\w*)$`.
static RUST_USE_ALIAS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*?)\s+as\s+([A-Za-z_]\w*)$").unwrap());

/// rustFieldTypeName (name-matcher.ts): reduce a field's declared type text
/// to the simple name a method call auto-derefs to. Unwraps only the layers
/// Rust's method-call auto-deref looks through (`&`, `Box`, `Rc`, `Arc`);
/// `Option`/`Vec`/etc. keep their own name and resolve to nothing. Generic
/// params, primitives, tuples, raw pointers, fn types → None.
fn rust_field_type_name(raw: &str) -> Option<String> {
    let mut t = raw.trim().to_string();
    loop {
        let before = t.clone();
        static REF_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^&\s*(?:'\w+\s+)?(?:mut\s+)?").unwrap());
        static PTR_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^(?:Box|Rc|Arc)\s*<\s*").unwrap());
        static DYN_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^(?:dyn|impl)\s+").unwrap());
        t = REF_RE.replace(&t, "").into_owned();
        t = PTR_RE.replace(&t, "").into_owned();
        t = DYN_RE.replace(&t, "").into_owned();
        if t == before {
            break;
        }
    }
    // Drop generic args, closing `>`s of unwrapped pointers, and trait-object
    // bounds (`dyn Source + Send`); keep the last path segment.
    static TRIM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[<>+].*$").unwrap());
    let t = TRIM_RE.replace(&t, "").trim().to_string();
    let seg = t.split("::").filter(|s| !s.is_empty()).last()?;
    static IDENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*$").unwrap());
    static GENERIC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z]$").unwrap());
    if !IDENT_RE.is_match(seg)
        || RUST_NON_PROJECT_FIELD_TYPES.contains(seg)
        || GENERIC_RE.is_match(seg)
    {
        return None;
    }
    Some(seg.to_string())
}

/// collectRustUseBindings (import-resolver.ts): `use` statements →
/// local-name → path map. `a::{b::{C, D}, E}` flattens one `{...}` level at
/// a time; `x as y` aliases; globs (`*`) are skipped.
fn collect_rust_use_bindings(content: &str) -> std::collections::HashMap<String, String> {
    fn expand(spec: &str) -> Vec<String> {
        let Some(open) = spec.find('{') else {
            return vec![spec.trim().to_string()];
        };
        let prefix = &spec[..open];
        let mut depth = 0i32;
        let mut close = -1i64;
        for (i, ch) in spec.char_indices().skip_while(|(i, _)| *i < open) {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    close = i as i64;
                    break;
                }
            }
        }
        if close < 0 {
            return vec![];
        }
        let close = close as usize;
        let suffix = &spec[close + 1..];
        let inner = &spec[open + 1..close];
        let mut parts = Vec::new();
        let mut depth2 = 0i32;
        let mut start = 0usize;
        for (i, ch) in inner.char_indices().chain(std::iter::once((inner.len(), '\0'))) {
            if ch == '{' {
                depth2 += 1;
            } else if ch == '}' {
                depth2 -= 1;
            }
            if i == inner.len() || (ch == ',' && depth2 == 0) {
                let seg = inner[start..i].trim();
                if !seg.is_empty() {
                    parts.push(seg.to_string());
                }
                start = i + 1;
            }
        }
        parts
            .into_iter()
            .flat_map(|p| expand(&format!("{}{}{}", prefix, p, suffix)))
            .collect()
    }

    let mut out = std::collections::HashMap::new();
    static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    for m in RUST_USE_RE.captures_iter(content) {
        let spec = WS_RE.replace_all(&m[1], " ");
        for flat in expand(&spec) {
            let alias = RUST_USE_ALIAS_RE.captures(&flat);
            let raw_path = alias
                .as_ref()
                .map(|a| a[1].trim())
                .unwrap_or_else(|| flat.trim());
            if raw_path.is_empty() || raw_path.ends_with('*') {
                continue;
            }
            let segments: Vec<&str> = raw_path
                .split("::")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            let Some(leaf) = segments.last() else {
                continue;
            };
            let local = alias.as_ref().map(|a| a[2].to_string()).unwrap_or_else(|| leaf.to_string());
            out.insert(local, segments.join("::"));
        }
    }
    out
}

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

/// isBindingReceiverCall (name-matcher.ts): a `calls` ref in a binding-
/// carrying language shaped `receiver.method`, excluding `()`-chains and
/// the self/this/super/cls receiver roots.
fn is_binding_receiver_call(r: &ResolveRefIn) -> bool {
    r.reference_kind == "calls"
        && (is_esm_family(&r.language)
            || matches!(
                r.language.as_str(),
                "python" | "go" | "java" | "kotlin" | "php" | "c" | "cpp"
            ))
        && BOUND_RECEIVER_RE.is_match(&r.reference_name)
        && !r.reference_name.contains("()")
        && !BOUND_ROOT_RE.is_match(&r.reference_name)
}

/// isUnresolvedJsMemberCall (name-matcher.ts): an untyped 2+-level member
/// chain in a JS-family calls ref — terminal null, never name-matched.
fn is_unresolved_js_member_call(r: &ResolveRefIn) -> bool {
    r.reference_kind == "calls"
        && matches!(
            r.language.as_str(),
            "typescript" | "tsx" | "javascript" | "jsx"
        )
        && !JS_MEMBER_ROOT_RE.is_match(&r.reference_name)
        && JS_MEMBER_RE.is_match(&r.reference_name)
}

/// preferCallSiteFile (name-matcher.ts): same-file candidates first,
/// preserving order; a no-op under <2 candidates or no same-file member.
fn prefer_call_site_file(nodes: Vec<Rc<KNode>>, call_site_file: &str) -> Vec<Rc<KNode>> {
    if nodes.len() < 2 || !nodes.iter().any(|n| n.file_path == call_site_file) {
        return nodes;
    }
    let (mut same, other): (Vec<Rc<KNode>>, Vec<Rc<KNode>>) = nodes
        .into_iter()
        .partition(|n| n.file_path == call_site_file);
    same.extend(other);
    same
}

/// STATIC_MEMBER_CONTAINERS (import-resolver.ts).
fn is_static_member_container(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "struct" | "union" | "interface" | "enum" | "trait" | "protocol"
    )
}

/// OBJECT_LITERAL_LANGUAGES (name-matcher.ts): object literals declare
/// callable members.
fn is_object_literal_language(lang: &str) -> bool {
    matches!(lang, "typescript" | "tsx" | "javascript" | "jsx" | "arkts")
}

/// splitCamelCase — receiver/class word split for matchMethodCall's
/// name-similarity scoring. `permissionEngine` → ["permission","Engine"];
/// `HTTPServer` → ["HTTP","Server"]; words of length ≤ 1 are dropped.
/// Single pass: a break goes before a capital that follows a lowercase, or
/// that starts a Capital+lowercase word inside a capital run — the same
/// boundaries the two sequential JS replaces produce.
fn split_camel_case(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut spaced = String::with_capacity(s.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            let prev_lower = chars[i - 1].is_ascii_lowercase();
            let acronym_tail = chars[i - 1].is_ascii_uppercase()
                && i + 1 < chars.len()
                && chars[i + 1].is_ascii_lowercase();
            if prev_lower || acronym_tail {
                spaced.push(' ');
            }
        }
        spaced.push(c);
    }
    spaced
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | ':' | '/' | '\\'))
        .filter(|w| w.encode_utf16().count() > 1)
        .map(str::to_string)
        .collect()
}

/// JS `line.replace(/\/\/.*$/, '').replace(/\/\*.*?\*\//g, '')` applied to a
/// single line: cut at the first `//`, then drop `/* … */` spans; an
/// unterminated `/*` survives verbatim (the replace simply has no match).
fn strip_line_comments(line: &str) -> String {
    let cut = line.find("//").map(|i| &line[..i]).unwrap_or(line);
    let mut out = String::with_capacity(cut.len());
    let mut rest = cut;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(rel) => rest = &rest[start + 2 + rel + 2..],
            None => {
                out.push_str(rest);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// buildLocalReceiverTypePatterns for the migrated set (name-matcher.ts).
/// Each entry is (pattern, guard): guard 1 reproduces the TS annotation
/// pattern's `(?![\w.$]|\s*(?:<[^>]*>)?\s*[\[|&])` lookahead in
/// infer_match_line; the go param-type lookahead `(?=\s*[,)]|\s*$)` is folded
/// into its pattern as a consuming suffix (equivalent — the capture cannot
/// absorb `[,)]`/EOL, and shrinking it can never satisfy the suffix either).
/// Languages outside the switch (c, cpp, unmigrated) get no patterns, same
/// as the TS `default: return []`.
fn local_receiver_type_patterns(language: &str, r: &str) -> Vec<(String, u8)> {
    let pats: Vec<(&str, u8)> = match language {
        "typescript" | "javascript" | "tsx" | "jsx" | "arkts" => vec![
            (r"\bR\b\s*=\s*new\s+([A-Za-z_$][A-Za-z0-9_.$]*)", 0),
            (r"\bR\b\s*:\s*([A-Z][A-Za-z0-9_.$]*)", 1),
        ],
        "python" => vec![
            (r"\bR\b\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(", 0),
            (r#"\bR\b\s*:\s*["']([A-Z][A-Za-z0-9_.]*)["']"#, 0),
            (r"\bR\b\s*:\s*([A-Z][A-Za-z0-9_.]*)", 0),
        ],
        "java" => vec![
            (r"\bR\b\s*=\s*new\s+([A-Za-z_][A-Za-z0-9_.]*)", 0),
            (r"\b([A-Z][A-Za-z0-9_.]*)\s+R\b\s*[=;,:)]", 0),
        ],
        "kotlin" => vec![
            (r"\bR\b\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(", 0),
            (r"\bR\b\s*:\s*([A-Z][A-Za-z0-9_.]*)", 0),
        ],
        "rust" => vec![
            // let r [mut] [: T] = [&][mut] Type::new()/Type{}/Type — a `let`
            // binding with an optional annotation; the capture is the
            // initializer's type, not the annotation's.
            (
                r"\blet\s+(?:mut\s+)?R\b(?:\s*:[^=]+)?=\s*&?(?:mut\s+)?([A-Z][A-Za-z0-9_]*)",
                0,
            ),
            // r : [&][mut] Type — a `let r: T` binding OR a typed parameter
            // (`fn f(r: &T)`, closure `|r: T|`) — the same shape (#1125).
            (r"\bR\s*:\s*&?(?:mut\s+)?([A-Z][A-Za-z0-9_]*)", 0),
        ],
        "go" => vec![
            (
                r"\bR\s+\*?([a-z_][A-Za-z0-9_]*\.[A-Z][A-Za-z0-9_]*)(?:\s*[,)]|\s*$)",
                0,
            ),
            (r"\bR\b\s*:=\s*&?([A-Za-z_][A-Za-z0-9_.]*)\s*\{", 0),
            (r"\bvar\s+R\s+\*?([A-Za-z_][A-Za-z0-9_.]*)", 0),
            (r"\bR\s+\*?([A-Z][A-Za-z0-9_.]*)", 0),
        ],
        "php" => vec![
            (r"\$?R\b\s*=\s*new\s+([A-Za-z_\\][A-Za-z0-9_\\]*)", 0),
            (r"\b([A-Za-z_\\][A-Za-z0-9_\\]*)\s+&?\$R\b", 0),
        ],
        _ => vec![],
    };
    pats.into_iter()
        .map(|(p, g)| (p.replace('R', r), g))
        .collect()
}

/// buildPhpPropertyTypePatterns — only property-shaped declarations qualify
/// for `$this->prop` receivers (typed/promoted property, `new` assignment).
fn php_property_type_patterns(r: &str) -> Vec<(String, u8)> {
    vec![
        (
            format!(
                r"\b(?:(?:private|protected|public|readonly|static|final)(?:\(set\))?\s+)+\??([A-Za-z_\\][A-Za-z0-9_\\]*)\s+&?\${}\b",
                r
            ),
            0,
        ),
        (
            format!(r"\$this->{}\b\s*=\s*new\s+([A-Za-z_\\][A-Za-z0-9_\\]*)", r),
            0,
        ),
    ]
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
    return_type: Option<String>,
    /// JSON `string[]` (queries.ts safeJsonParse) — None when absent/malformed.
    type_parameters: Option<Vec<String>>,
    /// JSON `string[]` like type_parameters — Kotlin `expect`/`actual` etc.
    decorators: Option<Vec<String>>,
}

impl KNode {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let raw_tps: Option<String> = row.get("type_parameters")?;
        let raw_decs: Option<String> = row.get("decorators")?;
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
            return_type: row.get("return_type")?,
            type_parameters: raw_tps.as_deref().and_then(parse_json_string_array),
            decorators: raw_decs.as_deref().and_then(parse_json_string_array),
        })
    }
}

/// safeJsonParse for the `["a","b"]` shape type_parameters is stored as —
/// returns None on anything that is not a flat array of JSON strings.
fn parse_json_string_array(raw: &str) -> Option<Vec<String>> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut rest = inner;
    loop {
        rest = rest.trim_start();
        let s = rest.strip_prefix('"')?;
        let mut val = String::new();
        let mut chars = s.char_indices();
        let mut escaped = false;
        let mut end = 0usize;
        let mut closed = false;
        while let Some((i, c)) = chars.next() {
            if escaped {
                val.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let (_, h) = chars.next()?;
                            code = code * 16 + h.to_digit(16)?;
                        }
                        char::from_u32(code)?
                    }
                    other => other,
                });
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = i;
                closed = true;
                break;
            } else {
                val.push(c);
            }
        }
        if !closed || escaped {
            return None;
        }
        rest = &s[end + 1..];
        out.push(val);
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        rest = rest.strip_prefix(',')?;
    }
    Some(out)
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

/// rustSelfModuleDir (import-resolver.ts): the directory under which the
/// current file's module declares its SUBMODULES — `mod.rs`/`lib.rs`/
/// `main.rs` own their directory; `foo.rs`'s submodules live in `foo/`.
fn rust_self_module_dir(from_file: &str) -> String {
    let base = pos_basename(from_file);
    let dir = pos_dirname(from_file);
    if base == "mod.rs" || base == "lib.rs" || base == "main.rs" {
        return dir.to_string();
    }
    pos_normalize(&format!(
        "{}/{}",
        dir,
        base.strip_suffix(".rs").unwrap_or(base)
    ))
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

/// `receiver.charAt(0).toUpperCase() + receiver.slice(1)` — JS full case
/// mapping on the first char (to_uppercase may widen, e.g. ß→SS, as does JS).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// splitAnchor (name-matcher.ts): `path#anchor` → (path, anchor?).
fn split_anchor(name: &str) -> (&str, Option<&str>) {
    match name.find('#') {
        Some(i) => (&name[..i], Some(&name[i + 1..])),
        None => (name, None),
    }
}

/// splitFileSymbol (name-matcher.ts): `path::symbol` → (path, symbol?).
fn split_file_symbol(name: &str) -> (&str, Option<&str>) {
    match name.find("::") {
        Some(i) => (&name[..i], Some(&name[i + 2..])),
        None => (name, None),
    }
}

/// decodeURIComponent — %XX sequences → UTF-8 bytes; `+` stays literal.
/// JS throws on malformed input and the caller keeps the original, so a bad
/// escape or non-UTF-8 payload returns the input verbatim.
fn uri_component_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return s.to_string();
            }
            let pair = (bytes[i + 1] as char).to_digit(16).zip((bytes[i + 2] as char).to_digit(16));
            let Some((hi, lo)) = pair else {
                return s.to_string();
            };
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// normalizeMarkdownAnchor (name-matcher.ts): decode → lowercase → strip
/// `<…>` tag spans → keep letters/numbers/space/dash → trim → `-`-join.
/// `char::is_alphanumeric` approximates `[\p{L}\p{N}]` (it also keeps marks —
/// unreachable in practice behind the file-path arm).
fn normalize_markdown_anchor(anchor: &str) -> String {
    let lowered = uri_component_decode(anchor).to_lowercase();
    // /<[^>]+>/g: drop `<` through the next `>`; an unclosed `<` survives.
    let mut no_tags = String::with_capacity(lowered.len());
    let mut rest = lowered.as_str();
    while let Some(open) = rest.find('<') {
        no_tags.push_str(&rest[..open]);
        match rest[open..].find('>') {
            Some(close) => rest = &rest[open + close + 1..],
            None => {
                no_tags.push('<');
                rest = &rest[open + 1..];
            }
        }
    }
    no_tags.push_str(rest);
    let filtered: String = no_tags
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '-')
        .collect();
    // split_whitespace skips edge whitespace — the JS `.trim()` is subsumed.
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join("-");
    if collapsed.is_empty() {
        "section".to_string()
    } else {
        collapsed
    }
}

/// pickClosestFileNode (name-matcher.ts): same-dir candidates first, then
/// strict-`>` argmax of path proximity + a same-language-family bonus.
fn pick_closest_file_node(candidates: &[Rc<KNode>], r: &ResolveRefIn) -> Rc<KNode> {
    let ref_dir = pos_dirname(&r.file_path);
    let same_dir: Vec<Rc<KNode>> = candidates
        .iter()
        .filter(|c| pos_dirname(&c.file_path) == ref_dir)
        .cloned()
        .collect();
    let pool: &[Rc<KNode>] = if same_dir.is_empty() { candidates } else { &same_dir };
    let mut best = pool[0].clone();
    let mut best_score = i64::MIN;
    for c in pool {
        let score = KernelResolver::compute_path_proximity(&r.file_path, &c.file_path)
            + if same_language_family(&c.language, &r.language) { 5 } else { 0 };
        if score > best_score {
            best_score = score;
            best = c.clone();
        }
    }
    best
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

/// Tri-state for the non-bare import slice: a mid-arm source read (alias /
/// imported-instance inference) is not a miss — the ref must go back through
/// the TS spine, which re-derives everything natively evaluated so far.
enum ViaImport {
    Hit(KCand),
    Miss,
    Punt(&'static str),
}

/// matchBoundReceiverCall's claim contract: `undefined` (unclaimed) is
/// filtered by the `is_binding_receiver_call` gate before this is consulted,
/// so a claimed ref is always Hit/Refused/Punt — terminal either way.
enum BoundClaim {
    Hit(KCand),
    Refused,
    Punt(&'static str),
}

/// Tri-state inside the ported matchMethodCall helpers: a positive match, a
/// provable `null` (the caller maps it to continue/refuse exactly like TS), or
/// a punt when the next step needs state the snapshot can't see — live
/// supertype edges, tree-sitter parsing, or an unported arm.
enum McRes {
    Hit(KCand),
    Null,
    Punt(&'static str),
}

/// resolveBoundType outcome: the resolved owner node, a provable no-owner, or
/// a punt when a reachable sub-arm is unported (e.g. the JVM FQN fallback
/// needs a qualified-name lookup the enclosing call can't reach — kept for
/// symmetry; resolveJvmImport is ported so this is currently unused-defensive).
enum BtRes {
    Owner(Rc<KNode>),
    Null,
    Punt(&'static str),
}

/// NON_TYPE_RECEIVER_TOKENS (name-matcher.ts) — loose captures that are never
/// a user-defined type.
static NON_TYPE_RECEIVER_TOKENS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        [
            "this", "self", "super", "new", "return", "await", "yield", "typeof",
            "null", "nil", "None", "true", "false", "True", "False", "undefined",
        ]
        .into_iter()
        .collect()
    });

/// GO_BUILTIN_FIELD_TYPES (name-matcher.ts) — Go field types that never name
/// a project type in the 2-hop field-chain matcher.
static GO_BUILTIN_FIELD_TYPES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        [
            "string", "bool", "byte", "rune", "error", "any", "int", "int8",
            "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32",
            "uint64", "uintptr", "float32", "float64", "complex64",
            "complex128", "chan", "map", "func", "struct", "interface",
        ]
        .into_iter()
        .collect()
    });

/// CPP_NON_TYPE_TOKENS (name-matcher.ts) — C++ keywords/control-flow tokens
/// that can appear right before a receiver and must never type it.
static CPP_NON_TYPE_TOKENS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        [
            "return", "if", "else", "for", "while", "do", "switch", "case",
            "default", "break", "continue", "goto", "throw", "new", "delete",
            "co_await", "co_yield", "co_return", "static_cast", "const_cast",
            "dynamic_cast", "reinterpret_cast", "sizeof", "alignof", "typeid",
            "and", "or", "not", "xor",
        ]
        .into_iter()
        .collect()
    });

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
                         signature, visibility, is_exported, return_type, \
                         type_parameters, decorators";

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

/// TypeScript primitive type names — js-builtins.ts's TS_PRIMITIVE_TYPES.
/// Distinct from JS_BUILT_INS on purpose: a primitive receiver joins the
/// builtins at the same bail points (an inferred `string` must not fall
/// through to a project method named `split`).
static TS_PRIMITIVE_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "string", "number", "boolean", "bigint", "symbol", "void", "undefined", "null",
        "never", "unknown", "any", "object",
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

// GO_STDLIB_PACKAGES (index.ts isBuiltInOrExternal `pkg.member` arm) — live
// for non-bare names (`pkg.Member` where pkg is a stdlib import).
static GO_STDLIB_PACKAGES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "fmt", "os", "io", "net", "http", "log", "math", "sort", "sync", "time", "path",
        "bytes", "strings", "strconv", "errors", "context", "json", "xml", "csv", "html",
        "template", "regexp", "reflect", "runtime", "testing", "flag", "bufio", "crypto",
        "encoding", "filepath", "hash", "mime", "rand", "signal", "sql", "syscall",
        "unicode", "unsafe", "atomic", "binary", "debug", "exec", "heap", "ring",
        "scanner", "tar", "zip", "gzip", "zlib", "tls", "url", "user", "pprof", "trace",
        "ast", "build", "parser", "printer", "token", "types", "cgo", "plugin", "race",
        "ioutil", "utilruntime", "utilwait", "utilnet",
    ]
    .into_iter()
    .collect()
});

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
        // Go: stdlib package member access (`fmt.Println`) is external — but
        // only when the ref is not a binding-receiver call: a bound receiver
        // owns `pkg.member` names (isBindingReceiverCall, index.ts).
        if r.language == "go" && !is_binding_receiver_call(r) {
            if let Some(dot) = name.find('.') {
                if dot > 0 && GO_STDLIB_PACKAGES.contains(&name[..dot]) {
                    return true;
                }
            }
            if GO_BUILT_INS.contains(name) {
                return true;
            }
        }
        if r.language == "c" || r.language == "cpp" {
            // `std::` prefix — never a user-defined qualified name.
            if name.starts_with("std::") {
                return true;
            }
            if C_BUILT_INS.contains(name) || CPP_BUILT_INS.contains(name) {
                return !self.has_any_possible_match(name);
            }
        }
        false
    }

    /// hasAnyPossibleMatch reduced to the bare-name form: every separator-
    /// dependent arm (dot/colon/slash/`$`/path) is dead by definition of
    /// `is_bare_name`, leaving the direct known-name check.
    /// hasAnyPossibleMatch (index.ts) — the full check: direct name, then the
    /// receiver/member segments around `.`/`::`/`:`/`$`, then the path tail.
    /// Every separator branch is dead for bare names (the previous callers'
    /// slice); the non-bare c/cpp imports arm needs them.
    fn has_any_possible_match(&self, name: &str) -> bool {
        let path_name = name.replace('\\', "/");
        let path_name = path_name.split('#').next().unwrap_or("");
        if self.known_names.contains(name) {
            return true;
        }
        if path_name != name && self.known_names.contains(path_name) {
            return true;
        }
        if let Some(dot_idx) = name.find('.') {
            if dot_idx > 0 {
                let (receiver, member) = (&name[..dot_idx], &name[dot_idx + 1..]);
                if self.known_names.contains(receiver) || self.known_names.contains(member) {
                    return true;
                }
                let capitalized = capitalize_first(receiver);
                if self.known_names.contains(capitalized.as_str()) {
                    return true;
                }
                if let Some(last_dot) = name.rfind('.') {
                    if last_dot > dot_idx {
                        let tail = &name[last_dot + 1..];
                        if !tail.is_empty() && self.known_names.contains(tail) {
                            return true;
                        }
                    }
                }
            }
        }
        if let Some(colon_idx) = name.find("::") {
            if colon_idx > 0 {
                let (receiver, member) = (&name[..colon_idx], &name[colon_idx + 2..]);
                if self.known_names.contains(receiver) || self.known_names.contains(member) {
                    return true;
                }
                if let Some(last_colon) = name.rfind("::") {
                    if last_colon > colon_idx {
                        let tail = &name[last_colon + 2..];
                        if !tail.is_empty() && self.known_names.contains(tail) {
                            return true;
                        }
                    }
                }
            }
        }
        for sep in [':', '$'] {
            if sep == ':' && name.contains("::") {
                continue;
            }
            if let Some(sep_idx) = name.find(sep) {
                if sep_idx > 0 {
                    let (receiver, member) = (&name[..sep_idx], &name[sep_idx + 1..]);
                    if self.known_names.contains(member) || self.known_names.contains(receiver) {
                        return true;
                    }
                    let capitalized = capitalize_first(receiver);
                    if self.known_names.contains(capitalized.as_str()) {
                        return true;
                    }
                }
            }
        }
        if let Some(slash_idx) = path_name.rfind('/') {
            if slash_idx > 0 && self.known_names.contains(&path_name[slash_idx + 1..]) {
                return true;
            }
        }
        if !path_name.contains('/')
            && EXT_TAIL_RE.is_match(path_name)
            && self.known_names.contains(path_name)
        {
            return true;
        }
        false
    }

    /// matchesAnyImport — `localName === name` or the `localName.` prefix arm
    /// (the latter is dead for bare names).
    fn matches_any_import(&mut self, r: &ResolveRefIn) -> Result<bool> {
        let imports = self.import_mappings(&r.file_path)?;
        Ok(imports.iter().any(|i| {
            i.local_name == r.reference_name
                || r.reference_name
                    .strip_prefix(&i.local_name)
                    .is_some_and(|rest| rest.starts_with('.'))
        }))
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
            let matches_bare = imp.local_name == r.reference_name;
            let matches_qualified = r
                .reference_name
                .starts_with(&format!("{}.", imp.local_name));
            if !matches_bare && !matches_qualified {
                continue;
            }
            let member_name = if matches_bare {
                imp.local_name.clone()
            } else {
                Self::js_slice(&r.reference_name, Self::utf16_len(&imp.local_name) + 1)
                    .to_string()
            };
            let fqn_path = format!("{}{}", imp.source.replace('.', "/"), ext);
            let candidates = self.nodes_by_name(&member_name)?;
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
            // member, the part before is the owner class. Bare matches only —
            // a qualified ref already named the member above.
            if matches_bare {
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
        // Qualified refs resolve by path, not through a bare import binding.
        if r.reference_name.contains("::") || r.reference_name.contains('.') {
            return Ok(false);
        }
        // Rust: a `use` path rooted at a stdlib crate ships outside the repo
        // by definition. Deliberately NOT "the module path doesn't resolve":
        // `pub use other_crate::{ports}` re-exports have no file to walk yet
        // are in-repo — a 2015-edition crate-relative path that walks to a
        // real file shadows a stdlib root and stays local.
        if r.language == "rust" {
            let Some(content) = self.read_file(&r.file_path) else {
                return Ok(false);
            };
            let bindings = collect_rust_use_bindings(&content.join("\n"));
            let Some(use_path) = bindings.get(&r.reference_name) else {
                return Ok(false);
            };
            let segments: Vec<&str> = use_path.split("::").collect();
            if segments.len() < 2 || !RUST_STDLIB_ROOTS.contains(segments[0]) {
                return Ok(false);
            }
            return Ok(
                self.resolve_rust_module_file(&segments[..segments.len() - 1], &r.file_path)?
                    .is_none(),
            );
        }
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
        // `!name.includes('.')` — dead on the bare path, live for non-bare
        // callers (a `Foo::bar` calls ref carries no '.').
        if r.reference_name.contains('.') {
            return Ok(false);
        }
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
    // Non-bare c/cpp `imports` refs — the #include-path slice (resolveOneInner
    // restricted). Measured on the linux corpus this leg resolves ~388k of
    // the ~393k ineligible:name edges; everything it can't prove punts to the
    // full TS spine rather than guessing.
    // -----------------------------------------------------------------------

    /// The resolveOneInner ordering for `(c|cpp, 'imports', non-bare)`:
    /// builtin → prefilter → frameworks → boundReceiver → viaImport →
    /// nameMatch. For this slice jvmImport/razor/phpStatic are
    /// language-gated dead, boundReceiver is calls-gated dead, the chain
    /// guards need calls + `().`, and viaImport's first branch IS the c/cpp
    /// include arm (≥0.9 or nothing — its <0.9 candidate path can't fire).
    /// The nameMatch tail (qualifiedName → cppChain → methodCall →
    /// exactName → fuzzy) stays in TS: a `member-tail` passthrough
    /// reproduces the full-spine verdict exactly.
    fn resolve_c_include_import_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        let pre_pass = self.has_any_possible_match(&r.reference_name)
            || self.matches_any_import(r)?
            || self.framework_claims(&r.reference_name);
        if !pre_pass {
            // The prefilter-miss fallback is matchJsStoreBindingCall — gated
            // to JS calls — so a c/cpp ref dies here exactly as in TS.
            return Ok(ResolveOutcome::unresolved());
        }
        let import_hit = self.resolve_via_import(r)?;
        let import_hit = self.gate_language(import_hit, r);
        if let Some(c) = import_hit {
            let winner = match self.gate_target_kind(c, r)? {
                Some(w) => w,
                None => {
                    // A gated-out ≥0.9 import can still lose to a ≥0.9
                    // framework hit — only the full TS spine distinguishes.
                    return Ok(if self.frameworks_active {
                        ResolveOutcome::passthrough("gated-import")
                    } else {
                        ResolveOutcome::unresolved()
                    });
                }
            };
            return self.finish(r, winner, None, true);
        }
        // matchReference's name arms in TS order: filePath, qualifiedName
        // on a file-path miss (`#include <sys/ioctl.h>` names its own import
        // node's qualified name — filePath can't see it, qualifiedName can),
        // then methodCall's unbound arm (an `a.h` name is a dot-shape
        // receiver). The chain arms can't fire — include names never carry
        // `()` — so they are skipped; exactName/fuzzy stay in TS.
        let name_cand = match self.match_by_file_path(r)? {
            Some(c) => Some(c),
            None => match self.match_by_qualified_name(r)? {
                Some(c) => Some(c),
                None => match self.match_method_call_free(r)? {
                    McRes::Hit(c) => Some(c),
                    McRes::Punt(p) => return Ok(ResolveOutcome::passthrough(p)),
                    McRes::Null => None,
                },
            },
        };
        let Some(c) = self.gate_language(name_cand, r) else {
            return Ok(ResolveOutcome::passthrough("member-tail"));
        };
        // The nameMatch result takes the cross-file visibility post-check; a
        // rejection leaves the later arms live in TS, so punt — never verdict.
        if !self.is_visible_across_files(&c.node, r)? {
            return Ok(ResolveOutcome::passthrough("member-tail"));
        }
        let Some(winner) = self.gate_target_kind(c, r)? else {
            return Ok(ResolveOutcome::passthrough("member-tail"));
        };
        // A file-path hit is a nameMatch candidate — it never early-returns
        // in TS, it first-maxes against framework candidates. Under active
        // frameworks report it for the merge instead of verdicting.
        if self.frameworks_active {
            let reported = vec![KernelCandidateOut {
                target_node_id: winner.node.id.clone(),
                confidence: winner.confidence,
                resolved_by: winner.resolved_by.to_string(),
            }];
            return self.finish(r, winner, Some(reported), false);
        }
        self.finish(r, winner, None, false)
    }

    /// matchByFilePath (name-matcher.ts): path-shaped (`a/b.h`) or
    /// extension-bearing bare (`Foo.h`) names → `file` nodes.
    fn match_by_file_path(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let normalized = r.reference_name.replace('\\', "/");
        let (path_and_symbol, anchor) = split_anchor(&normalized);
        let (path_wo_anchor, symbol_name) = split_file_symbol(path_and_symbol);
        if !path_wo_anchor.contains('/') && !FILE_PATH_EXT_RE.is_match(path_wo_anchor) {
            return Ok(None);
        }
        let file_name = pos_basename(path_wo_anchor);
        if file_name.is_empty() {
            return Ok(None);
        }
        let file_nodes: Vec<Rc<KNode>> = self
            .nodes_by_name(file_name)?
            .iter()
            .filter(|n| n.kind == "file")
            .cloned()
            .map(Rc::new)
            .collect();
        if file_nodes.is_empty() {
            return Ok(None);
        }
        if let Some(symbol_name) = symbol_name.filter(|s| !s.is_empty()) {
            if let Some(symbol) =
                self.find_symbol_in_referenced_file(path_wo_anchor, symbol_name, &file_nodes)?
            {
                return Ok(Some(KCand {
                    node: symbol,
                    confidence: 0.99,
                    resolved_by: "file-path",
                }));
            }
        }
        if let Some(anchor) = anchor.filter(|a| !a.is_empty()) {
            if let Some(anchored) =
                self.find_anchored_markdown_section(path_wo_anchor, anchor, &file_nodes)?
            {
                return Ok(Some(KCand {
                    node: anchored,
                    confidence: 0.98,
                    resolved_by: "file-path",
                }));
            }
        }
        if let Some(exact) = file_nodes
            .iter()
            .find(|n| n.qualified_name == path_wo_anchor || n.file_path == path_wo_anchor)
        {
            return Ok(Some(KCand {
                node: exact.clone(),
                confidence: 0.95,
                resolved_by: "file-path",
            }));
        }
        let suffix_matches: Vec<Rc<KNode>> = file_nodes
            .iter()
            .filter(|n| {
                n.qualified_name.ends_with(path_wo_anchor) || n.file_path.ends_with(path_wo_anchor)
            })
            .cloned()
            .collect();
        if !suffix_matches.is_empty() {
            return Ok(Some(KCand {
                node: pick_closest_file_node(&suffix_matches, r),
                confidence: 0.85,
                resolved_by: "file-path",
            }));
        }
        if file_nodes.len() == 1 {
            return Ok(Some(KCand {
                node: file_nodes[0].clone(),
                confidence: 0.7,
                resolved_by: "file-path",
            }));
        }
        Ok(None)
    }

    /// findSymbolInReferencedFile (name-matcher.ts): `path::symbol` — search
    /// the path-matched files (or the lone candidate) for the symbol.
    fn find_symbol_in_referenced_file(
        &mut self,
        path_wo_anchor: &str,
        symbol_name: &str,
        file_nodes: &[Rc<KNode>],
    ) -> Result<Option<Rc<KNode>>> {
        let candidate_files: Vec<Rc<KNode>> = file_nodes
            .iter()
            .filter(|n| {
                n.qualified_name == path_wo_anchor
                    || n.file_path == path_wo_anchor
                    || n.qualified_name.ends_with(path_wo_anchor)
                    || n.file_path.ends_with(path_wo_anchor)
            })
            .cloned()
            .collect();
        let search: &[Rc<KNode>] = if !candidate_files.is_empty() {
            &candidate_files
        } else if file_nodes.len() == 1 {
            file_nodes
        } else {
            &[]
        };
        for file_node in search {
            let nodes = self.nodes_in_file(&file_node.file_path)?;
            let normalized_symbol = symbol_name.replace('/', ".");
            let file_prefix = format!("{}::{}", file_node.file_path, normalized_symbol);
            let colon_tail = format!("::{}", normalized_symbol);
            let dot_tail = format!(".{}", normalized_symbol);
            if let Some(exact) = nodes.iter().find(|n| {
                n.name == normalized_symbol
                    || n.qualified_name == file_prefix
                    || n.qualified_name.ends_with(&colon_tail)
                    || n.qualified_name.ends_with(&dot_tail)
            }) {
                return Ok(Some(Rc::new(exact.clone())));
            }
            let last_part = normalized_symbol.split(['.', ':']).next_back().unwrap_or("");
            if last_part.is_empty() {
                continue;
            }
            if let Some(by_last) = nodes.iter().find(|n| {
                n.name == last_part
                    && matches!(
                        n.kind.as_str(),
                        "function" | "method" | "class" | "module" | "constant" | "variable"
                    )
            }) {
                return Ok(Some(Rc::new(by_last.clone())));
            }
        }
        Ok(None)
    }

    /// findAnchoredMarkdownSection (name-matcher.ts): `path#anchor` → the
    /// markdown section module node.
    fn find_anchored_markdown_section(
        &mut self,
        path_wo_anchor: &str,
        anchor: &str,
        file_nodes: &[Rc<KNode>],
    ) -> Result<Option<Rc<KNode>>> {
        let normalized_anchor = normalize_markdown_anchor(anchor);
        for file_node in file_nodes.iter().filter(|n| {
            n.qualified_name == path_wo_anchor
                || n.file_path == path_wo_anchor
                || n.qualified_name.ends_with(path_wo_anchor)
                || n.file_path.ends_with(path_wo_anchor)
        }) {
            let section_qn = format!("{}#{}", file_node.file_path, normalized_anchor);
            if let Some(exact) = self
                .nodes_by_qualified_name(&section_qn)?
                .iter()
                .find(|n| n.kind == "module" && n.language == "markdown")
            {
                return Ok(Some(Rc::new(exact.clone())));
            }
            if let Some(section) = self
                .nodes_in_file(&file_node.file_path)?
                .iter()
                .find(|n| {
                    n.kind == "module"
                        && n.language == "markdown"
                        && n.qualified_name == section_qn
                })
            {
                return Ok(Some(Rc::new(section.clone())));
            }
        }
        Ok(None)
    }

    // -----------------------------------------------------------------------
    // Non-bare migrated refs — resolveOneInner's member slice (§5.16).
    // Ported arms: builtin/external, prefilter, phpStatic, boundReceiver's
    // DB sub-arms (br:import + claim refusals), viaImport's member descent
    // (static member + object literal), filePath, qualifiedName. Arms that
    // read source (receiver-type inference, object-literal alias/instance
    // member, store bindings) or defer (chains, this.members) punt so the TS
    // spine reproduces them exactly.
    // -----------------------------------------------------------------------

    /// The `$`-receiver guard both phpStatic and boundReceiver share:
    /// `getFileLines(file)?.[line-1]?.slice(column).startsWith('$')`.
    fn ref_line_starts_with_dollar(&mut self, r: &ResolveRefIn) -> bool {
        let Some(lines) = self.read_file(&r.file_path) else {
            return false;
        };
        let Some(line) = (r.line - 1)
            .try_into()
            .ok()
            .and_then(|i: usize| lines.get(i))
        else {
            return false;
        };
        Self::js_slice(line, r.column.max(0) as usize).starts_with('$')
    }

    /// resolvePhpImportedStaticCall (import-resolver.ts): `Alias.method()`
    /// where `Alias` is a PHP class import — resolves to the imported class's
    /// qualified member. Returns None when the arm does not claim the ref;
    /// Some carries the terminal outcome (this arm precedes frameworks).
    fn resolve_php_imported_static(&mut self, r: &ResolveRefIn) -> Result<Option<ResolveOutcome>> {
        if r.language != "php" || r.reference_kind != "calls" {
            return Ok(None);
        }
        let Some(call) = PHP_STATIC_CALL_RE.captures(&r.reference_name) else {
            return Ok(None);
        };
        let receiver = call.get(1).unwrap().as_str();
        let member = call.get(2).unwrap().as_str();
        let imports = self.import_mappings(&r.file_path)?;
        let Some(imp) = imports.iter().find(|i| i.local_name == receiver) else {
            return Ok(None);
        };
        let imp_source = imp.source.clone();
        if self.ref_line_starts_with_dollar(r) {
            return Ok(None);
        }
        let fqn = imp_source.trim_start_matches('\\');
        let type_name = match fqn.rfind('\\') {
            Some(sep) => format!("{}::{}", &fqn[..sep], &fqn[sep + 1..]),
            None => fqn.to_string(),
        };
        let owners: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(&type_name)?
            .iter()
            .filter(|n| n.language == "php" && is_static_member_container(&n.kind))
            .map(|n| Rc::new(n.clone()))
            .collect();
        // Claimed: a single owner is required; ambiguity or absence is a
        // terminal refusal, never a name-match fallthrough.
        if owners.len() != 1 {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let owner = owners[0].clone();
        let member_qn = format!("{}::{}", owner.qualified_name, member);
        let methods: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(&member_qn)?
            .iter()
            .filter(|n| {
                n.language == "php" && n.kind == "method" && n.file_path == owner.file_path
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        if methods.len() != 1 {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let gated = self.gate_language(
            Some(KCand {
                node: methods[0].clone(),
                confidence: 0.95,
                resolved_by: "import",
            }),
            r,
        );
        let Some(cand) = gated else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        // gateTargetKind is a no-op for `calls` refs (imports/inheritance arms
        // only); the alias forward applies inside finish.
        self.finish(r, cand, None, true).map(Some)
    }

    // -----------------------------------------------------------------------
    // Stage-2 member-access arms — matchMethodCall(requireReceiverEvidence)
    // and its source-backed inference helpers (name-matcher.ts). Every helper
    // returns McRes: Hit for a proven edge, Null for a provable TS `null`, and
    // Punt when the next step needs state the snapshot can't see — live
    // supertype edges (getSupertypes/getSupertypeNodes), tree-sitter parsing
    // (inferGuardedReceiver / inferIterationReceiver), or unported arms.
    // -----------------------------------------------------------------------

    fn ref_clone(r: &ResolveRefIn) -> ResolveRefIn {
        ResolveRefIn {
            row_id: r.row_id,
            from_node_id: r.from_node_id.clone(),
            reference_name: r.reference_name.clone(),
            reference_kind: r.reference_kind.clone(),
            line: r.line,
            column: r.column,
            candidates: r.candidates.clone(),
            file_path: r.file_path.clone(),
            language: r.language.clone(),
            failure_reason: r.failure_reason.clone(),
        }
    }

    fn mc_to_claim(res: McRes) -> BoundClaim {
        match res {
            McRes::Hit(c) => BoundClaim::Hit(c),
            McRes::Null => BoundClaim::Refused,
            McRes::Punt(p) => BoundClaim::Punt(p),
        }
    }

    /// enclosingScopeStartLine — 1-based start line of the tightest
    /// function/method node enclosing `line` in `file_path`.
    fn enclosing_scope_start_line(
        &mut self,
        file_path: &str,
        language: &str,
        line: i64,
    ) -> Result<i64> {
        let mut start = 1i64;
        for n in self.nodes_in_file(file_path)?.iter() {
            if n.kind != "function" && n.kind != "method" {
                continue;
            }
            if n.language != language {
                continue;
            }
            if n.start_line <= line && n.end_line >= line && n.start_line >= start {
                start = n.start_line;
            }
        }
        Ok(start)
    }

    /// normalizeInferredTypeName — strip generics + `&`/`*`, take the last
    /// `.`/`:`-separated segment, reject non-type tokens.
    fn normalize_inferred_type_name(&mut self, raw: &str) -> Result<Option<String>> {
        let generics = self.cached_regex("<[^>]*>")?;
        let cleaned = generics.replace_all(raw, "");
        let cleaned: String = cleaned
            .chars()
            .filter(|c| *c != '&' && *c != '*')
            .collect::<String>()
            .trim()
            .to_string();
        let Some(seg) = cleaned
            .split(['.', ':'])
            .rfind(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        if NON_TYPE_RECEIVER_TOKENS.contains(seg) {
            return Ok(None);
        }
        Ok(Some(seg.to_string()))
    }

    /// First per-pattern match for one source line — mirrors `matchLine` in
    /// inferLocalReceiverType: per pattern only the first match position is
    /// considered (non-global `.match`), then the captured type must survive
    /// normalizeInferredTypeName. `guard == 1` reproduces the TS annotation
    /// pattern's negative lookahead `(?![\w.$]|\s*(?:<[^>]*>)?\s*[\[|&])`; a
    /// shrunk capture can't satisfy it (the released char is itself `[\w.$]`),
    /// so checking the greedy capture's tail at each start position is exact.
    fn infer_match_line(
        &mut self,
        line: &str,
        pats: &[(String, u8)],
        preserve: bool,
    ) -> Result<Option<String>> {
        if Self::utf16_len(line) > 10_000 {
            return Ok(None);
        }
        for (pat, guard) in pats {
            let re = self.cached_regex(pat)?;
            for caps in re.captures_iter(line) {
                let Some(m1) = caps.get(1) else { break };
                if m1.as_str().is_empty() {
                    break;
                }
                if *guard == 1 {
                    let rest = &line[caps.get(0).unwrap().end()..];
                    if rest.chars().next().is_some_and(|c| {
                        c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$'
                    }) {
                        continue;
                    }
                    let tail = self.cached_regex(r"^\s*(?:<[^>]*>)?\s*[\[|&]")?;
                    if tail.is_match(rest) {
                        continue;
                    }
                }
                match self.normalize_inferred_type_name(m1.as_str())? {
                    Some(t) => {
                        return Ok(Some(if preserve {
                            m1.as_str().to_string()
                        } else {
                            t
                        }));
                    }
                    None => break,
                }
            }
        }
        Ok(None)
    }

    /// inferLocalReceiverType (name-matcher.ts) — backward declaration scan
    /// bounded by the enclosing scope. The TS incremental-scan memo is a pure
    /// optimization: it returns the highest matching line in [start..call],
    /// identical to the plain backward scan reproduced here.
    fn infer_local_receiver_type(
        &mut self,
        receiver: &str,
        site: &ResolveRefIn,
        preserve: bool,
    ) -> Result<Option<String>> {
        // CFML scope prefixes are dead — cfml/cfscript aren't claim-eligible.
        let mut scan_receiver = receiver.to_string();
        let mut component_scoped = false;
        let mut php_property = false;
        if site.language == "php" {
            let re = self.cached_regex("^this->(.+)$")?;
            if let Some(m) = re.captures(&scan_receiver) {
                scan_receiver = m[1].to_string();
                component_scoped = true;
                php_property = true;
            }
        }
        let escaped = regex::escape(&scan_receiver);
        let pats = if php_property {
            php_property_type_patterns(&escaped)
        } else {
            local_receiver_type_patterns(&site.language, &escaped)
        };
        if pats.is_empty() {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&site.file_path) else {
            return Ok(None);
        };
        if lines.is_empty() {
            return Ok(None);
        }
        let call_idx = (site.line - 1).clamp(0, lines.len() as i64 - 1) as usize;
        let start_idx = if component_scoped {
            0usize
        } else {
            let scope = self.enclosing_scope_start_line(
                &site.file_path,
                &site.language,
                site.line,
            )?;
            call_idx.min((scope - 1).max(0) as usize)
        };
        for i in (start_idx..=call_idx).rev() {
            if let Some(t) = self.infer_match_line(&lines[i], &pats, preserve)? {
                return Ok(Some(t));
            }
        }
        if component_scoped {
            for line in lines.iter().skip(call_idx + 1) {
                if let Some(t) = self.infer_match_line(line, &pats, preserve)? {
                    return Ok(Some(t));
                }
            }
        }
        if php_property {
            return self.infer_php_assigned_property_type(&escaped, &lines, call_idx);
        }
        Ok(None)
    }

    /// inferPhpAssignedPropertyType — `$this->prop = $var` second-chance
    /// typing through the assigned variable's own declaration.
    fn infer_php_assigned_property_type(
        &mut self,
        escaped_prop: &str,
        lines: &[String],
        call_idx: usize,
    ) -> Result<Option<String>> {
        let assign_re = self.cached_regex(&format!(
            r"\$this->{}\b\s*=\s*\$([A-Za-z0-9_]+)\b",
            escaped_prop
        ))?;
        let func_re = self.cached_regex(r"\bfunction\b")?;
        let mut assign_idx: Option<usize> = None;
        let mut var_name: Option<String> = None;
        for i in (0..=call_idx).rev() {
            let line = &lines[i];
            if line.is_empty() || Self::utf16_len(line) > 10_000 {
                continue;
            }
            if let Some(m) = assign_re.captures(line) {
                assign_idx = Some(i);
                var_name = Some(m[1].to_string());
                break;
            }
        }
        if var_name.is_none() {
            for (i, line) in lines.iter().enumerate().skip(call_idx + 1) {
                if line.is_empty() || Self::utf16_len(line) > 10_000 {
                    continue;
                }
                if let Some(m) = assign_re.captures(line) {
                    assign_idx = Some(i);
                    var_name = Some(m[1].to_string());
                    break;
                }
            }
        }
        let (Some(ai), Some(vn)) = (assign_idx, var_name) else {
            return Ok(None);
        };
        let pats = local_receiver_type_patterns("php", &regex::escape(&vn));
        for i in (0..=ai).rev() {
            let line = &lines[i];
            if !line.is_empty() && Self::utf16_len(line) <= 10_000 {
                if let Some(t) = self.infer_match_line(line, &pats, false)? {
                    return Ok(Some(t));
                }
            }
            if !line.is_empty() && func_re.is_match(line) {
                break;
            }
        }
        Ok(None)
    }

    /// normalizeCppTypeName — strip cv-qualifiers/keywords, refs, generics;
    /// take the last `::` segment (or the qualified name when preserving).
    fn normalize_cpp_type_name(&mut self, raw: &str, preserve: bool) -> Result<Option<String>> {
        let kw = self
            .cached_regex(r"\b(?:const|volatile|mutable|typename|class|struct)\b")?
            .replace_all(raw, " ");
        let no_ref = self.cached_regex(r"[&*]+")?.replace_all(&kw, " ");
        let no_gen = self.cached_regex(r"<[^>]*>")?.replace_all(&no_ref, " ");
        let normalized = no_gen.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            return Ok(None);
        }
        let parts: Vec<&str> = normalized.split("::").filter(|s| !s.is_empty()).collect();
        let Some(last) = parts.last() else { return Ok(None) };
        if CPP_NON_TYPE_TOKENS.contains(last) {
            return Ok(None);
        }
        Ok(Some(if preserve {
            parts.join("::")
        } else {
            last.to_string()
        }))
    }

    fn cpp_last_segment(name: &str) -> String {
        let parts: Vec<&str> = name.split("::").filter(|s| !s.is_empty()).collect();
        parts.last().map(|s| s.to_string()).unwrap_or_else(|| name.to_string())
    }

    /// buildDeclaratorRegex — `Type receiver` requiring a declarator
    /// terminator. The JS lookahead `(?=[;=,)\[{(]|$)` is post-checked on the
    /// remainder: the greedy `\s*` tail can't shrink into a passing position.
    fn cpp_declarator_match(&mut self, line: &str, escaped_receiver: &str) -> Result<Option<String>> {
        let re = self.cached_regex(&format!(
            r"([A-Za-z_][A-Za-z0-9_:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*\b{}\b\s*",
            escaped_receiver
        ))?;
        for caps in re.captures_iter(line) {
            let Some(m0) = caps.get(0) else { continue };
            let rest = &line[m0.end()..];
            let ok = match rest.chars().next() {
                None => true,
                Some(c) => matches!(c, ';' | '=' | ',' | ')' | '[' | '{' | '('),
            };
            if ok {
                if let Some(m1) = caps.get(1) {
                    return Ok(Some(m1.as_str().to_string()));
                }
            }
        }
        Ok(None)
    }

    /// inferCppReceiverType — backward declarator scan, `auto` deduction via
    /// the initializer, then same-named header fallback (.h/.hpp/.hxx).
    fn infer_cpp_receiver_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
        depth: u32,
        preserve: bool,
    ) -> Result<Option<String>> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        if lines.is_empty() {
            return Ok(None);
        }
        let call_idx = (r.line - 1).clamp(0, lines.len() as i64 - 1) as usize;
        let escaped = regex::escape(receiver);
        let receiver_re = self.cached_regex(&format!(r"\b{}\b", escaped))?;
        for i in (0..=call_idx).rev() {
            let line = &lines[i];
            if line.is_empty() || !receiver_re.is_match(line) {
                continue;
            }
            if let Some(decl) = self.cpp_declarator_match(line, &escaped)? {
                match self.normalize_cpp_type_name(&decl, preserve)? {
                    Some(t) if t == "auto" || t.ends_with("::auto") => {
                        if let Some(init) =
                            self.infer_cpp_auto_initializer_type(line, receiver, r, depth)?
                        {
                            return Ok(Some(init));
                        }
                        // An undeduced `auto` local shadows earlier decls.
                        return Ok(None);
                    }
                    Some(t) => return Ok(Some(t)),
                    None => {}
                }
            }
        }
        let ext_re = self.cached_regex(r"(?i)\.(?:c|cc|cpp|cxx)$")?;
        let mut header_candidates: Vec<String> = Vec::new();
        for ext in [".h", ".hpp", ".hxx"] {
            let candidate = ext_re.replace(&r.file_path, ext).to_string();
            if !header_candidates.contains(&candidate) && candidate != r.file_path {
                header_candidates.push(candidate);
            }
        }
        for header in header_candidates {
            if !self.file_exists(&header) {
                continue;
            }
            let Some(header_lines) = self.read_file(&header) else {
                continue;
            };
            for line in header_lines.iter() {
                if !receiver_re.is_match(line) {
                    continue;
                }
                let Some(decl) = self.cpp_declarator_match(line, &escaped)? else {
                    continue;
                };
                if let Some(t) = self.normalize_cpp_type_name(&decl, preserve)? {
                    if t != "auto" {
                        return Ok(Some(t));
                    }
                }
            }
        }
        Ok(None)
    }

    /// inferCppAutoInitializerType — `auto x = <init>;` deduction.
    fn infer_cpp_auto_initializer_type(
        &mut self,
        line: &str,
        receiver: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Result<Option<String>> {
        let m = self
            .cached_regex(&format!(r"\b{}\b\s*=\s*([^;]+)", regex::escape(receiver)))?
            .captures(line)
            .and_then(|c| c.get(1).map(|g| g.as_str().trim().to_string()));
        let Some(init) = m else { return Ok(None) };
        let neu = self.cached_regex(r"^new\s+([A-Za-z_][A-Za-z0-9_:]*)")?;
        if let Some(n) = neu.captures(&init) {
            return Ok(Some(Self::cpp_last_segment(&n[1])));
        }
        let call = self
            .cached_regex(r"^([A-Za-z_][A-Za-z0-9_:]*(?:\s*<[^>;]*>)?)\s*\(")?;
        if let Some(c) = call.captures(&init) {
            let collapsed: String = c[1].split_whitespace().collect();
            return self.resolve_cpp_call_result_type(&collapsed, r, depth + 1);
        }
        Ok(None)
    }

    /// resolveCppCallResultType — make_unique/make_shared, single-level
    /// member call, callee returnType, direct construction.
    fn resolve_cpp_call_result_type(
        &mut self,
        inner: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Result<Option<String>> {
        if depth > 3 {
            return Ok(None);
        }
        let expr = inner.trim();
        let make = self
            .cached_regex(r"(?:^|::)(?:make_unique|make_shared)\s*<\s*([A-Za-z_][A-Za-z0-9_]*)")?;
        if let Some(m) = make.captures(expr) {
            return Ok(Some(m[1].to_string()));
        }
        if let Some(dot) = expr.rfind('.') {
            if dot > 0 {
                let recv = &expr[..dot];
                let method = &expr[dot + 1..];
                if recv.contains('.') || recv.contains('(') || recv.contains("::") {
                    return Ok(None);
                }
                let Some(recv_type) =
                    self.infer_cpp_receiver_type(recv, r, depth + 1, false)?
                else {
                    return Ok(None);
                };
                return self.lookup_callee_return_type(&format!("{}::{}", recv_type, method), r);
            }
        }
        if let Some(ret) = self.lookup_callee_return_type(expr, r)? {
            return Ok(Some(ret));
        }
        if self.cpp_class_exists(expr, r)? {
            return Ok(Some(Self::cpp_last_segment(expr)));
        }
        Ok(None)
    }

    /// lookupCalleeReturnType — the indexed `return_type` of `Cls::method` or
    /// a free function, language-filtered.
    fn lookup_callee_return_type(
        &mut self,
        callee: &str,
        r: &ResolveRefIn,
    ) -> Result<Option<String>> {
        let (method, cls) = if callee.contains("::") {
            let parts: Vec<&str> = callee.split("::").filter(|s| !s.is_empty()).collect();
            let m = parts.last().copied().unwrap_or(callee);
            let joined = parts[..parts.len() - 1].join("::");
            // `if (cls)` — '' is falsy, so `::x` falls to the function path.
            (m.to_string(), if joined.is_empty() { None } else { Some(joined) })
        } else {
            (callee.to_string(), None)
        };
        let candidates: Vec<KNode> = self
            .nodes_by_name(&method)?
            .iter()
            .filter(|n| {
                (n.kind == "method" || n.kind == "function")
                    && n.language == r.language
                    && n.return_type.as_deref().is_some_and(|t| !t.is_empty())
            })
            .cloned()
            .collect();
        if let Some(cls) = cls {
            let want = format!("{}::{}", cls, method);
            let hit = candidates.iter().find(|n| {
                n.qualified_name == want
                    || n.qualified_name.ends_with(&format!("::{}", want))
                    || want.ends_with(&format!("::{}", n.qualified_name))
            });
            return Ok(hit.and_then(|n| n.return_type.clone()));
        }
        Ok(candidates
            .iter()
            .find(|n| n.kind == "function")
            .and_then(|n| n.return_type.clone()))
    }

    /// cppClassExists — an aggregate type with this last `::` segment exists.
    fn cpp_class_exists(&mut self, name: &str, r: &ResolveRefIn) -> Result<bool> {
        let last = Self::cpp_last_segment(name);
        Ok(self.nodes_by_name(&last)?.iter().any(|n| {
            matches!(n.kind.as_str(), "class" | "struct" | "union") && n.language == r.language
        }))
    }

    /// importedFqnOf — the import mapping whose localName is the type.
    fn imported_fqn_of(&mut self, type_name: &str, r: &ResolveRefIn) -> Result<Option<String>> {
        Ok(self
            .import_mappings(&r.file_path)?
            .iter()
            .find(|i| i.local_name == type_name)
            .map(|i| i.source.clone()))
    }

    /// matchReference's chain arms in TS dispatch order — at most one runs
    /// per language: cppChain (c/cpp), scopedChain (php/rust), dottedChain
    /// (the dot-notation list). A provable `null` lets the member-tail punt
    /// reproduce the unported TS tail exactly.
    fn match_call_chain(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        match r.language.as_str() {
            "c" | "cpp" => self.match_cpp_call_chain(r),
            "php" | "rust" => self.match_scoped_call_chain(r),
            "java" | "kotlin" | "csharp" | "swift" | "go" | "scala" | "dart" | "objc"
            | "pascal" => self.match_dotted_call_chain(r),
            _ => Ok(McRes::Null),
        }
    }

    /// matchCppCallChain — `<inner>().<method>` where the inner call's
    /// return type is the receiver's type (#645); resolveMethodOnType
    /// validates, so a wrong inference yields no edge.
    fn match_cpp_call_chain(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        let Some(m) = CALL_CHAIN_RE.captures(&r.reference_name) else {
            return Ok(McRes::Null);
        };
        let inner = m.get(1).unwrap().as_str();
        let method = m.get(2).unwrap().as_str();
        let Some(cls) = self.resolve_cpp_call_result_type(inner, r, 0)? else {
            return Ok(McRes::Null);
        };
        self.resolve_method_on_type(&cls, method, r, 0.85, "instance-method", None)
    }

    /// matchScopedCallChain — `Cls::factory().method` static-factory chains
    /// (PHP `Cls::for($x)->m()`, Rust `Foo::new().bar()`); a `self` return
    /// marker resolves to the factory's own class (#608).
    fn match_scoped_call_chain(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        let Some(m) = CALL_CHAIN_RE.captures(&r.reference_name) else {
            return Ok(McRes::Null);
        };
        let inner = m.get(1).unwrap().as_str();
        let method = m.get(2).unwrap().as_str();
        if !inner.contains("::") {
            return Ok(McRes::Null);
        }
        let factory_class = &inner[..inner.rfind("::").unwrap()];
        let Some(ret) = self.lookup_callee_return_type(inner, r)? else {
            return Ok(McRes::Null);
        };
        let resolved = if ret == "self" { factory_class } else { ret.as_str() };
        self.resolve_method_on_type(resolved, method, r, 0.85, "instance-method", None)
    }

    /// matchDottedCallChain — `Foo.getInstance().bar` factory/fluent chains,
    /// Go's bare `New().Method`, and the objc/pascal convention arms
    /// (#645/#608). The Go bare-name fallback (exactName/fuzzy) is unported —
    /// the member-tail punt reproduces it.
    fn match_dotted_call_chain(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        let Some(m) = CALL_CHAIN_RE.captures(&r.reference_name) else {
            return Ok(McRes::Null);
        };
        let inner = m.get(1).unwrap().as_str();
        let method = m.get(2).unwrap().as_str();
        // TS `lastIndexOf('.') <= 0` — no dot, or a leading one.
        let last_dot = inner.rfind('.');
        if last_dot.is_none() || last_dot == Some(0) {
            if r.language == "go" {
                if let Some(ret) = self.lookup_callee_return_type(inner, r)? {
                    let fqn = self.imported_fqn_of(&ret, r)?;
                    return self.resolve_method_on_type(
                        &ret,
                        method,
                        r,
                        0.85,
                        "instance-method",
                        fqn.as_deref(),
                    );
                }
                return Ok(McRes::Punt("member-tail"));
            }
            if !CONSTRUCTS_VIA_BARE_CALL.contains(r.language.as_str())
                || !inner.as_bytes()[0].is_ascii_uppercase()
            {
                return Ok(McRes::Null);
            }
            let fqn = self.imported_fqn_of(inner, r)?;
            return self.resolve_method_on_type(
                inner,
                method,
                r,
                0.85,
                "instance-method",
                fqn.as_deref(),
            );
        }
        let last_dot = last_dot.unwrap();
        let factory_class = inner[..last_dot].split('.').next_back().unwrap();
        let factory_method = &inner[last_dot + 1..];
        if factory_class.is_empty() || factory_method.is_empty() {
            return Ok(McRes::Null);
        }
        let want = format!("{}::{}", factory_class, factory_method);
        let Some(ret) = self.lookup_callee_return_type(&want, r)? else {
            // objc `[X alloc]` / pascal `TFoo.Create` conventions — the
            // receiver's type is the class itself. Both unmigrated today
            // (unreachable); ported verbatim for fidelity.
            let first = factory_class.as_bytes()[0];
            if (r.language == "objc" && first.is_ascii_uppercase())
                || (r.language == "pascal" && matches!(first, b'T' | b'I'))
            {
                let fqn = self.imported_fqn_of(factory_class, r)?;
                return self.resolve_method_on_type(
                    factory_class,
                    method,
                    r,
                    0.8,
                    "instance-method",
                    fqn.as_deref(),
                );
            }
            return Ok(McRes::Null);
        };
        let fqn = self.imported_fqn_of(&ret, r)?;
        self.resolve_method_on_type(&ret, method, r, 0.85, "instance-method", fqn.as_deref())
    }

    /// resolveJvmImport (import-resolver.ts) — `imports`-kind java/kotlin FQN
    /// to a qualified-name node, KMP `expect` preferred on ties.
    fn resolve_jvm_import(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        if r.reference_kind != "imports" {
            return Ok(None);
        }
        if r.language != "java" && r.language != "kotlin" {
            return Ok(None);
        }
        let fqn = &r.reference_name;
        let Some(dot) = fqn.rfind('.') else { return Ok(None) };
        if dot == 0 {
            return Ok(None);
        }
        let (pkg, sym) = (&fqn[..dot], &fqn[dot + 1..]);
        if sym == "*" {
            return Ok(None);
        }
        let candidates = self.nodes_by_qualified_name(&format!("{}::{}", pkg, sym))?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let best = if candidates.len() == 1 {
            Rc::new(candidates[0].clone())
        } else {
            Self::pick_closest_jvm_candidate(&candidates, &r.file_path)
        };
        Ok(Some(KCand {
            node: best,
            confidence: 0.95,
            resolved_by: "import",
        }))
    }

    /// pickClosestJvmCandidate — shared-directory-prefix proximity, Kotlin
    /// Multiplatform `expect` preferred on a tie.
    fn pick_closest_jvm_candidate(candidates: &[KNode], from_path: &str) -> Rc<KNode> {
        let from_dirs: Vec<&str> = from_path.split('/').collect();
        let from_dirs = &from_dirs[..from_dirs.len().saturating_sub(1)];
        let shared = |p: &str| -> usize {
            let d: Vec<&str> = p.split('/').collect();
            let d = &d[..d.len().saturating_sub(1)];
            let mut n = 0;
            while n < from_dirs.len() && n < d.len() && from_dirs[n] == d[n] {
                n += 1;
            }
            n
        };
        let is_expect = |n: &KNode| {
            n.decorators
                .as_ref()
                .is_some_and(|ds| ds.iter().any(|d| d == "expect"))
        };
        let mut best = &candidates[0];
        let mut best_prox = shared(&best.file_path);
        for c in &candidates[1..] {
            let prox = shared(&c.file_path);
            if prox > best_prox || (prox == best_prox && is_expect(c) && !is_expect(best)) {
                best = c;
                best_prox = prox;
            }
        }
        Rc::new(best.clone())
    }

    /// resolveBoundType — the declared type's owner node: Java type-parameter
    /// bounds first, then its lexical binding, then (non-ESM) the visible
    /// unique candidate. Punt propagates from viaImport's source arms.
    fn resolve_bound_type(
        &mut self,
        ty: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Result<BtRes> {
        if depth > 4 {
            return Ok(BtRes::Null);
        }
        if r.language == "java" {
            let in_file = self.nodes_in_file(&r.file_path)?;
            let mut scopes: Vec<&KNode> = in_file
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "class" | "interface" | "method")
                        && n.start_line <= r.line
                        && n.end_line >= r.line
                        && (n.start_line != r.line || n.start_column <= r.column)
                        && (n.end_line != r.line || n.end_column >= r.column)
                })
                .collect();
            scopes.sort_by(|a, b| {
                (a.end_line - a.start_line)
                    .cmp(&(b.end_line - b.start_line))
                    .then(b.start_column.cmp(&a.start_column))
            });
            let bound_re =
                self.cached_regex(r"^[A-Za-z0-9_]+\s+extends\s+([A-Za-z0-9_.]+)$")?;
            for scope in scopes {
                let decl = scope.type_parameters.as_ref().and_then(|tps| {
                    tps.iter().find(|p| {
                        // split(/\s+/)[0] — leading whitespace yields ''.
                        p.split(|c: char| c.is_whitespace()).next() == Some(ty)
                    })
                });
                let Some(declaration) = decl else { continue };
                // An unbounded or self declaration shadows any outer bound.
                return match bound_re.captures(declaration) {
                    Some(m) if &m[1] != ty => {
                        let mut site = Self::ref_clone(r);
                        site.line = scope.start_line;
                        site.column = scope.start_column;
                        self.resolve_bound_type(&m[1], &site, depth + 1)
                    }
                    _ => Ok(BtRes::Null),
                };
            }
        }
        let bindings = self.bindings(&r.file_path)?;
        let binding = Self::innermost_binding(
            &bindings,
            ty.split('.').next().unwrap_or(ty),
            Some(r.line),
        );
        let mut owner_id: Option<String> = None;
        if let Some(b) = binding {
            if b.kind == "import" {
                let mut ref2 = Self::ref_clone(r);
                ref2.reference_name = ty.to_string();
                ref2.reference_kind = "references".to_string();
                let hit = if ty.contains('.') {
                    match self.resolve_via_import_member(&ref2)? {
                        ViaImport::Hit(c) => Some(c),
                        ViaImport::Miss => None,
                        ViaImport::Punt(p) => return Ok(BtRes::Punt(p)),
                    }
                } else {
                    self.resolve_via_import(&ref2)?
                };
                owner_id = hit.map(|c| c.node.id.clone());
                if owner_id.is_none() {
                    let mut ref3 = Self::ref_clone(r);
                    ref3.reference_name =
                        b.target_spec.clone().unwrap_or_else(|| ty.to_string());
                    ref3.reference_kind = "imports".to_string();
                    if let Some(c) = self.resolve_jvm_import(&ref3)? {
                        owner_id = Some(c.node.id.clone());
                    }
                }
            } else {
                owner_id = b.node_id.clone();
            }
        }
        let mut owner: Option<Rc<KNode>> = match &owner_id {
            Some(id) => self.node_by_id(id)?,
            None => None,
        };
        if binding.is_some_and(|b| b.kind == "import") && r.language == "php" {
            if let Some(spec) = binding.and_then(|b| b.target_spec.clone()) {
                let stripped = spec.strip_prefix('\\').unwrap_or(&spec);
                let qualified = match stripped.rfind('\\') {
                    Some(pos) if pos + 1 < stripped.len() => {
                        format!("{}::{}", &stripped[..pos], &stripped[pos + 1..])
                    }
                    _ => stripped.to_string(),
                };
                let owners: Vec<KNode> = self
                    .nodes_by_qualified_name(&qualified)?
                    .iter()
                    .filter(|n| {
                        n.language == "php"
                            && matches!(n.kind.as_str(), "class" | "interface" | "trait")
                    })
                    .cloned()
                    .collect();
                owner = if owners.len() == 1 {
                    Some(Rc::new(owners[0].clone()))
                } else {
                    None
                };
            }
        }
        if binding.is_none() && !is_esm_family(&r.language) {
            let raw = if ty.contains("::") {
                self.nodes_by_qualified_name(ty)?
            } else {
                self.nodes_by_name(ty)?
            };
            let mut candidates: Vec<Rc<KNode>> = Vec::new();
            for n in raw.iter() {
                if !matches!(
                    n.kind.as_str(),
                    "class" | "struct" | "interface" | "component" | "type_alias" | "union"
                ) || n.language != r.language {
                    continue;
                }
                if !self.is_visible_across_files(n, r)? {
                    continue;
                }
                candidates.push(Rc::new(n.clone()));
            }
            let local: Vec<Rc<KNode>> = candidates
                .iter()
                .filter(|n| n.file_path == r.file_path)
                .cloned()
                .collect();
            let namespace = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .find(|n| n.kind == "namespace")
                .map(|n| n.qualified_name.clone());
            let mut packages: Vec<String> = Vec::new();
            if r.language == "java" || r.language == "kotlin" {
                packages = self
                    .import_mappings(&r.file_path)?
                    .iter()
                    .filter(|i| i.is_namespace && i.source.ends_with(".*"))
                    .map(|i| i.source[..i.source.len() - 2].to_string())
                    .collect();
                if let Some(ns) = &namespace {
                    packages.insert(0, ns.clone());
                }
            }
            let package_candidates: Vec<Rc<KNode>> = candidates
                .into_iter()
                .filter(|n| {
                    if r.language == "python" {
                        return false;
                    }
                    if r.language == "go" {
                        return pos_dirname(&n.file_path) == pos_dirname(&r.file_path);
                    }
                    if r.language == "php" {
                        return n.qualified_name
                            == match &namespace {
                                Some(ns) => format!("{}::{}", ns, ty),
                                None => ty.to_string(),
                            };
                    }
                    if r.language == "java" || r.language == "kotlin" {
                        if namespace.is_none() && n.qualified_name == ty {
                            return true;
                        }
                        return packages
                            .iter()
                            .any(|pkg| n.qualified_name == format!("{}::{}", pkg, ty));
                    }
                    true
                })
                .collect();
            let visible = if !local.is_empty() {
                local
            } else {
                package_candidates
            };
            if visible.len() == 1 {
                owner = Some(visible[0].clone());
            }
        }
        match owner {
            Some(o)
                if matches!(
                    o.kind.as_str(),
                    "class" | "struct" | "interface" | "component" | "type_alias" | "union"
                ) =>
            {
                Ok(BtRes::Owner(o))
            }
            _ => Ok(BtRes::Null),
        }
    }

    /// matchBoundTypeMember — owner's own `QName::method` member. A miss is a
    /// PUNT, not a refusal: TS next walks live supertype edges the snapshot
    /// can't see, so the TS spine must re-derive the miss.
    fn match_bound_type_member(
        &mut self,
        ty: &str,
        method: &str,
        site: &ResolveRefIn,
    ) -> Result<McRes> {
        let owner = match self.resolve_bound_type(ty, site, 0)? {
            BtRes::Owner(o) => o,
            BtRes::Null => return Ok(McRes::Null),
            BtRes::Punt(p) => return Ok(McRes::Punt(p)),
        };
        let members: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, method))?
            .iter()
            .filter(|n| {
                n.kind == "method"
                    && same_language_family(&n.language, &site.language)
                    && (n.file_path == owner.file_path
                        || (site.language == "go"
                            && pos_dirname(&n.file_path) == pos_dirname(&owner.file_path))
                        || site.language == "cpp")
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        let member = if members.len() == 1 {
            members.into_iter().next()
        } else {
            members
                .into_iter()
                .find(|n| n.file_path == owner.file_path)
        };
        match member {
            Some(m) => Ok(McRes::Hit(KCand {
                node: m,
                confidence: 0.9,
                resolved_by: "instance-method",
            })),
            None => Ok(McRes::Punt("btm-supers")),
        }
    }

    /// resolveMethodOnType — `typeName::methodName` qualified-name suffix
    /// match with preferred-FQN and call-site disambiguation. Zero direct
    /// matches is a PUNT: TS falls into the live-edge supertype walk.
    fn resolve_method_on_type(
        &mut self,
        type_name: &str,
        method: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
        preferred_fqn: Option<&str>,
    ) -> Result<McRes> {
        let want = format!("{}::{}", type_name, method);
        let matches: Vec<Rc<KNode>> = self
            .nodes_by_name(method)?
            .iter()
            .filter(|m| {
                m.kind == "method"
                    && same_language_family(&m.language, &r.language)
                    && (m.qualified_name == want
                        || m.qualified_name.ends_with(&format!("::{}", want)))
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        if matches.is_empty() {
            return Ok(McRes::Punt("rmot-supers"));
        }
        if matches.len() > 1 {
            if let Some(fqn) = preferred_fqn {
                let ext = if r.language == "kotlin" { ".kt" } else { ".java" };
                let fqn_path = format!("{}{}", fqn.replace('.', "/"), ext);
                if let Some(chosen) = matches.iter().find(|m| {
                    let fp = m.file_path.replace('\\', "/");
                    fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{}", fqn_path))
                }) {
                    return Ok(McRes::Hit(KCand {
                        node: chosen.clone(),
                        confidence,
                        resolved_by,
                    }));
                }
            }
        }
        let ordered = prefer_call_site_file(matches, &r.file_path);
        Ok(McRes::Hit(KCand {
            node: ordered[0].clone(),
            confidence,
            resolved_by,
        }))
    }

    /// inferJavaFieldReceiverType — the declared type of a `field` node in
    /// the class enclosing the call (`signature` = "<Type> <name>").
    fn infer_java_field_receiver_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Result<Option<String>> {
        let in_file = self.nodes_in_file(&r.file_path)?;
        if in_file.is_empty() {
            return Ok(None);
        }
        let mut enclosing: Option<&KNode> = None;
        for n in in_file.iter() {
            if n.kind != "class" && n.kind != "interface" {
                continue;
            }
            if n.language != r.language {
                continue;
            }
            if n.start_line <= r.line
                && n.end_line >= r.line
                && enclosing.is_none_or(|e| n.start_line >= e.start_line)
            {
                enclosing = Some(n);
            }
        }
        let Some(enclosing) = enclosing else { return Ok(None) };
        let Some(field) = in_file.iter().find(|n| {
            n.kind == "field"
                && n.name == receiver
                && n.language == r.language
                && n.start_line >= enclosing.start_line
                && n.end_line <= enclosing.end_line
        }) else {
            return Ok(None);
        };
        let Some(sig) = field.signature.as_deref() else {
            return Ok(None);
        };
        // slice(0, lastIndexOf(name)) — a -1 index drops the last UTF-16 unit.
        let before = match sig.rfind(&field.name) {
            Some(i) => &sig[..i],
            None => Self::js_prefix(sig, Self::utf16_len(sig).saturating_sub(1)),
        };
        let type_raw = before.trim();
        if type_raw.is_empty() {
            return Ok(None);
        }
        let no_generics = self.cached_regex(r"<[^>]*>")?.replace_all(type_raw, "");
        let no_array = self
            .cached_regex(r"\[\s*\]")?
            .replace_all(&no_generics, "")
            .to_string();
        let no_varargs = self.cached_regex(r"\.\.\.$")?.replace(&no_array, "");
        let Some(last) = no_varargs
            .split(|c: char| c == '.' || c.is_whitespace())
            .rfind(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        if !last.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return Ok(None);
        }
        Ok(Some(last.to_string()))
    }

    /// matchGoFactoryReceiver — a Go receiver bound to a declared/param value
    /// or to the first result of a same-line `:=` factory call.
    fn match_go_factory_receiver(
        &mut self,
        receiver: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        let mut site = Self::ref_clone(r);
        let bindings = self.bindings(&r.file_path)?;
        let mut binding = Self::innermost_binding(&bindings, receiver, Some(r.line)).cloned();
        if binding.is_none() {
            let mut values: Vec<Rc<KNode>> = Vec::new();
            for n in self.nodes_by_name(receiver)?.iter() {
                if n.language != "go"
                    || !matches!(n.kind.as_str(), "variable" | "constant")
                    || pos_dirname(&n.file_path) != pos_dirname(&r.file_path)
                {
                    continue;
                }
                let decls = self.bindings(&n.file_path)?;
                if decls
                    .iter()
                    .any(|row| row.node_id.as_deref() == Some(n.id.as_str()) && row.kind == "decl")
                {
                    values.push(Rc::new(n.clone()));
                }
            }
            if values.len() != 1 {
                return Ok(McRes::Null);
            }
            let value = values[0].clone();
            site.file_path = value.file_path.clone();
            site.line = value.start_line;
            let site_bindings = self.bindings(&site.file_path)?;
            binding =
                Self::innermost_binding(&site_bindings, receiver, Some(site.line)).cloned();
        }
        let Some(binding) = binding else { return Ok(McRes::Null) };
        let declaration = self
            .read_file(&site.file_path)
            .and_then(|ls| ls.get((binding.line - 1) as usize).cloned())
            .unwrap_or_default();
        let escaped = regex::escape(receiver);
        let value = match &binding.node_id {
            Some(id) => self.node_by_id(id)?,
            None => None,
        };
        let ty = if binding.kind == "param" {
            self.cached_regex(&format!(
                r"\b{}\s+\*?([A-Za-z0-9_.]+)(?:\s*[,)]|\s*$)",
                escaped
            ))?
            .captures(&declaration)
            .and_then(|c| c.get(1).map(|g| g.as_str().to_string()))
        } else {
            let sig_ty = match value.as_ref().and_then(|v| v.signature.as_deref()) {
                Some(sig) => self
                    .cached_regex(r"^=\s*&?([A-Za-z0-9_.]+)\s*\{")?
                    .captures(sig)
                    .and_then(|c| c.get(1).map(|g| g.as_str().to_string())),
                None => None,
            };
            match sig_ty {
                Some(t) => Some(t),
                None => self
                    .cached_regex(&format!(r"\b{}\s+\*?([A-Za-z0-9_.]+)\s*(?:=|$)", escaped))?
                    .captures(&declaration)
                    .and_then(|c| c.get(1).map(|g| g.as_str().to_string())),
            }
        };
        if let Some(ty) = ty {
            let mut bsite = Self::ref_clone(&site);
            bsite.line = binding.line;
            return Ok(match self.match_bound_type_member(&ty, method, &bsite)? {
                McRes::Hit(c) => McRes::Hit(c),
                McRes::Null => McRes::Null,
                McRes::Punt(p) => McRes::Punt(p),
            });
        }
        if binding.kind == "param" {
            return Ok(McRes::Null);
        }
        let assign_re = self.cached_regex(
            r"\b([A-Za-z0-9_]+(?:\s*,\s*[A-Za-z0-9_]+)*)\s*:=\s*([A-Za-z0-9_.]+)\s*\(",
        )?;
        let site_bindings = self.bindings(&site.file_path)?;
        for caps in assign_re.captures_iter(&declaration) {
            let names: Vec<String> = caps[1]
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
            if names.first().map(|n| n != receiver).unwrap_or(true) {
                continue;
            }
            let name = caps[2].to_string();
            let mut factory_site = Self::ref_clone(&site);
            factory_site.line = binding.line;
            factory_site.reference_name = name.clone();
            let factory_binding = Self::innermost_binding(
                &site_bindings,
                name.split('.').next().unwrap_or(&name),
                Some(binding.line),
            )
            .cloned();
            let mut callee: Option<Rc<KNode>> = None;
            match &factory_binding {
                Some(b) if b.kind == "import" => {
                    let via = if name.contains('.') {
                        match self.resolve_via_import_member(&factory_site)? {
                            ViaImport::Hit(c) => Some(c),
                            ViaImport::Miss => None,
                            ViaImport::Punt(p) => return Ok(McRes::Punt(p)),
                        }
                    } else {
                        self.resolve_via_import(&factory_site)?
                    };
                    if let Some(c) = via {
                        callee = self.node_by_id(&c.node.id)?;
                    }
                }
                Some(b) => {
                    if let Some(id) = &b.node_id {
                        callee = self.node_by_id(id)?;
                    }
                }
                None => {
                    if !name.contains('.') {
                        let cands: Vec<Rc<KNode>> = self
                            .nodes_by_name(&name)?
                            .iter()
                            .filter(|n| {
                                n.language == "go"
                                    && n.kind == "function"
                                    && pos_dirname(&n.file_path)
                                        == pos_dirname(&site.file_path)
                            })
                            .map(|n| Rc::new(n.clone()))
                            .collect();
                        if cands.len() == 1 {
                            callee = Some(cands[0].clone());
                        }
                    }
                }
            }
            let ret_shape = self.cached_regex(r"^\*?[A-Za-z0-9_.]+$")?;
            let valid = callee.as_ref().is_some_and(|c| {
                c.kind == "function"
                    && c.return_type
                        .as_deref()
                        .is_some_and(|t| ret_shape.is_match(t))
            });
            if !valid {
                return Ok(McRes::Null);
            }
            let callee = callee.unwrap();
            let ret = callee.return_type.clone().unwrap();
            let stripped = ret.strip_prefix('*').unwrap_or(&ret);
            let mut tsite = Self::ref_clone(r);
            tsite.file_path = callee.file_path.clone();
            tsite.line = callee.start_line;
            return Ok(match self.match_bound_type_member(stripped, method, &tsite)? {
                McRes::Hit(c) => McRes::Hit(c),
                McRes::Null => McRes::Null,
                McRes::Punt(p) => McRes::Punt(p),
            });
        }
        Ok(McRes::Null)
    }

    /// matchGoFieldChainCall — Go 2-hop `base.field.Method`: base's type from
    /// the enclosing scope, field's declared type from the struct's own lines.
    fn match_go_field_chain_call(
        &mut self,
        chain: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        let segs: Vec<&str> = chain.split('.').collect();
        if segs.len() != 2 || segs[0].is_empty() || segs[1].is_empty() {
            return Ok(McRes::Null);
        }
        let (base, field) = (segs[0], segs[1]);
        let Some(base_type) = self.infer_local_receiver_type(base, r, false)? else {
            return Ok(McRes::Null);
        };
        let field_re = self.cached_regex(&format!(
            r"\b{}\s+\*?\[?\]?([A-Za-z_][A-Za-z0-9_.]*)",
            regex::escape(field)
        ))?;
        let structs: Vec<Rc<KNode>> = prefer_call_site_file(
            self.nodes_by_name(&base_type)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "struct" | "class") && n.language == "go"
                })
                .map(|n| Rc::new(n.clone()))
                .collect(),
            &r.file_path,
        );
        for s in structs {
            let Some(source) = self.read_file(&s.file_path) else {
                continue;
            };
            let start = (s.start_line - 1).max(0) as usize;
            let end = (s.end_line as usize).min(source.len());
            for raw in &source[start..end] {
                let line = strip_line_comments(raw);
                let Some(m) = field_re.captures(&line) else {
                    continue;
                };
                let raw_type = m[1].to_string();
                if raw_type.contains('.') {
                    let pkg = raw_type.split('.').next().unwrap_or("");
                    let in_module = match self.go_module_path.clone() {
                        Some(mod_path) => self
                            .import_mappings(&s.file_path)?
                            .iter()
                            .find(|i| i.local_name == pkg)
                            .is_some_and(|imp| {
                                imp.source == mod_path
                                    || imp.source.starts_with(&format!("{}/", mod_path))
                            }),
                        None => false,
                    };
                    if !in_module {
                        continue;
                    }
                }
                let Some(field_type) = raw_type.split('.').next_back() else {
                    continue;
                };
                if !field_type
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    || GO_BUILTIN_FIELD_TYPES.contains(field_type)
                {
                    continue;
                }
                match self.resolve_method_on_type(
                    field_type,
                    method,
                    r,
                    0.85,
                    "instance-method",
                    None,
                )? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {}
                }
            }
        }
        Ok(McRes::Null)
    }

    /// matchTsFieldCall restricted to the boundOwner path (br:fieldchain) —
    /// the unbound owners-by-name branch only runs for `this.`-rooted refs,
    /// which isBindingReceiverCall excludes before this arm.
    fn match_ts_field_call_bound(
        &mut self,
        owner: &KNode,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        let field_esc = regex::escape(field);
        let pats: Vec<(String, bool)> = vec![
            (
                format!(
                    r"\b{}\b\s*[?!]?\s*:\s*(?:readonly\s+)?typeof\s+([A-Za-z_$][A-Za-z0-9_.$]*)",
                    field_esc
                ),
                true,
            ),
            (
                format!(
                    r"\b{}\b\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Za-z_$][A-Za-z0-9_.$]*)",
                    field_esc
                ),
                false,
            ),
            (
                format!(
                    r"\b{}\b\s*=\s*new\s+([A-Za-z_$][A-Za-z0-9_.$]*)",
                    field_esc
                ),
                false,
            ),
        ];
        let Some(source) = self.read_file(&owner.file_path) else {
            return Ok(McRes::Null);
        };
        let tail_re = self.cached_regex(r"^[\s]*(?:<[^>]*>)?\s*[\[|&]")?;
        let start = (owner.start_line - 1).max(0) as usize;
        let end = (owner.end_line as usize).min(source.len());
        for raw in &source[start..end] {
            let line = strip_line_comments(raw);
            for (pat, value_type) in &pats {
                let re = self.cached_regex(pat)?;
                let Some(caps) = re.captures(&line) else {
                    continue;
                };
                let Some(m1) = caps.get(1) else { continue };
                if m1.as_str().is_empty() {
                    continue;
                }
                if tail_re.is_match(&line[caps.get(0).unwrap().end()..]) {
                    return Ok(McRes::Null);
                }
                if *value_type {
                    let cls_bindings = self.bindings(&owner.file_path)?;
                    let row = Self::innermost_binding(
                        &cls_bindings,
                        m1.as_str(),
                        Some(owner.start_line),
                    )
                    .cloned();
                    let holder_id = match &row {
                        Some(b) if b.kind == "import" => {
                            let mut ref2 = Self::ref_clone(r);
                            ref2.file_path = owner.file_path.clone();
                            ref2.line = owner.start_line;
                            ref2.reference_name = m1.as_str().to_string();
                            ref2.reference_kind = "references".to_string();
                            match self.resolve_via_import_member(&ref2)? {
                                ViaImport::Hit(c) => Some(c.node.id.clone()),
                                ViaImport::Miss => None,
                                ViaImport::Punt(p) => return Ok(McRes::Punt(p)),
                            }
                        }
                        Some(b) => b.node_id.clone(),
                        None => None,
                    };
                    let holder = match &holder_id {
                        Some(id) => self.node_by_id(id)?,
                        None => None,
                    };
                    return Ok(match holder {
                        Some(h) => match self.resolve_object_literal_member(
                            &h,
                            method,
                            r,
                            0.85,
                            "instance-method",
                        )? {
                            Some(c) => McRes::Hit(c),
                            None => McRes::Null,
                        },
                        None => McRes::Null,
                    });
                }
                let type_name = m1.as_str().split('.').next_back().unwrap_or("");
                if !type_name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                {
                    return Ok(McRes::Null);
                }
                let mut bsite = Self::ref_clone(r);
                bsite.file_path = owner.file_path.clone();
                bsite.line = owner.start_line;
                return Ok(match self.match_bound_type_member(
                    m1.as_str(),
                    method,
                    &bsite,
                )? {
                    McRes::Hit(c) => McRes::Hit(c),
                    McRes::Null => McRes::Null,
                    McRes::Punt(p) => McRes::Punt(p),
                });
            }
        }
        Ok(McRes::Null)
    }

    /// matchRustSelfCall (name-matcher.ts): `self.method()` — the method on
    /// the type the call sits inside. The owner is the calling method's
    /// qualified-name prefix; a free fn has no `self`. Exactly one candidate
    /// must belong to that owner — two same-named methods on the same type
    /// is the fabrication this declines instead of.
    fn match_rust_self_call(&mut self, method: &str, r: &ResolveRefIn) -> Result<McRes> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null);
        }
        let owner = &caller.qualified_name[..sep];
        let want = format!("{}::{}", owner, method);
        let mut owned: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(&want)?
            .iter()
            .filter(|n| {
                n.kind == "method" && n.language == "rust" && n.qualified_name == want
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        // Rust qualified names omit module paths — two modules can declare
        // the same `Target`. Require one owner declaration in the caller's
        // file and the method there too; a unique owner still permits impl
        // blocks split across files.
        let owners: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(owner)?
            .iter()
            .filter(|n| {
                n.language == "rust"
                    && matches!(
                        n.kind.as_str(),
                        "struct" | "enum" | "union" | "trait" | "class"
                    )
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        if owners.len() > 1 {
            if owners.iter().filter(|n| n.file_path == caller.file_path).count() != 1 {
                return Ok(McRes::Null);
            }
            owned.retain(|n| n.file_path == caller.file_path);
        }
        if owned.len() != 1 {
            return Ok(McRes::Null);
        }
        Ok(McRes::Hit(KCand {
            node: owned[0].clone(),
            confidence: 0.9,
            resolved_by: "qualified-name",
        }))
    }

    /// matchRustSelfPath (name-matcher.ts): `Self::item` associated-item
    /// path — `Self` binds to the caller qualified-name owner exactly as
    /// match_rust_self_call derives it, then the leaf resolves by
    /// `owner::leaf` qualified name over the prefixed member kinds (method,
    /// enum_member, constant). Advisory: Null falls through to the normal
    /// strategies so a ref today's bare-name arm resolves keeps its verdict.
    /// Two segments after the non-nested turbofish strip, plus the
    /// three-segment associated-type path `Self::Assoc::m` (`type Assoc = X`
    /// in the caller's enclosing impl block binds the middle segment, then
    /// `X::m` resolves like any owner path). A `Self::f().tail` leaf
    /// resolves through the receiver method's declared return type.
    fn match_rust_self_path(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        if r.language != "rust" || !r.reference_name.starts_with("Self::") {
            return Ok(McRes::Null);
        }

        let strip = self.cached_regex(r"<[^>]*>")?;
        let name = strip.replace_all(&r.reference_name, "");
        let segs: Vec<&str> = name.split("::").filter(|s| !s.is_empty()).collect();
        if segs[0] != "Self" || (segs.len() != 2 && segs.len() != 3) {
            return Ok(McRes::Null);
        }
        let mut leaf = segs[segs.len() - 1].to_string();
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null);
        }
        // `Self::Assoc::leaf` — the associated type binds in the caller's
        // enclosing `impl` block (`type Assoc = X`), not on the type.
        let mut owner: String = if segs.len() == 2 {
            caller.qualified_name[..sep].to_string()
        } else {
            match self.rust_assoc_type_binding(&caller, segs[1])? {
                Some(bound) => bound,
                None => return Ok(McRes::Null),
            }
        };

        // `Self::f().tail` — a call-chained member: the leaf carries
        // `f().tail`, so the receiver method resolves first and its
        // declared return type binds the tail (`-> Self` means the
        // RECEIVER's owner, not the caller's). Only an empty-arg call
        // chain is read; anything else declines.
        let chain_re = self.cached_regex(r"^(\w+)\(\)\.(\w+)$")?;
        if let Some(chained) = chain_re.captures(&leaf) {
            const METHOD: &[&str] = &["method"];
            let Some(recv) = self.resolve_rust_self_member(&owner, &chained[1], &caller, METHOD)?
            else {
                return Ok(McRes::Null);
            };
            let Some(sig) = recv.signature.as_deref() else {
                return Ok(McRes::Null);
            };
            let Some(arrow) = sig.rfind("->") else {
                return Ok(McRes::Null);
            };
            let raw_ret = sig[arrow + 2..].trim();
            owner = if raw_ret == "Self" {
                match recv.qualified_name.rfind("::") {
                    Some(rs) => recv.qualified_name[..rs].to_string(),
                    None => return Ok(McRes::Null),
                }
            } else {
                match self.normalize_inferred_type_name(raw_ret)? {
                    Some(t) => t,
                    None => return Ok(McRes::Null),
                }
            };
            leaf = chained[2].to_string();
        } else if !leaf.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Ok(McRes::Null);
        }

        const MEMBER_KINDS: &[&str] = &["method", "enum_member", "constant"];
        let Some(node) = self.resolve_rust_self_member(&owner, &leaf, &caller, MEMBER_KINDS)?
        else {
            return Ok(McRes::Null);
        };
        Ok(McRes::Hit(KCand {
            node,
            confidence: 0.9,
            resolved_by: "qualified-name",
        }))
    }

    /// resolveRustSelfMember (name-matcher.ts): the `owner::leaf`
    /// qualified-name lookup shared by `Self::item` and the `Self::f().tail`
    /// chain. Rust qualified names omit module paths, so two same-named
    /// owners need the caller's file to pin one (match_rust_self_call's
    /// disambiguation).
    fn resolve_rust_self_member(
        &mut self,
        owner: &str,
        leaf: &str,
        caller: &Rc<KNode>,
        kinds: &[&str],
    ) -> Result<Option<Rc<KNode>>> {
        let want = format!("{}::{}", owner, leaf);
        let mut owned: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(&want)?
            .iter()
            .filter(|n| {
                kinds.contains(&n.kind.as_str())
                    && n.language == "rust"
                    && n.qualified_name == want
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        let owners: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(owner)?
            .iter()
            .filter(|n| {
                n.language == "rust"
                    && matches!(
                        n.kind.as_str(),
                        "struct" | "enum" | "union" | "trait" | "class"
                    )
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        if owners.len() > 1 {
            if owners.iter().filter(|n| n.file_path == caller.file_path).count() != 1 {
                return Ok(None);
            }
            owned.retain(|n| n.file_path == caller.file_path);
        }
        if owned.len() != 1 {
            return Ok(None);
        }
        Ok(Some(owned[0].clone()))
    }

    /// matchRustBareSelf (name-matcher.ts): a bare `Self` ref names the
    /// enclosing type — `Self { .. }` constructions, `-> Self` positions
    /// and `Self(..)` calls all mean the caller qualified-name's owner.
    /// Only a concrete owner binds (struct/enum/union/class): inside a
    /// `trait` body `Self` is the abstract implementor and declines. A
    /// type-level caller (`struct S { next: Option<Self> }`) binds to
    /// itself. Same file-pin disambiguation as the member arms.
    fn match_rust_bare_self(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        const TYPE_KINDS: &[&str] = &["struct", "enum", "union", "class"];
        let sep = caller.qualified_name.rfind("::");
        let owner: &str = match sep {
            Some(0) | None if TYPE_KINDS.contains(&caller.kind.as_str()) => {
                &caller.qualified_name
            }
            Some(0) | None => return Ok(None),
            Some(s) => &caller.qualified_name[..s],
        };
        let mut owners: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(owner)?
            .iter()
            .filter(|n| {
                n.language == "rust"
                    && TYPE_KINDS.contains(&n.kind.as_str())
                    && n.qualified_name == owner
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        if owners.len() > 1 {
            owners.retain(|n| n.file_path == caller.file_path);
        }
        if owners.len() != 1 {
            return Ok(None);
        }
        Ok(Some(KCand {
            node: owners[0].clone(),
            confidence: 0.9,
            resolved_by: "qualified-name",
        }))
    }

    /// rustAssocTypeBinding (name-matcher.ts): the concrete type an
    /// associated-type name binds to inside the caller's enclosing `impl`
    /// block — `Self::Assoc` in `impl Tr for T` means that impl's
    /// `type Assoc = X` decl (trait defaults `type Assoc;` carry no `=` and
    /// miss). The impl block is found by brace-counting backward from the
    /// caller's first line to the enclosing block opener — only an `impl`
    /// opener qualifies — then the decl is matched per line at depth 1 (or
    /// on the opener line) and normalized like any inferred type name.
    fn rust_assoc_type_binding(
        &mut self,
        caller: &Rc<KNode>,
        assoc_name: &str,
    ) -> Result<Option<String>> {
        let Some(lines) = self.read_file(&caller.file_path) else {
            return Ok(None);
        };
        // Backward brace scan: the first `{` whose net depth goes negative
        // opens the block enclosing the caller — for a method, the impl.
        let at = |i: i64| -> String {
            if i < 0 {
                String::new()
            } else {
                strip_line_comments(lines.get(i as usize).map(|s| s.as_str()).unwrap_or(""))
            }
        };
        let mut depth = 0i32;
        let mut block_idx = -1i64;
        for i in (0..caller.start_line - 1).rev() {
            let code = at(i);
            for ch in code.chars() {
                if ch == '{' {
                    depth -= 1;
                } else if ch == '}' {
                    depth += 1;
                }
            }
            if depth < 0 {
                block_idx = i;
                break;
            }
        }
        let impl_re = self.cached_regex(r"\bimpl\b")?;
        // Single-line impls put the opener on the caller's own line
        // (`impl T { type A = X; fn m(&self) { ... } }`).
        if block_idx < 0 {
            let own = at(caller.start_line - 1);
            if let Some(pos) = own.find('{') {
                if impl_re.is_match(&own[..pos]) {
                    block_idx = caller.start_line - 1;
                }
            }
        }
        if block_idx < 0 {
            return Ok(None);
        }
        // The opener must head an `impl` — check the line's pre-`{` head,
        // then brace-free continuation lines above it (`impl Tr for T\n{`);
        // a line bearing `{`/`}` belongs to a different construct.
        let mut is_impl = false;
        for j in ((block_idx - 4).max(0)..=block_idx).rev() {
            let code = at(j);
            if j == block_idx {
                let head = &code[..code.find('{').unwrap_or(0)];
                if impl_re.is_match(head) {
                    is_impl = true;
                }
                continue;
            }
            if code.contains('{') || code.contains('}') {
                break;
            }
            if impl_re.is_match(&code) {
                is_impl = true;
                break;
            }
        }
        if !is_impl {
            return Ok(None);
        }
        // Forward: `type <assoc> = X;` is a direct member — match at depth 1
        // or on the opener line itself, stop when the block closes.
        let type_re = self.cached_regex(&format!(
            r"\btype\s+{}\s*=\s*([^;]+);",
            regex::escape(assoc_name)
        ))?;
        depth = 0;
        let mut opened = false;
        let mut bound: Option<String> = None;
        for i in block_idx..lines.len() as i64 {
            let code = at(i);
            if (opened && depth == 1) || (i == block_idx && code.contains('{')) {
                if let Some(m) = type_re.captures(&code) {
                    bound = Some(m.get(1).unwrap().as_str().to_string());
                    break;
                }
            }
            for ch in code.chars() {
                if ch == '{' {
                    depth += 1;
                    opened = true;
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            if opened && depth <= 0 {
                break;
            }
        }
        let Some(bound) = bound else { return Ok(None) };
        self.normalize_inferred_type_name(&bound)
    }

    /// matchRustSelfFieldCall (name-matcher.ts): `self.<field>.<method>()`,
    /// exclusive for `self.<field>` receivers — the field's declared type
    /// off the owner struct's OWN declaration lines (comment-stripped,
    /// line by line), validated by rmot, or nothing. Rust struct fields are
    /// not graph nodes; the declaration text is the only place the type
    /// lives.
    fn match_rust_self_field_call(
        &mut self,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        if field.is_empty() || field.contains('.') {
            return Ok(McRes::Null);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null);
        }
        let Some(owner) = caller.qualified_name[..sep].split("::").last() else {
            return Ok(McRes::Null);
        };
        let owners = prefer_call_site_file(
            self.nodes_by_name(owner)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "struct" | "union" | "class")
                        && n.language == "rust"
                })
                .map(|n| Rc::new(n.clone()))
                .collect(),
            &r.file_path,
        );
        let field_re = self.cached_regex(&format!(
            r"\b{}\s*:\s*([^,{{}}]+)",
            regex::escape(field)
        ))?;
        for s in owners {
            let Some(source) = self.read_file(&s.file_path) else {
                continue;
            };
            let start = (s.start_line - 1).max(0) as usize;
            let end = (s.end_line as usize).min(source.len());
            for raw in &source[start..end] {
                let line = RUST_LINE_COMMENTS.replace_all(raw, "");
                let Some(m) = field_re.captures(&line) else {
                    continue;
                };
                // The field is declared here; whether or not its type names a
                // project symbol, this owner is the answer — terminal.
                let Some(field_type) = rust_field_type_name(&m[1]) else {
                    return Ok(McRes::Null);
                };
                return self.resolve_method_on_type(
                    &field_type,
                    method,
                    r,
                    0.85,
                    "instance-method",
                    None,
                );
            }
        }
        Ok(McRes::Null)
    }

    /// matchTsThisFieldCall — the `this.field.method` entry point of
    /// matchMethodCall's requireReceiverEvidence=false arm. The owner is the
    /// enclosing class written on the calling method's qualified name, so it
    /// is not a guess — same exclusive discipline as the go/rust field arms.
    fn match_ts_this_field_call(
        &mut self,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        if field.is_empty() || field.contains('.') {
            return Ok(McRes::Null);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null); // not inside a class
        }
        let owner = caller.qualified_name[..sep]
            .split("::")
            .last()
            .unwrap_or("");
        if owner.is_empty() {
            return Ok(McRes::Null);
        }
        self.match_ts_field_call_free(owner, field, method, r)
    }

    /// matchTsFieldCall without boundOwner — the unbound variant used by the
    /// member-tail arm. Owners are named classes/components visible to the
    /// call site (call-site file first); the declared-type tail resolves
    /// through rmot, never btm. A `typeof` field still routes through the
    /// object-literal member scan, but by plain name lookup + same-family
    /// filter rather than the binding row the bound variant uses.
    fn match_ts_field_call_free(
        &mut self,
        owner: &str,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        let owners = prefer_call_site_file(
            self.nodes_by_name(owner)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "class" | "component")
                        && same_language_family(&n.language, &r.language)
                })
                .map(|n| Rc::new(n.clone()))
                .collect(),
            &r.file_path,
        );
        let field_esc = regex::escape(field);
        let pats: Vec<(String, bool)> = vec![
            (
                format!(
                    r"\b{}\b\s*[?!]?\s*:\s*(?:readonly\s+)?typeof\s+([A-Za-z_$][A-Za-z0-9_.$]*)",
                    field_esc
                ),
                true,
            ),
            (
                format!(
                    r"\b{}\b\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Za-z_$][A-Za-z0-9_.$]*)",
                    field_esc
                ),
                false,
            ),
            (
                format!(
                    r"\b{}\b\s*=\s*new\s+([A-Za-z_$][A-Za-z0-9_.$]*)",
                    field_esc
                ),
                false,
            ),
        ];
        for cls in &owners {
            let Some(source) = self.read_file(&cls.file_path) else {
                continue;
            };
            let start = (cls.start_line - 1).max(0) as usize;
            let end = (cls.end_line as usize).min(source.len());
            for raw in &source[start..end] {
                let line = strip_line_comments(raw);
                for (pat, value_type) in &pats {
                    let re = self.cached_regex(pat)?;
                    let Some(caps) = re.captures(&line) else {
                        continue;
                    };
                    let Some(m1) = caps.get(1) else { continue };
                    if m1.as_str().is_empty() {
                        continue;
                    }
                    // No tail_re check — that guard is `boundOwner &&` in TS.
                    if *value_type {
                        // `field: typeof Ns` — the namespace value's members
                        // are bare-named functions inside a const/variable.
                        let holder_name =
                            m1.as_str().split('.').next_back().unwrap_or("");
                        let holders = prefer_call_site_file(
                            self.nodes_by_name(holder_name)?
                                .iter()
                                .filter(|n| {
                                    matches!(n.kind.as_str(), "constant" | "variable")
                                        && same_language_family(&n.language, &r.language)
                                })
                                .map(|n| Rc::new(n.clone()))
                                .collect(),
                            &r.file_path,
                        );
                        for holder in &holders {
                            if let Some(hit) = self.resolve_object_literal_member(
                                holder,
                                method,
                                r,
                                0.85,
                                "instance-method",
                            )? {
                                return Ok(McRes::Hit(hit));
                            }
                        }
                        return Ok(McRes::Null);
                    }
                    // `ns.Mailer` → `Mailer`; a primitive or builtin names no
                    // project type.
                    let type_name = m1.as_str().split('.').next_back().unwrap_or("");
                    if !type_name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_uppercase())
                    {
                        return Ok(McRes::Null);
                    }
                    // Two apps in one repo may each declare the type. Among
                    // its declarations of the method prefer the one closest
                    // to the call site's directory — never index order.
                    let declared: Vec<Rc<KNode>> = self
                        .nodes_by_name(method)?
                        .iter()
                        .filter(|n| {
                            n.kind == "method"
                                && same_language_family(&n.language, &r.language)
                                && (n.qualified_name == format!("{type_name}::{method}")
                                    || n.qualified_name
                                        .ends_with(&format!("::{type_name}::{method}")))
                        })
                        .map(|n| Rc::new(n.clone()))
                        .collect();
                    if declared.len() > 1 {
                        let call_dirs: Vec<&str> = {
                            let mut v: Vec<&str> = r.file_path.split('/').collect();
                            v.pop();
                            v
                        };
                        let shared = |fp: &str| -> usize {
                            let mut dirs: Vec<&str> = fp.split('/').collect();
                            dirs.pop();
                            let mut i = 0;
                            while i < dirs.len()
                                && i < call_dirs.len()
                                && dirs[i] == call_dirs[i]
                            {
                                i += 1;
                            }
                            i
                        };
                        let max_shared =
                            declared.iter().map(|n| shared(&n.file_path)).max().unwrap_or(0);
                        let nearest: Vec<&Rc<KNode>> = declared
                            .iter()
                            .filter(|n| shared(&n.file_path) == max_shared)
                            .collect();
                        if nearest.len() > 1 {
                            // TS tiebreaks by localeCompare, which this port
                            // cannot model exactly — let the TS spine pick.
                            return Ok(McRes::Punt("mc-tfield-ambig"));
                        }
                        return Ok(McRes::Hit(KCand {
                            node: nearest[0].clone(),
                            confidence: 0.85,
                            resolved_by: "instance-method",
                        }));
                    }
                    return self.resolve_method_on_type(
                        type_name,
                        method,
                        r,
                        0.85,
                        "instance-method",
                        None,
                    );
                }
            }
        }
        Ok(McRes::Null)
    }

    /// The br:factory tail of matchBoundReceiverCall's ESM arm — receiver's
    /// initializer ends in a call/new-factory expression whose return type
    /// carries the method.
    fn esm_factory_tail(
        &mut self,
        binding: &KBinding,
        root: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        let value = match &binding.node_id {
            Some(id) => self.node_by_id(id)?,
            None => None,
        };
        let escaped = regex::escape(root);
        let lines = self.read_file(&r.file_path);
        let declares_re = self.cached_regex(&format!(
            r"\b(?:const|let|var)\s+{}\s*=",
            escaped
        ))?;
        let declares_value = lines
            .as_ref()
            .and_then(|ls| ls.get((binding.line - 1) as usize))
            .is_some_and(|l| declares_re.is_match(l));
        let declaration = if declares_value {
            lines.as_ref().map(|ls| {
                let lo = (binding.line - 1).max(0) as usize;
                let hi = ((binding.line + 2) as usize).min(ls.len());
                ls[lo..hi].join("\n")
            })
        } else {
            None
        };
        let signature = match value.as_ref().and_then(|v| v.signature.clone()) {
            Some(s) => Some(s),
            None => {
                let sig_re = self.cached_regex(&format!(
                    r"\b(?:const|let|var)\s+{}\s*(=[\s\S]+)",
                    escaped
                ))?;
                declaration
                    .as_deref()
                    .and_then(|d| sig_re.captures(d).map(|c| c[1].to_string()))
            }
        };
        let init = signature.unwrap_or_default();
        // parensEnd — UTF-16 unit index one past the ')' that closes the '('
        // at `from - 1`, or -1 when it never closes (JS string indexing).
        let parens_end = |s: &str, from: usize| -> i64 {
            let mut depth = 1i64;
            let mut i = from;
            let mut units = 0usize;
            for ch in s.chars() {
                let start = units;
                units += ch.len_utf16();
                if start < from || depth == 0 {
                    continue;
                }
                if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    depth -= 1;
                }
                i = units;
            }
            if depth != 0 {
                -1
            } else {
                i as i64
            }
        };
        // ^[ \t]*(?:;|\r?\n(?![ \t]*[.(\[?])) — the lookahead is emulated:
        // `;` always ends the initializer; a newline does unless a chained
        // `.`/`(`/`[`/`?` follows its leading whitespace.
        let ends_initializer = |init_s: &str, call_end: i64| -> bool {
            if call_end < 0 {
                return false;
            }
            let tail = Self::js_slice(init_s, call_end as usize);
            if tail.is_empty() {
                return true;
            }
            let t = tail.trim_start_matches([' ', '\t']);
            if t.starts_with(';') {
                return true;
            }
            if let Some(rest) = t.strip_prefix("\r\n").or_else(|| t.strip_prefix('\n')) {
                return !rest
                    .trim_start_matches([' ', '\t'])
                    .chars()
                    .next()
                    .is_some_and(|c| matches!(c, '.' | '(' | '[' | '?'));
            }
            false
        };
        let awaited_re = self.cached_regex(r"^=\s*await\b")?;
        let awaited = awaited_re.is_match(&init);
        let mut callee_name: Option<String> = None;
        let mut owner_name: Option<String> = None;
        let factory_re =
            self.cached_regex(r"^=\s*(await\s+)?([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(")?;
        if let Some(fm) = factory_re.captures(&init) {
            let end = parens_end(&init, Self::utf16_len(&init[..fm.get(0).unwrap().end()]));
            if ends_initializer(&init, end) {
                callee_name = fm.get(2).map(|g| g.as_str().to_string());
            }
        }
        if callee_name.is_none() {
            let ctor_re = self.cached_regex(
                r"^=\s*(?:await\s+)?new\s+([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(",
            )?;
            if let Some(cm) = ctor_re.captures(&init) {
                let ctor_end =
                    parens_end(&init, Self::utf16_len(&init[..cm.get(0).unwrap().end()]));
                if ctor_end >= 0 {
                    let member_re = self.cached_regex(
                        r"^\s*\.\s*([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(",
                    )?;
                    let tail = Self::js_slice(&init, ctor_end as usize);
                    if let Some(mm) = member_re.captures(tail) {
                        let m_end = parens_end(
                            &init,
                            ctor_end as usize
                                + Self::utf16_len(&tail[..mm.get(0).unwrap().end()]),
                        );
                        if ends_initializer(&init, m_end) {
                            owner_name = cm.get(1).map(|g| g.as_str().to_string());
                            callee_name = mm.get(1).map(|g| g.as_str().to_string());
                        }
                    }
                }
            }
        }
        let Some(callee_name) = callee_name else {
            return Ok(McRes::Null);
        };
        let bindings = self.bindings(&r.file_path)?;
        let callee: Option<Rc<KNode>> = if let Some(owner_name) = owner_name {
            let owner_binding =
                Self::innermost_binding(&bindings, &owner_name, Some(binding.line)).cloned();
            let owner_id = match &owner_binding {
                Some(b) if b.kind == "import" => {
                    let mut ref2 = Self::ref_clone(r);
                    ref2.line = binding.line;
                    ref2.reference_name = owner_name.clone();
                    ref2.reference_kind = "references".to_string();
                    match self.resolve_via_import(&ref2)? {
                        Some(c) => Some(c.node.id.clone()),
                        None => None,
                    }
                }
                Some(b) => b.node_id.clone(),
                None => None,
            };
            let owner = match &owner_id {
                Some(id) => self.node_by_id(id)?,
                None => None,
            };
            let Some(owner) = owner else { return Ok(McRes::Null) };
            if !matches!(owner.kind.as_str(), "class" | "interface" | "component") {
                return Ok(McRes::Null);
            }
            self.nodes_by_qualified_name(&format!(
                "{}::{}",
                owner.qualified_name, callee_name
            ))?
            .iter()
            .find(|n| n.kind == "method" && n.file_path == owner.file_path)
            .map(|n| Rc::new(n.clone()))
        } else {
            let factory_binding =
                Self::innermost_binding(&bindings, &callee_name, Some(binding.line))
                    .cloned();
            let callee_id = match &factory_binding {
                Some(b) if b.kind == "import" => {
                    let mut ref2 = Self::ref_clone(r);
                    ref2.line = binding.line;
                    ref2.reference_name = callee_name.clone();
                    match self.resolve_via_import(&ref2)? {
                        Some(c) => Some(c.node.id.clone()),
                        None => None,
                    }
                }
                Some(b) => b.node_id.clone(),
                None => None,
            };
            match &callee_id {
                Some(id) => self.node_by_id(id)?,
                None => None,
            }
        };
        let Some(callee) = callee else { return Ok(McRes::Null) };
        let ret_re = self
            .cached_regex(r"\)\s*:\s*([A-Za-z0-9_$]+(?:<[A-Za-z0-9_$]+>)?)\s*$")?;
        let return_type = callee.return_type.clone().or_else(|| {
            callee
                .signature
                .as_deref()
                .and_then(|s| ret_re.captures(s).map(|c| c[1].to_string()))
        });
        // `!returnType` — an empty annotation/returnType fails the same way.
        let Some(return_type) = return_type.filter(|t| !t.is_empty()) else {
            return Ok(McRes::Null);
        };
        let promise_re = self.cached_regex(r"^Promise<(.+)>$")?;
        let ty = if awaited {
            promise_re
                .replace(&return_type, "$1")
                .to_string()
        } else {
            return_type
        };
        let mut site = Self::ref_clone(r);
        site.file_path = callee.file_path.clone();
        site.line = callee.start_line;
        Ok(match self.match_bound_type_member(&ty, method, &site)? {
            McRes::Hit(c) => McRes::Hit(c),
            McRes::Null => McRes::Null,
            McRes::Punt(p) => McRes::Punt(p),
        })
    }

    /// Cheap raw-source gate for inferEsmAwaitedCallType — the awaited arm can
    /// only engage when the file binds `receiver` in an `= await x(` shape.
    /// True → punt (the arm needs sanitized scope parsing); false → provable
    /// null, continue natively.
    fn mc_await_gate(&mut self, receiver: &str, r: &ResolveRefIn) -> Result<bool> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        let re = self.cached_regex(&format!(
            r"\b(?:const|let|var)\s+{}\s*=\s*await\s+[A-Za-z0-9_$]+\s*\(",
            regex::escape(receiver)
        ))?;
        Ok(lines.iter().any(|l| re.is_match(l)))
    }

    /// Cheap gate for inferIterationReceiver — kotlin/go only, fires only
    /// when its declaration preconditions can hold; tree-sitter stays in TS.
    fn mc_iteration_gate(&mut self, receiver: &str, r: &ResolveRefIn) -> Result<bool> {
        if r.language != "kotlin" && r.language != "go" {
            return Ok(false);
        }
        let bindings = self.bindings(&r.file_path)?;
        let mut best: Option<&KBinding> = None;
        for b in bindings.iter() {
            if b.name != receiver || b.scope_start > r.line || b.scope_end < r.line {
                continue;
            }
            if best.is_none_or(|x| b.scope_end - b.scope_start < x.scope_end - x.scope_start) {
                best = Some(b);
            }
        }
        let declaration = match best {
            Some(b) => self
                .read_file(&r.file_path)
                .and_then(|ls| ls.get((b.line - 1) as usize).cloned()),
            None => None,
        };
        if r.language == "go" {
            // `!declaration?.includes('range')` → null → provable miss.
            return Ok(declaration.is_some_and(|d| d.contains("range")));
        }
        // kotlin: `receiver !== 'it' && !declaration?.includes('->')` → null.
        Ok(receiver == "it" || declaration.is_some_and(|d| d.contains("->")))
    }

    /// matchMethodCall(ref, context, requireReceiverEvidence=true) — the
    /// boundReceiver evidence slice. Punt points: php instanceof guards,
    /// go/kotlin iteration constructs, ESM awaited inference, and every
    /// member-miss that would walk live supertype edges.
    fn match_method_call(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        // PHP `$this->prop->method()` — exclusive declared-type path.
        if r.language == "php" {
            let re = self.cached_regex(r"^(this->[A-Za-z0-9_]+)\.([A-Za-z0-9_]+)$")?;
            if let Some(m) = re.captures(&r.reference_name) {
                let receiver = m[1].to_string();
                let php_method = m[2].to_string();
                let Some(inferred) =
                    self.infer_local_receiver_type(&receiver, r, false)?
                else {
                    return Ok(McRes::Null);
                };
                let fqn = self.imported_fqn_of(&inferred, r)?;
                return self.resolve_method_on_type(
                    &inferred,
                    &php_method,
                    r,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                );
            }
        }

        // Rust `Self::item` — associated-item path binding `Self` to the
        // caller's impl owner. Before the `!matched` bail so deeper paths
        // (`Self::Assoc::m`) reach it; a miss falls through like TS.
        if let McRes::Hit(c) = self.match_rust_self_path(r)? {
            return Ok(McRes::Hit(c));
        }

        let dot_re = self
            .cached_regex(r"^([A-Za-z0-9_.]+)\.([A-Za-z0-9_]+:?(?:[A-Za-z0-9_]+:)*)$")?;
        let mut dot_match = dot_re.captures(&r.reference_name);
        if dot_match.is_none() && r.language == "cpp" {
            let op_re = self.cached_regex(
                r"^([A-Za-z0-9_.]+)\.(operator[^A-Za-z0-9_\s.]+)$",
            )?;
            dot_match = op_re.captures(&r.reference_name);
        }
        let colon_re = self.cached_regex(r"^([A-Za-z0-9_]+)::([A-Za-z0-9_]+)$")?;
        let colon_match = colon_re.captures(&r.reference_name);
        // lua `:`/r `$` shapes are unmigrated-language only — dead here.
        let matched = dot_match.as_ref().or(colon_match.as_ref());
        let Some(m) = matched else {
            return Ok(McRes::Null);
        };
        let object_or_class = m[1].to_string();
        let method_name = m[2].to_string();
        let inferable = dot_match.is_some();

        let bindings = self.bindings(&r.file_path)?;
        let binding =
            Self::innermost_binding(&bindings, &object_or_class, Some(r.line)).cloned();

        if inferable {
            // inferGuardedReceiver is php-only and needs a tree parse — punt
            // when its cheap precondition can hold, else provable null.
            if r.language == "php" {
                let guarded = self
                    .read_file(&r.file_path)
                    .is_some_and(|ls| ls.iter().any(|l| l.contains("instanceof")));
                if guarded {
                    return Ok(McRes::Punt("mc-guarded"));
                }
            }
            let mut site = Self::ref_clone(r);
            if let Some(b) = &binding {
                if b.kind != "import" {
                    site.line = b.line;
                    if let Some(nid) = &b.node_id {
                        site.from_node_id = nid.clone();
                    }
                }
            }
            // TS passes requireReceiverEvidence (=true) as preserveQualifiedName
            // to both inferrers here — qualified names stay intact.
            let mut inferred = if r.language == "cpp" {
                self.infer_cpp_receiver_type(&object_or_class, r, 0, true)?
            } else {
                self.infer_local_receiver_type(&object_or_class, &site, true)?
            };
            if inferred.is_none() && r.language == "go" {
                match self.match_go_factory_receiver(&object_or_class, &method_name, r)? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {}
                }
            }
            if inferred.is_none() {
                if self.mc_iteration_gate(&object_or_class, r)? {
                    return Ok(McRes::Punt("mc-iteration"));
                }
                if is_esm_family(&r.language)
                    && self.mc_await_gate(&object_or_class, r)?
                {
                    return Ok(McRes::Punt("mc-await"));
                }
            }
            if let Some(t) = inferred.take() {
                let mut bsite = Self::ref_clone(r);
                if let Some(b) = &binding {
                    bsite.line = b.line;
                }
                return Ok(match self.match_bound_type_member(&t, &method_name, &bsite)? {
                    McRes::Hit(c) => McRes::Hit(c),
                    McRes::Null => McRes::Null,
                    McRes::Punt(p) => McRes::Punt(p),
                });
            }
        }

        // Go 2-hop field chain — exclusive branch.
        if r.language == "go" && dot_match.is_some() && object_or_class.contains('.') {
            return self.match_go_field_chain_call(&object_or_class, &method_name, r);
        }
        // rust field/self arms: rust is unmigrated — dead.
        // this.field arms: `this.` receivers are excluded by the claim gate —
        // dead inside boundReceiver.

        if (r.language == "java" || r.language == "kotlin") && dot_match.is_some() {
            if let Some(inferred) =
                self.infer_java_field_receiver_type(&object_or_class, r)?
            {
                let fqn = self.imported_fqn_of(&inferred, r)?;
                match self.resolve_method_on_type(
                    &inferred,
                    &method_name,
                    r,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                )? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {}
                }
            }
        }

        // mc-literal — OBJECT_LITERAL_LANGUAGES is the ESM set.
        if dot_match.is_some()
            && !object_or_class.contains('.')
            && is_object_literal_language(&r.language)
        {
            let holders: Vec<Rc<KNode>> = prefer_call_site_file(
                self.nodes_by_name(&object_or_class)?
                    .iter()
                    .filter(|n| {
                        matches!(n.kind.as_str(), "constant" | "variable")
                            && n.file_path == r.file_path
                    })
                    .map(|n| Rc::new(n.clone()))
                    .collect(),
                &r.file_path,
            );
            for holder in holders {
                // `binding?.nodeId !== holder.id` — an absent binding or
                // node_id skips every holder, exactly like TS.
                let bound_id = binding.as_ref().and_then(|b| b.node_id.as_deref());
                if bound_id != Some(holder.id.as_str()) {
                    continue;
                }
                if let Some(hit) = self.resolve_object_literal_member(
                    &holder,
                    &method_name,
                    r,
                    0.85,
                    "instance-method",
                )? {
                    return Ok(McRes::Hit(hit));
                }
            }
        }

        // Strategy 1 — under requireReceiverEvidence the first candidate
        // passing the binding filter returns matchBoundTypeMember(receiver).
        let class_candidates = prefer_call_site_file(
            self.nodes_by_name(&object_or_class)?
                .iter()
                .map(|n| Rc::new(n.clone()))
                .collect(),
            &r.file_path,
        );
        for c in &class_candidates {
            if let Some(b) = &binding {
                if b.node_id.as_deref() != Some(c.id.as_str()) {
                    continue;
                }
            }
            return self.match_bound_type_member(&object_or_class, &method_name, r);
        }
        Ok(McRes::Null)
    }

    /// matchMethodCall(ref, context, requireReceiverEvidence=false) — the
    /// member-tail arm of matchReference, reached by refs the boundReceiver
    /// claim never took (`this.`/`self.` roots, non-call kinds, deep
    /// receivers). Same pattern prelude and unconditional sub-arms as the
    /// bound path; the evidence-gated arms (mc-guarded, gofactory, iteration,
    /// the btm terminal) never run here — inferred types terminal-match via
    /// rmot, and the name-similarity strategies close the arm.
    fn match_method_call_free(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        // PHP `$this->prop.method` — exclusive declared-type path,
        // unconditional in both modes.
        if r.language == "php" {
            let re = self.cached_regex(r"^(this->[A-Za-z0-9_]+)\.([A-Za-z0-9_]+)$")?;
            if let Some(m) = re.captures(&r.reference_name) {
                let receiver = m[1].to_string();
                let php_method = m[2].to_string();
                let Some(inferred) =
                    self.infer_local_receiver_type(&receiver, r, false)?
                else {
                    return Ok(McRes::Null);
                };
                let fqn = self.imported_fqn_of(&inferred, r)?;
                return self.resolve_method_on_type(
                    &inferred,
                    &php_method,
                    r,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                );
            }
        }

        // Rust `Self::item` — associated-item path binding `Self` to the
        // caller's impl owner. Before the `!matched` bail so deeper paths
        // (`Self::Assoc::m`) reach it; a miss falls through like TS.
        if let McRes::Hit(c) = self.match_rust_self_path(r)? {
            return Ok(McRes::Hit(c));
        }

        let dot_re = self
            .cached_regex(r"^([A-Za-z0-9_.]+)\.([A-Za-z0-9_]+:?(?:[A-Za-z0-9_]+:)*)$")?;
        let mut dot_match = dot_re.captures(&r.reference_name);
        if dot_match.is_none() && r.language == "cpp" {
            let op_re = self.cached_regex(
                r"^([A-Za-z0-9_.]+)\.(operator[^A-Za-z0-9_\s.]+)$",
            )?;
            dot_match = op_re.captures(&r.reference_name);
        }
        let colon_re = self.cached_regex(r"^([A-Za-z0-9_]+)::([A-Za-z0-9_]+)$")?;
        let colon_match = colon_re.captures(&r.reference_name);
        // lua `:`/r `$` shapes are unmigrated-language only — dead here.
        let matched = dot_match.as_ref().or(colon_match.as_ref());
        let Some(m) = matched else {
            return Ok(McRes::Null);
        };
        let object_or_class = m[1].to_string();
        let method_name = m[2].to_string();
        let inferable = dot_match.is_some();

        if inferable {
            // No binding anchor under requireReceiverEvidence=false — the
            // inferrers run at the ref's own site with qualified names
            // normalized (preserveQualifiedName=false).
            let inferred = if r.language == "cpp" {
                self.infer_cpp_receiver_type(&object_or_class, r, 0, false)?
            } else {
                self.infer_local_receiver_type(&object_or_class, r, false)?
            };
            // mc-guarded/gofactory/iteration are evidence-gated in TS and
            // never run here; mc-await still does.
            if inferred.is_none()
                && is_esm_family(&r.language)
                && self.mc_await_gate(&object_or_class, r)?
            {
                return Ok(McRes::Punt("mc-await"));
            }
            if let Some(t) = inferred {
                // Java/Kotlin: the file's import pins WHICH same-named class.
                let fqn = if r.language == "java" || r.language == "kotlin" {
                    self.imported_fqn_of(&t, r)?
                } else {
                    None
                };
                match self.resolve_method_on_type(
                    &t,
                    &method_name,
                    r,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                )? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {
                        // A known builtin/primitive receiver is external when
                        // it has no project method — TS returns null here
                        // rather than letting Strategy 3 guess an unrelated
                        // `get`/`split`/`has`.
                        if is_esm_family(&r.language)
                            && (JS_BUILT_INS.contains(t.as_str())
                                || TS_PRIMITIVE_TYPES.contains(t.as_str()))
                        {
                            return Ok(McRes::Null);
                        }
                    }
                }
            }
        }

        // Go 2-hop field chain — EXCLUSIVE for chained Go receivers.
        if r.language == "go" && dot_match.is_some() && object_or_class.contains('.') {
            return self.match_go_field_chain_call(&object_or_class, &method_name, r);
        }
        // Rust `self.<field>.<method>` — EXCLUSIVE: validated field-type
        // inference or nothing (a null is the ref's verdict, not a fall-
        // through — the bare-name strategies fabricate this shape).
        if r.language == "rust" && dot_match.is_some() && object_or_class.starts_with("self.") {
            return self.match_rust_self_field_call(
                &object_or_class["self.".len()..],
                &method_name,
                r,
            );
        }
        // Rust `self.<method>` — EXCLUSIVE for the same reason: the owner
        // is the enclosing impl type on the caller's qualified name.
        if r.language == "rust" && dot_match.is_some() && object_or_class == "self" {
            return self.match_rust_self_call(&method_name, r);
        }

        // TS/JS `this.field.method` — EXCLUSIVE; the field's declared type
        // off the enclosing class, validated by rmot, or nothing.
        if matches!(r.language.as_str(), "typescript" | "javascript" | "tsx" | "jsx")
            && dot_match.is_some()
            && object_or_class.starts_with("this.")
        {
            return self.match_ts_this_field_call(
                &object_or_class["this.".len()..],
                &method_name,
                r,
            );
        }

        // Java/Kotlin field receiver inference — non-exclusive (a miss still
        // reaches the name strategies, exactly like TS).
        if (r.language == "java" || r.language == "kotlin") && dot_match.is_some() {
            if let Some(inferred) =
                self.infer_java_field_receiver_type(&object_or_class, r)?
            {
                let fqn = self.imported_fqn_of(&inferred, r)?;
                match self.resolve_method_on_type(
                    &inferred,
                    &method_name,
                    r,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                )? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {}
                }
            }
        }

        // Object-literal namespace receiver — same-file const/variable
        // holders; under requireReceiverEvidence=false there is no binding
        // filter — every holder gets its object-literal member scan.
        if dot_match.is_some()
            && !object_or_class.contains('.')
            && is_object_literal_language(&r.language)
        {
            let holders = prefer_call_site_file(
                self.nodes_by_name(&object_or_class)?
                    .iter()
                    .filter(|n| {
                        matches!(n.kind.as_str(), "constant" | "variable")
                            && n.file_path == r.file_path
                    })
                    .map(|n| Rc::new(n.clone()))
                    .collect(),
                &r.file_path,
            );
            for holder in &holders {
                if let Some(hit) = self.resolve_object_literal_member(
                    holder,
                    &method_name,
                    r,
                    0.85,
                    "instance-method",
                )? {
                    return Ok(McRes::Hit(hit));
                }
            }
        }

        // Strategy 1 — direct class-name match, call site's file first.
        let class_candidates = prefer_call_site_file(
            self.nodes_by_name(&object_or_class)?
                .iter()
                .map(|n| Rc::new(n.clone()))
                .collect(),
            &r.file_path,
        );
        for c in &class_candidates {
            if !matches!(c.kind.as_str(), "class" | "struct" | "union" | "interface") {
                continue;
            }
            if c.language != r.language {
                continue;
            }
            let in_file = self.nodes_in_file(&c.file_path)?;
            if let Some(mn) = in_file.iter().find(|n| {
                n.kind == "method"
                    && n.name == method_name
                    && n.qualified_name.contains(c.name.as_str())
            }) {
                return Ok(McRes::Hit(KCand {
                    node: Rc::new(mn.clone()),
                    confidence: 0.85,
                    resolved_by: "qualified-name",
                }));
            }
        }

        // Strategy 2 — capitalized receiver (`permissionEngine` →
        // `PermissionEngine`) against the same class scan.
        let mut cap_bytes = object_or_class.clone().into_bytes();
        if let Some(b) = cap_bytes.first_mut() {
            *b = b.to_ascii_uppercase();
        }
        let capitalized = String::from_utf8(cap_bytes).unwrap_or_default();
        if capitalized != object_or_class {
            let fuzzy_candidates = prefer_call_site_file(
                self.nodes_by_name(&capitalized)?
                    .iter()
                    .map(|n| Rc::new(n.clone()))
                    .collect(),
                &r.file_path,
            );
            for c in &fuzzy_candidates {
                if !matches!(c.kind.as_str(), "class" | "struct" | "union" | "interface") {
                    continue;
                }
                if c.language != r.language {
                    continue;
                }
                let in_file = self.nodes_in_file(&c.file_path)?;
                if let Some(mn) = in_file.iter().find(|n| {
                    n.kind == "method"
                        && n.name == method_name
                        && n.qualified_name.contains(c.name.as_str())
                }) {
                    return Ok(McRes::Hit(KCand {
                        node: Rc::new(mn.clone()),
                        confidence: 0.8,
                        resolved_by: "instance-method",
                    }));
                }
            }
        }

        // Strategy 3 — methods by name across the codebase, scored by
        // receiver-word overlap with the containing class name.
        if !method_name.is_empty() {
            let method_candidates = self.nodes_by_name(&method_name)?;
            // Ubiquitous-method ceiling: bail before the O(K) work.
            if method_candidates.len() as i64 > self.ambiguous_ceiling {
                return Ok(McRes::Null);
            }
            let methods: Vec<Rc<KNode>> = method_candidates
                .iter()
                .filter(|n| n.kind == "method" && n.name == method_name)
                .map(|n| Rc::new(n.clone()))
                .collect();
            let same_lang: Vec<Rc<KNode>> = methods
                .iter()
                .filter(|m| m.language == r.language)
                .cloned()
                .collect();
            let target = if !same_lang.is_empty() {
                &same_lang
            } else {
                &methods
            };
            if target.len() == 1 && target[0].language == r.language {
                return Ok(McRes::Hit(KCand {
                    node: target[0].clone(),
                    confidence: 0.7,
                    resolved_by: "instance-method",
                }));
            }
            if target.len() > 1 {
                let receiver_words = split_camel_case(&object_or_class);
                // Same-file candidates first, so a score tie resolves to the
                // call site's own file (`score > bestScore` keeps first seen).
                let ordered = prefer_call_site_file(target.clone(), &r.file_path);
                let mut best: Option<Rc<KNode>> = None;
                let mut best_score = 0i64;
                for m in &ordered {
                    let class_words = split_camel_case(&m.qualified_name);
                    let mut score = receiver_words
                        .iter()
                        .filter(|w| {
                            class_words
                                .iter()
                                .any(|cw| cw.eq_ignore_ascii_case(w))
                        })
                        .count() as i64;
                    if m.language == r.language {
                        score += 1;
                    }
                    if score > best_score {
                        best_score = score;
                        best = Some(m.clone());
                    }
                }
                if let Some(bm) = best {
                    if best_score >= 2 {
                        return Ok(McRes::Hit(KCand {
                            node: bm,
                            confidence: 0.65,
                            resolved_by: "instance-method",
                        }));
                    }
                }
            }
        }
        Ok(McRes::Null)
    }

    fn bound_receiver_claim(&mut self, r: &ResolveRefIn) -> Result<BoundClaim> {
        // `^(.+)\.([\w$]+)$` is guaranteed by the gate — split at the LAST dot.
        let dot = r.reference_name.rfind('.').unwrap();
        let receiver = &r.reference_name[..dot];
        let method = &r.reference_name[dot + 1..];
        let root = receiver.split('.').next().unwrap_or(receiver);
        let bindings = self.bindings(&r.file_path)?;
        let binding = Self::innermost_binding(&bindings, root, Some(r.line)).cloned();

        if !is_esm_family(&r.language) {
            // `binding?.kind === 'import' && !phpVariable` → br:import;
            // anything else is matchMethodCall receiver inference (source).
            let php_variable =
                r.language == "php" && self.ref_line_starts_with_dollar(r);
            if binding.as_ref().is_some_and(|b| b.kind == "import") && !php_variable {
                // java/kotlin bound-type resolution runs before the import
                // descent: an owner means btm owns the ref (or refuses a
                // deeper receiver); a miss falls through to br:import.
                if r.language == "java" || r.language == "kotlin" {
                    match self.resolve_bound_type(root, r, 0)? {
                        BtRes::Owner(_) => {
                            return Ok(if receiver == root {
                                Self::mc_to_claim(
                                    self.match_bound_type_member(root, method, r)?,
                                )
                            } else {
                                BoundClaim::Refused
                            });
                        }
                        BtRes::Null => {}
                        BtRes::Punt(p) => return Ok(BoundClaim::Punt(p)),
                    }
                }
                return Ok(match self.resolve_via_import_member(r)? {
                    ViaImport::Hit(c) => {
                        if matches!(
                            c.node.kind.as_str(),
                            "function" | "method" | "class" | "component"
                        ) {
                            BoundClaim::Hit(c)
                        } else {
                            BoundClaim::Refused
                        }
                    }
                    ViaImport::Miss => BoundClaim::Refused,
                    ViaImport::Punt(reason) => BoundClaim::Punt(reason),
                });
            }
            return Ok(Self::mc_to_claim(self.match_method_call(r)?));
        }

        if binding.as_ref().is_some_and(|b| b.kind == "import") {
            // The import resolver descends one member — a deeper receiver
            // must not mistake the first member for the call.
            if receiver.contains('.') {
                return Ok(BoundClaim::Refused);
            }
            return Ok(match self.resolve_via_import_member(r)? {
                ViaImport::Hit(c) => {
                    if matches!(
                        c.node.kind.as_str(),
                        "function" | "method" | "class" | "component"
                    ) || ((c.node.kind == "constant" || c.node.kind == "variable")
                        && method == "getState")
                    {
                        BoundClaim::Hit(c)
                    } else {
                        BoundClaim::Refused
                    }
                }
                ViaImport::Miss => BoundClaim::Refused,
                ViaImport::Punt(reason) => BoundClaim::Punt(reason),
            });
        }
        let Some(binding) = binding else {
            return Ok(BoundClaim::Refused);
        };
        if receiver.contains('.') {
            let parts: Vec<&str> = receiver.split('.').collect();
            if parts.len() != 2 {
                return Ok(BoundClaim::Refused);
            }
            // br:fieldinfer — root's declared type anchored at the binding
            // site (preserve qualified names), then the field on that owner.
            let mut site = Self::ref_clone(r);
            site.line = binding.line;
            if let Some(nid) = &binding.node_id {
                site.from_node_id = nid.clone();
            }
            let Some(ty) = self.infer_local_receiver_type(root, &site, true)? else {
                return Ok(BoundClaim::Refused);
            };
            let type_binding = Self::innermost_binding(
                &bindings,
                ty.split('.').next().unwrap_or(&ty),
                Some(binding.line),
            )
            .cloned();
            let owner_id = match &type_binding {
                Some(b) if b.kind == "import" => {
                    // `{ ...ref, referenceName: type, 'references' }` — the
                    // ORIGINAL ref, not the anchored site.
                    let mut ref2 = Self::ref_clone(r);
                    ref2.reference_name = ty.clone();
                    ref2.reference_kind = "references".to_string();
                    let via = if ty.contains('.') {
                        match self.resolve_via_import_member(&ref2)? {
                            ViaImport::Hit(c) => Some(c),
                            ViaImport::Miss => None,
                            ViaImport::Punt(p) => return Ok(BoundClaim::Punt(p)),
                        }
                    } else {
                        self.resolve_via_import(&ref2)?
                    };
                    via.map(|c| c.node.id.clone())
                }
                Some(b) => b.node_id.clone(),
                None => None,
            };
            let owner = match &owner_id {
                Some(id) => self.node_by_id(id)?,
                None => None,
            };
            return Ok(match owner {
                Some(o)
                    if matches!(
                        o.kind.as_str(),
                        "class" | "interface" | "component" | "type_alias"
                    ) =>
                {
                    Self::mc_to_claim(
                        self.match_ts_field_call_bound(o.as_ref(), parts[1], method, r)?,
                    )
                }
                _ => BoundClaim::Refused,
            });
        }
        match self.match_method_call(r)? {
            McRes::Hit(c) => return Ok(BoundClaim::Hit(c)),
            McRes::Punt(p) => return Ok(BoundClaim::Punt(p)),
            McRes::Null => {}
        }
        if binding.kind == "param" {
            return Ok(BoundClaim::Refused);
        }
        Ok(Self::mc_to_claim(
            self.esm_factory_tail(&binding, root, method, r)?,
        ))
    }

    /// resolveViaImport's non-bare slice (import-resolver.ts): the go/java/
    /// python language arms plus the `localName.member` descent. The c/cpp
    /// include arm lives in resolve_c_include_import_ref; module-file is
    /// dot-gated inside its own function.
    fn resolve_via_import_member(&mut self, r: &ResolveRefIn) -> Result<ViaImport> {
        let imports = self.import_mappings(&r.file_path)?;
        if imports.is_empty() && self.read_file(&r.file_path).is_none() {
            return Ok(ViaImport::Miss);
        }

        if r.language == "go" {
            if let Some(c) = self.resolve_go_cross_package(r, &imports)? {
                return Ok(ViaImport::Hit(c));
            }
        }
        if r.language == "java" || r.language == "kotlin" {
            if let Some(node) = self.resolve_java_imported_reference(r, &imports)? {
                return Ok(ViaImport::Hit(KCand {
                    node,
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }
        if r.language == "python" {
            if let Some(c) = self.resolve_python_module_member(r, &imports)? {
                return Ok(ViaImport::Hit(c));
            }
            if let Some(c) = self.resolve_python_absolute_module(r)? {
                return Ok(ViaImport::Hit(c));
            }
        }
        if matches!(
            r.language.as_str(),
            "python" | "typescript" | "tsx" | "javascript" | "jsx" | "arkts"
        ) {
            if let Some(node) = self.resolve_module_import_to_file(r, &imports)? {
                return Ok(ViaImport::Hit(KCand {
                    node,
                    confidence: 0.9,
                    resolved_by: "import",
                }));
            }
        }

        for imp in imports.iter() {
            let is_member = r
                .reference_name
                .starts_with(&format!("{}.", imp.local_name));
            if imp.local_name != r.reference_name && !is_member {
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
                // JS String.replace(string) removes the FIRST occurrence
                // anywhere — replacen(.., 1) matches, including the no-match
                // case that leaves the whole name as the member.
                member_name: if imp.is_namespace {
                    Some(r.reference_name.replacen(
                        &format!("{}.", imp.local_name),
                        "",
                        1,
                    ))
                } else {
                    None
                },
            };
            let mut visited = HashSet::new();
            let Some(target) = self.find_exported_symbol(
                &resolved_path,
                &want,
                &r.language,
                &mut visited,
                0,
            )?
            else {
                continue;
            };
            if !imp.is_namespace && is_member {
                if let Some(member_node) = self.resolve_static_member(&target, r, &imp.local_name)? {
                    return Ok(ViaImport::Hit(KCand {
                        node: member_node,
                        confidence: 0.9,
                        resolved_by: "import",
                    }));
                }
                if target.kind == "constant" || target.kind == "variable" {
                    // `if (member)` — an empty first segment skips the
                    // literal/alias arms entirely in TS.
                    let member0 = Self::js_slice(
                        &r.reference_name,
                        Self::utf16_len(&imp.local_name) + 1,
                    )
                    .split('.')
                    .next()
                    .unwrap_or("");
                    if !member0.is_empty() {
                        if let Some(lit) = self
                            .resolve_object_literal_member(&target, member0, r, 0.9, "import")?
                        {
                            return Ok(ViaImport::Hit(lit));
                        }
                        // resolveObjectLiteralAlias + resolveImportedInstanceMember
                        // read the exporting file — unported.
                        return Ok(ViaImport::Punt("via-src"));
                    }
                }
                // resolveImportedInstanceMember returns null for non-const/var
                // targets before reading anything — for them the decline rule
                // is the only remaining arm.
                if r.reference_kind == "calls"
                    && (target.kind == "function" || target.kind == "method")
                {
                    return Ok(ViaImport::Miss);
                }
            }
            return Ok(ViaImport::Hit(KCand {
                node: target,
                confidence: 0.9,
                resolved_by: "import",
            }));
        }
        Ok(ViaImport::Miss)
    }

    /// resolveGoCrossPackageReference (import-resolver.ts): `pkg.Member` via
    /// an in-module import — the package directory owns the member.
    fn resolve_go_cross_package(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Result<Option<KCand>> {
        let Some(mod_path) = self.go_module_path.clone() else {
            return Ok(None);
        };
        let Some(dot) = r.reference_name.find('.') else {
            return Ok(None);
        };
        if dot == 0 {
            return Ok(None);
        }
        let receiver = &r.reference_name[..dot];
        let member_name = &r.reference_name[dot + 1..];
        if member_name.is_empty() {
            return Ok(None);
        }
        for imp in imports.iter() {
            if imp.local_name != receiver {
                continue;
            }
            if imp.source != mod_path && !imp.source.starts_with(&format!("{}/", mod_path)) {
                continue;
            }
            let pkg_dir = if imp.source == mod_path {
                String::new()
            } else {
                imp.source[mod_path.len() + 1..].to_string()
            };
            for node in self.nodes_by_name(member_name)?.iter() {
                if node.language != "go" || !node.is_exported {
                    continue;
                }
                let fp = node.file_path.replace('\\', "/");
                let file_dir = fp.rfind('/').map(|i| &fp[..i]).unwrap_or("");
                if file_dir == pkg_dir {
                    return Ok(Some(KCand {
                        node: Rc::new(node.clone()),
                        confidence: 0.9,
                        resolved_by: "import",
                    }));
                }
            }
        }
        Ok(None)
    }

    /// resolvePythonModuleMember (import-resolver.ts): `mod.func` after
    /// `import mod` / `from pkg import mod` — the receiver is a submodule.
    fn resolve_python_module_member(
        &mut self,
        r: &ResolveRefIn,
        imports: &[KImport],
    ) -> Result<Option<KCand>> {
        let Some(dot_idx) = r.reference_name.find('.') else {
            return Ok(None);
        };
        if dot_idx == 0 {
            return Ok(None);
        }
        let receiver = &r.reference_name[..dot_idx];
        let members: Vec<&str> = r.reference_name[dot_idx + 1..].split('.').collect();
        if members.first().is_none_or(|m| m.is_empty()) {
            return Ok(None);
        }

        for imp in imports.iter() {
            if imp.local_name != receiver {
                continue;
            }
            // `from pkg import mod as alias` — join with the EXPORTED name.
            let module_name = if imp.exported_name == "*" {
                imp.local_name.clone()
            } else {
                imp.exported_name.clone()
            };
            let module_path = if imp.is_namespace {
                imp.source.clone()
            } else if imp.source.ends_with('.') {
                format!("{}{}", imp.source, module_name)
            } else {
                format!("{}.{}", imp.source, module_name)
            };
            let mut remaining: Vec<&str> = members.clone();
            if imp.is_namespace && imp.source.starts_with(&format!("{}.", receiver)) {
                // JS slice counts UTF-16 units — receiver may not be ASCII.
                let suffix: Vec<&str> =
                    Self::js_slice(&imp.source, Self::utf16_len(receiver) + 1)
                        .split('.')
                        .collect();
                if !suffix.iter().enumerate().all(|(i, s)| remaining.get(i) == Some(s)) {
                    continue;
                }
                remaining = remaining[suffix.len()..].to_vec();
            }
            if remaining.len() != 1 {
                continue;
            }
            let member = remaining[0];
            let mut resolved_path =
                self.resolve_import_path(&module_path, &r.file_path, &r.language)?;
            if resolved_path.is_none() {
                resolved_path = self
                    .find_python_module_file(&module_path, &r.file_path)?
                    .map(|n| n.file_path.clone());
            }
            let Some(resolved_path) = resolved_path else { continue };
            if resolved_path == r.file_path {
                continue;
            }
            let nodes = self.nodes_in_file(&resolved_path)?;
            let target = nodes.iter().find(|n| {
                n.name == member
                    && matches!(
                        n.kind.as_str(),
                        "function" | "class" | "variable" | "constant"
                    )
            });
            if let Some(target) = target {
                return Ok(Some(KCand {
                    node: Rc::new(target.clone()),
                    confidence: 0.85,
                    resolved_by: "import",
                }));
            }
        }
        Ok(None)
    }

    /// resolvePythonAbsoluteModule (import-resolver.ts): a dotted `imports`
    /// ref is the full module path — resolve to its file node.
    fn resolve_python_absolute_module(
        &mut self,
        r: &ResolveRefIn,
    ) -> Result<Option<KCand>> {
        if r.reference_kind != "imports" || !r.reference_name.contains('.') {
            return Ok(None);
        }
        Ok(self
            .find_python_module_file(&r.reference_name, &r.file_path)?
            .map(|node| KCand {
                node,
                confidence: 0.9,
                resolved_by: "import",
            }))
    }

    /// resolveStaticMember (import-resolver.ts): `Container.member` on a
    /// named class import — the `Container::member` qualifiedName inside the
    /// container's own file.
    fn resolve_static_member(
        &mut self,
        container: &KNode,
        r: &ResolveRefIn,
        local_name: &str,
    ) -> Result<Option<Rc<KNode>>> {
        if !is_static_member_container(&container.kind) {
            return Ok(None);
        }
        let member = Self::js_slice(&r.reference_name, Self::utf16_len(local_name) + 1)
            .split('.')
            .next()
            .unwrap_or("");
        if member.is_empty() {
            return Ok(None);
        }
        let member_qn = format!("{}::{}", container.qualified_name, member);
        let candidates: Vec<Rc<KNode>> = self
            .nodes_by_qualified_name(&member_qn)?
            .iter()
            .filter(|n| n.file_path == container.file_path)
            .map(|n| Rc::new(n.clone()))
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        if r.reference_kind == "calls" {
            if let Some(callable) = candidates
                .iter()
                .find(|n| n.kind == "method" || n.kind == "function")
            {
                return Ok(Some(callable.clone()));
            }
        }
        Ok(Some(candidates[0].clone()))
    }

    /// resolveObjectLiteralMember (name-matcher.ts): an imported object
    /// literal used as a namespace — find the member by containment.
    fn resolve_object_literal_member(
        &mut self,
        container: &KNode,
        member: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
    ) -> Result<Option<KCand>> {
        if container.kind != "constant" && container.kind != "variable" {
            return Ok(None);
        }
        if !is_object_literal_language(&container.language) {
            return Ok(None);
        }
        if !same_language_family(&container.language, &r.language) {
            return Ok(None);
        }
        let in_file = self.nodes_in_file(&container.file_path)?;
        let callable =
            |n: &KNode| n.kind == "function" || n.kind == "method";
        let accepts = |n: &KNode| {
            if r.reference_kind == "calls" {
                callable(n)
            } else {
                callable(n)
                    || n.kind == "property"
                    || n.kind == "variable"
                    || n.kind == "constant"
            }
        };
        // rangeWithin / sameRange (name-matcher.ts).
        let range_within = |inner: &KNode, outer: &KNode| {
            !(inner.start_line < outer.start_line
                || inner.end_line > outer.end_line
                || (inner.start_line == outer.start_line
                    && inner.start_column < outer.start_column)
                || (inner.end_line == outer.end_line && inner.end_column > outer.end_column))
        };
        let same_range = |a: &KNode, b: &KNode| {
            a.start_line == b.start_line
                && a.start_column == b.start_column
                && a.end_line == b.end_line
                && a.end_column == b.end_column
        };
        let inside: Vec<Rc<KNode>> = in_file
            .iter()
            .filter(|n| n.id != container.id && range_within(n, container))
            .map(|n| Rc::new(n.clone()))
            .collect();
        let mut candidates: Vec<Rc<KNode>> = inside
            .iter()
            .filter(|n| n.name == member && accepts(n))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        // Drop members nested inside another callable's body in the literal.
        let bodies: Vec<&Rc<KNode>> = inside.iter().filter(|n| callable(n)).collect();
        candidates.retain(|c| {
            !bodies
                .iter()
                .any(|b| b.id != c.id && !same_range(b, c) && range_within(c, b))
        });
        if candidates.is_empty() {
            return Ok(None);
        }
        candidates.sort_by(|a, b| {
            let ca = if callable(a) { 0 } else { 1 };
            let cb = if callable(b) { 0 } else { 1 };
            ca.cmp(&cb)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.start_column.cmp(&b.start_column))
        });
        Ok(Some(KCand {
            node: candidates[0].clone(),
            confidence,
            resolved_by,
        }))
    }

    /// matchByQualifiedName (name-matcher.ts) — exact `qualifiedName` lookup,
    /// then the last-segment suffix match. Erlang's arity arms are dead (not
    /// a migrated language).
    fn match_by_qualified_name(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        if !r.reference_name.contains("::") && !r.reference_name.contains('.') {
            return Ok(None);
        }
        // A `calls` ref never resolves to a yaml/properties config key (#1180).
        let keep_for_ref = |nodes: &[KNode]| -> Vec<Rc<KNode>> {
            nodes
                .iter()
                .filter(|n| {
                    r.reference_kind != "calls"
                        || !(n.kind == "constant"
                            && (n.language == "yaml" || n.language == "properties"))
                })
                .map(|n| Rc::new(n.clone()))
                .collect()
        };

        let candidates = keep_for_ref(self.nodes_by_qualified_name(&r.reference_name)?.as_slice());
        if candidates.len() == 1 {
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: 0.95,
                resolved_by: "qualified-name",
            }));
        }
        if candidates.len() > 1 {
            let ordered = prefer_call_site_file(candidates, &r.file_path);
            if ordered[0].file_path == r.file_path {
                return Ok(Some(KCand {
                    node: ordered[0].clone(),
                    confidence: 0.95,
                    resolved_by: "qualified-name",
                }));
            }
        }

        // Partial match — the last `:`/`.` segment, then the suffix filter.
        let last_name = r
            .reference_name
            .rsplit([':', '.'])
            .next()
            .unwrap_or("");
        if !last_name.is_empty() {
            let partial = keep_for_ref(self.nodes_by_name(last_name)?.as_slice())
                .into_iter()
                .filter(|n| n.qualified_name.ends_with(&r.reference_name))
                .collect();
            let chosen = prefer_call_site_file(partial, &r.file_path);
            if let Some(first) = chosen.into_iter().next() {
                return Ok(Some(KCand {
                    node: first,
                    confidence: 0.85,
                    resolved_by: "qualified-name",
                }));
            }
        }
        Ok(None)
    }

    /// resolveOneInner for non-bare refs in migrated languages. Ported arms
    /// run in TS order; every unported arm punts so the TS spine re-derives
    /// the outcome. A bound-receiver claim is terminal — a refusal never
    /// falls through to name matching.
    fn resolve_nonbare_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
        if self.is_built_in_or_external(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        // Prefilter — `existenceName` strips arkts' leading '.';
        // matchJsStoreBindingCall needs a dot-free name, so a non-bare
        // `Foo::bar` can still reach it — punt when the source check applies.
        let existence = if r.language == "arkts" && r.reference_name.starts_with('.') {
            &r.reference_name[1..]
        } else {
            &r.reference_name
        };
        let pre_pass = self.has_any_possible_match(existence)
            || self.matches_any_import(r)?
            || self.framework_claims(&r.reference_name);
        if !pre_pass {
            if self.is_bare_js_call(r)? {
                return Ok(ResolveOutcome::passthrough("store-bind"));
            }
            return Ok(ResolveOutcome::unresolved());
        }

        // `function_ref` refs resolve ONLY through matchFunctionRef, never
        // the fallthrough below — TS's function_ref block for non-bare
        // names, in order: viaImport (a `.` member-descent can still claim
        // an `a.b` function_ref), then the `::` member-pointer arm (the
        // only non-bare shape matchFunctionRef resolves; `.`/`this.` forms
        // always miss in it). A miss punts back to that same block.
        if r.reference_kind == "function_ref" {
            match self.resolve_via_import_member(r)? {
                ViaImport::Punt(reason) => {
                    return Ok(ResolveOutcome::passthrough(reason));
                }
                ViaImport::Miss => {}
                ViaImport::Hit(c) => {
                    // An import resolving to a non-callable is discarded —
                    // the scoped arm still runs, exactly like the bare path.
                    if let Some(c) = self.gate_language(Some(c), r) {
                        if c.node.kind == "function"
                            || c.node.kind == "method"
                            || (r.language == "python" && c.node.kind == "class")
                        {
                            return self.finish(r, c, None, true);
                        }
                    }
                }
            }
            if let Some(c) = self.match_function_ref_scoped(r)? {
                // Frameworks never run on this path — a gated candidate is
                // discarded to terminal unresolved, exactly like the bare arm.
                return match self.gate_language(Some(c), r) {
                    Some(c) => self.finish(r, c, None, true),
                    None => Ok(ResolveOutcome::unresolved()),
                };
            }
            return Ok(ResolveOutcome::passthrough("member-tail"));
        }
        // resolveJvmImport (java/kotlin `imports` refs) reads decorators —
        // an unselected column; the arm stays in TS.
        if r.reference_kind == "imports"
            && (r.language == "java" || r.language == "kotlin")
        {
            return Ok(ResolveOutcome::passthrough("jvm"));
        }
        // PHP `imports` refs take the include-path arm inside resolveViaImport
        // (unported — include-path file resolution) — punt the whole kind.
        if r.language == "php" && r.reference_kind == "imports" {
            return Ok(ResolveOutcome::passthrough("php-inc"));
        }
        // resolvePhpImportedStaticCall — terminal before frameworks.
        if let Some(outcome) = self.resolve_php_imported_static(r)? {
            return Ok(outcome);
        }
        // matchBoundReceiverCall — claimed refs are terminal either way.
        if is_binding_receiver_call(r) {
            match self.bound_receiver_claim(r)? {
                BoundClaim::Punt(reason) => {
                    return Ok(ResolveOutcome::passthrough(reason));
                }
                BoundClaim::Refused => {
                    return Ok(if self.frameworks_active {
                        ResolveOutcome::no_candidates()
                    } else {
                        ResolveOutcome::unresolved()
                    });
                }
                BoundClaim::Hit(c) => {
                    let gated = self.gate_language(Some(c), r);
                    return match gated {
                        Some(cand) => {
                            let Some(winner) = self.gate_target_kind(cand, r)? else {
                                return Ok(if self.frameworks_active {
                                    ResolveOutcome::no_candidates()
                                } else {
                                    ResolveOutcome::unresolved()
                                });
                            };
                            if self.frameworks_active {
                                // Framework <0.9 candidates merge first-max;
                                // a ≥0.9 framework hit would have pre-empted.
                                let reported = vec![KernelCandidateOut {
                                    target_node_id: winner.node.id.clone(),
                                    confidence: winner.confidence,
                                    resolved_by: winner.resolved_by.to_string(),
                                }];
                                self.finish(r, winner, Some(reported), false)
                            } else {
                                self.finish(r, winner, None, true)
                            }
                        }
                        None => Ok(if self.frameworks_active {
                            ResolveOutcome::no_candidates()
                        } else {
                            ResolveOutcome::unresolved()
                        }),
                    };
                }
            }
        }
        // isUnresolvedJsMemberCall — terminal null; framework candidates are
        // discarded by the TS `return null` as well.
        if is_unresolved_js_member_call(r) {
            return Ok(ResolveOutcome::unresolved());
        }
        // The chain guard routes `x().y` calls through matchReference only —
        // the chain matchers there (storeAccessorChain et al.) are unported.
        if r.reference_kind == "calls"
            && CHAIN_SHAPE_RE.is_match(&r.reference_name)
            && matches!(
                r.language.as_str(),
                "typescript" | "javascript" | "tsx" | "jsx" | "python"
            )
        {
            return Ok(ResolveOutcome::passthrough("chain"));
        }

        let mut cands: Vec<KCand> = Vec::new();
        match self.resolve_via_import_member(r)? {
            ViaImport::Punt(reason) => {
                return Ok(ResolveOutcome::passthrough(reason));
            }
            ViaImport::Miss => {}
            ViaImport::Hit(c) => {
                if let Some(c) = self.gate_language(Some(c), r) {
                    if c.confidence >= 0.9 {
                        let Some(winner) = self.gate_target_kind(c, r)? else {
                            return Ok(if self.frameworks_active {
                                ResolveOutcome::passthrough("gated-import")
                            } else {
                                ResolveOutcome::unresolved()
                            });
                        };
                        return self.finish(r, winner, None, true);
                    }
                    cands.push(c);
                }
            }
        }
        // isPhpIncludePathRef / cobol / nix / terraform terminal-merge arm:
        // php imports punt above; the rest are unmigrated languages.

        // matchReference's leading arkts arm — `.attr` names resolve ONLY to
        // decorator-marked helpers, never the name-match fallthrough.
        if r.language == "arkts" && r.reference_name.starts_with('.') {
            return Ok(ResolveOutcome::passthrough("arkts-dot"));
        }
        // matchReference's ported arms, in order: filePath, qualifiedName,
        // the per-language chain arm (cppChain/scopedChain/dottedChain), then
        // methodCall's requireReceiverEvidence=false arm. Everything after
        // (exactName/fuzzy, then deferred drains) stays in TS behind the
        // member-tail punt.
        let name_cand = match self.match_by_file_path(r)? {
            Some(c) => Some(c),
            None => match self.match_by_qualified_name(r)? {
                Some(c) => Some(c),
                None => match self.match_call_chain(r)? {
                    McRes::Hit(c) => Some(c),
                    McRes::Punt(p) => return Ok(ResolveOutcome::passthrough(p)),
                    McRes::Null => match self.match_method_call_free(r)? {
                        McRes::Hit(c) => Some(c),
                        McRes::Punt(p) => return Ok(ResolveOutcome::passthrough(p)),
                        McRes::Null => None,
                    },
                },
            },
        };
        let name_result = self.gate_language(name_cand, r);
        if let Some(c) = name_result {
            if self.is_visible_across_files(&c.node, r)? {
                cands.push(c);
            }
        }
        if cands.is_empty() {
            // nameMatch's remaining arms may still hit — the ref goes back.
            return Ok(ResolveOutcome::passthrough("member-tail"));
        }
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
                return Ok(ResolveOutcome {
                    candidates: reported,
                    ..ResolveOutcome::unresolved()
                });
            }
        };
        self.finish(r, winner, reported, false)
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

    /// aliasTargetName + resolveAliasBinding (alias-binding.ts). `member_name`
    /// is the ref's last `.` segment (null for bare/`::`-only names): a
    /// member access through an object-literal alias resolves the keyed or
    /// shorthand property binding (`{ member: fn }` / `{ member }`).
    fn resolve_alias_binding(
        &mut self,
        alias_node: &KNode,
        member_name: Option<&str>,
    ) -> Result<Option<Rc<KNode>>> {
        if !is_alias_binding_kind(&alias_node.kind) {
            return Ok(None);
        }
        let sig = alias_node.signature.as_deref().unwrap_or("").trim();
        let target_name = match member_name {
            Some(m) if !m.is_empty() => {
                let key = regex::escape(m);
                let explicit = self
                    .cached_regex(&format!(
                        r"[{{,]\s*{}\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[,}}]",
                        key
                    ))?
                    .captures(sig)
                    .map(|c| c.get(1).unwrap().as_str().to_string());
                match explicit {
                    Some(t) => Some(t),
                    None => self
                        .cached_regex(&format!(r"[{{,]\s*({})\s*[,}}]", key))?
                        .captures(sig)
                        .map(|c| c.get(1).unwrap().as_str().to_string()),
                }
            }
            _ => BARE_ALIAS_RE
                .captures(sig)
                .map(|c| c.get(1).unwrap().as_str().to_string()),
        };
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

    /// matchFunctionRef's `::` member-pointer arm (name-matcher.ts): an
    /// explicit `Cls::member` shape (`&Widget::on_click` emitted as
    /// `Widget::on_click`) resolves the member ON THAT SCOPE — exempt from
    /// bareFnOnly, origin excluded, qualified-name equality or `::`-suffix.
    /// Same-file pool wins by earliest line @0.9; cross-file unique-or-drop.
    fn match_function_ref_scoped(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let Some(sep) = r.reference_name.rfind("::") else {
            return Ok(None);
        };
        let member = &r.reference_name[sep + 2..];
        let suffix = format!("::{}", r.reference_name);
        let scoped: Vec<Rc<KNode>> = self
            .nodes_by_name(member)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "function" | "method")
                    && same_language_family(&n.language, &r.language)
                    && n.id != r.from_node_id
                    && (n.qualified_name == r.reference_name
                        || n.qualified_name.ends_with(&suffix))
            })
            .map(|n| Rc::new(n.clone()))
            .collect();
        if scoped.is_empty() {
            return Ok(None);
        }
        let same_file: Vec<Rc<KNode>> = scoped
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        if same_file.is_empty() && scoped.len() > 1 {
            return Ok(None);
        }
        let pool = if same_file.is_empty() { scoped } else { same_file };
        // `<=` reduce keeps the first minimum — min_by_key does the same.
        let target = pool.iter().min_by_key(|n| n.start_line).unwrap().clone();
        Ok(Some(KCand {
            node: target,
            confidence: 0.9,
            resolved_by: "function-ref",
        }))
    }

    /// The Rust slice of resolveOneInner for pure `::` path refs
    /// (`crate::m::Item`, `self::sub::f`, `super::x::y`, `a::b::c`). TS
    /// reaches resolveViaImport → resolveRustPathReference on these; the
    /// boundReceiver claim can't fire on a dot-free name, and every gate
    /// between the prefilter and viaImport is language- or shape-gated
    /// dead for them. A `::`-AND-`.` name (a receiver-shaped `a::b.c`)
    /// stays on the TS side — the claim can own it there.
    /// `None` = the module-path arm missed; the caller hands migrated rust
    /// to the normal pipeline (qualified-name/exact arms mirror TS's
    /// matchReference continuation) and non-migrated rust back to TS.
    fn resolve_rust_path_ref(&mut self, r: &ResolveRefIn) -> Result<Option<ResolveOutcome>> {
        if self.is_built_in_or_external(r) {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let pre_pass = self.has_any_possible_match(&r.reference_name)
            || self.matches_any_import(r)?
            || self.framework_claims(&r.reference_name);
        if !pre_pass {
            // matchJsStoreBindingCall is JS-gated — dead for rust — so a
            // prefilter miss is terminal unresolved in TS.
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        // resolveViaImport early-nulls when imports are empty AND the file
        // is unreadable — for rust (no bindings rows) that's every missing
        // file, which then falls to matchReference's unported arms.
        if self.read_file(&r.file_path).is_none() {
            return Ok(Some(ResolveOutcome::passthrough("ineligible:lang")));
        }
        match self.match_rust_path_reference(r)? {
            // A gated candidate is discarded to terminal unresolved — the
            // spine returns the import hit verbatim at ≥0.9, and this arm
            // always carries 0.9.
            Some(c) => match self.gate_language(Some(c), r) {
                Some(c) => self.finish(r, c, None, true).map(Some),
                None => Ok(Some(ResolveOutcome::unresolved())),
            },
            None => Ok(None),
        }
    }

    /// resolveRustPathReference (import-resolver.ts): split `A::B::C` into
    /// module prefix `A::B` + leaf `C`, map the prefix to a file, find the
    /// leaf symbol in it. `import` @0.9 — the analog of
    /// resolvePythonModuleMember for Rust module paths.
    fn match_rust_path_reference(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let segments: Vec<&str> = r
            .reference_name
            .split("::")
            .filter(|s| !s.is_empty())
            .collect();
        if segments.len() < 2 {
            return Ok(None);
        }
        let leaf = segments[segments.len() - 1];
        let Some(file) = self.resolve_rust_module_file(&segments[..segments.len() - 1], &r.file_path)?
        else {
            return Ok(None);
        };
        if file == r.file_path {
            return Ok(None);
        }
        let file_nodes = self.nodes_in_file(&file)?;
        let target = file_nodes.iter().find(|n| {
            n.name == leaf
                && matches!(
                    n.kind.as_str(),
                    "function"
                        | "struct"
                        | "union"
                        | "enum"
                        | "trait"
                        | "type_alias"
                        | "constant"
                        | "method"
                        | "class"
                        | "interface"
                )
        });
        Ok(target.map(|n| KCand {
            node: Rc::new(n.clone()),
            confidence: 0.9,
            resolved_by: "import",
        }))
    }

    /// resolveRustModuleFile (import-resolver.ts): map module segments to
    /// `<seg>.rs` or `<seg>/mod.rs` files. Anchors on `crate`/`self`/`super`;
    /// a bare path tries self-relative (2018 expression position) then
    /// crate-relative (2015 crate-root items). External crates miss both.
    fn resolve_rust_module_file(
        &self,
        segments: &[&str],
        from_file: &str,
    ) -> Result<Option<String>> {
        if segments.is_empty() {
            return Ok(None);
        }
        let first = segments[0];
        if first == "crate" {
            return Ok(self.rust_resolve_under(
                self.rust_crate_root_dir(from_file),
                &segments[1..],
            ));
        }
        if first == "self" {
            return Ok(self.rust_resolve_under(
                Some(rust_self_module_dir(from_file)),
                &segments[1..],
            ));
        }
        if first == "super" {
            let mut supers = 0usize;
            while segments.get(supers) == Some(&"super") {
                supers += 1;
            }
            let mut dir = Some(rust_self_module_dir(from_file));
            for _ in 0..supers {
                dir = dir.map(|d| pos_dirname(&d).to_string());
            }
            return Ok(self.rust_resolve_under(dir, &segments[supers..]));
        }
        Ok(self
            .rust_resolve_under(Some(rust_self_module_dir(from_file)), segments)
            .or_else(|| {
                self.rust_resolve_under(self.rust_crate_root_dir(from_file), segments)
            }))
    }

    /// The `resolveUnder` closure inside resolveRustModuleFile: walk module
    /// segments down from `start_dir`, each mapping to `<seg>.rs` or
    /// `<seg>/mod.rs`; `self`/`crate`/`super` mid-path are skipped (leading
    /// `super`s are consumed by the anchor dispatch). Returns the leaf
    /// module's file.
    fn rust_resolve_under(&self, start_dir: Option<String>, rest: &[&str]) -> Option<String> {
        let mut dir = start_dir?;
        let mut target: Option<String> = None;
        for seg in rest {
            if *seg == "self" || *seg == "crate" || *seg == "super" {
                continue;
            }
            let as_file = pos_normalize(&format!("{}/{}.rs", dir, seg));
            let as_mod = pos_normalize(&format!("{}/{}/mod.rs", dir, seg));
            if self.file_exists(&as_file) {
                target = Some(as_file);
            } else if self.file_exists(&as_mod) {
                target = Some(as_mod);
            } else {
                return None;
            }
            dir = pos_normalize(&format!("{}/{}", dir, seg));
        }
        target
    }

    /// rustCrateRootDir (import-resolver.ts): the directory holding
    /// `lib.rs`/`main.rs`, walking up from the ref's file (≤64 levels).
    fn rust_crate_root_dir(&self, from_file: &str) -> Option<String> {
        let mut dir = pos_dirname(from_file).to_string();
        for _ in 0..64 {
            if self.file_exists(&pos_normalize(&format!("{}/lib.rs", dir)))
                || self.file_exists(&pos_normalize(&format!("{}/main.rs", dir)))
            {
                return Some(dir);
            }
            let parent = pos_dirname(&dir);
            if parent == dir {
                return None;
            }
            dir = parent.to_string();
        }
        None
    }

    fn resolve_ref(&mut self, r: &ResolveRefIn) -> Result<ResolveOutcome> {
        // Rust pure-`::` path refs (`crate::m::Item`, `a::b::c`): TS
        // resolves them through resolveViaImport's module-file arm, which
        // needs no bindings rows — run it ahead of the eligibility gate.
        // A miss falls through for migrated rust (qualified-name/exact arms
        // mirror matchReference's continuation) and punts otherwise.
        // Dot-bearing `::` names stay punted (the boundReceiver claim can
        // own a `a::b.c` receiver in TS), and `function_ref` keeps its own
        // block — a module-path hit on a non-callable leaf is DISCARDED
        // there, not returned.
        if r.language == "rust"
            && r.reference_kind != "function_ref"
            && r.reference_name.contains("::")
            && !r.reference_name.contains('.')
        {
            match self.resolve_rust_path_ref(r)? {
                Some(o) => return Ok(o),
                None if is_migrated_language(&r.language) => {}
                None => return Ok(ResolveOutcome::passthrough("ineligible:lang")),
            }
        }
        // ref_is_eligible, split so the passthrough reason names the gate.
        if !is_migrated_language(&r.language) {
            return Ok(ResolveOutcome::passthrough("ineligible:lang"));
        }
        if !Self::name_is_bare(&r.reference_name) {
            // The measured-dominant slice of the non-bare tail (§5.14):
            // C/C++ `#include` path refs resolve through their own arm.
            if (r.language == "c" || r.language == "cpp") && r.reference_kind == "imports" {
                return self.resolve_c_include_import_ref(r);
            }
            // Rust dotted receivers: `calls` names without `::`/`()` ride the
            // ported pipeline — inferLocalReceiverType (`let ctx: Ctx`) is
            // native for rust now, and the self.-arms/strategies that follow
            // reproduce TS's tail (§5.25's confidence-drift class is exactly
            // what the inference arm resolves natively). `a::b.c`/`x::y().z`
            // (::+.), `x().y`, and non-call `x.y`/`self.x` stay punted — TS
            // verdicts by delegation.
            if r.language == "rust"
                && r.reference_name.contains('.')
                && !(r.reference_kind == "calls"
                    && (r.reference_name.starts_with("self.")
                        || (!r.reference_name.contains("::")
                            && !r.reference_name.contains("()"))))
            {
                return Ok(ResolveOutcome::passthrough("member-tail"));
            }
            // Member-access slice (§5.16): boundReceiver's DB sub-arms, the
            // import member descent, filePath and qualifiedName — the rest
            // of the member matchers punt back to the TS spine.
            return self.resolve_nonbare_ref(r);
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
        // Rust bare `Self` — a references/instantiates/calls ref naming the
        // enclosing impl's type. Binds to the concrete owner off the
        // caller's qualified name; a trait-kind owner is the abstract
        // implementor and declines. Advisory: a miss keeps the ref's normal
        // bare-name verdict.
        if r.language == "rust" && r.reference_name == "Self" {
            if let Some(c) = self.match_rust_bare_self(r)? {
                return self.finish(r, c, None, true);
            }
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
            // memberName = the last `.` segment — `Cls::member` and bare
            // names carry none (no '.' → bare arm); `a.b::c` yields `b::c`,
            // matching lastIndexOf('.') + slice.
            let member_name = r
                .reference_name
                .rfind('.')
                .map(|i| &r.reference_name[i + 1..]);
            if let Some(forwarded) = self.resolve_alias_binding(&winner.node, member_name)? {
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
