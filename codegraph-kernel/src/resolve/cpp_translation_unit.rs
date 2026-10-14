//! What only another C or C++ translation unit can name: a type or alias a source file declares,
//! a member of a class only source files declare, and what a source file's unnamed namespace
//! (`namespace { … }`) declares. rocksdb's test fixture `ColumnFamilyTestBase` is declared in
//! column_family_test.cc, and other test files' bare `Put(…)` reached its `Put` by name;
//! inlineskiplist.h's `Node*` reached hash_linklist_rep.cc's own `Node`.
//!
//! Port of upstream's `isCppTranslationUnitLocal` and `cppVisibleFirst` (name-matcher.ts at
//! 61cb4188, #2538); read the original with `git show 61cb4188:src/resolution/name-matcher.ts`.
//! What a source file declares is still seen by the file itself, by a file that includes it,
//! directly or through others (a test that includes the source it tests), and by a header it
//! includes below the declaration (rocksdb's version_set.cc includes version_set_sync_and_async.h
//! after its `FilePicker` classes).
use super::tables::c_source_ext_re;
use super::*;

/// What a C or C++ type declaration can be: the kinds a translation-unit-local type has.
fn cpp_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "union" | "enum" | "interface" | "type_alias")
}

fn cpp_class_kind(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "union")
}

/// A position in a file: its line (1-based) and its column (UTF-16 units from the line's start, 0-based).
pub(super) type Position = (i64, i64);

/// The spans of the unnamed namespaces `text` writes, the outermost of nested ones, from their
/// `{` to their `}`: what one declares has internal linkage, so no other translation unit names
/// it. Comments, preprocessor lines and string, character and raw string literals are skipped.
pub(super) fn unnamed_namespace_spans(text: &str) -> Vec<(Position, Position)> {
    let mut spans = Vec::new();
    if !re!(r"\bnamespace\s*\{").is_match(text) { return spans; }
    let bytes = text.as_bytes();
    // Each open brace: where it is, and whether it opens an unnamed namespace.
    let mut open: Vec<(usize, bool)> = Vec::new();
    let mut offsets: Vec<(usize, usize)> = Vec::new();
    let mut line_start = true;
    // The code since the last brace or `;`, without comments and literals.
    let mut code = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            line_start = true;
            code.push(' ');
            i += 1;
            continue;
        }
        if line_start && c == b'#' {
            // A preprocessor line, continued by a trailing backslash.
            let mut end = i;
            while end < bytes.len() && (bytes[end] != b'\n' || (end > 0 && bytes[end - 1] == b'\\') || (end > 1 && bytes[end - 1] == b'\r' && bytes[end - 2] == b'\\')) {
                end += 1;
            }
            i = end;
            continue;
        }
        // (A comment is a space: a directive may follow one that starts its line.)
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + 2 + n + 2);
            continue;
        }
        if !c.is_ascii_whitespace() { line_start = false; }
        if c == b'"' && {
            let from = (i.saturating_sub(4)..i).find(|&k| text.is_char_boundary(k)).unwrap_or(i);
            re!(r"(?:^|[^\w$])(?:u8|[uUL])?R$").is_match(&text[from..i])
        } {
            // `R"delim( … )delim"`.
            i = match text[i..].find('(') {
                Some(p) => {
                    let paren = i + p;
                    let closing = format!("){}\"", &text[i + 1..paren]);
                    text[paren..].find(&closing).map_or(bytes.len(), |n| paren + n + closing.len())
                }
                None => bytes.len(),
            };
            continue;
        }
        if c == b'"' || c == b'\'' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != c && bytes[j] != b'\n' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            i = j + 1;
            continue;
        }
        match c {
            b'{' => {
                open.push((i, re!(r"(?:^|[^\w$])namespace\s*$").is_match(&code)));
                code.clear();
            }
            b'}' => {
                if let Some((start, unnamed)) = open.pop() {
                    if unnamed && !open.iter().any(|(_, u)| *u) { offsets.push((start, i)); }
                }
                code.clear();
            }
            b';' => code.clear(),
            _ => {
                // (Only the code's end is read; keep it short.)
                if code.len() > 200 { code.drain(..code.len() - 100); }
                if c.is_ascii() { code.push(c as char); } else { code.push('_'); }
            }
        }
        i += 1;
    }
    if offsets.is_empty() { return spans; }
    let mut line_starts = vec![0usize];
    line_starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
    let position = |offset: usize| -> Position {
        let line = line_starts.partition_point(|&s| s <= offset) - 1;
        (line as i64 + 1, text[line_starts[line]..offset].encode_utf16().count() as i64)
    };
    spans.extend(offsets.into_iter().map(|(from, to)| (position(from), position(to))));
    spans
}

impl KernelResolver {
    /// Whether only another C or C++ translation unit than `r`'s can name `candidate`.
    pub(super) fn cpp_translation_unit_local(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if !matches!(r.language.as_str(), "c" | "cpp") { return Ok(false); }
        if cpp_type_kind(&candidate.kind) { return Ok(!self.cpp_seen_from(candidate, &r.file_path)?); }
        // A member written in its class's declaration is that declaration's; one defined out of
        // line is the class's, wherever a declaration of it its own file sees is (through an
        // alias, the class may be declared anywhere). A scope its file sees no type of may be a
        // namespace: fmt's posix-mock-test.cc defines `test::close` of posix-mock.h's `namespace
        // test`, and ostream-test.cc declares a `struct test` of its own.
        if let Some(enclosing) = self.cpp_enclosing_class_decl(candidate)? {
            return Ok(!self.cpp_seen_from(&enclosing, &r.file_path)?);
        }
        let scope = match candidate.qualified_name.rfind("::") { Some(cut) => &candidate.qualified_name[..cut], None => "" };
        if !scope.is_empty() {
            let mut decls = Vec::new();
            for d in self.nodes_by_qualified_name(scope)?.iter() {
                if cpp_type_kind(&d.kind) && matches!(d.language.as_str(), "c" | "cpp") && self.cpp_seen_from(d, &candidate.file_path)? { decls.push(d.clone()); }
            }
            if decls.iter().any(|d| d.kind == "type_alias") { return Ok(false); }
            if !decls.is_empty() {
                for d in &decls {
                    if self.cpp_seen_from(d, &r.file_path)? { return Ok(false); }
                }
                return Ok(true);
            }
        }
        // A source file's unnamed namespace is its own and its includers'; a header's is the
        // includers' of that header, so another file's `Helper` is not the includer's.
        if c_source_ext_re().is_match(&candidate.file_path) && self.cpp_seen_from(candidate, &r.file_path)? { return Ok(false); }
        let Some(source) = self.read_file(&candidate.file_path) else { return Ok(false) };
        let at = (candidate.start_line, candidate.start_column);
        if !source.cpp_unnamed_namespaces().iter().any(|&(open, close)| at > open && at < close) { return Ok(false); }
        Ok(!self.cpp_file_reaches(candidate, &r.file_path)?)
    }

    /// Whether the declaration `decl` is seen from `file`: it is no source file's, or `file` is its
    /// own, includes its file, or is a header its file includes below it.
    fn cpp_seen_from(&mut self, decl: &KNode, file: &str) -> Res<bool> {
        if !c_source_ext_re().is_match(&decl.file_path) { return Ok(true); }
        self.cpp_file_reaches(decl, file)
    }

    /// Whether `file` is the file of `decl`, includes it, or is a header it includes below it.
    fn cpp_file_reaches(&mut self, decl: &KNode, file: &str) -> Res<bool> {
        if decl.file_path == file { return Ok(true); }
        if self.namespace_visible_files(file, &decl.language)?.contains(&decl.file_path) { return Ok(true); }
        self.cpp_includes_after(&decl.file_path, decl.start_line, file, &decl.language)
    }

    /// Whether the C or C++ file `source` `#include`s `header` below line `line`.
    fn cpp_includes_after(&mut self, source: &str, line: i64, header: &str, language: &str) -> Res<bool> {
        let includes: Vec<_> = self.nodes_in_file(source)?.iter()
            .filter(|n| n.kind == "import" && n.start_line > line && matches!(n.language.as_str(), "c" | "cpp")).cloned().collect();
        for node in includes {
            let delimiter = if node.signature.as_deref().is_some_and(|s| s.contains('<')) { '<' } else { '"' };
            if self.resolve_cpp_include(source, delimiter, &node.name, language)?.as_deref() == Some(header) { return Ok(true); }
        }
        Ok(false)
    }

    /// The innermost C or C++ class, struct or union declaration of its file whose lines hold `member`.
    fn cpp_enclosing_class_decl(&mut self, member: &KNode) -> Res<Option<Arc<KNode>>> {
        let mut innermost: Option<Arc<KNode>> = None;
        for cls in self.nodes_in_file(&member.file_path)?.iter() {
            if !cpp_class_kind(&cls.kind) || cls.id == member.id || member.start_line < cls.start_line || member.start_line > cls.end_line { continue; }
            if innermost.as_ref().is_none_or(|i| cls.start_line >= i.start_line) { innermost = Some(cls.clone()); }
        }
        Ok(innermost)
    }

    /// Of the methods a C or C++ type's name gives, those `r`'s translation unit can see, when
    /// there are any (cppVisibleFirst). Classes of one qualified name can be declared apart:
    /// rocksdb's db_stress_shared_state.h and two benchmark `.cc` files each declare a
    /// `SharedState`, and a call in db_stress_common.cc is the header's.
    pub(super) fn cpp_visible_first(&mut self, methods: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        if methods.len() < 2 || !matches!(r.language.as_str(), "c" | "cpp") { return Ok(methods); }
        let mut visible = Vec::new();
        for m in &methods {
            if !self.cpp_translation_unit_local(m, r)? { visible.push(m.clone()); }
        }
        Ok(if visible.is_empty() { methods } else { visible })
    }
}

#[cfg(test)]
mod tests {
    use super::unnamed_namespace_spans;

    #[test]
    fn unnamed_namespaces_past_comments_and_literals() {
        let text = "namespace a {\nnamespace {\nint f();\nnamespace {\n}\n}\n// namespace {\n/* namespace { */\n#define X namespace {\nconst char* s = \"namespace {\";\nconst char* r = R\"(a \"namespace {\" b)\";\n}\n";
        assert_eq!(unnamed_namespace_spans(text), vec![((2, 10), (6, 0))]);
        assert_eq!(unnamed_namespace_spans("#define B \\\n  namespace {\nint g();\n"), vec![]);
    }

    #[test]
    fn a_directive_follows_a_comment_and_a_span_ends_at_its_brace() {
        assert_eq!(unnamed_namespace_spans("/* note */ #define X namespace {\nclass A {};\n"), vec![]);
        assert_eq!(unnamed_namespace_spans("namespace { class A {}; } class B {};\n"), vec![((1, 10), (1, 24))]);
        assert_eq!(unnamed_namespace_spans("// é\nnamespace { int é; }\n"), vec![((2, 10), (2, 19))]);
    }
}
