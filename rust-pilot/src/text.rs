//! Regex and string operations with the JavaScript semantics used by the port.
use fancy_regex::Regex;
use std::{cell::RefCell, collections::HashMap};
thread_local! { static REGEXES: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new()); }
#[derive(Debug)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<String>>,
}
impl Match {
    pub fn group(&self, index: usize) -> &str {
        self.groups[index].as_deref().unwrap_or("")
    }
}
fn js_pattern(pattern: &str) -> String {
    let mut out = String::new();
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next().unwrap() {
                's' if in_class => out.push_str(r"\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}"),
                's' => out.push_str(r"[\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]"),
                'w' if in_class => out.push_str("A-Za-z0-9_"),
                'w' => out.push_str("[A-Za-z0-9_]"),
                'b' if !in_class => out.push_str(
                    r"(?:(?<![A-Za-z0-9_])(?=[A-Za-z0-9_])|(?<=[A-Za-z0-9_])(?![A-Za-z0-9_]))",
                ),
                x => {
                    out.push('\\');
                    out.push(x);
                }
            }
        } else {
            if c == '[' {
                in_class = true;
            }
            if c == ']' {
                in_class = false;
            }
            out.push(c);
        }
    }
    out
}
pub fn matches(pattern: &str, text: &str) -> Vec<Match> {
    REGEXES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let re = cache
            .entry(pattern.to_owned())
            .or_insert_with(|| Regex::new(&js_pattern(pattern)).unwrap());
        re.captures_iter(text)
            .map(|c| {
                let c = c.expect("regex execution");
                let m = c.get(0).unwrap();
                Match {
                    start: m.start(),
                    end: m.end(),
                    groups: (0..c.len())
                        .map(|i| c.get(i).map(|m| m.as_str().to_owned()))
                        .collect(),
                }
            })
            .collect()
    })
}
pub fn first(pattern: &str, text: &str) -> Option<Match> {
    matches(pattern, text).into_iter().next()
}
pub fn test(pattern: &str, text: &str) -> bool {
    first(pattern, text).is_some()
}
pub fn capture(pattern: &str, text: &str, index: usize) -> Option<String> {
    first(pattern, text).and_then(|m| m.groups[index].clone())
}
pub fn line_at(s: &str, at: usize) -> i64 {
    s[..at].bytes().filter(|c| *c == b'\n').count() as i64 + 1
}
pub fn js_len(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() && c != '\u{85}' || c == '\u{feff}')
}

/// Only the JS/TS C-style branch is reachable from the NestJS resolver.
pub fn strip_comments(src: &str) -> String {
    let mut out: Vec<u16> = src.encode_utf16().collect();
    let raw = out.clone();
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        if c == 47 && (raw.get(i + 1) == Some(&42) || raw.get(i + 1) == Some(&47)) {
            let start = i;
            let block = raw[i + 1] == 42;
            i += 2;
            if block {
                while i < raw.len() && !(raw[i] == 42 && raw.get(i + 1) == Some(&47)) {
                    i += 1;
                }
                i = (i + 2).min(raw.len());
            } else {
                while i < raw.len() && raw[i] != 10 {
                    i += 1;
                }
            }
            for j in start..i {
                out[j] = if raw[j] == 10 { 10 } else { 32 };
            }
            continue;
        }
        if [34, 39, 96].contains(&c) {
            i += 1;
            while i < raw.len() {
                if raw[i] == c {
                    break;
                }
                if raw[i] == 92 && i + 1 < raw.len() {
                    i += 2;
                    continue;
                }
                if c != 96 && raw[i] == 10 {
                    break;
                }
                i += 1;
            }
            if raw.get(i) == Some(&c) {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    String::from_utf16(&out).expect("source remains valid UTF-16")
}
