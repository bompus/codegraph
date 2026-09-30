//! Candidate filters on what a bare name can mean at its site (name-matcher.ts
//! isDotNetTypeRef / canNameInTypePosition, isRustNameInScope): a .NET type
//! position names a type, never a same-named member; a bare Rust name reaches
//! only what is in scope — the prelude's `Ok`/`Some`/`Result` unless the file
//! defines or `use`s a project item of that name.

use super::*;

/// Names the Rust prelude puts in every module; a project item of the same
/// name needs a `use` to shadow one.
const RUST_PRELUDE: &[&str] = &[
    "Ok", "Err", "Some", "None", "Result", "Option", "Box", "Vec", "String", "Default", "Drop", "Iterator",
    "IntoIterator", "From", "Into", "Clone", "Copy", "Send", "Sync", "Sized", "ToString", "ToOwned", "PartialEq",
    "Eq", "PartialOrd", "Ord", "AsRef", "AsMut", "Fn", "FnMut", "FnOnce", "Extend", "drop",
];

/// A file's project `use` trees (rustUsesOf): every identifier in them, and
/// `X` of each `use …::X::*` (`super` for `use super::*`). `std::`, `core::`
/// and `alloc::` trees are skipped.
#[derive(Default)]
pub(crate) struct RustScopeUses {
    pub(crate) names: HashSet<String>,
    pub(crate) globs: HashSet<String>,
}

pub(super) fn collect_rust_scope_uses(text: &str) -> RustScopeUses {
    let mut uses = RustScopeUses::default();
    // Comments first: a doc comment's prose ("…use the Option…") is not a `use`.
    let text = strip_rust_comments(text);
    for m in re!(r"(?:^|[;{}\s])use\s+([^;]+);").captures_iter(&text) {
        let tree = &m[1];
        if utf16_len_exceeds(tree, 2000) || re!(r"^\s*(?:::)?(?:std|core|alloc)(?-u:\b)").is_match(tree) {
            continue;
        }
        for id in re!(r"[A-Za-z_][A-Za-z0-9_]*").find_iter(tree) {
            uses.names.insert(id.as_str().to_string());
        }
        for g in re!(r"([A-Za-z0-9_]+)\s*::\s*(?:\{[^}]*)?\*").captures_iter(tree) {
            uses.globs.insert(g[1].to_string());
        }
    }
    uses
}

/// stripCommentsForRegex(text, 'rust'): nested block and line comments
/// blanked (newlines kept); string and char literals skipped intact.
pub(super) fn strip_rust_comments(src: &str) -> String {
    let s: Vec<char> = src.chars().collect();
    let n = s.len();
    let mut out = String::with_capacity(src.len());
    let blank = |out: &mut String, chars: &[char]| {
        for &c in chars {
            out.push(if c == '\n' { '\n' } else { ' ' });
        }
    };
    let mut i = 0;
    while i < n {
        let c = s[i];
        let c2 = s.get(i + 1).copied();
        if c == '/' && c2 == Some('*') {
            let start = i;
            i += 2;
            let mut depth = 1;
            while i < n && depth > 0 {
                if s[i] == '/' && s.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && s.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank(&mut out, &s[start..i.min(n)]);
            continue;
        }
        if c == '/' && c2 == Some('/') {
            let start = i;
            while i < n && s[i] != '\n' {
                i += 1;
            }
            blank(&mut out, &s[start..i]);
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
                // A lifetime (`'a`) never closes on its line.
                if c == '\'' && s[i] == '\n' {
                    break;
                }
                i += 1;
            }
            if i < n && s[i] == c {
                i += 1;
            }
            out.extend(&s[start..i.min(n)]);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The module a Rust file is: `src/glob.rs` → `glob`, `src/walk/mod.rs` → `walk`.
fn rust_module_name(file_path: &str) -> &str {
    let parts: Vec<&str> = file_path.split('/').collect();
    let last = parts[parts.len() - 1];
    let base = last.strip_suffix(".rs").unwrap_or(last);
    if matches!(base, "mod" | "lib" | "main") && parts.len() >= 2 {
        parts[parts.len() - 2]
    } else {
        base
    }
}

/// JS `p.slice(0, p.lastIndexOf('/'))`: with no `/` that drops the last char.
fn js_dir(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[..i],
        None => p.char_indices().last().map_or(p, |(i, _)| &p[..i]),
    }
}

/// Does one of the file's globs bring in this candidate's module (or, for
/// `use super::*`, its parent's)?
fn rust_glob_covers(uses: &RustScopeUses, candidate: &KNode, r: &ResolveRefIn) -> bool {
    if uses.globs.contains(rust_module_name(&candidate.file_path)) {
        return true;
    }
    if !uses.globs.contains("super") {
        return false;
    }
    // `use super::*` in a child module: the parent's file, or a sibling in the parent's directory.
    let cand_dir = js_dir(&candidate.file_path);
    cand_dir == js_dir(&r.file_path) || cand_dir == js_dir(js_dir(&r.file_path))
}

/// A property, a method (a constructor is one), an enum case or a field
/// shares a type's name, not its meaning.
pub(super) fn can_name_in_type_position(n: &KNode) -> bool {
    !matches!(n.kind.as_str(), "property" | "method" | "enum_member" | "field")
}

/// A bare Rust reference name — the ones the scope filter applies to.
pub(super) fn is_bare_rust_name(r: &ResolveRefIn) -> bool {
    r.language == "rust" && re!(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(&r.reference_name)
}

impl KernelResolver {
    /// The ref's source line, when the reference name sits at its column.
    fn line_at_name(&mut self, r: &ResolveRefIn) -> Option<(String, String)> {
        let lines = self.read_file(&r.file_path)?;
        let line = lines.get((r.line - 1).max(0) as usize)?;
        let col = r.column.max(0) as usize;
        let at = js_slice(line, col);
        let after = at.strip_prefix(r.reference_name.as_str())?;
        Some((js_prefix(line, col).to_string(), after.to_string()))
    }

    /// isDotNetTypeRef (name-matcher.ts): a C# / VB.NET reference whose site
    /// is a TYPE position — `Type sourceType`, `List<int>`, `new TypeMap()`,
    /// `Exception? e`, `Dictionary<string, Type>`, VB's `As Type` /
    /// `New List(Of T)`. A member read, a method group or a route handler
    /// keeps every candidate.
    pub(super) fn is_dotnet_type_ref(&mut self, r: &ResolveRefIn) -> bool {
        if r.language != "csharp" && r.language != "vbnet" {
            return false;
        }
        if !matches!(r.reference_kind.as_str(), "references" | "instantiates" | "type_of" | "returns") {
            return false;
        }
        if !re!(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(&r.reference_name) {
            return false;
        }
        // `new TypeMap()` constructs a type; the reference's column is the `new`.
        if r.reference_kind == "instantiates" {
            return true;
        }
        let Some((before, after)) = self.line_at_name(r) else { return false };
        if r.language == "vbnet" {
            return re!(r"(?i)(?-u:\b)(?:As|New|Of)\s+$").is_match(&before);
        }
        if re!(r"(?-u:\b)new\s+$").is_match(&before) {
            return true;
        }
        // `Type name` — a declaration names its type first.
        if re!(r"^\s+@?[A-Za-z_]").is_match(&after) {
            return !re!(r"^\s+(?:is|as|and|or|not|when|in|switch|with)(?-u:\b)").is_match(&after);
        }
        // `List<int>`, `Type?` (not `?.` / `??` / `?[`), `Type[]`, and the arguments of a generic.
        if after.starts_with('<')
            || after.strip_prefix('?').is_some_and(|rest| !rest.starts_with(['.', '?', '[']))
            || re!(r"^\[\s*[,\]]").is_match(&after)
        {
            return true;
        }
        re!(r"^\s*[,>]").is_match(&after) && re!(r"<[^<>()]*$").is_match(&before)
    }

    /// isRustNameInScope (name-matcher.ts): whether a bare Rust name can mean
    /// this candidate. An enum's variant is in scope bare only through a `use`
    /// of it or of its enum's `*`, and never names a TYPE. A prelude name is
    /// the prelude's unless the file defines it or imports a project item of
    /// that name.
    pub(super) fn is_rust_name_in_scope(&mut self, candidate: &KNode, r: &ResolveRefIn) -> bool {
        let name = r.reference_name.as_str();
        // Bare in the SOURCE: the index keeps `crate::error::Result` by its
        // last segment, and a path is not a prelude lookup.
        if let Some((before, _)) = self.line_at_name(r) {
            if re!(r"::\s*$").is_match(&before) {
                return true;
            }
        }
        if candidate.kind == "enum_member" {
            if r.reference_kind == "references" {
                return false;
            }
            let owner = candidate
                .qualified_name
                .rfind("::")
                .map(|cut| candidate.qualified_name[..cut].rsplit("::").next().unwrap_or(""))
                .unwrap_or("");
            let Some(file) = self.read_file(&r.file_path) else { return false };
            let uses = file.rust_scope_uses();
            return !owner.is_empty()
                && (uses.globs.contains(owner) || (uses.names.contains(name) && uses.names.contains(owner)));
        }
        if !RUST_PRELUDE.contains(&name) || candidate.file_path == r.file_path {
            return true;
        }
        let Some(file) = self.read_file(&r.file_path) else { return false };
        let uses = file.rust_scope_uses();
        uses.names.contains(name) || rust_glob_covers(uses, candidate, r)
    }
}

/// A Java file's `import static a.b.Owner.member;` / `import static a.b.Owner.*;`
/// (javaStaticImportsOf): owners imported with `*`, and `Owner.member` pairs.
#[derive(Default)]
pub(crate) struct JavaStaticImports {
    pub(crate) owners: HashSet<String>,
    pub(crate) members: HashSet<String>,
}

pub(super) fn collect_java_static_imports(text: &str) -> JavaStaticImports {
    let mut found = JavaStaticImports::default();
    for m in re!(r"(?m)^\s*import\s+static\s+([A-Za-z0-9_.$]+)\s*\.\s*(\*|[A-Za-z0-9_$]+)\s*;").captures_iter(text) {
        let owner = m[1].rsplit('.').next().unwrap_or("").to_string();
        if &m[2] == "*" {
            found.owners.insert(owner);
        } else {
            found.members.insert(format!("{owner}.{}", &m[2]));
        }
    }
    found
}

/// JAVA_TYPE_KINDS (name-matcher.ts): the declarations a bare call's scope walks.
fn is_java_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "interface" | "enum" | "struct" | "record" | "trait")
}

/// A bare Java `calls` ref: `verify(mock)`, `helper()`.
pub(super) fn is_bare_java_call(r: &ResolveRefIn) -> bool {
    r.language == "java" && r.reference_kind == "calls" && re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(&r.reference_name)
}

/// The shape of a Python `calls` ref at its site (pythonCallShape).
pub(super) enum PythonCallShape {
    /// `get(1)`: no receiver, so never a method (Python has no implicit self).
    Bare,
    /// `a.b.get(…)` whose receiver the ref name lost: a member of what the
    /// chain names last.
    Chained(String),
}

/// Underscores dropped, lowercased: `user_service` fits `UserService`.
fn plain_name(s: &str) -> String {
    s.chars().filter(|&c| c != '_').flat_map(char::to_lowercase).collect()
}

/// Does the class that owns `method` fit a Python receiver's last segment
/// (`self.store.fetch()` on a `Store`, `self.user_service.find()` on a
/// `UserService`)?
pub(super) fn python_owner_fits(method: &KNode, receiver_last: &str) -> bool {
    let owner = method
        .qualified_name
        .rfind("::")
        .map(|cut| method.qualified_name[..cut].rsplit("::").next().unwrap_or(""))
        .unwrap_or("");
    !owner.is_empty() && plain_name(owner) == plain_name(receiver_last)
}

impl KernelResolver {
    /// isJavaMethodInScope (name-matcher.ts): a bare Java call reaches a
    /// method of a type around it, of one of that type's supertypes, or one
    /// the file imports statically — never another class's method of that
    /// name (Mockito's `verify(…)` onto a service's `verify`). Supertypes are
    /// read from the declarations, so one whose `extends` did not resolve
    /// still counts: this only narrows the candidates.
    pub(super) fn is_java_method_in_scope(&mut self, method: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some(cut) = method.qualified_name.rfind("::") else { return Ok(true) };
        let owner = method.qualified_name[..cut].rsplit("::").next().unwrap_or("").to_string();
        if let Some(file) = self.read_file(&r.file_path) {
            let imports = file.java_static_imports();
            if imports.owners.contains(&owner) || imports.members.contains(&format!("{owner}.{}", r.reference_name)) {
                return Ok(true);
            }
        }
        let mut queue: VecDeque<String> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| is_java_type_kind(&n.kind) && n.start_line <= r.line && n.end_line >= r.line)
            .map(|n| n.name.clone())
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        while seen.len() < 40 {
            let Some(name) = queue.pop_front() else { break };
            if !seen.insert(name.clone()) {
                continue;
            }
            if name == owner {
                return Ok(true);
            }
            queue.extend(self.java_supertypes_of(&name)?.iter().cloned());
        }
        Ok(false)
    }

    /// javaSupertypesOf (name-matcher.ts): the simple names a Java type's
    /// declarations extend or implement, read from each declaration's head.
    fn java_supertypes_of(&mut self, type_name: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.java_supers_memo.get(type_name) {
            return Ok(hit.clone());
        }
        let mut names: Vec<String> = Vec::new();
        // An anonymous class (`new PetType() { … }`, indexed as
        // `<PetType$anon@84>`) extends or implements the type it instantiates.
        if let Some(m) = re!(r"^<([A-Za-z_$][A-Za-z0-9_$.]*)\$anon@[0-9]+>$").captures(type_name) {
            names.push(m[1].rsplit('.').next().unwrap_or("").to_string());
        }
        let decls: Vec<Arc<KNode>> = self
            .nodes_by_name(type_name)?
            .iter()
            .filter(|d| d.language == "java" && is_java_type_kind(&d.kind))
            .cloned()
            .collect();
        for decl in decls {
            let Some(lines) = self.read_file(&decl.file_path) else { continue };
            let from = (decl.start_line - 1).max(0) as usize;
            let to = ((decl.start_line + 5).max(0) as usize).min(lines.len());
            let head = if from < to { lines[from..to].join(" ") } else { String::new() };
            let Some(clause) = re!(r"(?-u:\b)(?:extends|implements)(?-u:\b)([^{]*)\{").captures(&head) else { continue };
            let flat = re!(r"<[^<>]*(?:<[^<>]*>[^<>]*)*>").replace_all(&clause[1], "");
            for m in re!(r"[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*").find_iter(&flat) {
                let simple = m.as_str().rsplit('.').next().unwrap_or("");
                if simple != "extends" && simple != "implements" {
                    names.push(simple.to_string());
                }
            }
        }
        let names = Rc::new(names);
        self.java_supers_memo.insert(type_name.to_string(), names.clone());
        Ok(names)
    }

    /// The candidates a Python call's shape and a bare Java call's scope
    /// allow (matchByExactName / matchFuzzy filters, name-matcher.ts).
    pub(super) fn retain_python_java_scope(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let java_bare = is_bare_java_call(r);
        let python_shape = self.python_call_shape(r);
        if !java_bare && python_shape.is_none() {
            return Ok(candidates);
        }
        let mut kept = Vec::with_capacity(candidates.len());
        for n in candidates {
            if java_bare && n.kind == "method" && !self.is_java_method_in_scope(&n, r)? {
                continue;
            }
            if let Some(shape) = &python_shape {
                if !self.fits_python_call_shape(&n, shape, r)? {
                    continue;
                }
            }
            kept.push(n);
        }
        Ok(kept)
    }

    /// pythonCallShape (name-matcher.ts): read from the source at the ref's
    /// column, which is the call's start. `self.x()` / `cls.x()`, a chain
    /// split across lines, or an unreadable site is None: no narrowing.
    pub(super) fn python_call_shape(&mut self, r: &ResolveRefIn) -> Option<PythonCallShape> {
        if r.language != "python" || r.reference_kind != "calls" {
            return None;
        }
        let name = r.reference_name.as_str();
        if !re!(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(name) {
            return None;
        }
        let lines = self.read_file(&r.file_path)?;
        let line = lines.get((r.line - 1).max(0) as usize)?;
        let text = js_slice(line, r.column.max(0) as usize);
        if text.strip_prefix(name).is_some_and(|rest| re!(r"^\s*\(").is_match(rest)) {
            return Some(PythonCallShape::Bare);
        }
        let escaped = regex::escape(name);
        if Self::cached_regex(&format!(r"^(?:self|cls)\s*\.\s*{escaped}\s*\(")).ok()?.is_match(text) {
            return None;
        }
        // The call starts at its receiver: everything up to `.name(` is the chain.
        let chain_re = Self::cached_regex(&format!(r"^(.*?)\.\s*{escaped}\s*\(")).ok()?;
        let chain = chain_re.captures(text)?;
        let owner = re!(r"([A-Za-z0-9_]+)\s*(?:\([^()]*\)|\[[^\[\]]*\])?\s*$").captures(&chain[1])?;
        Some(PythonCallShape::Chained(owner[1].to_string()))
    }

    /// fitsPythonCallShape (name-matcher.ts): can a call of this shape mean
    /// the candidate? A bare call never means a method, nor another file's
    /// definition when the file imports the name from a module outside the
    /// project (`from django.shortcuts import render`). A chained call means a
    /// method of a class the chain's last name fits, or a definition in a
    /// module of that name (`helpers.slugify()`).
    pub(super) fn fits_python_call_shape(&mut self, n: &KNode, shape: &PythonCallShape, r: &ResolveRefIn) -> Res<bool> {
        match shape {
            PythonCallShape::Bare => {
                if n.kind == "method" {
                    return Ok(false);
                }
                Ok(n.file_path == r.file_path || !self.is_python_name_imported_from_outside(r)?)
            }
            PythonCallShape::Chained(owner) => {
                if n.kind == "method" {
                    return Ok(python_owner_fits(n, owner));
                }
                let parts: Vec<&str> = n.file_path.split('/').collect();
                let last = parts[parts.len() - 1];
                let stem = last.strip_suffix(".pyi").or_else(|| last.strip_suffix(".py")).unwrap_or(last);
                Ok(stem == owner || (stem == "__init__" && parts.len() >= 2 && parts[parts.len() - 2] == owner))
            }
        }
    }

    /// Does the file bind the ref's name by `from <module> import name` from a
    /// module no project file is? Read from the file's import mappings, the
    /// module placed by findPythonModuleFile.
    fn is_python_name_imported_from_outside(&mut self, r: &ResolveRefIn) -> Res<bool> {
        let source = self
            .import_mappings(&r.file_path)?
            .iter()
            .find(|i| i.local_name == r.reference_name && !i.is_namespace)
            .map(|i| i.source.clone());
        let Some(module) = source else { return Ok(false) };
        if module.is_empty() || module.starts_with('.') {
            return Ok(false);
        }
        Ok(!self.python_module_in_project(&module)?)
    }

    /// Whether any project file is `a/b/c.py` or `a/b/c/__init__.py` for
    /// module `a.b.c` (suffix-matched, like PY_MODULE_LOCAL upstream).
    fn python_module_in_project(&mut self, module: &str) -> Res<bool> {
        let rel = module.replace('.', "/");
        let last_seg = module.rsplit('.').next().unwrap_or(module);
        let file = format!("{rel}.py");
        if self.nodes_by_name(&format!("{last_seg}.py"))?.iter().any(|n| n.kind == "file" && is_path_or_tail(&n.file_path, &file)) {
            return Ok(true);
        }
        let init = format!("{rel}/__init__.py");
        Ok(self.nodes_by_name("__init__.py")?.iter().any(|n| n.kind == "file" && is_path_or_tail(&n.file_path, &init)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_uses_skip_comments_and_std() {
        let uses = collect_rust_scope_uses(
            "// use crate::Fake;\n/* use a::{B, /* nested */ C}; */\nuse std::fmt::Result;\nuse crate::error::{Error, Result};\nuse super::*;\nuse crate::walk::*;\n",
        );
        assert!(uses.names.contains("Result"));
        assert!(uses.names.contains("Error"));
        assert!(!uses.names.contains("Fake"));
        assert!(!uses.names.contains("fmt"));
        assert!(uses.globs.contains("super"));
        assert!(uses.globs.contains("walk"));
    }

    #[test]
    fn rust_module_name_folds_mod_files() {
        assert_eq!(rust_module_name("src/glob.rs"), "glob");
        assert_eq!(rust_module_name("src/walk/mod.rs"), "walk");
        assert_eq!(rust_module_name("lib.rs"), "lib");
    }
}
