//! Receiver-path function refs (#1820): a Python or Go method passed as a
//! value keeps its receiver (`pool.submit(self.store.fetch)`,
//! `Submit(c.store.Fetch)`), and the receiver's import, type or class scopes
//! the member. Only a unique function/method target resolves; a known
//! receiver that doesn't carry the member, a data attribute or a Python
//! property stays unlinked. An unknown receiver falls back to a unique name
//! across the project, never a same-file or file-order tie-break.

use super::*;

/// Base-class and mutual-recursion walks stop after this many classes.
const CLASS_WALK_LIMIT: usize = 16;

/// A receiver whose type was inferred but can't name one class.
const UNKNOWN_TYPE: &str = "<unknown>";

impl KernelResolver {
    /// matchMemberFunctionRef — `recv.member` for a Python/Go function_ref.
    pub(super) fn match_member_function_ref(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(dot) = r.reference_name.rfind('.') else {
            return Ok(None);
        };
        let (receiver, member) = (&r.reference_name[..dot], &r.reference_name[dot + 1..]);
        let root = receiver.split('.').next().unwrap_or(receiver);
        // An import is authoritative even when it points outside the project,
        // unless a parameter or local shadows it at the ref.
        if self.import_mappings(&r.file_path)?.iter().any(|i| i.local_name == root) && !self.is_shadowed_import(root, r)? {
            if r.language == "python" {
                if let Some(cls) = self.python_ref_class(receiver, r)? {
                    let members = self.python_members(&cls, member, r, &mut HashSet::new())?;
                    return self.unique_member(members, r, 0.9);
                }
            }
            let Some(hit) = self.resolve_via_import(r)? else {
                return Ok(None);
            };
            let same: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&hit.node.qualified_name)?
                .iter()
                .filter(|n| n.file_path == hit.node.file_path)
                .cloned()
                .collect();
            return self.unique_member(same, r, 0.9);
        }
        if r.language == "go" {
            let typed = if receiver.contains('.') {
                Some(self.match_go_field_chain_call(receiver, member, r)?)
            } else if let Some(raw) = self.infer_local_receiver_type(receiver, r, true)? {
                let raw = raw.trim_start_matches(['*', '&']);
                if raw.contains('.') {
                    // `pkg.Type` names that package's type, which only its
                    // import can place; an external package has none here.
                    Some(self.match_bound_type_member(raw, member, r)?)
                } else if let Some(ty) = self.normalize_inferred_type_name(raw)? {
                    Some(self.resolve_method_on_type(&ty, member, r, 0.9, "function-ref", None)?)
                } else {
                    Some(None)
                }
            } else {
                let types = self
                    .nodes_by_name(receiver)?
                    .iter()
                    .filter(|n| n.language == "go" && (n.kind == "struct" || n.kind == "interface"))
                    .count();
                match types {
                    0 => None,
                    1 => Some(self.resolve_method_on_type(receiver, member, r, 0.9, "function-ref", None)?),
                    _ => Some(None),
                }
            };
            // No name-only fallback for Go: struct fields aren't nodes, so a
            // field read (`c.Errors`) would match any same-named method.
            return match typed {
                Some(hit) => self.unique_member(hit.map(|c| c.node).into_iter().collect(), r, 0.9),
                None => Ok(None),
            };
        }
        if let Some(hit) = self.python_typed_member(receiver, member, r)? {
            return Ok(hit);
        }
        // Unknown receivers: unique-or-drop across ALL files, test doubles
        // and abstract-looking bodies included. Only a method can sit behind
        // a receiver; instance attributes aren't nodes, so a unique function
        // of the member's name is no evidence.
        let named: Vec<Arc<KNode>> = self.nodes_by_name(member)?.iter().cloned().collect();
        Ok(self.unique_member(named, r, 0.8)?.filter(|c| c.node.kind == "method"))
    }

    /// Is `name` bound at `r` by something other than its import (a
    /// parameter, a local)? Files without binding rows keep the import.
    fn is_shadowed_import(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let rows = self.bindings(&r.file_path)?;
        Ok(innermost_binding(&rows, name, Some(r.line)).is_some_and(|b| b.kind != "import"))
    }

    /// The Python receiver arms: `self`/`cls`, `self.field`, a local or
    /// parameter, or a class used directly. `None` means the receiver's type
    /// is unknown; `Some(None)` is a known receiver that settled no target.
    fn python_typed_member(&mut self, receiver: &str, member: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let owner = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| n.kind == "class" && n.start_line <= r.line && n.end_line >= r.line)
            .max_by_key(|n| n.start_line)
            .cloned();
        if receiver == "self" || receiver == "cls" {
            let Some(owner) = owner else { return Ok(Some(None)) };
            let members = self.python_members(&owner, member, r, &mut HashSet::new())?;
            return Ok(Some(self.unique_member(members, r, 0.9)?));
        }
        let field = receiver.strip_prefix("self.").or_else(|| receiver.strip_prefix("cls."));
        let mut ty = match field {
            Some(f) if is_word(f) => {
                let Some(owner) = owner else { return Ok(Some(None)) };
                self.python_field_type(f, &owner, r)?
            }
            _ => self.python_local_type(receiver, r)?,
        };
        // A type name used directly (`Store.fetch`) scopes like an annotation.
        if ty.is_none() && is_word(receiver) && receiver.starts_with(|c: char| c.is_ascii_uppercase()) {
            ty = Some(receiver.to_string());
        }
        let Some(ty) = ty.filter(|t| t != "object" && t != "Any") else {
            return Ok(None);
        };
        let Some(cls) = self.python_ref_class(&ty, r)? else {
            return Ok(Some(None));
        };
        let members = self.python_members(&cls, member, r, &mut HashSet::new())?;
        if !members.is_empty() {
            return Ok(Some(self.unique_member(members, r, 0.9)?));
        }
        // A base-typed receiver can hold a subclass-only method: keep only
        // descendants of THAT base, so unrelated same-name methods can't win.
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(member)?
            .iter()
            .filter(|n| n.kind == "method" && n.language == "python")
            .cloned()
            .collect();
        let mut descendants = Vec::new();
        for n in candidates {
            let Some(sep) = n.qualified_name.rfind("::") else { continue };
            let parent = self
                .nodes_in_file(&n.file_path)?
                .iter()
                .find(|c| c.kind == "class" && c.qualified_name == n.qualified_name[..sep])
                .cloned();
            if let Some(parent) = parent {
                if self.python_derives_from(&parent, &cls, r, &mut HashSet::new())? {
                    descendants.push(n);
                }
            }
        }
        Ok(Some(self.unique_member(descendants, r, 0.8)?))
    }

    /// The single same-family candidate, when it is a callable other than
    /// the referencing node and not a Python property.
    fn unique_member(&mut self, nodes: Vec<Arc<KNode>>, r: &ResolveRefIn, confidence: f64) -> Res<Option<KCand>> {
        let mut pool = nodes.into_iter().filter(|n| same_language_family(&n.language, &r.language));
        let (Some(target), None) = (pool.next(), pool.next()) else {
            return Ok(None);
        };
        if !(target.kind == "function" || target.kind == "method")
            || target.id == r.from_node_id
            || self.is_python_property(&target)
        {
            return Ok(None);
        }
        Ok(Some(KCand { node: target, confidence, resolved_by: "function-ref" }))
    }

    /// A class named from `r`'s file: through its import, else the file's
    /// own unique class of that name.
    fn python_ref_class(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let root = name.split('.').next().unwrap_or(name);
        if self.import_mappings(&r.file_path)?.iter().any(|i| i.local_name == root) {
            let Some(hit) = self.resolve_via_import(&r.clone().naming(name, "references"))? else {
                return Ok(None);
            };
            let node = hit.node;
            if node.kind != "class" {
                return Ok(None);
            }
            let same = self
                .nodes_by_qualified_name(&node.qualified_name)?
                .iter()
                .filter(|n| n.kind == "class" && n.file_path == node.file_path)
                .count();
            return Ok((same == 1).then_some(node));
        }
        let mut classes = self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| n.kind == "class" && n.file_path == r.file_path)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter();
        Ok(match (classes.next(), classes.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        })
    }

    /// The classes a Python class header names as bases, each resolved from
    /// the class's own file.
    fn python_bases(&mut self, cls: &KNode, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let Some(lines) = self.read_file(&cls.file_path) else {
            return Ok(Vec::new());
        };
        let site = r.clone().at(cls);
        let mut out = Vec::new();
        for name in python_base_names(&lines, (cls.start_line - 1).max(0) as usize) {
            if let Some(base) = self.python_ref_class(&name, &site)? {
                out.push(base);
            }
        }
        Ok(out)
    }

    fn python_derives_from(&mut self, cls: &KNode, base: &KNode, r: &ResolveRefIn, seen: &mut HashSet<String>) -> Res<bool> {
        if seen.len() >= CLASS_WALK_LIMIT || !seen.insert(cls.id.clone()) {
            return Ok(false);
        }
        for parent in self.python_bases(cls, r)? {
            if parent.id == base.id || self.python_derives_from(&parent, base, r, seen)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// What `member` names on `cls`: an assignment in the class body shadows
    /// any method (the class itself stands in, so it never resolves), else
    /// the class's own member, else its bases' (deduplicated).
    fn python_members(
        &mut self,
        cls: &Arc<KNode>,
        member: &str,
        r: &ResolveRefIn,
        seen: &mut HashSet<String>,
    ) -> Res<Vec<Arc<KNode>>> {
        if seen.len() >= CLASS_WALK_LIMIT || !seen.insert(cls.id.clone()) {
            return Ok(Vec::new());
        }
        if self.python_class_assigns(cls, member)? {
            return Ok(vec![cls.clone()]);
        }
        let own: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{}::{}", cls.qualified_name, member))?
            .iter()
            .filter(|n| n.file_path == cls.file_path)
            .cloned()
            .collect();
        if !own.is_empty() {
            return Ok(own);
        }
        // Python takes the first class along the C3 order that defines the
        // name, so a later base's same-named method never competes.
        if self.supertypes_complete {
            if let Some(mro) = self.python_mro(cls, 0)?.filter(|m| m.len() > 1) {
                for class in mro.iter().skip(1) {
                    if self.python_class_assigns(class, member)? {
                        return Ok(vec![class.clone()]);
                    }
                    let own: Vec<Arc<KNode>> = self
                        .nodes_by_qualified_name(&format!("{}::{}", class.qualified_name, member))?
                        .iter()
                        .filter(|n| n.file_path == class.file_path)
                        .cloned()
                        .collect();
                    if !own.is_empty() {
                        return Ok(own);
                    }
                }
                return Ok(Vec::new());
            }
        }
        let mut out: Vec<Arc<KNode>> = Vec::new();
        for base in self.python_bases(cls, r)? {
            for n in self.python_members(&base, member, r, seen)? {
                if !out.iter().any(|o| o.id == n.id) {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }

    /// Whether `cls` assigns `member` as data: `member = …` / `member: T` in
    /// the class body, or `self.member = …` inside one of its methods (a
    /// method's bare locals and keyword arguments don't count).
    fn python_class_assigns(&mut self, cls: &KNode, member: &str) -> Res<bool> {
        let Some(lines) = self.read_file(&cls.file_path) else {
            return Ok(false);
        };
        let bodies: Vec<(i64, i64)> = self
            .nodes_in_file(&cls.file_path)?
            .iter()
            .filter(|n| {
                (n.kind == "method" || n.kind == "function") && n.start_line > cls.start_line && n.end_line <= cls.end_line
            })
            .map(|n| (n.start_line, n.end_line))
            .collect();
        let assigns = re!(r"^\s*((?:self|cls)\.)?([A-Za-z_]\w*)\s*[=:]");
        let lo = (cls.start_line - 1).max(0) as usize;
        let hi = (cls.end_line.max(cls.start_line) as usize).min(lines.len());
        let starts = python_statement_starts(&lines, lo, hi);
        for (i, line) in lines.get(lo..hi).unwrap_or(&[]).iter().enumerate() {
            if !starts[i] {
                continue;
            }
            let Some(c) = assigns.captures(line) else { continue };
            if &c[2] != member || line.trim_start().starts_with('#') {
                continue;
            }
            let line_no = (lo + i + 1) as i64;
            let in_method = bodies.iter().any(|&(s, e)| s <= line_no && line_no <= e);
            if !in_method || c.get(1).is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `@property` / `@cached_property` methods are attribute reads, not
    /// callables passed by value.
    fn is_python_property(&mut self, node: &KNode) -> bool {
        if node.language != "python" || node.kind != "method" {
            return false;
        }
        let is_property = |d: &str| {
            let d = d.trim().trim_start_matches('@');
            d == "property" || d == "cached_property" || d == "functools.cached_property"
        };
        if node.decorators.as_ref().is_some_and(|ds| ds.iter().any(|d| is_property(d))) {
            return true;
        }
        let Some(lines) = self.read_file(&node.file_path) else {
            return false;
        };
        let mut i = node.start_line - 2;
        while i >= 0 {
            let line = lines.get(i as usize).map(|l| l.trim()).unwrap_or("");
            if !line.starts_with('@') {
                break;
            }
            if is_property(line) {
                return true;
            }
            i -= 1;
        }
        false
    }

    /// A local's type from the nearest assignment or annotation above the
    /// reference inside the caller, else the caller's parameter annotation.
    /// An assignment of anything but a constructor call is `<unknown>`.
    fn python_local_type(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if !is_word(receiver) {
            return Ok(None);
        }
        let caller = self.node_by_id(&r.from_node_id)?;
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let floor = caller.as_ref().map_or(1, |c| c.start_line).max(1);
        let mut line_no = r.line.min(lines.len() as i64);
        let starts = python_statement_starts(&lines, (floor - 1) as usize, line_no.max(floor) as usize);
        while line_no >= floor {
            let line = &lines[(line_no - 1) as usize];
            let starts_statement = starts[(line_no - floor) as usize];
            line_no -= 1;
            if !starts_statement {
                continue;
            }
            if let Some(c) = python_assignment_re().captures(line) {
                if c.get(1).is_none() && &c[2] == receiver {
                    return Ok(Some(assigned_type(c.get(3).map(|m| m.as_str()), &c[4])));
                }
            }
            if let Some(c) = python_annotation_re().captures(line) {
                if c.get(1).is_none() && &c[2] == receiver {
                    return Ok(Some(c[3].to_string()));
                }
            }
        }
        Ok(caller.and_then(|c| param_annotation(c.signature.as_deref().unwrap_or(""), receiver)))
    }

    /// `self.<field>`'s type within `owner`: a class-body or `self.`
    /// annotation, an annotated or constructor assignment, or an `__init__`
    /// (or referencing method) parameter assigned to it. Conflicting
    /// evidence is known-but-ambiguous, never a name-only fallback.
    fn python_field_type(&mut self, field: &str, owner: &KNode, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let prefix = format!("{}::", owner.qualified_name);
        let methods: Vec<Arc<KNode>> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| n.kind == "method" && n.qualified_name.starts_with(&prefix))
            .cloned()
            .collect();
        let mut types: Vec<String> = Vec::new();
        let mut untyped = false;
        let mut add = |t: String| {
            if !types.contains(&t) {
                types.push(t);
            }
        };
        let end = (owner.end_line.max(0) as usize).min(lines.len());
        let begin = (owner.start_line.max(0) as usize).min(end);
        let starts = python_statement_starts(&lines, begin, end);
        for i in begin..end {
            if !starts[i - begin] {
                continue;
            }
            let line_no = i as i64 + 1;
            let method = methods.iter().find(|m| m.start_line <= line_no && m.end_line >= line_no);
            if let Some(m) = method {
                if m.id == r.from_node_id {
                    if line_no > r.line {
                        continue;
                    }
                } else if m.name != "__init__" {
                    continue;
                }
            }
            let line = &lines[i];
            if let Some(c) = python_annotation_re().captures(line) {
                if &c[2] == field && (method.is_none() || c.get(1).is_some()) {
                    add(c[3].to_string());
                }
            }
            let Some(c) = python_assignment_re().captures(line) else { continue };
            if c.get(1).is_none() || &c[2] != field {
                continue;
            }
            if let Some(t) = c.get(3) {
                add(t.as_str().to_string());
                continue;
            }
            let value = c[4].trim();
            if let Some(ctor) = constructor_type(value) {
                add(ctor);
                continue;
            }
            match method {
                Some(_) if value == "None" => {}
                Some(m) if is_word(value) => match param_annotation(m.signature.as_deref().unwrap_or(""), value) {
                    Some(t) => add(t),
                    None => untyped = true,
                },
                _ => add(UNKNOWN_TYPE.to_string()),
            }
        }
        Ok(match types.len() {
            0 => None,
            // An untyped reassignment (`self.store = replacement`) outvotes
            // the one typed assignment; alone it stays no evidence.
            1 if !untyped => types.pop(),
            _ => Some(UNKNOWN_TYPE.to_string()),
        })
    }
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
}

/// `[self.|cls.]name[: Type] = value` — groups: receiver prefix, name,
/// annotation, value.
fn python_assignment_re() -> Rc<Regex> {
    re!(r#"^\s*(?:(self|cls)\.)?([A-Za-z_]\w*)\s*(?::\s*["']?([\w.]+)["']?)?\s*=\s*(.*)$"#)
}

/// `[self.|cls.]name: Type` — groups: receiver prefix, name, type.
fn python_annotation_re() -> Rc<Regex> {
    re!(r#"^\s*(?:(self|cls)\.)?([A-Za-z_]\w*)\s*:\s*["']?([\w.]+)"#)
}

/// For each line in `lines[lo..hi]`, whether it begins a statement: not
/// inside a string (a docstring's `name: Description` line is not an
/// annotation), an open bracket (a call's `key=value,` line is not an
/// assignment) or a backslash continuation. `lo` must itself begin a
/// statement, as a `def` or `class` line does.
fn python_statement_starts(lines: &[String], lo: usize, hi: usize) -> Vec<bool> {
    let hi = hi.min(lines.len());
    let mut out = Vec::with_capacity(hi.saturating_sub(lo));
    let mut depth = 0usize;
    // The open string's quote byte and whether it is triple-quoted.
    let mut string: Option<(u8, bool)> = None;
    let mut continued = false;
    for line in lines.get(lo..hi).unwrap_or(&[]) {
        out.push(depth == 0 && string.is_none() && !continued);
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match string {
                Some((q, triple)) => {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == q && (!triple || b[i..].starts_with(&[q, q, q])) {
                        string = None;
                        i += if triple { 3 } else { 1 };
                        continue;
                    }
                }
                None => match b[i] {
                    b'#' => break,
                    q @ (b'"' | b'\'') => {
                        let triple = b[i..].starts_with(&[q, q, q]);
                        string = Some((q, triple));
                        i += if triple { 3 } else { 1 };
                        continue;
                    }
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                    _ => {}
                },
            }
            i += 1;
        }
        // A single-quoted string never spans lines without a backslash.
        if matches!(string, Some((_, false))) {
            string = None;
        }
        continued = string.is_none() && line.trim_end().ends_with('\\');
    }
    out
}

/// The base-class names of the `class` statement at `lines[at]`, read across
/// lines to the closing parenthesis: `Store[int]` names `Store`, keyword
/// arguments (`metaclass=ABCMeta`) name no base.
fn python_base_names(lines: &[String], at: usize) -> Vec<String> {
    let Some(first) = lines.get(at) else { return Vec::new() };
    let Some(m) = re!(r"^\s*class\s+\w+\s*\(").find(first) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let text = std::iter::once(&first[m.end()..]).chain(lines[at + 1..].iter().take(64).map(String::as_str));
    'lines: for line in text {
        for ch in line.chars() {
            match ch {
                '#' => break,
                '(' | '[' | '{' => depth += 1,
                ')' if depth == 0 => break 'lines,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => names.push(std::mem::take(&mut current)),
                _ if depth == 0 => current.push(ch),
                _ => {}
            }
        }
        current.push(' ');
    }
    names.push(current);
    names
        .into_iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && !n.contains('=') && !n.starts_with('*'))
        .collect()
}

/// `Type(...)` — the constructed class of an assigned value.
fn constructor_type(value: &str) -> Option<String> {
    re!(r"^([A-Z][\w.]*)\s*\(").captures(value).map(|c| c[1].to_string())
}

fn assigned_type(annotation: Option<&str>, value: &str) -> String {
    annotation
        .map(str::to_string)
        .or_else(|| constructor_type(value.trim()))
        .unwrap_or_else(|| UNKNOWN_TYPE.to_string())
}

/// `param: Type` in a signature.
fn param_annotation(signature: &str, param: &str) -> Option<String> {
    re!(r#"(?-u:\b)([A-Za-z_]\w*)\s*:\s*["']?([\w.]+)"#)
        .captures_iter(signature)
        .find(|c| &c[1] == param)
        .map(|c| c[2].to_string())
}
