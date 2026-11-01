//! Which project crate a Rust `use` path can name an item of (upstream's `rustManifestDependencies`,
//! `rustCrateGraph` and `rustUseCanReach`, name-matcher.ts at 65d73423, #2692).
//!
//! A name a `use` binds is an item of the crate its path starts from, or of a project crate that
//! one depends on: tokio's tests' `use tokio_test::task::spawn` is tokio-test's, never the
//! examples crate's `pub fn spawn`, which nothing depends on.
//!
//! The crates are the manifests above the indexed `.rs` files and the path dependencies they
//! name; upstream reads the root package, the workspace's members and their path dependencies.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DepKind {
    Normal,
    Dev,
    Build,
    Workspace,
}

pub(super) struct ManifestDep {
    /// The name code writes for it (`futures_util`).
    pub(super) key: String,
    /// The package it is: its key, unless `package = "…"` renames it.
    pub(super) pkg: String,
    /// Its `path = "…"`, from the manifest's directory.
    pub(super) path: Option<String>,
    /// `workspace = true`: the workspace's `[workspace.dependencies]` declares it.
    pub(super) workspace: bool,
    pub(super) kind: DepKind,
}

/// A Cargo manifest's dependencies, however each is written: `foo = "1"`, an inline table, dotted
/// keys (`foo.workspace = true`) or a `[dependencies.foo]` table of its own.
pub(super) fn rust_manifest_dependencies(text: &str) -> Vec<ManifestDep> {
    /// The dependency `name` of `kind`, made on first mention (a table and a dotted key may both name it).
    fn entry(deps: &mut Vec<ManifestDep>, index: &mut HashMap<(u8, String), usize>, kind: DepKind, name: &str) -> usize {
        let key = name.replace('-', "_");
        *index.entry((kind as u8, key.clone())).or_insert_with(|| {
            deps.push(ManifestDep { pkg: key.clone(), key, path: None, workspace: false, kind });
            deps.len() - 1
        })
    }
    fn read(dep: &mut ManifestDep, fields: &str) {
        if let Some(m) = re!(r#"\bpackage\s*=\s*"([^"]+)""#).captures(fields) {
            dep.pkg = m[1].replace('-', "_");
        }
        if let Some(m) = re!(r#"\bpath\s*=\s*"([^"]+)""#).captures(fields) {
            dep.path = Some(m[1].replace('\\', "/"));
        }
        if re!(r"\bworkspace\s*=\s*true\b").is_match(fields) {
            dep.workspace = true;
        }
    }
    let mut deps: Vec<ManifestDep> = Vec::new();
    let mut index: HashMap<(u8, String), usize> = HashMap::new();
    let mut kind: Option<DepKind> = None;
    let mut table: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(header) = re!(r"^\[([^\]]+)\]$").captures(line) {
            let m = re!(r"^(?:(workspace)\.|target\..+\.)?(dev-|build-)?dependencies(?:\.([A-Za-z0-9_-]+))?$").captures(header[1].trim());
            let of = m.as_ref().map(|m| {
                if m.get(1).is_some() {
                    DepKind::Workspace
                } else {
                    match m.get(2).map(|g| g.as_str()) {
                        Some("dev-") => DepKind::Dev,
                        Some("build-") => DepKind::Build,
                        _ => DepKind::Normal,
                    }
                }
            });
            let named = m.as_ref().and_then(|m| m.get(3)).map(|g| g.as_str().to_string());
            kind = if named.is_none() { of } else { None };
            table = match (of, named) {
                (Some(of), Some(name)) => Some(entry(&mut deps, &mut index, of, &name)),
                _ => None,
            };
            continue;
        }
        if let Some(t) = table {
            read(&mut deps[t], line);
            continue;
        }
        let Some(kind) = kind else { continue };
        if let Some(dep) = re!(r#"^"?([A-Za-z0-9_-]+)"?\s*(?:\.\s*([A-Za-z_-]+)\s*)?=\s*(.*)$"#).captures(line) {
            let at = entry(&mut deps, &mut index, kind, &dep[1]);
            let fields = match dep.get(2) {
                Some(sub) => format!("{} = {}", sub.as_str(), &dep[3]),
                None => dep[3].to_string(),
            };
            read(&mut deps[at], &fields);
        }
    }
    deps
}

/// parseTargetPaths (cargo-workspace.ts): the target roots a manifest names with a `path` of their
/// own — `[lib] path = "…"`, each `[[bin]] path = "…"` (ripgrep's `crates/core/main.rs`), and each
/// `[[example]]`, `[[test]]` and `[[bench]]` one (dioxus's
/// `examples/07-fullstack/handling_errors.rs`), as written — relative to the manifest's directory.
fn rust_manifest_target_paths(text: &str) -> (Option<String>, Vec<String>, Vec<String>) {
    let mut lib = None;
    let mut bins = Vec::new();
    let mut others = Vec::new();
    let mut table = "";
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            table = match line {
                "[lib]" => "lib",
                "[[bin]]" => "bin",
                _ if re!(r"^\[\[(?:example|test|bench)\]\]$").is_match(line) => "other",
                _ => "",
            };
            continue;
        }
        let Some(at) = (!table.is_empty())
            .then(|| re!(r#"^path\s*=\s*["']([^"'\n]+)["']"#).captures(line))
            .flatten()
            .map(|m| m[1].replace('\\', "/"))
        else {
            continue;
        };
        match table {
            "lib" => lib = Some(at),
            "bin" => bins.push(at),
            _ => others.push(at),
        }
    }
    (lib, bins, others)
}

pub(super) struct RustCrate {
    /// Its package name as code writes it (`tokio_test`).
    pub(super) name: String,
    /// Each dependency of any kind that is a project crate: the name code writes → that crate's directory.
    pub(super) keys: HashMap<String, String>,
    /// The library root its manifest gives a `path` of its own, as a project file.
    pub(super) lib: Option<String>,
    /// The binaries' roots its manifest gives a `path` (ripgrep's `crates/core/main.rs`), as project files.
    pub(super) bins: Vec<String>,
    /// The examples', tests' and benches' roots its manifest gives a `path`, as project files: each its own crate.
    pub(super) others: Vec<String>,
}

pub(super) struct RustCrateGraph {
    /// The project's crates by directory: the root package's is "".
    pub(super) crates: HashMap<String, RustCrate>,
    /// A crate's directory → itself and every project crate it depends on, transitively.
    reach: HashMap<String, HashSet<String>>,
}

/// `rel` from the project directory `dir`, as a project directory; None when it leaves the project.
pub(super) fn join_project_dir(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for segment in dir.split('/').chain(rel.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    (!rel.starts_with('/')).then(|| parts.join("/"))
}

fn package_name(text: &str) -> Option<String> {
    re!(r#"(?s)\[package\][^\[]*?\bname\s*=\s*"([^"]+)""#).captures(text).map(|m| m[1].replace('-', "_"))
}

/// The crates found so far, in the order they were found.
#[derive(Default)]
struct GraphBuilder {
    crates: HashMap<String, RustCrate>,
    manifests: HashMap<String, String>,
    by_name: HashMap<String, String>,
    order: Vec<String>,
}

impl GraphBuilder {
    /// Reads the crate whose manifest is in `dir`; false when there is none.
    fn add(&mut self, resolver: &mut KernelResolver, dir: &str) -> bool {
        if self.crates.contains_key(dir) {
            return true;
        }
        let path = if dir.is_empty() { "Cargo.toml".to_string() } else { format!("{dir}/Cargo.toml") };
        let Some(source) = resolver.read_file(&path) else { return false };
        let Some(name) = package_name(source.text()) else { return false };
        let (lib, bins, others) = rust_manifest_target_paths(source.text());
        let files = |rels: Vec<String>| {
            rels.iter()
                .filter_map(|rel| join_project_dir(dir, rel))
                .collect()
        };
        self.crates.insert(
            dir.to_string(),
            RustCrate {
                name: name.clone(),
                keys: HashMap::new(),
                lib: lib.and_then(|rel| join_project_dir(dir, &rel)),
                bins: files(bins),
                others: files(others),
            },
        );
        self.manifests.insert(dir.to_string(), source.text().to_string());
        self.by_name.entry(name).or_insert_with(|| dir.to_string());
        self.order.push(dir.to_string());
        true
    }
}

impl KernelResolver {
    /// The manifests above the indexed `.rs` files, the root's first.
    pub(super) fn rust_manifest_files(&mut self) -> Res<Vec<String>> {
        let mut manifests: HashSet<String> = HashSet::from(["Cargo.toml".to_string()]);
        let files = match self.sorted_files() {
            Some(files) => files,
            None => Arc::new(self.table()?.files.iter().cloned().collect()),
        };
        for file in files.iter().filter(|f| f.ends_with(".rs")) {
            let mut dir = pos_dirname(file);
            while dir != "." && !dir.is_empty() {
                manifests.insert(format!("{dir}/Cargo.toml"));
                let next = pos_dirname(dir);
                if next == dir {
                    break;
                }
                dir = next;
            }
        }
        let mut sorted: Vec<String> = manifests.into_iter().collect();
        sorted.sort();
        sorted.retain(|m| m != "Cargo.toml");
        sorted.insert(0, "Cargo.toml".to_string());
        Ok(sorted)
    }

    /// The project's crates — the root package, the others the manifests name, and the path
    /// dependencies they name — and which of them each depends on. A dependency is found by its
    /// `path` (its own or, for `workspace = true`, the workspace's), so a renamed key or a crate
    /// outside the members still lands; without one, by its package name.
    pub(super) fn rust_crate_graph(&mut self) -> Res<Rc<RustCrateGraph>> {
        if let Some(hit) = &self.rust_crate_graph_memo {
            return Ok(hit.clone());
        }
        let mut b = GraphBuilder::default();
        for manifest in self.rust_manifest_files()? {
            b.add(self, manifest.strip_suffix("Cargo.toml").unwrap_or("").trim_end_matches('/'));
        }
        let inherited: HashMap<String, ManifestDep> = self
            .read_file("Cargo.toml")
            .map(|s| rust_manifest_dependencies(s.text()))
            .unwrap_or_default()
            .into_iter()
            .filter(|dep| dep.kind == DepKind::Workspace)
            .map(|dep| (dep.key.clone(), dep))
            .collect();
        let mut normal: HashMap<String, HashSet<String>> = HashMap::new();
        let mut i = 0;
        while i < b.order.len() {
            let dir = b.order[i].clone();
            i += 1;
            for dep in rust_manifest_dependencies(&b.manifests[&dir]) {
                if dep.kind == DepKind::Workspace {
                    continue;
                }
                let shared = if dep.workspace { inherited.get(&dep.key) } else { None };
                let at = match shared {
                    Some(shared) => shared.path.as_deref().and_then(|p| join_project_dir("", p)),
                    None => dep.path.as_deref().and_then(|p| join_project_dir(&dir, p)),
                };
                let mut to = None;
                if let Some(at) = &at {
                    if b.add(self, at) {
                        to = Some(at.clone());
                    }
                }
                let to = to.or_else(|| b.by_name.get(shared.map_or(&dep.pkg, |s| &s.pkg)).cloned());
                let Some(to) = to.filter(|to| *to != dir) else { continue };
                b.crates.get_mut(&dir).unwrap().keys.insert(dep.key.clone(), to.clone());
                if dep.kind == DepKind::Normal {
                    normal.entry(dir.clone()).or_default().insert(to);
                }
            }
        }
        let mut reach = HashMap::new();
        for dir in b.crates.keys() {
            let mut seen: HashSet<String> = HashSet::from([dir.clone()]);
            let mut pending = vec![dir.clone()];
            while let Some(next) = pending.pop() {
                for to in normal.get(&next).into_iter().flatten() {
                    if seen.insert(to.clone()) {
                        pending.push(to.clone());
                    }
                }
            }
            reach.insert(dir.clone(), seen);
        }
        let graph = Rc::new(RustCrateGraph { crates: b.crates, reach });
        self.rust_crate_graph_memo = Some(graph.clone());
        Ok(graph)
    }

    /// Whether a `use` path from one of `roots` can name `candidate`, by its crate. A path from a
    /// crate the file's own manifest depends on (`use tokio_test::task::spawn`), or from its own
    /// crate's name, names that crate's item or one it re-exports from a crate its
    /// `[dependencies]` name, in turn; `crate::`, `self::` and `super::` start from the file's own
    /// crate. Any other path — through a module, a crate from outside the project — and a file in
    /// no crate the manifests name say nothing.
    pub(super) fn rust_use_can_reach(&mut self, roots: &HashSet<String>, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let graph = self.rust_crate_graph()?;
        let crate_dir_of = |file: &str| -> Option<String> {
            let mut best: Option<&String> = graph.crates.contains_key("").then(|| graph.crates.get_key_value("").unwrap().0);
            for dir in graph.crates.keys() {
                if !dir.is_empty() && file.strip_prefix(dir.as_str()).is_some_and(|rest| rest.starts_with('/')) && best.is_none_or(|b| dir.len() > b.len()) {
                    best = Some(dir);
                }
            }
            best.cloned()
        };
        let (Some(at), Some(own)) = (crate_dir_of(&candidate.file_path), crate_dir_of(&r.file_path)) else { return Ok(true) };
        let crate_ = &graph.crates[&own];
        for root in roots {
            let from = if matches!(root.as_str(), "crate" | "self" | "super") || *root == crate_.name {
                Some(&own)
            } else {
                crate_.keys.get(root)
            };
            if from.is_none_or(|from| graph.reach[from].contains(&at)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{join_project_dir, rust_manifest_dependencies, DepKind};

    #[test]
    fn manifest_dependencies_in_every_spelling() {
        let deps = rust_manifest_dependencies(
            "[package]\nname = \"app\"\n\n[dependencies]\nfacade = { path = \"../facade\" }\nshared = { package = \"shared-types\", path = \"../types\" }\n\n[dev-dependencies]\nhelper.workspace = true\n\n[build-dependencies.cc]\nversion = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n",
        );
        let find = |key: &str| deps.iter().find(|d| d.key == key).unwrap();
        assert_eq!(find("facade").path.as_deref(), Some("../facade"));
        assert_eq!(find("shared").pkg, "shared_types");
        assert!(find("helper").workspace && find("helper").kind == DepKind::Dev);
        assert!(find("cc").kind == DepKind::Build);
        assert!(find("libc").kind == DepKind::Normal);
    }

    #[test]
    fn project_directories_stay_inside_the_project() {
        assert_eq!(join_project_dir("app", "../facade").as_deref(), Some("facade"));
        assert_eq!(join_project_dir("", "helper").as_deref(), Some("helper"));
        assert_eq!(join_project_dir("", ".").as_deref(), Some(""));
        assert_eq!(join_project_dir("app", "../../x"), None);
        assert_eq!(join_project_dir("app", "/abs"), None);
    }
}
