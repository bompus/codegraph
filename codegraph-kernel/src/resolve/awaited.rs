//! ESM awaited-call receiver inference (name-matcher.ts inferEsmAwaitedCallType,
//! resolveAwaitedCallType, importShadowedAt, hasParameterBinding) and the two
//! offset-preserving sanitizers it scans with (strip-comments.ts).
//!
//! JavaScript indexes strings in UTF-16 units; every sanitizer here writes one
//! space per unit it blanks, so a UTF-16 offset means the same position in the
//! raw and the sanitized text. Internally positions are byte offsets into the
//! sanitized text, converted only where a ref's column or a node's column is
//! compared.

use super::*;

/// One file's awaited-binding index. `names` is the raw-source prefilter; the
/// sanitized index is built on the first ref that names one of them.
pub(super) struct AwaitedFile {
    raw_names: HashSet<String>,
    ready: OnceCell<AwaitedIndex>,
}

struct AwaitedIndex {
    code: String,
    names: HashSet<String>,
    /// Byte offset of each line's start.
    offsets: Vec<usize>,
    scopes: Vec<Scope>,
    declarations: HashMap<String, Vec<(usize, usize)>>,
}

struct Scope {
    start: i64,
    end: usize,
    parent: i64,
}

/// What an awaited receiver is known to be: `name: None` is an awaited
/// receiver of unknown type, which must not fall back to an unrelated method.
pub(super) struct AwaitedType {
    pub(super) name: Option<String>,
    pub(super) file_path: String,
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `(?:const|let|var)\s+NAME\s*=\s*await\s+CALLEE\s*\(` over raw or
/// sanitized text — the names such a declaration binds.
fn awaited_names(text: &str) -> HashSet<String> {
    let re = re!(r"(?-u:\b)(?:const|let|var)\s+([A-Za-z0-9_$]+)\s*=\s*await\s+[A-Za-z0-9_$]+\s*\(");
    re.captures_iter(text).map(|c| c[1].to_string()).collect()
}

/// Push `c` blanked: one space per UTF-16 unit, a newline kept.
fn push_blank(out: &mut String, c: char) {
    if c == '\n' {
        out.push('\n');
    } else {
        for _ in 0..c.len_utf16() {
            out.push(' ');
        }
    }
}

/// stripCommentsForRegex(text, 'typescript'): block and line comments
/// blanked (newlines kept); string literals skipped intact.
pub(super) fn strip_ts_comments(src: &str) -> String {
    let s: Vec<char> = src.chars().collect();
    let n = s.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < n {
        let c = s[i];
        let c2 = s.get(i + 1).copied();
        if c == '/' && c2 == Some('*') {
            let start = i;
            i += 2;
            while i < n && !(s[i] == '*' && s.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            for &ch in &s[start..i.min(n)] {
                push_blank(&mut out, ch);
            }
            continue;
        }
        if c == '/' && c2 == Some('/') {
            let start = i;
            while i < n && s[i] != '\n' {
                i += 1;
            }
            for &ch in &s[start..i] {
                push_blank(&mut out, ch);
            }
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            let start = i;
            i += 1;
            while i < n && s[i] != c {
                if s[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if c != '`' && s[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && s[i] == c {
                i += 1;
            }
            out.extend(&s[start..i.min(n)]);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// blankStringContents: string (and template) contents blanked, quotes and
/// newlines kept; a regex literal's body is skipped so its quotes stay data.
pub(super) fn blank_string_contents(text: &str) -> String {
    let s: Vec<char> = text.chars().collect();
    let n = s.len();
    let mut out: Vec<Option<char>> = s.iter().map(|&c| Some(c)).collect();
    let regex_start = re!(
        r"(?:^|[=(:,)!&|?;{}\[\]+*%~^<>-]|(?-u:\b)(?:return|throw|case|yield|await|else|do|typeof|void|delete|new|in|of|instanceof))\s*$"
    );
    let mut i = 0;
    while i < n {
        let c = s[i];
        if c == '/' {
            // `text.slice(max(0, i - 32), i)` in UTF-16 units; a surrogate
            // pair cut in half leaves a lone (non-word, non-space) unit.
            let mut window: Vec<char> = Vec::new();
            let mut units = 0;
            let mut k = i;
            while k > 0 && units < 32 {
                let ch = s[k - 1];
                let w = ch.len_utf16();
                if units + w > 32 {
                    window.push('\u{FFFD}');
                    break;
                }
                window.push(ch);
                units += w;
                k -= 1;
            }
            let window: String = window.into_iter().rev().collect();
            if regex_start.is_match(&window) {
                let mut end = i + 1;
                let mut in_class = false;
                while end < n && s[end] != '\n' {
                    if s[end] == '\\' {
                        end += 2;
                        continue;
                    }
                    if s[end] == '[' {
                        in_class = true;
                    }
                    if s[end] == ']' {
                        in_class = false;
                    }
                    if s[end] == '/' && !in_class {
                        break;
                    }
                    end += 1;
                }
                if end < n && s[end] == '/' {
                    i = end + 1;
                    continue;
                }
            }
        }
        if c == '"' || c == '\'' || c == '`' {
            i += 1;
            while i < n && s[i] != c {
                if s[i] == '\\' && i + 1 < n {
                    out[i] = None;
                    out[i + 1] = None;
                    i += 2;
                    continue;
                }
                if c != '`' && s[i] == '\n' {
                    break;
                }
                if s[i] != '\n' {
                    out[i] = None;
                }
                i += 1;
            }
            if i < n && s[i] == c {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    let mut result = String::with_capacity(text.len());
    for (k, slot) in out.iter().enumerate() {
        match slot {
            Some(ch) => result.push(*ch),
            // `out[i] = ' '` per unit — an escaped `\n` blanks to a space too.
            None => {
                for _ in 0..s[k].len_utf16() {
                    result.push(' ');
                }
            }
        }
    }
    result
}

/// Byte just past `units` UTF-16 units from `from`, clamped to the end.
fn advance_units(s: &str, from: usize, units: usize) -> usize {
    let mut seen = 0;
    for (off, ch) in s[from..].char_indices() {
        if seen >= units {
            return from + off;
        }
        seen += ch.len_utf16();
    }
    s.len()
}

fn skip_ws(s: &str, at: usize) -> usize {
    at + s[at..].len() - s[at..].trim_start().len()
}

/// Every `\bNAME\b` occurrence's byte offset in `hay` (JS ASCII boundaries).
fn word_occurrences<'a>(hay: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    hay.match_indices(name)
        .map(|(at, _)| at)
        .filter(move |&at| word_boundary_at(hay, at) && word_boundary_at(hay, at + name.len()))
}

/// Non-overlapping matches, leftmost first, of
/// `\b(?:KEYWORDS)\s+(?:NAME\b|\{[^}]*\bNAME\b)` (the brace arm only when
/// `braces`): the byte offset of each match's keyword.
fn declaration_matches(code: &str, name: &str, keywords: &[&str], braces: bool) -> Vec<usize> {
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut p = 0;
    while p < bytes.len() {
        let mut matched_end = None;
        if (p == 0 || !is_word(bytes[p - 1])) && is_word(bytes[p]) {
            for kw in keywords {
                if !code[p..].starts_with(kw) {
                    continue;
                }
                let after_kw = p + kw.len();
                let after_ws = skip_ws(code, after_kw);
                if after_ws == after_kw {
                    continue;
                }
                let rest = &code[after_ws..];
                // No `\b` before NAME here — only after it.
                if rest.starts_with(name) && word_boundary_at(code, after_ws + name.len()) {
                    matched_end = Some(after_ws + name.len());
                    break;
                }
                if braces && rest.starts_with('{') {
                    let body_start = after_ws + 1;
                    let body_end = code[body_start..].find('}').map_or(code.len(), |k| body_start + k);
                    // Greedy `[^}]*` backtracks from the right: the last hit.
                    let body = &code[body_start..body_end];
                    if let Some(at) = body
                        .match_indices(name)
                        .map(|(at, _)| body_start + at)
                        .filter(|&at| word_boundary_at(code, at) && word_boundary_at(code, at + name.len()))
                        .last()
                    {
                        matched_end = Some(at + name.len());
                        break;
                    }
                }
            }
        }
        match matched_end {
            Some(end) => {
                out.push(p);
                p = end.max(p + 1);
            }
            None => {
                p += code[p..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    out
}

/// `\bNAME\s*=(?!=)` anywhere in `code`.
fn has_assignment(code: &str, name: &str) -> bool {
    code.match_indices(name).any(|(at, _)| {
        if !word_boundary_at(code, at) {
            return false;
        }
        let eq = skip_ws(code, at + name.len());
        code[eq..].starts_with('=') && !code[eq + 1..].starts_with('=')
    })
}

/// hasParameterBinding: NAME bound by an arrow's bare parameter, or inside a
/// balanced parameter list followed by `=>` or a body `{` (an optional return
/// annotation between). Control-flow parentheses are not parameter lists.
pub(super) fn has_parameter_binding(code: &str, name: &str) -> bool {
    let arrow = code.match_indices(name).any(|(at, _)| {
        word_boundary_at(code, at) && code[skip_ws(code, at + name.len())..].starts_with("=>")
    });
    if arrow {
        return true;
    }
    let after_list = re!(r"^\s*(?::[^=;{]*)?(?:=>|\{)");
    let bytes = code.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] != b'(' {
            continue;
        }
        let before = code[..i].trim_end();
        let control = ["if", "while", "for", "switch", "with"].iter().any(|kw| {
            before.ends_with(kw) && word_boundary_at(before, before.len() - kw.len())
        });
        if control {
            continue;
        }
        let mut depth = 1;
        let mut j = i + 1;
        while j < bytes.len() && depth > 0 {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        if depth == 0
            && word_occurrences(&code[i + 1..j - 1], name).next().is_some()
            && after_list.is_match(&code[j..])
        {
            return true;
        }
    }
    false
}

impl AwaitedIndex {
    fn build(raw: &str) -> AwaitedIndex {
        let code = blank_string_contents(&strip_ts_comments(raw));
        let names = awaited_names(&code);
        let mut offsets = vec![0];
        let mut scopes = vec![Scope { start: -1, end: code.len(), parent: -1 }];
        let mut stack = vec![0usize];
        for (i, b) in code.bytes().enumerate() {
            match b {
                b'\n' => offsets.push(i + 1),
                b'{' => {
                    scopes.push(Scope { start: i as i64, end: code.len(), parent: *stack.last().unwrap() as i64 });
                    stack.push(scopes.len() - 1);
                }
                b'}' if stack.len() > 1 => {
                    let top = stack.pop().unwrap();
                    scopes[top].end = i;
                }
                _ => {}
            }
        }
        let mut declarations: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        let decl = re!(r"(?-u:\b)(?:const|let|var)\s+([A-Za-z0-9_$]+)\s*=\s*");
        for c in decl.captures_iter(&code) {
            if names.contains(&c[1]) {
                let m = c.get(0).unwrap();
                declarations.entry(c[1].to_string()).or_default().push((m.start(), m.len()));
            }
        }
        AwaitedIndex { code, names, offsets, scopes, declarations }
    }

    fn scope_at(&self, offset: usize) -> usize {
        let (mut lo, mut hi) = (0usize, self.scopes.len());
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.scopes[mid].start < offset as i64 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        while lo > 0 && self.scopes[lo].end < offset {
            lo = self.scopes[lo].parent as usize;
        }
        lo
    }

    fn visible_at(&self, declaration: usize, use_at: usize) -> bool {
        let ancestor = self.scope_at(declaration) as i64;
        let mut scope = self.scope_at(use_at) as i64;
        while scope >= 0 {
            if scope == ancestor {
                return true;
            }
            scope = self.scopes[scope as usize].parent;
        }
        false
    }

    /// 1-based line of a byte offset, and its UTF-16 column.
    fn line_col(&self, at: usize) -> (i64, i64) {
        let line = self.offsets.partition_point(|&o| o <= at);
        let start = self.offsets[line - 1];
        (line as i64, utf16_len(&self.code[start..at]) as i64)
    }
}

impl KernelResolver {
    /// inferEsmAwaitedCallType — `None` means no awaited evidence.
    pub(super) fn infer_esm_awaited_call_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<AwaitedType>> {
        if !re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(receiver) {
            return Ok(None);
        }
        let file = match self.awaited_files.get(&r.file_path) {
            Some(f) => f.clone(),
            None => {
                let raw_names = self
                    .read_file(&r.file_path)
                    .map(|lines| awaited_names(lines.text()))
                    .unwrap_or_default();
                let f = (!raw_names.is_empty())
                    .then(|| Rc::new(AwaitedFile { raw_names, ready: OnceCell::new() }));
                if self.awaited_files.len() >= 256 {
                    self.awaited_files.clear();
                }
                self.awaited_files.insert(r.file_path.clone(), f.clone());
                f
            }
        };
        let Some(file) = file else { return Ok(None) };
        if !file.raw_names.contains(receiver) {
            return Ok(None);
        }
        if file.ready.get().is_none() {
            let text = self.read_file(&r.file_path).map(|l| l.text().to_string()).unwrap_or_default();
            let _ = file.ready.set(AwaitedIndex::build(&text));
        }
        let idx = file.ready.get().unwrap();
        if !idx.names.contains(receiver) {
            return Ok(None);
        }
        self.resolve_awaited_call_type(receiver, idx, r)
    }

    fn resolve_awaited_call_type(
        &mut self,
        receiver: &str,
        file: &AwaitedIndex,
        r: &ResolveRefIn,
    ) -> Res<Option<AwaitedType>> {
        let unknown = || Ok(Some(AwaitedType { name: None, file_path: r.file_path.clone() }));
        let base = usize::try_from(r.line - 1)
            .ok()
            .and_then(|l| file.offsets.get(l).copied())
            .unwrap_or(file.code.len());
        let end = advance_units(&file.code, base.min(file.code.len()), r.column.max(0) as usize);
        let code = &file.code[..end];
        let Some(&(index, length)) = file
            .declarations
            .get(receiver)
            .and_then(|d| d.iter().rev().find(|&&(i, _)| i < end && file.visible_at(i, end)))
        else {
            return Ok(None);
        };
        let init = code.get(index + length..).unwrap_or("");
        if !(init.starts_with("await") && word_boundary_at(init, 5)) {
            return Ok(None);
        }
        let Some(call) = re!(r"^await\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*\(").captures(init) else {
            return Ok(None);
        };
        let callee = call[1].to_string();
        let ib = init.as_bytes();
        let mut depth = 1;
        let mut call_end = call.get(0).unwrap().end();
        while call_end < ib.len() && depth > 0 {
            match ib[call_end] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            call_end += 1;
        }
        if depth > 0 {
            return unknown();
        }
        let tail = &init[call_end..];
        // `^[ \t]*(?:;|\r?\n(?![ \t]*[.(\[?]))` — a following property,
        // index or call is not the callee's annotated value.
        let after = tail.trim_start_matches([' ', '\t']);
        let bare = if after.starts_with(';') {
            true
        } else {
            let nl = after.strip_prefix("\r\n").or_else(|| after.strip_prefix('\n'));
            nl.is_some_and(|next| {
                !next.trim_start_matches([' ', '\t']).starts_with(['.', '(', '[', '?'])
            })
        };
        if !bare {
            return unknown();
        }
        if !declaration_matches(tail, receiver, &["const", "let", "var", "function", "class"], true).is_empty()
            || has_assignment(tail, receiver)
            || has_parameter_binding(tail, receiver)
        {
            return unknown();
        }

        let (binding_line, binding_col) = file.line_col(index);
        let mut binding_ref = r.clone();
        binding_ref.line = binding_line;
        binding_ref.column = binding_col;
        for n in self.nodes_in_file(&r.file_path)?.iter() {
            if (n.kind == "function" || n.kind == "method")
                && n.start_line <= binding_line
                && n.end_line >= binding_line
                && n.signature.as_deref().is_some_and(|s| has_parameter_binding(&format!("{s} {{"), &callee))
            {
                return unknown();
            }
        }

        let imported = self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == callee);
        let declaring: Option<Arc<KNode>> = if imported {
            if self.import_shadowed_at(&callee, &binding_ref)? {
                return unknown();
            }
            let mut call_ref = binding_ref.clone();
            call_ref.reference_name = callee.clone();
            call_ref.reference_kind = "calls".to_string();
            self.resolve_via_import(&call_ref)?.map(|c| c.node)
        } else {
            let mut local = Vec::new();
            for n in self.nodes_by_name(&callee)?.iter() {
                if n.kind == "function"
                    && n.file_path == r.file_path
                    && is_esm_family(&n.language)
                    && self.is_lexically_reachable(n, &binding_ref)?
                {
                    local.push(n.clone());
                }
            }
            (local.len() == 1).then(|| local.pop().unwrap())
        };
        let Some(declaring) = declaring.filter(|d| d.kind == "function") else {
            return unknown();
        };
        let Some(signature) = declaring.signature.clone() else {
            return unknown();
        };
        if !imported {
            for at in declaration_matches(&code[..index], &callee, &["const", "let", "var"], false) {
                if !file.visible_at(at, index) {
                    continue;
                }
                // A typed arrow function may itself be the declared local factory.
                let (line, col) = file.line_col(at);
                if line != declaring.start_line || col > declaring.start_column {
                    return unknown();
                }
            }
        }
        let after_params = &signature[signature.rfind(')').map_or(0, |p| p + 1)..];
        let Some(annotation) = re!(r"^\s*:\s*([\s\S]+)$")
            .captures(after_params)
            .map(|c| c[1].trim().to_string())
            .filter(|a| !a.is_empty())
        else {
            return unknown();
        };
        // One named Promise<T> layer, else the annotation itself.
        let returned = re!(r"^Promise\s*<\s*([A-Za-z0-9_$]+)\s*>$")
            .captures(&annotation)
            .map_or(annotation.clone(), |c| c[1].to_string());
        if !re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(&returned) {
            return unknown();
        }
        if TS_PRIMITIVE_TYPES.contains(returned.as_str()) {
            return Ok(Some(AwaitedType { name: Some(returned), file_path: declaring.file_path.clone() }));
        }

        let type_import = self
            .import_mappings(&declaring.file_path)?
            .iter()
            .any(|m| m.local_name == returned);
        let type_node = if type_import {
            let mut type_ref = binding_ref.clone();
            type_ref.from_node_id = declaring.id.clone();
            type_ref.file_path = declaring.file_path.clone();
            type_ref.language = declaring.language.clone();
            type_ref.line = declaring.start_line;
            type_ref.column = declaring.start_column;
            type_ref.reference_name = returned.clone();
            type_ref.reference_kind = "references".to_string();
            self.resolve_via_import(&type_ref)?.map(|c| c.node)
        } else {
            self.nodes_by_name(&returned)?
                .iter()
                .find(|n| {
                    n.file_path == declaring.file_path
                        && is_esm_family(&n.language)
                        && (n.kind == "class" || n.kind == "interface")
                })
                .cloned()
        };
        match type_node {
            Some(t) if t.kind == "class" || t.kind == "interface" => {
                Ok(Some(AwaitedType { name: Some(t.name.clone()), file_path: t.file_path.clone() }))
            }
            _ => unknown(),
        }
    }

    /// importShadowedAt: a parameter of an enclosing function, or a nearer
    /// declaration in an enclosing block, shadows the import binding here.
    pub(super) fn import_shadowed_at(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        for f in self.nodes_in_file(&r.file_path)?.iter() {
            if (f.kind == "function" || f.kind == "method")
                && f.start_line <= r.line
                && f.end_line >= r.line
                && f.signature.as_deref().is_some_and(|s| has_parameter_binding(&format!("{s} {{"), name))
            {
                return Ok(true);
            }
        }
        let Some(lines) = self.read_file(&r.file_path) else {
            // `readFile ?? ''` — one empty line, nothing declared.
            return Ok(false);
        };
        let cut = (r.line - 1).max(0) as usize;
        let mut before = lines[..cut.min(lines.len())].join("\n");
        if cut > 0 {
            before.push('\n');
        }
        if let Some(l) = lines.get(cut) {
            before.push_str(js_prefix(l, r.column.max(0) as usize));
        }
        let code = blank_string_contents(&strip_ts_comments(&before));
        let matches = declaration_matches(&code, name, &["const", "let", "var", "function", "class"], true);
        if matches.is_empty() {
            return Ok(false);
        }
        // Each match's brace stack must be a prefix of the stack at the end.
        let bytes = code.as_bytes();
        let mut stack: Vec<usize> = Vec::new();
        let mut at_match: Vec<Vec<usize>> = Vec::with_capacity(matches.len());
        let mut next = 0;
        for (i, &b) in bytes.iter().enumerate() {
            while next < matches.len() && matches[next] == i {
                at_match.push(stack.clone());
                next += 1;
            }
            if b == b'{' {
                stack.push(i);
            } else if b == b'}' {
                stack.pop();
            }
        }
        Ok(at_match.iter().any(|s| s.iter().enumerate().all(|(i, p)| stack.get(i) == Some(p))))
    }
}
