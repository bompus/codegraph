use super::*;

struct VbReceiver {
    owners: Vec<Arc<KNode>>,
    typed: Option<VbType>,
    through_type: bool,
    color_color: bool,
}


impl KernelResolver {
    fn vb_value_receiver(&mut self, typed: Option<VbType>) -> Res<Option<VbReceiver>> {
        let Some(t) = typed.filter(|t| vb_key(t) != "object") else { return Ok(None); };
        Ok(self.vb_owners(&t)?.map(|owners| VbReceiver { owners, typed: Some(t), through_type: false, color_color: false }))
    }
    fn vb_path_receiver(&mut self, head: &str, links: &[&str], r: &ResolveRefIn) -> Res<Option<VbReceiver>> {
        let mut at = if head.starts_with('{') {
            let owner = self.node_by_id(&r.from_node_id)?;
            let typed = self.vb_resolve_param(vb_type(head.trim_matches(['{', '}']), &r.file_path, r.line), owner.as_deref())?;
            self.vb_value_receiver(typed)?
        } else if re!(r"(?i)^(Me|MyClass|MyBase)$").is_match(head) {
            let own = self.vb_around(&r.file_path, r.line)?.first().cloned();
            match own {
                Some(own) if head.eq_ignore_ascii_case("MyBase") => {
                    let base = self.vb_supers(&own)?.into_iter().find(|(_, implemented)| !implemented).map(|(t, _)| t);
                    self.vb_value_receiver(base)?
                }
                Some(own) => Some(VbReceiver { typed: Some(vb_simple(&own.name, r)), owners: vec![own], through_type: false, color_color: false }),
                None => None,
            }
        } else {
            let name = head.trim_matches(['[', ']']);
            let bound = self.vb_receiver_type(name, r, 0)?;
            let written = match &bound { None => Some(vb_simple(name, r)), Some(Some(t)) if !t.array && t.name.eq_ignore_ascii_case(name) => Some(t.clone()), _ => None };
            if let Some(written) = written {
                let (owners, ambiguous) = self.vb_types_at(&written, true)?;
                (!ambiguous && !owners.is_empty()).then_some(VbReceiver { owners, typed: None, through_type: true, color_color: bound.is_some() })
            } else { self.vb_value_receiver(bound.flatten())? }
        };
        for link in links {
            let Some(prior) = at else { return Ok(None); };
            let name = link.trim_matches(['[', ']']);
            let found = self.vb_member_on(&prior.owners, name, r, true, prior.typed.as_ref(), true)?
                .or(self.vb_member_on(&prior.owners, &format!("[{name}]"), r, true, prior.typed.as_ref(), true)?);
            let Some(found) = found else { return Ok(None); };
            at = if vb_like_kind(&found.node.kind) {
                prior.through_type.then_some(VbReceiver { owners: vec![found.node], typed: None, through_type: true, color_color: false })
            } else { let typed = self.vb_member_type(&found)?; self.vb_value_receiver(typed)? };
        }
        Ok(at)
    }
    fn vb_member_read(&mut self, head: &str, links: &[&str], name: &str, r: &ResolveRefIn) -> Res<Option<VbHit>> {
        let Some(at) = self.vb_path_receiver(head, links, r)? else { return Ok(None); };
        let name = name.trim_matches(['[', ']']);
        let found = self.vb_member_on(&at.owners, name, r, true, at.typed.as_ref(), true)?
            .or(self.vb_member_on(&at.owners, &format!("[{name}]"), r, true, at.typed.as_ref(), true)?);
        let Some(mut found) = found else { return Ok(None); };
        if found.node.id == r.from_node_id { return Ok(None); }
        if !at.through_type && !matches!(found.node.kind.as_str(), "method" | "field" | "property" | "constant" | "variable") { return Ok(None); }
        if at.through_type && !found.node.is_static {
            if let Some(shared) = self.nodes_by_qualified_name(&found.node.qualified_name)?.iter().find(|n| n.is_static && n.file_path == found.node.file_path && n.id != r.from_node_id) { found.node = shared.clone(); }
        }
        let constant = self.read_file(&found.node.file_path).and_then(|f| f.get((found.node.start_line - 1).max(0) as usize).cloned())
            .is_some_and(|line| re!(r"(?i)^\s*(?:(?:Public|Private|Protected|Friend|Shadows)\s+)*Const\b").is_match(&line));
        let shared = found.node.is_static || constant || matches!(found.node.kind.as_str(), "constant" | "enum_member") || at.owners.iter().any(|n| self.vb_module(n));
        let through_value = !at.through_type || at.color_color && !shared;
        let line = self.read_file(&r.file_path).and_then(|f| f.get((r.line - 1).max(0) as usize).cloned()).unwrap_or_default();
        let named = re!(r"(?i)\b(?:AddressOf\s+|NameOf\s*\(\s*)$").is_match(super::names::js_prefix(&line, r.column.max(0) as usize));
        let edge_kind = (found.node.kind == "method" && !named).then(|| "calls".to_string());
        if through_value || found.node.kind == "method" || !links.is_empty() {
            return Ok(Some(VbHit { candidate: KCand { node: found.node, confidence: if through_value { 0.9 } else { 0.85 }, resolved_by: if through_value { "instance-method" } else { "qualified-name" } }, edge_kind, also: vec![] }));
        }
        self.vb_read_through(found.node, &at.owners, r, 0.9, edge_kind.as_deref())
    }
    fn vb_path_call(&mut self, head: &str, links: &[&str], name: &str, r: &ResolveRefIn) -> Res<Option<VbHit>> {
        let Some(at) = self.vb_path_receiver(head, links, r)? else { return Ok(None); };
        let Some(typed) = at.typed.filter(|_| !at.through_type) else { return Ok(None); };
        let name = name.trim_matches(['[', ']']);
        self.vb_value_call(&typed, &at.owners, name, r)
    }
    fn vb_value_call(&mut self, typed: &VbType, owners: &[Arc<KNode>], member: &str, r: &ResolveRefIn) -> Res<Option<VbHit>> {
        if let Some(found) = self.vb_member_on(owners, member, r, false, Some(typed), false)? {
            return Ok(Some(VbHit { candidate: KCand { node: found.node, confidence: 0.9, resolved_by: "instance-method" }, edge_kind: None, also: vec![] }));
        }
        if let Some(found) = self.vb_member_on(owners, member, r, true, Some(typed), false)? {
            return Ok(Some(VbHit { candidate: KCand { node: found.node, confidence: 0.9, resolved_by: "instance-method" }, edge_kind: Some("references".into()), also: vec![] }));
        }
        Ok(self.vb_extension_for(typed, owners, member, r)?.map(|candidate| VbHit { candidate, edge_kind: None, also: vec![] }))
    }
    pub(in crate::resolve) fn vb_read_through(
        &mut self,
        member: Arc<KNode>,
        owners: &[Arc<KNode>],
        r: &ResolveRefIn,
        confidence: f64,
        edge_kind: Option<&str>,
    ) -> Res<Option<VbHit>> {
        if member.id == r.from_node_id {
            return Ok(None);
        }
        let Some(owner) = owners
            .iter()
            .find(|o| o.file_path == member.file_path)
            .or_else(|| owners.first())
        else {
            return Ok(None);
        };
        let from = self
            .node_by_id(&r.from_node_id)?
            .map(|n| n.qualified_name.to_ascii_lowercase());
        let own = owner.qualified_name.to_ascii_lowercase();
        let inside = owner.id == r.from_node_id
            || from.is_some_and(|from| from == own || from.starts_with(&format!("{own}::")));
        Ok(Some(VbHit {
            candidate: KCand {
                node: member,
                confidence,
                resolved_by: "qualified-name",
            },
            edge_kind: edge_kind.map(str::to_string),
            also: if inside {
                vec![]
            } else {
                vec![owner.id.clone()]
            },
        }))
    }
    // None = no proven type/owner, so ordinary matching may continue.
    // Some(None) = a proven receiver/owner has no project target, so stop.
    pub(in crate::resolve) fn vb_typed_call(
        &mut self,
        receiver: &str,
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<VbHit>>> {
        if !re!(r"^[A-Za-z_]\w*$").is_match(receiver) {
            return Ok(None);
        }
        let bound = self.vb_receiver_type(receiver, r, 0)?;
        if bound.is_none() {
            let (owners, ambiguous) = self.vb_types_at(&vb_simple(receiver, r), false)?;
            if owners.is_empty() {
                return Ok(None);
            }
            if ambiguous {
                return Ok(Some(None));
            }
            if let Some(found) = self.vb_member_on(&owners, member, r, false, None, false)? {
                return Ok(Some(Some(VbHit {
                    candidate: KCand {
                        node: found.node,
                        confidence: 0.85,
                        resolved_by: "qualified-name",
                    },
                    edge_kind: None,
                    also: vec![],
                })));
            }
            let indexed = self.vb_member_on(&owners, member, r, true, None, false)?;
            return match indexed {
                Some(found) => self
                    .vb_read_through(found.node, &owners, r, 0.85, Some("references"))
                    .map(Some),
                None => Ok(Some(None)),
            };
        }
        let Some(t) = bound.flatten() else {
            return Ok(None);
        };
        if vb_key(&t) == "object" {
            return Ok(None);
        }
        let Some(owners) = self.vb_owners(&t)? else {
            return Ok(Some(None));
        };
        self.vb_value_call(&t, &owners, member, r).map(Some)
    }

    pub(in crate::resolve) fn vb_site_receiver(&mut self, r: &ResolveRefIn) -> Option<String> {
        let lines = self.read_file(&r.file_path)?;
        let line = lines.get((r.line - 1).max(0) as usize)?;
        let member = r.reference_name.rsplit('.').next()?.to_ascii_lowercase();
        if !re!(r"^\w+$").is_match(&member) {
            return None;
        }
        let lower = line.to_ascii_lowercase();
        let byte = super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len());
        let start = if lower[byte..].starts_with(&member) {
            Some(byte)
        } else {
            let re = Self::cached_regex(&format!(r"(?i)\b{}\b", regex::escape(&member))).ok()?;
            re.find_at(&lower, byte).map(|m| m.start()) // never the earlier same-named property before New
        }?;
        let before = &line[..start];
        if let Some(m) = re!(r"(?i)\(\s*Of\s+([A-Za-z0-9_.]+)\s*\)\s*\.\s*$").captures(before) {
            return Some(m[1].to_string());
        }
        let m = re!(r"([A-Za-z0-9_.()]*?)\s*\.\s*$").captures(before)?;
        Some(re!(r"\([^()]*\)").replace_all(&m[1], "").into_owned())
    }
    pub(in crate::resolve) fn resolve_vb_explicit(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.language != "vbnet" {
            return Ok(None);
        }
        let hit = if r.reference_kind == "references" {
            let Some(m) = re!(r"^(\{[^{}]+\}|\[?[A-Za-z_]\w*\]?)((?:\.\[?[A-Za-z_]\w*\]?)*)\.(\[?[A-Za-z_]\w*\]?)$").captures(&r.reference_name) else { return Ok(None); };
            let links = m[2].trim_start_matches('.').split('.').filter(|s| !s.is_empty()).collect::<Vec<_>>();
            Some(self.vb_member_read(&m[1], &links, &m[3], r)?)
        } else if r.reference_kind == "calls" {
            if let Some(m) = re!(r"^(\{[^{}]+\}|\[?[A-Za-z_]\w*\]?)((?:\.\[?[A-Za-z_]\w*\]?)*)\.(\[?[A-Za-z_]\w*\]?)$").captures(&r.reference_name) {
                if !m[2].is_empty() || m[1].starts_with('{') || re!(r"(?i)^(Me|MyClass|MyBase)$").is_match(&m[1]) {
                    let links = m[2].trim_start_matches('.').split('.').filter(|s| !s.is_empty()).collect::<Vec<_>>();
                    let hit = self.vb_path_call(&m[1], &links, &m[3], r)?;
                    return match hit { Some(hit) => {
                        let mut out = self.finish_pre_framework(r, hit.candidate)?;
                        if out.status == "resolved" { out.edge_kind = hit.edge_kind; }
                        Ok(Some(out))
                    }, None => Ok(Some(ResolveOutcome::unresolved())) };
                }
            }
            let Some(receiver) = self.vb_site_receiver(r) else {
                return Ok(None);
            };
            if re!(r"(?i)^(Me|MyClass|MyBase)$").is_match(&receiver) {
                return Ok(None);
            }
            let member = r
                .reference_name
                .rsplit('.')
                .next()
                .unwrap_or(&r.reference_name);
            self.vb_typed_call(&receiver, member, r)?
        } else {
            None
        };
        let Some(hit) = hit else { return Ok(None) };
        let Some(hit) = hit else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        let target = hit.candidate.node.id.clone();
        let mut out = self.finish_pre_framework(r, hit.candidate)?;
        if out.status == "resolved" && out.target_node_id.as_deref() == Some(target.as_str()) {
            out.edge_kind = hit.edge_kind;
            if !hit.also.is_empty() {
                out.also_target_node_ids = Some(hit.also);
            }
        }
        Ok(Some(out))
    }
    pub(in crate::resolve) fn vb_scope_owners(&mut self, r: &ResolveRefIn) -> Res<Option<HashSet<String>>> {
        let around = self.vb_around(&r.file_path, r.line)?;
        if around.is_empty() {
            return Ok(None);
        }
        let mut names = HashSet::new();
        for owner in around {
            for ancestor in self.vb_ancestry(&owner)? {
                names.insert(ancestor.node.name.to_ascii_lowercase());
            }
        }
        Ok(Some(names))
    }
    pub(in crate::resolve) fn vb_member_in_scope(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.language != "vbnet" || !vb_member_kind(&n.kind) {
            return Ok(true);
        }
        let Some(qn) = vb_parent_qn(n) else {
            return Ok(true);
        };
        let Some(owners) = self.vb_scope_owners(r)? else {
            return Ok(true);
        };
        let owner = vb_segments(qn).pop().unwrap_or_default();
        if owners.contains(&owner)
            || self
                .vb_header(&r.file_path)
                .imports
                .iter()
                .any(|i| i.rsplit('.').next() == Some(owner.as_str()))
        {
            return Ok(true);
        }
        for parent in self.nodes_by_qualified_name(qn)?.iter() {
            if self.vb_module(parent) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(in crate::resolve) fn vb_nested_in_scope(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.language != "vbnet" || !vb_like_kind(&n.kind) {
            return Ok(true);
        }
        let Some(parent) = self.vb_enclosing(n)? else {
            return Ok(true);
        };
        let Some(owners) = self.vb_scope_owners(r)? else {
            return Ok(true);
        };
        if owners.contains(&parent.name.to_ascii_lowercase()) {
            return Ok(true);
        }
        let h = self.vb_header(&r.file_path);
        if h.imports.iter().any(|i| {
            i.rsplit('.')
                .next()
                .is_some_and(|v| v.eq_ignore_ascii_case(&parent.name))
        }) {
            return Ok(true);
        }
        if let Some(alias) = h.aliases.get(&r.reference_name.to_ascii_lowercase()) {
            let full = self.vb_full(n);
            if alias.name.eq_ignore_ascii_case(&n.name)
                && (alias.qualifier.is_empty()
                    || vb_tail(&full[..full.len() - 1], &alias.qualifier))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(in crate::resolve) fn vb_qualified_by(&mut self, n: &KNode, qualifier: &str, r: &ResolveRefIn) -> bool {
        if n.language != "vbnet" || !vb_like_kind(&n.kind) {
            return true;
        }
        let Some(written) = vb_type(&format!("{qualifier}.{}", n.name), &r.file_path, r.line)
        else {
            return true;
        };
        let written = self.vb_unalias(&written);
        let full = self.vb_full(n);
        written.qualifier.is_empty() || vb_tail(&full[..full.len() - 1], &written.qualifier)
    }
    pub(in crate::resolve) fn vb_break_tie(
        &mut self,
        tied: Vec<Arc<KNode>>,
        r: &ResolveRefIn,
    ) -> Option<Arc<KNode>> {
        let mut seen = HashSet::new();
        let distinct: Vec<_> = tied
            .into_iter()
            .filter(|n| {
                seen.insert((
                    self.vb_project(&n.file_path),
                    n.qualified_name.to_ascii_lowercase(),
                ))
            })
            .collect();
        if distinct.len() == 1 {
            return distinct.into_iter().next();
        }
        let mut pool: Vec<_> = distinct
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        if pool.is_empty() {
            pool = distinct
                .iter()
                .filter(|n| self.vb_same_project(&n.file_path, &r.file_path))
                .cloned()
                .collect();
            if pool.is_empty() {
                pool = distinct;
            }
        }
        if pool.len() == 1 {
            return pool.into_iter().next();
        }
        let shared = |file: &str| {
            r.file_path
                .split('/')
                .rev()
                .skip(1)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .zip(
                    file.split('/')
                        .rev()
                        .skip(1)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                )
                .take_while(|(a, b)| a == b)
                .count()
        };
        let best = pool.iter().map(|n| shared(&n.file_path)).max()?;
        let mut winners = pool.into_iter().filter(|n| shared(&n.file_path) == best);
        let first = winners.next()?;
        winners.next().is_none().then_some(first)
    }
}
