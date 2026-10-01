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

    /// Top-level extensions need package visibility; member extensions need a reachable dispatch receiver.
    pub(super) fn kotlin_visible_extension(&mut self,n:&KNode,r:&ResolveRefIn)->Res<bool> {
        if n.language!="kotlin" || n.kind!="method" {return Ok(false);}
        let Some(owner)=super::call_shape::owner(n) else {return Ok(false)};
        let dispatch=self.nodes_in_file(&n.file_path)?.iter().filter(|o|super::call_shape::type_kind(&o.kind) && !matches!(o.kind.as_str(),"namespace"|"module") && o.start_line<=n.start_line && o.end_line>=n.end_line).min_by_key(|o|o.end_line-o.start_line).cloned();
        if dispatch.as_ref().is_some_and(|o|o.name==owner) {return Ok(false);}
        Ok(if dispatch.is_some() {self.kotlin_member_reachable(n,r)?} else {self.kotlin_top_level_visible(n,r)?})
    }

    pub(super) fn kotlin_unknown_extension_target(&mut self,n:&KNode,r:&ResolveRefIn)->Res<bool> {
        Ok(self.kotlin_visible_extension(n,r)? && self.kotlin_unknown_receiver_allowed(r)?)
    }

    pub(super) fn kotlin_unknown_receiver_allowed(&mut self,r:&ResolveRefIn)->Res<bool> {
        if let Some(receiver)=self.kotlin_chain_receiver_call(r) {
            if let Some(call)=re!(r"^\s*([A-Za-z_]\w*)\s*\(").captures(&receiver) {
                let bindings=self.bindings(&r.file_path)?;
                if let Some(binding)=innermost_binding(&bindings,&call[1],Some(r.line)).filter(|b|b.kind!="import") {
                    let node=self.node_by_opt_id(binding.node_id.as_deref())?;
                    if !node.as_ref().is_some_and(|n|matches!(n.kind.as_str(),"function"|"method"|"class")) {
                        let name=regex::escape(&call[1]);
                        let pattern=Self::cached_regex(&format!(r"\b{name}\s*:\s*(?:[A-Za-z_][\w?.<>]*\s*\.\s*)?\([^)]*\)\s*->|\b(?:val|var)\s+{name}(?:\s*:[^=\n]*)?\s*=\s*\(?\s*(?:\{{|(?:[A-Za-z_][\w.]*\s*)?::|fun\b)"))?;
                        let source=self.read_file(&r.file_path).map(|file| {
                            let start=(binding.line-1).max(0) as usize;
                            file.get(start..(start+8).min(file.len())).unwrap_or(&[]).join("\n")
                        }).unwrap_or_default();
                        let alias_callable=match self.infer_local_receiver_type(&call[1],r,true)? {
                            Some(ty)=>match self.resolve_bound_type(&ty,r,0)? {
                                Some(alias) if alias.kind=="type_alias"=>self.read_file(&alias.file_path).is_some_and(|file| {
                                    let start=(alias.start_line-1).max(0) as usize;
                                    let end=(alias.end_line.max(alias.start_line) as usize).min(file.len()).min(start+8);
                                    let declaration=file.get(start..end).unwrap_or(&[]).join("\n");
                                    super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(&declaration)).contains("->")
                                }),
                                _=>false,
                            },
                            None=>false,
                        };
                        if alias_callable || pattern.is_match(&super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(&source))) || node.as_ref().and_then(|n|n.return_type.as_deref()).is_some_and(|ty|ty.contains("->")) {return Ok(false);}
                    }
                }
            }
        }
        Ok(true)
    }

    fn kotlin_property_chain_receiver(&mut self,r:&ResolveRefIn)->bool {
        let Some(file)=self.read_file(&r.file_path) else {return false};
        let Some(tree)=self.parsed_tree(&file,r) else {return false};
        let mut node=descendant_for_position(tree.root_node(),file.text(),((r.line-1).max(0) as usize,r.column.max(0) as usize+1));
        while let Some(parent)=node.parent() {
            if parent.kind()=="navigation_expression" {
                let Some(receiver)=parent.named_child(0) else {return false};
                let code=super::awaited::strip_ts_comments(text(receiver,file.text()));
                return re!(r"[?!]*\s*\.\s*[A-Za-z_]\w*[?!]*\s*$").is_match(&code);
            }
            if parent.kind()!="navigation_suffix" {return false;}
            node=parent;
        }
        false
    }

    pub(super) fn kotlin_unique_unknown_member(&mut self,n:&KNode,r:&ResolveRefIn)->Res<bool> {
        if n.kind!="method" || super::call_shape::is_std_method("kotlin",&n.name) || !matches!(n.language.as_str(),"kotlin"|"java") || self.kotlin_visible_extension(n,r)? || !self.kotlin_unknown_receiver_allowed(r)? || !self.kotlin_property_chain_receiver(r) {return Ok(false);}
        let groups:HashSet<_>=self.nodes_by_name(&n.name)?.iter().filter(|m|matches!(m.language.as_str(),"kotlin"|"java") && m.kind=="method").map(|m|(m.file_path.clone(),m.qualified_name.clone())).collect();
        Ok(groups.len()==1 && (n.visibility.as_deref()!=Some("private") || self.kotlin_member_reachable(n,r)?))
    }

    pub(super) fn kotlin_receiver_accepts(&mut self, n: &KNode, hit: &KotlinReceiverHit, r: &ResolveRefIn) -> Res<bool> {
        let Some(owner_name) = super::call_shape::owner(n) else { return Ok(false) };
        let site = r.clone().at(n);
        let extension=self.kotlin_visible_extension(n,r)?;
        let Some(owner) = self.resolve_bound_type(owner_name, &site, 0)? else {
            // Builtin receiver types have no project declaration to traverse.
            return Ok(extension && simple_type(&hit.ty)==simple_type(owner_name));
        };
        if !self.kotlin_type_ancestors(hit)?.iter().any(|a| a.id == owner.id) { return Ok(false); }
        if n.file_path == owner.file_path && n.qualified_name == format!("{}::{}", owner.qualified_name, n.name) { return Ok(true); }
        Ok(extension)
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

    fn kotlin_decl_tree_node<'t>(tree: &'t tree_sitter::Tree, source: &str, decl: &KNode, kind: &str) -> Option<Node<'t>> {
        let mut node=descendant_for_position(tree.root_node(),source,((decl.start_line-1).max(0) as usize,decl.start_column.max(0) as usize+1));
        while node.kind()!=kind {node=node.parent()?;}
        Some(node)
    }

    fn kotlin_type_argument(node: Node, source: &str) -> Option<String> {
        let suffix=named_children(node).into_iter().find(|n|n.kind()=="call_suffix")?;
        let args=named_children(suffix).into_iter().find(|n|n.kind()=="type_arguments")?;
        let projections=named_children(args); let [projection]=projections.as_slice() else {return None};
        let ty=named_children(*projection).into_iter().find(|n|n.kind()=="user_type")?;
        Some(simple_type(text(ty,source)).to_string())
    }

    fn kotlin_local_property<'t>(root:Node<'t>,source:&str,binding:&KBinding)->Option<Node<'t>> {
        let row=(binding.line-1).max(0) as usize; let mut stack=vec![root]; let mut matching=Vec::new();
        while let Some(node)=stack.pop() {
            if node.start_position().row>row || node.end_position().row<row {continue;}
            if node.kind()=="property_declaration" && node.start_position().row==row {
                let name=named_children(node).into_iter().find(|n|n.kind()=="variable_declaration").and_then(|n|n.named_child(0));
                if name.is_some_and(|n|text(n,source)==binding.name) {matching.push(node);}
            }
            stack.extend(named_children(node));
        }
        match matching.as_slice() {[only]=>Some(*only),_=>None}
    }

    fn kotlin_collection_element(&mut self,node:Node,source:&str,r:&ResolveRefIn,depth:usize)->Res<Option<KotlinReceiverHit>> {
        if node.kind()!="simple_identifier" {return Ok(None);}
        let bindings=self.bindings(&r.file_path)?;
        let Some(binding)=innermost_binding(&bindings,text(node,source),Some(r.line)) else {return Ok(None)};
        let decl=self.node_by_opt_id(binding.node_id.as_deref())?;
        let path=decl.as_ref().map_or(r.file_path.as_str(),|n|n.file_path.as_str());
        let Some(file)=self.read_file(path) else {return Ok(None)};
        let mut site=decl.as_ref().map_or_else(||r.clone(),|n|r.clone().at(n)); site.line=binding.line;
        let Some(tree)=self.parsed_tree(&file,&site) else {return Ok(None)};
        let prop=match &decl {Some(decl)=>Self::kotlin_decl_tree_node(&tree,file.text(),decl,"property_declaration"),None=>Self::kotlin_local_property(tree.root_node(),file.text(),binding)};
        let Some(prop)=prop else {return Ok(None)};
        let Some(init)=named_children(prop).into_iter().last().filter(|n|n.kind()=="call_expression") else {return Ok(None)};
        let Some(func)=init.named_child(0).filter(|n|n.kind()=="navigation_expression") else {return Ok(None)};
        if !property(func).is_some_and(|n|text(n,file.text())=="filterIsInstance") {return Ok(None);}
        let Some(root)=func.named_child(0) else {return Ok(None)};
        let receiver=self.kotlin_expression_type(root,file.text(),&site,None,depth+1)?;
        if let Some(hit)=&receiver {
            if self.match_bound_type_member(&hit.ty,"filterIsInstance",&hit.site)?.is_some() {return Ok(None);}
        }
        let competitors=self.nodes_by_name("filterIsInstance")?;
        for candidate in competitors.iter().filter(|n|n.language=="kotlin" && matches!(n.kind.as_str(),"function"|"method")) {
            if candidate.kind=="function" && self.kotlin_top_level_visible(candidate,&site)? {return Ok(None);}
            if self.kotlin_visible_extension(candidate,&site)? && match &receiver {Some(hit)=>self.kotlin_receiver_accepts(candidate,hit,&site)?,None=>true} {return Ok(None);}
        }
        let Some(ty)=Self::kotlin_type_argument(init,file.text()) else {return Ok(None)};
        Ok(self.resolve_bound_type(&ty,&site,0)?.map(|_|KotlinReceiverHit{ty,site,heuristic:false}))
    }

    fn kotlin_narrowed_identifier(&mut self,node:Node,source:&str,r:&ResolveRefIn,depth:usize)->Res<Option<KotlinReceiverHit>> {
        let name=text(node,source); let mut parent=node.parent();
        while let Some(cur)=parent {parent=cur.parent();
            if cur.kind()=="lambda_literal" && self.kotlin_lambda_names(cur,source,r)?.contains(&name) {
                let Some(call)=super::iteration::kotlin_lambda_call(cur) else {return Ok(None)};
                let Some(nav)=call.named_child(0).filter(|n|n.kind()=="navigation_expression") else {return Ok(None)};
                let Some(method)=property(nav) else {return Ok(None)};
                if !matches!(text(method,source),"let"|"also") {return Ok(None);}
                let Some(root)=nav.named_child(0) else {return Ok(None)};
                let Some(hit)=self.kotlin_expression_type(root,source,r,None,depth+1)? else {return Ok(None)};
                if self.kotlin_user_member(&hit.ty,text(method,source),&hit.site)? || self.kotlin_user_scope_callee(nav,text(method,source),source,r)? {return Ok(None);}
                let extensions=self.nodes_by_name(text(method,source))?;
                for extension in extensions.iter().filter(|n|n.language=="kotlin") {
                    if self.kotlin_visible_extension(extension,r)? && self.kotlin_receiver_accepts(extension,&hit,r)? {return Ok(None);}
                }
                return Ok(Some(hit));
            }
            if cur.kind()=="when_entry" {
                let conditions:Vec<_>=named_children(cur).into_iter().filter(|n|n.kind()=="when_condition").collect();
                let [condition]=conditions.as_slice() else {return Ok(None)};
                let Some(test)=condition.named_child(0).filter(|n|n.kind()=="type_test") else {return Ok(None)};
                if !text(test,source).trim_start().starts_with("is ") {return Ok(None);}
                let Some(when)=cur.parent().filter(|n|n.kind()=="when_expression") else {return Ok(None)};
                let subject=named_children(when).into_iter().find(|n|n.kind()=="when_subject").and_then(|n|n.named_child(0));
                if !subject.is_some_and(|n|n.kind()=="simple_identifier" && text(n,source)==name) {return Ok(None);}
                let bindings=self.bindings(&r.file_path)?;
                if let Some(binding)=innermost_binding(&bindings,name,Some(r.line)) {
                    if binding.line>when.start_position().row as i64+1 || Self::kotlin_local_property(when,source,binding).is_some_and(|decl|decl.start_byte()>when.start_byte() && decl.start_byte()<node.start_byte()) {return Ok(None);}
                }
                let Some(ty)=named_children(test).into_iter().find(|n|n.kind()=="user_type") else {return Ok(None)};
                let ty=simple_type(text(ty,source)).to_string();
                return Ok(self.resolve_bound_type(&ty,r,0)?.map(|_|KotlinReceiverHit{ty,site:r.clone(),heuristic:false}));
            }
        }
        Ok(None)
    }

    fn kotlin_constructor_return(&mut self,decl:&KNode,r:&ResolveRefIn)->Res<Option<KotlinReceiverHit>> {
        let Some(file)=self.read_file(&decl.file_path) else {return Ok(None)};
        let site=r.clone().at(decl); let Some(tree)=self.parsed_tree(&file,&site) else {return Ok(None)};
        let Some(func)=Self::kotlin_decl_tree_node(&tree,file.text(),decl,"function_declaration") else {return Ok(None)};
        let Some(body)=named_children(func).into_iter().find(|n|n.kind()=="function_body") else {return Ok(None)};
        if !text(body,file.text()).trim_start().starts_with('=') {return Ok(None);}
        let children=named_children(body); let [call]=children.as_slice() else {return Ok(None)};
        if call.kind()!="call_expression" {return Ok(None);}
        let Some(name)=call.named_child(0).filter(|n|n.kind()=="simple_identifier") else {return Ok(None)};
        let ty=text(name,file.text()).to_string();
        let bindings=self.bindings(&decl.file_path)?;
        if let Some(binding)=innermost_binding(&bindings,&ty,Some(site.line)) {
            if binding.kind!="import" && !self.node_by_opt_id(binding.node_id.as_deref())?.is_some_and(|n|super::call_shape::type_kind(&n.kind)) {return Ok(None);}
        }
        Ok(self.resolve_bound_type(&ty,&site,0)?.map(|_|KotlinReceiverHit{ty,site,heuristic:false}))
    }

    fn kotlin_declares_return_parameter(&mut self,decl:&KNode,ty:&str,r:&ResolveRefIn)->Res<bool> {
        let Some(file)=self.read_file(&decl.file_path) else {return Ok(false)};
        let site=r.clone().at(decl); let Some(tree)=self.parsed_tree(&file,&site) else {return Ok(false)};
        let kind=if matches!(decl.kind.as_str(),"function"|"method") {"function_declaration"} else {"class_declaration"};
        let Some(func)=Self::kotlin_decl_tree_node(&tree,file.text(),decl,kind) else {return Ok(false)};
        let Some(params)=named_children(func).into_iter().find(|n|n.kind()=="type_parameters") else {return Ok(false)};
        Ok(named_children(params).iter().any(|p|named_children(*p).iter().any(|n|n.kind()=="type_identifier" && text(*n,file.text())==ty)))
    }

    fn kotlin_generic_argument_return(&mut self,decl:&KNode,call:Node,source:&str,r:&ResolveRefIn)->Res<Option<KotlinReceiverHit>> {
        let Some(raw)=decl.return_type.as_deref() else {return Ok(None)};
        let Some(file)=self.read_file(&decl.file_path) else {return Ok(None)};
        let declaration=r.clone().at(decl); let Some(tree)=self.parsed_tree(&file,&declaration) else {return Ok(None)};
        let Some(func)=Self::kotlin_decl_tree_node(&tree,file.text(),decl,"function_declaration") else {return Ok(None)};
        let Some(params)=named_children(func).into_iter().find(|n|n.kind()=="type_parameters") else {return Ok(None)};
        if !named_children(params).iter().any(|p|named_children(*p).iter().any(|n|n.kind()=="type_identifier" && text(*n,file.text())==raw)) {return Ok(None);}
        let Some(values)=named_children(func).into_iter().find(|n|n.kind()=="function_value_parameters") else {return Ok(None)};
        let parameters=named_children(values); let [parameter]=parameters.as_slice() else {return Ok(None)};
        let Some(ty)=named_children(*parameter).into_iter().find(|n|n.kind()=="user_type") else {return Ok(None)};
        let pattern=Self::cached_regex(&format!(r"^([\w.]+)\s*<\s*{}\s*>$",regex::escape(raw)))?;
        let Some(expected)=pattern.captures(text(ty,file.text())) else {return Ok(None)};
        let Some(suffix)=named_children(call).into_iter().find(|n|n.kind()=="call_suffix") else {return Ok(None)};
        let Some(values)=named_children(suffix).into_iter().find(|n|n.kind()=="value_arguments") else {return Ok(None)};
        let arguments=named_children(values); let [argument]=arguments.as_slice() else {return Ok(None)};
        let Some(arg)=argument.named_child(0).filter(|n|n.kind()=="simple_identifier") else {return Ok(None)};
        let name=text(arg,source); let bindings=self.bindings(&r.file_path)?;
        let Some(binding)=innermost_binding(&bindings,name,Some(r.line)) else {return Ok(None)};
        let bound=if binding.kind=="import" {
            let mappings=self.import_mappings(&r.file_path)?;
            let candidates=self.nodes_by_name(name)?;
            let matching:Vec<_>=candidates.iter().filter(|n|mappings.iter().any(|m|m.local_name==name && m.source.replace(".Companion.",".")==n.qualified_name.replace("::","."))).cloned().collect();
            match matching.as_slice() {[only]=>Some(only.clone()),_=>None}
        } else {self.node_by_opt_id(binding.node_id.as_deref())?};
        let Some(bound)=bound else {return Ok(None)};
        let Some(arg_file)=self.read_file(&bound.file_path) else {return Ok(None)};
        let arg_site=r.clone().at(&bound); let Some(arg_tree)=self.parsed_tree(&arg_file,&arg_site) else {return Ok(None)};
        let Some(prop)=Self::kotlin_decl_tree_node(&arg_tree,arg_file.text(),&bound,"property_declaration") else {return Ok(None)};
        let Some(init)=named_children(prop).into_iter().last().filter(|n|n.kind()=="call_expression") else {return Ok(None)};
        let Some(ctor)=init.named_child(0).filter(|n|n.kind()=="simple_identifier") else {return Ok(None)};
        let expected_type=self.resolve_bound_type(&expected[1],&declaration,0)?;
        let actual_type=self.resolve_bound_type(text(ctor,arg_file.text()),&arg_site,0)?;
        if !expected_type.zip(actual_type).is_some_and(|(a,b)|a.id==b.id) {return Ok(None);}
        let Some(ty)=Self::kotlin_type_argument(init,arg_file.text()) else {return Ok(None)};
        Ok(self.resolve_bound_type(&ty,&arg_site,0)?.map(|_|KotlinReceiverHit{ty,site:arg_site,heuristic:false}))
    }

    fn kotlin_expression_type(&mut self, node: Node, source: &str, r: &ResolveRefIn, implicit: Option<&KotlinReceiverHit>, depth: usize) -> Res<Option<KotlinReceiverHit>> {
        if depth > 8 { return Ok(None); }
        let site = site_at(node, source, r);
        if node.kind()=="parenthesized_expression" {return match node.named_child(0) {Some(inner)=>self.kotlin_expression_type(inner,source,r,implicit,depth+1),None=>Ok(None)};}
        if node.kind()=="as_expression" {
            let Some(ty)=named_children(node).into_iter().find(|n|n.kind()=="user_type") else {return Ok(None)};
            let ty=simple_type(text(ty,source)).to_string();
            return Ok(self.resolve_bound_type(&ty,&site,0)?.map(|_|KotlinReceiverHit{ty,site,heuristic:false}));
        }
        if node.kind() == "simple_identifier" {
            if let Some(hit)=self.kotlin_narrowed_identifier(node,source,&site,depth)? {return Ok(Some(hit));}
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
                    if let Some(raw)=self.declared_member_type(&owner,&decl.name)? {
                        if self.kotlin_declares_return_parameter(&owner,simple_type(&raw),&site)? {return Ok(None);}
                        return Ok(Some(KotlinReceiverHit{ty:simple_type(&raw).to_string(),site:site.clone().at(&owner),heuristic:hit.heuristic}));
                    }
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
            if text(name,source)=="find" && !self.kotlin_user_scope_callee(func,"find",source,&site)? {
                let overrides=self.nodes_by_name("find")?; let mut overridden=false;
                for candidate in overrides.iter().filter(|n|n.language=="kotlin") {
                    if self.kotlin_visible_extension(candidate,&site)? && super::call_shape::owner(candidate).is_some_and(|o|matches!(simple_type(o),"List"|"MutableList"|"Collection"|"Iterable"|"Sequence")) {overridden=true;break;}
                }
                if !overridden {if let Some(hit)=self.kotlin_collection_element(root,source,&site,depth+1)? {return Ok(Some(hit));}}
            }
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
                    Some(binding) => {
                        if let Some(n)=self.node_by_opt_id(binding.node_id.as_deref())? {
                            if n.kind=="function" || (n.kind=="method" && self.kotlin_member_reachable(&n,&site)?) {member=Some(n);}
                        }
                    }
                    None => {
                        let enclosing=self.nodes_in_file(&site.file_path)?.iter().filter(|n|super::call_shape::type_kind(&n.kind) && n.start_line<=site.line && n.end_line>=site.line).min_by_key(|n|n.end_line-n.start_line).cloned();
                        if let Some(owner)=enclosing {member=self.match_bound_type_member(&owner.name,name,&site)?.map(|c|c.node);}
                        let candidates: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| n.language == "kotlin" && n.kind == "function").cloned().collect();
                        let mut visible = Vec::new();
                        for n in candidates { if self.kotlin_top_level_visible(&n, &site)? { visible.push(n); } }
                        if member.is_none() {if let [only] = visible.as_slice() { member = Some(only.clone()); }}
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
                    if let Some(hit)=self.kotlin_generic_argument_return(&overload,node,source,&site)? {Some(hit)}
                    else {
                        if self.kotlin_declares_return_parameter(&overload,ty,&site)? || overload.type_parameters.as_ref().is_some_and(|ps| ps.iter().any(|p| p.split_whitespace().next() == Some(ty))) { return Ok(None); }
                        Some(KotlinReceiverHit { ty: ty.to_string(), site: site.clone().at(&overload), heuristic: receiver.as_ref().is_some_and(|h| h.heuristic) })
                    }
                }
                None => match &receiver {
                    Some(hit) if self.kotlin_preserves_receiver(&overload, hit, &site)? => Some(KotlinReceiverHit { ty: hit.ty.clone(), site: hit.site.clone(), heuristic: hit.heuristic }),
                    _ => self.kotlin_constructor_return(&overload,&site)?,
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
        let candidates: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| n.language == "kotlin" && n.kind == "method").cloned().collect();
        let mut imported = Vec::new();
        for n in candidates {
            if !self.kotlin_top_level_visible(&n,r)? {continue;}
            if self.nodes_in_file(&n.file_path)?.iter().any(|o| super::call_shape::type_kind(&o.kind) && !matches!(o.kind.as_str(), "namespace" | "module") && o.start_line <= n.start_line && o.end_line >= n.end_line) { continue; }
            imported.push(n);
        }
        let Some(factory) = imported.first() else { return Ok(None) };
        let Some(raw) = &factory.return_type else { return Ok(None) };
        let hit = KotlinReceiverHit { ty: simple_type(raw).to_string(), site: r.clone().at(factory), heuristic: true };
        let Some(owner) = self.resolve_bound_type(&hit.ty, &hit.site, 0)? else { return Ok(None) };
        // A declared incompatible member can shadow the imported extension.
        let mut competing=imported.clone();
        let receiver_owner=super::call_shape::owner(factory).unwrap_or("");
        let receiver_type=self.resolve_bound_type(receiver_owner,&r.clone().at(factory),0)?;
        if let Some(receiver_type)=receiver_type {
            let receiver_hit=KotlinReceiverHit{ty:receiver_owner.to_string(),site:r.clone().at(factory),heuristic:true};
            let receiver_ancestors=self.kotlin_type_ancestors(&receiver_hit)?;
            for member in self.nodes_by_name(name)?.iter().filter(|n|matches!(n.language.as_str(),"kotlin"|"java") && matches!(n.kind.as_str(),"function"|"method")) {
                let dispatch=self.nodes_in_file(&member.file_path)?.iter().filter(|o|super::call_shape::type_kind(&o.kind) && !matches!(o.kind.as_str(),"namespace"|"module") && o.start_line<=member.start_line && o.end_line>=member.end_line).min_by_key(|o|o.end_line-o.start_line).cloned();
                let Some(dispatch)=dispatch else {continue};
                if receiver_ancestors.iter().any(|a|a.id==dispatch.id) || self.kotlin_top_level_visible(&dispatch,r)? {competing.push(member.clone());continue;}
                let mut queue=VecDeque::from([dispatch]);
                let mut seen=HashSet::new();
                while let Some(ty)=queue.pop_front() {
                    if seen.len()>=32 || !seen.insert(ty.id.clone()) {continue;}
                    if ty.id==receiver_type.id {competing.push(member.clone());break;}
                    queue.extend(self.supertype_nodes(&ty.id)?);
                }
            }
        }
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
