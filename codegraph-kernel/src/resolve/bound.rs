//! Bound receiver types and member lookup on a resolved owner type.

use super::*;

/// Node kinds a bound receiver type can resolve to.
const TYPE_OWNER_KINDS: [&str; 7] = ["class", "struct", "interface", "enum", "component", "type_alias", "union"];

impl KernelResolver {
    /// resolveBoundType — the declared type's owner node: Java type-parameter
    /// bounds first, then its lexical binding, then (non-ESM) the visible
    /// unique candidate.
    pub(super) fn resolve_bound_type(
        &mut self,
        ty: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Res<Option<Arc<KNode>>> {
        if depth > 4 {
            return Ok(None);
        }
        if r.language == "cpp" { if let Some(owner) = self.cpp_type_owner(ty, r, depth, false)? { return Ok(Some(owner)); } }
        if matches!(r.language.as_str(), "java" | "kotlin" | "csharp") && ty.contains('.') {
            let name = ty.rsplit('.').next().unwrap_or(ty);
            let owners: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| {
                same_language_family(&n.language, &r.language) && TYPE_OWNER_KINDS.contains(&n.kind.as_str())
                    && n.qualified_name.replace("::", ".") == ty
            }).cloned().collect();
            if let [only] = owners.as_slice() { return Ok(Some(only.clone())); }
            let (root, tail) = ty.split_once('.').unwrap();
            if let Some(owner) = self.resolve_bound_type(root, r, depth + 1)? {
                let qualified = format!("{}.{}", owner.qualified_name.replace("::", "."), tail);
                let nested: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| {
                    same_language_family(&n.language, &r.language) && TYPE_OWNER_KINDS.contains(&n.kind.as_str())
                        && n.qualified_name.replace("::", ".") == qualified
                }).cloned().collect();
                return Ok(match nested.as_slice() { [only] => Some(only.clone()), _ => None });
            }
            return Ok(None);
        }
        if r.language == "java" {
            if let Some((declaration, site)) = self.java_type_parameter(ty, r)? {
                // An unbounded or self declaration shadows any outer bound.
                let bound_re = re!(r"^[A-Za-z0-9_]+\s+extends\s+([A-Za-z0-9_.]+)$");
                return match bound_re.captures(&declaration) {
                    Some(m) if &m[1] != ty => self.resolve_bound_type(&m[1], &site, depth + 1),
                    _ => Ok(None),
                };
            }
        }
        let bindings = self.bindings(&r.file_path)?;
        let binding = innermost_binding(&bindings, ty.split('.').next().unwrap_or(ty), Some(r.line));
        let owner = match binding {
            Some(b) => self.bound_type_from_binding(ty, b, r)?,
            None if !is_esm_family(&r.language) => self.visible_unique_type(ty, r)?,
            None => None,
        };
        Ok(owner.filter(|o| TYPE_OWNER_KINDS.contains(&o.kind.as_str())))
    }

    /// The innermost enclosing Java class/interface/method declaring `ty` as
    /// a type parameter: that declaration and the declaring scope's site.
    fn java_type_parameter(&mut self, ty: &str, r: &ResolveRefIn) -> Res<Option<(String, ResolveRefIn)>> {
        let in_file = self.nodes_in_file(&r.file_path)?;
        let mut scopes: Vec<&Arc<KNode>> = in_file
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "class" | "interface" | "method")
                    && n.start_line <= r.line
                    && n.end_line >= r.line
                    && (n.start_line != r.line || n.start_column <= r.column)
                    && (n.end_line != r.line || n.end_column >= r.column)
            })
            .collect();
        scopes.sort_by(|a, b| {
            (a.end_line - a.start_line)
                .cmp(&(b.end_line - b.start_line))
                .then(b.start_column.cmp(&a.start_column))
        });
        for scope in scopes {
            let decl = scope.type_parameters.as_ref().and_then(|tps| {
                tps.iter().find(|p| {
                    // split(/\s+/)[0] — leading whitespace yields ''.
                    p.split(|c: char| c.is_whitespace()).next() == Some(ty)
                })
            });
            if let Some(declaration) = decl {
                let mut site = r.clone();
                site.line = scope.start_line;
                site.column = scope.start_column;
                return Ok(Some((declaration.clone(), site)));
            }
        }
        Ok(None)
    }

    /// The node `ty`'s lexical binding names: an import resolved through the
    /// import resolver (then the JVM import path), or the declaration itself.
    /// A PHP `use` then re-picks the owner by its fully qualified name.
    fn bound_type_from_binding(&mut self, ty: &str, b: &KBinding, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        if b.kind != "import" {
            return self.node_by_opt_id(b.node_id.as_deref());
        }
        if r.language=="rust" {
            let path=b.target_spec.as_deref().unwrap_or(ty);
            return Ok(self.match_rust_path_reference(&r.clone().naming(path,"references"))?.map(|c|c.node));
        }
        let ref2 = r.clone().naming(ty, "references");
        let hit = if ty.contains('.') {
            self.resolve_via_import_member(&ref2)?
        } else {
            self.resolve_via_import(&ref2)?
        };
        let mut owner_id = hit.map(|c| c.node.id.clone());
        if owner_id.is_none() {
            let spec = b.target_spec.as_deref().unwrap_or(ty);
            if let Some(c) = self.resolve_jvm_import(&r.clone().naming(spec, "imports"))? {
                owner_id = Some(c.node.id.clone());
            }
        }
        let owner = self.node_by_opt_id(owner_id.as_deref())?;
        let (true, Some(spec)) = (r.language == "php", &b.target_spec) else {
            return Ok(owner);
        };
        let stripped = spec.strip_prefix('\\').unwrap_or(spec);
        let qualified = match stripped.rfind('\\') {
            Some(pos) if pos + 1 < stripped.len() => {
                format!("{}::{}", &stripped[..pos], &stripped[pos + 1..])
            }
            _ => stripped.to_string(),
        };
        let owners: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&qualified)?
            .iter()
            .filter(|n| {
                n.language == "php" && matches!(n.kind.as_str(), "class" | "interface" | "trait")
            })
            .cloned()
            .collect();
        Ok(if owners.len() == 1 { Some(owners[0].clone()) } else { None })
    }

    /// An unbound type name's owner: the single visible candidate, preferring
    /// the ref's own file, else its package (Go directory, PHP namespace,
    /// JVM package or wildcard import).
    fn visible_unique_type(&mut self, ty: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let raw = if ty.contains("::") {
            self.nodes_by_qualified_name(ty)?
        } else {
            self.nodes_by_name(ty)?
        };
        let mut candidates: Vec<Arc<KNode>> = Vec::new();
        for n in raw.iter() {
            if !TYPE_OWNER_KINDS.contains(&n.kind.as_str()) || n.language != r.language {
                continue;
            }
            if !self.is_visible_across_files(n, r)? {
                continue;
            }
            candidates.push(n.clone());
        }
        let local: Vec<Arc<KNode>> = candidates
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        let namespace = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .find(|n| n.kind == "namespace")
            .map(|n| n.qualified_name.clone());
        let mut packages: Vec<String> = Vec::new();
        if r.language == "java" || r.language == "kotlin" {
            packages = self
                .import_mappings(&r.file_path)?
                .iter()
                .filter(|i| i.is_namespace && i.source.ends_with(".*"))
                .map(|i| i.source[..i.source.len() - 2].to_string())
                .collect();
            if let Some(ns) = &namespace {
                packages.insert(0, ns.clone());
            }
        }
        let package_candidates: Vec<Arc<KNode>> = candidates
            .into_iter()
            .filter(|n| {
                if r.language == "python" {
                    return false;
                }
                if r.language == "go" {
                    return pos_dirname(&n.file_path) == pos_dirname(&r.file_path);
                }
                if r.language == "php" {
                    return n.qualified_name
                        == match &namespace {
                            Some(ns) => format!("{}::{}", ns, ty),
                            None => ty.to_string(),
                        };
                }
                if r.language == "java" || r.language == "kotlin" {
                    if namespace.is_none() && n.qualified_name == ty {
                        return true;
                    }
                    return packages
                        .iter()
                        .any(|pkg| n.qualified_name == format!("{}::{}", pkg, ty));
                }
                true
            })
            .collect();
        let mut visible = if !local.is_empty() {
            local
        } else {
            package_candidates
        };
        // Ruby finds a constant lexically, from the class or module around
        // the site outward.
        if r.language == "ruby" && visible.len() > 1 {
            if let Some(qn) = self.ruby_lexical_constant(ty, r)? {
                visible.retain(|n| n.qualified_name == qn);
            }
        }
        Ok(if visible.len() == 1 { Some(visible[0].clone()) } else { None })
    }

    /// matchBoundTypeMember — owner's own `QName::method` member. A miss is
    /// not a refusal: the caller next walks the supertype edges.
    pub(super) fn match_bound_type_member(
        &mut self,
        ty: &str,
        method: &str,
        site: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let owner = match self.resolve_bound_type(ty, site, 0)? {
            Some(o) => o,
            None => return Ok(None),
        };
        if owner.language == "python" && self.python_type_assigns(&owner, method)? { return Ok(None); }
        let member = self.own_bound_member(&owner, method, site)?;
        if let Some(m) = member {
            return Ok(Some(bound_member_cand(m)));
        }
        // A db that may lack supertype edges (a pool worker's snapshot taken
        // mid-prerequisite phase — unreachable since the snapshot refreshes
        // before the first calls batch) gets no walk: a miss, never a guess.
        if !self.supertypes_complete {
            return Ok(None);
        }
        // An inherited class method is the one that runs, so the superclass
        // chain goes first; an interface on the owner would otherwise answer
        // with its bodiless declaration.
        if owner.language == "python" && is_class_like(&owner.kind) {
            // Python looks a method up along the C3 linearization, which
            // reaches a shared base only after every class deriving it.
            let Some(mro) = self.python_mro(&owner, 0)? else { return Ok(None) };
            for class in mro.iter().skip(1) {
                if self.python_type_assigns(class, method)? { return Ok(None); }
                if let Some(m) = self.own_bound_member(class, method, site)? {
                    return Ok(Some(bound_member_cand(m)));
                }
            }
            return Ok(None);
        }
        if is_class_like(&owner.kind) {
            // Ruby and PHP look in a class's mixins (Ruby included modules,
            // PHP traits) before its parent class.
            let mixins_first = matches!(owner.language.as_str(), "ruby" | "php");
            let mut mixins_seen: HashSet<String> = HashSet::new();
            let mut class = owner.clone();
            let mut chain: HashSet<String> = HashSet::from([owner.id.clone()]);
            loop {
                if mixins_first {
                    if let Some(m) = self.mixin_member(&class, method, site, &mut mixins_seen, 0)? {
                        return Ok(Some(bound_member_cand(m)));
                    }
                }
                let mut parent = None;
                for t in self.outgoing_edge_targets(&class.id, &["extends"])? {
                    if let Some(n) = self.node_by_id(&t)?.filter(|n| is_class_like(&n.kind)) {
                        parent = Some(n);
                        break;
                    }
                }
                let Some(parent) = parent.filter(|p| chain.insert(p.id.clone())) else { break };
                if let Some(m) = self.own_bound_member(&parent, method, site)? {
                    return Ok(Some(bound_member_cand(m)));
                }
                class = parent;
            }
        }
        // matchBoundTypeMember's supertype BFS: `getSupertypeNodes` (the
        // type's outgoing implements/extends edges, any target kind), each
        // node visited once, the owner's own-member rule at every step.
        let mut pending: VecDeque<Arc<KNode>> = self.supertype_nodes(&owner.id)?.into();
        let mut seen: HashSet<String> = HashSet::from([owner.id.clone()]);
        while let Some(type_node) = pending.pop_front() {
            if !seen.insert(type_node.id.clone()) {
                continue;
            }
            if let Some(m) = self.own_bound_member(&type_node, method, site)? {
                return Ok(Some(bound_member_cand(m)));
            }
            pending.extend(self.supertype_nodes(&type_node.id)?);
        }
        Ok(None)
    }

    /// `method` on one of `class`'s mixins or theirs, depth first.
    fn mixin_member(
        &mut self,
        class: &Arc<KNode>,
        method: &str,
        site: &ResolveRefIn,
        seen: &mut HashSet<String>,
        depth: u32,
    ) -> Res<Option<Arc<KNode>>> {
        if depth > 8 {
            return Ok(None);
        }
        for mixin in self.mixin_nodes(class)? {
            if !seen.insert(mixin.id.clone()) {
                continue;
            }
            if let Some(m) = self.own_bound_member(&mixin, method, site)? {
                return Ok(Some(m));
            }
            if let Some(m) = self.mixin_member(&mixin, method, site, seen, depth + 1)? {
                return Ok(Some(m));
            }
        }
        Ok(None)
    }

    /// A Ruby or PHP type's mixins in method lookup order: Ruby modules
    /// last-included first (`include A, B` keeps `A` first), PHP traits.
    /// Interfaces are not mixins; other languages have none.
    fn mixin_nodes(&mut self, class: &KNode) -> Res<Vec<Arc<KNode>>> {
        let mixin_kind = match class.language.as_str() {
            "ruby" => "module",
            "php" => "trait",
            _ => return Ok(Vec::new()),
        };
        let mut rows: Vec<(String, i64)> = {
            let conn = self.conn()?;
            let mut stmt = conn
                .prepare("SELECT target, line FROM edges WHERE source = ?1 AND kind = 'implements' ORDER BY id")
                .map_err(|e| Error::from_reason(e.to_string()))?;
            let rows = stmt
                .query_map([&class.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))))
                .map_err(|e| Error::from_reason(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, rusqlite::Error>>()
                .map_err(|e| Error::from_reason(e.to_string()))?
        };
        if mixin_kind == "module" {
            rows.sort_by_key(|row| std::cmp::Reverse(row.1));
        }
        let mut out = Vec::with_capacity(rows.len());
        for (target, _) in rows {
            if let Some(n) = self.node_by_id(&target)?.filter(|n| n.kind == mixin_kind) {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// The C3 method resolution order of a Python class over its indexed
    /// `extends` edges (in base-list order); None when the bases admit no
    /// consistent order, which Python itself rejects.
    pub(super) fn python_mro(&mut self, class: &Arc<KNode>, depth: u32) -> Res<Option<Vec<Arc<KNode>>>> {
        if depth > 16 {
            return Ok(None);
        }
        let mut bases: Vec<Arc<KNode>> = Vec::new();
        for t in self.outgoing_edge_targets(&class.id, &["extends"])? {
            if let Some(n) = self.node_by_id(&t)?.filter(|n| is_class_like(&n.kind)) {
                if !bases.iter().any(|b| b.id == n.id) {
                    bases.push(n);
                }
            }
        }
        let mut seqs: Vec<Vec<Arc<KNode>>> = Vec::with_capacity(bases.len() + 1);
        for b in &bases {
            let Some(m) = self.python_mro(b, depth + 1)? else { return Ok(None) };
            seqs.push(m);
        }
        seqs.push(bases);
        let mut out = vec![class.clone()];
        loop {
            seqs.retain(|q| !q.is_empty());
            if seqs.is_empty() {
                return Ok(Some(out));
            }
            // The first head that appears in no other sequence's tail.
            let Some(head) = seqs
                .iter()
                .map(|q| q[0].clone())
                .find(|h| !seqs.iter().any(|q| q[1..].iter().any(|n| n.id == h.id)))
            else {
                return Ok(None);
            };
            for q in seqs.iter_mut() {
                if q[0].id == head.id {
                    q.remove(0);
                }
            }
            out.push(head);
        }
    }

    /// `super().<name>()`, which the extractor records as `super().<name>`:
    /// the method the class around the call inherits, the first one along
    /// its C3 linearization after the class itself, or after the class
    /// `super(Cls, self)` names. None unless the call runs in a function of
    /// that class with a first parameter, which the explicit form must pass
    /// (Python raises in the class body, a lambda, a `@staticmethod`, a
    /// generator expression the zero-argument form runs in, or a function
    /// without one); when the call sees `super` rebound, or sees the name
    /// the explicit form passes bound to anything but that class; when a
    /// class in the order has a metaclass that may reorder it; when a class
    /// up to the one that declares the method lists a base the index does
    /// not hold, which may come first at runtime; or when a class on the way
    /// binds the name in its body other than by its `def`, or makes it a
    /// property, where only runtime could tell.
    pub(super) fn python_super_method(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(name) = r.reference_name.strip_prefix("super().").filter(|n| re!(r"^[A-Za-z_]\w*$").is_match(n)) else {
            return Ok(None);
        };
        if !self.supertypes_complete {
            return Ok(None);
        }
        let Some(cls) = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| n.kind == "class" && n.start_line <= r.line && n.end_line >= r.line)
            .max_by_key(|n| n.start_line)
            .cloned()
        else {
            return Ok(None);
        };
        let Some(source) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let at = (r.line - 1).max(0) as usize;
        let Some(line) = source.get(at) else {
            return Ok(None);
        };
        let col = super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len());
        let Some(call) = re!(r"^super\s*\(\s*(?:\)|([A-Za-z_]\w*)\s*,\s*([A-Za-z_]\w*)\s*\))").captures(&line[col..]) else {
            return Ok(None);
        };
        let named = call.get(1).map(|m| m.as_str());
        let file = source.python_file();
        let Some((def, receiver)) = python_method_frame(file, at, col, named.is_none()) else {
            return Ok(None);
        };
        if call.get(2).is_some_and(|m| m.as_str() != receiver) {
            return Ok(None);
        }
        let fns = file.functions_around(Some(def));
        if !file.bindings(&fns, "super", true)?.is_empty() {
            return Ok(None);
        }
        let Some(mro) = self.python_mro(&cls, 0)? else { return Ok(None) };
        let start = match named {
            Some(n) if n != cls.name => {
                let mut at = mro.iter().enumerate().filter(|(_, c)| c.name == n).map(|(i, _)| i);
                let (Some(start), None) = (at.next(), at.next()) else {
                    return Ok(None);
                };
                start
            }
            _ => 0,
        };
        // `super(Cls, self)` looks `Cls` up as it runs: its one binding the
        // call can see must be the `class` statement of the class it names.
        if let Some(n) = named {
            let target = &mro[start];
            let Some(header) = file.class_header(target).filter(|_| target.file_path == r.file_path) else {
                return Ok(None);
            };
            if file.bindings(&fns, n, true)? != [header] {
                return Ok(None);
            }
        }
        for class in &mro {
            if !self.python_metaclass_keeps_order(class)? {
                return Ok(None);
            }
        }
        // C3 puts a class's bases after it, so only the bases of classes up
        // to the hit can come before the hit.
        for class in &mro[..=start] {
            if !self.python_bases_indexed(class, &["object"])? {
                return Ok(None);
            }
        }
        for class in mro.iter().skip(start + 1) {
            if self.python_class_body_binds(class, name)? {
                return Ok(None);
            }
            if let Some(m) = self.own_bound_member(class, name, r)? {
                if self.python_property(&m)? {
                    return Ok(None);
                }
                return Ok(Some(bound_member_cand(m)));
            }
            if !self.python_bases_indexed(class, &["object"])? {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// Whether every base a Python class header lists, `known` external
    /// bases aside, is one of its indexed `extends` targets.
    fn python_bases_indexed(&mut self, class: &KNode, known: &[&str]) -> Res<bool> {
        let Some(lines) = self.read_file(&class.file_path) else {
            return Ok(false);
        };
        let from = (class.start_line - 1).max(0) as usize;
        let to = (class.end_line.max(0) as usize).min(lines.len());
        let Some(bases) = super::overloads_upstream::python_bases(&lines[from.min(to)..to], &class.name) else {
            return Ok(false);
        };
        let mut indexed: Vec<String> = Vec::new();
        for t in self.outgoing_edge_targets(&class.id, &["extends"])? {
            if self.node_by_id(&t)?.is_some_and(|n| is_class_like(&n.kind)) && !indexed.contains(&t) {
                indexed.push(t);
            }
        }
        let listed = bases.iter().filter(|b| !known.contains(&b.rsplit('.').next().unwrap_or(b))).count();
        Ok(listed == indexed.len())
    }

    /// Whether a Python class's metaclass leaves its C3 order alone: none
    /// declared; `type`, or `ABCMeta` imported from `abc`, where the class
    /// statement sees no other binding of the name; or indexed metaclasses
    /// none of which, nor any of their bases, defines `mro`. A metaclass
    /// written as any other expression, or passed in `**` keywords, may.
    fn python_metaclass_keeps_order(&mut self, class: &KNode) -> Res<bool> {
        let Some(source) = self.read_file(&class.file_path) else {
            return Ok(false);
        };
        let file = source.python_file();
        let Some((h, header)) = file.class_header(class).and_then(|h| file.stmt(h)) else {
            return Ok(false);
        };
        if header.contains("**") {
            return Ok(false);
        }
        let Some(kw) = re!(r"\bmetaclass\s*=").find(header) else {
            return Ok(true);
        };
        let Some(m) = re!(r"^\s*([A-Za-z_][\w.]*)\s*[,)]").captures(&header[kw.end()..]) else {
            return Ok(false);
        };
        let fns = file.functions_around(file.scope[*h]);
        // Every binding of `name` the class statement sees is `form`.
        let only = |name: &str, form: &dyn Fn(&str) -> bool| -> Res<bool> {
            let at = file.bindings(&fns, name, true)?;
            Ok(!at.is_empty() && at.iter().all(|&i| file.stmt(i).is_some_and(|(_, s)| form(s))))
        };
        let keeps = match &m[1] {
            "type" => file.bindings(&fns, "type", true)?.is_empty(),
            "ABCMeta" => only("ABCMeta", &|s| {
                re!(r"^\s*from\s+abc\s+import\b").is_match(s) && !re!(r"\bas\s+ABCMeta\b|\bABCMeta\s+as\b").is_match(s)
            })?,
            "abc.ABCMeta" => only("abc", &|s| re!(r"^\s*import\s+abc\s*$").is_match(s))?,
            _ => false,
        };
        if keeps {
            return Ok(true);
        }
        let meta = m[1].rsplit('.').next().unwrap_or("").to_string();
        let metas: Vec<Arc<KNode>> =
            self.nodes_by_name(&meta)?.iter().filter(|n| n.kind == "class" && n.language == "python").cloned().collect();
        if metas.is_empty() {
            return Ok(false);
        }
        for meta in &metas {
            let Some(order) = self.python_mro(meta, 0)? else { return Ok(false) };
            for c in &order {
                if !self.nodes_by_qualified_name(&format!("{}::mro", c.qualified_name))?.is_empty()
                    || !self.python_bases_indexed(c, &["object", "type", "ABCMeta"])?
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Whether a Python class body binds `name` other than by a `def`: an
    /// assignment (tuple targets included), an import, a loop or `with`
    /// target, a nested class. Statements of its methods and nested classes
    /// don't count.
    fn python_class_body_binds(&mut self, class: &KNode, name: &str) -> Res<bool> {
        let Some(source) = self.read_file(&class.file_path) else {
            return Ok(true);
        };
        let file = source.python_file();
        let Some(h) = file.class_header(class) else {
            return Ok(true);
        };
        let binds = python_binder(name, true)?;
        Ok(file.stmts.iter().any(|(i, s)| file.scope[*i] == Some(h) && !re!(r"^\s*(?:async\s+)?def\b").is_match(s) && binds(s)))
    }

    /// Whether a Python method is a property (`@property`, `@cached_property`,
    /// `@<name>.setter`), an attribute read rather than the method called.
    fn python_property(&mut self, method: &KNode) -> Res<bool> {
        let Some(lines) = self.read_file(&method.file_path) else {
            return Ok(true);
        };
        let def = Self::cached_regex(&format!(r"^\s*(?:async\s+)?def\s+{}\b", regex::escape(&method.name)))?;
        let from = (method.start_line - 1).max(0) as usize;
        let Some(d) = (from..(from + 32).min(lines.len())).find(|&i| def.is_match(&lines[i])) else {
            return Ok(true);
        };
        let code = super::member_fn_ref::python_code_lines(&lines, d.saturating_sub(32), d);
        for (start, c) in code.iter().rev() {
            let c = c.trim();
            if c.is_empty() || !*start {
                continue;
            }
            if !c.starts_with('@') {
                break;
            }
            if re!(r"^@\s*(?:[\w.]*\.)?(?:property|cached_property)\b|^@\s*\w+\.(?:setter|getter|deleter)\b").is_match(c) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// matchBoundTypeMember's per-type member rule: `Owner::method`, a
    /// method (or a C/C++ callable field) of the site's language family,
    /// declared in the owner's file (Go: its package directory; C++: anywhere),
    /// the lone candidate or the owner-file one.
    fn own_bound_member(&mut self, owner: &KNode, method: &str, site: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let members: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, method))?
            .iter()
            .filter(|n| {
                (n.kind == "method"
                    || (n.kind == "field"
                        && (site.language == "c" || site.language == "cpp")))
                    && same_language_family(&n.language, &site.language)
                    && (n.file_path == owner.file_path
                        || (site.language == "go"
                            && pos_dirname(&n.file_path) == pos_dirname(&owner.file_path))
                        || site.language == "cpp")
            })
            .cloned()
            .collect();
        Ok(if members.len() == 1 {
            members.into_iter().next()
        } else {
            members.into_iter().find(|n| n.file_path == owner.file_path)
        })
    }

    /// getSupertypeNodes: the nodes a type's `implements`/`extends` edges
    /// point at, in edge-table order (the same `WHERE source = ? AND kind
    /// IN (…)` scan TS runs, so the order matches).
    pub(super) fn supertype_nodes(&mut self, id: &str) -> Res<Vec<Arc<KNode>>> {
        let targets = self.outgoing_edge_targets(id, &["implements", "extends"])?;
        let mut out = Vec::with_capacity(targets.len());
        for t in targets {
            if let Some(n) = self.node_by_id(&t)? {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// context.getSupertypes: the distinct names (first-seen order) of what
    /// every same-language, supertype-bearing node named `type_name` extends
    /// or implements, excluding the name itself. `from_site`: the name was
    /// written at `r`, so a Java type must also be one `r` can see (its
    /// file, its package, or an import) — an unrelated same-named class
    /// elsewhere contributes nothing. PHP names supertypes by their
    /// qualified names, and `qualified` looks `type_name` up as one: a
    /// class's parent is the one its declaration resolved to, never every
    /// same-named class in other namespaces.
    fn supertype_names(&mut self, type_name: &str, r: &ResolveRefIn, from_site: bool, qualified: bool) -> Res<Vec<String>> {
        let named = if qualified { self.nodes_by_qualified_name(type_name)? } else { self.nodes_by_name(type_name)? };
        let mut type_nodes: Vec<Arc<KNode>> = named
            .iter()
            // Scala singletons can inherit members even though they cannot be parents.
            .filter(|n| {
                n.language == r.language
                    && (is_supertype_bearing_kind(&n.kind) || (n.language == "scala" && n.kind == "module"))
            })
            .cloned()
            .collect();
        // A name the site binds to one type keeps that type's declarations
        // (partial or extended parts) and drops a same-named type in another
        // namespace, package or module.
        if from_site && type_nodes.len() > 1 {
            if let Some(owner) = self.resolve_bound_type(type_name, r, 0)? {
                if type_nodes.iter().any(|n| n.id == owner.id) {
                    type_nodes.retain(|n| same_declared_type(n, &owner));
                }
            }
        }
        let mut names: Vec<String> = Vec::new();
        for tn in type_nodes {
            if from_site && r.language == "java" && !self.java_type_visible(&tn, r)? {
                continue;
            }
            // Ruby and PHP look in mixins before the parent class.
            let mut targets = self.mixin_nodes(&tn)?;
            for n in self.supertype_nodes(&tn.id)? {
                if !targets.iter().any(|t| t.id == n.id) {
                    targets.push(n);
                }
            }
            for target in targets {
                // PHP, and Ruby from a qualified start, name supertypes
                // by qualified name.
                let name = if r.language == "php" || (qualified && r.language == "ruby") {
                    &target.qualified_name
                } else {
                    &target.name
                };
                if !name.is_empty() && name != type_name && !names.contains(name) {
                    names.push(name.clone());
                }
            }
            // A Swift conformance to a type the project only extends (SwiftUI's
            // `View`) resolves to nothing — the extension is not the type — yet
            // the members its extensions add are still the conformer's.
            if tn.language == "swift" {
                for name in self.swift_extended_conformances(&tn)?.iter() {
                    if name != type_name && !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
        }
        Ok(names)
    }

    /// swiftExtendedConformances (swift-type-visibility.ts): the types a
    /// Swift type's own declaration conforms to that the project only
    /// extends — `View` for `struct HomeView: View` when `extension View {}`
    /// is all the index holds of it. Read from the declaration head once per
    /// node: the resolved edges it would otherwise come from do not exist.
    fn swift_extended_conformances(&mut self, node: &KNode) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.swift_conformance_memo.get(&node.id) {
            return Ok(hit.clone());
        }
        let mut out = Vec::new();
        for name in self.swift_declared_supertypes(node) {
            let typed: Vec<Arc<KNode>> = self
                .nodes_by_name(&name)?
                .iter()
                .filter(|n| n.language == "swift" && is_swift_type_kind(&n.kind))
                .cloned()
                .collect();
            if typed.is_empty() {
                continue;
            }
            let mut only_extended = true;
            for n in &typed {
                if !self.is_swift_extension(n) {
                    only_extended = false;
                    break;
                }
            }
            if only_extended {
                out.push(name);
            }
        }
        let out = Rc::new(out);
        self.swift_conformance_memo.insert(node.id.clone(), out.clone());
        Ok(out)
    }

    /// The supertypes a Swift type node's own declaration names: `View`,
    /// `Sendable` for `struct HomeView: View, Sendable {`.
    fn swift_declared_supertypes(&mut self, node: &KNode) -> Vec<String> {
        let head = self.swift_declaration_head(node, 6);
        let Some(clause) = swift_inheritance_clause_re().captures(&head).and_then(|c| c.get(1)) else {
            return Vec::new();
        };
        clause
            .as_str()
            .split(',')
            .filter_map(|part| {
                let bare = swift_generic_args_re().replace_all(part, "");
                let name = bare.trim().rsplit('.').next().unwrap_or("").trim().to_string();
                swift_type_name_re().is_match(&name).then_some(name)
            })
            .collect()
    }

    /// isSwiftExtension (swift-type-visibility.ts): extraction classifies an
    /// `extension X {}` as a class, so the declaration keyword decides.
    fn is_swift_extension(&mut self, node: &KNode) -> bool {
        if node.language != "swift" || node.kind != "class" {
            return false;
        }
        if let Some(&hit) = self.swift_extension_memo.get(&node.id) {
            return hit;
        }
        let head = self.swift_declaration_head(node, 4);
        let extension = swift_declaration_keyword_re()
            .captures(&head)
            .and_then(|c| c.get(1))
            .is_some_and(|m| m.as_str() == "extension");
        self.swift_extension_memo.insert(node.id.clone(), extension);
        extension
    }

    /// A declaration's first lines from its start column, joined, with string
    /// literals emptied (an attribute's `message: "use class Foo"` is not a
    /// keyword).
    fn swift_declaration_head(&mut self, node: &KNode, extra_lines: i64) -> String {
        let Some(lines) = self.read_file(&node.file_path) else { return String::new() };
        let first = lines.get((node.start_line - 1).max(0) as usize).map(|s| s.as_str()).unwrap_or("");
        let mut head = first.get(node.start_column.max(0) as usize..).unwrap_or(first).to_string();
        let end = node.end_line.min(node.start_line + extra_lines).max(node.start_line);
        for i in node.start_line..end {
            head.push(' ');
            head.push_str(lines.get(i as usize).map(|s| s.as_str()).unwrap_or(""));
        }
        swift_string_literal_re().replace_all(&head, "\"\"").into_owned()
    }

    /// resolveSwiftTypePathCall (swift-type-visibility.ts):
    /// `API.PackageController.GetRoute.query(on:)` lands on the `query` of the
    /// type the path names, and `API.PackageController.Model(name:)` on that
    /// nested type, never on the member's name alone: in a Vapor app every
    /// route's type has a `query`. A path the call's own namespace lets it
    /// shorten (`PackageController.GetRoute` inside `extension API`) or a
    /// module qualifier (`Vapor.HTTPStatus`) still fits. No type on the path,
    /// or two, leaves the call unresolved.
    pub(super) fn resolve_swift_type_path_call(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let name = r.reference_name.as_str();
        let Some(dot) = name.rfind('.') else { return Ok(None) };
        let member = &name[dot + 1..];
        let path = name[..dot].split('.').collect::<Vec<_>>().join("::");
        // `API.PackageController.Model(name:)` constructs a nested type.
        let mut fit = if member.as_bytes().first().is_some_and(|b| b.is_ascii_uppercase()) {
            self.swift_members_on_path(member, &path, true)?
        } else {
            Vec::new()
        };
        if fit.is_empty() {
            fit = self.swift_members_on_path(member, &path, false)?;
        }
        // The Composable Architecture's `@Reducer enum Path { case detail(Detail) }`
        // generates `Path.State` and `Path.Action` with the same cases, so
        // `Path.State.detail(…)` constructs the case written in `Path`.
        if fit.is_empty() {
            let reducer = path
                .strip_suffix("::State")
                .or_else(|| path.strip_suffix("::Action"))
                .filter(|p| !p.is_empty())
                .map(str::to_string);
            if let Some(reducer) = reducer {
                let suffix = format!("::{reducer}");
                for n in self.nodes_by_name(member)?.iter() {
                    if n.language != "swift" || n.kind != "enum_member" {
                        continue;
                    }
                    let fits = match self.swift_owner_path(n)? {
                        Some(owner) => owner == reducer || owner.ends_with(&suffix),
                        None => false,
                    };
                    if fits && self.is_swift_reducer_enum(n)? {
                        fit.push(n.clone());
                    }
                }
            }
        }
        if fit.is_empty() {
            return Ok(None);
        }
        let mut owners: HashSet<Option<String>> = HashSet::new();
        for n in &fit {
            owners.insert(self.swift_owner_path(n)?);
        }
        if owners.len() > 1 {
            // A shortened path is looked up from the call's namespace
            // outward: `PackageController.GetRoute` inside `extension API`
            // is `API.PackageController.GetRoute`, not `Other.…`'s.
            fit = self.swift_nearest_namespace_fit(fit, &path, r)?;
            if fit.is_empty() {
                return Ok(None);
            }
        }
        // Twin declarations of one path in several files (a type per
        // module): the call's own file, else the nearest by directory, as
        // the type gate picks a type (swift-type-visibility.ts declarationFor).
        // A tie is left unresolved.
        if fit.iter().any(|n| n.file_path != fit[0].file_path) {
            if fit.iter().any(|n| n.file_path == r.file_path) {
                fit.retain(|n| n.file_path == r.file_path);
            } else {
                let dirs: Vec<&str> = r.file_path.split('/').collect();
                let dirs = &dirs[..dirs.len() - 1];
                let near = fit.iter().map(|n| shared_dir_prefix(dirs, &n.file_path)).max().unwrap_or(0);
                fit.retain(|n| shared_dir_prefix(dirs, &n.file_path) == near);
            }
            if fit.iter().any(|n| n.file_path != fit[0].file_path) {
                return Ok(None);
            }
        }
        // Of one type's overloads in one file, the first declared.
        let target = fit
            .into_iter()
            .reduce(|a, b| {
                if a.file_path < b.file_path || (a.file_path == b.file_path && a.start_line <= b.start_line) {
                    a
                } else {
                    b
                }
            })
            .expect("non-empty");
        Ok(Some(KCand { node: target, confidence: 0.9, resolved_by: "qualified-name" }))
    }

    /// Of `fit`'s members on several owners, those whose owner is `path`
    /// under the innermost enclosing namespace of the call that has one;
    /// empty when no enclosing namespace settles it on a single owner.
    fn swift_nearest_namespace_fit(
        &mut self,
        fit: Vec<Arc<KNode>>,
        path: &str,
        r: &ResolveRefIn,
    ) -> Res<Vec<Arc<KNode>>> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(Vec::new()) };
        let scope = if is_swift_type_kind(&caller.kind) {
            let parent = self.swift_owner_path(&caller)?;
            Some(parent.map_or_else(|| caller.name.clone(), |p| format!("{p}::{}", caller.name)))
        } else {
            self.swift_owner_path(&caller)?
        };
        let Some(scope) = scope else { return Ok(Vec::new()) };
        let segments: Vec<&str> = scope.split("::").collect();
        for len in (1..=segments.len()).rev() {
            let want = format!("{}::{path}", segments[..len].join("::"));
            let mut hits: Vec<Arc<KNode>> = Vec::new();
            for n in &fit {
                if self.swift_owner_path(n)?.as_deref() == Some(want.as_str()) {
                    hits.push(n.clone());
                }
            }
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        Ok(Vec::new())
    }

    /// The Swift `member`s declared on `path` (types when `types`, else a
    /// method, function or enum case): the exact owner path first, else one
    /// the path shortens or module-qualifies.
    fn swift_members_on_path(&mut self, member: &str, path: &str, types: bool) -> Res<Vec<Arc<KNode>>> {
        let mut exact: Vec<Arc<KNode>> = Vec::new();
        let mut shortened: Vec<Arc<KNode>> = Vec::new();
        let suffix = format!("::{path}");
        for n in self.nodes_by_name(member)?.iter() {
            if n.language != "swift" {
                continue;
            }
            let kind_fits = if types {
                is_swift_type_kind(&n.kind)
            } else {
                matches!(n.kind.as_str(), "method" | "function" | "enum_member")
            };
            if !kind_fits || (types && self.is_swift_extension(n)) {
                continue;
            }
            let Some(owner) = self.swift_owner_path(n)? else { continue };
            if owner == path {
                exact.push(n.clone());
            } else if owner.ends_with(&suffix) || self.swift_module_qualified(&owner, path)? {
                shortened.push(n.clone());
            }
        }
        Ok(if exact.is_empty() { shortened } else { exact })
    }

    /// Is this case declared in a `@Reducer enum`?
    fn is_swift_reducer_enum(&mut self, member: &KNode) -> Res<bool> {
        let mut owner: Option<Arc<KNode>> = None;
        for t in self.nodes_in_file(&member.file_path)?.iter() {
            if t.kind == "enum"
                && t.start_line <= member.start_line
                && t.end_line >= member.end_line
                && owner.as_ref().is_none_or(|o| t.start_line >= o.start_line)
            {
                owner = Some(t.clone());
            }
        }
        let Some(owner) = owner else { return Ok(false) };
        let Some(lines) = self.read_file(&owner.file_path) else { return Ok(false) };
        // The node starts at its attributes; `@Reducer` may also sit on the line above.
        let from = ((owner.start_line - 2).max(0) as usize).min(lines.len());
        let to = ((owner.start_line + 1).max(0) as usize).clamp(from, lines.len());
        let text = lines[from..to].join(" ");
        Ok(re!(r"@Reducer\b").is_match(&text))
    }

    /// `Vapor::HTTPStatus` for a type `HTTPStatus`: a qualifier that names no
    /// project type is a module.
    fn swift_module_qualified(&mut self, owner: &str, path: &str) -> Res<bool> {
        let Some(head) = path.strip_suffix(owner).and_then(|h| h.strip_suffix("::")) else {
            return Ok(false);
        };
        if head.contains("::") {
            return Ok(false);
        }
        Ok(!self
            .nodes_by_name(head)?
            .iter()
            .any(|n| n.language == "swift" && is_swift_type_kind(&n.kind)))
    }

    /// ownerPath (swift-type-visibility.ts): the full path of the type a
    /// member is declared on. Its qualified name starts at the outermost
    /// declaration in its file, and an extension is named by its last
    /// segment (the members of `extension API.PackageController { enum
    /// GetRoute { … } }` are `PackageController::GetRoute::…`), so the
    /// outermost extension's written path is read back from its line. None
    /// for a top-level function.
    fn swift_owner_path(&mut self, member: &KNode) -> Res<Option<String>> {
        if let Some(hit) = self.swift_owner_memo.get(&member.id) {
            return Ok(hit.clone());
        }
        let mut path: Option<String> = member.qualified_name.rfind("::").map(|cut| member.qualified_name[..cut].to_string());
        if let Some(p) = path.clone() {
            let first = p.split("::").next().unwrap_or("").to_string();
            let mut outer: Option<Arc<KNode>> = None;
            for t in self.nodes_in_file(&member.file_path)?.iter() {
                if t.name == first
                    && is_swift_type_kind(&t.kind)
                    && t.start_line <= member.start_line
                    && t.end_line >= member.end_line
                    && outer.as_ref().is_none_or(|o| t.start_line < o.start_line)
                {
                    outer = Some(t.clone());
                }
            }
            if let Some(outer) = outer {
                if let Some(extended) = self.swift_extended_path(&outer) {
                    path = Some(format!("{extended}{}", &p[first.len()..]));
                }
            }
        }
        self.swift_owner_memo.insert(member.id.clone(), path.clone());
        Ok(path)
    }

    /// extendedPath (swift-type-visibility.ts): `extension API.PackageController {`
    /// → `API::PackageController`; None for anything but an extension of a
    /// nested type.
    fn swift_extended_path(&mut self, node: &KNode) -> Option<String> {
        if !self.is_swift_extension(node) {
            return None;
        }
        let head = self.swift_declaration_head(node, 3);
        let written = re!(r"\bextension\s+((?:[A-Za-z_][A-Za-z0-9_]*\s*\.\s*)+[A-Za-z_][A-Za-z0-9_]*)")
            .captures(&head)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())?;
        let compact: String = written.chars().filter(|c| !c.is_whitespace()).collect();
        Some(compact.split('.').collect::<Vec<_>>().join("::"))
    }

    /// A Java type `r` can name by its simple name: declared in `r`'s file
    /// or package directory, or imported by name or by package wildcard.
    fn java_type_visible(&mut self, ty: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if pos_dirname(&ty.file_path) == pos_dirname(&r.file_path) {
            return Ok(true);
        }
        let fqn = ty.qualified_name.replace("::", ".");
        let package = fqn.rsplit_once('.').map_or("", |(p, _)| p);
        Ok(self
            .import_mappings(&r.file_path)?
            .iter()
            .any(|i| i.source == fqn || (!package.is_empty() && i.source == format!("{package}.*"))))
    }

    /// resolveMethodOnType — `typeName::methodName` qualified-name suffix
    /// match with preferred-FQN and call-site disambiguation. Zero direct
    /// matches is a PUNT: TS falls into the live-edge supertype walk.
    pub(super) fn resolve_method_on_type(
        &mut self,
        type_name: &str,
        method: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
        preferred_fqn: Option<&str>,
    ) -> Res<Option<KCand>> {
        self.resolve_method_on_type_at(type_name, method, r, confidence, resolved_by, preferred_fqn, 0, false)
    }

    /// resolve_method_on_type for a PHP or Ruby type named by its fully
    /// qualified name (`Lib::Store`, or `Sub` at the top level): only that
    /// type's member, or its supertypes' along their qualified names.
    pub(super) fn resolve_method_on_qualified_type(
        &mut self,
        qualified_name: &str,
        method: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
    ) -> Res<Option<KCand>> {
        self.resolve_method_on_type_at(qualified_name, method, r, confidence, resolved_by, None, 0, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_method_on_type_at(
        &mut self,
        type_name: &str,
        method: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
        preferred_fqn: Option<&str>,
        depth: u32,
        qualified: bool,
    ) -> Res<Option<KCand>> {
        let want = format!("{}::{}", type_name, method);
        // A qualified name means that one type: the global `Mid` never
        // matches `Other\Mid`'s member.
        let exact = qualified;
        let matches: Vec<Arc<KNode>> = self
            .nodes_by_name(method)?
            .iter()
            .filter(|m| {
                m.kind == "method"
                    && same_language_family(&m.language, &r.language)
                    && (m.qualified_name == want
                        || (!exact && m.qualified_name.ends_with(&format!("::{}", want))))
            })
            .cloned()
            .collect();
        if matches.is_empty() {
            // No walk over a db that may lack supertype edges (see
            // match_bound_type_member): a miss, never a guess.
            if !self.supertypes_complete {
                return Ok(None);
            }
            // The conformance fallback: the method may live on a supertype
            // (transitively, depth-capped), still validated by name.
            if depth < 4 {
                // PHP supertypes, and Ruby ones from a qualified start, come
                // back by qualified name.
                let php = r.language == "php" || (qualified && r.language == "ruby");
                for supertype in self.supertype_names(type_name, r, depth == 0 && !qualified, qualified)? {
                    if let Some(via) = self.resolve_method_on_type_at(
                        &supertype, method, r, confidence, resolved_by, preferred_fqn, depth + 1, php,
                    )? {
                        return Ok(Some(via));
                    }
                }
            }
            return Ok(None);
        }
        // A method value (#1820) never breaks a tie by call-site file.
        if r.reference_kind == "function_ref" && matches.len() != 1 {
            return Ok(None);
        }
        if matches.len() > 1 {
            if let Some(fqn) = preferred_fqn {
                let ext = if r.language == "kotlin" { ".kt" } else { ".java" };
                let fqn_path = format!("{}{}", fqn.replace('.', "/"), ext);
                if let Some(chosen) = matches.iter().find(|m| m.file_path.ends_with(&fqn_path)) {
                    return Ok(Some(KCand {
                        node: chosen.clone(),
                        confidence,
                        resolved_by,
                    }));
                }
            }
        }
        let ordered = prefer_call_site_file(matches, &r.file_path);
        Ok(Some(KCand {
            node: ordered[0].clone(),
            confidence,
            resolved_by,
        }))
    }

    /// inferJavaFieldReceiverType — the declared type of a `field` node in
    /// the class enclosing the call (`signature` = "<Type> <name>").
    pub(super) fn infer_java_field_receiver_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let in_file = self.nodes_in_file(&r.file_path)?;
        if in_file.is_empty() {
            return Ok(None);
        }
        let mut enclosing: Option<&KNode> = None;
        for n in in_file.iter() {
            if n.kind != "class" && n.kind != "interface" {
                continue;
            }
            if n.language != r.language {
                continue;
            }
            if n.start_line <= r.line
                && n.end_line >= r.line
                && enclosing.is_none_or(|e| n.start_line >= e.start_line)
            {
                enclosing = Some(n);
            }
        }
        let Some(enclosing) = enclosing else { return Ok(None) };
        let Some(field) = in_file.iter().find(|n| {
            n.kind == "field"
                && n.name == receiver
                && n.language == r.language
                && n.start_line >= enclosing.start_line
                && n.end_line <= enclosing.end_line
        }) else {
            return Ok(None);
        };
        let Some(sig) = field.signature.as_deref() else {
            return Ok(None);
        };
        // slice(0, lastIndexOf(name)) — a -1 index drops the last UTF-16 unit.
        let before = match sig.rfind(&field.name) {
            Some(i) => &sig[..i],
            None => js_prefix(sig, utf16_len(sig).saturating_sub(1)),
        };
        let type_raw = before.trim();
        if type_raw.is_empty() {
            return Ok(None);
        }
        let no_generics = re!(r"<[^>]*>").replace_all(type_raw, "");
        let no_array = re!(r"\[\s*\]")
            .replace_all(&no_generics, "")
            .to_string();
        let no_varargs = re!(r"\.\.\.$").replace(&no_array, "");
        let Some(last) = no_varargs
            .split(|c: char| c == '.' || c.is_whitespace())
            .rfind(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        if !last.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return Ok(None);
        }
        Ok(Some(last.to_string()))
    }

    /// matchGoFactoryReceiver — a Go receiver bound to a declared/param value
    /// or to the first result of a same-line `:=` factory call.
    pub(super) fn match_go_factory_receiver(
        &mut self,
        receiver: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let mut site = r.clone();
        let bindings = self.bindings(&r.file_path)?;
        let mut binding = innermost_binding(&bindings, receiver, Some(r.line)).cloned();
        if binding.is_none() {
            let mut values: Vec<Arc<KNode>> = Vec::new();
            for n in self.nodes_by_name(receiver)?.iter() {
                if n.language != "go"
                    || !matches!(n.kind.as_str(), "variable" | "constant")
                    || pos_dirname(&n.file_path) != pos_dirname(&r.file_path)
                {
                    continue;
                }
                let decls = self.bindings(&n.file_path)?;
                if decls
                    .iter()
                    .any(|row| row.node_id.as_deref() == Some(n.id.as_str()) && row.kind == "decl")
                {
                    values.push(n.clone());
                }
            }
            if values.len() != 1 {
                return Ok(None);
            }
            let value = values[0].clone();
            site.file_path = value.file_path.clone();
            site.line = value.start_line;
            let site_bindings = self.bindings(&site.file_path)?;
            binding =
                innermost_binding(&site_bindings, receiver, Some(site.line)).cloned();
        }
        let Some(binding) = binding else { return Ok(None) };
        let declaration = self
            .read_file(&site.file_path)
            .and_then(|ls| ls.get((binding.line - 1) as usize).cloned())
            .unwrap_or_default();
        // `\bRECV(?:\s*,\s*\w+)*\s+\*?TYPE(?:\s*[,)]|\s*$)` (a grouped
        // `value, other Store` types both names) / `\bRECV\s+\*?TYPE\s*(?:=|$)`
        static PARAM_TYPE: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new("", r"(?:\s*,\s*[A-Za-z0-9_]+)*\s+\*?([A-Za-z0-9_.]+)(?:\s*[,)]|\s*$)", true, false, false)
        });
        static VAR_TYPE: LazyLock<Affix> =
            LazyLock::new(|| Affix::new("", r"\s+\*?([A-Za-z0-9_.]+)\s*(?:=|$)", true, false, false));
        let value = self.node_by_opt_id(binding.node_id.as_deref())?;
        let ty = if binding.kind == "param" {
            PARAM_TYPE.capture(&declaration, receiver).map(str::to_string)
        } else {
            let sig_ty = match value.as_ref().and_then(|v| v.signature.as_deref()) {
                Some(sig) => re!(r"^=\s*&?([A-Za-z0-9_.]+)\s*\{")
                    .captures(sig)
                    .and_then(|c| c.get(1).map(|g| g.as_str().to_string())),
                None => None,
            };
            match sig_ty {
                Some(t) => Some(t),
                None => VAR_TYPE.capture(&declaration, receiver).map(str::to_string),
            }
        };
        if let Some(ty) = ty {
            let mut bsite = site.clone();
            bsite.line = binding.line;
            return self.match_bound_type_member(&ty, method, &bsite);
        }
        if binding.kind == "param" {
            return Ok(None);
        }
        let assign_re = re!(r"(?-u:\b)([A-Za-z0-9_]+(?:\s*,\s*[A-Za-z0-9_]+)*)\s*:=\s*([A-Za-z0-9_.]+)\s*\(");
        let site_bindings = self.bindings(&site.file_path)?;
        for caps in assign_re.captures_iter(&declaration) {
            let names: Vec<String> = caps[1]
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
            if names.first().map(|n| n != receiver).unwrap_or(true) {
                continue;
            }
            let name = caps[2].to_string();
            let mut factory_site = site.clone();
            factory_site.line = binding.line;
            factory_site.reference_name = name.clone();
            let factory_binding = innermost_binding(
                &site_bindings,
                name.split('.').next().unwrap_or(&name),
                Some(binding.line),
            )
            .cloned();
            let mut callee: Option<Arc<KNode>> = None;
            match &factory_binding {
                Some(b) if b.kind == "import" => {
                    let via = if name.contains('.') {
                        self.resolve_via_import_member(&factory_site)?
                    } else {
                        self.resolve_via_import(&factory_site)?
                    };
                    if let Some(c) = via {
                        callee = self.node_by_id(&c.node.id)?;
                    }
                }
                Some(b) => {
                    if let Some(id) = &b.node_id {
                        callee = self.node_by_id(id)?;
                    }
                }
                None => {
                    if !name.contains('.') {
                        let cands: Vec<Arc<KNode>> = self
                            .nodes_by_name(&name)?
                            .iter()
                            .filter(|n| {
                                n.language == "go"
                                    && matches!(n.kind.as_str(), "function" | "struct" | "interface" | "type_alias")
                                    && pos_dirname(&n.file_path)
                                        == pos_dirname(&site.file_path)
                            })
                            .cloned()
                            .collect();
                        if cands.len() == 1 {
                            callee = Some(cands[0].clone());
                        }
                    }
                }
            }
            // `w := T(x)` converts to the type `T` itself.
            if let Some(ty) = callee.as_ref().filter(|c| matches!(c.kind.as_str(), "struct" | "interface" | "type_alias")) {
                return self.match_bound_type_member(&ty.name.clone(), method, &r.clone().at(ty));
            }
            let ret_shape = re!(r"^\*?[A-Za-z0-9_.]+$");
            let valid = callee.as_ref().is_some_and(|c| {
                c.kind == "function"
                    && c.return_type
                        .as_deref()
                        .is_some_and(|t| ret_shape.is_match(t))
            });
            if !valid {
                return Ok(None);
            }
            let callee = callee.unwrap();
            let ret = callee.return_type.clone().unwrap();
            let stripped = ret.strip_prefix('*').unwrap_or(&ret);
            return self.match_bound_type_member(stripped, method, &r.clone().at(&callee));
        }
        Ok(None)
    }
}

/// Whether `n` declares the same type as `owner`: the same qualified name
/// in the same scope. A TS/JS or Python type is scoped by its module file
/// and a Go type by its package directory; qualified names carry the
/// namespace or package elsewhere, where a type may span files.
fn same_declared_type(n: &KNode, owner: &KNode) -> bool {
    n.qualified_name == owner.qualified_name
        && match n.language.as_str() {
            l if is_esm_family(l) || l == "python" => n.file_path == owner.file_path,
            "go" => pos_dirname(&n.file_path) == pos_dirname(&owner.file_path),
            _ => true,
        }
}

/// A Python file as `super()` resolution reads it: its code lines (from
/// `python_code_lines`), its logical statements, and the `def` or `class`
/// statement whose block holds each line.
pub(crate) struct PyFile {
    code: Vec<(bool, String)>,
    /// The statements: first line, and code with continuation lines joined
    /// by a space.
    stmts: Vec<(usize, String)>,
    /// By line: the first line of the innermost `def` or `class` statement
    /// whose block holds it; `if`, `for`, `with` and `try` open no scope.
    scope: Vec<Option<usize>>,
}

impl PyFile {
    pub(super) fn new(lines: &[String]) -> Self {
        let code = super::member_fn_ref::python_code_lines(lines, 0, lines.len());
        let mut stmts: Vec<(usize, String)> = Vec::new();
        for (i, (start, c)) in code.iter().enumerate() {
            match stmts.last_mut() {
                Some((_, s)) if !*start => {
                    s.push(' ');
                    s.push_str(c);
                }
                _ => stmts.push((i, c.clone())),
            }
        }
        let mut scope = vec![None; code.len()];
        let mut open: Vec<(usize, usize)> = Vec::new();
        for (k, (i, s)) in stmts.iter().enumerate() {
            if s.trim().is_empty() {
                continue;
            }
            let indent = s.len() - s.trim_start().len();
            while open.last().is_some_and(|&(d, _)| d >= indent) {
                open.pop();
            }
            let end = stmts.get(k + 1).map_or(code.len(), |(j, _)| *j);
            scope[*i..end].fill(open.last().map(|&(_, h)| h));
            if re!(r"^\s*(?:async\s+)?(?:def|class)\b").is_match(s) {
                open.push((indent, *i));
            }
        }
        PyFile { code, stmts, scope }
    }

    /// The statement line `at` belongs to: its first line and code.
    fn stmt(&self, at: usize) -> Option<&(usize, String)> {
        self.stmts.get(self.stmts.partition_point(|(i, _)| *i <= at).checked_sub(1)?)
    }

    /// The first line of a class's `class` statement, past any decorators.
    fn class_header(&self, class: &KNode) -> Option<usize> {
        let lo = (class.start_line - 1).max(0) as usize;
        let hi = (class.end_line.max(class.start_line) as usize).min(self.code.len());
        let k = self.stmts.partition_point(|(i, _)| *i < lo);
        self.stmts[k..].iter().take_while(|(i, _)| *i < hi).find(|(_, s)| re!(r"^\s*class\b").is_match(s)).map(|(i, _)| *i)
    }

    /// The `def` statements, innermost first, whose names code in the block
    /// of the statement at line `at` (and the statement itself, given its
    /// own line) looks up: class bodies between are skipped, as in Python.
    fn functions_around(&self, mut at: Option<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        while let Some(h) = at {
            if self.stmt(h).is_some_and(|(_, s)| re!(r"^\s*(?:async\s+)?def\b").is_match(s)) {
                out.push(h);
            }
            at = self.scope[h];
        }
        out
    }

    /// The first lines of the statements that bind `name` (see
    /// `python_binder`) where code running in the functions `fns` (from
    /// `functions_around`) looks it up: at module level or in one of `fns`,
    /// as a parameter of one of `fns`, or by a `global` statement anywhere.
    fn bindings(&self, fns: &[usize], name: &str, defs: bool) -> Res<Vec<usize>> {
        let binds = python_binder(name, defs)?;
        let word = KernelResolver::cached_regex(&format!(r"\b{}\b", regex::escape(name)))?;
        let global = KernelResolver::cached_regex(&format!(r"^\s*global\b.*\b{}\b", regex::escape(name)))?;
        Ok(self
            .stmts
            .iter()
            .filter(|(i, s)| {
                (self.scope[*i].is_none_or(|h| fns.contains(&h)) && binds(s))
                    || global.is_match(s)
                    || (fns.contains(i) && python_def_params(s).is_some_and(|p| word.is_match(&s[p])))
            })
            .map(|(i, _)| *i)
            .collect())
    }
}

/// The function a Python `super(...)` call at byte `col` of line `at` runs
/// in: the first line of its `def` statement and its first parameter. None
/// in a class body or a parameter default, in a lambda, in a generator
/// expression for the zero-argument form, under `@staticmethod`, or without
/// a first parameter, where Python raises.
fn python_method_frame(file: &PyFile, at: usize, col: usize, zero_arg: bool) -> Option<(usize, String)> {
    let def = re!(r"^\s*(?:async\s+)?def\b");
    let (s, stmt) = file.stmt(at)?;
    let off = file.code.get(*s..at)?.iter().map(|(_, c)| c.len() + 1).sum::<usize>() + col;
    let before = stmt.get(..off)?;
    if re!(r"\blambda\b").is_match(before) || re!(r"^\s*class\b").is_match(stmt) || (zero_arg && python_in_generator(stmt, off)) {
        return None;
    }
    // A one-line `def` holds the call itself, past its parameters.
    let d = if def.is_match(stmt) {
        if off <= python_def_params(stmt)?.end {
            return None;
        }
        *s
    } else {
        file.scope[*s]?
    };
    let (_, header) = file.stmt(d)?;
    if !def.is_match(header) {
        return None;
    }
    let k = file.stmts.partition_point(|(i, _)| *i < d);
    for (_, c) in file.stmts[..k].iter().rev() {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        if !c.starts_with('@') {
            break;
        }
        if re!(r"^@\s*(?:[\w.]*\.)?staticmethod\b").is_match(c) {
            return None;
        }
    }
    let receiver = re!(r"^\s*([A-Za-z_]\w*)\s*(?:[,:=]|$)").captures(&header[python_def_params(header)?])?;
    Some((d, receiver[1].to_string()))
}

/// Whether byte `at` of a Python statement sits in a generator expression,
/// past its first iterable (which runs in the enclosing frame). A list, set
/// or dict comprehension runs in the enclosing frame from Python 3.12 on.
fn python_in_generator(stmt: &str, at: usize) -> bool {
    let b = stmt.as_bytes();
    let word = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
    let keyword = |i: usize, k: &str| b[i..].starts_with(k.as_bytes()) && (i == 0 || !word(i - 1)) && !word(i + k.len());
    let mut open = Vec::new();
    for (i, c) in b.iter().enumerate().take(at) {
        match c {
            b'(' | b'[' | b'{' => open.push(i),
            b')' | b']' | b'}' => {
                open.pop();
            }
            _ => {}
        }
    }
    open.into_iter().filter(|&o| b[o] == b'(').any(|o| {
        // The bracket's own `for`, its first iterable's `in`, and the end of
        // that iterable.
        let (mut depth, mut for_at, mut in_at, mut end) = (0usize, None, None, b.len());
        for (j, &c) in b.iter().enumerate().skip(o + 1) {
            match c {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth == 0 => {
                    end = end.min(j);
                    break;
                }
                b')' | b']' | b'}' => depth -= 1,
                _ if depth > 0 => {}
                _ if for_at.is_none() => for_at = keyword(j, "for").then_some(j),
                _ if in_at.is_none() => in_at = keyword(j, "in").then_some(j),
                _ if keyword(j, "for") || keyword(j, "if") => end = end.min(j),
                _ => {}
            }
        }
        for_at.is_some() && !in_at.is_some_and(|i| i < at && at < end)
    })
}

/// The byte range of a Python `def` statement's parameter list.
fn python_def_params(stmt: &str) -> Option<std::ops::Range<usize>> {
    let open = re!(r"^\s*(?:async\s+)?def\s+\w+\s*(?:\[[^\]]*\])?\s*\(").find(stmt)?.end();
    let mut depth = 1usize;
    for (i, ch) in stmt[open..].char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open..open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a Python statement (from `PyFile`) binds `name`: assigns it
/// (tuple, annotated, augmented and `:=` forms included), imports it, makes
/// it a loop, `with` or `except` target, or names it in `global`,
/// `nonlocal` or `del`; with `defs`, defines a function or class of that
/// name. A keyword argument or a parameter is no binding here.
fn python_binder(name: &str, defs: bool) -> Res<impl Fn(&str) -> bool> {
    let n = regex::escape(name);
    let mut forms = vec![
        format!(r"(?:^|[:;=])\s*{n}\s*(?:[-+*/%&|^@]|//|\*\*|<<|>>)?=(?:[^=]|$)"),
        format!(r"\b{n}\s*:="),
        format!(r"\bas\s+{n}\b"),
        format!(r"^\s*(?:from\s+\S+\s+)?import\b.*\b{n}\b"),
        format!(r"^\s*(?:async\s+)?for\b[^:]*\b{n}\b[^:]*\bin\b"),
        format!(r"^\s*(?:global|nonlocal|del)\b.*\b{n}\b"),
    ];
    if defs {
        forms.push(format!(r"^\s*(?:async\s+)?(?:def|class)\s+{n}\b"));
    }
    let binds = KernelResolver::cached_regex(&forms.iter().map(|f| format!("(?:{f})")).collect::<Vec<_>>().join("|"))?;
    let word = KernelResolver::cached_regex(&format!(r"\b{n}\b"))?;
    // A target of `a, name = …` or `name: T = …`, not `x.name = …` or `name.x = …`.
    Ok(move |s: &str| {
        binds.is_match(s)
            || python_assignment_target(s).is_some_and(|t| {
                word.find_iter(t).any(|m| {
                    !t[..m.start()].ends_with('.') && !matches!(t[m.end()..].trim_start().chars().next(), Some('.' | '(' | '['))
                })
            })
    })
}

/// The target side of a Python assignment statement: the code before its
/// first `=` outside brackets that is not part of a comparison.
fn python_assignment_target(code: &str) -> Option<&str> {
    let b = code.as_bytes();
    let mut depth = 0usize;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'=' if depth == 0 => {
                let prev = if i > 0 { b[i - 1] } else { b' ' };
                if b.get(i + 1) != Some(&b'=') && !matches!(prev, b'=' | b'!' | b'<' | b'>' | b':') {
                    return Some(&code[..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// matchBoundTypeMember's verdict for a member it proved.
fn bound_member_cand(m: Arc<KNode>) -> KCand {
    KCand {
        resolved_by: if m.kind == "field" { "field-call" } else { "instance-method" },
        node: m,
        confidence: 0.9,
    }
}
