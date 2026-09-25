//! Language, kind and framework tables — ports of the TS sets in src/resolution/{index,name-matcher,frameworks}.ts — plus the fixed regexes and small predicates the resolver shares.

use super::*;

// ---------------------------------------------------------------------------
// Tables — ports of the TS sets in src/resolution/{index,name-matcher,
// import-resolver}.ts.
// ---------------------------------------------------------------------------

/// Kernel pipeline eligibility: every language with a native walker. The
/// first thirteen are BINDINGS_LANGUAGES (src/extraction/kernel/index.ts);
/// the last eight emit no binding rows, so — exactly like rust before its
/// `use` rows landed — their import mappings are empty on both engines and
/// every arm they reach is a bindings-free join or a ported source scan.
/// Their language-specific TS arms (receiver-type patterns, the lua `:` /
/// r `$` receiver shapes, lua `require`) are ported below; anything else
/// they touch punts by the same gates the migrated set uses.
pub(super) fn is_migrated_language(lang: &str) -> bool {
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
            | "csharp"
            | "ruby"
            | "swift"
            | "scala"
            | "dart"
            | "lua"
            | "luau"
            | "r"
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

pub(super) static DRUPAL_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*::?[A-Za-z0-9_]+$").unwrap());
pub(super) static EXPO_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\.)(?:push|replace|navigate|dismissTo)$|^[a-z][A-Za-z]*(?:Push|Replace|Navigate)$")
        .unwrap()
});
pub(super) static LARAVEL_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*Controller@[A-Za-z0-9_]+$").unwrap());
pub(super) static NEXT_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\.)(?:push|replace|prefetch)$|^(?:redirect|permanentRedirect)$|^(?:NextResponse|Response)\.redirect$").unwrap()
});
pub(super) static PLAY_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*\.[A-Za-z_][A-Za-z0-9_]*$").unwrap());
pub(super) static RR_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:history|navigate|router)\.(?:push|replace|navigate)$|^(?:navigate|redirect)$")
        .unwrap()
});
pub(super) static RAILS_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_/]+#[A-Za-z0-9_]+$").unwrap());
pub(super) static TANSTACK_NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:navigate|redirect)$|^(?:router|Route)\.navigate$").unwrap()
});
pub(super) static TERRA_CLAIM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^module\.[^.:\s]+:(?:file$|var\.|output\.|remote-output\.)").unwrap()
});
pub(super) static VUE_NAV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$?router\.(?:push|replace)$|^navigateTo$").unwrap());
/// matchByFilePath's shape gate — `\.ext` (1–4 chars) or `.markdown` tail.
pub(super) static FILE_PATH_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\.[A-Za-z][A-Za-z0-9]{0,3}|\.markdown)$").unwrap());
/// hasAnyPossibleMatch's bare-filename tail check (`\.[A-Za-z0-9]+$`).
pub(super) static EXT_TAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.[A-Za-z0-9]+$").unwrap());
/// isBindingReceiverCall's name shape — `^.+\.[\w$]+$` (JS `\w` is ASCII).
pub(super) static BOUND_RECEIVER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.+\.[A-Za-z0-9_$]+$").unwrap());
/// isBindingReceiverCall's excluded receiver roots — `^(this|self|super|cls)(\.|$)`.
pub(super) static BOUND_ROOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:this|self|super|cls)(?:\.|$)").unwrap());
/// isUnresolvedJsMemberCall's retained chain — `^[A-Za-z_$][\w$]*(\.[A-Za-z_$][\w$]*){2,}$`.
pub(super) static JS_MEMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*){2,}$").unwrap()
});
/// isUnresolvedJsMemberCall's excluded roots — `^(?:this|window)\.`.
pub(super) static JS_MEMBER_ROOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:this|window)\.").unwrap());
/// CHAIN_SHAPE (index.ts) — `^(.+)\(\)\.(\w+)$`: a call-receiver chain.
pub(super) static CHAIN_SHAPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.+\(\)\.[A-Za-z0-9_]+$").unwrap());
/// The chain arms' `<inner>().<method>` capture — `^(.+)\(\)\.(\w+)$`
/// with TS's ASCII `\w`.
pub(super) static CALL_CHAIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+)\(\)\.([A-Za-z0-9_]+)$").unwrap());
/// CONSTRUCTS_VIA_BARE_CALL (name-matcher.ts) — languages where an
/// unprefixed capitalized `Foo(args)` constructs the class.
pub(super) static CONSTRUCTS_VIA_BARE_CALL: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        ["kotlin", "swift", "scala", "dart", "pascal"].into_iter().collect()
    });
/// resolvePhpImportedStaticCall's receiver shape — `^(\w+)\.(\w+)$`.
pub(super) static PHP_STATIC_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_]+)\.([A-Za-z0-9_]+)$").unwrap());

/// RUST_NON_PROJECT_FIELD_TYPES (name-matcher.ts): primitives and prelude
/// types — a field of one never names a project type.
pub(super) static RUST_NON_PROJECT_FIELD_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "bool", "char", "str", "String", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16",
        "u32", "u64", "u128", "usize", "f32", "f64", "Self", "self",
    ]
    .into_iter()
    .collect()
});
/// Per-line comment stripper for rust decl scans — `//.*$` and `/\*.*?\*/`.
pub(super) static RUST_LINE_COMMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"//.*$|/\*.*?\*/").unwrap());
/// RUST_STDLIB_ROOTS (import-resolver.ts): `use` roots that by definition
/// ship outside the repository.
pub(super) static RUST_STDLIB_ROOTS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["std", "core", "alloc", "proc_macro"].into_iter().collect());
/// collectRustUseBindings' `use` statement matcher —
/// `(^|\n)\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);`.
pub(super) static RUST_USE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\n)\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);").unwrap()
});
/// `use` alias tail — `^(.*?)\s+as\s+([A-Za-z_][0-9A-Za-z_]*)$`.
pub(super) static RUST_USE_ALIAS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*?)\s+as\s+([A-Za-z_][0-9A-Za-z_]*)$").unwrap());

/// rustFieldTypeName (name-matcher.ts): reduce a field's declared type text
/// to the simple name a method call auto-derefs to. Unwraps only the layers
/// Rust's method-call auto-deref looks through (`&`, `Box`, `Rc`, `Arc`);
/// `Option`/`Vec`/etc. keep their own name and resolve to nothing. Generic
/// params, primitives, tuples, raw pointers, fn types → None.
pub(super) fn rust_field_type_name(raw: &str) -> Option<String> {
    let mut t = raw.trim().to_string();
    loop {
        let before = t.clone();
        static REF_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^&\s*(?:'[0-9A-Za-z_]+\s+)?(?:mut\s+)?").unwrap());
        static PTR_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^(?:Box|Rc|Arc)\s*<\s*").unwrap());
        static DYN_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^(?:dyn|impl)\s+").unwrap());
        t = thread_regex(&REF_RE).replace(&t, "").into_owned();
        t = thread_regex(&PTR_RE).replace(&t, "").into_owned();
        t = thread_regex(&DYN_RE).replace(&t, "").into_owned();
        if t == before {
            break;
        }
    }
    // Drop generic args, closing `>`s of unwrapped pointers, and trait-object
    // bounds (`dyn Source + Send`); keep the last path segment.
    static TRIM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[<>+].*$").unwrap());
    let t = thread_regex(&TRIM_RE).replace(&t, "").trim().to_string();
    let seg = t.split("::").filter(|s| !s.is_empty()).last()?;
    static IDENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z_][0-9A-Za-z_]*$").unwrap());
    static GENERIC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z]$").unwrap());
    if !thread_regex(&IDENT_RE).is_match(seg)
        || RUST_NON_PROJECT_FIELD_TYPES.contains(seg)
        || thread_regex(&GENERIC_RE).is_match(seg)
    {
        return None;
    }
    Some(seg.to_string())
}

/// collectRustUseBindings (import-resolver.ts): `use` statements →
/// local-name → path map. `a::{b::{C, D}, E}` flattens one `{...}` level at
/// a time; `x as y` aliases; globs (`*`) are skipped.
pub(super) fn collect_rust_use_bindings(content: &str) -> std::collections::HashMap<String, String> {
    pub(super) fn expand(spec: &str) -> Vec<String> {
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
    for m in thread_regex(&RUST_USE_RE).captures_iter(content) {
        let spec = thread_regex(&WS_RE).replace_all(&m[1], " ");
        for flat in expand(&spec) {
            let alias = thread_regex(&RUST_USE_ALIAS_RE).captures(&flat);
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
pub(super) fn framework_claims_reference(framework: &str, name: &str) -> bool {
    match framework {
        "cics" => name.starts_with("cics-transid:"),
        "django" => name == "_iterable_class" || name.ends_with(".urls"),
        "drupal" => {
            name.starts_with("hook_") || name.contains('\\') || thread_regex(&DRUPAL_CLAIM_RE).is_match(name)
        }
        "expo-router" => thread_regex(&EXPO_NAV_RE).is_match(name),
        "laravel" => thread_regex(&LARAVEL_CLAIM_RE).is_match(name),
        "nextjs" => thread_regex(&NEXT_NAV_RE).is_match(name),
        "play" => thread_regex(&PLAY_CLAIM_RE).is_match(name),
        // react-native-bridge's claimsReference returns false — JS-visible
        // method names reach the resolver through the name-exists arm.
        "react-native-bridge" => false,
        "react-router" => thread_regex(&RR_NAV_RE).is_match(name),
        "rails" => thread_regex(&RAILS_CLAIM_RE).is_match(name),
        "spring" => name.ends_with(":prefix"),
        "sveltekit-router" => matches!(name, "goto" | "redirect"),
        "swift-objc-bridge" => name.contains(':'),
        "tanstack-router" => thread_regex(&TANSTACK_NAV_RE).is_match(name),
        "terraform" => thread_regex(&TERRA_CLAIM_RE).is_match(name),
        "vue-router" => thread_regex(&VUE_NAV_RE).is_match(name),
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
pub(super) fn language_family(lang: &str) -> Option<&'static str> {
    match lang {
        "java" | "kotlin" | "scala" => Some("jvm"),
        "swift" | "objc" => Some("apple"),
        "typescript" | "tsx" | "javascript" | "jsx" | "arkts" => Some("web"),
        "c" | "cpp" => Some("c"),
        "csharp" | "razor" => Some("dotnet"),
        _ => None,
    }
}

pub(super) fn same_language_family(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    matches!(language_family(a), Some(fa) if Some(fa) == language_family(b))
}

pub(super) fn is_known_language_family(lang: &str) -> bool {
    language_family(lang).is_some()
}

pub(super) fn crosses_known_family(a: &str, b: &str) -> bool {
    is_known_language_family(a) && is_known_language_family(b) && !same_language_family(a, b)
}

/// ESM_FAMILY (name-matcher.ts).
pub(super) fn is_esm_family(lang: &str) -> bool {
    matches!(lang, "typescript" | "tsx" | "javascript" | "jsx" | "arkts")
}

/// ESM_IMPORT_LANGUAGES (import-resolver.ts): imports are ES specifiers.
pub(super) fn is_esm_import_language(lang: &str) -> bool {
    matches!(
        lang,
        "typescript" | "tsx" | "javascript" | "jsx" | "arkts" | "svelte" | "vue" | "astro"
    )
}

/// isBindingReceiverCall (name-matcher.ts): a `calls` ref in a binding-
/// carrying language shaped `receiver.method`, excluding `()`-chains and
/// the self/this/super/cls receiver roots.
pub(super) fn is_binding_receiver_call(r: &ResolveRefIn) -> bool {
    r.reference_kind == "calls"
        && (is_esm_family(&r.language)
            || matches!(
                r.language.as_str(),
                "python" | "go" | "java" | "kotlin" | "php" | "c" | "cpp"
            ))
        && thread_regex(&BOUND_RECEIVER_RE).is_match(&r.reference_name)
        && !r.reference_name.contains("()")
        && !thread_regex(&BOUND_ROOT_RE).is_match(&r.reference_name)
}

/// isUnresolvedJsMemberCall (name-matcher.ts): an untyped 2+-level member
/// chain in a JS-family calls ref — terminal null, never name-matched.
pub(super) fn is_unresolved_js_member_call(r: &ResolveRefIn) -> bool {
    r.reference_kind == "calls"
        && matches!(
            r.language.as_str(),
            "typescript" | "tsx" | "javascript" | "jsx"
        )
        && !thread_regex(&JS_MEMBER_ROOT_RE).is_match(&r.reference_name)
        && thread_regex(&JS_MEMBER_RE).is_match(&r.reference_name)
}

/// preferCallSiteFile (name-matcher.ts): same-file candidates first,
/// preserving order; a no-op under <2 candidates or no same-file member.
pub(super) fn prefer_call_site_file(nodes: Vec<Arc<KNode>>, call_site_file: &str) -> Vec<Arc<KNode>> {
    if nodes.len() < 2 || !nodes.iter().any(|n| n.file_path == call_site_file) {
        return nodes;
    }
    let (mut same, other): (Vec<Arc<KNode>>, Vec<Arc<KNode>>) = nodes
        .into_iter()
        .partition(|n| n.file_path == call_site_file);
    same.extend(other);
    same
}

/// STATIC_MEMBER_CONTAINERS (import-resolver.ts).
pub(super) fn is_static_member_container(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "struct" | "union" | "interface" | "enum" | "trait" | "protocol"
    )
}

/// OBJECT_LITERAL_LANGUAGES (name-matcher.ts): object literals declare
/// callable members.
pub(super) fn is_object_literal_language(lang: &str) -> bool {
    matches!(lang, "typescript" | "tsx" | "javascript" | "jsx" | "arkts")
}

/// splitCamelCase — receiver/class word split for matchMethodCall's
/// name-similarity scoring. `permissionEngine` → ["permission","Engine"];
/// `HTTPServer` → ["HTTP","Server"]; words of length ≤ 1 are dropped.
/// Single pass: a break goes before a capital that follows a lowercase, or
/// that starts a Capital+lowercase word inside a capital run — the same
/// boundaries the two sequential JS replaces produce.
pub(super) fn split_camel_case(s: &str) -> Vec<String> {
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
pub(super) fn strip_line_comments(line: &str) -> String {
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

/// SUPERTYPE_TARGET_KINDS (resolution/types.ts).
pub(super) fn is_supertype_target_kind(kind: &str) -> bool {
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
pub(super) fn is_inheritance_ref(kind: &str) -> bool {
    kind == "extends" || kind == "implements"
}

/// isImportableKind (resolution/types.ts).
pub(super) fn is_importable_kind(kind: &str) -> bool {
    !matches!(kind, "property" | "field" | "method" | "enum_member" | "parameter")
}

/// PRIVATE_IS_FILE_LOCAL (name-matcher.ts).
pub(super) fn private_is_file_local(lang: &str) -> bool {
    matches!(lang, "kotlin" | "java" | "csharp" | "swift" | "scala" | "dart" | "php")
}

/// NO_NESTED_FUNCTIONS (name-matcher.ts).
pub(super) fn no_nested_functions(lang: &str) -> bool {
    lang == "c" || lang == "cpp"
}

/// JS_FAMILY (name-matcher.ts): the bare-call method guard applies to these.
pub(super) fn is_js_family(lang: &str) -> bool {
    matches!(lang, "typescript" | "tsx" | "javascript" | "jsx")
}

/// BARE_CALL_TARGET_KINDS (name-matcher.ts).
pub(super) fn is_bare_call_target_kind(kind: &str) -> bool {
    matches!(kind, "function" | "class" | "component" | "constant" | "variable")
}

/// CPP_ADL_RANGE_NAMES (name-matcher.ts).
pub(super) fn is_cpp_adl_name(name: &str) -> bool {
    matches!(name, "begin" | "end" | "rbegin" | "rend" | "cbegin" | "cend")
}

/// DEFAULT_BINDING_KINDS (import-resolver.ts).
pub(super) fn is_default_binding_kind(kind: &str) -> bool {
    matches!(kind, "function" | "class" | "component" | "constant" | "variable")
}

/// ALIAS_BINDING_KINDS (alias-binding.ts).
pub(super) fn is_alias_binding_kind(kind: &str) -> bool {
    matches!(kind, "constant" | "variable" | "property")
}

/// CALLABLE_KINDS (alias-binding.ts).
pub(super) fn is_callable_kind(kind: &str) -> bool {
    matches!(kind, "function" | "method" | "class" | "component")
}

/// EXTENSION_RESOLUTION (import-resolver.ts).
pub(super) fn extension_resolution(language: &str) -> &'static [&'static str] {
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
pub(super) fn emitted_to_source(rel: &str, language: &str) -> Option<&'static [&'static str]> {
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
pub(super) const ESM_BUILTIN_MODULES: &[&str] = &[
    "fs", "path", "os", "crypto", "http", "https", "url", "util", "events", "stream",
    "child_process", "buffer",
];

/// The stdlib head check isExternalImport applies to Python imports.
pub(super) const PYTHON_STDLIB_HEADS: &[&str] = &[
    "os", "sys", "json", "re", "math", "datetime", "collections", "typing", "pathlib", "logging",
];

/// The hardcoded alias fallback map (import-resolver.ts resolveAliasedImport
/// step 2): prefix → replacement, tried in declaration order.
pub(super) const FALLBACK_ALIASES: &[(&str, &str)] = &[
    ("@/", "src/"),
    ("~/", "src/"),
    ("@src/", "src/"),
    ("src/", "src/"),
    ("@app/", "app/"),
    ("app/", "app/"),
];

/// MARKDOWN_PATH_REF (resolution/index.ts isBuiltInOrExternal).
pub(super) static MARKDOWN_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?i:md|markdown)(?:$|[#?]|::)").unwrap());

/// C_SOURCE_EXT (name-matcher.ts).
pub(super) static C_SOURCE_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?i:c|cc|cpp|cxx|c\+\+|m|mm)$").unwrap());

/// BARE_ALIAS_RE (alias-binding.ts): `= foo` / `= foo as T` / `= foo;`.
pub(super) static BARE_ALIAS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^=\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:as\s+[A-Za-z0-9_.<>\[\]]+\s*)?;?$").unwrap()
});

pub(super) static IMPL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(pub(\([^)]*\))?\s+)?(unsafe\s+)?impl(?-u:\b)").unwrap()
});
pub(super) static IMPL_FOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\sfor\s").unwrap());
pub(super) static ITEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(pub(\([^)]*\))?\s+)?(fn|struct|enum|mod|trait|const|static|type)(?-u:\b)").unwrap()
});
pub(super) static JS_CALL_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.A-Za-z0-9_$\]\)]\s*$").unwrap());
pub(super) static JS_CALL_KEYWORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)(?:return|await|yield|typeof|void|new|else|case|throw|in|of|instanceof)\s*$")
        .unwrap()
});
pub(super) static CPP_THIS_DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^A-Za-z0-9_])this\.$").unwrap());
pub(super) static CPP_THIS_ARROW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^A-Za-z0-9_])this->$").unwrap());
pub(super) static AFTER_NAME_PAREN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\)").unwrap());

/// NON_TYPE_RECEIVER_TOKENS (name-matcher.ts) — loose captures that are never
/// a user-defined type.
pub(super) static NON_TYPE_RECEIVER_TOKENS: LazyLock<HashSet<&'static str>> =
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
pub(super) static GO_BUILTIN_FIELD_TYPES: LazyLock<HashSet<&'static str>> =
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
pub(super) static CPP_NON_TYPE_TOKENS: LazyLock<HashSet<&'static str>> =
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
// Built-in tables (index.ts isBuiltInOrExternal) — verbatim ports.
// ---------------------------------------------------------------------------

pub(super) static JS_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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
pub(super) static TS_PRIMITIVE_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "string", "number", "boolean", "bigint", "symbol", "void", "undefined", "null",
        "never", "unknown", "any", "object",
    ]
    .into_iter()
    .collect()
});

pub(super) static REACT_HOOKS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "useState", "useEffect", "useContext", "useReducer", "useCallback", "useMemo", "useRef",
        "useLayoutEffect", "useImperativeHandle", "useDebugValue",
    ]
    .into_iter()
    .collect()
});

pub(super) static PYTHON_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "print", "len", "range", "str", "int", "float", "list", "dict", "set", "tuple", "open",
        "input", "type", "isinstance", "hasattr", "getattr", "setattr", "super", "self", "cls",
        "None", "True", "False",
    ]
    .into_iter()
    .collect()
});

pub(super) static PYTHON_BUILT_IN_METHODS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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
pub(super) static GO_STDLIB_PACKAGES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

pub(super) static GO_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

pub(super) static C_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

pub(super) static CPP_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "cout", "cin", "cerr", "clog", "endl", "flush", "ws", "std", "nullptr", "true", "false",
        "this", "sizeof", "alignof", "typeid", "static_cast", "dynamic_cast", "reinterpret_cast",
        "const_cast", "make_unique", "make_shared", "make_pair", "move", "forward", "swap",
    ]
    .into_iter()
    .collect()
});

/// C_CPP_STDLIB_HEADERS (import-resolver.ts isExternalImport).
pub(super) static C_CPP_STDLIB_HEADERS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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
pub(super) struct ExportWant {
    pub(super) is_default: bool,
    pub(super) is_namespace: bool,
    pub(super) exported_name: String,
    pub(super) member_name: Option<String>,
}
