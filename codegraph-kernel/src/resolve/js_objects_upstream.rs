//! Native f40db4b9 named-object matching.
use super::*;
use super::js_object_facts_upstream::JsObjectFacts;

pub(super) struct JsObjects {
    pub files: Lru<Rc<JsObjectFacts>>,
    owners: HashMap<String,Option<Arc<KNode>>>,
    this_callers: HashMap<String,Arc<KNode>>,
    globals: HashMap<String,bool>,
}

impl Default for JsObjects {
    fn default() -> Self {
        Self { files: Lru::new(32), owners: HashMap::new(),
            this_callers: HashMap::new(), globals: HashMap::new() }
    }
}

impl JsObjects {
    pub fn clear(&mut self) { *self = Self::default(); }
}

pub(super) enum ObjectPathMatch {
    NoHolder,
    Refused,
    Found(KCand),
}

pub(super) fn last_segment(n: &KNode) -> &str {
    n.qualified_name.rsplit("::").next().unwrap_or(&n.qualified_name)
}

pub(super) fn holder_path(n: &KNode) -> &str {
    let last = last_segment(n);
    if !last.contains('.') { return &n.name; }
    last.strip_prefix("window.").or_else(|| last.strip_prefix("globalThis.")).unwrap_or(last)
}

pub(super) fn path_holder(n: &KNode) -> bool { last_segment(n).contains('.') }
fn global_host(n: &KNode) -> bool {
    last_segment(n).starts_with("window.") || last_segment(n).starts_with("globalThis.")
}

pub(super) fn literal_owner(n: &KNode) -> bool {
    matches!(n.kind.as_str(),"constant" | "variable") && is_js_family(&n.language)
        && n.signature.as_deref().is_some_and(|s| re!(r"^=\s*(?:(?:Object\.(?:freeze|seal)\s*)?\(\s*)*\{").is_match(s))
}

fn range_within(n: &KNode,p: &KNode) -> bool {
    (n.start_line,n.start_column) >= (p.start_line,p.start_column)
        && (n.end_line,n.end_column) <= (p.end_line,p.end_column)
}

fn position_within(r: &ResolveRefIn,n: &KNode) -> bool {
    (r.line,r.column) >= (n.start_line,n.start_column)
        && (r.line,r.column) < (n.end_line,n.end_column)
}

impl KernelResolver {
    fn js_object_parent(&mut self,n: &KNode) -> Res<Option<Arc<KNode>>> {
        let Some(cut) = n.qualified_name.rfind("::").filter(|cut| *cut > 0) else { return Ok(None) };
        Ok(self.nodes_by_qualified_name(&n.qualified_name[..cut])?.iter()
            .find(|p| p.file_path == n.file_path && p.id != n.id && range_within(n,p)).cloned())
    }

    fn js_member_owner(&mut self,n: &KNode) -> Res<Option<Arc<KNode>>> {
        if n.kind != "function" || !is_js_family(&n.language) { return Ok(None); }
        if let Some(hit) = self.js_objects.owners.get(&n.id) { return Ok(hit.clone()); }
        let hit = self.js_object_parent(n)?.filter(|p| literal_owner(p));
        self.js_objects.owners.insert(n.id.clone(),hit.clone());
        Ok(hit)
    }

    fn js_arrow_member(&mut self,n: &KNode) -> bool {
        self.read_file(&n.file_path).and_then(|src| {
            src.get((n.start_line - 1).max(0) as usize).map(|line| {
                re!(r"^(?:async\s*)?(?:[(<]|[A-Za-z_$][\w$]*\s*=>)")
                    .is_match(js_slice(line,n.start_column.max(0) as usize))
            })
        }).unwrap_or(false)
    }

    pub(super) fn this_scope_caller(&mut self,caller: Arc<KNode>) -> Res<Arc<KNode>> {
        if !is_js_family(&caller.language) { return Ok(caller); }
        if let Some(hit) = self.js_objects.this_callers.get(&caller.id) { return Ok(hit.clone()); }
        let mut current = caller.clone();
        for _ in 0..8 {
            let owner = if literal_owner(&current) { Some(current.clone()) }
                else if self.js_arrow_member(&current) { self.js_member_owner(&current)? }
                else { None };
            let Some(owner) = owner else { break };
            let Some(parent) = self.js_object_parent(&owner)? else { break };
            current = parent;
        }
        self.js_objects.this_callers.insert(caller.id.clone(),current.clone());
        Ok(current)
    }

    fn this_object(&mut self,caller: Arc<KNode>) -> Res<Option<Arc<KNode>>> {
        let scope = self.this_scope_caller(caller)?;
        let owner = self.js_member_owner(&scope)?;
        Ok(if self.js_arrow_member(&scope) { None } else { owner })
    }

    fn js_holder_scope(&mut self,n: &KNode,r: &ResolveRefIn,via_global: bool) -> Res<Option<i64>> {
        if global_host(n) && !via_global && !self.js_global_holder(n)? { return Ok(None); }
        let root = if via_global && global_host(n) { last_segment(n).split('.').next().unwrap_or("") }
            else { holder_path(n).split('.').next().unwrap_or("") };
        if path_holder(n) {
            let site = ResolveRefIn { column: n.start_column, ..r.clone().at(n).naming(root, "references") };
            if self.js_root_binding_scope(&site, root) != self.js_root_binding_scope(r, root) { return Ok(None); }
        }
        let mut depth = -1;
        {
            if let Some(facts) = self.js_object_facts(&n.file_path) {
                let declared = facts.offset(n.start_line, n.start_column) as i64;
                let at = facts.offset(r.line, r.column) as i64;
                let scopes = facts.binding_scopes(root);
                let binding_at = |position| scopes.iter()
                    .filter(|(start, end)| position > *start && position < *end)
                    .max_by_key(|(start, _)| *start).copied();
                if binding_at(declared) != binding_at(at) { return Ok(None); }
            }
        }
        if path_holder(n) && !global_host(n) {
            if let Some(facts) = self.js_object_facts(&n.file_path) {
                let declared = facts.offset(n.start_line, n.start_column) as i64;
                let at = facts.offset(r.line, r.column) as i64;
                for (start, end) in facts.binding_scopes(root).iter() {
                    if declared > *start && declared < *end {
                        if at <= *start || at >= *end { return Ok(None); }
                        depth = depth.max(*start);
                    }
                }
            }
        }
        if !path_holder(n) && matches!(n.language.as_str(),"javascript" | "jsx" | "typescript" | "tsx") {
            if let Some(facts) = self.js_object_facts(&n.file_path) {
                if let Some((start,end)) = facts.block_at(facts.offset(n.start_line,n.start_column)) {
                    let at = facts.offset(r.line,r.column);
                    if at <= start || at >= end { return Ok(None); }
                    depth = start as i64;
                }
            }
        }
        if !via_global {
            if let Some(caller) = self.node_by_id(&r.from_node_id)? {
                if matches!(caller.kind.as_str(),"function" | "method")
                    && caller.start_line <= r.line && caller.end_line >= r.line
                    && self.js_object_facts(&r.file_path).is_some_and(|f| f.function_binds(root,&caller,r))
                    && !(n.file_path == r.file_path && n.start_line >= caller.start_line && n.start_line <= caller.end_line) {
                    return Ok(None);
                }
            }
        }
        Ok(Some(depth))
    }

    fn js_global_holder(&mut self,n: &KNode) -> Res<bool> {
        if let Some(hit) = self.js_objects.globals.get(&n.id) { return Ok(*hit); }
        // A false seed breaks recursive path/root cycles.
        self.js_objects.globals.insert(n.id.clone(),false);
        let root = if global_host(n) { last_segment(n).split('.').next().unwrap_or("") }
            else { holder_path(n).split('.').next().unwrap_or("") };
        let site = ResolveRefIn { row_id: None, from_node_id: n.id.clone(),
            reference_name: root.to_string(), reference_kind: "references".to_string(),
            line: n.start_line, column: n.start_column, candidates: None,
            file_path: n.file_path.clone(), language: n.language.clone(), failure_reason: None, generated: None };
        if self.js_root_binding_scope(&site, root).is_some() { return Ok(false); }
        let global = if global_host(n) {
            let host = root;
            self.js_object_facts(&n.file_path).is_some_and(|facts| {
                let declared = facts.offset(n.start_line, n.start_column) as i64;
                !facts.binding_scopes(host).iter().any(|(start, end)| declared > *start && declared < *end)
            })
        }
            else if !matches!(n.language.as_str(),"javascript" | "jsx" | "typescript" | "tsx") { false }
            else if let Some(facts) = self.js_object_facts(&n.file_path) {
                if !facts.classic(&n.file_path) { false }
                else if !path_holder(n) { facts.block_at(facts.offset(n.start_line,n.start_column)).is_none() }
                else {
                    let root = holder_path(n).split('.').next().unwrap_or("");
                    let declared = facts.offset(n.start_line, n.start_column) as i64;
                    if facts.binding_scopes(root).iter().any(|(start, end)|
                        *start >= 0 && declared > *start && declared < *end) {
                        return Ok(false);
                    }
                    let roots: Vec<_> = self.nodes_by_name(root)?.iter().filter(|p| {
                        p.id != n.id && matches!(p.kind.as_str(),"constant" | "variable")
                            && is_js_family(&p.language) && holder_path(p) == root
                    }).cloned().collect();
                    let mut found = false;
                    for root in roots { if self.js_global_holder(&root)? { found = true; break; } }
                    found
                }
            } else { false };
        self.js_objects.globals.insert(n.id.clone(),global);
        Ok(global)
    }

    fn js_hit_holder(&mut self,n: &KNode,member: &str,r: &ResolveRefIn) -> Res<Option<KCand>> {
        if let Some(hit) = self.resolve_object_literal_member(n,member,r,0.85,"instance-method")? {
            return Ok(Some(hit));
        }
        self.resolve_object_literal_binding(n,member,r)
    }

    pub(super) fn resolve_object_path_member(&mut self,path: &str,member: &str,r: &ResolveRefIn,host: Option<&str>) -> Res<ObjectPathMatch> {
        if host.is_some_and(|name| self.js_object_facts(&r.file_path)
            .is_some_and(|facts| facts.binds_at(name, r))) {
            return Ok(ObjectPathMatch::Refused);
        }
        let tail = path.rsplit('.').next().unwrap_or(path);
        let named: Vec<_> = self.nodes_by_name(tail)?.iter().filter(|n| {
            literal_owner(n) && same_language_family(&n.language,&r.language) && holder_path(n) == path
        }).cloned().collect();
        if named.is_empty() { return Ok(ObjectPathMatch::NoHolder); }
        let mut local = Vec::new();
        for n in named.iter().filter(|n| n.file_path == r.file_path) {
            if let Some(depth) = self.js_holder_scope(n,r,host.is_some())? { local.push((n.clone(),depth)); }
        }
        // Stable sort preserves upstream first-hit order at equal depth.
        local.sort_by_key(|(_,depth)| std::cmp::Reverse(*depth));
        if let Some((_,nearest)) = local.first() {
            let nearest = *nearest;
            for (n,_) in local.iter().filter(|(_,depth)| *depth == nearest) {
                if let Some(hit) = self.js_hit_holder(n,member,r)? { return Ok(ObjectPathMatch::Found(hit)); }
            }
            return Ok(ObjectPathMatch::Refused);
        }
        if self.js_parameter_binds(r, host.unwrap_or(path.split('.').next().unwrap_or(path))) {
            return Ok(ObjectPathMatch::Refused);
        }
        let mut hits = Vec::new();
        for n in named.iter().filter(|n| n.file_path != r.file_path && literal_owner(n)) {
            if self.js_global_holder(n)? {
                if let Some(hit) = self.js_hit_holder(n,member,r)? { hits.push((n.clone(),hit)); }
            }
        }
        if hits.is_empty() { return Ok(ObjectPathMatch::Refused); }
        let root = path.split('.').next().unwrap_or(path);
        let imported = host.is_none() && self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == root);
        let bound = self.js_object_facts(&r.file_path).is_some_and(|f| f.binds_at(host.unwrap_or(root),r));
        if imported || bound { return Ok(ObjectPathMatch::Refused); }
        let targets: HashSet<_> = hits.iter().map(|(_,hit)| hit.node.id.clone()).collect();
        if targets.len() == 1 {
            let (_,mut hit) = hits.remove(0); hit.confidence = 0.8;
            return Ok(ObjectPathMatch::Found(hit));
        }
        let dir = r.file_path.rfind('/').map_or("",|at| &r.file_path[..at + 1]);
        let mut nearby: Vec<_> = hits.into_iter().filter(|(n,_)| {
            n.file_path.strip_prefix(dir).is_some_and(|rest| !rest.contains('/'))
        }).collect();
        let targets: HashSet<_> = nearby.iter().map(|(_,hit)| hit.node.id.clone()).collect();
        if targets.len() != 1 { return Ok(ObjectPathMatch::Refused); }
        let (_,mut hit) = nearby.remove(0); hit.confidence = 0.75;
        Ok(ObjectPathMatch::Found(hit))
    }

    pub(super) fn match_object_path_call(&mut self,r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.reference_kind != "calls" || !is_js_family(&r.language) { return Ok(None); }
        let Some((path,member)) = r.reference_name.rsplit_once('.') else { return Ok(None) };
        let (host,path) = if let Some(rest) = path.strip_prefix("window.") { (Some("window"),rest) }
            else if let Some(rest) = path.strip_prefix("globalThis.") { (Some("globalThis"),rest) }
            else { (None,path) };
        if path.is_empty() { return Ok(None); }
        let root = path.split('.').next().unwrap_or(path);
        if host.is_none() && self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == root) {
            return Ok(None);
        }
        Ok(match self.resolve_object_path_member(path,member,r,host)? {
            ObjectPathMatch::Found(hit) => Some(hit), _ => None,
        })
    }

    pub(super) fn match_collapsed_object_call(&mut self,r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.reference_kind != "calls" || !is_js_family(&r.language)
            || !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name) { return Ok(None); }
        if let Some(src) = self.read_file(&r.file_path) {
            if let Some(line) = src.get((r.line - 1).max(0) as usize) {
                if js_slice(line,r.column.max(0) as usize).strip_prefix(r.reference_name.as_str())
                    .is_some_and(|rest| re!(r"^\s*\(").is_match(rest)) { return Ok(None); }
            }
        }
        let Some((_,receiver)) = self.bare_call_receiver(r)? else { return Ok(None) };
        if receiver.trim() == "this" {
            if let Some(caller) = self.node_by_id(&r.from_node_id)? {
                if let Some(owner) = self.this_object(caller)? {
                    return self.resolve_object_literal_member(&owner,&r.reference_name,r,0.85,"instance-method");
                }
            }
            return Ok(None);
        }
        let Some(caps) = re!(r"(?:^|[^\w$.])(window|globalThis)\s*\??\.\s*([A-Za-z_$][\w$]*(?:\s*\??\.\s*[A-Za-z_$][\w$]*)*)$").captures(&receiver) else { return Ok(None) };
        let path: String = caps[2].chars().filter(|ch| !ch.is_whitespace() && *ch != '?').collect();
        Ok(match self.resolve_object_path_member(&path,&r.reference_name,r,Some(&caps[1]))? {
            ObjectPathMatch::Found(hit) => Some(hit), _ => None,
        })
    }

    pub(super) fn object_member_reachable_by_name(&mut self,n: &KNode,r: &ResolveRefIn) -> Res<bool> {
        let Some(owner) = self.js_member_owner(n)? else { return Ok(true) };
        if r.file_path == n.file_path {
            let written_this = r.reference_kind == "calls" && position_within(r,&owner)
                && self.bare_call_receiver(r)?.is_some_and(|(_,head)| head.trim() == "this");
            if written_this {
                if let Some(caller) = self.node_by_id(&r.from_node_id)? {
                    if self.this_object(caller)?.is_some_and(|p| p.id == owner.id) { return Ok(true); }
                }
            }
            if position_within(r,n) {
                let pattern = format!(r"^(?:async\s+)?function\s*\*?\s*{}(?:[^\w$]|$)",regex::escape(&n.name));
                if let Some(src) = self.read_file(&n.file_path) {
                    if src.get((n.start_line - 1).max(0) as usize).is_some_and(|line| {
                        Self::cached_regex(&pattern).is_ok_and(|re| re.is_match(js_slice(line,n.start_column.max(0) as usize)))
                    }) { return Ok(true); }
                }
            }
        }
        self.js_destructured_object_member(n, r)
    }

    /// A scoped destructure can call an object member declared outside its caller.
    pub(super) fn js_destructured_object_member(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some(owner) = self.js_member_owner(n)? else { return Ok(false) };
        let Some(source) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(tree) = self.js_binding_tree(&source, r) else { return Ok(false) };
        let Some(facts) = self.js_object_facts(&r.file_path) else { return Ok(false) };
        for (line, column, root) in facts.destructure_sites(holder_path(&owner), &n.name, r, &tree) {
            // The captured member keeps the RHS binding at its declaration.
            let site = ResolveRefIn { line, column, ..r.clone() };
            let rhs_scope = self.js_root_binding_scope(&site, &root);
            if owner.file_path == r.file_path {
                let owner_root = if global_host(&owner) { last_segment(&owner) }
                    else { holder_path(&owner) }.split('.').next().unwrap_or("");
                if owner_root != root {
                    if rhs_scope.is_none() && self.js_global_holder(&owner)? { return Ok(true); }
                    continue;
                }
                let declaration = ResolveRefIn { column: owner.start_column, ..r.clone().at(&owner) };
                if self.js_root_binding_scope(&declaration, owner_root) == rhs_scope { return Ok(true); }
            } else if rhs_scope.is_none() && self.js_global_holder(&owner)? { return Ok(true); }
        }
        Ok(false)
    }
}

const JS_SUPER_LANGS: &[&str] = &["typescript", "tsx", "javascript", "jsx"];

/// `this.m()` written inside a class member names the member on `this`'s
/// class (its own first, then the nearest `extends` ancestor) — the
/// extractors strip `this.` so the ref arrives bare and the claim gate's
/// `this.` exclusion would otherwise leave it to name guesses.
impl KernelResolver {
    pub(super) fn match_js_this_call(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.reference_kind != "calls" || !JS_SUPER_LANGS.contains(&r.language.as_str())
            || !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name) { return Ok(None); }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return Ok(None) };
        let Some(slice) = line.get(r.column.max(0) as usize..) else { return Ok(None) };
        if !this_dot_name(slice, &r.reference_name) { return Ok(None); }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
        let Some((member, cls)) = self.js_class_member_around(&caller)? else { return Ok(None) };
        if let Some(own) = self.js_own_method(&cls, &r.reference_name, member.is_static)? {
            return Ok(Some(ResolveOutcome::resolved(&own, 0.9, "exact-match", true, None)));
        }
        match self.js_inherited_method(&cls, &r.reference_name, member.is_static)? {
            Some(node) => Ok(Some(ResolveOutcome::resolved(&node, 0.9, "exact-match", true, None))),
            None => Ok(None),
        }
    }
}

/// matchJsSuperCall (js-super-calls.ts): a TS/JS call written `super.name(…)`
/// runs the method of the class's parent, or the nearest ancestor of that
/// one that has the method, static matching the caller's kind (#2497).
/// `Ok(None)` leaves the ref to the other strategies; a site whose
/// ancestors don't carry the method is terminal — never a guess by name.
impl KernelResolver {
    pub(super) fn match_js_super_call(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if !self.is_js_super_call_site(r)? { return Ok(None); }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
        let Some((member, cls)) = self.js_class_member_around(&caller)? else { return Ok(None) };
        if self.js_writes_class_before(&member, r)? { return Ok(None); }
        match self.js_inherited_method(&cls, &r.reference_name, member.is_static)? {
            // The `extends` walk and static match prove the ancestor; the
            // name-guess post guards in `finish` would veto it (`run` is a
            // local method name), so the verdict is built directly.
            Some(node) => Ok(Some(ResolveOutcome::resolved(&node, 0.9, "instance-method", true, None))),
            None => Ok(Some(ResolveOutcome::unresolved())),
        }
    }

    /// isJsSuperCallSite: the call is written `super.name(…)` where it
    /// starts (the member on the next line or two included).
    fn is_js_super_call_site(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.reference_kind != "calls" || !JS_SUPER_LANGS.contains(&r.language.as_str())
            || !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name) { return Ok(false); }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return Ok(false) };
        let column = r.column.max(0) as usize;
        let Some(slice) = line.get(column..) else { return Ok(false) };
        if !slice.starts_with("super") { return Ok(false); }
        if super_dot_name(slice, &r.reference_name) { return Ok(true); }
        let at = (r.line - 1).max(0) as usize;
        let window = lines.get(at..(at + 3).min(lines.len())).unwrap_or_default().join("\n");
        Ok(window.get(column..).is_some_and(|w| super_dot_name(w, &r.reference_name)))
    }

    /// classMemberAround: the class member `caller` belongs to, and its
    /// class — following arrow functions nested in it up to 4 deep.
    fn js_class_member_around(&mut self, caller: &Arc<KNode>) -> Res<Option<(Arc<KNode>, Arc<KNode>)>> {
        let mut member = caller.clone();
        for _ in 0..4 {
            if member.kind != "function" { break; }
            let Some(outer) = self.js_owner_of(&member, &["function", "method", "property"])? else { return Ok(None) };
            member = outer;
        }
        if member.kind != "method" && member.kind != "property" { return Ok(None); }
        let Some(cls) = self.js_owner_of(&member, &["class"])? else { return Ok(None) };
        Ok(Some((member, cls)))
    }

    /// ownerOf: the node `node` is declared in, by its qualified name, in
    /// its file and around it.
    fn js_owner_of(&mut self, node: &Arc<KNode>, kinds: &[&str]) -> Res<Option<Arc<KNode>>> {
        let Some(cut) = node.qualified_name.rfind("::") else { return Ok(None) };
        if cut == 0 { return Ok(None); }
        Ok(self.nodes_by_qualified_name(&node.qualified_name[..cut])?.iter()
            .find(|n| kinds.contains(&n.kind.as_str()) && n.file_path == node.file_path
                && n.start_line <= node.start_line && n.end_line >= node.end_line)
            .cloned())
    }

    /// writesClassBefore: a `class` is written between where `member`
    /// starts and the call (a class expression's `super` is that class's).
    fn js_writes_class_before(&mut self, member: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        if member.start_line > r.line { return Ok(false); }
        let from = (member.start_line - 1).max(0) as usize;
        let to = (r.line as usize).min(lines.len());
        if from >= to { return Ok(false); }
        let mut span: Vec<String> = lines[from..to].to_vec();
        let last = span.len() - 1;
        let col = (r.column.max(0) as usize).min(span[last].len());
        if !span[last].is_char_boundary(col) { return Ok(false); }
        span[last] = span[last][..col].to_string();
        let first_col = if member.start_line == r.line {
            (member.start_column as usize).min(col)
        } else {
            (member.start_column as usize).min(span[0].len())
        };
        if !span[0].is_char_boundary(first_col) { return Ok(false); }
        span[0] = span[0][first_col..].to_string();
        let text = span.join("\n");
        if !has_class_keyword(&text) { return Ok(false); }
        Ok(has_class_keyword(&awaited::blank_string_contents(&awaited::strip_ts_comments(&text))))
    }

    /// inheritedMethod: the nearest method named `name` of `cls`'s
    /// ancestors, static or instance as asked (BFS over `extends`).
    fn js_inherited_method(&mut self, cls: &Arc<KNode>, name: &str, is_static: bool) -> Res<Option<Arc<KNode>>> {
        let mut seen: HashSet<String> = HashSet::from([cls.id.clone()]);
        let mut level = self.js_superclasses_of(cls)?;
        for _ in 0..8 {
            if level.is_empty() { break; }
            let mut next = Vec::new();
            for parent in level {
                if !seen.insert(parent.id.clone()) { continue; }
                if let Some(own) = self.js_own_method(&parent, name, is_static)? { return Ok(Some(own)); }
                next.extend(self.js_superclasses_of(&parent)?);
            }
            level = next;
        }
        Ok(None)
    }

    fn js_superclasses_of(&mut self, cls: &KNode) -> Res<Vec<Arc<KNode>>> {
        let ids = self.outgoing_edge_targets(&cls.id, &["extends"])?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(n) = self.node_by_id(&id)? {
                if n.kind == "class" && JS_SUPER_LANGS.contains(&n.language.as_str()) { out.push(n); }
            }
        }
        Ok(out)
    }

    /// ownMethod: a method `cls` itself declares under `name`; of
    /// TypeScript's overload signatures and the body, the body (last one).
    fn js_own_method(&mut self, cls: &KNode, name: &str, is_static: bool) -> Res<Option<Arc<KNode>>> {
        let mut own: Vec<Arc<KNode>> = self.nodes_by_qualified_name(&format!("{}::{}", cls.qualified_name, name))?
            .iter()
            .filter(|m| m.kind == "method" && m.file_path == cls.file_path
                && m.start_line >= cls.start_line && m.end_line <= cls.end_line
                && m.is_static == is_static)
            .cloned()
            .collect();
        own.sort_by_key(|m| (m.start_line, m.start_column));
        Ok(own.pop())
    }
}

/// `class` written as a keyword — not an identifier tail or a member
/// (upstream's `(?<![\w$.])class(?![\w$])`; rust regex has no look-around).
fn has_class_keyword(text: &str) -> bool {
    let bytes = text.as_bytes();
    for m in re!(r"class").find_iter(text) {
        let before = m.start().checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(m.end()).copied();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
        let before_ok = before.is_none_or(|b| !word(b) && b != b'.');
        let after_ok = after.is_none_or(|b| !word(b));
        if before_ok && after_ok { return true; }
    }
    false
}

/// The text starts `super.<name>` with `name` not an identifier's head
/// (upstream's `^super\s*\.\s*NAME(?![\w$])`, written out by hand).
fn super_dot_name(slice: &str, name: &str) -> bool {
    let rest = slice.strip_prefix("super").unwrap_or("");
    if rest.is_empty() { return false; }
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('.') else { return false };
    let rest = rest.trim_start();
    if !rest.starts_with(name) { return false; }
    rest[name.len()..].chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'))
}

/// The text starts `this.<name>` with `name` not an identifier's head.
fn this_dot_name(slice: &str, name: &str) -> bool {
    let Some(rest) = slice.strip_prefix("this.") else { return false };
    if !rest.starts_with(name) { return false; }
    rest[name.len()..].chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'))
}
