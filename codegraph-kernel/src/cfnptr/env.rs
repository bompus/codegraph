//! Stage C: the per-file macro environment (function-like and object-like macros, defined names).

use super::*;

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
pub(super) fn join_continuations(s: &[u8]) -> Vec<u8> {
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
pub(super) fn define_name_at(s: &[u8], a: usize, b: usize) -> Option<usize> {
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
pub(super) fn is_js_dot(s: &[u8], i: usize) -> bool {
    match s[i] {
        b'\n' | b'\r' => false,
        0xE2 => !matches!((s.get(i + 1), s.get(i + 2)), (Some(&0x80), Some(&0xA8)) | (Some(&0x80), Some(&0xA9))),
        _ => true,
    }
}

/// JS-trim a byte range (both ends, full `\s` set).
pub(super) fn js_trim(s: &[u8], mut a: usize, mut b: usize) -> (usize, usize) {
    while a < b && jsws_len(s, a) > 0 {
        a += jsws_len(s, a);
    }
    while b > a && jsws_len_back(s, b) > 0 {
        b -= jsws_len_back(s, b);
    }
    (a, b)
}

/// Byte length of the JS `\s` char ENDING at `b` (backward jsws_len).
pub(super) fn jsws_len_back(s: &[u8], b: usize) -> usize {
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
pub(super) fn line_term_len(s: &[u8], i: usize) -> usize {
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
pub(super) fn next_anchor(s: &[u8], mut from: usize) -> Option<usize> {
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
pub(super) fn parse_fn_macros(joined: &[u8]) -> Vec<FnMacro> {
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
pub(super) fn params_raw(s: &[u8], a: usize, b: usize) -> Vec<String> {
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
pub(super) fn parse_obj_macros(joined: &[u8]) -> Vec<(String, String)> {
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
pub(super) fn parse_defined_names(stripped: &[u8]) -> Vec<String> {
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
    // Valid UTF-8 (the usual case) moves in without a copy.
    env.stripped = String::from_utf8(stripped).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    env
}
