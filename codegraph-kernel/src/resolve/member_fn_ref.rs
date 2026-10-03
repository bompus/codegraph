//! Receiver-path function refs (#1820): a Python or Go method passed as a
//! value keeps its receiver (`pool.submit(self.store.fetch)`,
//! `Submit(c.store.Fetch)`), and the receiver's import, type or class scopes
//! the member. Only a unique function/method target resolves; a known
//! receiver that doesn't carry the member, a data attribute or a Python
//! property stays unlinked. An unknown receiver falls back to a unique name
//! across the project, never a same-file or file-order tie-break.

use super::*;

/// Base-class and mutual-recursion walks stop after this many classes.
const CLASS_WALK_LIMIT: usize = 16;

/// A receiver whose type was inferred but can't name one class.
const UNKNOWN_TYPE: &str = "<unknown>";

impl KernelResolver {
    /// matchMemberFunctionRef — `recv.member` for a Python/Go function_ref.
    pub(super) fn match_member_function_ref(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(dot) = r.reference_name.rfind('.') else {
            return Ok(None);
        };
        let (receiver, member) = (&r.reference_name[..dot], &r.reference_name[dot + 1..]);
        let root = receiver.split('.').next().unwrap_or(receiver);
        // An import is authoritative even when it points outside the project,
        // unless a parameter or local shadows it at the ref.
        if self.import_mappings(&r.file_path)?.iter().any(|i| i.local_name == root) && !self.is_shadowed_import(root, r)? {
            if r.language == "python" {
                if let Some(cls) = self.python_ref_class(receiver, r)? {
                    let members = self.python_members(&cls, member, r, &mut HashSet::new())?;
                    return self.unique_member(members, r, 0.9);
                }
                if let Some(hit) = self.python_imported_global(receiver, member, r)? {
                    return Ok(hit);
                }
            }
            let Some(hit) = self.resolve_via_import(r)? else {
                return Ok(None);
            };
            let same: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&hit.node.qualified_name)?
                .iter()
                .filter(|n| n.file_path == hit.node.file_path)
                .cloned()
                .collect();
            return self.unique_member(same, r, 0.9);
        }
        if r.language == "go" {
            let typed = if receiver.contains('.') {
                Some(self.match_go_field_chain_call(receiver, member, r)?)
            } else if let Some(raw) = self.infer_local_receiver_type(receiver, r, true)? {
                let raw = raw.trim_start_matches(['*', '&']);
                if raw.contains('.') {
                    // `pkg.Type` names that package's type, which only its
                    // import can place; an external package has none here.
                    Some(self.match_bound_type_member(raw, member, r)?)
                } else if let Some(ty) = self.normalize_inferred_type_name(raw)? {
                    Some(self.resolve_method_on_type(&ty, member, r, 0.9, "function-ref", None)?)
                } else {
                    Some(None)
                }
            } else {
                let types = self
                    .nodes_by_name(receiver)?
                    .iter()
                    .filter(|n| n.language == "go" && (n.kind == "struct" || n.kind == "interface"))
                    .count();
                match types {
                    0 => None,
                    1 => Some(self.resolve_method_on_type(receiver, member, r, 0.9, "function-ref", None)?),
                    _ => Some(None),
                }
            };
            // No name-only fallback for Go: struct fields aren't nodes, so a
            // field read (`c.Errors`) would match any same-named method.
            return match typed {
                Some(hit) => self.unique_member(hit.map(|c| c.node).into_iter().collect(), r, 0.9),
                None => Ok(None),
            };
        }
        if let Some(hit) = self.python_typed_member(receiver, member, r)? {
            return Ok(hit);
        }
        // Unknown receivers: unique-or-drop across ALL files, test doubles
        // and abstract-looking bodies included. Only a method can sit behind
        // a receiver; instance attributes aren't nodes, so a unique function
        // of the member's name is no evidence.
        let named: Vec<Arc<KNode>> = self.nodes_by_name(member)?.iter().cloned().collect();
        Ok(self.unique_member(named, r, 0.8)?.filter(|c| c.node.kind == "method" && super::method_call::shares_receiver_word(super::call_shape::receiver_link(receiver), &c.node)))
    }

    /// `self.<field>.method()` / `cls.<field>.method()` in Python: the field's
    /// type from its class or a base (`self.cache = Store()` in `__init__`,
    /// an annotation, an annotated parameter assigned to it), then the member
    /// on that type or along its MRO, else a subclass's. `None`: the field's
    /// type (or the subclass) is not known here, so the name strategies
    /// decide; `Some(None)`: the type's member is data or a property.
    pub(super) fn python_self_field_call(&mut self, receiver: &str, member: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let Some(field) = receiver.strip_prefix("self.").or_else(|| receiver.strip_prefix("cls.")).filter(|f| is_word(f)) else {
            return Ok(None);
        };
        let owner = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| n.kind == "class" && n.start_line <= r.line && n.end_line >= r.line)
            .max_by_key(|n| n.start_line)
            .cloned();
        let Some(owner) = owner else { return Ok(None) };
        // The field's evidence in the class and every base along its MRO,
        // each read in its own file, must name one class: a subclass's
        // `self.cache = Other()` conflicts with the base's `Store()`, and
        // the name strategies decide. A base's declared type
        // (`cache: Base`) may be narrowed to a subclass of it.
        let mut classes: Vec<Arc<KNode>> = vec![owner.clone()];
        if self.supertypes_complete {
            if let Some(mro) = self.python_mro(&owner, 0)? {
                classes.extend(mro.iter().skip(1).cloned());
            }
        }
        // (class, every fact naming it is declared)
        let mut named: Vec<(Arc<KNode>, bool)> = Vec::new();
        for class in &classes {
            let site = r.clone().at(class);
            let facts = self.python_field_facts(field, class)?;
            for fact in visible_field_facts(&facts, r) {
                let Some(t) = fact.ty.as_deref().filter(|t| *t != UNKNOWN_TYPE && *t != "object" && *t != "Any") else {
                    return Ok(None);
                };
                let Some(found) = self.python_ref_class(t, &site)? else { return Ok(None) };
                match named.iter_mut().find(|(c, _)| c.id == found.id) {
                    Some((_, declared)) => *declared &= fact.declared,
                    None => named.push((found, fact.declared)),
                }
            }
        }
        let (cls, exact) = match named.len() {
            0 => return Ok(None),
            1 => (named[0].0.clone(), !named[0].1),
            _ => {
                let mut narrowest = None;
                for (c, own_declared) in &named {
                    let mro = self.python_mro(c, 0)?.unwrap_or_default();
                    let covers = named
                        .iter()
                        .all(|(d, declared)| d.id == c.id || (*declared && mro.iter().any(|m| m.id == d.id)));
                    if covers {
                        narrowest = Some((c.clone(), !*own_declared));
                        break;
                    }
                }
                let Some(pick) = narrowest else { return Ok(None) };
                pick
            }
        };
        let mut members = self.python_members(&cls, member, r, &mut HashSet::new())?;
        let mut confidence = 0.9;
        if members.is_empty() {
            // A field only ever assigned `Store()` holds exactly a `Store`.
            if exact {
                return Ok(Some(None));
            }
            // A declared base type can hold a subclass: its one method of
            // that name, else the name strategies decide as before.
            members = self.python_descendant_members(&cls, member, r)?;
            if members.len() != 1 {
                return Ok(None);
            }
            confidence = 0.8;
        }
        let mut methods = members.into_iter().filter(|n| n.language == "python");
        Ok(Some(match (methods.next(), methods.next()) {
            (Some(m), None) if m.kind == "method" && !self.is_python_property(&m) => {
                Some(KCand { node: m, confidence, resolved_by: "instance-method" })
            }
            _ => None,
        }))
    }

    /// Is `name` bound at `r` by something other than its import (a
    /// parameter, a local)? Files without binding rows keep the import.
    fn is_shadowed_import(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let rows = self.bindings(&r.file_path)?;
        Ok(innermost_binding(&rows, name, Some(r.line)).is_some_and(|b| b.kind != "import"))
    }

    /// The Python receiver arms: `self`/`cls`, `self.field`, a local or
    /// parameter, or a class used directly. `None` means the receiver's type
    /// is unknown; `Some(None)` is a known receiver that settled no target.
    fn python_typed_member(&mut self, receiver: &str, member: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let owner = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| n.kind == "class" && n.start_line <= r.line && n.end_line >= r.line)
            .max_by_key(|n| n.start_line)
            .cloned();
        if receiver == "self" || receiver == "cls" {
            let Some(owner) = owner else { return Ok(Some(None)) };
            let members = self.python_members(&owner, member, r, &mut HashSet::new())?;
            return Ok(Some(self.unique_member(members, r, 0.9)?));
        }
        let field = receiver.strip_prefix("self.").or_else(|| receiver.strip_prefix("cls."));
        let mut ty = match field {
            Some(f) if is_word(f) => {
                let Some(owner) = owner else { return Ok(Some(None)) };
                self.python_field_type(f, &owner, r)?
            }
            _ => self.python_local_type(receiver, r)?,
        };
        // An untyped module global (`conn = None`, rebound elsewhere).
        if ty.is_none() && field.is_none() && is_word(receiver) {
            if let Some(hit) = self.python_same_file_global(receiver, member, r)? {
                return Ok(Some(hit));
            }
        }
        // A type name used directly (`Store.fetch`) scopes like an annotation.
        if ty.is_none() && is_word(receiver) && receiver.starts_with(|c: char| c.is_ascii_uppercase()) {
            ty = Some(receiver.to_string());
        }
        let Some(ty) = ty.filter(|t| t != "object" && t != "Any") else {
            return Ok(None);
        };
        let Some(cls) = self.python_ref_class(&ty, r)? else {
            return Ok(Some(None));
        };
        let members = self.python_members(&cls, member, r, &mut HashSet::new())?;
        if !members.is_empty() {
            return Ok(Some(self.unique_member(members, r, 0.9)?));
        }
        // A base-typed receiver can hold a subclass-only method.
        let descendants = self.python_descendant_members(&cls, member, r)?;
        Ok(Some(self.unique_member(descendants, r, 0.8)?))
    }

    /// `member` methods of the classes deriving from `cls` (a base-typed
    /// receiver can hold a subclass): only descendants of THAT base, so
    /// unrelated same-name methods can't win.
    fn python_descendant_members(&mut self, cls: &KNode, member: &str, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let key = (cls.id.clone(), member.to_string());
        if let Some(hit) = self.py_descendants_memo.get(&key) {
            return Ok(hit.as_ref().clone());
        }
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(member)?
            .iter()
            .filter(|n| n.kind == "method" && n.language == "python")
            .cloned()
            .collect();
        let mut descendants = Vec::new();
        for n in candidates {
            let Some(sep) = n.qualified_name.rfind("::") else { continue };
            let parent = self
                .nodes_in_file(&n.file_path)?
                .iter()
                .find(|c| c.kind == "class" && c.qualified_name == n.qualified_name[..sep])
                .cloned();
            if let Some(parent) = parent {
                if self.python_derives_from(&parent, cls, r, &mut HashSet::new())? {
                    descendants.push(n);
                }
            }
        }
        self.py_descendants_memo.insert(key, Rc::new(descendants.clone()));
        Ok(descendants)
    }

    /// The single same-family candidate, when it is a callable other than
    /// the referencing node and not a Python property.
    pub(super) fn unique_member(&mut self, nodes: Vec<Arc<KNode>>, r: &ResolveRefIn, confidence: f64) -> Res<Option<KCand>> {
        let mut pool = nodes.into_iter().filter(|n| same_language_family(&n.language, &r.language));
        let (Some(target), None) = (pool.next(), pool.next()) else {
            return Ok(None);
        };
        if !(target.kind == "function" || target.kind == "method")
            || target.id == r.from_node_id
            || self.is_python_property(&target)
        {
            return Ok(None);
        }
        Ok(Some(KCand { node: target, confidence, resolved_by: "function-ref" }))
    }

    /// A class named from `r`'s file: through its import, else the file's
    /// own unique class of that name.
    pub(super) fn python_ref_class(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let root = name.split('.').next().unwrap_or(name);
        if self.import_mappings(&r.file_path)?.iter().any(|i| i.local_name == root) {
            let Some(hit) = self.resolve_via_import(&r.clone().naming(name, "references"))? else {
                return Ok(None);
            };
            let node = hit.node;
            if node.kind != "class" {
                return Ok(None);
            }
            let same = self
                .nodes_by_qualified_name(&node.qualified_name)?
                .iter()
                .filter(|n| n.kind == "class" && n.file_path == node.file_path)
                .count();
            return Ok((same == 1).then_some(node));
        }
        let mut classes = self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| n.kind == "class" && n.file_path == r.file_path)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter();
        Ok(match (classes.next(), classes.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        })
    }

    /// The classes a Python class header names as bases, each resolved from
    /// the class's own file.
    pub(super) fn python_bases(&mut self, cls: &KNode, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let Some(lines) = self.read_file(&cls.file_path) else {
            return Ok(Vec::new());
        };
        let site = r.clone().at(cls);
        let mut out = Vec::new();
        for name in python_base_names(&lines, (cls.start_line - 1).max(0) as usize) {
            if let Some(base) = self.python_ref_class(&name, &site)? {
                out.push(base);
            }
        }
        Ok(out)
    }

    pub(super) fn python_derives_from(&mut self, cls: &KNode, base: &KNode, r: &ResolveRefIn, seen: &mut HashSet<String>) -> Res<bool> {
        if seen.len() >= CLASS_WALK_LIMIT || !seen.insert(cls.id.clone()) {
            return Ok(false);
        }
        for parent in self.python_bases(cls, r)? {
            if parent.id == base.id || self.python_derives_from(&parent, base, r, seen)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// What `member` names on `cls`: an assignment in the class body shadows
    /// any method (the class itself stands in, so it never resolves), else
    /// the class's own member, else its bases' (deduplicated).
    pub(super) fn python_members(
        &mut self,
        cls: &Arc<KNode>,
        member: &str,
        r: &ResolveRefIn,
        seen: &mut HashSet<String>,
    ) -> Res<Vec<Arc<KNode>>> {
        if seen.len() >= CLASS_WALK_LIMIT || !seen.insert(cls.id.clone()) {
            return Ok(Vec::new());
        }
        if self.python_class_assigns(cls, member)? {
            return Ok(vec![cls.clone()]);
        }
        let own: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{}::{}", cls.qualified_name, member))?
            .iter()
            .filter(|n| n.file_path == cls.file_path)
            .cloned()
            .collect();
        if !own.is_empty() {
            return Ok(own);
        }
        // Python takes the first class along the C3 order that defines the
        // name, so a later base's same-named method never competes.
        if self.supertypes_complete {
            if let Some(mro) = self.python_mro(cls, 0)?.filter(|m| m.len() > 1) {
                for class in mro.iter().skip(1) {
                    if self.python_class_assigns(class, member)? {
                        return Ok(vec![class.clone()]);
                    }
                    let own: Vec<Arc<KNode>> = self
                        .nodes_by_qualified_name(&format!("{}::{}", class.qualified_name, member))?
                        .iter()
                        .filter(|n| n.file_path == class.file_path)
                        .cloned()
                        .collect();
                    if !own.is_empty() {
                        return Ok(own);
                    }
                }
                return Ok(Vec::new());
            }
        }
        let mut out: Vec<Arc<KNode>> = Vec::new();
        for base in self.python_bases(cls, r)? {
            for n in self.python_members(&base, member, r, seen)? {
                if !out.iter().any(|o| o.id == n.id) {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }

    /// Whether `cls` assigns `member` as data: `member = …` / `member: T` in
    /// the class body, or `self.member = …` inside one of its methods (a
    /// method's bare locals and keyword arguments don't count).
    pub(super) fn python_class_assigns(&mut self, cls: &KNode, member: &str) -> Res<bool> {
        self.python_member_assignment(cls,member,true)
    }

    pub(super) fn python_type_assigns(&mut self, cls: &KNode, member: &str) -> Res<bool> {
        self.python_member_assignment(cls,member,false)
    }

    fn python_member_assignment(&mut self, cls: &KNode, member: &str, instance: bool) -> Res<bool> {
        let Some(lines) = self.read_file(&cls.file_path) else {
            return Ok(false);
        };
        let bodies: Vec<(i64, i64)> = self
            .nodes_in_file(&cls.file_path)?
            .iter()
            .filter(|n| {
                (n.kind == "method" || n.kind == "function") && n.start_line > cls.start_line && n.end_line <= cls.end_line
            })
            .map(|n| (n.start_line, n.end_line))
            .collect();
        let assigns = re!(r"^\s*((?:self|cls)\.)?([A-Za-z_]\w*)\s*[=:]");
        let lo = (cls.start_line - 1).max(0) as usize;
        let hi = (cls.end_line.max(cls.start_line) as usize).min(lines.len());
        let starts = python_statement_starts(&lines, lo, hi);
        for (i, line) in lines.get(lo..hi).unwrap_or(&[]).iter().enumerate() {
            if !starts[i] {
                continue;
            }
            let Some(c) = assigns.captures(line) else { continue };
            if &c[2] != member || line.trim_start().starts_with('#') {
                continue;
            }
            if !instance {
                let code=super::awaited::blank_string_contents(line);
                if !code.split('#').next().unwrap_or("").contains('=') {continue;}
            }
            let line_no = (lo + i + 1) as i64;
            let in_method = bodies.iter().any(|&(s, e)| s <= line_no && line_no <= e);
            if !in_method || (instance && c.get(1).is_some()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `@property` / `@cached_property` methods are attribute reads, not
    /// callables passed by value.
    pub(super) fn is_python_property(&mut self, node: &KNode) -> bool {
        if node.language != "python" || node.kind != "method" {
            return false;
        }
        let is_property = |d: &str| {
            let d = d.trim().trim_start_matches('@');
            d == "property" || d == "cached_property" || d == "functools.cached_property"
        };
        if node.decorators.as_ref().is_some_and(|ds| ds.iter().any(|d| is_property(d))) {
            return true;
        }
        let Some(lines) = self.read_file(&node.file_path) else {
            return false;
        };
        let mut i = node.start_line - 2;
        while i >= 0 {
            let line = lines.get(i as usize).map(|l| l.trim()).unwrap_or("");
            if !line.starts_with('@') {
                break;
            }
            if is_property(line) {
                return true;
            }
            i -= 1;
        }
        false
    }

    /// A local's type from the nearest assignment or annotation above the
    /// reference inside the caller, else the caller's parameter annotation.
    /// An assignment of anything but a constructor call is `<unknown>`.
    fn python_local_type(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if !is_word(receiver) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let mut scope = self.node_by_id(&r.from_node_id)?;
        let mut upto = r.line;
        // A nested def reads a name it does not bind from the function around
        // it (`def outer(obj: Store): def inner(): submit(obj.fetch)`). Only a
        // type found there counts; an untyped outer assignment (`app =
        // ctx.app`) leaves the nested site as unknown as it was.
        for depth in 0..CLASS_WALK_LIMIT {
            if let Some(found) = self.python_scope_local_type(receiver, scope.as_deref(), upto, &lines, r)? {
                return Ok((depth == 0 || found != UNKNOWN_TYPE).then_some(found));
            }
            let Some(inner) = scope else { return Ok(None) };
            let signature = inner.signature.as_deref().unwrap_or("");
            if let Some(t) = param_annotation(signature, receiver) {
                return Ok(Some(t));
            }
            if has_param(signature, receiver) || inner.kind == "method" {
                return Ok(None);
            }
            let outer = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "function" | "method" | "class")
                        && n.id != inner.id
                        && n.start_line <= inner.start_line
                        && n.end_line >= inner.end_line
                })
                .max_by_key(|n| n.start_line)
                .cloned();
            match outer {
                Some(o) if o.kind != "class" => {
                    upto = inner.start_line - 1;
                    scope = Some(o);
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The nearest assignment or annotation of `receiver` in `scope`'s own
    /// body at or above line `upto`, nested def and class bodies skipped.
    fn python_scope_local_type(
        &mut self,
        receiver: &str,
        scope: Option<&KNode>,
        upto: i64,
        lines: &[String],
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let floor = scope.map_or(1, |c| c.start_line).max(1);
        // A nested def or class body binds its own locals, never the caller's.
        let ceiling = scope.map_or(i64::MAX, |c| c.end_line);
        let nested: Vec<(i64, i64)> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "function" | "method" | "class")
                    && n.start_line > floor
                    && n.end_line <= ceiling
                    && !(n.start_line <= r.line && r.line <= n.end_line)
            })
            .map(|n| (n.start_line, n.end_line))
            .collect();
        let mut line_no = upto.min(lines.len() as i64);
        let starts = python_statement_starts(lines, (floor - 1) as usize, line_no.max(floor) as usize);
        while line_no >= floor {
            let line = &lines[(line_no - 1) as usize];
            let starts_statement = starts[(line_no - floor) as usize];
            let in_nested = nested.iter().any(|&(s, e)| s <= line_no && line_no <= e);
            line_no -= 1;
            if !starts_statement || in_nested {
                continue;
            }
            if let Some(c) = python_assignment_re().captures(line) {
                if c.get(1).is_none() && &c[2] == receiver {
                    return Ok(Some(assigned_type(c.get(3).map(|m| m.as_str()), &c[4])));
                }
            }
            if let Some(c) = python_annotation_re().captures(line) {
                if c.get(1).is_none() && &c[2] == receiver {
                    return Ok(Some(c[3].to_string()));
                }
            }
        }
        Ok(None)
    }

    /// `self.<field>`'s type within `owner`: a class-body or `self.`
    /// annotation, an annotated or constructor assignment in any of its
    /// methods, or an annotated method parameter assigned to it (the
    /// referencing method counts only above the reference). Conflicting
    /// evidence is known-but-ambiguous, never a name-only fallback.
    fn python_field_type(&mut self, field: &str, owner: &KNode, r: &ResolveRefIn) -> Res<Option<String>> {
        Ok(self.python_field_evidence(field, owner, r)?.map(|(t, _)| t))
    }

    /// python_field_type plus whether every piece of evidence is a
    /// constructor call (`self.cache = Store()`): then the runtime type is
    /// exactly that class, never a subclass.
    fn python_field_evidence(&mut self, field: &str, owner: &KNode, r: &ResolveRefIn) -> Res<Option<(String, bool)>> {
        let facts = self.python_field_facts(field, owner)?;
        let mut types: Vec<String> = Vec::new();
        let mut untyped = false;
        let mut declared = false;
        for fact in visible_field_facts(&facts, r) {
            match &fact.ty {
                Some(t) => {
                    declared |= fact.declared;
                    if !types.contains(t) {
                        types.push(t.clone());
                    }
                }
                None => untyped = true,
            }
        }
        Ok(match types.len() {
            0 => None,
            // An untyped reassignment (`self.store = replacement`) outvotes
            // the one typed assignment; alone it stays no evidence.
            1 if !untyped => types.pop().map(|t| (t, !declared)),
            _ => Some((UNKNOWN_TYPE.to_string(), false)),
        })
    }

    /// Every annotation and assignment of `self.<field>` in `owner`'s body
    /// and its own methods (a nested class's `__init__` types its own
    /// fields), read once per class and field.
    fn python_field_facts(&mut self, field: &str, owner: &KNode) -> Res<Rc<Vec<PyFieldFact>>> {
        let key = (owner.id.clone(), field.to_string());
        if let Some(hit) = self.py_field_facts_memo.get(&key) {
            return Ok(hit.clone());
        }
        let mut facts: Vec<PyFieldFact> = Vec::new();
        if let Some(lines) = self.read_file(&owner.file_path) {
            let prefix = format!("{}::", owner.qualified_name);
            let in_file = self.nodes_in_file(&owner.file_path)?;
            let methods: Vec<Arc<KNode>> = in_file
                .iter()
                .filter(|n| {
                    n.kind == "method" && n.qualified_name.strip_prefix(&prefix).is_some_and(|rest| !rest.contains("::"))
                })
                .cloned()
                .collect();
            let nested_classes: Vec<(i64, i64)> = in_file
                .iter()
                .filter(|n| n.kind == "class" && n.id != owner.id && n.start_line > owner.start_line && n.end_line <= owner.end_line)
                .map(|n| (n.start_line, n.end_line))
                .collect();
            let end = (owner.end_line.max(0) as usize).min(lines.len());
            let begin = (owner.start_line.max(0) as usize).min(end);
            let starts = python_statement_starts(&lines, begin, end);
            for i in begin..end {
                if !starts[i - begin] {
                    continue;
                }
                let line_no = i as i64 + 1;
                if nested_classes.iter().any(|&(s, e)| s <= line_no && line_no <= e) {
                    continue;
                }
                let method = methods.iter().find(|m| m.start_line <= line_no && m.end_line >= line_no);
                let mut push = |ty: Option<String>, declared: bool| {
                    facts.push(PyFieldFact { line: line_no, method: method.map(|m| m.id.clone()), ty, declared });
                };
                let line = &lines[i];
                if let Some(c) = python_annotation_re().captures(line) {
                    if &c[2] == field && (method.is_none() || c.get(1).is_some()) {
                        push(Some(c[3].to_string()), true);
                    }
                }
                let Some(c) = python_assignment_re().captures(line) else { continue };
                if c.get(1).is_none() || &c[2] != field {
                    continue;
                }
                if let Some(t) = c.get(3) {
                    push(Some(t.as_str().to_string()), true);
                    continue;
                }
                let value = c[4].trim();
                if let Some(ctor) = constructor_type(value) {
                    push(Some(ctor), false);
                    continue;
                }
                match method {
                    Some(_) if value == "None" => {}
                    Some(m) if is_word(value) => match param_annotation(m.signature.as_deref().unwrap_or(""), value) {
                        Some(t) => push(Some(t), true),
                        None => push(None, false),
                    },
                    _ => push(Some(UNKNOWN_TYPE.to_string()), true),
                }
            }
        }
        let facts = Rc::new(facts);
        self.py_field_facts_memo.insert(key, facts.clone());
        Ok(facts)
    }
}

/// One annotation or assignment of a Python `self.<field>`: its line, the
/// method it sits in, and the type it names (`None`: an untyped value).
pub(super) struct PyFieldFact {
    line: i64,
    method: Option<String>,
    ty: Option<String>,
    declared: bool,
}

/// The facts a reference sees: every method's, the referencing method's
/// only up to the reference.
fn visible_field_facts<'a>(facts: &'a [PyFieldFact], r: &'a ResolveRefIn) -> impl Iterator<Item = &'a PyFieldFact> {
    facts
        .iter()
        .filter(move |f| !(f.method.as_deref() == Some(r.from_node_id.as_str()) && f.line > r.line))
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
}

/// `[self.|cls.]name[: Type] = value` — groups: receiver prefix, name,
/// annotation, value.
fn python_assignment_re() -> Rc<Regex> {
    re!(r#"^\s*(?:(self|cls)\.)?([A-Za-z_]\w*)\s*(?::\s*["']?([\w.]+)["']?)?\s*=\s*(.*)$"#)
}

/// `[self.|cls.]name: Type` — groups: receiver prefix, name, type.
fn python_annotation_re() -> Rc<Regex> {
    re!(r#"^\s*(?:(self|cls)\.)?([A-Za-z_]\w*)\s*:\s*["']?([\w.]+)"#)
}

/// For each line in `lines[lo..hi]`, whether it begins a statement: not
/// inside a string (a docstring's `name: Description` line is not an
/// annotation), an open bracket (a call's `key=value,` line is not an
/// assignment) or a backslash continuation. `lo` must itself begin a
/// statement, as a `def` or `class` line does.
fn python_statement_starts(lines: &[String], lo: usize, hi: usize) -> Vec<bool> {
    python_code_lines(lines, lo, hi).into_iter().map(|(start, _)| start).collect()
}

/// For each line in `lines[lo..hi]`, whether it begins a statement (see
/// [`python_statement_starts`]) and its code: string contents blanked to
/// spaces, byte for byte, and any comment cut off, so prose never reads as a
/// binding and byte columns still line up.
pub(super) fn python_code_lines(lines: &[String], lo: usize, hi: usize) -> Vec<(bool, String)> {
    python_code_lines_as(lines, lo, hi, false)
}

/// [`python_code_lines`], but the expressions in f- and t-string fields stay
/// as code: they run in the enclosing frame.
pub(super) fn python_field_code_lines(lines: &[String], lo: usize, hi: usize) -> Vec<(bool, String)> {
    python_code_lines_as(lines, lo, hi, true)
}

fn python_code_lines_as(lines: &[String], lo: usize, hi: usize, field_code: bool) -> Vec<(bool, String)> {
    let hi = hi.min(lines.len());
    let mut out = Vec::with_capacity(hi.saturating_sub(lo));
    let mut depth = 0usize;
    // The strings and f-string fields open here, innermost last.
    let mut open: Vec<PyOpen> = Vec::new();
    let mut continued = false;
    // Lines before this one are known to close the single-quoted string open
    // across them.
    let mut checked_to = 0;
    // The lines left for looking ahead: each valid string's span is read
    // once, so only strings that never close spend it.
    let mut budget = 2 * hi.saturating_sub(lo) + PY_LOOKAHEAD_LINES;
    for (k, line) in lines.iter().enumerate().take(hi).skip(lo) {
        let start = depth == 0 && open.is_empty() && !continued;
        let mut code = line.as_bytes().to_vec();
        let (escaped_eol, _) = python_scan_line(line.as_bytes(), &mut code, &mut depth, &mut open, field_code);
        python_drop_broken(&mut open, escaped_eol);
        // A field of a single-quoted f-string may run on to later lines
        // (Python 3.12), and the lookahead reads past `hi` to the file's end;
        // one that never closes is cut here, as Python would reject it, so
        // it can't swallow the rest of the file.
        if k >= checked_to {
            if let Some(first) = open.iter().position(|o| matches!(o, PyOpen::Str { triple: false, .. })) {
                match python_closes_within(&lines[k + 1..], &open, first, &mut budget) {
                    Ok(Some(m)) => checked_to = k + 1 + m,
                    // Out of lookahead, a line ending in a backslash still
                    // continues the string, as Python reads a plain one.
                    Err(()) if escaped_eol => {}
                    _ => {
                        open.truncate(first);
                        python_pop_fields(&mut open);
                    }
                }
            }
        }
        // A backslash in a comment continues nothing.
        continued = open.is_empty() && code.trim_ascii_end().ends_with(b"\\");
        // Every blanked byte belonged to a whole character, so this is UTF-8.
        out.push((start, String::from_utf8(code).unwrap_or_default()));
    }
    out
}

/// The lines `python_code_lines` may read ahead beyond twice its window.
const PY_LOOKAHEAD_LINES: usize = 1000;

/// Scans one line from the strings and fields `open` at its start: blanks
/// string contents in `code` (opening and closing quotes of the outermost
/// string kept), cuts a comment off, and tracks bracket `depth` outside
/// strings. With `field_code`, a field's expression stays (its closing `}`
/// and the `:` before its format spec still blanked). Returns whether the
/// line ends in a backslash inside a string or field, and the fewest levels
/// left open at any point.
fn python_scan_line(b: &[u8], code: &mut Vec<u8>, depth: &mut usize, open: &mut Vec<PyOpen>, field_code: bool) -> (bool, usize) {
    let mut low = open.len();
    let mut escaped_eol = false;
    let mut i = 0;
    while i < b.len() {
        let Some(&top) = open.last() else {
            match b[i] {
                b'#' => {
                    code.truncate(i);
                    break;
                }
                // The opening quote stays.
                q @ (b'"' | b'\'') => {
                    i += python_open_string(b, i, q, open);
                    continue;
                }
                b'(' | b'[' | b'{' => *depth += 1,
                b')' | b']' | b'}' => *depth = depth.saturating_sub(1),
                _ => {}
            }
            i += 1;
            continue;
        };
        let k = open.len() - 1;
        let mut keep = false;
        // The bytes this step reads, blanked unless kept.
        let n = match top {
            PyOpen::Str { quote, triple, fields } => {
                if b[i] == quote && (!triple || b[i..].starts_with(&[quote; 3])) {
                    open.pop();
                    low = low.min(open.len());
                    let n = if triple { 3 } else { 1 };
                    // The outermost string's closing quote stays.
                    if open.is_empty() {
                        i += n;
                        continue;
                    }
                    n
                } else if b[i] == b'\\' {
                    escaped_eol = i + 1 == b.len();
                    // `\{` keeps the backslash and still opens a field.
                    if fields && b.get(i + 1) == Some(&b'{') { 1 } else { 2 }
                } else if fields && matches!(b[i], b'{' | b'}') && b.get(i + 1) == Some(&b[i]) {
                    2
                } else {
                    if fields && b[i] == b'{' {
                        open.push(PyOpen::Field { depth: 0, spec: false });
                    }
                    1
                }
            }
            // The format spec is literal text, but for its own fields.
            // A backslash escapes only another backslash here: `\}` still
            // closes the field and `\{` opens one.
            PyOpen::Field { spec: true, .. } => match b[i] {
                b'\\' => {
                    escaped_eol = i + 1 == b.len();
                    if b.get(i + 1) == Some(&b'\\') { 2 } else { 1 }
                }
                b'{' => {
                    open.push(PyOpen::Field { depth: 0, spec: false });
                    1
                }
                b'}' => {
                    open.pop();
                    low = low.min(open.len());
                    1
                }
                _ => 1,
            },
            PyOpen::Field { depth: d, spec: false } => match b[i] {
                // A comment in a field is cut like any other; the field stays open.
                b'#' => {
                    code.truncate(i);
                    break;
                }
                q @ (b'"' | b'\'') => python_open_string(b, i, q, open),
                b'\\' => {
                    escaped_eol = i + 1 == b.len();
                    1
                }
                c => {
                    open[k] = match c {
                        b'(' | b'[' | b'{' => PyOpen::Field { depth: d + 1, spec: false },
                        b')' | b']' => PyOpen::Field { depth: d.saturating_sub(1), spec: false },
                        b'}' if d > 0 => PyOpen::Field { depth: d - 1, spec: false },
                        b':' if d == 0 => PyOpen::Field { depth: 0, spec: true },
                        _ => top,
                    };
                    if c == b'}' && d == 0 {
                        open.pop();
                        low = low.min(open.len());
                    }
                    keep = field_code && !(d == 0 && matches!(c, b'}' | b':'));
                    1
                }
            },
        };
        if !keep {
            for c in code.iter_mut().skip(i).take(n) {
                *c = b' ';
            }
        }
        i += n;
    }
    (escaped_eol, low)
}

/// Drops what a line's end breaks: a single-quoted string the line doesn't
/// continue with a backslash, with the fields it sits in, and the format
/// spec of a single-quoted f-string, with its string.
fn python_drop_broken(open: &mut Vec<PyOpen>, mut escaped_eol: bool) {
    loop {
        match open.last() {
            Some(PyOpen::Str { triple: false, .. }) if !escaped_eol => {
                open.pop();
                python_pop_fields(open);
            }
            Some(PyOpen::Field { spec: true, .. })
                if !escaped_eol && matches!(open.iter().rev().find(|o| matches!(o, PyOpen::Str { .. })), Some(PyOpen::Str { triple: false, .. })) =>
            {
                python_pop_fields(open);
            }
            _ => return,
        }
        escaped_eol = false;
    }
}

/// Pops the fields on top of `open`, back to the string that holds them.
fn python_pop_fields(open: &mut Vec<PyOpen>) {
    while matches!(open.last(), Some(PyOpen::Field { .. })) {
        open.pop();
    }
}

/// Within how many of `rest`'s lines the string `open[first]` closes,
/// scanning on from `open` and spending `budget` a line; None when a line's
/// end breaks it first or it is still open at the end of `rest`, and an error
/// when the budget runs out first.
fn python_closes_within(rest: &[String], open: &[PyOpen], first: usize, budget: &mut usize) -> std::result::Result<Option<usize>, ()> {
    let mut open = open.to_vec();
    let mut depth = 0;
    for (j, line) in rest.iter().enumerate() {
        *budget = budget.checked_sub(1).ok_or(())?;
        let mut code = line.as_bytes().to_vec();
        let (escaped_eol, low) = python_scan_line(line.as_bytes(), &mut code, &mut depth, &mut open, false);
        if low <= first {
            return Ok(Some(j));
        }
        python_drop_broken(&mut open, escaped_eol);
        if open.len() <= first {
            return Ok(None);
        }
    }
    Ok(None)
}

/// One level of Python string syntax open across `python_code_lines`.
#[derive(Clone, Copy)]
enum PyOpen {
    /// A string: its quote byte, whether it is triple-quoted, and whether
    /// `{` opens a field in it (an f- or t-string).
    Str { quote: u8, triple: bool, fields: bool },
    /// A replacement field: its open brackets, and whether it has reached its
    /// format spec (a `:` outside brackets).
    Field { depth: usize, spec: bool },
}

/// Opens the string whose quote `q` is at `b[i]` and returns the quote's length.
fn python_open_string(b: &[u8], i: usize, q: u8, open: &mut Vec<PyOpen>) -> usize {
    let triple = b[i..].starts_with(&[q, q, q]);
    open.push(PyOpen::Str { quote: q, triple, fields: python_interpolates(b, i) });
    if triple { 3 } else { 1 }
}

/// Whether the string whose quote is at `b[q]` is an f- or t-string: the
/// letters right before the quote are a string prefix holding `f` or `t`.
pub(super) fn python_interpolates(b: &[u8], q: usize) -> bool {
    let mut p = q;
    while p > 0 && b[p - 1].is_ascii_alphabetic() {
        p -= 1;
    }
    let prefix = &b[p..q];
    prefix.len() <= 2
        && !(p > 0 && (b[p - 1] == b'_' || b[p - 1].is_ascii_digit() || b[p - 1] >= 0x80))
        && prefix.iter().all(|c| b"rRbBuUfFtT".contains(c))
        && prefix.iter().any(|c| b"fFtT".contains(c))
}

/// The base-class names of the `class` statement at `lines[at]`, read across
/// lines to the closing parenthesis: `Store[int]` names `Store`, keyword
/// arguments (`metaclass=ABCMeta`) name no base.
fn python_base_names(lines: &[String], at: usize) -> Vec<String> {
    let Some(first) = lines.get(at) else { return Vec::new() };
    let Some(m) = re!(r"^\s*class\s+\w+\s*\(").find(first) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let text = std::iter::once(&first[m.end()..]).chain(lines[at + 1..].iter().take(64).map(String::as_str));
    'lines: for line in text {
        for ch in line.chars() {
            match ch {
                '#' => break,
                '(' | '[' | '{' => depth += 1,
                ')' if depth == 0 => break 'lines,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => names.push(std::mem::take(&mut current)),
                _ if depth == 0 => current.push(ch),
                _ => {}
            }
        }
        current.push(' ');
    }
    names.push(current);
    names
        .into_iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && !n.contains('=') && !n.starts_with('*'))
        .collect()
}

/// `Type(...)` — the constructed class of an assigned value.
/// `Store(...)` as a value names `Store`; a conditional expression
/// (`Store() if cond else Other()`) names no one class.
fn constructor_type(value: &str) -> Option<String> {
    let c = re!(r"^(_*[A-Z][\w.]*)\s*\(").captures(value)?;
    let open = c.get(0)?.end() - 1;
    let mut depth = 0usize;
    for (i, b) in value.bytes().enumerate().skip(open) {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if re!(r"^\s*if\s").is_match(&value[i + 1..]) {
                        return None;
                    }
                    break;
                }
            }
            _ => {}
        }
    }
    if let Some(m) = re!(r"^([A-Z]\w*)\.objects\.(?:create|get|first|last|latest|earliest|get_by_natural_key)$").captures(&c[1]) { return Some(m[1].to_string()); }
    Some(c[1].to_string())
}

fn assigned_type(annotation: Option<&str>, value: &str) -> String {
    annotation
        .map(str::to_string)
        .or_else(|| constructor_type(value.trim()))
        .unwrap_or_else(|| UNKNOWN_TYPE.to_string())
}

/// Does `signature` declare a parameter named `param`, annotated or not?
fn has_param(signature: &str, param: &str) -> bool {
    re!(r"[(,*]\s*([A-Za-z_]\w*)\s*(?:[:=,)]|$)").captures_iter(signature).any(|c| &c[1] == param)
}

/// `param: Type` in a signature.
fn param_annotation(signature: &str, param: &str) -> Option<String> {
    re!(r#"(?-u:\b)([A-Za-z_]\w*)\s*:\s*["']?([\w.]+)"#)
        .captures_iter(signature)
        .find(|c| &c[1] == param)
        .map(|c| c[2].to_string())
}

#[cfg(test)]
mod tests {
    use super::python_code_lines;

    fn starts(src: &str) -> Vec<bool> {
        let lines: Vec<String> = src.lines().map(String::from).collect();
        python_code_lines(&lines, 0, lines.len()).into_iter().map(|(s, _)| s).collect()
    }

    #[test]
    fn same_quote_fields_close_where_python_closes_them() {
        // Python 3.12 nests the same quote in a field; quoted brackets and `#` are text.
        assert_eq!(starts("a = f\"{t.replace(\"(\", \"[\")}\"\nb = 1"), [true, true]);
        assert_eq!(starts("a = f\"{d[\"#\"]}\"; b = (\nc)\nd = 1"), [true, false, true]);
        // A field spans lines inside brackets, with a comment of its own.
        assert_eq!(starts("a = f\"{\", \".join([\n  'x',  # \"}\n])}\"\nb = 1"), [true, false, false, true]);
        // Doubled braces, a format spec with a field, `\{`, t- and raw f-strings.
        assert_eq!(starts("a = f\"{{x}} {d[\"k\"]:>{w}} \\{y}\"\nb = rf'{z['(']}' + t\"{d[\"[\"]}\"\nc = 1"), [true, true, true]);
        // A field or a backslash-continued format spec runs on to later lines.
        assert_eq!(starts("a = f\"{1:>\\\n10}\"; b = 1\nc = 1"), [true, false, true]);
        assert_eq!(starts("a = f\"{1\n}\"; b = 1\nc = 1"), [true, false, true]);
        // A field that never closes ends with its line, so later code stays code.
        assert_eq!(starts("a = f\"{x\nb = 1"), [true, true]);
        assert_eq!(starts("a = f\"{(\nb = 1\ndef later():\n    return 1"), [true, true, true, true]);
        assert_eq!(starts("a = f\"\"\"{\"abc\nx\"\"\"\ny = 1"), [true, false, true]);
        // A window that ends inside a string still sees it close beyond.
        let lines: Vec<String> = ["def cb(obj):", "    return f\"{log(\\", "        obj=Decoy(),\\", "        f=obj.fetch\\", "    )}\""].map(String::from).into();
        assert_eq!(python_code_lines(&lines, 0, 4).iter().map(|(s, _)| *s).collect::<Vec<_>>(), [true, true, false, false]);
        // A backslash in a format spec escapes only a backslash.
        let lines = vec!["x = rf\"{1:\\}\"; y = 1".to_string()];
        assert!(python_code_lines(&lines, 0, 1)[0].1.ends_with("\"; y = 1"));
        // A long valid string is never cut short.
        let long = format!("a = \"\\\n{}\"\nb = 1", "x\\\n".repeat(150));
        let s = starts(&long);
        assert_eq!((s[0], s[1..152].iter().any(|s| *s), s[152]), (true, false, true));
        // Past the lookahead budget, a backslash still continues a string.
        let mut lines: Vec<String> = ["def cb(obj):", "    return f\"{log(\\", "        obj=Decoy(),\\"].map(String::from).into();
        lines.extend(std::iter::repeat_n(String::new(), 1005));
        lines.push("    )}\"".to_string());
        assert_eq!(python_code_lines(&lines, 0, 3).iter().map(|(s, _)| *s).collect::<Vec<_>>(), [true, true, false]);
        // A comment in a field is cut, and multibyte text stays aligned.
        let lines: Vec<String> = ["a = f\"{(", "    1  # setattr(s, \"c\", 1)", ")}\" + \"\u{fc}\"  # \u{e9}"].map(String::from).into();
        let code = python_code_lines(&lines, 0, 3);
        assert_eq!(code.iter().map(|(s, _)| *s).collect::<Vec<_>>(), [true, false, false]);
        assert_eq!(code[1].1, "       ");
        assert_eq!(code[2].1, "  \" + \"  \"  ");
        let lines = vec!["x = f\"{d[\"k\"]}\"  # c".to_string()];
        assert_eq!(python_code_lines(&lines, 0, 1)[0].1, "x = f\"        \"  ");
        // Field code can stay, with the same statement starts.
        assert_eq!(super::python_field_code_lines(&lines, 0, 1)[0].1, "x = f\" d[   ] \"  ");
        let lines: Vec<String> = ["    return f\"{(", "        super().render()", "    )}\"", "x = 1"].map(String::from).into();
        let code = super::python_field_code_lines(&lines, 0, 4);
        assert_eq!(code.iter().map(|(s, _)| *s).collect::<Vec<_>>(), [true, false, false, true]);
        assert_eq!(code[1].1, lines[1]);
    }
}
