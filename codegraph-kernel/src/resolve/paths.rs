//! Path helpers — DB paths are '/'-joined and project-relative; project_root is absolute.

use super::*;

// ---------------------------------------------------------------------------
// Path helpers — DB paths are always '/'-joined relative; project_root is
// absolute. On the supported platforms path == path.posix.
// ---------------------------------------------------------------------------

pub(super) fn pos_dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[..i],
        None => "",
    }
}

pub(super) fn pos_basename(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

/// path.posix.normalize semantics for a joined path: collapse `.`, `..`,
/// duplicate slashes; a relative input keeps its leading `..` segments.
pub(super) fn pos_normalize(p: &str) -> String {
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
pub(super) fn rust_self_module_dir(from_file: &str) -> String {
    let base = pos_basename(from_file);
    let dir = pos_dirname(from_file);
    if base == "mod.rs" || base == "lib.rs" || base == "main.rs" {
        return dir.to_string();
    }
    pos_join(dir, base.strip_suffix(".rs").unwrap_or(base))
}

/// path.posix.join for a project-relative directory: the project root is
/// `""` here (where TS has the absolute root), so an empty `dir` joins to
/// `name` alone rather than to an absolute `/name`.
pub(super) fn pos_join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        pos_normalize(name)
    } else {
        pos_normalize(&format!("{dir}/{name}"))
    }
}

/// path.resolve(dir, p): join + normalize; absolute `p` wins.
pub(super) fn pos_resolve(dir: &str, p: &str) -> String {
    if p.starts_with('/') {
        pos_normalize(p)
    } else {
        pos_normalize(&format!("{}/{}", dir.trim_end_matches('/'), p))
    }
}

/// path.relative(root, abs): `abs` must be inside `root` in every call site
/// that reaches here; return the forward-slash suffix, "" when equal.
pub(super) fn pos_relative(root: &str, abs: &str) -> String {
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

pub(super) fn is_within_dir(root_abs: &str, target_abs: &str) -> bool {
    target_abs == root_abs || target_abs.starts_with(&format!("{}/", root_abs.trim_end_matches('/')))
}

/// lexicalPathWithinRoot (sync/scanners/files.ts).
pub(super) fn lexical_path_within_root(root_abs: &str, rel: &str) -> bool {
    let resolved = pos_resolve(root_abs, rel);
    is_within_dir(root_abs, &resolved)
}

/// `receiver.charAt(0).toUpperCase() + receiver.slice(1)` — JS full case
/// mapping on the first char (to_uppercase may widen, e.g. ß→SS, as does JS).
pub(super) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// splitAnchor (name-matcher.ts): `path#anchor` → (path, anchor?).
pub(super) fn split_anchor(name: &str) -> (&str, Option<&str>) {
    match name.find('#') {
        Some(i) => (&name[..i], Some(&name[i + 1..])),
        None => (name, None),
    }
}

/// splitFileSymbol (name-matcher.ts): `path::symbol` → (path, symbol?).
pub(super) fn split_file_symbol(name: &str) -> (&str, Option<&str>) {
    match name.find("::") {
        Some(i) => (&name[..i], Some(&name[i + 2..])),
        None => (name, None),
    }
}

/// decodeURIComponent — %XX sequences → UTF-8 bytes; `+` stays literal.
/// JS throws on malformed input and the caller keeps the original, so a bad
/// escape or non-UTF-8 payload returns the input verbatim.
pub(super) fn uri_component_decode(s: &str) -> String {
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
pub(super) fn normalize_markdown_anchor(anchor: &str) -> String {
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
pub(super) fn pick_closest_file_node(candidates: &[Arc<KNode>], r: &ResolveRefIn) -> Arc<KNode> {
    let ref_dir = pos_dirname(&r.file_path);
    let same_dir: Vec<Arc<KNode>> = candidates
        .iter()
        .filter(|c| pos_dirname(&c.file_path) == ref_dir)
        .cloned()
        .collect();
    let pool: &[Arc<KNode>] = if same_dir.is_empty() { candidates } else { &same_dir };
    let mut best = pool[0].clone();
    let mut best_score = i64::MIN;
    for c in pool {
        let score = compute_path_proximity(&r.file_path, &c.file_path)
            + if same_language_family(&c.language, &r.language) { 5 } else { 0 };
        if score > best_score {
            best_score = score;
            best = c.clone();
        }
    }
    best
}

/// How many leading directory segments `dirs` shares with `file`'s directory
/// (every '/'-segment of `file` but the last) — the proximity measure the
/// name matchers use to prefer a nearby candidate.
pub(super) fn shared_dir_prefix<S: AsRef<str>>(dirs: &[S], file: &str) -> usize {
    let mut other: Vec<&str> = file.split('/').collect();
    other.pop();
    dirs.iter().zip(other).take_while(|(a, b)| a.as_ref() == *b).count()
}
