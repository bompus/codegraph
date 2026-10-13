//! Read-only translations of the QueryBuilder methods used by dead-code.ts.
use crate::types::Node;
use indexmap::{IndexMap, IndexSet};
use rusqlite::{Connection, OpenFlags, Result, Row, params_from_iter};
use serde_json::{Map, Value, json};
use std::{cell::RefCell, path::Path, time::Duration};

pub type Set = IndexSet<String>;
pub type Names = IndexMap<String, Vec<String>>;
#[derive(Clone)]
pub struct Candidate {
    pub node: Node,
    pub generated: bool,
}
pub struct Edge {
    pub source: String,
    pub target: String,
}
pub struct Store {
    conn: Connection,
    cache: RefCell<IndexMap<String, Node>>,
}
pub fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values.into_iter().collect::<Set>().into_iter().collect()
}
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}
fn node_row(row: &Row<'_>) -> Result<Node> {
    let mut extra = Map::new();
    for (db, key) in [
        ("docstring", "docstring"),
        ("signature", "signature"),
        ("return_type", "returnType"),
    ] {
        if let Some(v) = row.get::<_, Option<String>>(db)? {
            extra.insert(key.into(), json!(v));
        }
    }
    extra.insert(
        "visibility".into(),
        json!(row.get::<_, Option<String>>("visibility")?),
    );
    for (db, key) in [
        ("is_exported", "isExported"),
        ("is_async", "isAsync"),
        ("is_static", "isStatic"),
        ("is_abstract", "isAbstract"),
    ] {
        extra.insert(key.into(), json!(row.get::<_, Option<i64>>(db)? == Some(1)));
    }
    for (db, key) in [
        ("decorators", "decorators"),
        ("type_parameters", "typeParameters"),
    ] {
        if let Some(v) = row.get::<_, Option<String>>(db)?
            && !v.is_empty()
            && let Ok(v) = serde_json::from_str::<Value>(&v)
        {
            extra.insert(key.into(), v);
        }
    }
    Ok(Node {
        id: row.get("id")?,
        kind: row.get("kind")?,
        name: row.get("name")?,
        qualified_name: row.get("qualified_name")?,
        file_path: row.get("file_path")?,
        language: row.get("language")?,
        start_line: row.get("start_line")?,
        end_line: row.get("end_line")?,
        start_column: row.get("start_column")?,
        end_column: row.get("end_column")?,
        updated_at: row.get("updated_at")?,
        extra,
    })
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-64000; PRAGMA temp_store=MEMORY; PRAGMA mmap_size=268435456;")?;
        Ok(Self {
            conn,
            cache: RefCell::new(IndexMap::new()),
        })
    }
    pub fn unreferenced(&self, kinds: &[String], limit: usize) -> Result<Vec<Candidate>> {
        if kinds.is_empty() || limit == 0 {
            return Ok(vec![]);
        }
        let sql = format!("SELECT n.*, COALESCE(f.generated, 0) AS file_generated FROM nodes n
            LEFT JOIN files f ON f.path = n.file_path WHERE n.kind IN ({})
            AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.target = n.id AND e.kind != 'contains' AND e.source != n.id)
            ORDER BY n.file_path, n.start_line, n.name LIMIT ?", placeholders(kinds.len()));
        let mut params: Vec<Value> = kinds.iter().map(|s| json!(s)).collect();
        params.push(json!(limit));
        let params: Vec<rusqlite::types::Value> = params
            .into_iter()
            .map(|v| match v {
                Value::String(s) => rusqlite::types::Value::Text(s),
                Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap()),
                _ => unreachable!(),
            })
            .collect();
        self.conn
            .prepare(&sql)?
            .query_map(params_from_iter(params), |row| {
                Ok(Candidate {
                    node: node_row(row)?,
                    generated: row.get::<_, i64>("file_generated")? == 1,
                })
            })?
            .collect()
    }
    pub fn nodes(&self, ids: &[String]) -> Result<IndexMap<String, Node>> {
        let mut out = IndexMap::new();
        let mut misses = vec![];
        let mut cache = self.cache.borrow_mut();
        for id in ids {
            if let Some(node) = cache.shift_remove(id) {
                out.insert(id.clone(), node.clone());
                cache.insert(id.clone(), node);
            } else {
                misses.push(id.clone());
            }
        }
        for chunk in misses.chunks(500) {
            let sql = format!(
                "SELECT * FROM nodes WHERE id IN ({})",
                placeholders(chunk.len())
            );
            for row in self
                .conn
                .prepare(&sql)?
                .query_map(params_from_iter(chunk), node_row)?
            {
                let node = row?;
                out.insert(node.id.clone(), node.clone());
                if cache.len() >= 1000 {
                    cache.shift_remove_index(0);
                }
                cache.insert(node.id.clone(), node);
            }
        }
        Ok(out)
    }
    pub fn edges(&self, ids: &[String], kinds: &[&str], incoming: bool) -> Result<Vec<Edge>> {
        let mut out = vec![];
        for chunk in unique(ids.iter().cloned()).chunks(500) {
            let column = if incoming { "target" } else { "source" };
            let mut sql = format!(
                "SELECT * FROM edges WHERE {column} IN ({})",
                placeholders(chunk.len())
            );
            let mut params = chunk.to_vec();
            if !kinds.is_empty() {
                sql += &format!(" AND kind IN ({})", placeholders(kinds.len()));
                params.extend(kinds.iter().map(|s| s.to_string()));
            }
            out.extend(
                self.conn
                    .prepare(&sql)?
                    .query_map(params_from_iter(params), |row| {
                        Ok(Edge {
                            source: row.get("source")?,
                            target: row.get("target")?,
                        })
                    })?
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        Ok(out)
    }
    pub fn unresolved_from(&self, ids: &[String], kinds: &[&str]) -> Result<Names> {
        let mut out: Names = IndexMap::new();
        if kinds.is_empty() {
            return Ok(out);
        }
        for chunk in unique(ids.iter().cloned()).chunks(500) {
            let sql = format!(
                "SELECT DISTINCT from_node_id AS id, reference_name AS name FROM unresolved_refs WHERE from_node_id IN ({}) AND reference_kind IN ({})",
                placeholders(chunk.len()),
                placeholders(kinds.len())
            );
            let params = chunk
                .iter()
                .map(String::as_str)
                .chain(kinds.iter().copied());
            for row in self
                .conn
                .prepare(&sql)?
                .query_map(params_from_iter(params), |row| {
                    Ok((row.get::<_, String>("id")?, row.get::<_, String>("name")?))
                })?
            {
                let (id, name) = row?;
                out.entry(id).or_default().push(name);
            }
        }
        Ok(out)
    }
    pub fn unresolved_sources(
        &self,
        names: &[String],
        kinds: &[&str],
    ) -> Result<IndexMap<String, Set>> {
        let mut out: IndexMap<String, Set> = IndexMap::new();
        if kinds.is_empty() {
            return Ok(out);
        }
        for chunk in unique(names.iter().filter(|s| !s.is_empty()).cloned()).chunks(500) {
            let sql = format!(
                "SELECT DISTINCT reference_name AS name, from_node_id AS id FROM unresolved_refs WHERE reference_name IN ({}) AND reference_kind IN ({})",
                placeholders(chunk.len()),
                placeholders(kinds.len())
            );
            let params = chunk
                .iter()
                .map(String::as_str)
                .chain(kinds.iter().copied());
            for row in self
                .conn
                .prepare(&sql)?
                .query_map(params_from_iter(params), |row| {
                    Ok((row.get::<_, String>("name")?, row.get::<_, String>("id")?))
                })?
            {
                let (name, id) = row?;
                out.entry(name).or_default().insert(id);
            }
        }
        Ok(out)
    }
    pub fn unresolved_names(&self, names: &[String]) -> Result<Set> {
        let mut out = Set::new();
        for chunk in unique(names.iter().filter(|s| !s.is_empty()).cloned()).chunks(500) {
            let marks = placeholders(chunk.len());
            let sql = format!(
                "SELECT DISTINCT reference_name AS name FROM unresolved_refs WHERE reference_name IN ({marks}) UNION SELECT DISTINCT name_tail AS name FROM unresolved_refs WHERE name_tail IN ({marks})"
            );
            out.extend(
                self.conn
                    .prepare(&sql)?
                    .query_map(params_from_iter(chunk.iter().chain(chunk)), |row| {
                        row.get::<_, String>("name")
                    })?
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        Ok(out)
    }
    pub fn ambiguous_names(&self, names: &[String]) -> Result<Set> {
        let mut out = Set::new();
        for chunk in unique(names.iter().filter(|s| !s.is_empty()).cloned()).chunks(500) {
            let sql = format!(
                "SELECT name FROM (SELECT n.name AS name, EXISTS (SELECT 1 FROM edges e WHERE e.target = n.id AND e.kind != 'contains') AS referenced FROM nodes n WHERE n.name IN ({})) GROUP BY name HAVING COUNT(*) > 1 AND SUM(referenced) > 0",
                placeholders(chunk.len())
            );
            out.extend(
                self.conn
                    .prepare(&sql)?
                    .query_map(params_from_iter(chunk), |row| row.get::<_, String>("name"))?
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        Ok(out)
    }
    pub fn languages_with_exports(&self, languages: &[String]) -> Result<Set> {
        let mut out = Set::new();
        for chunk in unique(languages.iter().filter(|s| !s.is_empty()).cloned()).chunks(500) {
            let sql = format!(
                "SELECT language, MAX(is_exported) AS any_exported FROM nodes WHERE language IN ({}) GROUP BY language",
                placeholders(chunk.len())
            );
            for row in self
                .conn
                .prepare(&sql)?
                .query_map(params_from_iter(chunk), |row| {
                    Ok((
                        row.get::<_, String>("language")?,
                        row.get::<_, Option<i64>>("any_exported")?,
                    ))
                })?
            {
                let (language, exported) = row?;
                if exported == Some(1) {
                    out.insert(language);
                }
            }
        }
        Ok(out)
    }
    pub fn dependent_counts(&self, files: &[String]) -> Result<IndexMap<String, i64>> {
        let sql = "SELECT tn.file_path AS filePath, COUNT(DISTINCT sn.file_path) AS dependents FROM edges e JOIN nodes tn ON tn.id = e.target JOIN nodes sn ON sn.id = e.source WHERE e.kind != 'contains' AND tn.file_path IN (SELECT value FROM json_each(?)) AND sn.file_path <> tn.file_path GROUP BY tn.file_path";
        self.conn
            .prepare(sql)?
            .query_map([serde_json::to_string(files).unwrap()], |row| {
                Ok((row.get("filePath")?, row.get("dependents")?))
            })?
            .collect()
    }
    pub fn dependents(&self, file: &str) -> Result<Vec<String>> {
        let sql = "SELECT DISTINCT src.file_path AS fp FROM edges e JOIN nodes tgt ON tgt.id = e.target JOIN nodes src ON src.id = e.source WHERE tgt.file_path = ? AND e.kind != 'contains' AND src.file_path != ?";
        self.conn
            .prepare(sql)?
            .query_map([file, file], |row| row.get("fp"))?
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn store_connection_cannot_write() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("readonly-{}.db", std::process::id()));
        let seed = Connection::open(&path).unwrap();
        seed.execute_batch(
            "CREATE TABLE sentinel (value INTEGER); INSERT INTO sentinel VALUES (1)",
        )
        .unwrap();
        drop(seed);
        let store = Store::open(&path).unwrap();
        let error = store
            .conn
            .execute("UPDATE sentinel SET value=2", [])
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
        assert_eq!(
            store
                .conn
                .query_row("SELECT value FROM sentinel", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
