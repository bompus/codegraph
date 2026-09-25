//! The per-file extraction sweep: typedef, struct, initializer, alias, assignment, dispatch and include scans, struct field parsing, and `scan_file`.

use super::*;

// ---------- scanners ----------

/// FNPTR_TYPEDEF_RE: /\btypedef\b[^;{}]*?\(\s*(?:\w+\s+)*\*\s*(\w+)\s*\)\s*\(/g
pub(super) fn scan_fnptr_typedefs(s: &[u8], out: &mut Vec<String>) {
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
pub(super) fn scan_fntype_typedefs(s: &[u8], out: &mut Vec<String>) {
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
pub(super) struct InlineScan {
    pub(super) ptr: bool,
    pub(super) types: Vec<String>,
    pub(super) tags: Vec<String>,
}

pub(super) fn scan_inline_structs(s: &[u8]) -> InlineScan {
    let mut out = InlineScan { ptr: false, types: Vec::new(), tags: Vec::new() };
    let mut last = 0;
    // Each keyword's next occurrence is searched once and kept until the
    // scan passes it: a hit at or beyond `last` is still the first hit from
    // `last`, so only the keyword the loop consumed is re-searched (the old
    // shape re-scanned BOTH to end of file on every iteration — O(n·k) on a
    // header with many `struct`s and no `union`).
    let mut next_struct = find_word(s, b"struct", 0);
    let mut next_union = find_word(s, b"union", 0);
    loop {
        if next_struct.is_some_and(|p| p < last) {
            next_struct = find_word(s, b"struct", last);
        }
        if next_union.is_some_and(|p| p < last) {
            next_union = find_word(s, b"union", last);
        }
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
pub(super) fn match_brace(s: &[u8], open: usize) -> Option<usize> {
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
pub(super) fn modifier_positions(s: &[u8], start: usize) -> Vec<usize> {
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
pub(super) fn bracket_span(s: &[u8], i: usize) -> Option<usize> {
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
pub(super) fn scan_anchored<F>(s: &[u8], mut body: F, out: &mut Vec<String>)
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
pub(super) fn init_body(s: &[u8], p: usize) -> Option<(String, usize)> {
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
pub(super) fn array_table_body(s: &[u8], p: usize) -> Option<(String, usize)> {
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
pub(super) fn scan_alias_names(stripped: &[u8], out: &mut Vec<String>) {
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
pub(super) fn skip_sp_tab(line: &[u8], mut i: usize) -> usize {
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
        i += 1;
    }
    i
}

pub(super) fn alias_line(line: &[u8]) -> Option<&[u8]> {
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
pub(super) fn scan_field_assign(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while let Some(m) = next_field_assign(s, pos) {
        let mut pair = bytes_to_string(&s[m.lfield.0..m.lfield.1]);
        pair.push('\0');
        pair.push_str(&bytes_to_string(&s[m.rfield.0..m.rfield.1]));
        out.push(pair);
        pos = m.end;
    }
}

#[inline]
pub(super) fn arrow_at(s: &[u8], i: usize) -> Option<usize> {
    if s.get(i) == Some(&b'-') && s.get(i + 1) == Some(&b'>') {
        Some(i + 2)
    } else if s.get(i) == Some(&b'.') {
        Some(i + 1)
    } else {
        None
    }
}

pub(super) type Range = (usize, usize);

/// FN_ASSIGN_RE: /(\w+)\s*(?:->|\.)\s*(\w+)\s*=(?!=)\s*&?\s*(\w+)\s*;/g
/// Collects the LHS field (second capture) of each `x->f = fn;` — the
/// bare-function-assignment registration filter. `a->f = b->g` can't match
/// (the RHS word must be followed by `;`), a bare `fp = fn` has no field
/// access, and `(?!=)` keeps `==` out. Every byte position is a candidate
/// start (JS advances one unit on failure); matches resume at their end.
pub(super) fn scan_fn_assign(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while pos < s.len() {
        if !is_word(s[pos]) {
            pos += 1;
            continue;
        }
        match fn_assign_at(s, pos) {
            Some((f, end)) => {
                push_str(out, &s[f.0..f.1]);
                pos = end;
            }
            None => pos += 1,
        }
    }
}

/// `\s*=(?!=)\s*&?\s*(\w+)\s*;` — the shared bare-function RHS tail of
/// FN_ASSIGN_RE / DEREF_FN_ASSIGN_RE, starting just past the LHS field word.
/// Returns the match end (one past the `;`).
pub(super) fn assign_rhs_tail(s: &[u8], i: usize) -> Option<usize> {
    let eq = skip_jsws(s, i);
    // `=(?!=)` — the `=` consumed, a second `=` rejected.
    if s.get(eq) != Some(&b'=') || s.get(eq + 1) == Some(&b'=') {
        return None;
    }
    let mut r1s = skip_jsws(s, eq + 1);
    if s.get(r1s) == Some(&b'&') {
        r1s = skip_jsws(s, r1s + 1);
    }
    if !is_word_at(s, r1s) {
        return None;
    }
    let r1e = word_end(s, r1s);
    let semi = skip_jsws(s, r1e);
    if s.get(semi) != Some(&b';') {
        return None;
    }
    Some(semi + 1)
}

/// `x->f` / `x.f` LHS of FN_ASSIGN_RE at `p` (a word start): returns the
/// field-word range and the match end.
pub(super) fn fn_assign_at(s: &[u8], p: usize) -> Option<(Range, usize)> {
    let w1 = word_end(s, p);
    let a1 = arrow_at(s, skip_jsws(s, w1))?;
    let f1s = skip_jsws(s, a1);
    if !is_word_at(s, f1s) {
        return None;
    }
    let f1e = word_end(s, f1s);
    let end = assign_rhs_tail(s, f1e)?;
    Some(((f1s, f1e), end))
}

/// DEREF_FN_ASSIGN_RE:
/// /\(\s*\*\s*(\w+)\s*\)\s*(?:->|\.)\s*(\w+)\s*=(?!=)\s*&?\s*(\w+)\s*;/g
/// The dereference-receiver form `(*x)->f = fn;` / `(*x).f = fn;`. Collects
/// the LHS field (second capture). Candidate starts are `(` positions only.
pub(super) fn scan_deref_fn_assign(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while pos < s.len() {
        if s[pos] != b'(' {
            pos += 1;
            continue;
        }
        match deref_fn_assign_at(s, pos) {
            Some((f, end)) => {
                push_str(out, &s[f.0..f.1]);
                pos = end;
            }
            None => pos += 1,
        }
    }
}

pub(super) fn deref_fn_assign_at(s: &[u8], p: usize) -> Option<(Range, usize)> {
    // p is the `(`.
    let star = skip_jsws(s, p + 1);
    if s.get(star) != Some(&b'*') {
        return None;
    }
    let rs = skip_jsws(s, star + 1);
    if !is_word_at(s, rs) {
        return None;
    }
    let re = word_end(s, rs);
    let cp = skip_jsws(s, re);
    if s.get(cp) != Some(&b')') {
        return None;
    }
    let a1 = arrow_at(s, skip_jsws(s, cp + 1))?;
    let f1s = skip_jsws(s, a1);
    if !is_word_at(s, f1s) {
        return None;
    }
    let f1e = word_end(s, f1s);
    let end = assign_rhs_tail(s, f1e)?;
    Some(((f1s, f1e), end))
}

/// DISPATCH_RE: /((?:\w+(?:\s*\[[^\][]*\])?\s*(?:->|\.)\s*)+)(\w+)\s*\)?\s*\(/g
/// The `+` loop is consumed greedily, then the field tail is tried at each
/// segment count k descending — the JS engine's observable backtracking. The
/// per-segment optional subscript needs no cross-product: the with/without
/// parses diverge at the arrow and at most one can complete a segment.
pub(super) fn scan_dispatch(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while let Some(m) = next_dispatch(s, pos) {
        push_str(out, &s[m.field.0..m.field.1]);
        pos = m.end;
    }
}

/// `\[[^\][]*\]` at `i` (the DISPATCH subscript form — no nested brackets):
/// position after `]`, or None.
pub(super) fn subscript_span(s: &[u8], i: usize) -> Option<usize> {
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
pub(super) fn arrow_tail(s: &[u8], i: usize) -> Option<usize> {
    let a = arrow_at(s, skip_jsws(s, i))?;
    Some(skip_jsws(s, a))
}

/// ARRAY_DISPATCH_RE: /(?:\(\s*\*\s*)?\b(\w+)\s*\[[^\][]*\]\s*\)?\s*\(/g
pub(super) fn scan_array_dispatch(s: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while let Some(m) = next_array_dispatch(s, pos) {
        push_str(out, &s[m.name.0..m.name.1]);
        pos = m.end;
    }
}

/// INCLUDE_RE over RAW text: /#[ \t]*include[ \t]+"([^"\n]+)"/g
pub(super) fn scan_includes(raw: &[u8], out: &mut Vec<String>) {
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
pub(super) fn split_top_level(body: &[u8], sep: u8) -> Vec<Range> {
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

/// /(\w+)\s+\**\s*(\w+)\s*$/ — leftmost match whose tail reaches the end.
/// Deterministic per start (greedy words/ws cannot backtrack usefully);
/// candidate starts advance one byte at a time like the JS engine.
pub(super) fn first_typed(part: &[u8]) -> Option<(Range, Range)> {
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
pub(super) fn fnptr_decl(part: &[u8]) -> Option<Range> {
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
        let (ds, de) = js_trim(inner, ds, de);
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
            let (ps2, pe2) = js_trim(decl, ps, pe);
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

pub(super) fn push_str(out: &mut Vec<String>, bytes: &[u8]) {
    out.push(bytes_to_string(bytes));
}

#[inline]
pub(super) fn bytes_to_string(bytes: &[u8]) -> String {
    // All slice boundaries land on ASCII delimiters, so the content is valid
    // UTF-8 whenever the input string was; lossy keeps us total anyway.
    String::from_utf8_lossy(bytes).into_owned()
}

pub(super) fn dedup_in_order(v: Vec<String>) -> Vec<String> {
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
pub(super) fn line_starts(s: &[u8]) -> Vec<usize> {
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
        assign_fields: Vec::new(),
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
        // Bare-function field assignment: FN_ASSIGN_RE runs over the whole
        // file first, then DEREF_FN_ASSIGN_RE — same Set-insertion order as
        // the JS sweep's `assignFields` collector.
        scan_fn_assign(s, &mut facts.assign_fields);
        scan_deref_fn_assign(s, &mut facts.assign_fields);
        facts.assign_fields = dedup_in_order(std::mem::take(&mut facts.assign_fields));
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
