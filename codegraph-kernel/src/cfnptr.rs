//! Native port of the cFnPtr synthesizer's EXTRACTION SWEEP (task #5 step 2,
//! plan §7a.9): raw file text in → collected per-file facts out. The linking
//! stages, gates, and every registration/dispatch decision stay TS-side; this
//! module only reproduces, bug-for-bug, what the JS sweep computes per file in
//! `src/resolution/c-fnptr-synthesizer.ts`:
//!
//!   • `stripCommentsForRegex(text, 'c')` — the C-style comment/string state
//!     machine (comments blanked to spaces, string interiors skipped, backtick
//!     treated as a multi-line string delimiter — quirks and all);
//!   • the typedef scans (fn-pointer + fn-type forms);
//!   • struct-node field declarations (structural parse; classification stays
//!     TS-side where the complete typedef sets live);
//!   • the survival-filter scans (inline structs, initializers, bare arrays,
//!     alias-shaped object macros, field-assign pairs, dispatch fields, array
//!     dispatch names);
//!   • the raw-text `#include "..."` capture (path resolution stays TS-side —
//!     it needs the filesystem).
//!
//! Parity discipline: the JS side runs these as JavaScript REGEXES, so every
//! scanner here is a hand-rolled byte machine replicating THAT engine's
//! semantics, not idiomatic Rust regex:
//!   • JS `\w`/`\b` are ASCII (non-ASCII chars are non-word) — byte checks
//!     against `[A-Za-z0-9_]` reproduce them exactly, because UTF-8
//!     continuation bytes are non-ASCII and therefore non-word on both sides.
//!   • JS `\s` is the UNICODE whitespace class (NBSP, U+2000-200A, U+FEFF, …)
//!     — `jsws_len` decodes exactly that set from UTF-8.
//!   • Backtracking is reproduced where it is observable (INIT/ARRAY modifier
//!     and `struct` keyword ambiguity, DISPATCH's greedy segment loop,
//!     optional groups) and elided only where analysis shows no input can
//!     distinguish greedy from backtracked (documented per scanner).
//!   • `lastIndex` advancement (resume after each match, +1 on failure) is
//!     reproduced so overlapping-match selection is identical.
//!
//! The stripper blanks per UTF-16 code unit (see `strip_c`), so its output
//! equals the JS stripper's output EXACTLY as a string — every scanner here
//! runs over the identical character stream the JS regexes see, and the strip
//! differential oracle test pins that equality directly. The record-level
//! differential suite (JS sweep vs this sweep over fixtures and whole repos)
//! then pins the scanners themselves.

/// One struct node's extent, as the TS side reads it from the graph.
pub struct StructExtent {
    pub id: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// A structurally-parsed struct field — mirror of the TS `RawFieldDecl`
/// (`name: null` is represented as an empty string; the TS side treats them
/// identically everywhere).
pub struct RawField {
    pub name: String,
    pub index: u32,
    pub ptr: bool,
    pub ty: String,
}

pub struct StructFields {
    pub id: String,
    /// False when the body never parsed (no `{`, unbalanced braces, or a
    /// falsy start line) — the TS side then records nothing for this node,
    /// exactly like the JS sweep.
    pub parsed: bool,
    pub fields: Vec<RawField>,
}

/// Everything the sweep collects for one file.
pub struct FileFacts {
    pub fn_ptr_typedefs: Vec<String>,
    pub fn_type_typedefs: Vec<String>,
    pub structs: Vec<StructFields>,
    pub inline_ptr: bool,
    pub inline_types: Vec<String>,
    pub inline_tags: Vec<String>,
    pub init_tokens: Vec<String>,
    /// `*`-prefixed when the declaration carried the pointer star.
    pub array_elems: Vec<String>,
    pub alias_names: Vec<String>,
    /// `lfield\0rfield`, distinct.
    pub d_pairs: Vec<String>,
    pub dispatch_fields: Vec<String>,
    pub array_dispatch_names: Vec<String>,
    /// Raw `#include "…"` captures, in source order, NOT deduplicated —
    /// extension filtering and path resolution happen TS-side.
    pub includes: Vec<String>,
}

/// Mirror of the TS `C_TYPE_KEYWORDS` set — keep in exact sync.
const C_TYPE_KEYWORDS: [&[u8]; 17] = [
    b"void", b"int", b"char", b"short", b"long", b"unsigned", b"signed", b"float", b"double",
    b"const", b"struct", b"union", b"enum", b"static", b"volatile", b"register", b"inline",
];

fn is_type_keyword(w: &[u8]) -> bool {
    C_TYPE_KEYWORDS.iter().any(|k| *k == w)
}

const MODIFIERS: [&[u8]; 5] = [b"static", b"const", b"extern", b"register", b"volatile"];

#[inline]
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[inline]
fn is_word_at(s: &[u8], i: usize) -> bool {
    i < s.len() && is_word(s[i])
}

/// Byte length of the JS `\s` character starting at `i`, or 0 when `s[i]`
/// doesn't start one. JS \s = [\t\n\v\f\r    -
///    　﻿].
#[inline]
fn jsws_len(s: &[u8], i: usize) -> usize {
    let Some(&b0) = s.get(i) else { return 0 };
    match b0 {
        0x09..=0x0D | 0x20 => 1,
        0xC2 if s.get(i + 1) == Some(&0xA0) => 2, // U+00A0
        0xE1 if s.get(i + 1) == Some(&0x9A) && s.get(i + 2) == Some(&0x80) => 3, // U+1680
        0xE2 => match (s.get(i + 1), s.get(i + 2)) {
            (Some(&0x80), Some(&b2)) if (0x80..=0x8A).contains(&b2) => 3, // U+2000-200A
            (Some(&0x80), Some(&0xA8)) => 3,                              // U+2028
            (Some(&0x80), Some(&0xA9)) => 3,                              // U+2029
            (Some(&0x80), Some(&0xAF)) => 3,                              // U+202F
            (Some(&0x81), Some(&0x9F)) => 3,                              // U+205F
            _ => 0,
        },
        0xE3 if s.get(i + 1) == Some(&0x80) && s.get(i + 2) == Some(&0x80) => 3, // U+3000
        0xEF if s.get(i + 1) == Some(&0xBB) && s.get(i + 2) == Some(&0xBF) => 3, // U+FEFF
        _ => 0,
    }
}

/// Advance past `\s*`.
#[inline]
fn skip_jsws(s: &[u8], mut i: usize) -> usize {
    loop {
        let l = jsws_len(s, i);
        if l == 0 {
            return i;
        }
        i += l;
    }
}

/// End of the `\w+` run starting at `i` (caller checks `is_word_at(s, i)`).
#[inline]
fn word_end(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && is_word(s[i]) {
        i += 1;
    }
    i
}

/// JS `\b` before position `i` (position 0, or previous byte non-word).
#[inline]
fn boundary_before(s: &[u8], i: usize) -> bool {
    i == 0 || !is_word(s[i - 1])
}

fn find_bytes(s: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || s.len() < needle.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= s.len() {
        // memchr on the first byte keeps this fast on 20KB+ files.
        match s[i..s.len() - needle.len() + 1].iter().position(|&b| b == needle[0]) {
            None => return None,
            Some(off) => {
                i += off;
                if &s[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
        }
    }
    None
}

fn contains_bytes(s: &[u8], needle: &[u8]) -> bool {
    find_bytes(s, needle, 0).is_some()
}

/// `\bWORD\b` occurrence search from `from`.
fn find_word(s: &[u8], word: &[u8], mut from: usize) -> Option<usize> {
    loop {
        let t = find_bytes(s, word, from)?;
        if boundary_before(s, t) && !is_word_at(s, t + word.len()) {
            return Some(t);
        }
        from = t + 1;
    }
}

// ---------- stripCommentsForRegex(src, 'c') ----------

/// Port of `stripCStyle(src, /*allowSingleQuoteStrings*/ false)`:
/// `/* */` and `//` comments blanked to spaces (newlines preserved), `"` and
/// backtick string interiors skipped verbatim (backtick spans lines — the JS
/// helper treats it as a template literal even for C), `'` NOT special.
///
/// Blanking is per UTF-16 CODE UNIT (one space per BMP char, two per astral
/// char), so the result equals the JS stripper's output EXACTLY as a string —
/// the scanners downstream see the identical character stream the JS regexes
/// see, and the strip differential oracle pins byte equality directly.
/// (Comment boundaries — `/*`, `*/`, `//`, quotes, `\n` — are all ASCII, so
/// the state machine's byte positions always land on char boundaries.)
pub fn strip_c(src: &[u8]) -> Vec<u8> {
    let n = src.len();
    let mut out = Vec::with_capacity(n);
    let mut copied = 0usize; // src[..copied] already emitted
    let mut i = 0;
    {
        let mut blank_to = |out: &mut Vec<u8>, start: usize, end: usize| {
            out.extend_from_slice(&src[copied..start]);
            emit_blank(out, &src[start..end]);
            copied = end;
        };
        while i < n {
            let c = src[i];
            let c2 = if i + 1 < n { src[i + 1] } else { 0 };
            if c == b'/' && c2 == b'*' {
                let start = i;
                i += 2;
                while i < n && !(src[i] == b'*' && i + 1 < n && src[i + 1] == b'/') {
                    i += 1;
                }
                if i < n {
                    i += 2;
                }
                blank_to(&mut out, start, i.min(n));
                continue;
            }
            if c == b'/' && c2 == b'/' {
                let start = i;
                while i < n && src[i] != b'\n' {
                    i += 1;
                }
                blank_to(&mut out, start, i);
                continue;
            }
            if c == b'"' || c == b'`' {
                let quote = c;
                i += 1;
                while i < n && src[i] != quote {
                    if src[i] == b'\\' && i + 1 < n {
                        i += 2;
                        continue;
                    }
                    if quote != b'`' && src[i] == b'\n' {
                        break;
                    }
                    i += 1;
                }
                if i < n && src[i] == quote {
                    i += 1;
                }
                continue;
            }
            i += 1;
        }
    }
    out.extend_from_slice(&src[copied..]);
    out
}

/// One space per UTF-16 code unit (`\n` preserved): ASCII and 2-3-byte chars
/// are one unit, 4-byte (astral) chars are a surrogate pair — two units.
fn emit_blank(out: &mut Vec<u8>, region: &[u8]) {
    let mut i = 0;
    while i < region.len() {
        let b = region[i];
        if b == b'\n' {
            out.push(b'\n');
            i += 1;
            continue;
        }
        let len = if b < 0x80 {
            1
        } else if b < 0xC0 {
            1 // continuation byte at region start — invalid UTF-8; count singly
        } else if b < 0xE0 {
            2
        } else if b < 0xF0 {
            3
        } else {
            4
        };
        out.push(b' ');
        if len == 4 {
            out.push(b' ');
        }
        i += len.min(region.len() - i);
    }
}

// ---------- shared regex tails ----------

/// `\(\s*(?:\w+\s+)*\*\s*(\w+)\s*\)\s*\(` matched at `open` (which must hold
/// `(`). Returns (name_range, end_after_second_paren). The `(?:\w+\s+)*`
/// group is greedy without backtracking: giving back an iteration repositions
/// `\*` onto a word char, which can never match, so greedy ≡ backtracked.
fn fnptr_paren_tail(s: &[u8], open: usize) -> Option<((usize, usize), usize)> {
    let mut i = skip_jsws(s, open + 1);
    loop {
        if !is_word_at(s, i) {
            break;
        }
        let we = word_end(s, i);
        let wse = skip_jsws(s, we);
        if wse == we {
            break; // \w+ not followed by \s+ — the iteration fails, word not consumed
        }
        i = wse;
    }
    if s.get(i) != Some(&b'*') {
        return None;
    }
    i = skip_jsws(s, i + 1);
    if !is_word_at(s, i) {
        return None;
    }
    let name = (i, word_end(s, i));
    i = skip_jsws(s, name.1);
    if s.get(i) != Some(&b')') {
        return None;
    }
    i = skip_jsws(s, i + 1);
    if s.get(i) != Some(&b'(') {
        return None;
    }
    Some((name, i + 1))
}

/// `\s*\)?\s*\(` at `i` → position after the `(`. The optional `)` needs no
/// backtracking: retrying without a consumed `)` lands `\(` on that `)`.
fn close_call_tail(s: &[u8], i: usize) -> Option<usize> {
    let mut j = skip_jsws(s, i);
    if s.get(j) == Some(&b')') {
        j = skip_jsws(s, j + 1);
    }
    if s.get(j) == Some(&b'(') {
        return Some(j + 1);
    }
    None
}

// ---------- scanners ----------

/// FNPTR_TYPEDEF_RE: /\btypedef\b[^;{}]*?\(\s*(?:\w+\s+)*\*\s*(\w+)\s*\)\s*\(/g
fn scan_fnptr_typedefs(s: &[u8], out: &mut Vec<String>) {
    let mut last = 0;
    while let Some(t) = find_word(s, b"typedef", last) {
        let mut j = t + 7;
        let mut matched = None;
        // Lazy [^;{}]*?: try the paren tail at each `(` in order; the class
        // may also expand ACROSS a failed `(` (it admits parens).
        while j < s.len() {
            let ch = s[j];
            if ch == b';' || ch == b'{' || ch == b'}' {
                break;
            }
            if ch == b'(' {
                if let Some((name, end)) = fnptr_paren_tail(s, j) {
                    matched = Some((name, end));
                    break;
                }
            }
            j += 1;
        }
        match matched {
            Some(((ns, ne), end)) => {
                push_str(out, &s[ns..ne]);
                last = end;
            }
            None => last = t + 1,
        }
    }
}

/// FNTYPE_TYPEDEF_STMT_RE (/\btypedef\b([^;{}]*);/g) + the TS-side guts
/// checks: skip when guts contains `(*` or `( *`; else the FIRST
/// /\b(\w+)\s*\(/ capture, filtered through C_TYPE_KEYWORDS.
fn scan_fntype_typedefs(s: &[u8], out: &mut Vec<String>) {
    let mut last = 0;
    while let Some(t) = find_word(s, b"typedef", last) {
        let mut j = t + 7;
        while j < s.len() && s[j] != b';' && s[j] != b'{' && s[j] != b'}' {
            j += 1;
        }
        if j >= s.len() || s[j] != b';' {
            last = t + 1;
            continue;
        }
        let guts = &s[t + 7..j];
        if !contains_bytes(guts, b"(*") && !contains_bytes(guts, b"( *") {
            // first \b(\w+)\s*\( in guts
            let mut p = 0;
            while p < guts.len() {
                if is_word(guts[p]) && boundary_before(guts, p) {
                    let we = word_end(guts, p);
                    let k = skip_jsws(guts, we);
                    if guts.get(k) == Some(&b'(') {
                        let w = &guts[p..we];
                        if !is_type_keyword(w) {
                            push_str(out, w);
                        }
                        break;
                    }
                    p = we;
                } else {
                    p += 1;
                }
            }
        }
        last = j + 1;
    }
}

/// INLINE_STRUCT_RE (/\bstruct\s+(\w+)\s*\{/g), sweep flavor: NO cursor jump
/// (the filter needs a superset of the registration pass's jump-scan), each
/// valid candidate (balanced braces + the `^\s*(\w+)…` var check) contributes
/// its tag and a structural field summary.
struct InlineScan {
    ptr: bool,
    types: Vec<String>,
    tags: Vec<String>,
}

fn scan_inline_structs(s: &[u8]) -> InlineScan {
    let mut out = InlineScan { ptr: false, types: Vec::new(), tags: Vec::new() };
    let mut last = 0;
    loop {
        let next_struct = find_word(s, b"struct", last);
        let next_union = find_word(s, b"union", last);
        let Some((t, keyword_len)) = (match (next_struct, next_union) {
            (Some(st), Some(un)) if st < un => Some((st, 6)),
            (Some(_), Some(un)) => Some((un, 5)),
            (Some(st), None) => Some((st, 6)),
            (None, Some(un)) => Some((un, 5)),
            (None, None) => None,
        }) else {
            break;
        };
        let after_kw = t + keyword_len;
        let ws = skip_jsws(s, after_kw);
        if ws == after_kw || !is_word_at(s, ws) {
            last = t + 1;
            continue;
        }
        let te = word_end(s, ws);
        let open = skip_jsws(s, te);
        if s.get(open) != Some(&b'{') {
            last = t + 1;
            continue;
        }
        last = open + 1; // lastIndex = end of match (after `{`)
        let Some(close) = match_brace(s, open) else { continue };
        // vm: /^\s*(\w+)…/ on the text after `}` — only vm[1] matters here.
        let v = skip_jsws(s, close + 1);
        if !is_word_at(s, v) {
            continue;
        }
        push_str(&mut out.tags, &s[ws..te]);
        for f in parse_struct_fields_raw(&s[open + 1..close]) {
            if f.name.is_empty() {
                continue;
            }
            if f.ptr {
                out.ptr = true;
            } else if !f.ty.is_empty() {
                out.types.push(f.ty);
            }
        }
    }
    out
}

/// matchBrace: index of the `}` matching the `{` at `open`, or None.
fn match_brace(s: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut i = open;
    while i < s.len() {
        match s[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The `(?:(?:static|const|extern|register|volatile)\s+)*` modifier loop:
/// greedy positions after 0..=k iterations, for the k-descending backtrack the
/// INIT/ARRAY skeletons need. No two alternatives share a prefix, so at most
/// one literal can match at a position; an alternative that matches without
/// trailing `\s+` ends the loop (JS: iteration fails, no other alt can fire).
fn modifier_positions(s: &[u8], start: usize) -> Vec<usize> {
    let mut stack = vec![start];
    loop {
        let cur = *stack.last().unwrap();
        let mut advanced = None;
        for m in MODIFIERS {
            if s.len() >= cur + m.len() && &s[cur..cur + m.len()] == m {
                let e = cur + m.len();
                let w = skip_jsws(s, e);
                if w > e {
                    advanced = Some(w);
                }
                break; // exactly one alternative can literal-match here
            }
        }
        match advanced {
            Some(w) => stack.push(w),
            None => return stack,
        }
    }
}

/// `\[[^\]]*\]` at `i` (the INIT/ARRAY declarator form — the class admits
/// newlines): position after the FIRST `]`, or None.
fn bracket_span(s: &[u8], i: usize) -> Option<usize> {
    if s.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() && s[j] != b']' {
        j += 1;
    }
    if j < s.len() {
        Some(j + 1)
    } else {
        None
    }
}

/// Anchor-skeleton driver shared by INIT_RE and ARRAY_TABLE_RE: both match
/// `(?:^|[;{}])` then a body, and resume from the end of each match. `body`
/// returns (token, match_end) when the body matches at the position after the
/// anchor.
fn scan_anchored<F>(s: &[u8], mut body: F, out: &mut Vec<String>)
where
    F: FnMut(&[u8], usize) -> Option<(String, usize)>,
{
    let mut last = 0usize;
    // The `^` branch consumes nothing and only exists at position 0.
    if last == 0 {
        if let Some((tok, end)) = body(s, 0) {
            out.push(tok);
            last = end;
        }
    }
    let mut p = last;
    while p < s.len() {
        let ch = s[p];
        if ch == b';' || ch == b'{' || ch == b'}' {
            if let Some((tok, end)) = body(s, p + 1) {
                out.push(tok);
                p = end;
                continue;
            }
        }
        p += 1;
    }
}

/// INIT_RE body after the anchor:
/// `\s*(?:MOD\s+)*(?:struct\s+)?(\w+)\s+(\w+)\s*(\[[^\]]*\])?\s*=\s*\{`
/// Backtracks: modifier count (desc), `struct` with/without, bracket
/// with/without — exactly the observable dimensions of the JS engine.
fn init_body(s: &[u8], p: usize) -> Option<(String, usize)> {
    let i = skip_jsws(s, p);
    let mods = modifier_positions(s, i);
    for &pos in mods.iter().rev() {
        for keyword in [Some(b"struct".as_slice()), Some(b"union".as_slice()), None] {
            let q = if let Some(keyword) = keyword {
                if s.len() >= pos + keyword.len() && &s[pos..pos + keyword.len()] == keyword {
                    let e = pos + keyword.len();
                    let w = skip_jsws(s, e);
                    if w == e {
                        continue;
                    }
                    w
                } else {
                    continue;
                }
            } else {
                pos
            };
            if !is_word_at(s, q) {
                continue;
            }
            let te = word_end(s, q);
            let w = skip_jsws(s, te);
            if w == te {
                continue; // \s+ needs ≥1
            }
            if !is_word_at(s, w) {
                continue;
            }
            let ne = word_end(s, w);
            let r = skip_jsws(s, ne);
            for with_bracket in [true, false] {
                let r2 = if with_bracket {
                    match bracket_span(s, r) {
                        Some(e) => e,
                        None => continue,
                    }
                } else {
                    r
                };
                let r3 = skip_jsws(s, r2);
                if s.get(r3) != Some(&b'=') {
                    continue;
                }
                let r4 = skip_jsws(s, r3 + 1);
                if s.get(r4) != Some(&b'{') {
                    continue;
                }
                return Some((bytes_to_string(&s[q..te]), r4 + 1));
            }
        }
    }
    None
}

/// ARRAY_TABLE_RE body after the anchor:
/// `\s*(?:MOD\s+)*(\w+)\s+(\*\s*)?(\w+)\s*\[[^\]]*\]\s*=\s*\{`
/// Token is `*`-prefixed when the star declarator is present.
fn array_table_body(s: &[u8], p: usize) -> Option<(String, usize)> {
    let i = skip_jsws(s, p);
    let mods = modifier_positions(s, i);
    for &pos in mods.iter().rev() {
        if !is_word_at(s, pos) {
            continue;
        }
        let te = word_end(s, pos);
        let w = skip_jsws(s, te);
        if w == te {
            continue;
        }
        for with_star in [true, false] {
            let q = if with_star {
                if s.get(w) == Some(&b'*') {
                    skip_jsws(s, w + 1)
                } else {
                    continue;
                }
            } else {
                w
            };
            if !is_word_at(s, q) {
                continue;
            }
            let ne = word_end(s, q);
            let r = skip_jsws(s, ne);
            let Some(r2) = bracket_span(s, r) else { continue };
            let r3 = skip_jsws(s, r2);
            if s.get(r3) != Some(&b'=') {
                continue;
            }
            let r4 = skip_jsws(s, r3 + 1);
            if s.get(r4) != Some(&b'{') {
                continue;
            }
            let mut tok = String::new();
            if with_star {
                tok.push('*');
            }
            tok.push_str(&bytes_to_string(&s[pos..te]));
            return Some((tok, r4 + 1));
        }
    }
    None
}

/// OBJ_ALIAS_RE over the continuation-joined text:
/// /^[ \t]*#[ \t]*define[ \t]+(\w+)[ \t]+(?:struct[ \t]+)*[A-Za-z_]\w*[ \t\r]*$/gm
fn scan_alias_names(stripped: &[u8], out: &mut Vec<String>) {
    // joined = stripped.replace(/\\\r?\n/g, ' ')
    let mut joined = Vec::with_capacity(stripped.len());
    let mut i = 0;
    while i < stripped.len() {
        let b = stripped[i];
        if b == b'\\' {
            if stripped.get(i + 1) == Some(&b'\n') {
                joined.push(b' ');
                i += 2;
                continue;
            }
            if stripped.get(i + 1) == Some(&b'\r') && stripped.get(i + 2) == Some(&b'\n') {
                joined.push(b' ');
                i += 3;
                continue;
            }
        }
        joined.push(b);
        i += 1;
    }
    for line in joined.split(|&b| b == b'\n') {
        if let Some(name) = alias_line(line) {
            push_str(out, name);
        }
    }
}

#[inline]
fn skip_sp_tab(line: &[u8], mut i: usize) -> usize {
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
        i += 1;
    }
    i
}

fn alias_line(line: &[u8]) -> Option<&[u8]> {
    let mut i = skip_sp_tab(line, 0);
    if line.get(i) != Some(&b'#') {
        return None;
    }
    i = skip_sp_tab(line, i + 1);
    if line.len() < i + 6 || &line[i..i + 6] != b"define" {
        return None;
    }
    i += 6;
    let w = skip_sp_tab(line, i);
    if w == i || !is_word_at(line, w) {
        return None;
    }
    let name_end = word_end(line, w);
    let name = &line[w..name_end];
    let v0 = skip_sp_tab(line, name_end);
    if v0 == name_end {
        return None; // [ \t]+ before the value
    }
    // (?:(?:struct|union)[ \t]+)* greedy, k-descending on value failure.
    let mut stack = vec![v0];
    loop {
        let cur = *stack.last().unwrap();
        let keyword_len = if line.len() >= cur + 6 && &line[cur..cur + 6] == b"struct" {
            Some(6)
        } else if line.len() >= cur + 5 && &line[cur..cur + 5] == b"union" {
            Some(5)
        } else {
            None
        };
        if let Some(keyword_len) = keyword_len {
            let e = cur + keyword_len;
            let w2 = skip_sp_tab(line, e);
            if w2 > e {
                stack.push(w2);
                continue;
            }
        }
        break;
    }
    for &vp in stack.iter().rev() {
        let Some(&b0) = line.get(vp) else { continue };
        if !(b0.is_ascii_alphabetic() || b0 == b'_') {
            continue; // value must start [A-Za-z_]
        }
        let ve = word_end(line, vp);
        // [ \t\r]*$
        let mut t = ve;
        while t < line.len() && (line[t] == b' ' || line[t] == b'\t' || line[t] == b'\r') {
            t += 1;
        }
        if t == line.len() {
            return Some(name);
        }
    }
    None
}

/// FIELD_ASSIGN_RE: /(\w+)\s*(?:->|\.)\s*(\w+)\s*=\s*(\w+)\s*(?:->|\.)\s*(\w+)/g
/// Pairs collected as `lfield\0rfield`. Every byte position is a candidate
/// start (JS advances one unit on failure — suffix starts included); matches
/// resume at their end.
fn scan_field_assign(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while pos < s.len() {
        if !is_word(s[pos]) {
            pos += 1;
            continue;
        }
        match field_assign_at(s, pos) {
            Some((lf, rf, end)) => {
                let mut pair = bytes_to_string(&s[lf.0..lf.1]);
                pair.push('\0');
                pair.push_str(&bytes_to_string(&s[rf.0..rf.1]));
                out.push(pair);
                pos = end;
            }
            None => pos += 1,
        }
    }
}

#[inline]
fn arrow_at(s: &[u8], i: usize) -> Option<usize> {
    if s.get(i) == Some(&b'-') && s.get(i + 1) == Some(&b'>') {
        Some(i + 2)
    } else if s.get(i) == Some(&b'.') {
        Some(i + 1)
    } else {
        None
    }
}

type Range = (usize, usize);

fn field_assign_at(s: &[u8], p: usize) -> Option<(Range, Range, usize)> {
    let w1 = word_end(s, p);
    let a1 = arrow_at(s, skip_jsws(s, w1))?;
    let f1s = skip_jsws(s, a1);
    if !is_word_at(s, f1s) {
        return None;
    }
    let f1e = word_end(s, f1s);
    let eq = skip_jsws(s, f1e);
    if s.get(eq) != Some(&b'=') {
        return None;
    }
    let r1s = skip_jsws(s, eq + 1);
    if !is_word_at(s, r1s) {
        return None;
    }
    let r1e = word_end(s, r1s);
    let a2 = arrow_at(s, skip_jsws(s, r1e))?;
    let f2s = skip_jsws(s, a2);
    if !is_word_at(s, f2s) {
        return None;
    }
    let f2e = word_end(s, f2s);
    Some(((f1s, f1e), (f2s, f2e), f2e))
}

/// DISPATCH_RE: /((?:\w+(?:\s*\[[^\][]*\])?\s*(?:->|\.)\s*)+)(\w+)\s*\)?\s*\(/g
/// The `+` loop is consumed greedily, then the field tail is tried at each
/// segment count k descending — the JS engine's observable backtracking. The
/// per-segment optional subscript needs no cross-product: the with/without
/// parses diverge at the arrow and at most one can complete a segment.
fn scan_dispatch(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while pos < s.len() {
        if !is_word(s[pos]) {
            pos += 1;
            continue;
        }
        // Greedy segment loop.
        let mut seg_ends: Vec<usize> = Vec::new();
        let mut cur = pos;
        while is_word_at(s, cur) {
            let we = word_end(s, cur);
            let with_sub = subscript_span(s, skip_jsws(s, we)).and_then(|e| arrow_tail(s, e));
            let seg = with_sub.or_else(|| arrow_tail(s, we));
            match seg {
                Some(e) => {
                    seg_ends.push(e);
                    cur = e;
                }
                None => break,
            }
        }
        let mut matched = None;
        for k in (1..=seg_ends.len()).rev() {
            let fpos = seg_ends[k - 1];
            if !is_word_at(s, fpos) {
                continue;
            }
            let fe = word_end(s, fpos);
            if let Some(end) = close_call_tail(s, fe) {
                matched = Some(((fpos, fe), end));
                break;
            }
        }
        match matched {
            Some(((fs_, fe), end)) => {
                push_str(out, &s[fs_..fe]);
                pos = end;
            }
            None => pos += 1,
        }
    }
}

/// `\[[^\][]*\]` at `i` (the DISPATCH subscript form — no nested brackets):
/// position after `]`, or None.
fn subscript_span(s: &[u8], i: usize) -> Option<usize> {
    if s.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() && s[j] != b']' && s[j] != b'[' {
        j += 1;
    }
    if j < s.len() && s[j] == b']' {
        Some(j + 1)
    } else {
        None
    }
}

/// `\s*(?:->|\.)\s*` at `i` → position after.
#[inline]
fn arrow_tail(s: &[u8], i: usize) -> Option<usize> {
    let a = arrow_at(s, skip_jsws(s, i))?;
    Some(skip_jsws(s, a))
}

/// ARRAY_DISPATCH_RE: /(?:\(\s*\*\s*)?\b(\w+)\s*\[[^\][]*\]\s*\)?\s*\(/g
fn scan_array_dispatch(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while pos < s.len() {
        let b = s[pos];
        if b != b'(' && !(is_word(b) && boundary_before(s, pos)) {
            pos += 1;
            continue;
        }
        let name_start = if b == b'(' {
            let i = skip_jsws(s, pos + 1);
            if s.get(i) == Some(&b'*') {
                let j = skip_jsws(s, i + 1);
                // \b holds: the previous char is `*` or whitespace.
                if is_word_at(s, j) { Some(j) } else { None }
            } else {
                None
            }
        } else {
            Some(pos)
        };
        let matched = name_start.and_then(|ns| {
            let ne = word_end(s, ns);
            let sub = subscript_span(s, skip_jsws(s, ne))?;
            let end = close_call_tail(s, sub)?;
            Some(((ns, ne), end))
        });
        match matched {
            Some(((ns, ne), end)) => {
                push_str(out, &s[ns..ne]);
                pos = end;
            }
            None => pos += 1,
        }
    }
}

/// INCLUDE_RE over RAW text: /#[ \t]*include[ \t]+"([^"\n]+)"/g
fn scan_includes(raw: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while pos < raw.len() {
        let Some(h) = find_bytes(raw, b"#", pos) else { break };
        let mut i = skip_sp_tab(raw, h + 1);
        if raw.len() < i + 7 || &raw[i..i + 7] != b"include" {
            pos = h + 1;
            continue;
        }
        i += 7;
        let q = skip_sp_tab(raw, i);
        if q == i || raw.get(q) != Some(&b'"') {
            pos = h + 1;
            continue;
        }
        let mut j = q + 1;
        while j < raw.len() && raw[j] != b'"' && raw[j] != b'\n' {
            j += 1;
        }
        if j > q + 1 && j < raw.len() && raw[j] == b'"' {
            out.push(bytes_to_string(&raw[q + 1..j]));
            pos = j + 1;
        } else {
            pos = h + 1;
        }
    }
}

// ---------- struct field parsing ----------

/// splitTopLevel(body, sep): split on `sep` at brace/paren/bracket depth 0.
fn split_top_level(body: &[u8], sep: u8) -> Vec<Range> {
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut start = 0usize;
    for (i, &c) in body.iter().enumerate() {
        match c {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            _ if c == sep && depth == 0 => {
                out.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push((start, body.len()));
    out
}

/// JS String.prototype.trim over bytes (the JS set == our jsws set).
fn jsws_trim(s: &[u8], mut a: usize, mut b: usize) -> (usize, usize) {
    loop {
        let l = jsws_len(s, a);
        if l == 0 || a + l > b {
            break;
        }
        a += l;
    }
    // Trailing: walk from the front to find the last non-ws position (ws
    // lengths vary, so scan forward tracking the end of the last non-ws char).
    let mut i = a;
    let mut last_end = a;
    while i < b {
        let l = jsws_len(s, i);
        if l == 0 {
            i += 1;
            last_end = i;
        } else {
            i += l;
        }
    }
    b = last_end;
    (a, b)
}

/// /(\w+)\s+\**\s*(\w+)\s*$/ — leftmost match whose tail reaches the end.
/// Deterministic per start (greedy words/ws cannot backtrack usefully);
/// candidate starts advance one byte at a time like the JS engine.
fn first_typed(part: &[u8]) -> Option<(Range, Range)> {
    let n = part.len();
    let mut p = 0usize;
    while p < n {
        if !is_word(part[p]) {
            p += 1;
            continue;
        }
        let te = word_end(part, p);
        let w = skip_jsws(part, te);
        if w == te {
            p += 1;
            continue;
        }
        let mut q = w;
        while q < n && part[q] == b'*' {
            q += 1;
        }
        let q = skip_jsws(part, q);
        if is_word_at(part, q) {
            let ne = word_end(part, q);
            let t = skip_jsws(part, ne);
            if t == n {
                return Some(((p, te), (q, ne)));
            }
        }
        p += 1;
    }
    None
}

/// FNPTR_DECL_RE (first match): /\(\s*(?:\w+\s+)*\*\s*(\w+)\s*\)\s*\(/
fn fnptr_decl(part: &[u8]) -> Option<Range> {
    let mut i = 0usize;
    while i < part.len() {
        if part[i] == b'(' {
            if let Some((name, _)) = fnptr_paren_tail(part, i) {
                return Some(name);
            }
        }
        i += 1;
    }
    None
}

/// Port of `parseStructFieldsRaw` — structure only, classification TS-side.
pub fn parse_struct_fields_raw(inner: &[u8]) -> Vec<RawField> {
    let mut fields = Vec::new();
    let mut idx: u32 = 0;
    for (ds, de) in split_top_level(inner, b';') {
        let (ds, de) = jsws_trim(inner, ds, de);
        if ds >= de {
            continue;
        }
        let decl = &inner[ds..de];
        let parts = split_top_level(decl, b',');
        let ft = first_typed(&decl[parts[0].0..parts[0].1]);
        let shared_type: &[u8] = match &ft {
            Some(((ts, te), _)) => &decl[parts[0].0 + ts..parts[0].0 + te],
            None => b"",
        };
        for (pi, &(ps, pe)) in parts.iter().enumerate() {
            let (ps2, pe2) = jsws_trim(decl, ps, pe);
            let p = &decl[ps2..pe2];
            let mut name: &[u8] = b"";
            let mut ty: &[u8] = b"";
            let mut ptr = false;
            if let Some((ns, ne)) = fnptr_decl(p) {
                name = &p[ns..ne];
                ptr = true;
            } else if pi == 0 {
                if let Some((_, (ns, ne))) = &ft {
                    name = &decl[parts[0].0 + ns..parts[0].0 + ne];
                    ty = shared_type;
                }
            } else {
                // /^\**\s*(\w+)/
                let mut q = 0usize;
                while q < p.len() && p[q] == b'*' {
                    q += 1;
                }
                let q = skip_jsws(p, q);
                if is_word_at(p, q) {
                    name = &p[q..word_end(p, q)];
                    ty = shared_type;
                }
            }
            fields.push(RawField {
                name: bytes_to_string(name),
                index: idx,
                ptr,
                ty: bytes_to_string(ty),
            });
            idx += 1;
        }
    }
    fields
}

// ---------- per-file entry ----------

fn push_str(out: &mut Vec<String>, bytes: &[u8]) {
    out.push(bytes_to_string(bytes));
}

#[inline]
fn bytes_to_string(bytes: &[u8]) -> String {
    // All slice boundaries land on ASCII delimiters, so the content is valid
    // UTF-8 whenever the input string was; lossy keeps us total anyway.
    String::from_utf8_lossy(bytes).into_owned()
}

fn dedup_in_order(v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(v.len());
    for x in v {
        if seen.insert(x.clone()) {
            out.push(x);
        }
    }
    out
}

/// Line start offsets (byte offset of each line's first byte).
fn line_starts(s: &[u8]) -> Vec<usize> {
    let mut out = vec![0usize];
    for (i, &b) in s.iter().enumerate() {
        if b == b'\n' {
            out.push(i + 1);
        }
    }
    out
}

/// Run the full extraction sweep for one file. `raw` is the file text exactly
/// as the TS side read it; `structs` are the file's struct-node extents.
pub fn scan_file(raw: &str, structs: &[StructExtent]) -> FileFacts {
    let raw_b = raw.as_bytes();
    let stripped = strip_c(raw_b);
    let s: &[u8] = &stripped;

    let mut facts = FileFacts {
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
        dispatch_fields: Vec::new(),
        array_dispatch_names: Vec::new(),
        includes: Vec::new(),
    };

    // Typedefs (gated like the JS sweep — purely a fast path, the scans find
    // nothing without the substring anyway).
    if contains_bytes(s, b"typedef") {
        scan_fnptr_typedefs(s, &mut facts.fn_ptr_typedefs);
        scan_fntype_typedefs(s, &mut facts.fn_type_typedefs);
    }

    // Struct-node field declarations.
    if !structs.is_empty() {
        let lines = line_starts(s);
        for st in structs {
            let mut sf = StructFields { id: st.id.clone(), parsed: false, fields: Vec::new() };
            // sliceLinesPre: falsy startLine → '' (never parses). end_line
            // arrives with the TS side's `?? startLine` already applied; a
            // slice whose end ≤ start is empty, exactly like Array.slice.
            if st.start_line >= 1 {
                let a = (st.start_line - 1) as usize;
                let b = st.end_line as usize;
                if a < lines.len() && b > a {
                    let body_start = lines[a];
                    // End of line (b-1): next line start minus the `\n`, or EOF.
                    let body_end = if b < lines.len() { lines[b] - 1 } else { s.len() };
                    let body = &s[body_start..body_end.max(body_start)];
                    if let Some(open) = body.iter().position(|&c| c == b'{') {
                        if let Some(close) = match_brace(body, open) {
                            sf.parsed = true;
                            sf.fields = parse_struct_fields_raw(&body[open + 1..close]);
                        }
                    }
                }
            }
            facts.structs.push(sf);
        }
    }

    // Registration filters.
    if contains_bytes(s, b"{") {
        let inline = scan_inline_structs(s);
        facts.inline_ptr = inline.ptr;
        facts.inline_types = dedup_in_order(inline.types);
        facts.inline_tags = dedup_in_order(inline.tags);
        if contains_bytes(s, b"=") {
            scan_anchored(s, init_body, &mut facts.init_tokens);
            facts.init_tokens = dedup_in_order(std::mem::take(&mut facts.init_tokens));
            scan_anchored(s, array_table_body, &mut facts.array_elems);
            facts.array_elems = dedup_in_order(std::mem::take(&mut facts.array_elems));
        }
    }

    // Alias-shaped object macros.
    if contains_bytes(s, b"#define") || contains_bytes(s, b"# define") {
        scan_alias_names(s, &mut facts.alias_names);
        facts.alias_names = dedup_in_order(std::mem::take(&mut facts.alias_names));
    }

    // Propagation + dispatch filters.
    if contains_bytes(s, b"=") {
        scan_field_assign(s, &mut facts.d_pairs);
        facts.d_pairs = dedup_in_order(std::mem::take(&mut facts.d_pairs));
    }
    scan_dispatch(s, &mut facts.dispatch_fields);
    facts.dispatch_fields = dedup_in_order(std::mem::take(&mut facts.dispatch_fields));
    scan_array_dispatch(s, &mut facts.array_dispatch_names);
    facts.array_dispatch_names = dedup_in_order(std::mem::take(&mut facts.array_dispatch_names));

    // Includes come from the RAW text (string contents survive there).
    if contains_bytes(raw_b, b"include") {
        scan_includes(raw_b, &mut facts.includes);
    }

    facts
}

// ---------- stage C: per-file macro environment pieces ----------
//
// `buildEnv` walks a file's depth-2 include closure pulling four per-file
// extracts through the `src()` caches (read + strip + parse): function-like
// macros, object-like macros, the "defined" name set, and local includes.
// Each is a pure function of the file's bytes, so they are extracted here in
// one threaded pass — the recursion itself stays TS-side (a set-union walk
// once the per-file tables exist).

/// `#define NAME(p,…) expansion` — one function-like macro.
pub struct FnMacro {
    pub name: String,
    pub params: Vec<String>,
    pub expansion: String,
}

/// Everything `buildEnv` / the include-rescan pull per file.
pub struct FileEnv {
    pub fn_macros: Vec<FnMacro>,
    pub obj_macros: Vec<(String, String)>,
    pub defined: Vec<String>,
    /// Raw `#include "…"` captures, source order, undeduplicated — extension
    /// filtering and path resolution stay TS-side (they need the filesystem).
    pub includes: Vec<String>,
    /// The comment-stripped text — the JS path's `src(file)` result, already
    /// computed here; the TS side pushes it into `srcCache` so stage C's
    /// `processUnit` doesn't pay a second read+strip (the old `fileFnMacros`→
    /// `src()` call warmed that cache as a side effect — keep the parity).
    pub stripped: String,
}

/// `stripped.replace(/\\\r?\n/g, ' ')` — backslash line-continuations fold to
/// one space before the `#define` regexes run (their `.`/`[^)]*` see a single
/// logical line).
fn join_continuations(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        if s[i] == b'\\' && s.get(i + 1) == Some(&b'\n') {
            out.push(b' ');
            i += 2;
        } else if s[i] == b'\\' && s.get(i + 1) == Some(&b'\r') && s.get(i + 2) == Some(&b'\n') {
            out.push(b' ');
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// `[ \t]*#[ \t]*define[ \t]+` at line start `a` (line spans `[a, b)`, `\n`
/// excluded) → position of the macro name.
fn define_name_at(s: &[u8], a: usize, b: usize) -> Option<usize> {
    let mut i = a;
    while i < b && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    if s.get(i) != Some(&b'#') {
        return None;
    }
    i += 1;
    while i < b && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    if i + 6 > b || &s[i..i + 6] != b"define" {
        return None;
    }
    i += 6;
    let ws = i;
    while i < b && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    if i == ws {
        return None; // `[ \t]+` — at least one
    }
    Some(i)
}

/// JS `.` — every byte except the line terminators (`\n`, `\r`, and the
/// UTF-8 encodings of U+2028/U+2029).
#[inline]
fn is_js_dot(s: &[u8], i: usize) -> bool {
    match s[i] {
        b'\n' | b'\r' => false,
        0xE2 => !matches!((s.get(i + 1), s.get(i + 2)), (Some(&0x80), Some(&0xA8)) | (Some(&0x80), Some(&0xA9))),
        _ => true,
    }
}

/// JS-trim a byte range (both ends, full `\s` set).
fn js_trim(s: &[u8], mut a: usize, mut b: usize) -> (usize, usize) {
    while a < b && jsws_len(s, a) > 0 {
        a += jsws_len(s, a);
    }
    while b > a && jsws_len_back(s, b) > 0 {
        b -= jsws_len_back(s, b);
    }
    (a, b)
}

/// Byte length of the JS `\s` char ENDING at `b` (backward jsws_len).
fn jsws_len_back(s: &[u8], b: usize) -> usize {
    let Some(&b0) = b.checked_sub(1).and_then(|i| s.get(i)) else { return 0 };
    match b0 {
        0x09..=0x0D | 0x20 => 1,
        0xA0 if b >= 2 && s[b - 2] == 0xC2 => 2,
        0x80 if b >= 3 && s[b - 3] == 0xE1 && s[b - 2] == 0x9A => 3,
        0x80..=0x8A | 0xA8 | 0xA9 | 0xAF if b >= 3 && s[b - 3] == 0xE2 && s[b - 2] == 0x80 => 3,
        0x9F if b >= 3 && s[b - 3] == 0xE2 && s[b - 2] == 0x81 => 3,
        0x80 if b >= 3 && s[b - 3] == 0xE3 && s[b - 2] == 0x80 => 3,
        0xBF if b >= 3 && s[b - 3] == 0xEF && s[b - 2] == 0xBB => 3,
        _ => 0,
    }
}

/// Byte length of the JS LineTerminator starting at `i` (`\n`, `\r`, U+2028,
/// U+2029), or 0. Multiline `^` holds at 0 and right after each of these;
/// multiline `$` holds right before them (and at end of text).
#[inline]
fn line_term_len(s: &[u8], i: usize) -> usize {
    match s.get(i) {
        Some(&b'\n') | Some(&b'\r') => 1,
        Some(&0xE2)
            if s.get(i + 1) == Some(&0x80)
                && matches!(s.get(i + 2), Some(&0xA8) | Some(&0xA9)) =>
        {
            3
        }
        _ => 0,
    }
}

/// Next position where JS multiline `^` holds, scanning from `from` — i.e.
/// right after the next line terminator. `exec` on a failed `^`-anchored
/// candidate skips ahead exactly this way (nothing between anchors can
/// satisfy `^`); on a match ending at `e`, continuing from `e` lands on the
/// same position since `e` sits at a terminator or end-of-text.
fn next_anchor(s: &[u8], mut from: usize) -> Option<usize> {
    while from < s.len() {
        let l = line_term_len(s, from);
        if l > 0 {
            return Some(from + l);
        }
        from += 1;
    }
    None
}

/// parseFunctionMacros: `/^[ \t]*#[ \t]*define[ \t]+(\w+)\(([^)]*)\)\s+(.+)$/gm`
/// over the continuation-joined strip. `[^)]` and `\s` cross line terminators;
/// `.` stops at any of them and `$` holds wherever `.+` stopped, so the tail
/// only fails when empty.
fn parse_fn_macros(joined: &[u8]) -> Vec<FnMacro> {
    let mut out = Vec::new();
    let mut pos = 0usize; // candidate `^` position
    while pos < joined.len() {
        // `[ \t]*#[ \t]*define[ \t]+` — spaces/tabs/literals never cross a
        // terminator, so the candidate's own line needs no bound.
        let Some(name_at) = define_name_at(joined, pos, joined.len()) else {
            let Some(n) = next_anchor(joined, pos) else { break };
            pos = n;
            continue;
        };
        let ne = word_end(joined, name_at);
        // `(` must IMMEDIATELY follow the name for the function-like form.
        if joined.get(ne) != Some(&b'(') {
            let Some(n) = next_anchor(joined, pos) else { break };
            pos = n;
            continue;
        }
        // params `([^)]*)` — `[^)]` spans line terminators.
        let mut j = ne + 1;
        while j < joined.len() && joined[j] != b')' {
            j += 1;
        }
        if j >= joined.len() {
            break; // no `)` remains anywhere — every later `(` candidate fails too
        }
        // `)\s+` — JS `\s`, terminators included
        let k = skip_jsws(joined, j + 1);
        // `(.+)$` — `.` run to the first terminator; `$` holds there or at end.
        let es = k;
        let mut e = k;
        while e < joined.len() && is_js_dot(joined, e) {
            e += 1;
        }
        if k == j + 1 || e == es {
            let Some(n) = next_anchor(joined, pos) else { break };
            pos = n;
            continue;
        }
        let (ta, tb) = js_trim(joined, es, e);
        let expansion = bytes_to_string(&joined[ta..tb]);
        let params: Vec<String> = params_raw(joined, ne + 1, j);
        if params.iter().all(|p| p != "..." && !p.ends_with("...")) {
            out.push(FnMacro {
                name: bytes_to_string(&joined[name_at..ne]),
                params,
                expansion,
            });
        }
        // variadic or not, lastIndex lands at `e`; the next `^` follows the
        // terminator sitting there.
        let Some(n) = next_anchor(joined, e) else { break };
        pos = n;
    }
    out
}

/// `[^)]*` split on `,`, each `.trim()`ed, empties dropped.
fn params_raw(s: &[u8], a: usize, b: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut seg = a;
    for i in a..=b {
        if i == b || s[i] == b',' {
            let (ta, tb) = js_trim(s, seg, i);
            if tb > ta {
                out.push(bytes_to_string(&s[ta..tb]));
            }
            seg = i + 1;
        }
    }
    out
}

/// parseObjectMacros: `/^[ \t]*#[ \t]*define[ \t]+(\w+)[ \t]+(\S[^\n]*)$/gm`
/// over the continuation-joined strip. `(\w+)` not followed by `(` lands here
/// (the two regexes are disjoint by construction: `(` vs `[ \t]+` after the
/// name). `[^\n]` spans `\r`/U+2028/U+2029 — only `\n` (or end-of-text) ends
/// the value, where `$` holds.
fn parse_obj_macros(joined: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < joined.len() {
        let Some(name_at) = define_name_at(joined, pos, joined.len()) else {
            let Some(n) = next_anchor(joined, pos) else { break };
            pos = n;
            continue;
        };
        let ne = word_end(joined, name_at);
        // `[ \t]+` after the name, then `\S` (any non-`\s`, terminators
        // included — but a `\S` value can't start with one anyway).
        let mut k = ne;
        while joined.get(k) == Some(&b' ') || joined.get(k) == Some(&b'\t') {
            k += 1;
        }
        if k == ne || jsws_len(joined, k) > 0 || k >= joined.len() {
            let Some(n) = next_anchor(joined, pos) else { break };
            pos = n;
            continue;
        }
        // `[^\n]*` to the next `\n`; `$` holds there or at end-of-text.
        let mut eol = k;
        while eol < joined.len() && joined[eol] != b'\n' {
            eol += 1;
        }
        let (ta, tb) = js_trim(joined, k, eol);
        out.push((bytes_to_string(&joined[name_at..ne]), bytes_to_string(&joined[ta..tb])));
        // lastIndex = eol (the `\n` position); next `^` is just past it.
        pos = eol + 1;
    }
    out
}

/// parseDefinedNames: `/^[ \t]*#[ \t]*define[ \t]+(\w+)/gm` over the RAW strip
/// (no continuation join — the JS side passes `stripped` directly).
fn parse_defined_names(stripped: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < stripped.len() {
        let Some(name_at) = define_name_at(stripped, pos, stripped.len()) else {
            let Some(n) = next_anchor(stripped, pos) else { break };
            pos = n;
            continue;
        };
        let ne = word_end(stripped, name_at);
        out.push(bytes_to_string(&stripped[name_at..ne]));
        // lastIndex = ne; the next `^` follows the next terminator.
        let Some(n) = next_anchor(stripped, ne) else { break };
        pos = n;
    }
    out
}

/// Per-file env extraction for stage C's `buildEnv` — one read + strip +
/// three macro parses, all pure functions of the file bytes. Returns None on
/// an unreadable path so the TS side can fall back to `ctx.readFile` (a
/// virtual FS still resolves through the JS path).
pub fn file_env(raw: &str) -> FileEnv {
    let raw_b = raw.as_bytes();
    let stripped = strip_c(raw_b);
    let has_define = contains_bytes(&stripped, b"#define") || contains_bytes(&stripped, b"# define");
    let mut env = FileEnv {
        fn_macros: Vec::new(),
        obj_macros: Vec::new(),
        defined: Vec::new(),
        includes: Vec::new(),
        stripped: String::new(),
    };
    if has_define {
        let joined = join_continuations(&stripped);
        env.fn_macros = parse_fn_macros(&joined);
        env.obj_macros = parse_obj_macros(&joined);
        env.defined = parse_defined_names(&stripped);
    }
    if contains_bytes(raw_b, b"include") {
        scan_includes(raw_b, &mut env.includes);
    }
    env.stripped = String::from_utf8_lossy(&stripped).into_owned();
    env
}

// ---------- stages D + E: propagation fixpoint + dispatch edges ----------
//
// The TS side hands in the survivor files (already filtered on facts), their
// fn-node extents, and the registration tables. D scans fn bodies for
// `a->f = b->g` field←field assignments between fn-pointer fields, merges the
// pairs into `reg` to a 3-pass fixpoint, then E scans for dispatch sites
// (`recv->field(…)`, `recv.field(…)`, `arr[i](…)`) and emits call edges.

/// One fn-node extent, as the TS side reads it from the graph.
pub struct LinkFn {
    pub id: String,
    pub start_line: i64,
    pub end_line: i64,
}

/// A survivor file: `prop`/`dispatch` are the stage-D/E survivor flags the TS
/// filter already computed; `fns` are its function/method extents in
/// `getNodesInFile` order.
pub struct LinkFile {
    /// Project-relative path — `fn.filePath`, used in `registeredAt`.
    pub rel: String,
    /// Absolute path the kernel reads.
    pub abs: String,
    pub prop: bool,
    pub dispatch: bool,
    pub fns: Vec<LinkFn>,
}

/// `{name, type, isFnPtr}` — the three FieldInfo members D/E consult.
pub struct LinkField {
    pub name: String,
    pub ftype: Option<String>,
    pub is_fn_ptr: bool,
}

/// `{file, ids}` — one arrayReg entry.
pub struct LinkArrEntry {
    pub file: String,
    pub ids: Vec<String>,
}

/// The registration tables, verbatim.
pub struct LinkTables {
    /// fieldToStructs — field → structs declaring it as a fn-pointer field.
    pub field_to_structs: Vec<(String, Vec<String>)>,
    /// structLayout — the registered layout per struct name.
    pub struct_layout: Vec<(String, Vec<LinkField>)>,
    /// allStructFields — every layout variant per struct name.
    pub all_struct_fields: Vec<(String, Vec<Vec<LinkField>>)>,
    /// globalVarType — file-scope table variable → struct type.
    pub global_var_type: Vec<(String, String)>,
    /// reg — `struct.field` → registered fn-node ids (insertion order).
    pub reg: Vec<(String, Vec<String>)>,
    /// arrayReg — array name → [{file, ids}].
    pub array_reg: Vec<(String, Vec<LinkArrEntry>)>,
}

/// One synthesized dispatch edge.
pub struct LinkEdge {
    pub source: String,
    pub target: String,
    pub line: i64,
    pub via: String,
    pub registered_at: String,
}

struct Tabs<'t> {
    field_to_structs: std::collections::HashMap<&'t str, &'t Vec<String>>,
    layout: std::collections::HashMap<&'t str, &'t Vec<LinkField>>,
    all_fields: std::collections::HashMap<&'t str, &'t Vec<Vec<LinkField>>>,
    global_var_type: std::collections::HashMap<&'t str, &'t str>,
    reg: std::collections::HashMap<String, Vec<String>>,
    array_reg: std::collections::HashMap<&'t str, &'t Vec<LinkArrEntry>>,
}

impl<'t> Tabs<'t> {
    fn from(t: &'t LinkTables) -> Tabs<'t> {
        Tabs {
            field_to_structs: t
                .field_to_structs
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect(),
            layout: t.struct_layout.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            all_fields: t.all_struct_fields.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            global_var_type: t
                .global_var_type
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
            reg: t.reg.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            array_reg: t.array_reg.iter().map(|(k, v)| (k.as_str(), v)).collect(),
        }
    }
}

/// The type token a receiver declaration carries: scan each `\brecv\b`
/// occurrence backward for `(\w+)\s*\*?\s*` before it and forward for
/// `(?:[,)=;]|\[)` after it — the observable matches of
/// `(?:(?:struct|union)\s+)?(\w+)\s*\*?\s*\bRECV\b\s*(?:[,)=;]|\[)` in order.
fn recv_decl_types<'b>(body: &'b [u8], recv: &'b [u8]) -> impl Iterator<Item = &'b [u8]> {
    let mut from = 0usize;
    std::iter::from_fn(move || loop {
        let q = find_word(body, recv, from)?;
        from = q + 1;
        // forward: `\b` (find_word), `\s*`, one of `,)=;[`
        let k = skip_jsws(body, q + recv.len());
        if !(k < body.len() && matches!(body[k], b',' | b')' | b'=' | b';' | b'[')) {
            continue;
        }
        // backward: `\s*`, optional `*`, `\s*`, then the `(\w+)` ending there
        let mut j = q;
        while j > 0 && jsws_len_back(body, j) > 0 {
            j -= jsws_len_back(body, j);
        }
        if j > 0 && body[j - 1] == b'*' {
            j -= 1;
            while j > 0 && jsws_len_back(body, j) > 0 {
                j -= jsws_len_back(body, j);
            }
        }
        let mut ws = j;
        while ws > 0 && is_word(body[ws - 1]) {
            ws -= 1;
        }
        if ws == j {
            continue; // no type word before the receiver
        }
        return Some(&body[ws..j]);
    })
}

/// `recvTypeIn` — first declared type that is a registered layout.
fn recv_type_in<'b>(body: &'b [u8], recv: &'b [u8], tabs: &Tabs) -> Option<&'b str> {
    for ty in recv_decl_types(body, recv) {
        let Ok(t) = std::str::from_utf8(ty) else { continue };
        if tabs.layout.contains_key(t) {
            return Some(t);
        }
    }
    None
}

/// `varTypeIn` — first declared type that isn't a C keyword, else the
/// file-scope table-variable map.
fn var_type_in<'t>(body: &[u8], v: &[u8], tabs: &'t Tabs<'t>) -> Option<String> {
    for ty in recv_decl_types(body, v) {
        if !is_type_keyword(ty) {
            return Some(bytes_to_string(ty));
        }
    }
    tabs.global_var_type.get(std::str::from_utf8(v).ok()?).map(|s| s.to_string())
}

/// The chain's segments — `chain.replace(/\s*\[[^\]]*\]/g,'')` then split on
/// the `->`/`.` separators. The chain text is regex-built (words, subscripts,
/// separators, whitespace only), so collecting `\w+` runs outside `[…]` spans
/// yields exactly the filtered segments.
fn chain_segs<'b>(chain: &'b [u8]) -> Vec<&'b [u8]> {
    let mut segs = Vec::new();
    let mut i = 0usize;
    while i < chain.len() {
        if chain[i] == b'[' {
            // `\s*\[[^\]]*\]` — to the first `]` (inner `[` allowed).
            while i < chain.len() && chain[i] != b']' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if is_word(chain[i]) {
            let e = word_end(chain, i);
            segs.push(&chain[i..e]);
            i = e;
            continue;
        }
        i += 1;
    }
    segs
}

/// `resolveChainType` — walk `seg[i]`'s declared field type down the chain.
fn resolve_chain_type(body: &[u8], chain: &[u8], tabs: &Tabs) -> Option<String> {
    let segs = chain_segs(chain);
    let mut t = var_type_in(body, segs[0], tabs)?;
    for seg in &segs[1..] {
        let mut next: Option<String> = None;
        if let Some(variants) = tabs.all_fields.get(t.as_str()) {
            for fields in variants.iter() {
                if let Some(f) = fields.iter().find(|f| f.name.as_bytes() == *seg && f.ftype.is_some()) {
                    next = f.ftype.clone();
                    break;
                }
            }
        }
        t = next?;
    }
    Some(t)
}

/// `fnPtrFieldOf` — the registered layout has this field as a fn pointer.
fn fn_ptr_field_of(struct_name: &str, field: &[u8], tabs: &Tabs) -> bool {
    tabs.layout
        .get(struct_name)
        .is_some_and(|fields| fields.iter().any(|f| f.name.as_bytes() == field && f.is_fn_ptr))
}

fn owners_has(owners: &[String], t: &str) -> bool {
    owners.iter().any(|o| o == t)
}

/// FIELD_ASSIGN_RE match with all four captures:
/// `(\w+)\s*(?:->|\.)\s*(\w+)\s*=\s*(\w+)\s*(?:->|\.)\s*(\w+)`
struct FaMatch {
    lrecv: Range,
    lfield: Range,
    rrecv: Range,
    rfield: Range,
    end: usize,
}

fn field_assign_match(s: &[u8], p: usize) -> Option<FaMatch> {
    let w1e = word_end(s, p);
    let a1 = arrow_at(s, skip_jsws(s, w1e))?;
    let f1s = skip_jsws(s, a1);
    if !is_word_at(s, f1s) {
        return None;
    }
    let f1e = word_end(s, f1s);
    let eq = skip_jsws(s, f1e);
    if s.get(eq) != Some(&b'=') {
        return None;
    }
    let r1s = skip_jsws(s, eq + 1);
    if !is_word_at(s, r1s) {
        return None;
    }
    let r1e = word_end(s, r1s);
    let a2 = arrow_at(s, skip_jsws(s, r1e))?;
    let f2s = skip_jsws(s, a2);
    if !is_word_at(s, f2s) {
        return None;
    }
    let f2e = word_end(s, f2s);
    Some(FaMatch { lrecv: (p, w1e), lfield: (f1s, f1e), rrecv: (r1s, r1e), rfield: (f2s, f2e), end: f2e })
}

/// DISPATCH_RE match: `/((?:\w+(?:\s*\[[^\][]*\])?\s*(?:->|\.)\s*)+)(\w+)\s*\)?\s*\(/g`.
/// `chain` covers the receiver text (`m[1]` minus its trailing arrow, i.e.
/// through the last segment's word/subscript end); `mstart`/`end` are the
/// whole match bounds for the exec loop and the line counter.
struct DsMatch {
    mstart: usize,
    chain: Range,
    field: Range,
    end: usize,
}

fn dispatch_match(s: &[u8], p: usize) -> Option<DsMatch> {
    let mut seg_ends: Vec<usize> = Vec::new();
    let mut seg_body_ends: Vec<usize> = Vec::new();
    let mut cur = p;
    while is_word_at(s, cur) {
        let we = word_end(s, cur);
        let after_w = skip_jsws(s, we);
        let sub = subscript_span(s, after_w);
        let seg = sub.and_then(|e| arrow_tail(s, e)).or_else(|| arrow_tail(s, we));
        match seg {
            Some(e) => {
                seg_ends.push(e);
                seg_body_ends.push(sub.unwrap_or(we));
                cur = e;
            }
            None => break,
        }
    }
    for k in (1..=seg_ends.len()).rev() {
        let fpos = seg_ends[k - 1];
        if !is_word_at(s, fpos) {
            continue;
        }
        let fe = word_end(s, fpos);
        if let Some(end) = close_call_tail(s, fe) {
            return Some(DsMatch {
                mstart: p,
                chain: (p, seg_body_ends[k - 1]),
                field: (fpos, fe),
                end,
            });
        }
    }
    None
}

/// ARRAY_DISPATCH_RE match:
/// `/(?:\(\s*\*\s*)?\b(\w+)\s*\[[^\][]*\]\s*\)?\s*\(/g` — name + bounds.
struct AdMatch {
    mstart: usize,
    name: Range,
    end: usize,
}

fn array_dispatch_match(s: &[u8], p: usize) -> Option<AdMatch> {
    let b = s[p];
    if b != b'(' && !(is_word(b) && boundary_before(s, p)) {
        return None;
    }
    let name_start = if b == b'(' {
        let i = skip_jsws(s, p + 1);
        if s.get(i) == Some(&b'*') {
            let j = skip_jsws(s, i + 1);
            if is_word_at(s, j) {
                Some(j)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        Some(p)
    };
    let ns = name_start?;
    let ne = word_end(s, ns);
    let sub = subscript_span(s, skip_jsws(s, ne))?;
    let end = close_call_tail(s, sub)?;
    Some(AdMatch { mstart: p, name: (ns, ne), end })
}

/// The fn body slice — `sliceLinesPre(lines, start, end)`: falsy start →
/// empty; 1-indexed inclusive [start..end].
fn body_slice<'s>(s: &'s [u8], lines: &[usize], start_line: i64, end_line: i64) -> &'s [u8] {
    if start_line < 1 {
        return &s[0..0];
    }
    let a = (start_line - 1) as usize;
    let b = end_line as usize;
    if a >= lines.len() || b <= a {
        return &s[0..0];
    }
    let body_start = lines[a];
    let body_end = if b < lines.len() { lines[b] - 1 } else { s.len() };
    &s[body_start..body_end.max(body_start)]
}

/// `exec`-style scan for FIELD_ASSIGN_RE: candidate at every word byte, a
/// failed candidate advances by one, a match resumes at its end.
fn next_field_assign(s: &[u8], mut pos: usize) -> Option<FaMatch> {
    while pos < s.len() {
        if is_word(s[pos]) {
            if let Some(m) = field_assign_match(s, pos) {
                return Some(m);
            }
        }
        pos += 1;
    }
    None
}

/// `exec`-style scan for DISPATCH_RE.
fn next_dispatch(s: &[u8], mut pos: usize) -> Option<DsMatch> {
    while pos < s.len() {
        if is_word(s[pos]) {
            if let Some(m) = dispatch_match(s, pos) {
                return Some(m);
            }
        }
        pos += 1;
    }
    None
}

/// `exec`-style scan for ARRAY_DISPATCH_RE — candidates at `(` or a
/// word-boundary word byte.
fn next_array_dispatch(s: &[u8], mut pos: usize) -> Option<AdMatch> {
    while pos < s.len() {
        if s[pos] == b'(' || (is_word(s[pos]) && boundary_before(s, pos)) {
            if let Some(m) = array_dispatch_match(s, pos) {
                return Some(m);
            }
        }
        pos += 1;
    }
    None
}

/// Stage D for one file — `a->f = b->g` where both fields are fn-pointer
/// fields of resolved receiver structs → (to, from) `struct.field` pairs.
fn link_propagate_file(text: &str, f: &LinkFile, tabs: &Tabs, out: &mut Vec<(String, String)>) {
    let stripped = strip_c(text.as_bytes());
    let s: &[u8] = &stripped;
    if !contains_bytes(s, b"=") {
        return;
    }
    let lines = line_starts(s);
    for fun in &f.fns {
        let body = body_slice(s, &lines, fun.start_line, fun.end_line);
        if !contains_bytes(body, b"=") {
            continue;
        }
        let mut pos = 0usize;
        while let Some(m) = next_field_assign(body, pos) {
            pos = m.end;
            let lf = &body[m.lfield.0..m.lfield.1];
            let rf = &body[m.rfield.0..m.rfield.1];
            let lfs = bytes_to_string(lf);
            let rfs = bytes_to_string(rf);
            if !tabs.field_to_structs.contains_key(lfs.as_str())
                || !tabs.field_to_structs.contains_key(rfs.as_str())
            {
                continue;
            }
            let lt = recv_type_in(body, &body[m.lrecv.0..m.lrecv.1], tabs);
            let rt = recv_type_in(body, &body[m.rrecv.0..m.rrecv.1], tabs);
            if let (Some(lt), Some(rt)) = (lt, rt) {
                if fn_ptr_field_of(lt, lf, tabs) && fn_ptr_field_of(rt, rf, tabs) {
                    out.push((format!("{}.{}", lt, lfs), format!("{}.{}", rt, rfs)));
                }
            }
        }
    }
}

const FANOUT_CAP: usize = 300;

/// Stage E for one file — dispatch sites → edges. `seen` is the JS pass's
/// global dedupe, but a `fn.id > tid` key can't collide across files (node
/// ids are file-scoped), so a per-file set reproduces it exactly.
fn link_dispatch_file(text: &str, f: &LinkFile, tabs: &Tabs, out: &mut Vec<LinkEdge>) {
    let stripped = strip_c(text.as_bytes());
    let s: &[u8] = &stripped;
    let lines = line_starts(s);
    let mut seen = std::collections::HashSet::new();
    for fun in &f.fns {
        let body = body_slice(s, &lines, fun.start_line, fun.end_line);
        let mut added = 0usize;
        // Incremental newline counting, like the JS `lineAt` cursor — matches
        // arrive in ascending index order.
        let mut lc_idx = 0usize;
        let mut lc_line = fun.start_line;
        // `recv->…->field(` / `recv.…field(` dispatch sites.
        let mut pos = 0usize;
        while added < FANOUT_CAP {
            let Some(m) = next_dispatch(body, pos) else { break };
            pos = m.end;
            let field = &body[m.field.0..m.field.1];
            let field_s = bytes_to_string(field);
            let Some(owners) = tabs.field_to_structs.get(field_s.as_str()) else {
                continue;
            };
            if owners.is_empty() {
                continue;
            }
            let chain = &body[m.chain.0..m.chain.1];
            // 1) the whole receiver chain's declared type; 2) else the last
            // segment as a local/param of a fn-pointer-bearing struct; 3)
            // else a field owned by exactly one struct.
            let mut structure = resolve_chain_type(body, chain, tabs);
            if structure.as_deref().is_none_or(|t| !owners_has(owners, t)) {
                structure = chain_segs(chain)
                    .last()
                    .and_then(|w| recv_type_in(body, w, tabs))
                    .filter(|t| owners_has(owners, t))
                    .map(str::to_string);
            }
            if structure.as_deref().is_none_or(|t| !owners_has(owners, t)) {
                structure = (owners.len() == 1).then(|| owners[0].clone());
            }
            let Some(struct_name) = structure else { continue };
            let Some(targets) = tabs.reg.get(&format!("{}.{}", struct_name, field_s)) else {
                continue;
            };
            while lc_idx < m.mstart {
                if body[lc_idx] == b'\n' {
                    lc_line += 1;
                }
                lc_idx += 1;
            }
            let line = lc_line;
            let via = format!("{}.{}", struct_name, field_s);
            for tid in targets {
                if *tid == fun.id || !seen.insert((fun.id.clone(), tid.clone())) {
                    continue;
                }
                out.push(LinkEdge {
                    source: fun.id.clone(),
                    target: tid.clone(),
                    line,
                    via: via.clone(),
                    registered_at: format!("{}:{}", f.rel, line),
                });
                added += 1;
                if added >= FANOUT_CAP {
                    break;
                }
            }
        }
        // `arr[i](…)` / `(*arr[i])(…)` — registered bare arrays; fresh scan
        // from the body's start, line-count cursor rewound with it.
        if tabs.array_reg.is_empty() || added >= FANOUT_CAP {
            continue;
        }
        lc_idx = 0;
        lc_line = fun.start_line;
        let mut pos = 0usize;
        while added < FANOUT_CAP {
            let Some(m) = next_array_dispatch(body, pos) else { break };
            pos = m.end;
            let name = &body[m.name.0..m.name.1];
            let Some(entries) = tabs.array_reg.get(std::str::from_utf8(name).unwrap_or("")) else {
                continue;
            };
            // Same-file table wins a name collision; a unique name resolves
            // cross-file; otherwise ambiguous — bail.
            let ids = if entries.len() == 1 {
                Some(&entries[0].ids)
            } else {
                entries.iter().find(|e| e.file == f.rel).map(|e| &e.ids)
            };
            let Some(ids) = ids else { continue };
            while lc_idx < m.mstart {
                if body[lc_idx] == b'\n' {
                    lc_line += 1;
                }
                lc_idx += 1;
            }
            let line = lc_line;
            let via = format!("{}[]", bytes_to_string(name));
            for tid in ids {
                if *tid == fun.id || !seen.insert((fun.id.clone(), tid.clone())) {
                    continue;
                }
                out.push(LinkEdge {
                    source: fun.id.clone(),
                    target: tid.clone(),
                    line,
                    via: via.clone(),
                    registered_at: format!("{}:{}", f.rel, line),
                });
                added += 1;
                if added >= FANOUT_CAP {
                    break;
                }
            }
        }
    }
}

fn read_text(abs: &str) -> Option<String> {
    std::fs::read(abs)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .filter(|t| !t.is_empty())
}

/// Chunked scoped-thread map over files — the scan_paths pattern. Output is
/// chunk-concatenated in input order; a panicking chunk contributes nothing.
fn threaded_files<F, R>(files: &[&LinkFile], work: F) -> Vec<R>
where
    F: Fn(&LinkFile) -> R + Sync,
    R: Send,
{
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16)
        .min(files.len().max(1));
    if threads <= 1 {
        return files.iter().map(|f| work(f)).collect();
    }
    let chunk_len = files.len().div_ceil(threads);
    let chunks: Vec<&[&LinkFile]> = files.chunks(chunk_len).collect();
    let mut parts: Vec<Vec<R>> = Vec::with_capacity(chunks.len());
    std::thread::scope(|sc| {
        let work = &work;
        let handles: Vec<_> = chunks
            .iter()
            .map(|c| sc.spawn(move || c.iter().map(|f| work(f)).collect::<Vec<_>>()))
            .collect();
        for h in handles {
            parts.push(h.join().unwrap_or_default());
        }
    });
    parts.into_iter().flatten().collect()
}

/// Stage D+E for the whole survivor set. Propagation pairs are gathered per
/// file across scoped threads (file order preserved), merged into `reg` to
/// the 3-pass fixpoint, then the dispatch scan runs the same way. The TS side
/// keeps its loops as the fallback for kernels without this entry point.
pub fn cfnptr_link(files: &[LinkFile], tables: &LinkTables) -> Vec<LinkEdge> {
    let mut tabs = Tabs::from(tables);

    // ---- D: field←field propagations (tables read-only here) ----
    let prop: Vec<&LinkFile> = files.iter().filter(|f| f.prop).collect();
    let tabs_ref = &tabs;
    let propagations: Vec<(String, String)> = threaded_files(&prop, |f| {
        let mut v = Vec::new();
        if let Some(t) = read_text(&f.abs) {
            link_propagate_file(&t, f, tabs_ref, &mut v);
        }
        v
    })
    .into_iter()
    .flatten()
    .collect();

    // ---- fixpoint: `to` inherits `from`'s handlers, ≤3 passes ----
    for _ in 0..3 {
        if propagations.is_empty() {
            break;
        }
        let mut changed = false;
        for (to, from) in &propagations {
            let Some(from_ids) = tabs.reg.get(from).cloned() else { continue };
            let to_set = tabs.reg.entry(to.clone()).or_default();
            for id in from_ids {
                if !to_set.contains(&id) {
                    to_set.push(id);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    if tabs.reg.is_empty() && tabs.array_reg.is_empty() {
        return Vec::new();
    }

    // ---- E: dispatch sites → edges ----
    let disp: Vec<&LinkFile> = files.iter().filter(|f| f.dispatch).collect();
    let tabs_ref = &tabs;
    threaded_files(&disp, |f| {
        let mut v = Vec::new();
        if let Some(t) = read_text(&f.abs) {
            link_dispatch_file(&t, f, tabs_ref, &mut v);
        }
        v
    })
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(src: &str) -> FileFacts {
        scan_file(src, &[])
    }

    #[test]
    fn strip_blanks_comments_keeps_strings() {
        let s = strip_c(b"a /* x\ny */ b // c\nd \"in//str\" e");
        assert_eq!(&s, b"a     \n     b     \nd \"in//str\" e".as_slice());
    }

    #[test]
    fn typedef_forms() {
        let f = facts("typedef void (*hook_fn)(int);\ntypedef void redisCommandProc(int c);\n");
        assert_eq!(f.fn_ptr_typedefs, vec!["hook_fn"]);
        assert_eq!(f.fn_type_typedefs, vec!["redisCommandProc"]);
    }

    #[test]
    fn init_modifier_backtrack() {
        // `static x = {` must match with type token `static` (the JS engine
        // backtracks the modifier loop) — harmless downstream, but collected.
        let f = facts("; static x = {1};\n; static struct cmd t[] = { {0} };");
        assert!(f.init_tokens.contains(&"static".to_string()));
        assert!(f.init_tokens.contains(&"cmd".to_string()));
    }

    #[test]
    fn dispatch_backtracks_segments() {
        let f = facts("int go(struct c *x){ x->cmd->proc(1); tbl[i](2); (*ops[k])(3); }");
        assert!(f.dispatch_fields.contains(&"proc".to_string()));
        assert!(f.array_dispatch_names.contains(&"tbl".to_string()));
        assert!(f.array_dispatch_names.contains(&"ops".to_string()));
    }

    #[test]
    fn field_assign_pairs() {
        let f = facts("void g(void){ a->f = b->g; h.x = k.y; m == n; }");
        assert!(f.d_pairs.contains(&"f\0g".to_string()));
        assert!(f.d_pairs.contains(&"x\0y".to_string()));
        assert_eq!(f.d_pairs.len(), 2);
    }

    #[test]
    fn alias_shapes() {
        let f = facts("#define A redisCommand\n#define B struct foo\n#define C 0x12\n#define D(x) x\n");
        assert!(f.alias_names.contains(&"A".to_string()));
        assert!(f.alias_names.contains(&"B".to_string()));
        assert!(!f.alias_names.contains(&"C".to_string()));
        assert!(!f.alias_names.contains(&"D".to_string()));
    }

    #[test]
    fn includes_from_raw() {
        let f = facts("#include \"commands.def\"\n// #include \"in-comment.h\"\n");
        // Raw-text scan: the commented include IS captured (parity with the
        // JS INCLUDE_RE over raw text).
        assert_eq!(f.includes, vec!["commands.def", "in-comment.h"]);
    }
}
