//! Dart library namespaces, parts and URI imports. Scope is unknown without a package root.
use super::*;

#[derive(Clone, Default)]
struct Directive {
    uris: Vec<String>,
    prefix: String,
    show: Option<HashSet<String>>,
    hide: HashSet<String>,
}
impl Directive {
    fn admits(&self, name: &str) -> bool {
        self.show.as_ref().is_none_or(|s| s.contains(name)) && !self.hide.contains(name)
    }
}
#[derive(Clone, Default)]
struct Directives {
    name: Option<String>,
    parent: Option<(String, bool)>,
    parts: Vec<String>,
    imports: Vec<Directive>,
    exports: Vec<Directive>,
}
#[derive(Clone)]
pub(super) struct DartToken {
    pub text: String,
    pub string: bool,
    pub start: usize,
    pub end: usize,
}

/// Offset-preserving lexer for directives and resolver source evidence. Comments are skipped.
pub(super) fn tokens(source: &str) -> Vec<DartToken> {
    let b = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") || (i == 0 && b[i..].starts_with(b"#!")) {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let start = i;
        let raw = b[i] == b'r' && b.get(i + 1).is_some_and(|c| matches!(c, b'\'' | b'"'));
        if raw {
            i += 1;
        }
        if matches!(b[i], b'\'' | b'"') {
            let quote = b[i];
            let triple = b.get(i..i + 3) == Some(&[quote; 3]);
            let width = if triple { 3 } else { 1 };
            i += width;
            let content = i;
            while i < b.len() {
                if !raw && b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                    continue;
                }
                if b.get(i..i + width)
                    .is_some_and(|s| s.iter().all(|c| *c == quote))
                {
                    break;
                }
                i += 1;
            }
            let text = source[content..i].to_string();
            i = (i + width).min(b.len());
            out.push(DartToken {
                text,
                string: true,
                start,
                end: i,
            });
            continue;
        }
        if b[i].is_ascii_alphabetic() || matches!(b[i], b'_' | b'$') || b[i] >= 128 {
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$') || b[i] >= 128)
            {
                i += 1;
            }
        } else {
            i += 1;
        }
        out.push(DartToken {
            text: source[start..i].to_string(),
            string: false,
            start,
            end: i,
        });
    }
    out
}
fn directives(source: &str) -> Directives {
    let t = tokens(source.trim_start_matches('\u{feff}'));
    let mut out = Directives::default();
    let mut i = 0;
    while i < t.len() {
        if t[i].text == "@" {
            i += 1;
            if i < t.len() {
                i += 1;
            }
            while t.get(i).is_some_and(|n| n.text == ".")
                && t.get(i + 1)
                    .is_some_and(|n| re!(r"^[A-Za-z_$][\w$]*$").is_match(&n.text))
            {
                i += 2;
            }
            if t.get(i).is_some_and(|t| t.text == "(") {
                let mut depth = 0;
                loop {
                    if i >= t.len() {
                        break;
                    }
                    if !t[i].string && t[i].text == "(" {
                        depth += 1;
                    }
                    if !t[i].string && t[i].text == ")" {
                        depth -= 1;
                    }
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            continue;
        }
        let kind = t[i].text.as_str();
        if !matches!(kind, "library" | "part" | "import" | "export") {
            break;
        }
        let Some(end) = t[i + 1..]
            .iter()
            .position(|t| !t.string && t.text == ";")
            .map(|j| i + 1 + j)
        else {
            break;
        };
        let body = &t[i + 1..end];
        let valid = match kind {
            "part" => body.first().is_some_and(|n| n.string || n.text == "of"),
            "import" | "export" => body.first().is_some_and(|n| n.string),
            "library" => body.iter().all(|n| {
                !n.string && (n.text == "." || re!(r"^[A-Za-z_$][\w$]*$").is_match(&n.text))
            }),
            _ => false,
        };
        if !valid {
            break;
        }

        match kind {
            "library" => {
                out.name = Some(body.iter().map(|t| t.text.as_str()).collect());
            }
            "part" => {
                if body.first().is_some_and(|t| t.text == "of" && !t.string) {
                    if let Some(uri) = body.get(1).filter(|t| t.string) {
                        out.parent = Some((uri.text.clone(), true));
                    } else {
                        out.parent =
                            Some((body[1..].iter().map(|t| t.text.as_str()).collect(), false));
                    }
                } else if let Some(uri) = body.first().filter(|t| t.string) {
                    out.parts.push(uri.text.clone());
                }
            }
            _ => {
                let mut d = Directive::default();
                let mut depth = 0;
                let mut j = 0;
                while j < body.len() {
                    let token = &body[j];
                    if token.string && depth == 0 && !token.text.contains('$') {
                        d.uris.push(token.text.clone());
                    }
                    if !token.string && token.text == "(" {
                        depth += 1;
                    }
                    if !token.string && token.text == ")" {
                        depth -= 1;
                    }
                    if !token.string && depth == 0 && token.text == "as" {
                        if let Some(p) = body.get(j + 1) {
                            d.prefix = p.text.clone();
                        }
                        j += 1;
                    } else if !token.string
                        && depth == 0
                        && matches!(token.text.as_str(), "show" | "hide")
                    {
                        let show = token.text == "show";
                        let mut names = HashSet::new();
                        j += 1;
                        while j < body.len()
                            && re!(r"^[A-Za-z_$][\w$]*$").is_match(&body[j].text)
                            && !matches!(body[j].text.as_str(), "show" | "hide")
                        {
                            names.insert(body[j].text.clone());
                            j += 1;
                            if body.get(j).is_some_and(|t| t.text == ",") {
                                j += 1;
                            } else {
                                break;
                            }
                        }
                        if show {
                            d.show = Some(match d.show.take() {
                                Some(old) => old.intersection(&names).cloned().collect(),
                                None => names,
                            });
                        } else {
                            d.hide.extend(names);
                        }
                        continue;
                    }
                    j += 1;
                }
                if kind == "import" {
                    out.imports.push(d);
                } else {
                    out.exports.push(d);
                }
            }
        }
        i = end + 1;
    }
    out
}
#[derive(Default)]
pub(super) struct DartLibraries {
    directives: HashMap<String, Option<Directives>>,
    packages: Option<Vec<(String, String)>>,
    package_of: HashMap<String, Option<(String, String)>>,
    library_of: HashMap<String, Option<String>>,
    members: HashMap<String, HashSet<String>>,
    visible: HashMap<(String, String, String, String), bool>,
}
impl KernelResolver {
    fn dart_directives(&mut self, file: &str) -> Option<Directives> {
        if let Some(d) = self.dart_libraries.directives.get(file) {
            return d.clone();
        }
        let d = self.read_file(file).map(|s| directives(s.text()));
        self.dart_libraries
            .directives
            .insert(file.into(), d.clone());
        d
    }
    fn dart_files(&self) -> Vec<String> {
        let mut files = self
            .sorted_files()
            .map(|v| v.as_ref().clone())
            .unwrap_or_else(|| {
                self.table()
                    .map(|t| t.files.iter().cloned().collect())
                    .unwrap_or_default()
            });
        files.retain(|f| f.ends_with(".dart"));
        files.sort();
        files
    }
    fn dart_package_of(&mut self, file: &str) -> Option<(String, String)> {
        let mut dir = pos_dirname(file).to_string();
        if let Some(p) = self.dart_libraries.package_of.get(&dir) {
            return p.clone();
        }
        let original = dir.clone();
        let found = loop {
            let manifest = pos_join(&dir, "pubspec.yaml");
            if let Some(source) = self.read_file(&manifest) {
                if let Some(m) =
                    re!(r#"(?m)^name[ \t]*:[ \t]*['\"]?([A-Za-z_]\w*)"#).captures(source.text())
                {
                    break Some((m[1].to_string(), dir));
                }
            }
            if dir.is_empty() {
                break None;
            }
            dir = pos_dirname(&dir).to_string();
        };
        self.dart_libraries
            .package_of
            .insert(original, found.clone());
        found
    }
    fn dart_packages(&mut self) -> Vec<(String, String)> {
        if let Some(p) = &self.dart_libraries.packages {
            return p.clone();
        }
        let mut packages = Vec::new();
        for file in self.dart_files() {
            if let Some(p) = self.dart_package_of(&file) {
                if !packages.contains(&p) {
                    packages.push(p);
                }
            }
        }
        self.dart_libraries.packages = Some(packages.clone());
        packages
    }
    fn dart_path_dependencies(&mut self, root: &str, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        for file in ["pubspec_overrides.yaml", "pubspec.yaml"] {
            let Some(source) = self.read_file(&pos_join(root, file)) else {
                continue;
            };
            let lines: Vec<_> = source.text().lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let Some(key) =
                    re!(r#"^([ \t]+)['\"]?([A-Za-z_]\w*)['\"]?[ \t]*:(?:[ \t]+(.*))?$"#)
                        .captures(line)
                else {
                    continue;
                };
                if &key[2] != name {
                    continue;
                }
                let value = key.get(3).map_or("", |m| m.as_str()).trim();
                let mut paths = Vec::new();
                if value.starts_with('{') && value.ends_with('}') {
                    if let Some(m) =
                        re!(r#"[,{][ \t]*path[ \t]*:[ \t]*(?:"([^"]*)"|'([^']*)'|([^,}\s]+))"#)
                            .captures(value)
                    {
                        paths.push(
                            m.get(1)
                                .or_else(|| m.get(2))
                                .or_else(|| m.get(3))
                                .unwrap()
                                .as_str()
                                .to_string(),
                        );
                    }
                } else if value.is_empty() || value.starts_with('#') {
                    let mut child_indent = None;
                    for line in &lines[i + 1..] {
                        if line.trim().is_empty() || line.trim_start().starts_with('#') {
                            continue;
                        }
                        let indent = line.len() - line.trim_start().len();
                        if indent <= key[1].len() {
                            break;
                        }
                        let first = *child_indent.get_or_insert(indent);
                        if indent != first {
                            continue;
                        }
                        if let Some(m) =
                            re!(r#"^[ \t]*path[ \t]*:[ \t]*(?:"([^"]*)"|'([^']*)'|([^\s#]+))"#)
                                .captures(line)
                        {
                            paths.push(
                                m.get(1)
                                    .or_else(|| m.get(2))
                                    .or_else(|| m.get(3))
                                    .unwrap()
                                    .as_str()
                                    .to_string(),
                            );
                        }
                    }
                }
                for path in paths {
                    let path = path.replace('\\', "/");
                    if path.starts_with('/') || re!(r"^[A-Za-z]:").is_match(&path) {
                        continue;
                    }
                    let dir = pos_normalize(&pos_join(root, &path));
                    if dir != ".." && !dir.starts_with("../") {
                        out.push(if dir == "." { String::new() } else { dir });
                    }
                }
            }
        }
        out
    }
    pub(super) fn dart_uri_files(&mut self, from: &str, uri: &str) -> Vec<String> {
        if let Some(package) = uri.strip_prefix("package:") {
            let Some((name, rest)) = package.split_once('/') else {
                return vec![];
            };
            let mut chosen: Vec<_> = self
                .dart_packages()
                .into_iter()
                .filter(|p| p.0 == name)
                .collect();
            if chosen.len() > 1 {
                let own = self.dart_package_of(from);
                if let Some(p) = own.as_ref().filter(|p| p.0 == name) {
                    chosen = vec![p.clone()];
                } else if let Some(p) = chosen
                    .iter()
                    .filter(|p| p.1.is_empty() || from.starts_with(&format!("{}/", p.1)))
                    .max_by_key(|p| p.1.len())
                {
                    chosen = vec![p.clone()];
                } else if let Some((_, root)) = own {
                    let dirs = self.dart_path_dependencies(&root, name);
                    let declared: Vec<_> = chosen
                        .iter()
                        .filter(|p| dirs.contains(&p.1))
                        .cloned()
                        .collect();
                    if declared.len() == 1 {
                        chosen = declared;
                    }
                }
            }
            return chosen
                .iter()
                .map(|p| pos_normalize(&pos_join(&pos_join(&p.1, "lib"), rest)))
                .collect();
        }
        if re!(r"^[A-Za-z][\w+.-]*:").is_match(uri) {
            return vec![];
        }
        let path = pos_normalize(&pos_join(pos_dirname(from), uri));
        if path == ".." || path.starts_with("../") || path.starts_with('/') {
            vec![]
        } else {
            vec![path]
        }
    }
    fn dart_library_of(&mut self, file: &str) -> Option<String> {
        if let Some(v) = self.dart_libraries.library_of.get(file) {
            return v.clone();
        }
        self.dart_libraries.library_of.insert(file.into(), None);
        let d = self.dart_directives(file)?;
        let library = if let Some((parent, uri)) = d.parent {
            let candidates = if uri {
                self.dart_uri_files(file, &parent)
                    .into_iter()
                    .filter(|f| self.dart_directives(f).is_some())
                    .collect::<Vec<_>>()
            } else {
                self.dart_files()
                    .into_iter()
                    .filter(|f| {
                        self.dart_directives(f).and_then(|d| d.name).as_deref() == Some(&parent)
                    })
                    .collect()
            };
            let parent = if candidates.len() == 1 {
                candidates.first().cloned()
            } else {
                let listing: Vec<_> = candidates
                    .into_iter()
                    .filter(|c| {
                        self.dart_directives(c).is_some_and(|d| {
                            d.parts
                                .iter()
                                .any(|uri| self.dart_uri_files(c, uri).iter().any(|f| f == file))
                        })
                    })
                    .collect();
                if listing.len() == 1 {
                    listing.first().cloned()
                } else {
                    None
                }
            };
            parent.and_then(|p| self.dart_library_of(&p))
        } else {
            Some(file.into())
        };
        self.dart_libraries
            .library_of
            .insert(file.into(), library.clone());
        library
    }
    fn dart_library_members(&mut self, library: &str) -> HashSet<String> {
        if let Some(v) = self.dart_libraries.members.get(library) {
            return v.clone();
        }
        let mut files = HashSet::from([library.to_string()]);
        let mut queue = VecDeque::from([library.to_string()]);
        while files.len() < 1000 {
            let Some(file) = queue.pop_front() else {
                break;
            };
            for uri in self.dart_directives(&file).unwrap_or_default().parts {
                for part in self.dart_uri_files(&file, &uri) {
                    if files.insert(part.clone()) {
                        queue.push_back(part);
                    }
                }
            }
        }
        self.dart_libraries
            .members
            .insert(library.into(), files.clone());
        files
    }
    pub(super) fn dart_same_library(&mut self, from: &str, decl: &str) -> bool {
        if from == decl {
            return true;
        }
        let Some(lib) = self.dart_library_of(from) else {
            return false;
        };
        self.dart_library_members(&lib).contains(decl)
    }
    fn dart_exports_name(
        &mut self,
        library: &str,
        decl: &str,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> bool {
        if seen.len() >= 1000 || !seen.insert(library.into()) {
            return false;
        }
        let members = self.dart_library_members(library);
        if members.contains(decl) {
            seen.remove(library);
            return true;
        }
        for file in members {
            for d in self.dart_directives(&file).unwrap_or_default().exports {
                if !d.admits(name) {
                    continue;
                }
                for uri in d.uris {
                    for target in self.dart_uri_files(&file, &uri) {
                        if let Some(lib) = self.dart_library_of(&target) {
                            if self.dart_exports_name(&lib, decl, name, seen) {
                                seen.remove(library);
                                return true;
                            }
                        }
                    }
                }
            }
        }
        seen.remove(library);
        false
    }
    pub(super) fn dart_visible(
        &mut self,
        from: &str,
        decl: &str,
        name: &str,
        prefix: &str,
    ) -> bool {
        let key = (from.into(), decl.into(), name.into(), prefix.into());
        if let Some(v) = self.dart_libraries.visible.get(&key) {
            return *v;
        }
        let answer = self.dart_visible_inner(from, decl, name, prefix);
        self.dart_libraries.visible.insert(key, answer);
        answer
    }
    fn dart_visible_inner(&mut self, from: &str, decl: &str, name: &str, prefix: &str) -> bool {
        if self.dart_package_of(from).is_none() {
            return true;
        }
        let Some(library) = self.dart_library_of(from) else {
            return true;
        };
        if prefix.is_empty() && (from == decl || self.dart_library_members(&library).contains(decl))
        {
            return true;
        }
        if name.starts_with('_') {
            return false;
        }
        let mut found_prefix = prefix.is_empty();
        for file in HashSet::from([from.to_string(), library]) {
            for d in self.dart_directives(&file).unwrap_or_default().imports {
                if d.prefix != prefix {
                    continue;
                }
                found_prefix = true;
                if !d.admits(name) {
                    continue;
                }
                for uri in d.uris {
                    for target in self.dart_uri_files(&file, &uri) {
                        if let Some(lib) = self.dart_library_of(&target) {
                            if self.dart_exports_name(&lib, decl, name, &mut HashSet::new()) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        !found_prefix
    }
    pub(super) fn dart_prefixes(&mut self, from: &str) -> HashSet<String> {
        let lib = self.dart_library_of(from).unwrap_or_else(|| from.into());
        HashSet::from([from.into(), lib])
            .into_iter()
            .flat_map(|f| {
                self.dart_directives(&f)
                    .unwrap_or_default()
                    .imports
                    .into_iter()
                    .map(|d| d.prefix)
                    .filter(|p| !p.is_empty())
            })
            .collect()
    }
    pub(super) fn dart_library_decl(&mut self, n: &KNode) -> Res<bool> {
        if n.language != "dart" || n.qualified_name.contains("::") {
            return Ok(false);
        }
        if !matches!(
            n.kind.as_str(),
            "function"
                | "class"
                | "enum"
                | "type_alias"
                | "variable"
                | "constant"
                | "interface"
                | "struct"
                | "trait"
        ) {
            return Ok(false);
        }
        Ok(!self.nodes_in_file(&n.file_path)?.iter().any(|parent| {
            parent.id != n.id
                && matches!(
                    parent.kind.as_str(),
                    "class" | "enum" | "interface" | "struct" | "trait" | "function" | "method"
                )
                && parent.start_line <= n.start_line
                && parent.end_line >= n.end_line
                && (parent.start_line < n.start_line || parent.end_line > n.end_line)
        }))
    }
    pub(super) fn dart_top_level_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.language != "dart" {
            return Ok(true);
        }
        if self.dart_unnamed_extension(n) {
            return Ok(false);
        }
        if !self.dart_library_decl(n)? {
            return Ok(true);
        }
        let written_prefix = r
            .reference_name
            .split_once('.')
            .map(|(prefix, _)| prefix.to_string())
            .filter(|prefix| self.dart_prefixes(&r.file_path).contains(prefix));
        let prefix = written_prefix.or_else(|| {
            self.read_file(&r.file_path).and_then(|source| {
                let line = source.get((r.line - 1).max(0) as usize)?;
                let at = super::lang_scope::name_start_at_column(
                    line,
                    r.reference_name
                        .rsplit('.')
                        .next()
                        .unwrap_or(&r.reference_name),
                    r.column.max(0) as usize,
                )
                .or_else(|| line.find(&r.reference_name))?;
                re!(r"([A-Za-z_$][\w$]*)\s*\.\s*$")
                    .captures(&line[..at])
                    .map(|m| m[1].to_string())
            })
        });
        Ok(self.dart_visible(
            &r.file_path,
            &n.file_path,
            &n.name,
            prefix.as_deref().unwrap_or(""),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directive_head_retains_alternatives_filters_and_metadata() {
        let d = directives(
            r#"// Copyright
@TestOn('vm')
library app.main;
import 'dart:async';
import 'package:kit/kit.dart' show A, B hide B;
import 'stub.dart' if (dart.library.io) 'io.dart' if (dart.library.js_interop == 'true') 'web.dart';
import 'lazy.dart' deferred as lazy;
export 'src/a.dart' show // one
One, Two;
part 'main.g.dart';
void main() {}
import 'late.dart';
"#,
        );
        assert_eq!(d.name.as_deref(), Some("app.main"));
        assert_eq!(d.parent, None);
        assert_eq!(d.parts, ["main.g.dart"]);
        assert_eq!(
            d.imports.iter().map(|n| n.uris.clone()).collect::<Vec<_>>(),
            vec![
                vec!["dart:async"],
                vec!["package:kit/kit.dart"],
                vec!["stub.dart", "io.dart", "web.dart"],
                vec!["lazy.dart"]
            ]
        );
        assert_eq!(
            d.imports
                .iter()
                .map(|n| n.prefix.as_str())
                .collect::<Vec<_>>(),
            ["", "", "", "lazy"]
        );
        assert_eq!(
            d.imports[1].show.as_ref().unwrap(),
            &HashSet::from(["A".into(), "B".into()])
        );
        assert_eq!(d.imports[1].hide, HashSet::from(["B".into()]));
        assert_eq!(d.exports[0].uris, ["src/a.dart"]);
        assert_eq!(
            d.exports[0].show.as_ref().unwrap(),
            &HashSet::from(["One".into(), "Two".into()])
        );
    }
    #[test]
    fn part_parent_forms_and_unnamed_library() {
        assert_eq!(
            directives("part of 'async.dart';").parent,
            Some(("async.dart".into(), true))
        );
        assert_eq!(
            directives("part of app.named;").parent,
            Some(("app.named".into(), false))
        );
        let d = directives("/// Docs\nlibrary;\nimport 'a.dart';");
        assert_eq!(d.name.as_deref(), Some(""));
        assert_eq!(d.imports[0].uris, ["a.dart"]);
    }
    #[test]
    fn declaration_named_like_directive_stops_head() {
        let d = directives("import 'a.dart';\npart() => 1;\nimport 'b.dart';");
        assert_eq!(d.imports.len(), 1);
        assert_eq!(d.imports[0].uris, ["a.dart"]);
    }
}
