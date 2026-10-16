//! Is a C function defined only for a build that turns on a flag nothing in the project turns on
//! (c-feature-variants.ts, upstream #2615)? lua-cjson's fpconv.h defines `static inline void
//! fpconv_init()` under `#ifdef USE_INTERNAL_FPCONV` and otherwise declares it `extern`, for
//! fpconv.c to define. Nothing in redis's tree sets USE_INTERNAL_FPCONV, so its build compiles
//! fpconv.c's.
//!
//! The overload choice asks this only when choosing among definitions of one name, where a call
//! has to reach one of them (cpp_function_overload): of a flag nothing sets, the build to follow
//! is the one that leaves it off. Whether a macro is visible at a call (cpp.rs) keeps such a flag
//! unknown instead, since there an unknown flag must not suppress anything.
//!
//! A branch is off only when flags decide its test: `#ifdef X`, `#ifndef X`, `#if defined(X)`,
//! `#if X`, `#if 0`, with `!`, `&&`, `||` and parentheses, and the `#elif` and `#else` after
//! them. A flag is set nowhere when the definition's own file doesn't `#define` it, no build file
//! in the directories from the project root down to the definition or the call names it (a
//! Makefile's `-DX`, CMake's `option(X …)`, configure's `AC_DEFINE(X)`) and no C or C++ file of
//! their components does, a component being the tree under the nearest directory with a build
//! file, redis's `deps/lua/src` for fpconv.h. A macro of the compiler or the platform, a reserved
//! name like `_WIN32` or `__linux__`, or `WIN32`, `linux`, `unix`, is unknown, and so is any
//! other test (`#if LEVEL > 1`).

use super::cpp::{and, directive_lines, not, or, Truth};
use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Op { If, Ifdef, Ifndef, Elifdef, Elifndef, Elif, Else, Endif }

struct Directive { op: Op, expression: String, line: i64 }

struct FileDirectives { conditionals: Vec<Directive>, defines: HashSet<String> }

#[derive(Default)]
pub(super) struct VariantCache {
    files: HashMap<String, Rc<FileDirectives>>,
    /// Directory → the text of the build files directly in it.
    build_text: HashMap<String, Rc<str>>,
    /// Directory → the nearest directory at or above it with a build file (`` for the project root).
    components: HashMap<String, String>,
    /// The project's C and C++ files, once.
    c_files: Option<Rc<Vec<String>>>,
    /// (flag, component) → whether a C or C++ file of the component `#define`s it.
    defines: HashMap<(String, String), bool>,
    answers: HashMap<String, bool>,
}

const PLATFORM_FLAGS: [&str; 4] = ["WIN32", "WIN64", "linux", "unix"];
const BUILD_FILES: [&str; 15] = [
    "Makefile", "makefile", "GNUmakefile", "Makefile.am", "Makefile.in", "CMakeLists.txt", "meson.build",
    "meson_options.txt", "configure.ac", "configure.in", "config.h.in", "BUILD", "BUILD.bazel", "SConstruct", "SConscript",
];

fn dir_of(file: &str) -> &str {
    file.rfind('/').map_or("", |at| &file[..at])
}

/// `a/b/c.h` → `a/b`, `a`, `` (the project root).
fn ancestors(file: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut dir = dir_of(file);
    loop {
        dirs.push(dir.to_string());
        if dir.is_empty() { return dirs; }
        dir = dir_of(dir);
    }
}

impl KernelResolver {
    /// Is `function`'s definition compiled only by a build that sets a flag nothing in the project
    /// sets, for the call `r`? A branch around both the definition and the call, in the call's own
    /// file, decides nothing.
    pub(super) fn is_c_feature_variant(&mut self, function: &KNode, r: &ResolveRefIn) -> bool {
        let same_file = function.file_path == r.file_path;
        let key = format!("v\0{}\0{}", function.id, if same_file { r.line.to_string() } else { dir_of(&r.file_path).to_string() });
        if let Some(&hit) = self.c_variants.answers.get(&key) { return hit; }
        let file = self.variant_directives(&function.file_path);
        let mut groups = enclosing_tests(&file.conditionals, function.start_line);
        if same_file {
            let around = enclosing_tests(&file.conditionals, r.line);
            let shared = groups.iter().zip(&around).take_while(|(a, b)| a[0] == b[0] && a.len() == b.len()).count();
            groups.drain(..shared);
        }
        let off = !groups.is_empty() && self.variant_off(&groups, &file, &[function.file_path.as_str(), r.file_path.as_str()]);
        self.c_variants.answers.insert(key, off);
        off
    }

    /// Is line `line` of `path` compiled only by a build that sets a flag nothing in the project sets?
    pub(super) fn is_off_by_default(&mut self, path: &str, line: i64) -> bool {
        let key = format!("o\0{path}\0{line}");
        if let Some(&hit) = self.c_variants.answers.get(&key) { return hit; }
        let file = self.variant_directives(path);
        let groups = enclosing_tests(&file.conditionals, line);
        let off = !groups.is_empty() && self.variant_off(&groups, &file, &[path]);
        self.c_variants.answers.insert(key, off);
        off
    }

    /// Is a branch of `groups` off when no flag `scope`'s files see is set?
    fn variant_off(&mut self, groups: &[Vec<usize>], file: &FileDirectives, scope: &[&str]) -> bool {
        let mut dirs: Vec<String> = Vec::new();
        for f in scope {
            for dir in ancestors(f) { if !dirs.contains(&dir) { dirs.push(dir); } }
        }
        let mut components: Vec<String> = Vec::new();
        for f in scope {
            let component = self.variant_component(dir_of(f));
            if !components.contains(&component) { components.push(component); }
        }
        let mut flag = |name: &str| -> Truth {
            if name.starts_with('_') || PLATFORM_FLAGS.contains(&name) || file.defines.contains(name) { return None; }
            // A word of its own, or a compiler flag's `-DX` / `/DX`.
            let Ok(named) = shared_regex(&format!(r"(?m)(?:^|[^\w]|[-/]D){}\b", regex::escape(name))) else { return None };
            if dirs.iter().any(|dir| named.is_match(&self.variant_build_text(dir))) { return None; }
            if components.iter().any(|component| self.variant_defined_in(name, component)) { None } else { Some(false) }
        };
        groups.iter().any(|tests| {
            let tests: Vec<&Directive> = tests.iter().map(|&i| &file.conditionals[i]).collect();
            branch_truth(&tests, &mut flag) == Some(false)
        })
    }

    fn variant_directives(&mut self, file: &str) -> Rc<FileDirectives> {
        if let Some(hit) = self.c_variants.files.get(file) { return hit.clone(); }
        let text = self.read_file(file).map(|s| s.text().to_string()).unwrap_or_default();
        let lines = directive_lines(&text);
        let mut conditionals = Vec::new();
        let mut defines = HashSet::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(define) = re!(r"^\s*#\s*define\s+(\w+)").captures(line) {
                defines.insert(define[1].to_string());
                continue;
            }
            let Some(m) = re!(r"^\s*#\s*(ifdef|ifndef|if|elifdef|elifndef|elif|else|endif)\b(.*)$").captures(line) else { continue };
            let op = match &m[1] {
                "ifdef" => Op::Ifdef, "ifndef" => Op::Ifndef, "if" => Op::If, "elifdef" => Op::Elifdef,
                "elifndef" => Op::Elifndef, "elif" => Op::Elif, "else" => Op::Else, _ => Op::Endif,
            };
            // A test continued with a backslash goes on over the next lines.
            let mut expression = m[2].to_string();
            let mut j = i;
            while re!(r"\\\s*$").is_match(&lines[j]) && j + 1 < lines.len() {
                expression = format!("{} {}", re!(r"\\\s*$").replace(&expression, ""), lines[j + 1]);
                j += 1;
            }
            conditionals.push(Directive { op, expression, line: i as i64 + 1 });
        }
        let directives = Rc::new(FileDirectives { conditionals, defines });
        self.c_variants.files.insert(file.to_string(), directives.clone());
        directives
    }

    fn variant_build_text(&mut self, dir: &str) -> Rc<str> {
        if let Some(hit) = self.c_variants.build_text.get(dir) { return hit.clone(); }
        let text: Vec<String> = BUILD_FILES.iter().map(|name| {
            let path = if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") };
            self.read_file_uncached(&path).map(|s| s.text().to_string()).unwrap_or_default()
        }).collect();
        let text: Rc<str> = text.join("\n").into();
        self.c_variants.build_text.insert(dir.to_string(), text.clone());
        text
    }

    /// The nearest directory at or above `dir` with a build file, else the project root. An empty
    /// build file bounds a component as well.
    fn variant_component(&mut self, dir: &str) -> String {
        if let Some(hit) = self.c_variants.components.get(dir) { return hit.clone(); }
        let component = if dir.is_empty() || BUILD_FILES.iter().any(|name| self.file_exists(&format!("{dir}/{name}"))) { dir.to_string() } else { self.variant_component(dir_of(dir)) };
        self.c_variants.components.insert(dir.to_string(), component.clone());
        component
    }

    /// Does a C or C++ file of `component` `#define` `name`? Read only from files whose bytes hold
    /// the name; a `#define` in a comment or raw string is none, and a file of a component nested
    /// inside it (under a directory with a build file of its own) is that component's.
    fn variant_defined_in(&mut self, name: &str, component: &str) -> bool {
        let key = (name.to_string(), component.to_string());
        if let Some(&hit) = self.c_variants.defines.get(&key) { return hit; }
        let files = match self.c_variants.c_files.clone() {
            Some(files) => files,
            None => {
                let all: Vec<String> = match self.sorted_files() {
                    Some(files) => files.to_vec(),
                    None => self.table().map(|t| t.files.iter().cloned().collect()).unwrap_or_default(),
                };
                let files = Rc::new(all.into_iter().filter(|f| re!(r"(?i)\.(c|h|cc|cpp|cxx|c\+\+|hpp|hh|hxx|h\+\+|inl|ipp|tcc|inc|m|mm)$").is_match(f)).collect::<Vec<_>>());
                self.c_variants.c_files = Some(files.clone());
                files
            }
        };
        let prefix = if component.is_empty() { String::new() } else { format!("{component}/") };
        let defined = shared_regex(&format!(r"(?m)^[ \t]*#[ \t]*define[ \t]+{}\b", regex::escape(name))).is_ok_and(|define| {
            files.iter().any(|file| file.starts_with(&prefix)
                && self.read_file_uncached(file).is_some_and(|s| s.text().contains(name) && define.is_match(s.text())
                    && directive_lines(s.text()).iter().any(|line| define.is_match(line)))
                && self.variant_component(dir_of(file)) == component)
        });
        self.c_variants.defines.insert(key, defined);
        defined
    }
}

/// The branches around `line`, innermost last: for each, the tests of its group's branches up to
/// and including the one `line` is in (`#else` has no test), as indexes into `conditionals`.
fn enclosing_tests(conditionals: &[Directive], line: i64) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, d) in conditionals.iter().enumerate() {
        if d.line >= line { break; }
        match d.op {
            Op::If | Op::Ifdef | Op::Ifndef => groups.push(vec![i]),
            Op::Endif => { groups.pop(); }
            _ => { if let Some(group) = groups.last_mut() { group.push(i); } }
        }
    }
    groups
}

/// Whether the last branch of `tests` is taken: no earlier test held, and its own does.
fn branch_truth(tests: &[&Directive], flag: &mut dyn FnMut(&str) -> Truth) -> Truth {
    let mut earlier: Truth = Some(false);
    for d in &tests[..tests.len() - 1] {
        earlier = or(earlier, test_truth(d, flag));
        if earlier == Some(true) { return Some(false); }
    }
    and(not(earlier), test_truth(tests[tests.len() - 1], flag))
}

fn test_truth(d: &Directive, mut flag: &mut dyn FnMut(&str) -> Truth) -> Truth {
    let trimmed = d.expression.trim();
    let name = (!trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')).then_some(trimmed);
    match d.op {
        Op::Ifdef | Op::Elifdef => name.and_then(&mut flag),
        Op::Ifndef | Op::Elifndef => not(name.and_then(&mut flag)),
        Op::Else => Some(true),
        _ => evaluate(&d.expression, flag),
    }
}

/// A `#if` expression of `defined`, flags, integers, `!`, `&&`, `||` and parentheses; anything
/// else is unknown. A flag that is set nowhere reads as `0`, as the preprocessor reads a name no
/// `#define` gave a value.
fn evaluate(expression: &str, flag: &mut dyn FnMut(&str) -> Truth) -> Truth {
    let tokens: Vec<&str> = re!(r"(?i)0x[\da-f]+\w*|\d\w*|[A-Za-z_]\w*|&&|\|\||\S").find_iter(expression).map(|m| m.as_str()).collect();
    // Check the shape first, so an unsupported test reads no flag.
    let mut i = 0;
    if tokens.is_empty() || !shape(&tokens, &mut i) || i != tokens.len() { return None; }
    let mut i = 0;
    disjunction(&tokens, &mut i, flag)
}

fn is_identifier(t: &str) -> bool {
    re!(r"^[A-Za-z_]\w*$").is_match(t)
}

fn shape(tokens: &[&str], i: &mut usize) -> bool {
    if !shape_term(tokens, i) { return false; }
    while matches!(tokens.get(*i), Some(&"&&") | Some(&"||")) {
        *i += 1;
        if !shape_term(tokens, i) { return false; }
    }
    true
}

fn shape_term(tokens: &[&str], i: &mut usize) -> bool {
    let t = tokens.get(*i).copied();
    *i += 1;
    match t {
        Some("!") => shape_term(tokens, i),
        Some("(") => {
            if !shape(tokens, i) { return false; }
            let close = tokens.get(*i) == Some(&")");
            *i += 1;
            close
        }
        Some("defined") => {
            let paren = tokens.get(*i) == Some(&"(");
            if paren { *i += 1; }
            let named = tokens.get(*i).is_some_and(|t| is_identifier(t));
            *i += 1;
            named && (!paren || { let close = tokens.get(*i) == Some(&")"); *i += 1; close })
        }
        Some(t) => re!(r"(?i)^(0x[\da-f]+|\d+)[ul]*$").is_match(t) || is_identifier(t),
        None => false,
    }
}

fn disjunction(tokens: &[&str], i: &mut usize, flag: &mut dyn FnMut(&str) -> Truth) -> Truth {
    let mut value = conjunction(tokens, i, flag);
    while tokens.get(*i) == Some(&"||") {
        *i += 1;
        value = or(value, conjunction(tokens, i, flag));
    }
    value
}

fn conjunction(tokens: &[&str], i: &mut usize, flag: &mut dyn FnMut(&str) -> Truth) -> Truth {
    let mut value = term(tokens, i, flag);
    while tokens.get(*i) == Some(&"&&") {
        *i += 1;
        value = and(value, term(tokens, i, flag));
    }
    value
}

fn term(tokens: &[&str], i: &mut usize, flag: &mut dyn FnMut(&str) -> Truth) -> Truth {
    let t = tokens[*i];
    *i += 1;
    match t {
        "!" => not(term(tokens, i, flag)),
        "(" => {
            let value = disjunction(tokens, i, flag);
            *i += 1;
            value
        }
        "defined" => {
            if tokens.get(*i) == Some(&"(") {
                let value = flag(tokens[*i + 1]);
                *i += 3;
                value
            } else {
                let value = flag(tokens[*i]);
                *i += 1;
                value
            }
        }
        _ if t.as_bytes()[0].is_ascii_digit() => {
            // (Nonzero when any digit is: `0x10`, `1UL`.)
            let digits = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t).trim_end_matches(['u', 'l', 'U', 'L']);
            Some(digits.bytes().any(|b| b != b'0'))
        }
        _ => if flag(t) == Some(false) { Some(false) } else { None },
    }
}
