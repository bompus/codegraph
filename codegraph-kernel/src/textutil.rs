//! Utilities shared by the language walkers: compiled regexes, UTF-16 column
//! conversion, generated-file detection, the walker predicates, and small
//! text helpers — each mirroring a helper in src/extraction/tree-sitter.ts
//! (noted inline).

use regex::Regex;
use std::sync::OnceLock;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        pub fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pat).expect(concat!("regex ", stringify!($name))))
        }
    };
}

// RTK_HOOK_NAME_RE (tree-sitter.ts)
re!(rtk_hook_name, r"^use[A-Z][A-Za-z0-9]*(?:Query|Mutation)$");
// reactComponentHoc's styled test
re!(styled_callee, r"^styled(?-u:\b)");
// PascalCase component gate (#841)
re!(pascal_case, r"^[A-Z]");
// extractCall parenthesized-conversion normalization
re!(paren_conversion, r"^\(\s*\*?\s*([A-Za-z_][0-9A-Za-z_.]*)\s*\)$");
// flushFnRefCandidates SIMPLE_NAME
re!(simple_name, r"^[A-Za-z_$][A-Za-z0-9_$]*$");
// flushFnRefCandidates QUALIFIED_IMPORT
re!(qualified_import, r"^[A-Za-z_$][A-Za-z0-9_$.\\]*[.\\]([A-Za-z_$][A-Za-z0-9_$]*)$");
// captureFnRefCandidates rhs param-storage skip — trailing identifier of LHS
re!(lhs_last_name, r"([A-Za-z_$][A-Za-z0-9_$]*)\s*$");
// extractTsTupleContractNames identifier test
re!(ident_dollar, r"^[A-Za-z_$][A-Za-z0-9_$]*$");
// looksLikeVueStoreFile signal (VUE_STORE_FILE_SIGNAL)
re!(
    vue_store_signal,
    r"(?-u:\b)defineStore(?-u:\b)|(?-u:\b)createStore(?-u:\b)|(?-u:\b)Vuex(?-u:\b)|(?-u:\b)mutations(?-u:\b)|(?-u:\b)actions(?-u:\b)|(?-u:\b)getters(?-u:\b)|(?-u:\b)namespaced(?-u:\b)"
);
// value-ref target-name distinctiveness: /[A-Z_]/
re!(has_upper_or_underscore, r"[A-Z_]");

/// isGeneratedFile (src/extraction/generated-detection.ts) — full pattern list
/// ported so future language walkers share it.
pub fn is_generated_file(file_path: &str) -> bool {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = RES.get_or_init(|| {
        [
            r"\.pb\.go$",
            r"\.pulsar\.go$",
            r"_grpc\.pb\.go$",
            r"_mock\.go$",
            r"_mocks\.go$",
            r"^mock_[^/]+\.go$",
            r"\.generated\.[jt]sx?$",
            r"\.gen\.[jt]sx?$",
            r"\.pb\.[jt]s$",
            r"_pb\.[jt]s$",
            r"_grpc_pb\.[jt]s$",
            r"\.min\.m?js$",
            r"_pb2(_grpc)?\.py$",
            r"_pb2\.pyi$",
            r"\.pb\.(cc|h)$",
            r"\.g\.cs$",
            r"Grpc\.cs$",
            r"OuterClass\.java$",
            r"Grpc\.java$",
            r"\.pb\.swift$",
            r"\.g\.dart$",
            r"\.freezed\.dart$",
            r"\.pb\.dart$",
            r"\.pbgrpc\.dart$",
            r"\.chopper\.dart$",
            r"\.generated\.rs$",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("generated pattern"))
        .collect()
    });
    patterns.iter().any(|p| p.is_match(file_path))
}

/// Byte offsets of each line start, for UTF-16 column conversion.
pub fn line_starts(src: &str) -> Vec<usize> {
    let mut out = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            out.push(i + 1);
        }
    }
    out
}

/// UTF-16 code units in `s` — what web-tree-sitter (and JS string ops)
/// count, so kernel-emitted columns are byte-identical to the wasm path's.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// Column (UTF-16 units) of `byte_pos` on line `row`, given `line_starts` —
/// the rescanning reference `Cols::col` is checked against.
#[cfg(test)]
pub fn col16(src: &str, starts: &[usize], row: usize, byte_pos: usize) -> u32 {
    let ls = starts.get(row).copied().unwrap_or(0);
    if byte_pos <= ls {
        return 0;
    }
    utf16_len(&src[ls..byte_pos]) as u32
}

/// Prefix table: UTF-16 units before each byte offset (len + 1 entries).
pub fn utf16_prefix(src: &str) -> Vec<u32> {
    let bytes = src.as_bytes();
    let mut out = vec![0u32; bytes.len() + 1];
    let mut units = 0u32;
    let mut i = 0;
    for ch in src.chars() {
        let len = ch.len_utf8();
        for k in 0..len {
            out[i + k] = units;
        }
        units += ch.len_utf16() as u32;
        i += len;
    }
    out[bytes.len()] = units;
    out
}

/// Per-file column service for the walkers: `col` is the same UTF-16 column
/// `col16` computes, in O(1) instead of a rescan of the line prefix per
/// node — an all-ASCII file (the common case, and every minified one-liner,
/// where the rescan was quadratic) reads columns straight off the byte
/// offsets; a non-ASCII file builds the UTF-16 prefix table once, on first
/// use.
pub struct Cols {
    starts: Vec<usize>,
    ascii: bool,
    prefix: std::cell::OnceCell<Vec<u32>>,
}

impl Cols {
    pub fn new(src: &str) -> Cols {
        Cols { starts: line_starts(src), ascii: src.is_ascii(), prefix: std::cell::OnceCell::new() }
    }

    /// `split('\n').length` — the file node's endLine.
    pub fn line_count(&self) -> u32 {
        self.starts.len() as u32
    }

    /// Column (UTF-16 units) of `byte_pos` on line `row`.
    pub fn col(&self, src: &str, row: usize, byte_pos: usize) -> u32 {
        let ls = self.starts.get(row).copied().unwrap_or(0);
        if byte_pos <= ls {
            return 0;
        }
        if self.ascii {
            return (byte_pos - ls) as u32;
        }
        let prefix = self.prefix.get_or_init(|| utf16_prefix(src));
        let at = |b: usize| prefix.get(b).copied().unwrap_or_else(|| prefix[prefix.len() - 1]);
        at(byte_pos) - at(ls)
    }
}

/// JS `String.prototype.slice(0, n)` in UTF-16 units, without splitting a
/// surrogate pair (when the cut would split one, we stop one code unit short —
/// a lone surrogate isn't representable in Rust and never round-trips through
/// SQLite anyway). Returns (sliced, was_truncated_at_or_beyond_n).
pub fn slice_utf16(s: &str, n: usize) -> (String, bool) {
    let mut used = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let w = c.len_utf16();
        if used + w > n {
            return (out, true);
        }
        used += w;
        out.push(c);
        if used == n {
            // Exactly at the limit: truncated iff any source remains.
            let truncated = out.len() < s.len();
            return (out, truncated);
        }
    }
    (out, false)
}

/// objectKeyName (tree-sitter.ts): strip ONE leading and ONE trailing quote
/// character (`'`, `"`, or backtick).
pub fn object_key_name(s: &str) -> String {
    let mut out = s;
    if let Some(first) = out.chars().next() {
        if first == '\'' || first == '"' || first == '`' {
            out = &out[first.len_utf8()..];
        }
    }
    if let Some(last) = out.chars().last() {
        if last == '\'' || last == '"' || last == '`' {
            out = &out[..out.len() - last.len_utf8()];
        }
    }
    out.to_string()
}

/// The `= <first 100 UTF-16 units>[...]` initializer signature used by
/// extractVariable (its `.length >= 100` check fires exactly when the slice
/// hit the cap).
pub fn init_signature(value_text: &str) -> String {
    let (sliced, _) = slice_utf16(value_text, 100);
    if utf16_len(&sliced) >= 100 {
        format!("= {sliced}...")
    } else {
        format!("= {sliced}")
    }
}


// Walker predicates shared by every language that needs them (each was
// once a byte-identical copy per walker).

/// NAME_STOPLIST (function-ref.ts).
pub fn is_stoplisted(name: &str) -> bool {
    matches!(
        name,
        "this" | "self" | "super" | "null" | "nil" | "true" | "false" | "undefined" | "new"
            | "NULL" | "nullptr" | "None"
    )
}

/// BUILTIN_TYPES (tree-sitter.ts) — the full shared table; membership is what
/// the TS code tests, so every row is ported even where only the Java/C# row
/// can fire (a C# type named `String`/`error` IS suppressed via other rows).
pub fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "string" | "number" | "boolean" | "void" | "null" | "undefined" | "never" | "any"
            | "unknown" | "object" | "symbol" | "bigint" | "true" | "false"
            | "str" | "bool" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" | "char"
            | "int" | "long" | "short" | "byte" | "float" | "double"
            | "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64"
            | "float32" | "float64" | "complex64" | "complex128" | "rune" | "error"
            | "Int" | "Long" | "Short" | "Byte" | "Float" | "Double" | "Boolean" | "Char"
            | "Unit" | "String" | "Any" | "AnyRef" | "AnyVal" | "Nothing" | "Null"
    )
}

/// LITERAL_RECEIVER_TYPES (tree-sitter.ts) — full set; membership is what the
/// TS code tests even though only a few kinds occur in the c/cpp grammars.
pub fn is_literal_receiver(kind: &str) -> bool {
    matches!(
        kind,
        "string" | "string_literal" | "interpreted_string_literal" | "raw_string_literal"
            | "template_string" | "concatenated_string" | "formatted_string" | "f_string"
            | "line_string_literal" | "string_content" | "heredoc_body"
            | "number" | "number_literal" | "integer" | "integer_literal" | "float"
            | "float_literal" | "int_literal" | "decimal_integer_literal" | "real_literal"
            | "char_literal" | "character_literal" | "rune_literal" | "regex" | "regex_literal"
            | "true" | "false" | "boolean_literal" | "bool_literal" | "none" | "null" | "nil"
            | "null_literal" | "undefined"
            | "list" | "list_literal" | "array" | "array_literal" | "array_creation_expression"
            | "dictionary" | "dict_literal" | "object" | "tuple" | "set"
    )
}

/// The `new ns.Foo<T>()` name normalization shared by instantiation /
/// anonymous-class extraction: strip `<...` from the first `<` (index > 0),
/// keep the segment after the last `.`/`::`, strip ONE leading `:` or `.`,
/// trim. (The vbnet paren strip in the TS path is vbnet-gated — inert here.)
pub fn strip_generic_and_qualifier(raw: &str) -> String {
    let mut name = raw.to_string();
    if let Some(lt) = name.find('<') {
        if lt > 0 {
            name.truncate(lt);
        }
    }
    let last_dot = name
        .rfind('.')
        .map(|i| i as isize)
        .unwrap_or(-1)
        .max(name.rfind("::").map(|i| i as isize).unwrap_or(-1));
    if last_dot >= 0 {
        name = name[(last_dot as usize + 1)..].to_string();
        if name.starts_with(':') || name.starts_with('.') {
            name.remove(0);
        }
    }
    name.trim().to_string()
}

/// extractStaticMemberRef's capitalized-receiver test.
pub fn capitalized_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z][A-Za-z0-9_]*$").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_cols() {
        let src = "aé😀b";
        // 'a'=1, 'é'=1, '😀'=2 utf16 units; bytes: a=1, é=2, 😀=4
        assert_eq!(utf16_len(src), 5);
        let starts = line_starts(src);
        assert_eq!(col16(src, &starts, 0, 1), 1); // after 'a'
        assert_eq!(col16(src, &starts, 0, 3), 2); // after 'é'
        assert_eq!(col16(src, &starts, 0, 7), 4); // after '😀'
        let cols = Cols::new(src);
        for (byte, want) in [(0, 0), (1, 1), (3, 2), (7, 4), (8, 5)] {
            assert_eq!(cols.col(src, 0, byte), want, "byte {byte}");
            assert_eq!(cols.col(src, 0, byte), col16(src, &starts, 0, byte));
        }
    }

    #[test]
    fn cols_match_col16_across_lines() {
        let src = "ascii line\n  x = 1\nünïcödé π\n\tlast";
        let starts = line_starts(src);
        let cols = Cols::new(src);
        assert_eq!(cols.line_count(), 4);
        for (row, &ls) in starts.iter().enumerate() {
            let end = starts.get(row + 1).map(|n| n - 1).unwrap_or(src.len());
            for byte in ls..=end {
                if !src.is_char_boundary(byte) {
                    continue;
                }
                assert_eq!(cols.col(src, row, byte), col16(src, &starts, row, byte), "row {row} byte {byte}");
            }
        }
        // ASCII fast path agrees too.
        let a = "plain\nlines only\n";
        let ac = Cols::new(a);
        let astarts = line_starts(a);
        assert_eq!(ac.col(a, 1, 8), col16(a, &astarts, 1, 8));
    }

    #[test]
    fn init_sig_short_and_long() {
        assert_eq!(init_signature("[1, 2]"), "= [1, 2]");
        let long = "x".repeat(150);
        let sig = init_signature(&long);
        assert!(sig.starts_with("= "));
        assert!(sig.ends_with("..."));
        assert_eq!(utf16_len(&sig[2..sig.len() - 3]), 100);
    }

    #[test]
    fn generated_patterns() {
        assert!(is_generated_file("src/api.generated.ts"));
        assert!(is_generated_file("vendor/jquery.min.js"));
        assert!(!is_generated_file("src/app.ts"));
    }
}
