//! Query layer — every statement reproduces the TypeScript ORDER BY verbatim (src/db/queries.ts).

use super::*;

impl KernelResolver {
    /// The live connection — errors once close() has run. Statements borrow
    /// it, so the Option indirection stays inside this accessor.
    pub(super) fn conn(&self) -> Result<&Connection> {
        self.conn
            .as_ref()
            .ok_or_else(|| Error::from_reason("KernelResolver is closed"))
    }

    pub(super) fn table(&self) -> Result<&NodeTable> {
        if let Some(t) = self.table.get() {
            return Ok(t);
        }
        let table = node_table_for(&self.db_path, self.generation.as_deref(), self.conn()?)?;
        Ok(self.table.get_or_init(|| table))
    }

    /// queries.getNodesByName — ORDER BY file_path, start_line.
    pub(super) fn nodes_by_name(&self, name: &str) -> Result<NodeList> {
        let t = self.table()?;
        Ok(t.by_name.get(name).cloned().unwrap_or_else(|| t.empty.clone()))
    }

    /// queries.getNodesByLowerName — `WHERE lower(name) = lower(?)`, no
    /// ORDER BY (rowid order, same as the TS reader).
    pub(super) fn nodes_by_lower_name(&self, name: &str) -> Result<NodeList> {
        let t = self.table()?;
        Ok(t.by_lower().get(&name.to_ascii_lowercase()).cloned().unwrap_or_else(|| t.empty.clone()))
    }

    /// queries.getNodesByQualifiedName — no ORDER BY (TS uses rowid order).
    pub(super) fn nodes_by_qualified_name(&self, qname: &str) -> Result<NodeList> {
        let t = self.table()?;
        Ok(t.by_qname.get(qname).cloned().unwrap_or_else(|| t.empty.clone()))
    }

    /// queries.getNodesInFile — ORDER BY start_line.
    pub(super) fn nodes_in_file(&self, file_path: &str) -> Result<NodeList> {
        let t = self.table()?;
        Ok(t.by_file.get(file_path).cloned().unwrap_or_else(|| t.empty.clone()))
    }

    /// getNodeById over an optional id (a binding row's `node_id`).
    pub(super) fn node_by_opt_id(&self, id: Option<&str>) -> Result<Option<Arc<KNode>>> {
        match id {
            Some(id) => self.node_by_id(id),
            None => Ok(None),
        }
    }

    /// queries.getNodeById.
    pub(super) fn node_by_id(&self, id: &str) -> Result<Option<Arc<KNode>>> {
        Ok(self.table()?.by_id.get(id).cloned())
    }

    /// knownSymbols membership (`SELECT DISTINCT name FROM nodes`); false
    /// once closed.
    pub(super) fn known_name(&self, name: &str) -> bool {
        self.table().is_ok_and(|t| t.by_name.contains_key(name))
    }

    /// knownFiles membership (`SELECT path FROM files`); false once closed.
    pub(super) fn known_file(&self, path: &str) -> bool {
        self.table().is_ok_and(|t| t.files.contains(path))
    }

    /// queries.getBindings — ORDER BY rowid.
    pub(super) fn bindings(&mut self, file_path: &str) -> Result<Rc<Vec<KBinding>>> {
        if let Some(v) = self.bindings_cache.get(file_path) {
            return Ok(v.clone());
        }
        let mut stmt = self
            .conn()?
            .prepare(
                "SELECT file_path, name, kind, node_id, target_spec, target_name, \
                 exported_as, scope_start, scope_end, storage, line \
                 FROM bindings WHERE file_path = ?1 ORDER BY rowid",
            )
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let rows = stmt
            .query_map([file_path], |row| {
                Ok(KBinding {
                    name: row.get("name")?,
                    kind: row.get("kind")?,
                    node_id: row.get("node_id")?,
                    target_spec: row.get("target_spec")?,
                    target_name: row.get("target_name")?,
                    exported_as: row.get("exported_as")?,
                    scope_start: row.get("scope_start")?,
                    scope_end: row.get("scope_end")?,
                    storage: row.get("storage")?,
                    line: row.get("line")?,
                })
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut v = Vec::new();
        for b in rows {
            v.push(b.map_err(|e| Error::from_reason(e.to_string()))?);
        }
        drop(stmt); // release the self.conn borrow before the cache write
        let v = Rc::new(v);
        self.bindings_cache.insert(file_path.to_string(), v.clone());
        Ok(v)
    }

    /// context.fileExists — the knownFiles set answers for the exact and the
    /// `\`-normalized spelling, then lexical containment + existsSync
    /// fallback for files not yet indexed (queries' per-candidate hot path).
    pub(super) fn file_exists(&self, rel: &str) -> bool {
        let normalized = rel.replace('\\', "/");
        if self.known_file(rel) || self.known_file(normalized.as_str()) {
            return true;
        }
        lexical_path_within_root(&self.root_abs, rel)
            && std::fs::metadata(pos_resolve(&self.root_abs, rel)).is_ok()
    }

    /// context.getFileLines — `readFile` content split on /\r?\n/,
    /// LRU-cached (nulls cached too).
    pub(super) fn read_file(&mut self, rel: &str) -> Option<Rc<Vec<String>>> {
        if let Some(v) = self.file_cache.get(rel) {
            return v.clone();
        }
        let v = std::fs::read_to_string(pos_resolve(&self.root_abs, rel))
            .ok()
            .map(|s| {
                let normalized = s.replace("\r\n", "\n");
                Rc::new(normalized.split('\n').map(|l| l.to_string()).collect::<Vec<String>>())
            });
        self.file_cache.put(rel.to_string(), v.clone());
        v
    }

    /// A name-parameterized regex from the bounded process-wide cache. Only
    /// the C++ declarator pattern still needs one (its greedy type-capture
    /// prefix has no `Affix` split); every other per-name pattern is an Affix.
    pub(super) fn cached_regex(&mut self, pattern: &str) -> Result<Arc<Regex>> {
        shared_regex(pattern)
    }

    // -----------------------------------------------------------------------
    // Bindings → import mappings / re-exports (import-resolver.ts)
    // -----------------------------------------------------------------------

    /// importMappingsFromBindings. Returns None when the file has no binding
    /// rows — same as TS, where a null result falls through to
    /// extractImportMappings, which now always yields `[]`.
    pub(super) fn import_mappings_from_bindings(&self, rows: &[KBinding]) -> Option<Vec<KImport>> {
        if rows.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for r in rows {
            if r.kind != "import" || r.target_spec.is_none() {
                continue;
            }
            let exported_name = r.target_name.clone().unwrap_or_else(|| r.name.clone());
            out.push(KImport {
                local_name: r.name.clone(),
                is_default: exported_name == "default",
                is_namespace: exported_name == "*",
                exported_name,
                source: r.target_spec.clone().unwrap(),
            });
        }
        Some(out)
    }

    /// getImportMappings equivalent — bindings-backed, no source fallback
    /// needed (extractImportMappings is a stub returning []).
    pub(super) fn import_mappings(&mut self, file_path: &str) -> Result<Rc<Vec<KImport>>> {
        if let Some(v) = self.import_map_cache.get(file_path) {
            return Ok(v.clone());
        }
        let rows = self.bindings(file_path)?;
        let v = Rc::new(self.import_mappings_from_bindings(&rows).unwrap_or_default());
        self.import_map_cache.insert(file_path.to_string(), v.clone());
        Ok(v)
    }

    /// reExportsFromBindings.
    pub(super) fn reexports(&mut self, file_path: &str) -> Result<Rc<Vec<KReExport>>> {
        if let Some(v) = self.reexport_cache.get(file_path) {
            return Ok(v.clone());
        }
        let rows = self.bindings(file_path)?;
        let mut out = Vec::new();
        for r in rows.iter() {
            if r.kind != "reexport" || r.target_spec.is_none() {
                continue;
            }
            if r.name == "*" {
                out.push(KReExport {
                    kind: "wildcard",
                    exported_name: None,
                    original_name: None,
                    source: r.target_spec.clone().unwrap(),
                });
            } else {
                out.push(KReExport {
                    kind: "named",
                    exported_name: Some(r.exported_as.clone().unwrap_or_else(|| r.name.clone())),
                    original_name: Some(r.name.clone()),
                    source: r.target_spec.clone().unwrap(),
                });
            }
        }
        let v = Rc::new(out);
        self.reexport_cache.insert(file_path.to_string(), v.clone());
        Ok(v)
    }

    /// defaultExportBinding (import-resolver.ts): the identifier
    /// `export default NAME` names, from the file's binding rows.
    pub(super) fn default_export_binding(rows: &[KBinding]) -> Option<String> {
        rows.iter()
            .find(|r| r.exported_as.as_deref() == Some("default") && r.node_id.is_some())
            .map(|r| r.name.clone())
    }

    /// getFileExportIndex (import-resolver.ts).
    pub(super) fn file_export_index(&mut self, file_path: &str) -> Result<Rc<FileExportIndexK>> {
        if let Some(Some(v)) = self.export_index.get(file_path) {
            return Ok(v.clone());
        }
        let nodes = self.nodes_in_file(file_path)?;
        let mut by_name: HashMap<String, Arc<KNode>> = HashMap::new();
        let mut default_component: Option<Arc<KNode>> = None;
        let mut default_fn_class: Option<Arc<KNode>> = None;
        for n in nodes.iter() {
            if !n.is_exported {
                continue;
            }
            by_name.entry(n.name.clone()).or_insert_with(|| n.clone());
            if default_component.is_none() && n.kind == "component" {
                default_component = Some(n.clone());
            }
            if default_fn_class.is_none() && (n.kind == "function" || n.kind == "class") {
                default_fn_class = Some(n.clone());
            }
        }
        let rows = self.bindings(file_path)?;
        let mut default_binding: Option<Arc<KNode>> = None;
        if let Some(bound) = Self::default_export_binding(&rows) {
            let mut candidates: Vec<&Arc<KNode>> = nodes
                .iter()
                .filter(|n| n.name == bound && is_default_binding_kind(&n.kind))
                .collect();
            candidates.sort_by(|a, b| {
                a.start_line
                    .cmp(&b.start_line)
                    .then(a.start_column.cmp(&b.start_column))
            });
            default_binding = candidates.first().map(|n| (*n).clone());
        }
        // Local export clauses: `export { impl as alias }` binds the renamed
        // name to the real declaration.
        if !rows.is_empty() {
            let by_id: HashMap<&str, &Arc<KNode>> =
                nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            for r in rows.iter() {
                let (Some(exported), Some(node_id)) = (r.exported_as.as_deref(), r.node_id.as_deref())
                else {
                    continue;
                };
                if by_name.contains_key(exported) {
                    continue;
                }
                if let Some(decl) = by_id.get(node_id) {
                    by_name.insert(exported.to_string(), (*decl).clone());
                }
            }
        }
        let idx = Rc::new(FileExportIndexK {
            by_name,
            default_component,
            default_fn_class,
            default_binding,
        });
        self.export_index.insert(file_path.to_string(), Some(idx.clone()));
        Ok(idx)
    }

    /// findExportedSymbol (import-resolver.ts) — bounded, cycle-safe export
    /// walk over direct exports, named re-exports and wildcard barrels.
    /// Top-level (fresh-visited) results are memoized like the TS WeakMap.
    pub(super) fn find_exported_symbol(
        &mut self,
        file_path: &str,
        want: &ExportWant,
        language: &str,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> Result<Option<Arc<KNode>>> {
        const REEXPORT_MAX_DEPTH: usize = 8;
        if depth > REEXPORT_MAX_DEPTH {
            return Ok(None);
        }
        let fresh = depth == 0 && visited.is_empty();
        let memo_key = if fresh {
            Some(format!(
                "{}\0{}{}\0{}\0{}\0{}",
                file_path,
                if want.is_default { 1 } else { 0 },
                if want.is_namespace { 1 } else { 0 },
                want.exported_name,
                want.member_name.as_deref().unwrap_or(""),
                language
            ))
        } else {
            None
        };
        if let Some(k) = &memo_key {
            if let Some(hit) = self.exported_symbol_memo.get(k) {
                return Ok(hit.clone());
            }
        }
        if visited.contains(file_path) {
            return Ok(None);
        }
        visited.insert(file_path.to_string());

        let export_index = self.file_export_index(file_path)?;
        // 1. Direct hit.
        if want.is_default {
            let direct = export_index
                .default_component
                .clone()
                .or_else(|| export_index.default_binding.clone())
                .or_else(|| export_index.default_fn_class.clone());
            if let Some(d) = direct {
                return self.memo_symbol_opt(memo_key, Some(d.clone()));
            }
        } else if want.is_namespace && want.member_name.is_some() {
            if let Some(d) = export_index.by_name.get(want.member_name.as_deref().unwrap()) {
                return self.memo_symbol_opt(memo_key, Some(d.clone()));
            }
        } else if let Some(d) = export_index.by_name.get(&want.exported_name) {
            return self.memo_symbol_opt(memo_key, Some(d.clone()));
        }

        // Python module-level imports are public bindings — same walk.
        if language == "python" {
            let name = if want.is_namespace {
                want.member_name.clone()
            } else {
                Some(want.exported_name.clone())
            };
            if let Some(name) = name {
                let rows = self.bindings(file_path)?;
                let file_nodes = self.nodes_in_file(file_path)?;
                let matching: Vec<&KBinding> = rows
                    .iter()
                    .filter(|row| {
                        row.name == name
                            && row.scope_start == 1
                            && !file_nodes.iter().any(|node| {
                                matches!(node.kind.as_str(), "function" | "method" | "class")
                                    && node.start_line <= row.line
                                    && node.end_line >= row.line
                            })
                    })
                    .collect();
                if matching.len() == 1 {
                    let binding = matching[0];
                    if binding.kind == "import"
                        && binding.target_spec.is_some()
                        && binding.target_name.as_deref() != Some("*")
                    {
                        let spec = binding.target_spec.clone().unwrap();
                        let mut next = self.resolve_import_path(&spec, file_path, language)?;
                        if next.is_none() {
                            next = self
                                .find_python_module_file(&spec, file_path)?
                                .map(|n| n.file_path.clone());
                        }
                        if let Some(next) = next {
                            let chained = self.find_exported_symbol(
                                &next,
                                &ExportWant {
                                    is_default: false,
                                    is_namespace: false,
                                    exported_name: binding
                                        .target_name
                                        .clone()
                                        .unwrap_or_else(|| binding.name.clone()),
                                    member_name: None,
                                },
                                language,
                                visited,
                                depth + 1,
                            )?;
                            if chained.is_some() {
                                return self.memo_symbol_opt(memo_key, chained);
                            }
                        }
                    }
                }
            }
        }

        // 2. Named re-exports.
        let reexports = self.reexports(file_path)?;
        if reexports.is_empty() {
            return self.memo_symbol_opt(memo_key, None);
        }
        let target_name = if want.is_default {
            "default".to_string()
        } else {
            want.exported_name.clone()
        };
        for rex in reexports.iter() {
            if rex.kind == "named" && rex.exported_name.as_deref() == Some(target_name.as_str()) {
                let next = self.resolve_import_path(&rex.source, file_path, language)?;
                let Some(next) = next else { continue };
                let chained = self.find_exported_symbol(
                    &next,
                    &ExportWant {
                        is_default: rex.original_name.as_deref() == Some("default"),
                        is_namespace: false,
                        exported_name: rex.original_name.clone().unwrap_or_default(),
                        member_name: None,
                    },
                    language,
                    visited,
                    depth + 1,
                )?;
                if chained.is_some() {
                    return self.memo_symbol_opt(memo_key, chained);
                }
            }
        }
        // 3. Wildcard re-exports.
        for rex in reexports.iter() {
            if rex.kind == "wildcard" {
                let next = self.resolve_import_path(&rex.source, file_path, language)?;
                let Some(next) = next else { continue };
                let chained =
                    self.find_exported_symbol(&next, want, language, visited, depth + 1)?;
                if chained.is_some() {
                    return self.memo_symbol_opt(memo_key, chained);
                }
            }
        }
        self.memo_symbol_opt(memo_key, None)
    }

    pub(super) fn memo_symbol_opt(
        &mut self,
        key: Option<String>,
        v: Option<Arc<KNode>>,
    ) -> Result<Option<Arc<KNode>>> {
        if let Some(k) = key {
            self.exported_symbol_memo.insert(k, v.clone());
        }
        Ok(v)
    }
}
