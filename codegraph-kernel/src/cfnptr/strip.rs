//! `stripCommentsForRegex(src, 'c')`: comments blanked per UTF-16 unit, string interiors skipped.


// ---------- stripCommentsForRegex(src, 'c') ----------

/// Port of `stripCStyle(src, /*allowSingleQuoteStrings*/ false)`:
/// `/* */` and `//` comments blanked to spaces (newlines preserved), `"` and
/// backtick string interiors skipped verbatim (backtick spans lines — the JS
/// helper treats it as a template literal even for C), `'` NOT special.
///
/// Blanking is per UTF-16 CODE UNIT (one space per BMP char, two per astral
/// char), so the result equals the JS stripper's output EXACTLY as a string —
/// the scanners downstream see the identical character stream the JS regexes
/// see, and the strip differential oracle pins byte equality directly.
/// (Comment boundaries — `/*`, `*/`, `//`, quotes, `\n` — are all ASCII, so
/// the state machine's byte positions always land on char boundaries.)
pub fn strip_c(src: &[u8]) -> Vec<u8> {
    let n = src.len();
    let mut out = Vec::with_capacity(n);
    let mut copied = 0usize; // src[..copied] already emitted
    let mut i = 0;
    {
        let mut blank_to = |out: &mut Vec<u8>, start: usize, end: usize| {
            out.extend_from_slice(&src[copied..start]);
            emit_blank(out, &src[start..end]);
            copied = end;
        };
        while i < n {
            let c = src[i];
            let c2 = if i + 1 < n { src[i + 1] } else { 0 };
            if c == b'/' && c2 == b'*' {
                let start = i;
                i += 2;
                while i < n && !(src[i] == b'*' && i + 1 < n && src[i + 1] == b'/') {
                    i += 1;
                }
                if i < n {
                    i += 2;
                }
                blank_to(&mut out, start, i.min(n));
                continue;
            }
            if c == b'/' && c2 == b'/' {
                let start = i;
                while i < n && src[i] != b'\n' {
                    i += 1;
                }
                blank_to(&mut out, start, i);
                continue;
            }
            if c == b'"' || c == b'`' {
                let quote = c;
                i += 1;
                while i < n && src[i] != quote {
                    if src[i] == b'\\' && i + 1 < n {
                        i += 2;
                        continue;
                    }
                    if quote != b'`' && src[i] == b'\n' {
                        break;
                    }
                    i += 1;
                }
                if i < n && src[i] == quote {
                    i += 1;
                }
                continue;
            }
            i += 1;
        }
    }
    out.extend_from_slice(&src[copied..]);
    out
}

/// One space per UTF-16 code unit (`\n` preserved): ASCII and 2-3-byte chars
/// are one unit, 4-byte (astral) chars are a surrogate pair — two units.
pub(super) fn emit_blank(out: &mut Vec<u8>, region: &[u8]) {
    let mut i = 0;
    while i < region.len() {
        let b = region[i];
        if b == b'\n' {
            out.push(b'\n');
            i += 1;
            continue;
        }
        // b < 0xC0 covers ASCII and a continuation byte at region start
        // (invalid UTF-8) — both count singly.
        let len = if b < 0xC0 {
            1
        } else if b < 0xE0 {
            2
        } else if b < 0xF0 {
            3
        } else {
            4
        };
        out.push(b' ');
        if len == 4 {
            out.push(b' ');
        }
        i += len.min(region.len() - i);
    }
}
