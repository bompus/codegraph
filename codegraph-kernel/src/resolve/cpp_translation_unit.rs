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
use super::cpp_implicit_this::Receiver;
use super::tables::c_source_ext_re;
use super::*;

/// What a C or C++ type declaration can be: the kinds a translation-unit-local type has.
fn cpp_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "union" | "enum" | "interface" | "type_alias")
}

fn cpp_class_kind(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "union")
}

/// What `within_cpp_translation_unit` remembers per run: a file's same-stem headers.
#[derive(Default)]
pub(super) struct UnitMemo {
    stem_headers: HashMap<String, Rc<Vec<String>>>,
}

/// A file's name without its last extension (`dir/conformance_test.cc` → `conformance_test`).
fn file_stem(path: &str) -> &str {
    let name = &path[path.rfind('/').map_or(0, |i| i + 1)..];
    match name.rfind('.') { Some(dot) if dot > 0 => &name[..dot], _ => name }
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

    /// The headers a C or C++ file includes under its own name (`conformance_test.cc` →
    /// `conformance_test.h`): the declarations its definitions implement. One beside it, when it
    /// includes one, is the only such header: protobuf's pyext/message.cc implements
    /// pyext/message.h, not the google/protobuf/message.h it also includes; fmt's src/format.cc,
    /// with none beside it, implements include/fmt/format.h. (cppSameStemHeaders)
    fn cpp_same_stem_headers(&mut self, file: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.cpp_units.stem_headers.get(file) { return Ok(hit.clone()); }
        let stem = file_stem(file);
        let includes: Vec<_> = self.nodes_in_file(file)?.iter()
            .filter(|n| n.kind == "import" && file_stem(&n.name) == stem && matches!(n.language.as_str(), "c" | "cpp")).cloned().collect();
        let mut found: Vec<String> = Vec::new();
        for node in includes {
            let delimiter = if node.signature.as_deref().is_some_and(|s| s.contains('<')) { '<' } else { '"' };
            if let Some(header) = self.resolve_cpp_include(file, delimiter, &node.name, &node.language)? {
                if header != file && file_stem(&header) == stem { found.push(header); }
            }
        }
        let dir = &file[..file.rfind('/').map_or(0, |i| i + 1)];
        let mut beside: Vec<String> = found.iter().filter(|h| &h[..h.rfind('/').map_or(0, |i| i + 1)] == dir).cloned().collect();
        if beside.is_empty() {
            // The adjacent header can be reached through another header with the same stem.
            let visible = self.namespace_visible_files(file, "cpp")?;
            beside = visible.into_iter().filter(|h| h != file && file_stem(h) == stem
                && &h[..h.rfind('/').map_or(0, |i| i + 1)] == dir).collect();
            beside.sort();
        }
        let headers = Rc::new(if beside.is_empty() { found } else { beside });
        self.cpp_units.stem_headers.insert(file.to_string(), headers.clone());
        Ok(headers)
    }

    /// Whether a C or C++ definition is in `r`'s translation unit, as its includes show it: in the
    /// reference's own file, in a file the reference's file includes, directly or through other
    /// includes, or in the source file that implements such a header — the one named like it that
    /// includes it (`googletest.cc` for `googletest.h`) — unless the source keeps it to itself (a
    /// `static`, or what `cpp_translation_unit_local` finds). A test suite's source implements
    /// only a test suite's header. (isInCppTranslationUnit)
    fn is_in_cpp_translation_unit(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if candidate.file_path == r.file_path { return Ok(true); }
        if !matches!(candidate.language.as_str(), "c" | "cpp") { return Ok(false); }
        let visible = self.namespace_visible_files(&r.file_path, &r.language)?;
        if visible.contains(&candidate.file_path) { return Ok(true); }
        if !c_source_ext_re().is_match(&candidate.file_path) || self.cpp_translation_unit_local(candidate, r)?
            || (matches!(candidate.kind.as_str(), "function" | "variable" | "constant") && self.is_static_c_function(candidate)?) { return Ok(false); }
        let suite = super::resolver_upstream::test_suite_path(&candidate.file_path);
        // (The header may be the reference's own: json.h's inline overloads call the ones json.cc defines.)
        for header in self.cpp_same_stem_headers(&candidate.file_path)?.iter() {
            if (!suite || super::resolver_upstream::test_suite_path(header)) && (*header == r.file_path || visible.contains(header)) { return Ok(true); }
        }
        Ok(false)
    }

    /// What a C or C++ name written alone can mean outside a class, by its kind: a call reaches a
    /// function, or a type and its constructor (`Foo(…)`); a construction makes a type; any other
    /// reference names anything but a member. Nothing a class declares is in reach — its methods,
    /// its nested types, their constructors: only an object, a qualified name or the class's own
    /// code reaches them. (isCppNameAloneTarget)
    fn is_cpp_name_alone_target(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let constructor = n.kind == "method" && n.qualified_name.ends_with(&format!("{0}::{0}", n.name));
        let fits = match r.reference_kind.as_str() {
            "calls" => n.kind == "function" || constructor || cpp_type_kind(&n.kind),
            "instantiates" => constructor || cpp_type_kind(&n.kind),
            _ => constructor || !matches!(n.kind.as_str(), "method" | "field" | "property"),
        };
        if !fits { return Ok(false); }
        let parent = |qualified: &str| qualified.rfind("::").map_or("", |cut| &qualified[..cut]).to_string();
        let scope = parent(&if constructor { parent(&n.qualified_name) } else { n.qualified_name.clone() });
        Ok(scope.is_empty() || self.cpp_class_decls(&scope)?.is_empty())
    }

    /// Whether as many arguments as `args` can fill the parameters of the function `n`, its
    /// defaults and a pack counted; None when `n` is no function or its parameters can't be read.
    fn cpp_argument_count_fits(&mut self, n: &KNode, args: &[String], r: &ResolveRefIn) -> Option<bool> {
        if !matches!(n.kind.as_str(), "function" | "method") { return None; }
        self.declaration_arity(n, r).map(|(min, max)| args.len() >= min && args.len() <= max)
    }

    /// Same-named candidates for a C or C++ reference, narrowed to the ones its translation unit
    /// declares, when there are any: the name a file uses is one its includes declare. protobuf's
    /// csharp generator tests call the `TestSourceDir()` googletest.h declares, not
    /// test_util2.h's `TestUtil::TestSourceDir`, a test suite none of them includes; its Python
    /// and Ruby extensions and upb's own code call `upb_Encode()` in upb/wire/encode.c, not the
    /// copy php-upb.c bundles. For a name the reference writes alone (`by_name`), only what the
    /// name alone can mean counts, and a call the extractor recorded without the receiver it is
    /// written on (`columns()[0].value()`) is left to the name strategies. When the unit declares
    /// none of them — a file whose includes don't resolve — the candidates stay as they are. Not
    /// for an `#include`, which names a file. (withinCppTranslationUnit)
    pub(super) fn within_cpp_translation_unit(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn, by_name: bool) -> Res<Vec<Arc<KNode>>> {
        if candidates.len() < 2 || !matches!(r.language.as_str(), "c" | "cpp") || r.reference_kind == "imports" { return Ok(candidates); }
        let calls = r.reference_kind == "calls";
        if by_name && calls && self.cpp_implicit_receiver(r) != Some(Receiver::Bare) { return Ok(candidates); }
        // A "call" outside any function is a macro, or a declaration read as a call
        // (rocksdb's c.h `extern … rocksdb_t*` above `rocksdb_open(…);`): no call.
        if calls && self.node_by_id(&r.from_node_id)?.is_some_and(|n| n.kind == "file") { return Ok(candidates); }
        let mut within: Vec<Arc<KNode>> = Vec::new();
        for n in &candidates {
            if self.is_in_cpp_translation_unit(n, r)? && (!by_name || n.file_path == r.file_path || self.is_cpp_name_alone_target(n, r)?) { within.push(n.clone()); }
        }
        if within.is_empty() || within.len() == candidates.len() { return Ok(candidates); }
        let outside: Vec<Arc<KNode>> = candidates.iter().filter(|n| !within.iter().any(|w| w.id == n.id)).cloned().collect();
        // Nor, for a C call, to definitions only a flag nothing sets compiles, past one the build
        // without it does (lua-cjson's fpconv.h defines `fpconv_init` under `#ifdef
        // USE_INTERNAL_FPCONV`; redis's build calls fpconv.c's).
        if r.language == "c" && calls && !self.is_off_by_default(&r.file_path, r.line) {
            let mut every_variant = true;
            for n in &within {
                if !self.is_c_feature_variant(n, r) { every_variant = false; break; }
            }
            if every_variant {
                for n in &outside {
                    if n.kind == "function" && !self.is_static_c_function(n)? && !self.is_c_feature_variant(n, r) { return Ok(candidates); }
                }
            }
        }
        // Nor to functions too few or too many arguments for the call, past one that takes them:
        // glog's `GetStackTrace(stack, ARRAYSIZE(stack), 1)` is the one stacktrace.h declares,
        // which the stacktrace_*-inl.h headers its stacktrace.cc includes define, not
        // utilities.h's `GetStackTrace()`.
        if calls {
            let name = within[0].name.clone();
            if let Some(args) = self.call_arguments(r, &name) {
                let mut none_fit = true;
                for n in &within {
                    if self.cpp_argument_count_fits(n, &args, r) != Some(false) { none_fit = false; break; }
                }
                if none_fit {
                    for n in &outside {
                        if self.cpp_argument_count_fits(n, &args, r) == Some(true) { return Ok(candidates); }
                    }
                }
            }
        }
        Ok(within)
    }

    /// The class-like candidates of a receiver's name, for a C or C++ reference narrowed to its
    /// translation unit's (the class-name and capitalized-receiver strategies; isMethodOwnerKind).
    pub(super) fn cpp_translation_unit_owners(&mut self, named: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        if !matches!(r.language.as_str(), "c" | "cpp") { return Ok(named); }
        let owners = named.into_iter().filter(|n| matches!(n.kind.as_str(), "class" | "struct" | "union" | "interface")).collect();
        self.within_cpp_translation_unit(owners, r, false)
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
