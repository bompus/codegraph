//! A C++ construction (`new log::Writer(…)`, `Writer w(…)`, which extraction records by the type's
//! last name) makes the class C++ lookup finds for the type as written there: from the scopes
//! around the code outwards (a gtest `TEST_F` body's fixture right after the body itself, a class
//! scope with its bases), its qualifier followed a segment at a time and an alias to the class it
//! names. leveldb's db_impl.cc writes `new log::Writer(…)` in `DBImpl` methods, which also
//! construct the `DBImpl::Writer` it defines out of line as `Writer w(…)`; by the last name alone
//! the two were one.
//!  - At 0.7 when lookup doesn't place the type and the class is the only one of that name the
//!    file can see.
//!  - An enum lookup finds is the enum: rocksdb's `const ValueType types[] = {…}`.
//!  - Through an alias the calling function declares itself, the class it names, if one of the
//!    project's; else nothing.
//!  - Nothing (no edge) for a template parameter, a member of a type from outside the project
//!    (`stl_wrappers::KVMap::value_type`) or a function's own alias of one. A member of an alias
//!    lookup can't follow that names no standard type is left undecided when the project
//!    declares a class of the member's name.
//!  - A header of a test tree is seen only from inside that tree or by including it.
//!  - Undecided, for the name strategies, when nothing the file sees is named so.
//!
//! Of several classes or aliases the file declares under one qualified name (the local classes of
//! separate gtest `TEST_F` bodies, the members of unnamed specializations), the one declared
//! around the construction.
//!
//! Port of upstream's `matchCppConstruction`, `cppEnumWritten`, `cppNamesakeAround` and
//! `cppTypeWrittenAt` (name-matcher.ts at 34dd3d2f, #2542), with the fixture scope and local-class
//! visibility of #2551 (`seenFrom`, cpp-supertypes.ts at d94a92ea).
use super::cpp_aliases::type_segments;
use super::*;

/// What a lookup of a written type found.
enum Lookup {
    /// The class, enum or alias, and whether C++ lookup placed it (rather than its being the only
    /// one of that name the file can see).
    Found(Arc<KNode>, bool),
    /// The name names no particular class of the project's.
    NoClass,
    /// Nothing the file sees is named so.
    Unknown,
}

fn type_kind(n: &KNode) -> bool {
    matches!(n.language.as_str(), "c" | "cpp") && matches!(n.kind.as_str(), "class" | "struct" | "union" | "type_alias" | "enum")
}

fn class_like(n: &KNode) -> bool {
    matches!(n.language.as_str(), "c" | "cpp") && matches!(n.kind.as_str(), "class" | "struct" | "union")
}

/// Whether a C or C++ file is a source file (a translation unit), whose classes only it sees.
fn source_file(file: &str) -> bool {
    re!(r"(?i)\.(?:c|cc|cpp|cxx|c\+\+|cu|metal)$").is_match(file)
}

/// The type name `text` starts with, template arguments and all: `log::Writer` of `log::Writer(…)`,
/// `Outer<int>::Inner` of `Outer<int>::Inner x(…)`. None when it starts with none, or a template
/// argument list goes on past the line. (A `>` in parentheses compares: `Bits<(N > 8)>`.)
fn type_written_at(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut end = 0;
    loop {
        let segment = if end == 0 { re!(r"^(?:::\s*)?[A-Za-z_]\w*") } else { re!(r"^\s*::\s*[A-Za-z_]\w*") };
        let Some(name) = segment.find(&text[end..]) else { return (end > 0).then(|| text[..end].to_string()) };
        end += name.end();
        let Some(open) = re!(r"^\s*<").find(&text[end..]) else { continue };
        end += open.end();
        let (mut angles, mut parens) = (1, 0);
        while angles > 0 {
            match bytes.get(end)? {
                b'(' => parens += 1,
                b')' => parens -= 1,
                _ if parens > 0 => {}
                b'<' => angles += 1,
                b'>' => angles -= 1,
                _ => {}
            }
            end += 1;
        }
    }
}

/// One of several declarations of a class: the one in `file` itself, then in its header, then the
/// nearest by path.
fn pick(classes: Vec<Arc<KNode>>, file: &str) -> Option<Arc<KNode>> {
    if classes.len() <= 1 { return classes.into_iter().next(); }
    if let Some(own) = classes.iter().find(|n| n.file_path == file) { return Some(own.clone()); }
    let stem = |p: &str| p.rsplit_once('.').filter(|(head, _)| !head.ends_with('/')).map_or(p.to_string(), |(head, _)| head.to_string());
    if let Some(header) = classes.iter().find(|n| stem(&n.file_path) == stem(file)) { return Some(header.clone()); }
    let shared = |p: &str| {
        let (a, b): (Vec<_>, Vec<_>) = (p.split('/').collect(), file.split('/').collect());
        a.iter().zip(&b).take(a.len().min(b.len()).saturating_sub(1)).take_while(|(x, y)| x == y).count()
    };
    classes.into_iter().reduce(|best, n| if shared(&n.file_path) > shared(&best.file_path) { n } else { best })
}

impl KernelResolver {
    /// The class (or enum, or alias) a C++ `instantiates` ref makes: Some(None) for no edge, None
    /// to leave it to the name strategies.
    pub(super) fn cpp_construction(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "cpp" || r.reference_kind != "instantiates" { return Ok(None); }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = source.get((r.line - 1).max(0) as usize) else { return Ok(None) };
        let text = &line[super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len())..];
        // (A declaration's column is where its specifiers start: `const Slice key(…)`.)
        let specifiers = re!(r"^(?:(?:new|const|volatile|static|thread_local|constexpr|constinit|mutable|struct|class|union|typename)\s+)*").find(text).map_or(0, |m| m.end());
        let Some(written) = type_written_at(&text[specifiers..]) else { return Ok(None) };
        let made = |node: Arc<KNode>, exact: bool| KCand { node, confidence: if exact { 0.9 } else { 0.7 }, resolved_by: if exact { "qualified-name" } else { "exact-match" } };
        let Some(parts) = type_segments(&written) else { return Ok(Some(None)) };
        let global = written.trim_start().starts_with("::");
        let visible = self.cpp_construction_visible_files(&r.file_path)?;
        if !global && self.cpp_alias_declaration(&written, &parts, r, 0, &visible)?.is_some_and(|(_, declared_in, _)| declared_in.id == caller.id) {
            let class = match self.cpp_alias_expansion(&written, r, 0, &visible)? {
                Some(super::cpp_aliases::CppAlias { raw: Some(raw), site, pointer: false, .. }) => self.cpp_type_owner_visible(&raw, &site, 0, true, &visible)?.filter(|n| class_like(n)),
                _ => None,
            };
            return Ok(Some(class.map(|n| made(n, true))));
        }
        if !global && self.cpp_template_parameter(&caller, &parts[0], r)? { return Ok(Some(None)); }
        let (node, exact) = match self.cpp_construction_lookup(&written, &parts, global, &caller, r, &visible)? {
            Lookup::Found(node, exact) => (node, exact),
            Lookup::NoClass => return Ok(Some(None)),
            Lookup::Unknown => return Ok(None),
        };
        let namesakes: Vec<_> = self.nodes_by_qualified_name(&node.qualified_name)?.iter()
            .filter(|n| n.file_path == r.file_path && type_kind(n) && n.kind != "enum").cloned().collect();
        if namesakes.len() < 2 { return Ok(Some(Some(made(node, exact)))); }
        Ok(self.cpp_namesake_around(&namesakes, r)?.map(|n| Some(made(n, exact))))
    }

    /// The files whose declarations a C or C++ file can see without knowing its includes: its own
    /// and every header, but for a header of a test tree, which code outside one sees only by
    /// including it.
    fn cpp_construction_visible_files(&mut self, file: &str) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = self.cpp_visible_memo.get(file) { return Ok(hit.clone()); }
        let files = match self.sorted_files() { Some(files) => files, None => Arc::new(self.table()?.files.iter().cloned().collect()) };
        let mut visible = HashSet::new();
        for f in files.iter().filter(|f| *f == file || !source_file(f)) {
            if !self.cpp_unincluded_test_header(f, file)? { visible.insert(f.clone()); }
        }
        let visible = Rc::new(visible);
        self.cpp_visible_memo.insert(file.to_string(), visible.clone());
        Ok(visible)
    }

    /// Whether `header` is a header of a test tree (a test directory's child: `tests/abi/`) that
    /// `file`, outside that tree, doesn't include: nlohmann/json's examples and benchmarks
    /// construct `json::json_pointer` and `json` with the library's headers, not with the old
    /// release `tests/abi/include/nlohmann/json_v3_10_5.hpp` keeps.
    fn cpp_unincluded_test_header(&mut self, header: &str, file: &str) -> Res<bool> {
        if header == file || !is_test_path(header) { return Ok(false); }
        let dirs: Vec<_> = header.split('/').collect();
        let dirs = &dirs[..dirs.len() - 1];
        let Some(at) = (1..=dirs.len()).find(|&i| is_test_path(&format!("{}/x", dirs[..i].join("/")))) else { return Ok(false) };
        let root = format!("{}/", dirs[..(at + 1).min(dirs.len())].join("/"));
        if file.starts_with(&root) { return Ok(false); }
        Ok(!self.namespace_visible_files(file, "cpp")?.contains(header))
    }

    /// Whether code at `r` sees the declaration `n`: one in its own file or a header (of a test
    /// tree only from a test or by including it), and a class local to a function only inside
    /// that function (the index names it after the function, and every gtest `TEST_F` body in a
    /// namespace is `ns::TEST_F`).
    fn cpp_construction_sees(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.file_path != r.file_path && source_file(&n.file_path) { return Ok(false); }
        if self.cpp_unincluded_test_header(&n.file_path, &r.file_path)? { return Ok(false); }
        let Some((scope, _)) = n.qualified_name.rsplit_once("::") else { return Ok(true) };
        let around = self.nodes_by_qualified_name(scope)?.iter().find(|f| matches!(f.kind.as_str(), "function" | "method")
            && f.file_path == n.file_path && f.start_line <= n.start_line && f.end_line >= n.start_line).cloned();
        Ok(around.is_none_or(|f| f.file_path == r.file_path && f.start_line <= r.line && r.line <= f.end_line))
    }

    /// The types named `name` in the scope `scope` that code at `r` sees, else in the classes a
    /// class of that name derives from, nearest first.
    pub(super) fn cpp_scope_types(&mut self, scope: &str, name: &str, r: &ResolveRefIn, depth: u32) -> Res<Vec<Arc<KNode>>> {
        let qualified = if scope.is_empty() { name.to_string() } else { format!("{scope}::{name}") };
        let mut found = Vec::new();
        for n in self.nodes_by_qualified_name(&qualified)?.iter().filter(|n| type_kind(n)) {
            if self.cpp_construction_sees(n, r)? { found.push(n.clone()); }
        }
        if !found.is_empty() || scope.is_empty() || depth >= 4 { return Ok(found); }
        let classes: Vec<_> = self.nodes_by_qualified_name(scope)?.iter().filter(|n| class_like(n)).cloned().collect();
        for class in classes {
            for base in self.supertype_nodes(&class.id)?.into_iter().filter(|n| class_like(n)) {
                let inherited = self.cpp_scope_types(&base.qualified_name, name, r, depth + 1)?;
                if !inherited.is_empty() { return Ok(inherited); }
            }
        }
        Ok(Vec::new())
    }

    /// The class (or enum, or alias of a type outside the project) `written` names, looked up as
    /// C++ looks it up from `caller`'s body.
    fn cpp_construction_lookup(&mut self, written: &str, parts: &[String], global: bool, caller: &KNode, r: &ResolveRefIn, visible: &HashSet<String>) -> Res<Lookup> {
        let mut scopes: Vec<String> = Vec::new();
        if !global && caller.kind != "file" {
            let names: Vec<_> = caller.qualified_name.split("::").collect();
            scopes = (1..=names.len()).rev().map(|i| names[..i].join("::")).collect();
            if let Some(fixture) = self.cpp_gtest_fixture(caller, r)? { scopes.insert(1.min(scopes.len()), fixture); }
        }
        scopes.push(String::new());
        for scope in &scopes {
            // Namespaces have no nodes: the first type is the shortest prefix of the path that
            // names one.
            let mut first = None;
            for count in 1..=parts.len() {
                let found = self.cpp_scope_types(scope, &parts[..count].join("::"), r, 0)?;
                if let Some(found) = pick(found, &r.file_path) { first = Some((found, count)); break; }
            }
            let Some((mut current, consumed)) = first else { continue };
            for part in &parts[consumed..] {
                match current.kind.as_str() {
                    "enum" => return Ok(Lookup::NoClass),
                    "type_alias" => match self.cpp_expand_owner(&current, r, 0, false, visible)? {
                        Some(class) => current = class,
                        // An alias of a standard type (`using Bytes = std::string;`) has no
                        // member of the project's. Another the lookup can't follow may lead to a
                        // class of the member's name through what it doesn't read: nlohmann/json's
                        // `using json = nlohmann::json;` names a namespace a macro opens.
                        None if !self.cpp_alias_of_std(&current) && self.cpp_project_type_named(part, r)? => return Ok(Lookup::Unknown),
                        None => return Ok(Lookup::NoClass),
                    },
                    _ => {}
                }
                let found = self.cpp_scope_types(&current.qualified_name, part, r, 0)?;
                let Some(next) = pick(found, &r.file_path) else { return Ok(Lookup::Unknown) };
                current = next;
            }
            if current.kind == "type_alias" {
                if let Some(class) = self.cpp_expand_owner(&current, r, 0, false, visible)? { current = class; }
            }
            return Ok(Lookup::Found(current, true));
        }
        if !global {
            if let Some(found) = self.cpp_type_via_using(written, parts, r, visible)?.filter(|n| class_like(n)) { return Ok(Lookup::Found(found, true)); }
        }
        // The only class of that name the file can see, by its qualified tail.
        let path = parts.join("::");
        let mut tail = Vec::new();
        for n in self.nodes_by_name(parts.last().unwrap())?.iter().filter(|n| class_like(n) && (n.qualified_name == path || n.qualified_name.ends_with(&format!("::{path}")))) {
            if self.cpp_construction_sees(n, r)? { tail.push(n.clone()); }
        }
        let own: Vec<_> = tail.iter().filter(|n| n.file_path == r.file_path).cloned().collect();
        let sole = if own.is_empty() { tail } else { own };
        Ok(match sole.as_slice() { [only] => Lookup::Found(only.clone(), false), _ => Lookup::Unknown })
    }

    /// Whether the alias `alias` names a type of the standard library: `using Bytes = std::string;`,
    /// `typedef std::map<int, int> Counts;`.
    fn cpp_alias_of_std(&mut self, alias: &KNode) -> bool {
        let Some(source) = self.read_file(&alias.file_path) else { return false };
        let text = source.get((alias.start_line - 1).max(0) as usize..alias.end_line.max(0) as usize).unwrap_or(&[]).join(" ");
        re!(r"(?:=|\btypedef)\s*(?:typename\s+)?(?:::)?std\s*::").is_match(&text)
    }

    /// Whether a class the file at `r` can see is named `name`.
    fn cpp_project_type_named(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        for n in self.nodes_by_name(name)?.iter().filter(|n| class_like(n)) {
            if self.cpp_construction_sees(n, r)? { return Ok(true); }
        }
        Ok(false)
    }

    /// The type `raw` names, looked up from the scopes around `r`'s caller a segment at a time,
    /// each class's bases included: protobuf's `using ConformanceRequestSetting =
    /// BinaryAndJsonConformanceSuite::ConformanceRequestSetting;` names a class the suite inherits
    /// from `ConformanceTestSuite`. (Port of upstream's #2549.)
    pub(super) fn cpp_type_through_bases(&mut self, raw: &str, r: &ResolveRefIn, included: &HashSet<String>) -> Res<Option<Arc<KNode>>> {
        let Some(parts) = type_segments(raw) else { return Ok(None) };
        let mut scopes: Vec<String> = Vec::new();
        if !raw.trim_start().starts_with("::") {
            if let Some(caller) = self.node_by_id(&r.from_node_id)? {
                let mut names: Vec<_> = caller.qualified_name.split("::").map(str::to_string).collect();
                if is_inheritance_ref(&r.reference_kind) { names.pop(); }
                scopes = (1..=names.len()).rev().map(|i| names[..i].join("::")).collect();
            }
        }
        scopes.push(String::new());
        for scope in &scopes {
            let mut first = None;
            for count in 1..=parts.len() {
                let found = self.cpp_scope_types(scope, &parts[..count].join("::"), r, 0)?;
                let found = found.into_iter().filter(|n| included.contains(&n.file_path)).collect();
                if let Some(found) = pick(found, &r.file_path) { first = Some((found, count)); break; }
            }
            let Some((mut current, consumed)) = first else { continue };
            for part in &parts[consumed..] {
                if !class_like(&current) { return Ok(None); }
                let next: Vec<_> = self.cpp_scope_types(&current.qualified_name, part, r, 0)?.into_iter().filter(|n| included.contains(&n.file_path)).collect();
                let Some(next) = pick(next, &r.file_path) else { return Ok(None) };
                current = next;
            }
            return Ok(Some(current));
        }
        Ok(None)
    }

    /// Of several classes or aliases `r`'s file declares under one qualified name, the one whose
    /// holder (the function or class it is declared in, else the file) holds `r`. None unless
    /// exactly one is.
    fn cpp_namesake_around(&mut self, namesakes: &[Arc<KNode>], r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let nodes = self.nodes_in_file(&r.file_path)?;
        let holders: Vec<_> = nodes.iter().filter(|n| matches!(n.kind.as_str(), "function" | "method" | "class" | "struct" | "union")).collect();
        let around: Vec<_> = namesakes.iter().filter(|cls| {
            let holder = holders.iter().filter(|n| n.id != cls.id && n.start_line <= cls.start_line && n.end_line >= cls.end_line)
                .min_by_key(|n| n.end_line - n.start_line);
            holder.is_none_or(|h| h.start_line <= r.line && r.line <= h.end_line)
        }).cloned().collect();
        Ok(match around.as_slice() { [only] => Some(only.clone()), _ => None })
    }
}

#[cfg(test)]
mod tests {
    use super::type_written_at;

    #[test]
    fn reads_the_type_a_construction_writes() {
        assert_eq!(type_written_at("log::Writer(&file);").as_deref(), Some("log::Writer"));
        assert_eq!(type_written_at("Outer<(sizeof(int) > 2)>::Inner();").as_deref(), Some("Outer<(sizeof(int) > 2)>::Inner"));
        assert_eq!(type_written_at("Map<int, std::vector<int>> m;").as_deref(), Some("Map<int, std::vector<int>>"));
        assert_eq!(type_written_at("Map<int,").as_deref(), None);
        assert_eq!(type_written_at("(x)"), None);
    }
}
