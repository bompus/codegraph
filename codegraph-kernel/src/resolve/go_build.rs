//! preferGoBuildTwin and goFileConstraint (name-matcher.ts, #2581): a name a
//! Go package defines once per build target resolves to the definition that
//! builds for the most typical target the reference's own file builds for.

use super::*;

/// The operating systems and architectures go/build reads from a file name
/// (its syslist).
const GO_KNOWN_OS: &[&str] = &[
    "aix", "android", "darwin", "dragonfly", "freebsd", "hurd", "illumos", "ios", "js", "linux",
    "nacl", "netbsd", "openbsd", "plan9", "solaris", "wasip1", "windows", "zos",
];
const GO_KNOWN_ARCH: &[&str] = &[
    "386", "amd64", "amd64p32", "arm", "armbe", "arm64", "arm64be", "loong64", "mips", "mipsle",
    "mips64", "mips64le", "mips64p32", "mips64p32le", "ppc", "ppc64", "ppc64le", "riscv", "riscv64",
    "s390", "s390x", "sparc", "sparc64", "wasm",
];
/// The systems the `unix` build tag holds for.
const GO_UNIX_OS: &[&str] = &[
    "aix", "android", "darwin", "dragonfly", "freebsd", "hurd", "illumos", "ios", "linux", "netbsd",
    "openbsd", "solaris",
];

#[derive(Clone, Copy)]
pub(super) struct GoTarget {
    goos: &'static str,
    goarch: &'static str,
}

/// GO_BUILD_TARGETS: the targets a Go name with build-constrained twins is
/// read for, the most typical first: a Linux server, a Mac, Windows, then
/// every other system on its usual architecture and Linux on every other
/// architecture.
fn go_build_targets() -> &'static [GoTarget] {
    static TARGETS: std::sync::OnceLock<Vec<GoTarget>> = std::sync::OnceLock::new();
    TARGETS.get_or_init(|| {
        let usual = |goos: &str| match goos {
            "js" | "wasip1" => "wasm",
            "ios" | "android" => "arm64",
            "aix" => "ppc64",
            "zos" => "s390x",
            "hurd" => "386",
            _ => "amd64",
        };
        let mut targets = vec![
            GoTarget { goos: "linux", goarch: "amd64" },
            GoTarget { goos: "darwin", goarch: "arm64" },
            GoTarget { goos: "windows", goarch: "amd64" },
            GoTarget { goos: "linux", goarch: "arm64" },
            GoTarget { goos: "darwin", goarch: "amd64" },
            GoTarget { goos: "freebsd", goarch: "amd64" },
        ];
        for goos in GO_KNOWN_OS {
            let goarch = usual(goos);
            if !targets.iter().any(|t| t.goos == *goos && t.goarch == goarch) {
                targets.push(GoTarget { goos, goarch });
            }
        }
        for goarch in GO_KNOWN_ARCH {
            if !targets.iter().any(|t| t.goos == "linux" && t.goarch == *goarch) {
                targets.push(GoTarget { goos: "linux", goarch });
            }
        }
        targets
    })
}

/// goBuildTagHolds: whether a build tag holds for a target, as the go command
/// sets them by default: the system and architecture (`android` is also
/// `linux`, `ios` `darwin`, `illumos` `solaris`), `unix` on a Unix system, the
/// `gc` compiler, cgo, and every `go1.N` release. Any other tag is unset.
fn go_tag_holds(tag: &str, t: &GoTarget) -> bool {
    if tag == t.goos || tag == t.goarch {
        return true;
    }
    match tag {
        "unix" => GO_UNIX_OS.contains(&t.goos),
        "linux" => t.goos == "android",
        "darwin" => t.goos == "ios",
        "solaris" => t.goos == "illumos",
        "gc" => true,
        "cgo" => t.goos != "js" && t.goos != "wasip1",
        _ => tag
            .strip_prefix("go1.")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
    }
}

/// A parsed build expression: a `//go:build` line, a `// +build` line, or a
/// file-name suffix as a test of a target.
enum BuildExpr {
    Tag(String),
    Not(Box<BuildExpr>),
    And(Box<BuildExpr>, Box<BuildExpr>),
    Or(Box<BuildExpr>, Box<BuildExpr>),
}

impl BuildExpr {
    fn holds(&self, t: &GoTarget) -> bool {
        match self {
            Self::Tag(s) => go_tag_holds(s, t),
            Self::Not(e) => !e.holds(t),
            Self::And(a, b) => a.holds(t) && b.holds(t),
            Self::Or(a, b) => a.holds(t) || b.holds(t),
        }
    }
}

fn build_tokens(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"&&") || b[i..].starts_with(b"||") {
            out.push(text[i..i + 2].to_string());
            i += 2;
            continue;
        }
        if matches!(b[i], b'(' | b')' | b'!') {
            out.push(text[i..i + 1].to_string());
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
            i += 1;
        }
        if i > start {
            out.push(text[start..i].to_string());
            continue;
        }
        // The upstream `\S` catch-all: a stray token the parser then rejects.
        out.push(text[i..i + 1].to_string());
        i += 1;
    }
    out
}

fn parse_or(toks: &[String], i: &mut usize) -> Option<BuildExpr> {
    let mut left = parse_and(toks, i)?;
    while toks.get(*i).is_some_and(|t| t == "||") {
        *i += 1;
        left = BuildExpr::Or(Box::new(left), Box::new(parse_and(toks, i)?));
    }
    Some(left)
}

fn parse_and(toks: &[String], i: &mut usize) -> Option<BuildExpr> {
    let mut left = parse_not(toks, i)?;
    while toks.get(*i).is_some_and(|t| t == "&&") {
        *i += 1;
        left = BuildExpr::And(Box::new(left), Box::new(parse_not(toks, i)?));
    }
    Some(left)
}

fn parse_not(toks: &[String], i: &mut usize) -> Option<BuildExpr> {
    let tok = toks.get(*i)?;
    *i += 1;
    match tok.as_str() {
        "!" => Some(BuildExpr::Not(Box::new(parse_not(toks, i)?))),
        "(" => {
            let inner = parse_or(toks, i)?;
            if toks.get(*i).is_some_and(|t| t == ")") {
                *i += 1;
                Some(inner)
            } else {
                None
            }
        }
        w if !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.') => {
            Some(BuildExpr::Tag(w.to_string()))
        }
        _ => None,
    }
}

/// goParseBuildExpr: a `//go:build` expression (`unix && !(linux || darwin)`)
/// as a test of a target, or None when it can't be read.
fn go_parse_build_expr(text: &str) -> Option<BuildExpr> {
    let toks = build_tokens(text);
    let mut i = 0;
    let expr = parse_or(&toks, &mut i)?;
    (i == toks.len()).then_some(expr)
}

/// goParsePlusBuild: a legacy `// +build` line's options (`linux,amd64
/// darwin`, `!plan9`) as a test of a target, or None when it can't be read.
fn go_parse_plus_build(text: &str) -> Option<BuildExpr> {
    let options: Option<Vec<Vec<(bool, String)>>> = text
        .split_whitespace()
        .map(|option| {
            option
                .split(',')
                .map(|term| {
                    let (neg, tag) = term
                        .strip_prefix('!')
                        .map_or((false, term), |t| (true, t));
                    (!tag.is_empty()
                        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.'))
                    .then(|| (neg, tag.to_string()))
                })
                .collect()
        })
        .collect();
    let mut options = options?.into_iter();
    let first = options.next()?;
    let mut acc = plus_option(first);
    for option in options {
        acc = BuildExpr::Or(Box::new(acc), Box::new(plus_option(option)));
    }
    Some(acc)
}

fn plus_option(terms: Vec<(bool, String)>) -> BuildExpr {
    let mut terms = terms.into_iter().map(|(neg, tag)| {
        let e = BuildExpr::Tag(tag);
        if neg {
            BuildExpr::Not(Box::new(e))
        } else {
            e
        }
    });
    let mut acc = terms.next().unwrap();
    for term in terms {
        acc = BuildExpr::And(Box::new(acc), Box::new(term));
    }
    acc
}

/// goFileConstraint's answer: every part must hold for the file to build.
pub(super) struct GoFileConstraint {
    parts: Vec<BuildExpr>,
}

impl GoFileConstraint {
    pub(super) fn holds(&self, t: &GoTarget) -> bool {
        self.parts.iter().all(|p| p.holds(t))
    }
}

impl KernelResolver {
    /// goFileConstraint: the targets a Go file builds for, as go/build reads
    /// them: a `_linux`, `_amd64` or `_windows_arm64` suffix on its name
    /// (before `_test`), and the `//go:build` line above its package clause,
    /// or the legacy `// +build` lines when it has none. None for a file that
    /// builds everywhere, or whose constraint can't be read.
    pub(super) fn go_file_constraint(&mut self, file: &str) -> Option<Rc<GoFileConstraint>> {
        if let Some(hit) = self.go_build_constraint_memo.get(file) {
            return hit.clone();
        }
        let mut parts: Vec<BuildExpr> = Vec::new();
        // go/build ignores what comes before the name's first `_`, so
        // `linux.go` names no system.
        let base = file
            .rsplit('/')
            .next()
            .unwrap_or(file)
            .split('.')
            .next()
            .unwrap_or("");
        if let Some(u) = base.find('_') {
            let mut suffix: Vec<&str> = base[u + 1..].split('_').collect();
            if suffix.last() == Some(&"test") {
                suffix.pop();
            }
            let last = suffix.last().copied();
            let prev = suffix.len().checked_sub(2).map(|i| suffix[i]);
            if let (Some(p), Some(l)) = (prev, last) {
                if GO_KNOWN_OS.contains(&p) && GO_KNOWN_ARCH.contains(&l) {
                    parts.push(BuildExpr::And(
                        Box::new(BuildExpr::Tag(p.to_string())),
                        Box::new(BuildExpr::Tag(l.to_string())),
                    ));
                } else if GO_KNOWN_OS.contains(&l) || GO_KNOWN_ARCH.contains(&l) {
                    parts.push(BuildExpr::Tag(l.to_string()));
                }
            } else if let Some(l) = last {
                if GO_KNOWN_OS.contains(&l) || GO_KNOWN_ARCH.contains(&l) {
                    parts.push(BuildExpr::Tag(l.to_string()));
                }
            }
        }
        // The constraint comes before the package clause, after only blank
        // lines and line comments.
        let mut go_build: Option<String> = None;
        let mut plus_build: Vec<String> = Vec::new();
        if let Some(lines) = self.read_file(file) {
            for line in lines.iter().take(200) {
                let text = line
                    .as_str()
                    .strip_prefix('\u{feff}')
                    .unwrap_or(line.as_str())
                    .trim();
                if text.is_empty() {
                    continue;
                }
                if !text.starts_with("//") {
                    break;
                }
                if go_build.is_none() {
                    if let Some(c) = re!(r"^//go:build\s+(.*)$").captures(text) {
                        go_build = Some(c[1].to_string());
                    }
                }
                if let Some(c) = re!(r"^//\s*\+build\s+(.*)$").captures(text) {
                    plus_build.push(c[1].to_string());
                }
            }
        }
        let mut readable = true;
        if let Some(g) = go_build {
            match go_parse_build_expr(&g) {
                Some(e) => parts.push(e),
                None => readable = false,
            }
        } else {
            for p in &plus_build {
                match go_parse_plus_build(p) {
                    Some(e) => parts.push(e),
                    None => readable = false,
                }
            }
        }
        let constraint = if !readable || parts.is_empty() {
            None
        } else {
            Some(Rc::new(GoFileConstraint { parts }))
        };
        self.go_build_constraint_memo.insert(file.to_string(), constraint.clone());
        constraint
    }

    /// The twins of `target`: same name, kind and qualified name in the same
    /// package (directory and clause), defined in different build files.
    fn go_build_twins(&mut self, target: &KNode) -> Res<Rc<Vec<Arc<KNode>>>> {
        if let Some(hit) = self.go_build_twins_memo.get(&target.id) {
            return Ok(hit.clone());
        }
        let dir = pos_dirname(&target.file_path);
        let clause = self.go_package_clause(&target.file_path);
        let list = self.nodes_by_name(&target.name)?;
        let mut twins = Vec::new();
        for n in list.iter() {
            if n.language == "go"
                && n.kind == target.kind
                && n.qualified_name == target.qualified_name
                && pos_dirname(&n.file_path) == dir
                && self.go_package_clause(&n.file_path) == clause
            {
                twins.push(n.clone());
            }
        }
        let twins = Rc::new(twins);
        self.go_build_twins_memo.insert(target.id.clone(), twins.clone());
        Ok(twins)
    }

    /// The twin the reference's file builds with, among candidates no
    /// resolution arm could collapse to one: same walk preferGoBuildTwin
    /// runs — the most typical target the reference's file builds for whose
    /// build admits exactly one. None when no target singles one out.
    pub(super) fn go_pick_twin(
        &mut self,
        cands: &[Arc<KNode>],
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        let own = self.go_file_constraint(&r.file_path);
        for t in go_build_targets() {
            if let Some(c) = &own {
                if !c.holds(t) {
                    continue;
                }
            }
            let mut built: Vec<Arc<KNode>> = Vec::new();
            for n in cands {
                if self
                    .go_file_constraint(&n.file_path)
                    .map(|c| c.holds(t))
                    .unwrap_or(true)
                {
                    built.push(n.clone());
                }
            }
            if built.len() == 1 {
                return Ok(Some(built.remove(0)));
            }
            if built.len() > 1 {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// preferGoBuildTwin: the definition Go builds, of a name its package
    /// defines more than once for different targets — `sep` in `sep_unix.go`
    /// (`//go:build unix`) and `sep_other.go` (`//go:build !unix`). The
    /// definition kept is the one that builds for the most typical target the
    /// reference's own file builds for: Linux on amd64 first, then a Mac,
    /// then Windows and the rest (GO_BUILD_TARGETS), so a file that only
    /// builds for Windows reaches the Windows definition. The result stands
    /// when no twin builds for those targets, or two do (#2581).
    pub(super) fn prefer_go_build_twin(
        &mut self,
        mut outcome: ResolveOutcome,
        r: &ResolveRefIn,
    ) -> Res<ResolveOutcome> {
        if r.language != "go" || outcome.status != "resolved" {
            return Ok(outcome);
        }
        let Some(tid) = outcome.target_node_id.clone() else {
            return Ok(outcome);
        };
        let Some(target) = self.node_by_id(&tid)? else {
            return Ok(outcome);
        };
        // A file that builds everywhere can have no twin: the package would
        // declare the name twice.
        if target.language != "go" || self.go_file_constraint(&target.file_path).is_none() {
            return Ok(outcome);
        }
        let twins = self.go_build_twins(&target)?;
        if twins.len() < 2 {
            return Ok(outcome);
        }
        let own = self.go_file_constraint(&r.file_path);
        for t in go_build_targets() {
            if let Some(c) = &own {
                if !c.holds(t) {
                    continue;
                }
            }
            let mut built: Vec<Arc<KNode>> = Vec::new();
            for n in twins.iter() {
                if self
                    .go_file_constraint(&n.file_path)
                    .map(|c| c.holds(t))
                    .unwrap_or(true)
                {
                    built.push(n.clone());
                }
            }
            if built.len() == 1 {
                outcome.target_node_id = Some(built[0].id.clone());
                return Ok(outcome);
            }
            if built.len() > 1 {
                return Ok(outcome);
            }
        }
        Ok(outcome)
    }
}
