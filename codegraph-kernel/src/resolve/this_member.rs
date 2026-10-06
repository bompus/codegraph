//! `this.<member>` function-as-value refs (index.ts resolveThisMemberFnRef and
//! the deferred pass's matchDeferredThisMember): `btn.on('click',
//! this.handleClick)` names a member of the enclosing class — on the class
//! itself (same file), or once supertype edges exist, on the nearest
//! supertype that declares it.

use super::*;

/// resolveThisMemberFnRef's three results.
pub(super) enum ThisMember {
    Found(KCand),
    /// Not on the class itself: possibly inherited — retry in the deferred
    /// pass once implements/extends edges exist.
    Defer,
    Miss,
}

impl KernelResolver {
    /// The targets of `source`'s `kinds` edges, by kind, then in insertion
    /// order: the order the (source, kind) index returns them, pinned so a
    /// different query plan cannot change which supertype comes first.
    pub(super) fn outgoing_edge_targets(&self, source: &str, kinds: &[&str]) -> Res<Vec<String>> {
        let conn = self.conn()?;
        let placeholders = (0..kinds.len()).map(|i| format!("?{}", i + 2)).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT target FROM edges WHERE source = ?1 AND kind IN ({placeholders}) ORDER BY kind, id");
        let mut stmt = conn.prepare(&sql).map_err(|e| Error::from_reason(e.to_string()))?;
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&source];
        for k in kinds {
            params.push(k);
        }
        let rows = stmt
            .query_map(params.as_slice(), |row| row.get::<_, String>("target"))
            .map_err(|e| Error::from_reason(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<String>, rusqlite::Error>>()
            .map_err(|e| Error::from_reason(e.to_string()).into())
    }

    /// resolveThisMemberFnRef — the enclosing class's own member, same file.
    pub(super) fn resolve_this_member_fn_ref(&mut self, r: &ResolveRefIn) -> Res<ThisMember> {
        let member = &r.reference_name["this.".len()..];
        if member.is_empty() {
            return Ok(ThisMember::Miss);
        }
        let Some(from) = self.node_by_id(&r.from_node_id)? else {
            return Ok(ThisMember::Miss);
        };
        let from = self.this_scope_caller(from)?;
        // A class-body hook (Ruby `before_action :x`) attributes to the class
        // node itself; a member strips its own segment.
        let class_prefix = if is_supertype_bearing_kind(&from.kind) || from.kind == "module" {
            from.qualified_name.clone()
        } else {
            match from.qualified_name.rfind("::") {
                Some(sep) if sep > 0 => from.qualified_name[..sep].to_string(),
                _ => return Ok(ThisMember::Miss),
            }
        };
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{class_prefix}::{member}"))?
            .iter()
            .filter(|n| {
                (n.kind == "function" || n.kind == "method") && n.file_path == r.file_path && n.id != r.from_node_id
            })
            .cloned()
            .collect();
        // `reduce((a, b) => a.startLine <= b.startLine ? a : b)`: the first of
        // the earliest-starting candidates.
        let Some(target) = candidates.iter().fold(None::<&Arc<KNode>>, |best, n| match best {
            Some(b) if b.start_line <= n.start_line => Some(b),
            _ => Some(n),
        }) else {
            return Ok(ThisMember::Defer);
        };
        Ok(ThisMember::Found(KCand { node: target.clone(), confidence: 0.95, resolved_by: "function-ref" }))
    }

    /// matchDeferredThisMember — a node-anchored BFS from the enclosing class
    /// up implements/extends edges (depth < 5), taking the first supertype
    /// whose `contains` edges hold a same-family function/method `member`,
    /// except that an interface or protocol declaration yields to any body.
    pub(super) fn match_deferred_this_member(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        // Every queued ref starts `this.` (resolveThisMemberFnRef deferred it).
        let member = r.reference_name.get("this.".len()..).unwrap_or("");
        let Some(from) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        let from = self.this_scope_caller(from)?;
        if member.is_empty() {
            return Ok(None);
        }
        let (class_prefix, class_name) = if is_supertype_bearing_kind(&from.kind) || from.kind == "module" {
            (from.qualified_name.clone(), from.name.clone())
        } else {
            match from.qualified_name.rfind("::") {
                Some(sep) if sep > 0 => {
                    let prefix = &from.qualified_name[..sep];
                    let name = match prefix.rfind("::") {
                        Some(i) => &prefix[i + 2..],
                        None => prefix,
                    };
                    (prefix.to_string(), name.to_string())
                }
                _ => return Ok(None),
            }
        };
        let named = self.nodes_by_name(&class_name)?;
        // The enclosing class by its qualified name, so a same-named class
        // nested in a function elsewhere in the file (`decoy::LoginForm`)
        // never seeds the walk; by bare name only when none matches.
        let same_file = |n: &&Arc<KNode>| is_supertype_bearing_kind(&n.kind) && n.file_path == r.file_path;
        let mut frontier: Vec<Arc<KNode>> =
            named.iter().filter(same_file).filter(|n| n.qualified_name == class_prefix).cloned().collect();
        if frontier.is_empty() {
            frontier = named.iter().filter(same_file).cloned().collect();
        }
        if frontier.is_empty() {
            // Declared in another file (partial/reopened classes).
            frontier = named
                .iter()
                .filter(|n| is_supertype_bearing_kind(&n.kind) && same_language_family(&n.language, &r.language))
                .cloned()
                .collect();
        }
        let mut seen: HashSet<String> = frontier.iter().map(|n| n.id.clone()).collect();
        // A member declared on an interface or protocol is a signature; an
        // inherited class body anywhere up the chain is what runs, so the
        // declaration is only a fallback. Traits stay first-hit: Scala and PHP
        // traits carry bodies that override the parent chain.
        let mut declared_only: Option<Arc<KNode>> = None;
        for _depth in 0..5 {
            if frontier.is_empty() {
                break;
            }
            let mut next: Vec<Arc<KNode>> = Vec::new();
            for type_node in &frontier {
                for edge_target in self.outgoing_edge_targets(&type_node.id, &["implements", "extends"])? {
                    let Some(super_node) = self.node_by_id(&edge_target)? else { continue };
                    if !seen.insert(super_node.id.clone()) {
                        continue;
                    }
                    if !is_supertype_bearing_kind(&super_node.kind) {
                        continue;
                    }
                    for child in self.outgoing_edge_targets(&super_node.id, &["contains"])? {
                        let Some(m) = self.node_by_id(&child)? else { continue };
                        if m.name == member
                            && (m.kind == "function" || m.kind == "method")
                            && same_language_family(&m.language, &r.language)
                        {
                            if !matches!(super_node.kind.as_str(), "interface" | "protocol") {
                                return Ok(Some(KCand { node: m, confidence: 0.85, resolved_by: "function-ref" }));
                            }
                            declared_only.get_or_insert(m);
                            break;
                        }
                    }
                    next.push(super_node);
                }
            }
            frontier = next;
        }
        Ok(declared_only.map(|node| KCand { node, confidence: 0.85, resolved_by: "function-ref" }))
    }
}
