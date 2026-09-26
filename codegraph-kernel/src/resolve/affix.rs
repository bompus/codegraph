//! Name-in-pattern matching without a regex per name: `Affix` finds the name as a literal and runs only the fixed head and tail, compiled once.

use super::*;

/// buildLocalReceiverTypePatterns for the migrated set (name-matcher.ts).
/// Each entry is (pattern, guard): guard 1 reproduces the TS annotation
/// pattern's `(?![\w.$]|\s*(?:<[^>]*>)?\s*[\[|&])` lookahead in
/// infer_match_line, guard 2 the lua annotation pattern's
/// `(?![\w.]|\s*[({"'\[])`; the go param-type lookahead `(?=\s*[,)]|\s*$)` is
/// folded into its pattern as a consuming suffix (equivalent — the capture
/// cannot absorb `[,)]`/EOL, and shrinking it can never satisfy the suffix
/// either). Languages outside the switch (cpp, pascal, cfml — the latter two
/// unrouted) get no patterns, same as the TS `default: return []`.
/// JS `\w` / the crate's `(?-u:\b)` word class.
pub(super) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `(?-u:\b)` holds at byte offset `at` of `hay` (a non-ASCII byte
/// is non-word, as in the ASCII boundary).
pub(super) fn word_boundary_at(hay: &str, at: usize) -> bool {
    let bytes = hay.as_bytes();
    let before = at > 0 && is_word_byte(bytes[at - 1]);
    let after = at < bytes.len() && is_word_byte(bytes[at]);
    before != after
}

/// Every byte offset of `needle` in `hay` at or after `from`, overlapping
/// occurrences included (a regex examines every start position too). A
/// whole file gets memchr's SIMD substring search (`str::find` is the
/// two-way algorithm, several times slower there); a line keeps `str::find`,
/// since building the searcher costs more than scanning 80 bytes and the
/// receiver scans call this once per line.
pub(super) fn occurrences<'a>(hay: &'a str, needle: &'a str, from: usize) -> impl Iterator<Item = usize> + 'a {
    let step = needle.chars().next().map_or(1, char::len_utf8);
    let finder = (hay.len() >= 512).then(|| memchr::memmem::Finder::new(needle));
    let mut pos = from;
    std::iter::from_fn(move || {
        if needle.is_empty() || pos > hay.len() {
            return None;
        }
        let rel = match &finder {
            Some(f) => f.find(&hay.as_bytes()[pos..])?,
            None => hay[pos..].find(needle)?,
        };
        let at = pos + rel;
        pos = at + step;
        Some(at)
    })
}

/// `(?-u:\b)word(?-u:\b)` anywhere in `hay`.
pub(super) fn has_word(hay: &str, word: &str) -> bool {
    occurrences(hay, word, 0)
        .any(|at| word_boundary_at(hay, at) && word_boundary_at(hay, at + word.len()))
}

/// A pattern of the form `HEAD word TAIL`, kept apart so the word is found
/// as a literal and only the fixed parts are regexes, compiled once for the
/// process. The resolver used to format the word into the pattern and
/// compile the result per reference — tens of thousands of regexes per pool
/// worker on a large corpus, about 2 GB, plus the compile time.
///
/// With a `head`, the search runs on the head — its literal (`const`,
/// `type`, `$this->`) is what the regex engine prefilters on, where a short
/// receiver like `e` occurs in every line — and the word must start where
/// the head match ends; `tail` is anchored at its start (`^…`) and matched
/// against the text after the word. Without a head the word is found as a
/// literal. A `(?-u:\b)` that sat directly against the word is
/// `bound_before` / `bound_after`, checked on the whole line (an anchor at a
/// slice edge would see the wrong neighbour). Matches are taken in order and
/// never overlap, the sequence `captures_iter` produces for these shapes:
/// every head ends in whitespace, a literal, or an identifier the word's own
/// boundary keeps apart, so a head's leftmost match is the one the whole
/// pattern would use.
pub(super) struct Affix {
    pub(super) head: Option<Regex>,
    pub(super) tail: Option<Regex>,
    pub(super) bound_before: bool,
    pub(super) bound_after: bool,
    /// Capture group 1 lives in `head` (else in `tail`).
    pub(super) group_in_head: bool,
    /// The bytes the tail can start with after optional whitespace, when
    /// known — a lookahead cheaper than the regex for a headless pattern
    /// whose word is common (`e` in minified code occurs a thousand times a
    /// line; `e.target` fails on `.` without a regex call).
    pub(super) tail_lead: Option<&'static [u8]>,
}

/// One match: where it ends in the line, and group 1's span when any.
pub(super) struct AffixMatch {
    pub(super) end: usize,
    pub(super) group: Option<(usize, usize)>,
}

impl Affix {
    pub(super) fn new(head: &str, tail: &str, bound_before: bool, bound_after: bool, group_in_head: bool) -> Affix {
        let compile = |p: String| Regex::new(&p).unwrap_or_else(|e| panic!("affix pattern {p:?}: {e}"));
        Affix {
            head: (!head.is_empty()).then(|| compile(head.to_string())),
            tail: (!tail.is_empty()).then(|| compile(format!("^{tail}"))),
            bound_before,
            bound_after,
            group_in_head,
            tail_lead: None,
        }
    }

    /// The tail's possible first bytes after optional whitespace (see
    /// `tail_lead`); the equivalence tests keep a hint honest.
    pub(super) fn lead(mut self, bytes: &'static [u8]) -> Affix {
        self.tail_lead = Some(bytes);
        self
    }

    /// This thread's clones of the head and tail, fetched once per line set.
    pub(super) fn local(&'static self) -> (Option<Rc<Regex>>, Option<Rc<Regex>>) {
        (self.head.as_ref().map(thread_regex), self.tail.as_ref().map(thread_regex))
    }

    /// The first match at or after `from`.
    pub(super) fn find_from(&'static self, line: &str, word: &str, from: usize) -> Option<AffixMatch> {
        let (head, tail) = self.local();
        self.find_with(line, word, from, head.as_deref(), tail.as_deref())
    }

    pub(super) fn find_with(&self, line: &str, word: &str, from: usize, head: Option<&Regex>, tail: Option<&Regex>) -> Option<AffixMatch> {
        match head {
            Some(head) => {
                // A miss on the whole line is the common case and the plain
                // search answers it from the prefilter alone.
                if from == 0 && !head.is_match(line) {
                    return None;
                }
                let mut pos = from;
                while pos <= line.len() {
                    let caps = head.captures_at(line, pos)?;
                    let m0 = caps.get(0)?;
                    let at = m0.end();
                    if line[at..].starts_with(word) {
                        let group = self.group_in_head.then(|| caps.get(1).map(|g| (g.start(), g.end()))).flatten();
                        if let Some(m) = self.finish_at(line, word, at, group, tail) {
                            return Some(m);
                        }
                    }
                    pos = m0.end().max(m0.start() + 1);
                }
                None
            }
            None => occurrences(line, word, from).find_map(|at| self.finish_at(line, word, at, None, tail)),
        }
    }

    /// The match for a word at `at`, once the head (if any) has matched up
    /// to it: the word's boundaries, then the tail.
    pub(super) fn finish_at(
        &self,
        line: &str,
        word: &str,
        at: usize,
        group: Option<(usize, usize)>,
        tail: Option<&Regex>,
    ) -> Option<AffixMatch> {
        let word_end = at + word.len();
        if (self.bound_before && !word_boundary_at(line, at))
            || (self.bound_after && !word_boundary_at(line, word_end))
        {
            return None;
        }
        if let Some(lead) = self.tail_lead {
            let next = line[word_end..].trim_start().bytes().next();
            if !next.is_some_and(|b| lead.contains(&b)) {
                return None;
            }
        }
        let mut group = group;
        let mut end = word_end;
        if let Some(tail) = tail {
            let caps = tail.captures(&line[word_end..])?;
            end = word_end + caps.get(0).map_or(0, |m| m.end());
            if !self.group_in_head {
                group = caps.get(1).map(|g| (word_end + g.start(), word_end + g.end()));
            }
        }
        Some(AffixMatch { end, group })
    }

    pub(super) fn is_match(&'static self, line: &str, word: &str) -> bool {
        self.find_from(line, word, 0).is_some()
    }

    /// Group 1 of the first match.
    pub(super) fn capture<'l>(&'static self, line: &'l str, word: &str) -> Option<&'l str> {
        self.find_from(line, word, 0)?.group.map(|(s, e)| &line[s..e])
    }
}

/// A receiver-type pattern (name-matcher.ts buildLocalReceiverTypePatterns)
/// with its guard (see infer_match_line).
pub(super) struct ReceiverPattern {
    pub(super) affix: Affix,
    pub(super) guard: u8,
}

pub(super) fn rp(head: &str, tail: &str, bound_before: bool, bound_after: bool, group_in_head: bool, guard: u8) -> ReceiverPattern {
    ReceiverPattern { affix: Affix::new(head, tail, bound_before, bound_after, group_in_head), guard }
}

/// buildLocalReceiverTypePatterns, split around the receiver (`R` in the
/// TypeScript source): the receiver's own `\b`s become the bound flags and
/// every other anchor stays in the head or tail.
pub(super) static RECEIVER_TYPE_PATTERNS: LazyLock<HashMap<&'static str, Vec<ReceiverPattern>>> = LazyLock::new(|| {
    // `\bR\b TAIL` — the common shape; `lead` is the tail's first byte set.
    let both = |tail: &str, lead: &'static [u8], guard: u8| {
        ReceiverPattern { affix: Affix::new("", tail, true, true, false).lead(lead), guard }
    };
    let mut m: HashMap<&'static str, Vec<ReceiverPattern>> = HashMap::new();
    for lang in ["typescript", "javascript", "tsx", "jsx", "arkts"] {
        m.insert(
            lang,
            vec![
                both(r"\s*=\s*new\s+([A-Za-z_$][A-Za-z0-9_.$]*)", b"=", 0),
                both(r"\s*:\s*([A-Z][A-Za-z0-9_.$]*)", b":", 1),
            ],
        );
    }
    m.insert(
        "python",
        vec![
            both(r"\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(", b"=", 0),
            both(r#"\s*:\s*["']([A-Z][A-Za-z0-9_.]*)["']"#, b":", 0),
            both(r"\s*:\s*([A-Z][A-Za-z0-9_.]*)", b":", 0),
        ],
    );
    m.insert(
        "java",
        vec![
            both(r"\s*=\s*new\s+([A-Za-z_][A-Za-z0-9_.]*)", b"=", 0),
            rp(r"(?-u:\b)([A-Z][A-Za-z0-9_.]*)\s+", r"\s*[=;,:)]", false, true, true, 0),
        ],
    );
    m.insert(
        "kotlin",
        vec![
            both(r"\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(", b"=", 0),
            both(r"\s*:\s*([A-Z][A-Za-z0-9_.]*)", b":", 0),
        ],
    );
    m.insert(
        "rust",
        vec![
            // let r [mut] [: T] = [&][mut] Type::new()/Type{}/Type — a `let`
            // binding with an optional annotation; the capture is the
            // initializer's type, not the annotation's.
            rp(
                r"(?-u:\b)let\s+(?:mut\s+)?",
                r"(?:\s*:[^=]+)?=\s*&?(?:mut\s+)?([A-Z][A-Za-z0-9_]*)",
                false,
                true,
                false,
                0,
            ),
            // r : [&][mut] Type — a `let r: T` binding OR a typed parameter
            // (`fn f(r: &T)`, closure `|r: T|`) — the same shape (#1125).
            ReceiverPattern { affix: Affix::new("", r"\s*:\s*&?(?:mut\s+)?([A-Z][A-Za-z0-9_]*)", true, false, false).lead(b":"), guard: 0 },
        ],
    );
    m.insert(
        "go",
        vec![
            rp("", r"\s+\*?([a-z_][A-Za-z0-9_]*\.[A-Z][A-Za-z0-9_]*)(?:\s*[,)]|\s*$)", true, false, false, 0),
            both(r"\s*:=\s*&?([A-Za-z_][A-Za-z0-9_.]*)\s*\{", b":", 0),
            rp(r"(?-u:\b)var\s+", r"\s+\*?([A-Za-z_][A-Za-z0-9_.]*)", false, false, false, 0),
            rp("", r"\s+\*?([A-Z][A-Za-z0-9_.]*)", true, false, false, 0),
        ],
    );
    m.insert(
        "php",
        vec![
            // `\$?R\b …` — the optional `$` never decides whether a line matches.
            rp("", r"\s*=\s*new\s+([A-Za-z_\\][A-Za-z0-9_\\]*)", false, true, false, 0),
            rp(r"(?-u:\b)([A-Za-z_\\][A-Za-z0-9_\\]*)\s+&?\$", "", false, true, true, 0),
        ],
    );
    // `struct ops *o` / `ops_t *o` / `ops o` — a declared parameter or
    // local carrying an aggregate or typedef'd type; mirrors the cfnptr
    // receiver-decl scan (`recv_decl_types`). Single `*` only — a
    // pointer-to-pointer receiver can't be called through. The captured
    // word is validated by the member lookup, so a loose hit is a miss,
    // never a wrong edge.
    m.insert(
        "c",
        vec![rp(
            r"(?-u:\b)(?:(?:struct|union)\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\*?\s*",
            r"\s*(?:[,)=;]|\[)",
            true,
            true,
            true,
            0,
        )],
    );
    m.insert(
        "csharp",
        vec![
            both(r"\s*=\s*new\s+([A-Za-z_][A-Za-z0-9_.]*)", b"=", 0),
            rp(r"(?-u:\b)([A-Z][A-Za-z0-9_.]*)\s+", r"\s*[=;,)]", false, true, true, 0),
        ],
    );
    m.insert(
        "swift",
        vec![
            both(r"\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(", b"=", 0),
            both(r"\s*:\s*([A-Z][A-Za-z0-9_.]*)", b":", 0),
        ],
    );
    m.insert("ruby", vec![both(r"\s*=\s*([A-Z][A-Za-z0-9_:]*)\.new(?-u:\b)", b"=", 0)]);
    m.insert(
        "scala",
        vec![
            both(r"\s*=\s*(?:new\s+)?([A-Z][A-Za-z0-9_.]*)", b"=", 0),
            both(r"\s*:\s*([A-Z][A-Za-z0-9_.]*)", b":", 0),
        ],
    );
    m.insert(
        "dart",
        vec![
            both(r"\s*=\s*([A-Z][A-Za-z0-9_.]*)\s*\(", b"=", 0),
            rp(r"(?-u:\b)([A-Z][A-Za-z0-9_.]*)\s+", r"\s*[=;,)]", false, true, true, 0),
        ],
    );
    // The annotation arm's lookahead rejects Lua's `receiver:Name(` /
    // `"s"` / `{t}` call forms (#1124) — guard 2 in infer_match_line.
    for lang in ["lua", "luau"] {
        m.insert(
            lang,
            vec![
                both(r"\s*=\s*([A-Z][A-Za-z0-9_]*)\.new(?-u:\b)", b"=", 0),
                both(r"\s*=\s*([A-Z][A-Za-z0-9_]*)\s*\(", b"=", 0),
                both(r"\s*:\s*([A-Z][A-Za-z0-9_.]*)", b":", 2),
            ],
        );
    }
    m.insert("r", vec![both(r"\s*(?:<-|<<-|=)\s*([A-Z][A-Za-z0-9_.]*)\$new(?-u:\b)", b"<=", 0)]);
    // `var lg: TLogger` / a `lg: TLogger` parameter, then `lg := TLogger.Create`.
    m.insert(
        "pascal",
        vec![
            both(r"\s*:\s*([A-Z][A-Za-z0-9_]*)", b":", 0),
            both(r"\s*:=\s*([A-Z][A-Za-z0-9_.]*)\.Create(?-u:\b)", b":", 0),
        ],
    );
    m
});

pub(super) fn local_receiver_type_patterns(language: &str) -> &'static [ReceiverPattern] {
    RECEIVER_TYPE_PATTERNS.get(language).map_or(&[], Vec::as_slice)
}

/// buildPhpPropertyTypePatterns — only property-shaped declarations qualify
/// for `$this->prop` receivers (typed/promoted property, `new` assignment).
pub(super) static PHP_PROPERTY_TYPE_PATTERNS: LazyLock<Vec<ReceiverPattern>> = LazyLock::new(|| {
    vec![
        rp(
            r"(?-u:\b)(?:(?:private|protected|public|readonly|static|final)(?:\(set\))?\s+)+\??([A-Za-z_\\][A-Za-z0-9_\\]*)\s+&?\$",
            "",
            false,
            true,
            true,
            0,
        ),
        rp(r"\$this->", r"\s*=\s*new\s+([A-Za-z_\\][A-Za-z0-9_\\]*)", false, true, false, 0),
    ]
});

/// The lookahead tails of infer_match_line's guards 1 and 2.
pub(super) fn guard1_tail_re() -> Rc<Regex> {
    re!(r"^\s*(?:<[^>]*>)?\s*[\[|&]")
}
pub(super) fn guard2_tail_re() -> Rc<Regex> {
    re!(r#"^\s*[({"'\[]"#)
}
/// `^NAME\s*[(<]` / `(^|[^A-Za-z0-9_])NAME\s*\(` after the literal name.
pub(super) fn bare_call_opener_re() -> Rc<Regex> {
    re!(r"^\s*[(<]")
}
pub(super) fn cpp_call_opener_re() -> Rc<Regex> {
    re!(r"^\s*\(")
}

/// The TypeScript class-field type shapes matchTsFieldCall reads off an
/// owner's body — `field?: Type`, `field: typeof Ns` (the bool: a value
/// type), `field = new Type` — around the literal field name.
pub(super) static TS_FIELD_TYPE_PATTERNS: LazyLock<[(Affix, bool); 3]> = LazyLock::new(|| {
    [
        (
            Affix::new("", r"\s*[?!]?\s*:\s*(?:readonly\s+)?typeof\s+([A-Za-z_$][A-Za-z0-9_.$]*)", true, true, false).lead(b"?!:"),
            true,
        ),
        (
            Affix::new("", r"\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Za-z_$][A-Za-z0-9_.$]*)", true, true, false).lead(b"?!:"),
            false,
        ),
        (Affix::new("", r"\s*=\s*new\s+([A-Za-z_$][A-Za-z0-9_.$]*)", true, true, false).lead(b"="), false),
    ]
});

/// matchSelectedStoreCall's selector names: every `NAME` in
/// `const NAME = f((s) =>` / `const NAME = f(s =>` over the raw source, with
/// TS's ASCII `\w`.
pub(super) fn js_selector_names(text: &str) -> HashSet<String> {
    static SELECTOR: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?-u:\b)const\s+([0-9A-Za-z_$]+)\s*=\s*[0-9A-Za-z_$]+\s*\(\s*(?:\(\s*[0-9A-Za-z_$]+\s*\)|[0-9A-Za-z_$]+)\s*=>",
        )
        .expect("selector regex")
    });
    SELECTOR.captures_iter(text).map(|c| c[1].to_string()).collect()
}
