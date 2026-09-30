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
