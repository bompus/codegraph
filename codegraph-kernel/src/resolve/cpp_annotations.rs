//! The C++ the resolver parses has the annotation macros extraction blanks before parsing blanked
//! the same way, so a declaration reads as extraction read it: protobuf's `Printer*
//! PROTOBUF_NONNULL printer_;` declares `printer_`, and leveldb's `VersionSet* const versions_
//! GUARDED_BY(mutex_);` declares `versions_`. tree-sitter takes such a macro for the declared name,
//! or for a second declarator, and errors.
//!
//! Port of two of c-cpp.ts's preParse passes, `blankCppPointerAnnotationMacros` and
//! `blankCppDeclaratorAttributeMacros`, matched on the code with comments and literals masked.
//! Each macro is replaced by spaces, so every byte offset stays where it was.
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::LazyLock;

use regex::Regex;

/// `text` with comments and the contents of string and character literals (raw strings included)
/// replaced by spaces, byte for byte (line breaks kept).
pub(super) fn masked_code(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for b in &mut out[from..to.min(bytes.len())] { if *b != b'\n' && *b != b'\r' { *b = b' '; } }
    };
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + 2 + n + 2);
                blank(&mut out, i, end);
                i = end;
            }
            b'R' if bytes.get(i + 1) == Some(&b'"') && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') || matches!(bytes[i - 1], b'8' | b'u' | b'U' | b'L')) => {
                let open = i + 1;
                let Some(paren) = text[open..].find('(').map(|p| open + p) else { i += 1; continue };
                let closing = format!("){}\"", &text[open + 1..paren]);
                let end = text[paren..].find(&closing).map_or(bytes.len(), |n| paren + n + closing.len());
                blank(&mut out, open, end);
                i = end;
            }
            // (A `'` after a digit or letter separates digits: `1'000'000`.)
            b'\'' if i > 0 && bytes[i - 1].is_ascii_alphanumeric() => i += 1,
            quote @ (b'"' | b'\'') => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != quote && bytes[j] != b'\n' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                blank(&mut out, i + 1, j.min(bytes.len()));
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn is_word(b: u8) -> bool { b.is_ascii_alphanumeric() || b == b'_' }

/// The end of the ALL-CAPS annotation macro name (`[A-Z][A-Z0-9]*_[A-Z0-9_]*`) starting at `at`.
fn macro_name_end(code: &[u8], at: usize) -> Option<usize> {
    if !code.get(at)?.is_ascii_uppercase() || (at > 0 && is_word(code[at - 1])) { return None; }
    let mut end = at;
    while end < code.len() && is_word(code[end]) { end += 1; }
    let name = &code[at..end];
    (name.contains(&b'_') && name.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')).then_some(end)
}

fn skip_space(code: &[u8], mut i: usize) -> usize {
    while i < code.len() && code[i].is_ascii_whitespace() { i += 1; }
    i
}

/// The end of the balanced argument list opening at `open`, three levels deep at most.
fn arguments_end(code: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, &b) in code.iter().enumerate().skip(open) {
        match b {
            b'(' => { depth += 1; if depth > 3 { return None; } }
            b')' => { depth -= 1; if depth == 0 { return Some(i + 1); } }
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
    }
    None
}

const ALTERNATIVE_OPERATORS: &[&str] = &["and", "and_eq", "bitand", "bitor", "compl", "not", "not_eq", "or", "or_eq", "xor", "xor_eq"];
const TYPE_KEYWORDS: &[&str] = &["void", "bool", "char", "char8_t", "char16_t", "char32_t", "wchar_t", "short", "int", "long", "float", "double", "signed", "unsigned", "auto"];
const OTHER_KEYWORDS: &[&str] = &[
    "if", "while", "for", "switch", "catch", "return", "sizeof", "alignof", "alignas", "decltype", "typeid", "noexcept", "throw",
    "static_assert", "new", "delete", "case", "requires", "co_await", "co_yield", "co_return", "asm", "__asm__", "__attribute__",
    "__attribute", "__declspec", "_Pragma", "else", "do", "constexpr", "consteval", "explicit", "typeof", "__typeof__", "__typeof",
    "_Alignof", "__alignof__", "volatile", "__volatile__", "const", "static", "extern", "inline", "virtual", "constinit", "friend",
    "mutable", "thread_local", "register", "typedef", "using", "template", "typename", "class", "struct", "union", "enum",
    "namespace", "operator", "goto", "break", "continue", "default", "public", "private", "protected", "this", "true", "false",
    "nullptr", "and", "or", "not", "xor", "bitand", "bitor", "compl", "and_eq", "or_eq", "xor_eq", "not_eq",
];

fn is_keyword(word: &str) -> bool { TYPE_KEYWORDS.contains(&word) || OTHER_KEYWORDS.contains(&word) }

/// Whether the identifier ending at `code[last]` is a name a declaration declares: it has a
/// lowercase letter, is no keyword, and follows a type (a `*`, `&`, `>` that is no arrow, a type
/// keyword or another name; `const` and `volatile` only after a `*` or `&`). So in `const int
/// FOO_BAR = 1;` the name before `FOO_BAR` is `int`, a type, and `FOO_BAR` is the declared name.
fn follows_declared_name(code: &[u8], last: usize) -> bool {
    if !is_word(code[last]) { return false; }
    let mut start = last;
    while start > 0 && is_word(code[start - 1]) { start -= 1; }
    let name = std::str::from_utf8(&code[start..=last]).unwrap_or("");
    if name.as_bytes()[0].is_ascii_digit() || !name.bytes().any(|b| b.is_ascii_lowercase()) || is_keyword(name) { return false; }
    let mut i = start;
    while i > 0 && code[i - 1].is_ascii_whitespace() { i -= 1; }
    let Some(c) = i.checked_sub(1).map(|p| code[p]) else { return false };
    if c == b'*' || c == b'&' { return true; }
    if c == b'>' { return i < 2 || code[i - 2] != b'-'; }
    if !is_word(c) { return false; }
    let mut j = i;
    while j > 0 && is_word(code[j - 1]) { j -= 1; }
    let mut before = std::str::from_utf8(&code[j..i]).unwrap_or("");
    // Qualifiers after the type read through: `int const volatile value_`, `Foo* const value_`.
    while before == "const" || before == "volatile" {
        let mut k = j;
        while k > 0 && code[k - 1].is_ascii_whitespace() { k -= 1; }
        if k == 0 { return false; }
        match code[k - 1] {
            b'*' | b'&' => return true,
            b'>' => return k < 2 || code[k - 2] != b'-',
            c if is_word(c) => {
                i = k;
                j = k;
                while j > 0 && is_word(code[j - 1]) { j -= 1; }
                before = std::str::from_utf8(&code[j..i]).unwrap_or("");
            }
            _ => return false,
        }
    }
    if TYPE_KEYWORDS.contains(&before) { return true; }
    !before.as_bytes()[0].is_ascii_digit() && !is_keyword(before)
}

/// The spans of the macros right after a `*` or `&` and before the name it declares (or another
/// `*`): `Arena* PROTOBUF_NULLABLE arena`, `const FieldDescriptor* PROTOBUF_NULLABLE* PROTOBUF_NONNULL
/// field`. Not after a `)`, `]` or number (a product), in a `return` or an assignment's value, or
/// when nothing follows (`Foo* PROTOBUF_NONNULL)` names the parameter).
fn pointer_annotation_spans(code: &[u8], text: &str, spans: &mut Vec<(usize, usize)>) {
    for (star, &b) in code.iter().enumerate() {
        if b != b'*' && b != b'&' { continue; }
        let mut at = skip_space(code, star + 1);
        let mut run = Vec::new();
        while let Some(end) = macro_name_end(code, at) {
            let next = skip_space(code, end);
            // (A macro stands apart from what follows it: `n * FOO_BAR::VALUE` is a product.)
            if next == end && code.get(end) != Some(&b'*') { break; }
            run.push((at, end));
            at = next;
        }
        if run.is_empty() { continue; }
        let after = at;
        let follows = match code.get(after) {
            Some(b'*') => b == b'*',
            Some(b':') => code.get(after + 1) == Some(&b':'),
            Some(c) if c.is_ascii_alphabetic() || *c == b'_' || *c == b'~' => {
                let mut end = after;
                while end < code.len() && is_word(code[end]) { end += 1; }
                !ALTERNATIVE_OPERATORS.contains(&&text[after..end])
            }
            _ => false,
        };
        if !follows { continue; }
        let before = String::from_utf8_lossy(&code[star.saturating_sub(40)..star]);
        if re!(r"(?:[)\]]|\b\d[\w.]*)\s*$").is_match(&before) { continue; }
        let boundary = code[..star].iter().rposition(|c| matches!(c, b';' | b'{' | b'}')).map_or(0, |p| p + 1);
        let prefix = String::from_utf8_lossy(&code[boundary..star]);
        if re!(r"\b(?:return|throw|co_return)\b").is_match(&prefix) || re!(r"(?:^|[^=!<>])=(?:[^=]|$)").is_match(&prefix) { continue; }
        spans.extend(run);
    }
}

/// The spans of the macros after the name a declaration declares, or after its array brackets:
/// `versions_ GUARDED_BY(mutex_);`, `cord ABSL_ATTRIBUTE_LIFETIME_BOUND)`. The macro is followed by
/// `;`, `=`, `,` or `)`, and stands after `]` or after a name with a lowercase letter that itself
/// follows a type (a name, `*`, `&` or `>`), so a variable named in capitals stays.
fn declarator_attribute_spans(code: &[u8], spans: &mut Vec<(usize, usize)>) {
    let mut i = 0;
    while i < code.len() {
        let Some(mut end) = macro_name_end(code, i) else { i += 1; continue };
        let start = i;
        let mut run = Vec::new();
        let mut at = start;
        while let Some(name_end) = macro_name_end(code, at) {
            end = name_end;
            let open = skip_space(code, end);
            if code.get(open) == Some(&b'(') {
                match arguments_end(code, open) { Some(close) => end = close, None => break }
            }
            run.push((at, end));
            at = skip_space(code, end);
        }
        i = end.max(start + 1);
        if run.is_empty() || !matches!(code.get(at), Some(b';' | b'=' | b',' | b')')) || code.get(at + 1) == Some(&b'=') && code[at] == b'=' { continue; }
        // What stands before the macro.
        let mut k = start;
        let space_end = k;
        while k > 0 && code[k - 1].is_ascii_whitespace() { k -= 1; }
        if k == space_end || k == 0 { continue; }
        let declares = code[k - 1] == b']' || follows_declared_name(code, k - 1);
        if declares { spans.extend(run); }
    }
}

/// C++ `text` with the annotation macros extraction blanks before parsing replaced by spaces.
pub(super) fn cpp_without_annotations(text: &str) -> Cow<'_, str> {
    if !re!(r"[*&]\s*[A-Z][A-Z0-9]*_|[\w\]]\s+[A-Z][A-Z0-9]*_[A-Z0-9_]*\s*[(;=,)]").is_match(text) { return Cow::Borrowed(text); }
    let code = masked_code(text);
    let mut spans = Vec::new();
    pointer_annotation_spans(&code, text, &mut spans);
    declarator_attribute_spans(&code, &mut spans);
    if spans.is_empty() { return Cow::Borrowed(text); }
    let mut out = text.as_bytes().to_vec();
    for (from, to) in spans {
        for b in &mut out[from..to] { if *b != b'\n' && *b != b'\r' { *b = b' '; } }
    }
    // Only ASCII macro names and their (ASCII-delimited) arguments' bytes are blanked whole.
    Cow::Owned(String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::cpp_without_annotations;

    #[test]
    fn blanks_declaration_annotations_only() {
        let input = "int n = 1'000; // a \"b\"\nf(\"x(\", 'y'); /* z\n w */ g();";
        let code = String::from_utf8(super::masked_code(input)).unwrap();
        assert_eq!(code.len(), input.len());
        assert_eq!(code.split('\n').map(str::trim_end).collect::<Vec<_>>(), ["int n = 1'000;", "f(\"  \", ' ');", "      g();"]);
        let text = "Printer* PROTOBUF_NONNULL printer_;\nVersionSet* const versions_ GUARDED_BY(mutex_);\nint n = a * MAX_LEN + 1;\nFoo DEFAULT_OPTIONS;\nreturn x * FOO_BAR;\n// Foo* PROTOBUF_NONNULL c;\n";
        let out = cpp_without_annotations(text);
        assert_eq!(out.len(), text.len());
        let blank = |name: &str| " ".repeat(name.len());
        let expected = format!("Printer* {} printer_;\nVersionSet* const versions_ {};\n", blank("PROTOBUF_NONNULL"), blank("GUARDED_BY(mutex_)"));
        assert!(out.starts_with(&expected), "{out}");
        assert!(out.contains("a * MAX_LEN + 1") && out.contains("Foo DEFAULT_OPTIONS;") && out.contains("x * FOO_BAR") && out.contains("Foo* PROTOBUF_NONNULL c;"));
    }

    #[test]
    fn leaves_a_capitalized_constant_and_a_qualified_name_after_a_product() {
        for text in ["const int FOO_BAR = 1;\n", "static constexpr unsigned MAX_LEN = 4;\n", "consume(n * FOO_BAR::VALUE);\n", "volatile long RETRY_LIMIT = 3;\n"] {
            assert_eq!(cpp_without_annotations(text), text);
        }
        let text = "Printer* PROTOBUF_NONNULL printer_;\nconst Foo* PROTOBUF_NULLABLE ns::Bar::run();\nstd::vector<int> items_ GUARDED_BY(mu_);\n";
        let out = cpp_without_annotations(text);
        assert!(!out.contains("PROTOBUF_NONNULL") && !out.contains("PROTOBUF_NULLABLE") && !out.contains("GUARDED_BY"), "{out}");
    }

    #[test]
    fn a_const_qualified_type_precedes_a_declared_name() {
        let blanked = |text: &str| cpp_without_annotations(text).into_owned();
        assert_eq!(blanked("int const value_ GUARDED_BY(mu_);\n"), format!("int const value_ {};\n", " ".repeat("GUARDED_BY(mu_)".len())));
        assert_eq!(blanked("Foo const volatile value_ GUARDED_BY(mu_);\n"), format!("Foo const volatile value_ {};\n", " ".repeat("GUARDED_BY(mu_)".len())));
        assert_eq!(blanked("const int FOO_BAR = 1;\n"), "const int FOO_BAR = 1;\n");
    }
}
