//! Receiver types of trailing Kotlin DSL lambda parameters.
use super::*;
impl KernelResolver {
    pub(super) fn kotlin_lambda_receiver(
        &mut self,
        name: &str,
        r: &ResolveRefIn,
        owners: &HashSet<String>,
    ) -> Res<Option<String>> {
        let candidates: Vec<_> = self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| n.language == "kotlin" && matches!(n.kind.as_str(), "function" | "method"))
            .cloned()
            .collect();
        let mut receivers = HashSet::new();
        for declaration in candidates {
            let dispatch_key = format!("lambda-dispatch:{}", declaration.id);
            let dispatch =
                if let Some(dispatch) = self.kotlin_lambda_receiver_memo.get(&dispatch_key) {
                    dispatch.clone()
                } else {
                    let dispatch = self
                        .nodes_in_file(&declaration.file_path)?
                        .iter()
                        .filter(|n| {
                            n.id != declaration.id
                                && matches!(
                                    n.kind.as_str(),
                                    "class" | "interface" | "object" | "enum" | "struct" | "trait"
                                )
                                && n.start_line <= declaration.start_line
                                && declaration.end_line <= n.end_line
                        })
                        .min_by_key(|n| n.end_line - n.start_line)
                        .map(|n| n.name.clone());
                    self.kotlin_lambda_receiver_memo
                        .insert(dispatch_key, dispatch.clone());
                    dispatch
                };
            if let Some(dispatch) = dispatch {
                if !owners.contains(&dispatch) {
                    continue;
                }
            } else if !self.kotlin_top_level_visible(&declaration, r)? {
                continue;
            }
            // Memoize declaration syntax, never a name-only verdict across different caller scopes.
            let key = format!("lambda-declaration:{}", declaration.id);
            if let Some(receiver) = self.kotlin_lambda_receiver_memo.get(&key) {
                if let Some(receiver) = receiver {
                    receivers.insert(receiver.clone());
                }
                continue;
            }
            self.kotlin_lambda_receiver_memo.insert(key.clone(), None);
            let Some(source) = self.read_file(&declaration.file_path) else {
                continue;
            };
            let site = ResolveRefIn {
                row_id: None,
                from_node_id: declaration.id.clone(),
                reference_name: declaration.name.clone(),
                reference_kind: "calls".to_string(),
                line: declaration.start_line,
                column: declaration.start_column,
                candidates: None,
                file_path: declaration.file_path.clone(),
                language: "kotlin".to_string(),
                failure_reason: None,
            };
            let Some(tree) = self.parsed_tree(&source, &site) else {
                continue;
            };
            let mut node = super::iteration::descendant_for_position(
                tree.root_node(),
                source.text(),
                (
                    (declaration.start_line - 1).max(0) as usize,
                    declaration.start_column.max(0) as usize + 1,
                ),
            );
            while node.kind() != "function_declaration" {
                let Some(parent) = node.parent() else { break };
                node = parent;
            }
            let Some(parameters) = super::iteration::named_children(node)
                .into_iter()
                .find(|n| n.kind() == "function_value_parameters")
            else {
                continue;
            };
            let Some(last) = super::iteration::named_children(parameters)
                .into_iter()
                .rfind(|n| n.kind() == "parameter")
            else {
                continue;
            };
            let text = &source.text()[last.start_byte()..last.end_byte()];
            let Some((_, ty)) = text.split_once(':') else {
                continue;
            };
            let receiver = self.kotlin_lambda_type_receiver(ty, &site, 0)?;
            self.kotlin_lambda_receiver_memo
                .insert(key, receiver.clone());
            if let Some(receiver) = receiver {
                receivers.insert(receiver);
            }
        }
        let receiver = if receivers.len() == 1 {
            receivers.into_iter().next()
        } else {
            None
        };
        Ok(receiver)
    }
    fn kotlin_lambda_type_receiver(
        &mut self,
        ty: &str,
        site: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<String>> {
        if let Some(m) =
            re!(r"^\s*(?:suspend\s+)?([A-Z]\w*)(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\s*\.\s*\(")
                .captures(ty)
        {
            return Ok(Some(m[1].to_string()));
        }
        if depth > 2 {
            return Ok(None);
        }
        let Some(alias) = re!(r"^\s*([A-Z]\w*)\b").captures(ty) else {
            return Ok(None);
        };
        let name = &alias[1];
        let imports = self.import_mappings(&site.file_path)?;
        let explicit: Vec<_> = imports
            .iter()
            .filter(|mapping| !mapping.is_namespace && mapping.local_name == name)
            .collect();
        let mut candidates = Vec::new();
        if !explicit.is_empty() {
            for mapping in explicit {
                let leaf = mapping.source.rsplit('.').next().unwrap_or(&mapping.source);
                for declaration in self.nodes_by_name(leaf)?.iter().filter(|n| {
                    n.language == "kotlin"
                        && n.kind == "type_alias"
                        && n.qualified_name.replace("::", ".") == mapping.source
                }) {
                    if !candidates
                        .iter()
                        .any(|candidate: &Arc<KNode>| candidate.id == declaration.id)
                    {
                        candidates.push(declaration.clone());
                    }
                }
            }
        } else {
            let all: Vec<_> = self
                .nodes_by_name(name)?
                .iter()
                .filter(|n| n.language == "kotlin" && n.kind == "type_alias")
                .cloned()
                .collect();
            candidates = all
                .iter()
                .filter(|n| n.file_path == site.file_path)
                .cloned()
                .collect();
            if candidates.is_empty() {
                let package = self.kotlin_file_scope(&site.file_path).pkg.clone();
                for declaration in &all {
                    if self.kotlin_file_scope(&declaration.file_path).pkg == package {
                        candidates.push(declaration.clone());
                    }
                }
            }
            if candidates.is_empty() {
                for declaration in all {
                    if self.kotlin_top_level_visible(&declaration, site)? {
                        candidates.push(declaration);
                    }
                }
            }
        }
        // Ambiguous or external aliases cannot supply a project receiver.
        let [declaration] = candidates.as_slice() else {
            return Ok(None);
        };
        let Some(source) = self.read_file(&declaration.file_path) else {
            return Ok(None);
        };
        let from = (declaration.start_line - 1).max(0) as usize;
        let to = (declaration.end_line.max(0) as usize).min(source.len());
        let text = source[from.min(to)..to].join(" ");
        if let Some((_, rhs)) = text.split_once('=') {
            let mut alias_site = site.clone().at(declaration);
            alias_site.column = declaration.start_column;
            return self.kotlin_lambda_type_receiver(rhs, &alias_site, depth + 1);
        }
        Ok(None)
    }
}
