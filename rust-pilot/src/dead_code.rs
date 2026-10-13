//! Port of the query-time derivation in src/graph/dead-code.ts.
use crate::{
    store::{Candidate, Names, Set, Store, unique},
    text::test,
    types::Node,
};
use icu_collator::{Collator, options::CollatorOptions};
use icu_locale::locale;
use indexmap::IndexMap;
use rusqlite::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub const DEAD_CODE_KINDS: &[&str] = &[
    "function",
    "method",
    "class",
    "component",
    "interface",
    "struct",
    "trait",
    "protocol",
    "enum",
    "union",
    "type_alias",
];
pub const MAX_DEAD_CODE_CANDIDATES: usize = 20000;
pub const MAX_OVERRIDE_ANCESTOR_DEPTH: usize = 8;
pub const MAX_CORROBORATION_FILES: usize = 600;
pub const MAX_CORROBORATION_BYTES: u64 = 2_000_000;
const EXTRA_KINDS: &[&str] = &[
    "variable",
    "constant",
    "property",
    "field",
    "enum_member",
    "namespace",
    "module",
];
const CONTAINERS: &[&str] = &[
    "class",
    "interface",
    "struct",
    "trait",
    "protocol",
    "enum",
    "union",
    "type_alias",
];
const DECLARATIONS: &[&str] = &["interface", "trait", "protocol"];
const OVERRIDABLE: &[&str] = &["method", "function", "property", "field"];
const BY_NAME_LANGUAGES: &[&str] = &["typescript", "tsx"];
const HOOKS: &[&str] = &["method", "function"];
const IMPLICIT: &[&str] = &[
    "constructor",
    "main",
    "init",
    "deinit",
    "finalize",
    "destructor",
    "dispose",
    "drop",
    "default",
    "tostring",
    "equals",
    "gethashcode",
    "hashcode",
];
const TEST_SCOPES: &[&str] = &["test", "tests", "__tests__", "spec", "specs", "testing"];
const MARKUP: &[&str] = &[
    "svelte",
    "vue",
    "astro",
    "liquid",
    "html",
    "razor",
    "twig",
    "blade",
    "erb",
    "handlebars",
];
const HEADERS: &[&str] = &[
    ".h", ".hh", ".hpp", ".hxx", ".h++", ".inc", ".d.ts", ".d.mts", ".d.cts", ".pyi", ".pxd",
];
const VENDOR: &[&str] = &[
    "vendor",
    "vendored",
    "third_party",
    "third-party",
    "thirdparty",
    "external",
    "externals",
    "node_modules",
    "bower_components",
    "site-packages",
    "godeps",
    "pods",
    ".venv",
    "venv",
];

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadCodeQuery {
    pub kinds: Option<Vec<String>>,
    #[serde(default)]
    pub include_exported: bool,
    #[serde(default)]
    pub include_tests: bool,
    #[serde(default)]
    pub include_generated: bool,
    pub limit: Option<f64>,
    // Retains whether readSource was supplied as null, distinct from absent.
    #[serde(flatten)]
    pub options: serde_json::Map<String, Value>,
}
#[derive(Serialize)]
pub struct Entry {
    pub node: Node,
    pub members: Vec<Node>,
    pub lines: i64,
    pub exported: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub entries: Vec<Entry>,
    pub total: usize,
    pub candidates: usize,
    pub excluded: IndexMap<String, usize>,
    pub kinds: Vec<String>,
    pub include_exported: bool,
    pub bounded: bool,
    pub corroborated: bool,
}
pub type SourceReader<'a> = dyn Fn(&str) -> Option<String> + 'a;

pub fn build_dead_code_report(store: &Store, root: &Path, query: &DeadCodeQuery) -> Result<Report> {
    let default = |file: &str| default_source_reader(root, file);
    let reader = if query.options.get("readSource") == Some(&Value::Null) {
        None
    } else {
        Some(&default as &SourceReader<'_>)
    };
    build_with_reader(store, query, reader)
}
pub fn build_with_reader(
    store: &Store,
    query: &DeadCodeQuery,
    reader: Option<&SourceReader<'_>>,
) -> Result<Report> {
    let kinds = normalize_kinds(query.kinds.as_deref());
    let limit = query.limit.unwrap_or(200.0).max(1.0) as usize;
    let mut excluded: IndexMap<String, usize> = [
        "tests",
        "generated",
        "exported",
        "exportsUnknown",
        "declarations",
        "decorated",
        "overriding",
        "hooks",
        "implicit",
        "vendored",
        "testScope",
        "markup",
        "unreachableFile",
        "unresolvedName",
        "ambiguousName",
        "mentioned",
        "unreadable",
        "nested",
    ]
    .into_iter()
    .map(|s| (s.into(), 0))
    .collect();
    let mut count = |key: &str| {
        *excluded.get_mut(key).unwrap() += 1;
    };
    let raw = store.unreferenced(&kinds, MAX_DEAD_CODE_CANDIDATES + 1)?;
    let bounded = raw.len() > MAX_DEAD_CODE_CANDIDATES;
    let candidates: Vec<_> = raw.into_iter().take(MAX_DEAD_CODE_CANDIDATES).collect();
    let languages_with_exports = if query.include_exported {
        Set::new()
    } else {
        store.languages_with_exports(
            &candidates
                .iter()
                .map(|r| r.node.language.clone())
                .collect::<Vec<_>>(),
        )?
    };
    let mut surviving = vec![];
    for row in &candidates {
        let node = &row.node;
        let reason = if !query.include_tests && is_test_file(&node.file_path) {
            Some("tests")
        } else if !query.include_generated && row.generated {
            Some("generated")
        } else if !query.include_exported
            && (node.flag("isExported") || is_header_file(&node.file_path))
        {
            Some("exported")
        } else if !query.include_exported && !languages_with_exports.contains(&node.language) {
            Some("exportsUnknown")
        } else if node.flag("isAbstract") {
            Some("declarations")
        } else if is_implicit_entry_name(&node.name) {
            Some("implicit")
        } else if is_vendored_path(&node.file_path) {
            Some("vendored")
        } else if !query.include_tests && is_test_scope(&node.qualified_name) {
            Some("testScope")
        } else if MARKUP.contains(&node.language.as_str()) {
            Some("markup")
        } else {
            None
        };
        if let Some(reason) = reason {
            count(reason);
        } else {
            surviving.push(row);
        }
    }
    let nodes: Vec<_> = surviving.iter().map(|r| &r.node).collect();
    let decorated = carries_decorator(store, &nodes)?;
    let containers = containers_of(store, &nodes)?;
    let overriding = override_candidates(store, &nodes, &containers)?;
    let files = unique(surviving.iter().map(|r| r.node.file_path.clone()));
    let unreachable_files = files_nothing_reaches(store, &files)?;
    surviving.retain(|row| {
        if unreachable_files.contains(&row.node.file_path) {
            count("unreachableFile");
            false
        } else {
            true
        }
    });
    let names: Vec<_> = surviving.iter().map(|r| r.node.name.clone()).collect();
    let unresolved = store.unresolved_names(&names)?;
    let ambiguous = store.ambiguous_names(&names)?;
    let hooks = by_name_hook_candidates(
        store,
        &surviving
            .iter()
            .map(|r| &r.node)
            .filter(|n| !decorated.contains(&n.id) && !overriding.contains(&n.id))
            .collect::<Vec<_>>(),
        &containers,
        &candidates.iter().map(|r| &r.node).collect::<Vec<_>>(),
    )?;
    let mut kept = vec![];
    for row in surviving {
        let node = &row.node;
        let container = containers.get(&node.id);
        let reason = if decorated.contains(&node.id) {
            Some("decorated")
        } else if container.is_some_and(|c| DECLARATIONS.contains(&c.kind.as_str())) {
            Some("declarations")
        } else if overriding.contains(&node.id) {
            Some("overriding")
        } else if hooks.contains(&node.id) {
            Some("hooks")
        } else if unresolved.contains(&node.name) {
            Some("unresolvedName")
        } else if ambiguous.contains(&node.name) {
            Some("ambiguousName")
        } else {
            None
        };
        if let Some(reason) = reason {
            count(reason);
        } else {
            kept.push(row);
        }
    }
    let mut confirmed: Vec<&Candidate> = vec![];
    if let Some(read_source) = reader {
        let mut sources: IndexMap<String, Option<String>> = IndexMap::new();
        let mut scopes: IndexMap<String, Vec<String>> = IndexMap::new();
        for row in &kept {
            let file = &row.node.file_path;
            if !scopes.contains_key(file) {
                let mut scope = vec![file.clone()];
                scope.extend(store.dependents(file)?);
                scopes.insert(file.clone(), scope);
            }
        }
        for row in kept {
            let mut own_readable = false;
            let mut mentions = 0;
            for file in &scopes[&row.node.file_path] {
                if !sources.contains_key(file) {
                    let source = if sources.len() >= MAX_CORROBORATION_FILES {
                        None
                    } else {
                        read_source(file)
                    };
                    sources.insert(file.clone(), source);
                }
                let source = sources[file].as_deref();
                if file == &row.node.file_path {
                    own_readable = source.is_some();
                }
                if let Some(source) = source {
                    mentions += mention_count(source, &row.node.name, 2 - mentions);
                    if mentions >= 2 {
                        break;
                    }
                }
            }
            if !own_readable {
                count("unreadable");
            } else if mentions >= 2 {
                count("mentioned");
            } else {
                confirmed.push(row);
            }
        }
    } else {
        confirmed = kept;
    }
    let kept_ids: Set = confirmed.iter().map(|r| r.node.id.clone()).collect();
    let mut entries = IndexMap::new();
    let mut pending = vec![];
    for row in confirmed {
        if let Some(container) = containers
            .get(&row.node.id)
            .filter(|c| kept_ids.contains(&c.id))
        {
            pending.push((&row.node, &container.id));
            count("nested");
            continue;
        }
        entries.insert(
            row.node.id.clone(),
            Entry {
                node: row.node.clone(),
                members: vec![],
                lines: 1.max(row.node.end_line - row.node.start_line + 1),
                exported: row.node.flag("isExported"),
            },
        );
    }
    for (node, container_id) in pending {
        if let Some(entry) = entries.get_mut(container_id) {
            entry.members.push(node.clone());
        }
    }
    let collator = Collator::try_new(locale!("en-US").into(), CollatorOptions::default())
        .expect("built-in English collation");
    for entry in entries.values_mut() {
        entry.members.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| collator.compare(&a.name, &b.name))
        });
    }
    let mut ranked: Vec<_> = entries.into_values().collect();
    ranked.sort_by(|a, b| {
        b.lines
            .cmp(&a.lines)
            .then_with(|| collator.compare(&a.node.file_path, &b.node.file_path))
            .then(a.node.start_line.cmp(&b.node.start_line))
    });
    let total = ranked.len();
    ranked.truncate(limit);
    Ok(Report {
        entries: ranked,
        total,
        candidates: candidates.len(),
        excluded,
        kinds,
        include_exported: query.include_exported,
        bounded,
        corroborated: reader.is_some(),
    })
}

pub fn default_source_reader(root: &Path, file_path: &str) -> Option<String> {
    // Match path.resolve's lexical traversal check; symlinks are followed by stat/read.
    let normalize = |path: &Path| {
        let mut out = PathBuf::new();
        for part in path.components() {
            match part {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    };
    let root = normalize(root);
    let absolute = normalize(&root.join(file_path));
    if absolute != root && !absolute.starts_with(&root) {
        return None;
    }
    let stat = fs::metadata(&absolute).ok()?;
    if !stat.is_file() || stat.len() > MAX_CORROBORATION_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&fs::read(absolute).ok()?).into_owned())
}
pub fn mention_count(source: &str, name: &str, stop_at: usize) -> usize {
    if name.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut from = 0;
    while let Some(at) = source[from..].find(name) {
        let at = from + at;
        from = at + name.len();
        if !is_identifier_char(source[..at].chars().next_back())
            && !is_identifier_char(source[from..].chars().next())
        {
            count += 1;
            if count >= stop_at {
                return count;
            }
        }
    }
    count
}
fn is_identifier_char(c: Option<char>) -> bool {
    // JS indexes a single UTF-16 unit, so an adjacent surrogate is not a letter.
    c.is_some_and(|c| {
        c == '_' || c == '$' || c.len_utf16() == 1 && test(r"[\p{L}\p{N}]", &c.to_string())
    })
}
fn files_nothing_reaches(store: &Store, files: &[String]) -> Result<Set> {
    let files = unique(files.iter().cloned());
    if files.is_empty() {
        return Ok(Set::new());
    }
    let counts = store.dependent_counts(&files)?;
    Ok(files
        .into_iter()
        .filter(|f| counts.get(f).copied().unwrap_or(0) == 0)
        .collect())
}
pub fn is_header_file(path: &str) -> bool {
    HEADERS.iter().any(|ext| path.to_lowercase().ends_with(ext))
}
pub fn is_test_scope(name: &str) -> bool {
    name.split(['.', ':', '/', '\\', '#', '>'])
        .any(|s| TEST_SCOPES.contains(&s.to_lowercase().as_str()))
}
pub fn is_vendored_path(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|s| VENDOR.contains(&s.to_lowercase().as_str()))
}
pub fn is_implicit_entry_name(name: &str) -> bool {
    test(r"^__[a-zA-Z0-9_]+__$", name) || IMPLICIT.contains(&name.to_lowercase().as_str())
}
fn normalize_kinds(requested: Option<&[String]>) -> Vec<String> {
    let kinds = unique(
        requested
            .into_iter()
            .flatten()
            .filter(|k| DEAD_CODE_KINDS.contains(&k.as_str()) || EXTRA_KINDS.contains(&k.as_str()))
            .cloned(),
    );
    if kinds.is_empty() {
        DEAD_CODE_KINDS.iter().map(|s| s.to_string()).collect()
    } else {
        kinds
    }
}
pub fn is_test_file(path: &str) -> bool {
    if is_test_path(path) {
        return true;
    }
    let lower = path.to_lowercase();
    let scope = if let Some(src) = lower.find("/src/") {
        &lower[..src + 1]
    } else if lower.starts_with("src/") {
        ""
    } else {
        &lower
    };
    [
        "integration",
        "sample",
        "samples",
        "example",
        "examples",
        "fixture",
        "fixtures",
        "benchmark",
        "benchmarks",
        "demo",
        "demos",
    ]
    .iter()
    .any(|dir| scope.contains(&format!("/{dir}/")) || scope.starts_with(&format!("{dir}/")))
}
fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    let file_name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let lower_name = file_name.to_lowercase();
    if lower_name.starts_with("test_")
        || lower_name.starts_with("test.")
        || test(
            r"[._-](test|tests|unittest|unittests|spec|specs)\.[a-z0-9]+$",
            &lower_name,
        )
        || test(
            r"(?:Test|Tests|TestCase|Tester|Spec|Specs)\.[A-Za-z0-9]+$",
            file_name,
        )
    {
        return true;
    }
    [
        "tests",
        "test",
        "__tests__",
        "spec",
        "specs",
        "testlib",
        "testing",
        "e2e",
    ]
    .iter()
    .any(|d| lower.contains(&format!("/{d}/")))
        || ["test", "tests", "spec", "specs", "e2e"]
            .iter()
            .any(|d| lower.starts_with(&format!("{d}/")))
        || test(r"(?:^|/)[A-Za-z0-9]*(?:Test|Tests|Spec)/", path)
        || test(
            r"(?:^|/)(?:[\w.]+[-_]test(?:s|ing)?|testdata|testutils?|test[-_]utils?|fakes?|mocks?|__mocks__|stubs)/",
            &lower,
        )
        || test(r"(?:^|/)(?:unittests|[\w.]+[-_]unittests?)/", &lower)
}

fn containers_of(store: &Store, nodes: &[&Node]) -> Result<IndexMap<String, Node>> {
    let ids: Vec<_> = nodes
        .iter()
        .filter(|n| OVERRIDABLE.contains(&n.kind.as_str()))
        .map(|n| n.id.clone())
        .collect();
    let mut out = IndexMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let edges = store.edges(&ids, &["contains"], true)?;
    let mut by_member = IndexMap::new();
    for edge in edges {
        by_member.entry(edge.target).or_insert(edge.source);
    }
    let containers = store.nodes(&unique(by_member.values().cloned()))?;
    for (member, container) in by_member {
        if let Some(node) = containers
            .get(&container)
            .filter(|n| CONTAINERS.contains(&n.kind.as_str()))
        {
            out.insert(member, node.clone());
        }
    }
    Ok(out)
}
fn override_candidates(
    store: &Store,
    nodes: &[&Node],
    containers: &IndexMap<String, Node>,
) -> Result<Set> {
    let mut dropped = Set::new();
    let container_ids = unique(containers.values().map(|n| n.id.clone()));
    if container_ids.is_empty() {
        return Ok(dropped);
    }
    let reach = ancestors_of(store, &container_ids, &["extends", "implements"])?;
    let all = container_ids
        .iter()
        .chain(reach.keys())
        .cloned()
        .collect::<Vec<_>>();
    let external = store.unresolved_from(&all, &["extends", "implements"])?;
    let mut opaque: Set = container_ids
        .iter()
        .filter(|id| external.contains_key(*id))
        .cloned()
        .collect();
    for (ancestor, roots) in &reach {
        if external.contains_key(ancestor) {
            opaque.extend(roots.iter().cloned());
        }
    }
    let member_edges = store.edges(
        &reach.keys().cloned().collect::<Vec<_>>(),
        &["contains"],
        false,
    )?;
    let mut members: IndexMap<String, Vec<String>> = IndexMap::new();
    for edge in &member_edges {
        members
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
    }
    let member_nodes = store.nodes(
        &member_edges
            .iter()
            .map(|e| e.target.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut names_by_container: IndexMap<String, Set> = IndexMap::new();
    for (ancestor, roots) in &reach {
        let names: Vec<_> = members
            .get(ancestor)
            .into_iter()
            .flatten()
            .filter_map(|id| member_nodes.get(id))
            .filter(|m| OVERRIDABLE.contains(&m.kind.as_str()))
            .map(|m| m.name.clone())
            .collect();
        for root in roots {
            if names.is_empty() {
                opaque.insert(root.clone());
            } else {
                names_by_container
                    .entry(root.clone())
                    .or_default()
                    .extend(names.iter().cloned());
            }
        }
    }
    for node in nodes {
        if let Some(container) = containers.get(&node.id)
            && (opaque.contains(&container.id)
                || names_by_container
                    .get(&container.id)
                    .is_some_and(|names| names.contains(&node.name)))
        {
            dropped.insert(node.id.clone());
        }
    }
    Ok(dropped)
}
fn ancestors_of(store: &Store, ids: &[String], kinds: &[&str]) -> Result<IndexMap<String, Set>> {
    let mut reach: IndexMap<String, Set> = IndexMap::new();
    let mut seen: IndexMap<String, Set> = ids
        .iter()
        .map(|id| (id.clone(), Set::from_iter([id.clone()])))
        .collect();
    let mut frontier = seen.clone();
    for _ in 0..MAX_OVERRIDE_ANCESTOR_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let edges = store.edges(&frontier.keys().cloned().collect::<Vec<_>>(), kinds, false)?;
        let mut next: IndexMap<String, Set> = IndexMap::new();
        for edge in edges {
            if edge.source == edge.target {
                continue;
            }
            let Some(roots) = frontier.get(&edge.source) else {
                continue;
            };
            let visited = seen.entry(edge.target.clone()).or_default();
            for root in roots {
                if !visited.insert(root.clone()) {
                    continue;
                }
                next.entry(edge.target.clone())
                    .or_default()
                    .insert(root.clone());
                reach
                    .entry(edge.target.clone())
                    .or_default()
                    .insert(root.clone());
            }
        }
        frontier = next;
    }
    Ok(reach)
}
fn carries_decorator(store: &Store, nodes: &[&Node]) -> Result<Set> {
    let ids: Vec<_> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut found: Set = store
        .edges(&ids, &["decorates"], false)?
        .into_iter()
        .map(|e| e.source)
        .collect();
    found.extend(store.unresolved_from(&ids, &["decorates"])?.into_keys());
    for node in nodes {
        if node.extra.get("decorators").is_some_and(|v| {
            v.as_array().is_some_and(|a| !a.is_empty()) || v.as_str().is_some_and(|s| !s.is_empty())
        }) {
            found.insert(node.id.clone());
        }
    }
    Ok(found)
}
fn by_name_hook_candidates(
    store: &Store,
    nodes: &[&Node],
    containers: &IndexMap<String, Node>,
    unreferenced: &[&Node],
) -> Result<Set> {
    let mut dropped = Set::new();
    let shaped = |node: &&Node| {
        HOOKS.contains(&node.kind.as_str()) && BY_NAME_LANGUAGES.contains(&node.language.as_str())
    };
    let methods: Vec<_> = nodes
        .iter()
        .copied()
        .filter(shaped)
        .filter(|n| containers.get(&n.id).is_some_and(|c| c.kind == "class"))
        .collect();
    if methods.is_empty() {
        return Ok(dropped);
    }
    let classes: IndexMap<_, _> = methods
        .iter()
        .filter_map(|n| containers.get(&n.id))
        .map(|c| (c.id.clone(), c))
        .collect();
    let registered = carries_decorator(store, &classes.values().copied().collect::<Vec<_>>())?;
    let eligible: Vec<_> = methods
        .into_iter()
        .filter(|n| {
            containers
                .get(&n.id)
                .is_some_and(|c| registered.contains(&c.id))
        })
        .collect();
    if eligible.is_empty() {
        return Ok(dropped);
    }
    let names: Set = eligible.iter().map(|n| n.name.clone()).collect();
    let declarations: Vec<_> = unreferenced
        .iter()
        .copied()
        .filter(|n| names.contains(&n.name))
        .filter(shaped)
        .collect();
    let declared_in = containers_of(
        store,
        &declarations
            .iter()
            .copied()
            .filter(|n| !containers.contains_key(&n.id))
            .collect::<Vec<_>>(),
    )?;
    let mut declaring: IndexMap<String, Set> = IndexMap::new();
    for node in declarations {
        if let Some(c) = containers
            .get(&node.id)
            .or_else(|| declared_in.get(&node.id))
            .filter(|c| c.kind == "class")
        {
            declaring
                .entry(node.name.clone())
                .or_default()
                .insert(c.id.clone());
        }
    }
    let declarers = unique(declaring.values().flatten().cloned());
    let interfaces_of = store.unresolved_from(&declarers, &["implements"])?;
    let interfaces = unique(interfaces_of.values().flatten().cloned());
    if interfaces.is_empty() {
        return Ok(dropped);
    }
    let writers = store.unresolved_sources(&interfaces, &["implements"])?;
    let writer_ids = unique(writers.values().flatten().cloned());
    let writer_nodes = store.nodes(&writer_ids)?;
    let typescript: Vec<_> = writer_nodes
        .values()
        .filter(|n| BY_NAME_LANGUAGES.contains(&n.language.as_str()))
        .collect();
    let typescript_ids: Set = typescript.iter().map(|n| n.id.clone()).collect();
    let registered_writers = carries_decorator(store, &typescript)?;
    let mut implementers: Names = IndexMap::new();
    for (name, ids) in writers {
        let own: Vec<_> = ids
            .into_iter()
            .filter(|id| typescript_ids.contains(id))
            .collect();
        if own
            .iter()
            .filter(|id| registered_writers.contains(*id))
            .count()
            * 2
            > own.len()
        {
            implementers.insert(name, own);
        }
    }
    if implementers.is_empty() {
        return Ok(dropped);
    }
    let implementer_ids = unique(implementers.values().flatten().cloned());
    let ancestors = ancestors_of(store, &implementer_ids, &["extends"])?;
    let mut inherits: Names = IndexMap::new();
    for (ancestor, roots) in &ancestors {
        for root in roots {
            inherits
                .entry(root.clone())
                .or_default()
                .push(ancestor.clone());
        }
    }
    let all: Vec<_> = implementer_ids
        .iter()
        .chain(ancestors.keys())
        .cloned()
        .collect();
    let outside = store.unresolved_from(&all, &["extends"])?;
    let mut opaque: Set = implementer_ids
        .iter()
        .filter(|id| outside.contains_key(*id))
        .cloned()
        .collect();
    for (ancestor, roots) in &ancestors {
        if outside.contains_key(ancestor) {
            opaque.extend(roots.iter().cloned());
        }
    }
    let mut hook_names = Set::new();
    for name in names {
        let Some(declared) = declaring.get(&name) else {
            continue;
        };
        let has = |id: &String| {
            declared.contains(id)
                || opaque.contains(id)
                || inherits
                    .get(id)
                    .is_some_and(|a| a.iter().any(|ancestor| declared.contains(ancestor)))
        };
        let contracts: Set = declared
            .iter()
            .filter_map(|id| interfaces_of.get(id))
            .flatten()
            .cloned()
            .collect();
        for contract in contracts {
            if let Some(own) = implementers.get(&contract) {
                let declared_count = own.iter().filter(|id| declared.contains(*id)).count();
                if declared_count >= 2 && declared_count * 2 > own.len() && own.iter().all(has) {
                    hook_names.insert(name.clone());
                    break;
                }
            }
        }
    }
    for node in eligible {
        if hook_names.contains(&node.name) {
            dropped.insert(node.id.clone());
        }
    }
    Ok(dropped)
}
