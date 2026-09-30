//! Proven Kotlin receiver types through declarations and bounded expression walks.
use super::*;
use super::iteration::{descendant_for_position, named_children, site_at};
use tree_sitter::Node;

pub(super) struct KotlinReceiverHit {
    pub(super) ty: String,
    pub(super) site: ResolveRefIn,
    pub(super) heuristic: bool,
}

fn text<'a>(node: Node, source: &'a str) -> &'a str { &source[node.start_byte()..node.end_byte()] }
fn simple_type(raw: &str) -> &str { raw.split('<').next().unwrap_or(raw).trim().trim_end_matches('?') }
fn property(node: Node) -> Option<Node> {
    let child = node.named_child(1)?;
    if child.kind() == "navigation_suffix" {
        named_children(child).into_iter().find(|n| n.kind() == "simple_identifier")
    } else { Some(child) }
}

impl KernelResolver {
    pub(super) fn kotlin_call_receiver_type(&mut self, r: &ResolveRefIn) -> Res<Option<KotlinReceiverHit>> {
        if r.language != "kotlin" || r.reference_kind != "calls" { return Ok(None); }
        let Some(file) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&file, r) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), file.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1));
        if node.kind() != "simple_identifier" { return Ok(None); }
        while let Some(parent) = node.parent() {
            if parent.kind() == "navigation_expression" {
                let Some(receiver) = parent.named_child(0) else { return Ok(None) };
                return self.kotlin_expression_type(receiver, file.text(), r, None, 0);
            }
            if parent.kind() != "navigation_suffix" { return Ok(None); }
            node = parent;
        }
        Ok(None)
    }

    fn kotlin_type_ancestors(&mut self, hit: &KotlinReceiverHit) -> Res<Vec<Arc<KNode>>> {
        let Some(root) = self.resolve_bound_type(&hit.ty, &hit.site, 0)? else { return Ok(vec![]) };
        let mut queue = VecDeque::from([root]);
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        while result.len() < 32 {
            let Some(node) = queue.pop_front() else { break };
            if !seen.insert(node.id.clone()) { continue; }
            queue.extend(self.supertype_nodes(&node.id)?);
            result.push(node);
        }
        Ok(result)
    }

    pub(super) fn kotlin_receiver_accepts(&mut self, n: &KNode, hit: &KotlinReceiverHit, r: &ResolveRefIn) -> Res<bool> {
        let Some(owner_name) = super::call_shape::owner(n) else { return Ok(false) };
        let site = r.clone().at(n);
        let Some(owner) = self.resolve_bound_type(owner_name, &site, 0)? else { return Ok(false) };
        if !self.kotlin_type_ancestors(hit)?.iter().any(|a| a.id == owner.id) { return Ok(false); }
        if n.file_path == owner.file_path && n.qualified_name == format!("{}::{}", owner.qualified_name, n.name) { return Ok(true); }
        self.kotlin_top_level_visible(n, r)
    }

    fn kotlin_receiver_member(&mut self, hit: &KotlinReceiverHit, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        if let Some(member) = self.match_bound_type_member(&hit.ty, name, &hit.site)? { return Ok(Some(member.node)); }
        let candidates: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| n.language == "kotlin" && n.kind == "method").cloned().collect();
        let mut matching = Vec::new();
        for candidate in candidates {
            if self.kotlin_receiver_accepts(&candidate, hit, r)? { matching.push(candidate); }
        }
        let Some(first) = matching.first() else { return Ok(None) };
        if matching.iter().all(|m| m.qualified_name == first.qualified_name && m.file_path == first.file_path) { Ok(Some(first.clone())) }
        else { Ok(None) }
    }

    fn kotlin_expression_type(&mut self, node: Node, source: &str, r: &ResolveRefIn, implicit: Option<&KotlinReceiverHit>, depth: usize) -> Res<Option<KotlinReceiverHit>> {
        if depth > 8 { return Ok(None); }
        let site = site_at(node, source, r);
        if node.kind() == "simple_identifier" {
            let name = text(node, source);
            if let Some(ty) = self.infer_local_receiver_type(name, &site, true)? {
                return Ok(Some(KotlinReceiverHit { ty: simple_type(&ty).to_string(), site, heuristic: false }));
            }
            return Ok(self.resolve_bound_type(name, &site, 0)?.map(|_| KotlinReceiverHit { ty: name.to_string(), site, heuristic: false }));
        }
        if node.kind() == "navigation_expression" {
            let (Some(root), Some(member)) = (node.named_child(0), property(node)) else { return Ok(None) };
            let Some(hit) = self.kotlin_expression_type(root, source, &site, implicit, depth + 1)? else { return Ok(None) };
            for owner in self.kotlin_type_ancestors(&hit)? {
                let properties: Vec<_> = self.nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, text(member, source)))?.iter()
                    .filter(|n| n.file_path == owner.file_path && matches!(n.kind.as_str(), "property" | "field" | "variable" | "constant")).cloned().collect();
                if let [decl] = properties.as_slice() {
                    if let Some(raw) = &decl.return_type { return Ok(Some(KotlinReceiverHit { ty: simple_type(raw).to_string(), site: site.clone().at(decl), heuristic: hit.heuristic })); }
                    let mut inferred = self.kotlin_property_initializer(decl, &owner, &site, depth + 1)?;
                    if let Some(value) = &mut inferred { value.heuristic |= hit.heuristic; }
                    return Ok(inferred);
                }
                if !properties.is_empty() { return Ok(None); }
            }
            return Ok(None);
        }
        if node.kind() != "call_expression" { return Ok(None); }
        let Some(func) = node.named_child(0) else { return Ok(None) };
        let (member, receiver) = if func.kind() == "navigation_expression" {
            let (Some(root), Some(name)) = (func.named_child(0), property(func)) else { return Ok(None) };
            let hit = match self.kotlin_expression_type(root, source, &site, implicit, depth + 1)? {
                Some(hit) => hit,
                None if root.kind() == "simple_identifier" => return self.kotlin_imported_return_hypothesis(text(name, source), &site),
                None => return Ok(None),
            };
            let member = self.kotlin_receiver_member(&hit, text(name, source), &site)?;
            (member, Some(hit))
        } else if func.kind() == "simple_identifier" {
            let name = text(func, source);
            let bindings = self.bindings(&site.file_path)?;
            let binding = innermost_binding(&bindings, name, Some(site.line));
            if let Some(bound) = binding.filter(|b| b.kind != "import") {
                if !self.node_by_opt_id(bound.node_id.as_deref())?.is_some_and(|n| matches!(n.kind.as_str(), "function" | "method" | "class" | "interface" | "enum" | "type_alias")) { return Ok(None); }
            }
            if let Some(owner) = self.resolve_bound_type(name, &site, 0)? {
                return Ok(Some(KotlinReceiverHit { ty: owner.name.clone(), site: site.clone().at(&owner), heuristic: false }));
            }
            let mut member = match implicit { Some(hit) => self.kotlin_receiver_member(hit, name, &site)?, None => None };
            if member.is_none() {
                match binding {
                    Some(binding) if binding.kind == "import" => {
                        member = self.resolve_via_import(&site.clone().naming(name, "calls"))?.map(|c| c.node).filter(|n| n.kind == "function");
                        if member.is_none() {
                            let mappings = self.import_mappings(&site.file_path)?;
                            let candidates: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| n.language == "kotlin" && n.kind == "function").cloned().collect();
                            let mut imported = Vec::new();
                            for n in candidates {
                                let pkg = self.kotlin_file_scope(&n.file_path).pkg.clone();
                                if mappings.iter().any(|m| m.local_name == name && m.source == format!("{pkg}.{}", n.name) && m.source == n.qualified_name.replace("::", ".")) && self.kotlin_top_level_visible(&n, &site)? { imported.push(n); }
                            }
                            if let [only] = imported.as_slice() { member = Some(only.clone()); }
                        }
                    }
                    Some(binding) => { member = self.node_by_opt_id(binding.node_id.as_deref())?.filter(|n| n.kind == "function"); }
                    None => {
                        let candidates: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| n.language == "kotlin" && n.kind == "function").cloned().collect();
                        let mut visible = Vec::new();
                        for n in candidates { if self.kotlin_top_level_visible(&n, &site)? { visible.push(n); } }
                        if let [only] = visible.as_slice() { member = Some(only.clone()); }
                    }
                }
            }
            (member, implicit.map(|h| KotlinReceiverHit { ty: h.ty.clone(), site: h.site.clone(), heuristic: h.heuristic }))
        } else { return Ok(None); };
        let Some(member) = member else { return Ok(None) };
        let overloads: Vec<_> = self.nodes_by_qualified_name(&member.qualified_name)?.iter().filter(|m| m.file_path == member.file_path && m.kind == member.kind).cloned().collect();
        let mut result: Option<KotlinReceiverHit> = None;
        for overload in overloads {
            let next = match &overload.return_type {
                Some(raw) => {
                    let ty = simple_type(raw);
                    if overload.type_parameters.as_ref().is_some_and(|ps| ps.iter().any(|p| p.split_whitespace().next() == Some(ty))) { return Ok(None); }
                    Some(KotlinReceiverHit { ty: ty.to_string(), site: site.clone().at(&overload), heuristic: receiver.as_ref().is_some_and(|h| h.heuristic) })
                }
                None => match &receiver {
                    Some(hit) if self.kotlin_preserves_receiver(&overload, hit, &site)? => Some(KotlinReceiverHit { ty: hit.ty.clone(), site: hit.site.clone(), heuristic: hit.heuristic }),
                    _ => None,
                },
            };
            let Some(next) = next else { return Ok(None) };
            if let Some(previous) = &result {
                let a = self.resolve_bound_type(&previous.ty, &previous.site, 0)?;
                let b = self.resolve_bound_type(&next.ty, &next.site, 0)?;
                if !a.zip(b).is_some_and(|(a,b)| a.id == b.id) { return Ok(None); }
            }
            result = Some(next);
        }
        Ok(result)
    }

    fn kotlin_imported_return_hypothesis(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<KotlinReceiverHit>> {
        let mappings = self.import_mappings(&r.file_path)?;
        let candidates: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| n.language == "kotlin" && n.kind == "method").cloned().collect();
        let mut imported = Vec::new();
        for n in candidates {
            let pkg = self.kotlin_file_scope(&n.file_path).pkg.clone();
            if !mappings.iter().any(|m| m.local_name == name && m.source == format!("{pkg}.{name}")) { continue; }
            if self.nodes_in_file(&n.file_path)?.iter().any(|o| super::call_shape::type_kind(&o.kind) && !matches!(o.kind.as_str(), "namespace" | "module") && o.start_line <= n.start_line && o.end_line >= n.end_line) { continue; }
            imported.push(n);
        }
        let Some(factory) = imported.first() else { return Ok(None) };
        let Some(raw) = &factory.return_type else { return Ok(None) };
        let hit = KotlinReceiverHit { ty: simple_type(raw).to_string(), site: r.clone().at(factory), heuristic: true };
        let Some(owner) = self.resolve_bound_type(&hit.ty, &hit.site, 0)? else { return Ok(None) };
        // A declared incompatible member can shadow the imported extension.
        let competing: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| matches!(n.language.as_str(), "kotlin" | "java") && matches!(n.kind.as_str(), "function" | "method")).cloned().collect();
        for member in competing {
            let Some(raw) = &member.return_type else { continue };
            if !self.resolve_bound_type(simple_type(raw), &r.clone().at(&member), 0)?.is_some_and(|other| other.id == owner.id) { return Ok(None); }
        }
        Ok(Some(hit))
    }

    fn kotlin_property_initializer(&mut self, decl: &KNode, owner: &KNode, r: &ResolveRefIn, depth: usize) -> Res<Option<KotlinReceiverHit>> {
        let Some(file) = self.read_file(&decl.file_path) else { return Ok(None) };
        let site = r.clone().at(decl);
        let Some(tree) = self.parsed_tree(&file, &site) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), file.text(), ((decl.start_line - 1).max(0) as usize, decl.start_column.max(0) as usize + 1));
        while node.kind() != "property_declaration" {
            let Some(parent) = node.parent() else { return Ok(None) }; node = parent;
        }
        let Some(initializer) = named_children(node).into_iter().last().filter(|n| n.kind() == "call_expression") else { return Ok(None) };
        let implicit = KotlinReceiverHit { ty: owner.name.clone(), site: site.clone().at(owner), heuristic: false };
        self.kotlin_expression_type(initializer, file.text(), &site, Some(&implicit), depth)
    }

    fn kotlin_preserves_receiver(&mut self, decl: &KNode, receiver: &KotlinReceiverHit, r: &ResolveRefIn) -> Res<bool> {
        let Some(file) = self.read_file(&decl.file_path) else { return Ok(false) };
        let site = r.clone().at(decl);
        let Some(tree) = self.parsed_tree(&file, &site) else { return Ok(false) };
        let mut node = descendant_for_position(tree.root_node(), file.text(), ((decl.start_line - 1).max(0) as usize, decl.start_column.max(0) as usize + 1));
        while node.kind() != "function_declaration" {
            let Some(parent) = node.parent() else { return Ok(false) }; node = parent;
        }
        let Some(body) = named_children(node).into_iter().find(|n| n.kind() == "function_body") else { return Ok(false) };
        let children: Vec<_> = named_children(body).into_iter().filter(|n| !n.is_extra()).collect();
        let [expr] = children.as_slice() else { return Ok(false) };
        if !text(body, file.text()).trim_start().starts_with('=') { return Ok(false); }
        if expr.kind() == "this_expression" { return Ok(text(*expr, file.text()).trim() == "this"); }
        if expr.kind() != "call_expression" { return Ok(false); }
        let Some(func) = expr.named_child(0) else { return Ok(false) };
        if func.kind() != "simple_identifier" || text(func, file.text()) != "apply" { return Ok(false); }
        let call_site = site_at(func, file.text(), &site);
        let bindings = self.bindings(&site.file_path)?;
        if let Some(binding) = innermost_binding(&bindings, "apply", Some(call_site.line)) {
            if binding.kind != "import" || !self.import_mappings(&site.file_path)?.iter().any(|m| m.local_name == "apply" && m.source == "kotlin.apply") { return Ok(false); }
        }
        // A project scope function can return a different type.
        Ok(!self.kotlin_user_member(&receiver.ty, "apply", &site)? && !self.kotlin_user_scope_callee(func, "apply", file.text(), &site)?)
    }
}
