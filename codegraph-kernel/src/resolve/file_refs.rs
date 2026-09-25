//! File-path references (name-matcher.ts matchByFilePath): a ref naming a file, and a symbol or markdown section inside it.

use super::*;

impl KernelResolver {
    /// matchByFilePath (name-matcher.ts): path-shaped (`a/b.h`) or
    /// extension-bearing bare (`Foo.h`) names → `file` nodes.
    pub(super) fn match_by_file_path(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
        let normalized = r.reference_name.replace('\\', "/");
        let (path_and_symbol, anchor) = split_anchor(&normalized);
        let (path_wo_anchor, symbol_name) = split_file_symbol(path_and_symbol);
        if !path_wo_anchor.contains('/') && !thread_regex(&FILE_PATH_EXT_RE).is_match(path_wo_anchor) {
            return Ok(None);
        }
        let file_name = pos_basename(path_wo_anchor);
        if file_name.is_empty() {
            return Ok(None);
        }
        let file_nodes: Vec<Arc<KNode>> = self
            .nodes_by_name(file_name)?
            .iter()
            .filter(|n| n.kind == "file")
            .cloned()
            .collect();
        if file_nodes.is_empty() {
            return Ok(None);
        }
        if let Some(symbol_name) = symbol_name.filter(|s| !s.is_empty()) {
            if let Some(symbol) =
                self.find_symbol_in_referenced_file(path_wo_anchor, symbol_name, &file_nodes)?
            {
                return Ok(Some(KCand {
                    node: symbol,
                    confidence: 0.99,
                    resolved_by: "file-path",
                }));
            }
        }
        if let Some(anchor) = anchor.filter(|a| !a.is_empty()) {
            if let Some(anchored) =
                self.find_anchored_markdown_section(path_wo_anchor, anchor, &file_nodes)?
            {
                return Ok(Some(KCand {
                    node: anchored,
                    confidence: 0.98,
                    resolved_by: "file-path",
                }));
            }
        }
        if let Some(exact) = file_nodes
            .iter()
            .find(|n| n.qualified_name == path_wo_anchor || n.file_path == path_wo_anchor)
        {
            return Ok(Some(KCand {
                node: exact.clone(),
                confidence: 0.95,
                resolved_by: "file-path",
            }));
        }
        let suffix_matches: Vec<Arc<KNode>> = file_nodes
            .iter()
            .filter(|n| {
                n.qualified_name.ends_with(path_wo_anchor) || n.file_path.ends_with(path_wo_anchor)
            })
            .cloned()
            .collect();
        if !suffix_matches.is_empty() {
            return Ok(Some(KCand {
                node: pick_closest_file_node(&suffix_matches, r),
                confidence: 0.85,
                resolved_by: "file-path",
            }));
        }
        if file_nodes.len() == 1 {
            return Ok(Some(KCand {
                node: file_nodes[0].clone(),
                confidence: 0.7,
                resolved_by: "file-path",
            }));
        }
        Ok(None)
    }

    /// findSymbolInReferencedFile (name-matcher.ts): `path::symbol` — search
    /// the path-matched files (or the lone candidate) for the symbol.
    pub(super) fn find_symbol_in_referenced_file(
        &mut self,
        path_wo_anchor: &str,
        symbol_name: &str,
        file_nodes: &[Arc<KNode>],
    ) -> Result<Option<Arc<KNode>>> {
        let candidate_files: Vec<Arc<KNode>> = file_nodes
            .iter()
            .filter(|n| {
                n.qualified_name == path_wo_anchor
                    || n.file_path == path_wo_anchor
                    || n.qualified_name.ends_with(path_wo_anchor)
                    || n.file_path.ends_with(path_wo_anchor)
            })
            .cloned()
            .collect();
        let search: &[Arc<KNode>] = if !candidate_files.is_empty() {
            &candidate_files
        } else if file_nodes.len() == 1 {
            file_nodes
        } else {
            &[]
        };
        for file_node in search {
            let nodes = self.nodes_in_file(&file_node.file_path)?;
            let normalized_symbol = symbol_name.replace('/', ".");
            let file_prefix = format!("{}::{}", file_node.file_path, normalized_symbol);
            let colon_tail = format!("::{}", normalized_symbol);
            let dot_tail = format!(".{}", normalized_symbol);
            if let Some(exact) = nodes.iter().find(|n| {
                n.name == normalized_symbol
                    || n.qualified_name == file_prefix
                    || n.qualified_name.ends_with(&colon_tail)
                    || n.qualified_name.ends_with(&dot_tail)
            }) {
                return Ok(Some(exact.clone()));
            }
            let last_part = normalized_symbol.split(['.', ':']).next_back().unwrap_or("");
            if last_part.is_empty() {
                continue;
            }
            if let Some(by_last) = nodes.iter().find(|n| {
                n.name == last_part
                    && matches!(
                        n.kind.as_str(),
                        "function" | "method" | "class" | "module" | "constant" | "variable"
                    )
            }) {
                return Ok(Some(by_last.clone()));
            }
        }
        Ok(None)
    }

    /// findAnchoredMarkdownSection (name-matcher.ts): `path#anchor` → the
    /// markdown section module node.
    pub(super) fn find_anchored_markdown_section(
        &mut self,
        path_wo_anchor: &str,
        anchor: &str,
        file_nodes: &[Arc<KNode>],
    ) -> Result<Option<Arc<KNode>>> {
        let normalized_anchor = normalize_markdown_anchor(anchor);
        for file_node in file_nodes.iter().filter(|n| {
            n.qualified_name == path_wo_anchor
                || n.file_path == path_wo_anchor
                || n.qualified_name.ends_with(path_wo_anchor)
                || n.file_path.ends_with(path_wo_anchor)
        }) {
            let section_qn = format!("{}#{}", file_node.file_path, normalized_anchor);
            if let Some(exact) = self
                .nodes_by_qualified_name(&section_qn)?
                .iter()
                .find(|n| n.kind == "module" && n.language == "markdown")
            {
                return Ok(Some(exact.clone()));
            }
            if let Some(section) = self
                .nodes_in_file(&file_node.file_path)?
                .iter()
                .find(|n| {
                    n.kind == "module"
                        && n.language == "markdown"
                        && n.qualified_name == section_qn
                })
            {
                return Ok(Some(section.clone()));
            }
        }
        Ok(None)
    }

    // -----------------------------------------------------------------------
    // Non-bare migrated refs — resolveOneInner's member slice (§5.16).
    // Ported arms: builtin/external, prefilter, phpStatic, boundReceiver's
    // DB sub-arms (br:import + claim refusals), viaImport's member descent
    // (static member + object literal), filePath, qualifiedName. Arms that
    // read source (receiver-type inference, object-literal alias/instance
    // member, store bindings) or defer (chains, this.members) punt so the TS
    // spine reproduces them exactly.
    // -----------------------------------------------------------------------
}
