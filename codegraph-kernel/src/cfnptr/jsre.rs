//! Byte-level JavaScript-regex primitives the scanners are built from: the ASCII `\w`/`\b`, the Unicode `\s`, word and brace spans, and the shared regex tails.


/// Mirror of the TS `C_TYPE_KEYWORDS` set — keep in exact sync.
pub(super) const C_TYPE_KEYWORDS: [&[u8]; 17] = [
    b"void", b"int", b"char", b"short", b"long", b"unsigned", b"signed", b"float", b"double",
    b"const", b"struct", b"union", b"enum", b"static", b"volatile", b"register", b"inline",
];

pub(super) fn is_type_keyword(w: &[u8]) -> bool {
    C_TYPE_KEYWORDS.contains(&w)
}

pub(super) const MODIFIERS: [&[u8]; 5] = [b"static", b"const", b"extern", b"register", b"volatile"];

#[inline]
pub(super) fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[inline]
pub(super) fn is_word_at(s: &[u8], i: usize) -> bool {
    i < s.len() && is_word(s[i])
}

/// Byte length of the JS `\s` character starting at `i`, or 0 when `s[i]`
/// doesn't start one. JS \s = [\t\n\v\f\r    -
///    　﻿].
#[inline]
pub(super) fn jsws_len(s: &[u8], i: usize) -> usize {
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
pub(super) fn skip_jsws(s: &[u8], mut i: usize) -> usize {
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
pub(super) fn word_end(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && is_word(s[i]) {
        i += 1;
    }
    i
}

/// JS `\b` before position `i` (position 0, or previous byte non-word).
#[inline]
pub(super) fn boundary_before(s: &[u8], i: usize) -> bool {
    i == 0 || !is_word(s[i - 1])
}

pub(super) fn find_bytes(s: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || s.len() < needle.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= s.len() {
        // memchr on the first byte keeps this fast on 20KB+ files.
        let off = s[i..s.len() - needle.len() + 1].iter().position(|&b| b == needle[0])?;
        i += off;
        if &s[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub(super) fn contains_bytes(s: &[u8], needle: &[u8]) -> bool {
    find_bytes(s, needle, 0).is_some()
}

/// `\bWORD\b` occurrence search from `from`.
pub(super) fn find_word(s: &[u8], word: &[u8], mut from: usize) -> Option<usize> {
    loop {
        let t = find_bytes(s, word, from)?;
        if boundary_before(s, t) && !is_word_at(s, t + word.len()) {
            return Some(t);
        }
        from = t + 1;
    }
}

// ---------- shared regex tails ----------

/// `\(\s*(?:\w+\s+)*\*\s*(\w+)\s*\)\s*\(` matched at `open` (which must hold
/// `(`). Returns (name_range, end_after_second_paren). The `(?:\w+\s+)*`
/// group is greedy without backtracking: giving back an iteration repositions
/// `\*` onto a word char, which can never match, so greedy ≡ backtracked.
pub(super) fn fnptr_paren_tail(s: &[u8], open: usize) -> Option<((usize, usize), usize)> {
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
pub(super) fn close_call_tail(s: &[u8], i: usize) -> Option<usize> {
    let mut j = skip_jsws(s, i);
    if s.get(j) == Some(&b')') {
        j = skip_jsws(s, j + 1);
    }
    if s.get(j) == Some(&b'(') {
        return Some(j + 1);
    }
    None
}
