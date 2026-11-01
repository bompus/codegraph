//! matchRubyConstantPathRef (upstream src/resolution/ruby-constant-paths.ts)
//! and the Ruby class/instance method split (upstream #2834).
//!
//! A namespaced Ruby constant — `Billing::ChargeCard`, `::Foo`,
//! `::Billing::ChargeCard` — resolves only to a constant the project itself
//! declares, looked up from the lexical namespace around the reference
//! outward and then at top level (an absolute `::` path searches top level
//! only). A call on that path resolves a method of the class or module the
//! constant names, through its Ruby ancestry. A path the project does not
//! declare, or a method it does not have, resolves to nothing rather than
//! falling back to a same-name guess (#2827, fixing #2503).
//!
//! `def self.m` and `def m` share `Owner::m` in the index; `is_static`
//! distinguishes them. A call on a class constant prefers the class
//! method, one on a receiver `x = Widget.new` prefers the instance method,
//! and a bare call prefers the kind of the method it is written inside —
//! but only when both kinds really exist under the same qualified name,
//! never when the receiver is just uncertain (#2834).

use super::*;

impl KernelResolver {
    /// matchRubyConstantPathRef: `references` names that are constant paths
    /// resolve to the constant; `calls` names `Path.method` resolve to the
    /// owner's method. Returns `Some` (resolved or unresolved) whenever the
    /// ref spells a constant path — the claim is exclusive — and `None` for
    /// every other ref.
    pub(super) fn resolve_ruby_constant_path_ref(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.language != "ruby" {
            return Ok(None);
        }
        // A bare `Foo` or `Foo.call` is left to the name strategies; only a
        // path written with its namespace (`Foo::Bar`, `::Foo`) is claimed.
        let namespaced = |c: &regex::Captures| c.get(1).is_some() || c[2].contains("::");
        if r.reference_kind == "references" {
            let Some(c) = re!(r"^(::)?([A-Z]\w*(?:::[A-Z]\w*)*)$").captures(r.reference_name.trim_start_matches("()")) else {
                return Ok(None);
            };
            if !namespaced(&c) {
                return Ok(None);
            }
            return match self.ruby_constants_named(&c[2], r, c.get(1).is_some())?.into_iter().next() {
                Some(target) => Ok(Some(ResolveOutcome::resolved(&target, 0.9, "qualified-name", true, None))),
                None => Ok(Some(ResolveOutcome::unresolved())),
            };
        }
        if r.reference_kind == "calls" {
            // A call on an instance `Foo.new(…)` made, bare or namespaced
            // (`new Foo().m`, `new A::B().m!`): an instance method of the class
            // Ruby's lookup finds for `Foo` there, its own or one its ancestry
            // declares — never a namesake's by the method name alone. A class
            // the project doesn't declare (rack-protection's `Request.new(env)`
            // is rack's) has no method here.
            if let Some(c) = re!(r"^new (::)?([A-Z]\w*(?:::[A-Z]\w*)*)\(\)\.([A-Za-z_]\w*[?!=]?)$").captures(&r.reference_name) {
                let class = self
                    .ruby_constants_named(&c[2], r, c.get(1).is_some())?
                    .into_iter()
                    .next()
                    .filter(|n| n.kind == "class");
                let method = match class {
                    Some(class) => self.ruby_method_of(&class.qualified_name, &c[3], r, "instance")?,
                    None => None,
                };
                return Ok(Some(match method {
                    Some((method, own)) => ResolveOutcome::resolved(&method, if own { 0.9 } else { 0.85 }, "instance-method", true, None),
                    None => ResolveOutcome::unresolved(),
                }));
            }
            // A call on an instance variable `@writer.flush`, typed by the
            // class's `initialize`: an instance method of the class every
            // assignment there gives the variable — or nothing when that
            // class is not the project's. A variable `initialize` does not
            // type is left to the other strategies, as before (#3053).
            if let Some(c) = re!(r"^@(\w+)\.([A-Za-z_]\w*[?!=]?)$").captures(&r.reference_name) {
                let Some(class) = self.ruby_ivar_class(&c[1], r)? else {
                    return Ok(None);
                };
                let method = match class {
                    Some(class) => self.ruby_method_of(&class.qualified_name, &c[2], r, "instance")?,
                    None => None,
                };
                return Ok(Some(match method {
                    Some((method, own)) => ResolveOutcome::resolved(&method, if own { 0.85 } else { 0.8 }, "instance-method", true, None),
                    None => ResolveOutcome::unresolved(),
                }));
            }
            let Some(c) = re!(r"^(::)?([A-Z]\w*(?:::[A-Z]\w*)*)\.([A-Za-z_]\w*[?!=]?)$").captures(r.reference_name.trim_start_matches("()")) else {
                return Ok(None);
            };
            if !namespaced(&c) {
                return Ok(None);
            }
            let owner = self
                .ruby_constants_named(&c[2], r, c.get(1).is_some())?
                .into_iter()
                .find(|n| matches!(n.kind.as_str(), "class" | "module"));
            let Some(owner) = owner else {
                return Ok(Some(ResolveOutcome::unresolved()));
            };
            return match self.ruby_method_of(&owner.qualified_name, &c[3], r, "class")? {
                Some((method, own)) => Ok(Some(ResolveOutcome::resolved(
                    &method,
                    if own { 0.85 } else { 0.8 },
                    "qualified-name",
                    true,
                    None,
                ))),
                None => Ok(Some(ResolveOutcome::unresolved())),
            };
        }
        Ok(None)
    }

    /// constantsNamed: the declarations of the first of `<scope>::<path>`,
    /// `<outer>::<path>`, … `<path>` the project declares — the scope being
    /// the class or module around the reference, `<path>` alone for a
    /// `::`-rooted name. Production code never names a constant only a test
    /// declares.
    fn ruby_constants_named(&mut self, path: &str, r: &ResolveRefIn, absolute: bool) -> Res<Vec<Arc<KNode>>> {
        let production = !super::resolver_upstream::test_suite_path(&r.file_path);
        let mut prefix = if absolute { String::new() } else { self.ruby_lexical_scope(r)? };
        loop {
            let qn = if prefix.is_empty() { path.to_string() } else { format!("{prefix}::{path}") };
            let found: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&qn)?
                .iter()
                .filter(|n| {
                    n.language == "ruby"
                        && matches!(n.kind.as_str(), "class" | "module" | "variable" | "constant")
                        && !(production && super::resolver_upstream::test_suite_path(&n.file_path))
                })
                .cloned()
                .collect();
            if !found.is_empty() {
                return Ok(ruby_ordered(found, r));
            }
            if prefix.is_empty() {
                return Ok(Vec::new());
            }
            prefix = prefix.rfind("::").map_or(String::new(), |i| prefix[..i].to_string());
        }
    }

    /// lexicalScope: the qualified name of the innermost class or module
    /// around `r`, or empty at the top level of its file.
    fn ruby_lexical_scope(&mut self, r: &ResolveRefIn) -> Res<String> {
        Ok(self.ruby_innermost_namespace(r)?.map(|i| i.qualified_name.clone()).unwrap_or_default())
    }

    /// innermostNamespace: the innermost class or module around `r`, if any.
    fn ruby_innermost_namespace(&mut self, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let mut inner: Option<Arc<KNode>> = None;
        for n in self.nodes_in_file(&r.file_path)?.iter() {
            if n.language != "ruby" || !matches!(n.kind.as_str(), "class" | "module") || n.start_line > r.line || n.end_line < r.line {
                continue;
            }
            if inner.as_ref().is_none_or(|i| n.start_line > i.start_line || (n.start_line == i.start_line && n.start_column > i.start_column)) {
                inner = Some(n.clone());
            }
        }
        Ok(inner)
    }

    /// ivarClass (#3053): `Some` the class of what `@name` holds where `r`
    /// is written — `Some(None)` when that class is not the project's —
    /// or `None` for the other strategies when `r` is not in an instance
    /// method of a class, or `initialize` does not type the variable.
    fn ruby_ivar_class(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Option<Arc<KNode>>>> {
        let caller = self.node_by_id(&r.from_node_id)?;
        match &caller {
            Some(c) if c.language == "ruby" && c.kind == "method" && !c.is_static => {}
            _ => return Ok(None),
        }
        let Some(owner) = self.ruby_innermost_namespace(r)? else {
            return Ok(None);
        };
        if owner.kind != "class" {
            return Ok(None);
        }
        let key = format!("{}\0{}", owner.qualified_name, name);
        if let Some(hit) = self.ruby_ivar_memo.get(&key) {
            return Ok(hit.clone());
        }
        let found = self.ruby_ivar_class_from_initialize(name, &owner.qualified_name, r)?;
        self.ruby_ivar_memo.insert(key, found.clone());
        Ok(found)
    }

    /// ivarClassFromInitialize: every assignment to `@name` in the owner's
    /// `initialize` must be at a statement's start and a lone `Const.new(…)`
    /// of one constant — then the class Ruby's lookup finds for that
    /// constant from there.
    fn ruby_ivar_class_from_initialize(&mut self, name: &str, owner: &str, r: &ResolveRefIn) -> Res<Option<Option<Arc<KNode>>>> {
        let mut written: Option<(String, bool, String, i64)> = None;
        for init in self.nodes_by_qualified_name(&format!("{owner}::initialize"))?.iter() {
            if init.kind != "method" || init.language != "ruby" || init.is_static {
                continue;
            }
            let Some(lines) = self.read_file(&init.file_path) else {
                return Ok(None);
            };
            let mut body = String::new();
            for i in init.start_line - 1..init.end_line {
                match lines.line(i as usize) {
                    Some(l) => {
                        body.push_str(l);
                        body.push('\n');
                    }
                    None => break,
                }
            }
            let body = strip_ruby_comments(&body);
            let Some(assigned) = constructed_assignments(&body, name) else {
                return Ok(None);
            };
            for (path, absolute, line) in assigned {
                match &written {
                    Some((p, a, _, _)) if *p != path || *a != absolute => return Ok(None),
                    None => written = Some((path, absolute, init.file_path.clone(), init.start_line + line)),
                    _ => {}
                }
            }
        }
        let Some((path, absolute, file, line)) = written else {
            return Ok(None);
        };
        let mut at = r.clone();
        at.file_path = file;
        at.line = line;
        at.reference_name = path.clone();
        at.reference_kind = "references".to_string();
        let class = self.ruby_constants_named(&path, &at, absolute)?.into_iter().next();
        Ok(Some(class.filter(|n| n.kind == "class")))
    }

    /// methodOf: `name` declared on `owner_qn` itself (kind `class` first),
    /// else on the first ancestor in its Ruby ancestry that declares it.
    /// Returns the method and whether it is the owner's own. `r` is the call
    /// site: production code never reaches a method only a test declares.
    pub(super) fn ruby_method_of(&mut self, owner_qn: &str, name: &str, r: &ResolveRefIn, kind: &str) -> Res<Option<(Arc<KNode>, bool)>> {
        let production = !super::resolver_upstream::test_suite_path(&r.file_path);
        let own: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{owner_qn}::{name}"))?
            .iter()
            // A receiver bound to an instance (`new C().m`, a typed `@x`) can
            // only reach instance methods: an own class method does not stand
            // in for one an ancestor declares.
            .filter(|n| n.language == "ruby" && n.kind == "method" && !(kind == "instance" && n.is_static) && !(production && super::resolver_upstream::test_suite_path(&n.file_path)))
            .cloned()
            .collect();
        if let Some(m) = ruby_methods_of_kind_first(ruby_ordered(own, r), kind).into_iter().next() {
            return Ok(Some((m, true)));
        }
        let ancestry = self.ruby_ancestry(owner_qn)?;
        let mut ancestors: Vec<&String> = ancestry.iter().filter(|q| q.as_str() != owner_qn).collect();
        ancestors.sort();
        for qn in ancestors {
            let found: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&format!("{qn}::{name}"))?
                .iter()
                .filter(|n| n.language == "ruby" && n.kind == "method" && !(kind == "instance" && n.is_static) && !(production && super::resolver_upstream::test_suite_path(&n.file_path)))
                .cloned()
                .collect();
            if let Some(m) = ruby_methods_of_kind_first(ruby_ordered(found, r), kind).into_iter().next() {
                return Ok(Some((m, false)));
            }
        }
        Ok(None)
    }

    /// rubyMethodsOfCallerKind: of the methods `build` could mean, the ones
    /// whose `is_static` matches the enclosing method's stay. Applies only
    /// when a qualified name holds both kinds and the caller is itself a
    /// Ruby method — a bare call from a block or the file's top level keeps
    /// every candidate.
    pub(super) fn ruby_methods_of_caller_kind(&mut self, methods: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let mut static_qns = HashSet::new();
        let mut instance_qns = HashSet::new();
        for m in methods.iter().filter(|m| m.language == "ruby" && m.kind == "method") {
            if m.is_static {
                static_qns.insert(m.qualified_name.clone());
            } else {
                instance_qns.insert(m.qualified_name.clone());
            }
        }
        let twins: HashSet<String> = static_qns.intersection(&instance_qns).cloned().collect();
        if twins.is_empty() {
            return Ok(methods);
        }
        let caller_static = match self.node_by_id(&r.from_node_id)? {
            Some(c) if c.language == "ruby" && c.kind == "method" => c.is_static,
            _ => return Ok(methods),
        };
        Ok(methods
            .into_iter()
            .filter(|m| !(m.language == "ruby" && m.kind == "method" && twins.contains(&m.qualified_name) && m.is_static != caller_static))
            .collect())
    }
}

/// `ordered`: anything before a value, then the reference's own file, then
/// production code; the index's order within each.
fn ruby_ordered(mut nodes: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Vec<Arc<KNode>> {
    if nodes.len() < 2 {
        return nodes;
    }
    let rank = |n: &KNode| {
        matches!(n.kind.as_str(), "variable" | "constant") as u8 * 4
            + (n.file_path != r.file_path) as u8 * 2
            + super::resolver_upstream::test_suite_path(&n.file_path) as u8
    };
    nodes.sort_by_key(|n| rank(n));
    nodes
}

/// rubyMethodsOfKindFirst: methods whose `is_static` matches the wanted
/// kind, then the rest in their original order. `kind` is `"class"` for
/// calls on a class or module constant and `"instance"` for calls on a
/// receiver bound to an instance.
pub(super) fn ruby_methods_of_kind_first(methods: Vec<Arc<KNode>>, kind: &str) -> Vec<Arc<KNode>> {
    let want = kind == "class";
    let (mut same, other): (Vec<Arc<KNode>>, Vec<Arc<KNode>>) = methods.into_iter().partition(|m| m.is_static == want);
    same.extend(other);
    same
}

/// constructedAssignments (#3053): each assignment to `@name` in `body`
/// (an `initialize`, comments blanked): the constant whose `.new` it
/// assigns, and the assignment's line within `body`. None when one of
/// them is anything else: not at the start of a statement (`@a, @b = …`),
/// or not a lone `Const.new(…)` (`Foo.new.tap {…}`, `writer || Foo.new`,
/// `build_writer`).
fn constructed_assignments(body: &str, name: &str) -> Option<Vec<(String, bool, i64)>> {
    // Upstream's `/(?<![@\w])@name\s*(?:…)?=(?![=~>])/g`, minus the
    // look-around Rust's regex lacks: the boundary char is captured and
    // skipped, the `=~>` tail is checked by hand.
    let target = super::cached_pattern(&format!(r"(^|[^@\w])@{}\s*(?:\|\||&&|\*\*|<<|>>|[-+*/%|&^])?=", regex::escape(name))).ok()?;
    let mut out = Vec::new();
    for m in target.captures_iter(body) {
        let whole = m.get(0).unwrap();
        let at = whole.start() + m.get(1).unwrap().as_str().len();
        let after_eq = whole.end();
        match body.as_bytes().get(after_eq) {
            Some(b'=') | Some(b'~') | Some(b'>') => continue,
            _ => {}
        }
        let statement_start = body[..at].rfind(['\n', ';']).map_or(0, |i| i + 1);
        if !body[statement_start..at].trim().is_empty() {
            return None;
        }
        let (path, absolute) = constructed_value(body, after_eq)?;
        let line = body[..at].matches('\n').count() as i64;
        out.push((path, absolute, line));
    }
    Some(out)
}

/// constructedValue: the constant a value written at `from` constructs
/// when it is a lone `Const.new`, `A::B.new(…)` or `::Foo.new …`: its
/// arguments in parentheses or without them, a block, or a statement
/// modifier may follow, but nothing the value is passed through
/// (`.tap`, `||`). None otherwise.
fn constructed_value(s: &str, from: usize) -> Option<(String, bool)> {
    let m = re!(r"^\s*(::)?([A-Z]\w*(?:::[A-Z]\w*)*)\s*\.\s*new").captures(&s[from..])?;
    let mut i = from + m.get(0).unwrap().end();
    // upstream's `new(?![\w?!])` (`\w` is ASCII): `new?`/`new!`/`newly`
    // are method names.
    match s[i..].chars().next() {
        Some(c) if c.is_ascii_alphanumeric() || matches!(c, '_' | '?' | '!') => return None,
        _ => {}
    }
    if s.as_bytes().get(i) == Some(&b'(') {
        i = skip_parens(s, i)?;
    }
    // The end of the statement, a block, or (after a space) a modifier or
    // arguments without parentheses: anything but a chain or an operator.
    let tail = &s[i.min(s.len())..];
    let ok = match tail.chars().next() {
        None | Some('\n') | Some(';') | Some('{') => true,
        _ => re!(r#"^[ \t]+(?:$|\n|;|\{|[\w:@$"'\[])"#).is_match(tail.get(..200).unwrap_or(tail)),
    };
    // A chain may go on at the start of the next line: `Foo.new\n  .tap {…}`.
    let continued = re!(r"^[ \t]*\n\s*&?\.").is_match(tail.get(..200).unwrap_or(tail));
    (ok && !continued).then(|| (m[2].to_string(), m.get(1).is_some()))
}

/// skipParens: the index past the parenthesized group opening at `i`,
/// Ruby strings inside it skipped; None when it never closes.
fn skip_parens(s: &str, i: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut j = i;
    while j < b.len() {
        let c = b[j];
        if c == b'"' || c == b'\'' {
            j += 1;
            while j < b.len() && b[j] != c {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            j += 1;
            continue;
        }
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(j + 1);
            }
        }
        j += 1;
    }
    None
}

/// stripCommentsForRegex(…, 'ruby') (upstream strip-comments.ts):
/// `#` line comments and `=begin`/`=end` blocks blanked (newlines kept,
/// so offsets still map); string literals skipped intact.
fn strip_ruby_comments(src: &str) -> String {
    let s: Vec<char> = src.chars().collect();
    let n = s.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut at_line_start = true;
    while i < n {
        let c = s[i];
        // `=begin` opens a block comment only at a line's start (after
        // optional whitespace); `=end` must sit at a line's start too.
        if at_line_start && c == '=' && s[i..].iter().take(6).collect::<String>() == "=begin" {
            let start = i;
            i += 6;
            while i < n {
                if s[i] == '\n' {
                    let mut j = i + 1;
                    while j < n && (s[j] == ' ' || s[j] == '\t') {
                        j += 1;
                    }
                    if s[j..].iter().take(4).collect::<String>() == "=end" {
                        i = j + 4;
                        while i < n && s[i] != '\n' {
                            i += 1;
                        }
                        break;
                    }
                }
                i += 1;
            }
            for &ch in &s[start..i.min(n)] {
                super::awaited::push_blank(&mut out, ch);
            }
            at_line_start = i > 0 && s[i - 1] == '\n';
            continue;
        }
        if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            while i < n && s[i] != c {
                if s[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if s[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && s[i] == c {
                i += 1;
            }
            out.extend(&s[start..i.min(n)]);
            at_line_start = false;
            continue;
        }
        if c == '#' {
            let start = i;
            while i < n && s[i] != '\n' {
                i += 1;
            }
            for &ch in &s[start..i] {
                super::awaited::push_blank(&mut out, ch);
            }
            at_line_start = false;
            continue;
        }
        if c == '\n' {
            at_line_start = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ' ' || c == '\t' {
            out.push(c);
            i += 1;
            continue;
        }
        at_line_start = false;
        out.push(c);
        i += 1;
    }
    out
}
