//! Stages D and E: the field-propagation fixpoint and the dispatch edges.

use super::*;

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

pub(super) struct Tabs<'t> {
    pub(super) field_to_structs: std::collections::HashMap<&'t str, &'t Vec<String>>,
    pub(super) layout: std::collections::HashMap<&'t str, &'t Vec<LinkField>>,
    pub(super) all_fields: std::collections::HashMap<&'t str, &'t Vec<Vec<LinkField>>>,
    pub(super) global_var_type: std::collections::HashMap<&'t str, &'t str>,
    pub(super) reg: std::collections::HashMap<String, Vec<String>>,
    pub(super) array_reg: std::collections::HashMap<&'t str, &'t Vec<LinkArrEntry>>,
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
pub(super) fn recv_decl_types<'b>(body: &'b [u8], recv: &'b [u8]) -> impl Iterator<Item = &'b [u8]> {
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
pub(super) fn recv_type_in<'b>(body: &'b [u8], recv: &'b [u8], tabs: &Tabs) -> Option<&'b str> {
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
pub(super) fn var_type_in<'t>(body: &[u8], v: &[u8], tabs: &'t Tabs<'t>) -> Option<String> {
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
pub(super) fn chain_segs(chain: &[u8]) -> Vec<&[u8]> {
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
pub(super) fn resolve_chain_type(body: &[u8], chain: &[u8], tabs: &Tabs) -> Option<String> {
    let segs = chain_segs(chain);
    let mut t = var_type_in(body, segs.first()?, tabs)?;
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
pub(super) fn fn_ptr_field_of(struct_name: &str, field: &[u8], tabs: &Tabs) -> bool {
    tabs.layout
        .get(struct_name)
        .is_some_and(|fields| fields.iter().any(|f| f.name.as_bytes() == field && f.is_fn_ptr))
}

pub(super) fn owners_has(owners: &[String], t: &str) -> bool {
    owners.iter().any(|o| o == t)
}

/// FIELD_ASSIGN_RE match with all four captures:
/// `(\w+)\s*(?:->|\.)\s*(\w+)\s*=\s*(\w+)\s*(?:->|\.)\s*(\w+)`
pub(super) struct FaMatch {
    pub(super) lrecv: Range,
    pub(super) lfield: Range,
    pub(super) rrecv: Range,
    pub(super) rfield: Range,
    pub(super) end: usize,
}

pub(super) fn field_assign_match(s: &[u8], p: usize) -> Option<FaMatch> {
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
pub(super) struct DsMatch {
    pub(super) mstart: usize,
    pub(super) chain: Range,
    pub(super) field: Range,
    pub(super) end: usize,
}

pub(super) fn dispatch_match(s: &[u8], p: usize) -> Option<DsMatch> {
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
pub(super) struct AdMatch {
    pub(super) mstart: usize,
    pub(super) name: Range,
    pub(super) end: usize,
}

pub(super) fn array_dispatch_match(s: &[u8], p: usize) -> Option<AdMatch> {
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
pub(super) fn body_slice<'s>(s: &'s [u8], lines: &[usize], start_line: i64, end_line: i64) -> &'s [u8] {
    if start_line < 1 {
        return &s[0..0];
    }
    let a = (start_line - 1) as usize;
    // JS `slice(a, b)`: a negative end counts back from the array's end (the
    // TS caller never sends one — `endLine ?? startLine ?? 0` — but a raw
    // `as usize` would wrap it into "the whole tail of the file").
    let b = if end_line < 0 {
        (lines.len() as i64 + end_line).max(0) as usize
    } else {
        end_line as usize
    };
    if a >= lines.len() || b <= a {
        return &s[0..0];
    }
    let body_start = lines[a];
    let body_end = if b < lines.len() { lines[b] - 1 } else { s.len() };
    &s[body_start..body_end.max(body_start)]
}

/// `exec`-style scan for FIELD_ASSIGN_RE: candidate at every word byte, a
/// failed candidate advances by one, a match resumes at its end.
pub(super) fn next_field_assign(s: &[u8], mut pos: usize) -> Option<FaMatch> {
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
pub(super) fn next_dispatch(s: &[u8], mut pos: usize) -> Option<DsMatch> {
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
pub(super) fn next_array_dispatch(s: &[u8], mut pos: usize) -> Option<AdMatch> {
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
pub(super) fn link_propagate_file(text: &str, f: &LinkFile, tabs: &Tabs, out: &mut Vec<(String, String)>) {
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

pub(super) const FANOUT_CAP: usize = 300;

/// Stage E for one file — dispatch sites → edges. `seen` is the JS pass's
/// global dedupe, but a `fn.id > tid` key can't collide across files (node
/// ids are file-scoped), so a per-file set reproduces it exactly.
pub(super) fn link_dispatch_file(text: &str, f: &LinkFile, tabs: &Tabs, out: &mut Vec<LinkEdge>) {
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

pub(super) fn read_text(abs: &str) -> Option<String> {
    std::fs::read(abs)
        .ok()
        .map(|b| String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
        .filter(|t| !t.is_empty())
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
    let propagations: Vec<(String, String)> = crate::par_map(&prop, |f| {
        let mut v = Vec::new();
        if let Some(t) = read_text(&f.abs) {
            link_propagate_file(&t, f, tabs_ref, &mut v);
        }
        v
    }, Vec::new)
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
    crate::par_map(&disp, |f| {
        let mut v = Vec::new();
        if let Some(t) = read_text(&f.abs) {
            link_dispatch_file(&t, f, tabs_ref, &mut v);
        }
        v
    }, Vec::new)
    .into_iter()
    .flatten()
    .collect()
}
