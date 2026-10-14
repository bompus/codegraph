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
const NOT_A_TYPE: &[&str] = &["return", "throw", "co_return", "case", "goto", "else", "do", "new", "delete", "sizeof", "typedef", "using"];

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
            run.push((at, end));
            let next = skip_space(code, end);
            at = next;
            if next == end && code.get(end) != Some(&b'*') { break; }
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
fn declarator_attribute_spans(code: &[u8], text: &str, spans: &mut Vec<(usize, usize)>) {
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
        let declares = if code[k - 1] == b']' { true } else {
            let name_end = k;
            while k > 0 && is_word(code[k - 1]) { k -= 1; }
            let name = &text[k..name_end];
            if name.is_empty() || !name.bytes().any(|b| b.is_ascii_lowercase()) || name.as_bytes()[0].is_ascii_digit() { false } else {
                let mut t = k;
                while t > 0 && code[t - 1].is_ascii_whitespace() { t -= 1; }
                match t.checked_sub(1).map(|p| code[p]) {
                    Some(b'*' | b'&' | b'>') => true,
                    Some(c) if is_word(c) => {
                        let word_end = t;
                        while t > 0 && is_word(code[t - 1]) { t -= 1; }
                        !NOT_A_TYPE.contains(&&text[t..word_end])
                    }
                    _ => false,
                }
            }
        };
        if declares { spans.extend(run); }
    }
}

/// C++ `text` with the annotation macros extraction blanks before parsing replaced by spaces.
pub(super) fn cpp_without_annotations(text: &str) -> Cow<'_, str> {
    if !re!(r"[*&]\s*[A-Z][A-Z0-9]*_|[\w\]]\s+[A-Z][A-Z0-9]*_[A-Z0-9_]*\s*[(;=,)]").is_match(text) { return Cow::Borrowed(text); }
    let code = masked_code(text);
    let mut spans = Vec::new();
    pointer_annotation_spans(&code, text, &mut spans);
    declarator_attribute_spans(&code, text, &mut spans);
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
}
