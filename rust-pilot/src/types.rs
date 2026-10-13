use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cell::RefCell;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
    pub start_column: i64,
    pub end_column: i64,
    pub language: String,
    pub updated_at: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl Node {
    pub fn flag(&self, name: &str) -> bool {
        self.extra
            .get(name)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedRef {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
    pub line: i64,
    pub column: i64,
    pub file_path: String,
    pub language: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRef {
    pub original: UnresolvedRef,
    pub target_node_id: String,
    pub confidence: f64,
    pub resolved_by: &'static str,
}
pub trait ResolutionContext {
    fn all_files(&self) -> Vec<&str>;
    fn read_file(&self, path: &str) -> Option<&str>;
    fn nodes_by_name(&self, name: &str) -> Vec<&Node>;
    fn nodes_in_file(&self, path: &str) -> Vec<&Node>;
    fn now(&self) -> i64;
    fn dependency_cache(&self) -> &RefCell<Option<(usize, IndexSet<String>)>>;
    fn file_exists(&self, path: &str) -> bool {
        self.read_file(path).is_some()
    }
    fn file_contains(&self, path: &str, needle: &str) -> bool {
        self.read_file(path).is_some_and(|s| s.contains(needle))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryContext {
    pub files: IndexMap<String, String>,
    #[serde(default)]
    pub manifests: IndexMap<String, String>,
    pub nodes: Vec<Node>,
    pub refs: Vec<UnresolvedRef>,
    pub now: i64,
    #[serde(skip)]
    pub dependency_cache: RefCell<Option<(usize, IndexSet<String>)>>,
}
impl ResolutionContext for MemoryContext {
    fn all_files(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }
    fn read_file(&self, path: &str) -> Option<&str> {
        self.files
            .get(path)
            .or_else(|| self.manifests.get(path))
            .map(String::as_str)
    }
    fn nodes_by_name(&self, name: &str) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.name == name).collect()
    }
    fn nodes_in_file(&self, path: &str) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.file_path == path).collect()
    }
    fn dependency_cache(&self) -> &RefCell<Option<(usize, IndexSet<String>)>> {
        &self.dependency_cache
    }
    fn now(&self) -> i64 {
        self.now
    }
}
