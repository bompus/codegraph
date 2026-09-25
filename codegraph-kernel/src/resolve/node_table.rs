//! Row types and the per-run read-only node table the pool workers share.

use super::*;

// ---------------------------------------------------------------------------
// Row types (mirror src/types.ts Node + db/schema.sql bindings).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(super) struct KNode {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) qualified_name: String,
    pub(super) file_path: String,
    pub(super) language: String,
    pub(super) start_line: i64,
    pub(super) end_line: i64,
    pub(super) start_column: i64,
    pub(super) end_column: i64,
    pub(super) signature: Option<String>,
    pub(super) visibility: Option<String>,
    pub(super) is_exported: bool,
    pub(super) return_type: Option<String>,
    /// JSON `string[]` (queries.ts safeJsonParse) — None when absent/malformed.
    pub(super) type_parameters: Option<Vec<String>>,
    /// JSON `string[]` like type_parameters — Kotlin `expect`/`actual` etc.
    pub(super) decorators: Option<Vec<String>>,
}

impl KNode {
    /// A row selected with NODE_COLS, in that column order (positional reads:
    /// a by-name read scans the column list per column per row).
    pub(super) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let raw_tps: Option<String> = row.get(14)?;
        let raw_decs: Option<String> = row.get(15)?;
        Ok(KNode {
            id: row.get(0)?,
            kind: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            file_path: row.get(4)?,
            language: row.get(5)?,
            start_line: row.get(6)?,
            end_line: row.get(7)?,
            start_column: row.get(8)?,
            end_column: row.get(9)?,
            signature: row.get(10)?,
            visibility: row.get(11)?,
            is_exported: row.get::<_, i64>(12)? != 0,
            return_type: row.get(13)?,
            type_parameters: raw_tps.as_deref().and_then(parse_json_string_array),
            decorators: raw_decs.as_deref().and_then(parse_json_string_array),
        })
    }
}

/// Compiled patterns for the one pattern still built per reference — the
/// C++ declarator regex, whose greedy type-capture prefix has no split form
/// (`cpp_declarator_match`). Shared by every resolver in the process and
/// FIFO-bounded: a per-resolver map compiled the same pattern once per pool
/// worker and grew without bound (the receiver-type patterns, since split
/// into `Affix` forms, once put ~50k regexes, about 2 GB, in six workers).
/// An evicted pattern just recompiles.
pub(super) struct RegexCache {
    pub(super) map: HashMap<String, Arc<Regex>>,
    pub(super) order: VecDeque<String>,
}

pub(super) const REGEX_CACHE_CAP: usize = 4096;

pub(super) static REGEX_CACHE: LazyLock<Mutex<RegexCache>> =
    LazyLock::new(|| Mutex::new(RegexCache { map: HashMap::new(), order: VecDeque::new() }));

pub(super) fn shared_regex(pattern: &str) -> Result<Arc<Regex>> {
    if let Some(re) = REGEX_CACHE.lock().unwrap_or_else(|p| p.into_inner()).map.get(pattern) {
        return Ok(re.clone());
    }
    // Compiled outside the lock: a compile is the expensive part, and two
    // workers racing on one pattern just insert the same thing twice.
    let re = Arc::new(Regex::new(pattern).map_err(|e| Error::from_reason(e.to_string()))?);
    let mut cache = REGEX_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(existing) = cache.map.get(pattern) {
        return Ok(existing.clone());
    }
    if cache.map.len() >= REGEX_CACHE_CAP {
        if let Some(oldest) = cache.order.pop_front() {
            cache.map.remove(&oldest);
        }
    }
    cache.order.push_back(pattern.to_string());
    cache.map.insert(pattern.to_string(), re.clone());
    Ok(re)
}

pub(super) fn shared_regex_count() -> usize {
    REGEX_CACHE.lock().unwrap_or_else(|p| p.into_inner()).map.len()
}

/// A candidate list as one node query returns it — that query's ORDER BY,
/// as pointers into the run's table. Cloning it is one refcount.
pub(super) type NodeList = Arc<Vec<Arc<KNode>>>;

/// The `nodes` and `files` tables of one database, loaded once and indexed
/// the way the resolver queries them. Resolution never writes nodes, so the
/// table is immutable for the run. It replaces the per-resolver caches of
/// owned row copies (`name_cache`, `qname_cache`, `file_nodes`, `node_by_id`,
/// `lower_cache`, `known_names`, `known_files`) that each pool worker filled
/// separately — six workers held up to four copies of every touched node on
/// a large corpus, and every miss was a SQLite query.
///
/// The orders reproduce the TypeScript queries' ORDER BY exactly: SQLite
/// scans a `WHERE col = ?` through the column's index in rowid order and its
/// sorter is stable, so a stable sort of the rowid-ordered group by the
/// ORDER BY keys (BINARY collation, byte order) is the same sequence.
/// `CODEGRAPH_NODE_TABLE_VERIFY=1` re-runs every replaced query and reports
/// any sequence that differs.
pub(super) struct NodeTable {
    /// getNodesByName — ORDER BY file_path, start_line.
    pub(super) by_name: HashMap<String, NodeList>,
    /// getNodesByLowerName — `lower(name) = lower(?)`, rowid order. SQLite's
    /// built-in `lower()` folds ASCII only, as `to_ascii_lowercase` does.
    /// Built on first use: only the fuzzy matcher asks.
    pub(super) by_lower: std::sync::OnceLock<HashMap<String, NodeList>>,
    /// Every node in rowid order — the source for the lazy indexes.
    pub(super) nodes: Vec<Arc<KNode>>,
    /// getNodesByQualifiedName — rowid order.
    pub(super) by_qname: HashMap<String, NodeList>,
    /// getNodesInFile — ORDER BY start_line.
    pub(super) by_file: HashMap<String, NodeList>,
    /// getNodeById.
    pub(super) by_id: HashMap<String, Arc<KNode>>,
    /// `SELECT path FROM files` — knownFiles.
    pub(super) files: HashSet<String>,
    pub(super) empty: NodeList,
}

pub(super) fn push_group(map: &mut HashMap<String, Vec<Arc<KNode>>>, key: &str, n: &Arc<KNode>) {
    match map.get_mut(key) {
        Some(list) => list.push(n.clone()),
        None => {
            map.insert(key.to_string(), vec![n.clone()]);
        }
    }
}

pub(super) fn freeze(map: HashMap<String, Vec<Arc<KNode>>>) -> HashMap<String, NodeList> {
    map.into_iter().map(|(k, v)| (k, Arc::new(v))).collect()
}

impl NodeTable {
    pub(super) fn load(conn: &Connection) -> rusqlite::Result<NodeTable> {
        let t0 = std::time::Instant::now();
        let table = Self::load_inner(conn)?;
        if std::env::var_os("CODEGRAPH_KERNEL_STATS").is_some_and(|v| v == "1") {
            eprintln!(
                "[node-table] loaded {} nodes, {} names, {} files in {} ms",
                table.by_id.len(),
                table.by_name.len(),
                table.files.len(),
                t0.elapsed().as_millis()
            );
        }
        Ok(table)
    }

    pub(super) fn load_inner(conn: &Connection) -> rusqlite::Result<NodeTable> {
        let sql = format!("SELECT {NODE_COLS} FROM nodes ORDER BY rowid");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], KNode::from_row)?;
        let count: usize = conn.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))? as usize;
        let mut nodes: Vec<Arc<KNode>> = Vec::with_capacity(count);
        let mut by_name: HashMap<String, Vec<Arc<KNode>>> = HashMap::with_capacity(count / 2);
        let mut by_qname: HashMap<String, Vec<Arc<KNode>>> = HashMap::with_capacity(count / 2);
        let mut by_file: HashMap<String, Vec<Arc<KNode>>> = HashMap::with_capacity(count / 8);
        let mut by_id: HashMap<String, Arc<KNode>> = HashMap::with_capacity(count);
        for row in rows {
            let n = Arc::new(row?);
            push_group(&mut by_name, &n.name, &n);
            push_group(&mut by_qname, &n.qualified_name, &n);
            push_group(&mut by_file, &n.file_path, &n);
            by_id.insert(n.id.clone(), n.clone());
            nodes.push(n);
        }
        for list in by_name.values_mut() {
            list.sort_by(|a, b| {
                a.file_path
                    .as_bytes()
                    .cmp(b.file_path.as_bytes())
                    .then(a.start_line.cmp(&b.start_line))
            });
        }
        for list in by_file.values_mut() {
            list.sort_by_key(|n| n.start_line);
        }
        let mut stmt = conn.prepare("SELECT path FROM files")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut files = HashSet::new();
        for p in rows {
            files.insert(p?);
        }
        let table = NodeTable {
            by_name: freeze(by_name),
            by_lower: std::sync::OnceLock::new(),
            nodes,
            by_qname: freeze(by_qname),
            by_file: freeze(by_file),
            by_id,
            files,
            empty: Arc::new(Vec::new()),
        };
        if std::env::var_os("CODEGRAPH_NODE_TABLE_VERIFY").is_some_and(|v| v == "1") {
            let mismatches = table.verify(conn)?;
            eprintln!("[node-table] verify: {mismatches} mismatches");
        }
        Ok(table)
    }

    /// The lowercase-name index, built on first use.
    pub(super) fn by_lower(&self) -> &HashMap<String, NodeList> {
        self.by_lower.get_or_init(|| {
            let mut map: HashMap<String, Vec<Arc<KNode>>> = HashMap::with_capacity(self.by_name.len());
            for n in &self.nodes {
                if n.name.bytes().any(|b| b.is_ascii_uppercase()) {
                    push_group(&mut map, &n.name.to_ascii_lowercase(), n);
                } else {
                    push_group(&mut map, &n.name, n);
                }
            }
            freeze(map)
        })
    }

    /// Re-runs each replaced per-key query and counts id sequences that
    /// differ from the table's — the check behind the ordering argument
    /// above (a second pass over the database; the first five differences
    /// go to stderr).
    pub(super) fn verify(&self, conn: &Connection) -> rusqlite::Result<usize> {
        let checks: [(&str, &HashMap<String, NodeList>, &str); 4] = [
            ("name", &self.by_name, "SELECT id FROM nodes WHERE name = ?1 ORDER BY file_path, start_line"),
            ("lower", self.by_lower(), "SELECT id FROM nodes WHERE lower(name) = lower(?1)"),
            ("qname", &self.by_qname, "SELECT id FROM nodes WHERE qualified_name = ?1"),
            ("file", &self.by_file, "SELECT id FROM nodes WHERE file_path = ?1 ORDER BY start_line"),
        ];
        let mut mismatches = 0usize;
        for (label, map, sql) in checks {
            let mut stmt = conn.prepare(sql)?;
            for (key, list) in map {
                let rows = stmt.query_map([key], |r| r.get::<_, String>(0))?;
                let mut expected = Vec::with_capacity(list.len());
                for id in rows {
                    expected.push(id?);
                }
                let same = expected.len() == list.len()
                    && expected.iter().zip(list.iter()).all(|(e, n)| *e == n.id);
                if !same {
                    mismatches += 1;
                    if mismatches <= 5 {
                        eprintln!("[node-table] {label} {key:?}: table {:?} vs sqlite {expected:?}",
                            list.iter().map(|n| n.id.as_str()).collect::<Vec<_>>());
                    }
                }
            }
        }
        Ok(mismatches)
    }
}

/// Tables shared across the resolvers of one run, keyed by database path
/// and run generation. `Weak`, so a table lives exactly as long as some
/// resolver holds it — the pool's workers drop theirs at teardown, and the
/// next run's generation never matches a stale entry.
pub(super) static NODE_TABLES: OnceLock<Mutex<HashMap<String, Weak<NodeTable>>>> = OnceLock::new();

pub(super) fn node_table_for(db_path: &str, generation: Option<&str>, conn: &Connection) -> Result<Arc<NodeTable>> {
    let load = || NodeTable::load(conn).map_err(|e| Error::from_reason(format!("node table: {e}")));
    let Some(generation) = generation else {
        return Ok(Arc::new(load()?));
    };
    let key = format!("{db_path}\0{generation}");
    let registry = NODE_TABLES.get_or_init(Default::default);
    // Held across the load, so concurrent workers wait for one table instead
    // of each building their own.
    let mut tables = registry.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(table) = tables.get(&key).and_then(Weak::upgrade) {
        return Ok(table);
    }
    let table = Arc::new(load()?);
    tables.retain(|_, weak| weak.strong_count() > 0);
    tables.insert(key, Arc::downgrade(&table));
    Ok(table)
}

/// safeJsonParse for the `["a","b"]` shape type_parameters is stored as —
/// returns None on anything that is not a flat array of JSON strings.
pub(super) fn parse_json_string_array(raw: &str) -> Option<Vec<String>> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut rest = inner;
    loop {
        rest = rest.trim_start();
        let s = rest.strip_prefix('"')?;
        let mut val = String::new();
        let mut chars = s.char_indices();
        let mut escaped = false;
        let mut end = 0usize;
        let mut closed = false;
        while let Some((i, c)) = chars.next() {
            if escaped {
                val.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let (_, h) = chars.next()?;
                            code = code * 16 + h.to_digit(16)?;
                        }
                        char::from_u32(code)?
                    }
                    other => other,
                });
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = i;
                closed = true;
                break;
            } else {
                val.push(c);
            }
        }
        if !closed || escaped {
            return None;
        }
        rest = &s[end + 1..];
        out.push(val);
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        rest = rest.strip_prefix(',')?;
    }
    Some(out)
}

#[derive(Clone, Debug)]
pub(super) struct KBinding {
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) node_id: Option<String>,
    pub(super) target_spec: Option<String>,
    pub(super) target_name: Option<String>,
    pub(super) exported_as: Option<String>,
    pub(super) scope_start: i64,
    pub(super) scope_end: i64,
    pub(super) storage: Option<String>,
    pub(super) line: i64,
}

/// ImportMapping (import-resolver.ts).
#[derive(Clone, Debug)]
pub(super) struct KImport {
    pub(super) local_name: String,
    pub(super) exported_name: String,
    pub(super) source: String,
    pub(super) is_default: bool,
    pub(super) is_namespace: bool,
}

/// ReExport (import-resolver.ts).
#[derive(Clone, Debug)]
pub(super) struct KReExport {
    pub(super) kind: &'static str, // 'named' | 'wildcard'
    pub(super) exported_name: Option<String>,
    pub(super) original_name: Option<String>,
    pub(super) source: String,
}

/// FileExportIndex (import-resolver.ts): per-file export lookup —
/// first-wins `by_name` over `isExported` nodes plus binding-row aliases,
/// and the three default-export guesses. (The TS `declared` map is dead
/// code there and not ported.)
#[derive(Debug)]
pub(super) struct FileExportIndexK {
    pub(super) by_name: HashMap<String, Arc<KNode>>,
    pub(super) default_component: Option<Arc<KNode>>,
    pub(super) default_fn_class: Option<Arc<KNode>>,
    pub(super) default_binding: Option<Arc<KNode>>,
}

/// WorkspacePackages (workspace-packages.ts).
#[derive(Clone, Debug, Default)]
pub(super) struct WorkspaceK {
    pub(super) source_entries: Vec<(String, String)>,
    pub(super) by_name: HashMap<String, String>,
    pub(super) entry_by_name: Option<HashMap<String, String>>,
    pub(super) local_link_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Query layer — every statement reproduces the TypeScript ORDER BY verbatim
// (src/db/queries.ts): candidate order is observable through findBestMatch's
// first-max scoring.
// ---------------------------------------------------------------------------

pub(super) const NODE_COLS: &str = "id, kind, name, qualified_name, file_path, language, \
                         start_line, end_line, start_column, end_column, \
                         signature, visibility, is_exported, return_type, \
                         type_parameters, decorators";
