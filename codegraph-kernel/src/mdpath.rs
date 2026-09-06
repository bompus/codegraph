//! Markdown path references found inside code string literals — the code→doc
//! half of the markdown tier. Mirrors `isMarkdownPathStringNode`,
//! `extractMarkdownPathCandidates`, `decodePath` and
//! `normalizeMarkdownPathReference` in src/extraction/tree-sitter.ts; the
//! walker-side emission mirrors `extractMarkdownPathReferencesFromStringNode`.
//!
//! `open("docs/guide.md#install")` becomes a `references` edge onto
//! `docs/guide.md#install`. The doc side of the tier is unaffected by kernel
//! routing (markdown is not a routed language), but these edges are planted by
//! the CODE extractor, so without this module every routed file silently stops
//! pointing at the docs it names.

use regex::Regex;
use std::sync::OnceLock;

/// MARKDOWN_PATH_STRING_NODE_TYPES (tree-sitter.ts).
pub fn is_markdown_path_string_node(kind: &str) -> bool {
    matches!(
        kind,
        "string"
            | "string_literal"
            | "template_string"
            | "raw_string_literal"
            | "interpreted_string_literal"
            | "interpolated_string_expression"
    )
}

fn candidate_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)((?:\.{1,2}[\\/]+|[A-Za-z0-9_.@-]+[\\/]+|[\\/]+)?(?:[A-Za-z0-9_.@-]+[\\/]+)*[A-Za-z0-9_.@-]+\.(?:md|mdx|markdown)(?:\?[^'"`\s)>,;]*)?(?:#[^'"`\s)>,;]*)?)"#,
        )
        .expect("markdown path candidate regex")
    })
}

/// extractMarkdownPathCandidates: every `.md`-suffixed path in the literal's
/// text, each with its offset in UTF-16 code units (what the TS side adds to
/// the string node's column). A match preceded by `://` is a URL, not a repo
/// path, and is skipped — the TS code tests the 16 characters before the match
/// rather than using a lookbehind, so this does the same.
pub fn candidates(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for m in candidate_re().find_iter(text) {
        let start = m.start();
        let prefix_start = text[..start]
            .char_indices()
            .rev()
            .take(16)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(start);
        if text[prefix_start..start].ends_with("://") {
            continue;
        }
        let col = text[..start].chars().map(char::len_utf16).sum::<usize>() as u32;
        out.push((m.as_str().to_string(), col));
    }
    out
}

/// decodePath: decodeURIComponent, falling back to the raw value when the
/// escape sequence is malformed (the TS side catches the URIError).
fn decode_path(value: &str) -> String {
    if !value.contains('%') {
        return value.to_string();
    }
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return value.to_string();
            }
            let hex = match std::str::from_utf8(&bytes[i + 1..i + 3]) {
                Ok(h) => h,
                Err(_) => return value.to_string(),
            };
            match u8::from_str_radix(hex, 16) {
                Ok(b) => out.push(b),
                Err(_) => return value.to_string(),
            }
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

/// path.posix.dirname.
fn posix_dirname(p: &str) -> String {
    match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
        None => ".".to_string(),
    }
}

/// path.posix.normalize, for the relative and rooted forms this sees.
fn posix_normalize(p: &str) -> String {
    let is_abs = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if out.last().is_some_and(|l| *l != "..") {
                out.pop();
                continue;
            }
            if is_abs {
                continue;
            }
            out.push("..");
            continue;
        }
        out.push(seg);
    }
    let joined = out.join("/");
    if is_abs {
        return format!("/{joined}");
    }
    if joined.is_empty() {
        return ".".to_string();
    }
    joined
}

/// normalizeMarkdownPathReference: the repo-relative `path.md#anchor` a
/// candidate denotes, or None when it is a URL, not markdown, or escapes the
/// repo root.
pub fn normalize(reference_name: &str, file_path: &str) -> Option<String> {
    let trimmed = reference_name.trim().replace('\\', "/");
    if trimmed.is_empty() || scheme_re().is_match(&trimmed) {
        return None;
    }

    let (path_with_query, anchor) = match trimmed.find('#') {
        Some(i) => (&trimmed[..i], &trimmed[i..]),
        None => (trimmed.as_str(), ""),
    };
    let raw_path = match path_with_query.find('?') {
        Some(i) => &path_with_query[..i],
        None => path_with_query,
    };
    let clean_path = decode_path(raw_path);

    let lower = clean_path.to_ascii_lowercase();
    if !(lower.ends_with(".md") || lower.ends_with(".mdx") || lower.ends_with(".markdown")) {
        return None;
    }

    let normalized_file = file_path.replace('\\', "/");
    let base_dir = posix_dirname(&normalized_file);
    let normalized = if let Some(rooted) = clean_path.strip_prefix('/') {
        posix_normalize(rooted.trim_start_matches('/'))
    } else if clean_path.starts_with("./") || clean_path.starts_with("../") {
        let base = if base_dir == "." { "" } else { base_dir.as_str() };
        let joined =
            if base.is_empty() { clean_path.clone() } else { format!("{base}/{clean_path}") };
        posix_normalize(&joined)
    } else {
        posix_normalize(&clean_path)
    };

    if normalized.is_empty()
        || normalized == "."
        || normalized == ".."
        || normalized.starts_with("../")
    {
        return None;
    }
    Some(format!("{normalized}{anchor}"))
}

fn scheme_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*://").expect("url scheme regex"))
}
