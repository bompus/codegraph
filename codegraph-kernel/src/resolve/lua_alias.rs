//! Bare Lua calls through local aliases, followed through bounded module exports.
use super::*;
use super::iteration::{descendant_for_position, named_children};

const GLOBALS: &[&str] = &[
    "assert", "error", "ipairs", "pairs", "next", "type", "tostring", "tonumber", "setmetatable", "getmetatable",
    "rawget", "rawset", "rawequal", "rawlen", "select", "pcall", "xpcall", "unpack", "print", "load", "loadstring",
    "loadfile", "dofile", "collectgarbage", "require", "setfenv", "getfenv", "newproxy", "typeof", "warn", "tick", "wait",
    "describe", "it", "before_each", "after_each", "setup", "teardown", "lazy_setup", "lazy_teardown", "pending", "finally", "insulate", "expose",
];

pub(super) fn is_global(name: &str) -> bool { GLOBALS.contains(&name) }

fn path(raw: &str) -> Vec<String> { raw.split('.').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect() }
fn require_alias(raw: &str) -> Option<(String, Vec<String>)> {
    let m = re!(r#"^(?:require|[A-Za-z_]\w*(?:[Rr]equire|_module|[Ii]mport))\s*\(?\s*(?:"([^"]+)"|'([^']+)')\s*\)?((?:\s*\.\s*[A-Za-z_]\w*)*)\s*$"#).captures(raw)?;
    Some((m.get(1).or_else(|| m.get(2))?.as_str().to_string(), path(m.get(3)?.as_str())))
}

impl KernelResolver {
    /// None means no supported alias; Some(None) is a proven non-project alias.
    pub(super) fn lua_alias_target(&mut self, r: &ResolveRefIn) -> Res<Option<Option<Arc<KNode>>>> {
        if !matches!(r.language.as_str(), "lua" | "luau") || r.reference_kind != "calls" || !re!(r"^[A-Za-z_]\w*$").is_match(&r.reference_name) { return Ok(None); }
        let decl = self.lua_local_decl(&r.reference_name, r)?;
        if let Some(source) = self.read_file(&r.file_path) {
            if let Some(tree) = self.parsed_tree(&source, r) {
                let mut node = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1));
                while let Some(parent) = node.parent() {
                    if parent.kind() == "for_statement" && node.kind() == "block" {
                        let clause = named_children(parent).into_iter().find(|n| matches!(n.kind(), "for_generic_clause" | "for_numeric_clause"));
                        if clause.is_some_and(|clause| {
                            let vars = named_children(clause).into_iter().find(|n| n.kind() == "variable_list");
                            vars.map(named_children).unwrap_or_else(|| named_children(clause)).into_iter().any(|n| n.kind() == "identifier" && source.text()[n.start_byte()..n.end_byte()] == r.reference_name)
                        }) && decl.as_ref().is_none_or(|d| (d.start_line - 1, d.start_column) < (parent.start_position().row as i64, parent.start_position().column as i64)) { return Ok(Some(None)); }
                    }
                    if let Some(params) = parent.child_by_field_name("parameters") {
                        if named_children(params).into_iter().any(|p| {
                            let p = p.child_by_field_name("name").unwrap_or(p);
                            source.text()[p.start_byte()..p.end_byte()] == r.reference_name
                        }) && decl.as_ref().is_none_or(|d| {
                            (d.start_line - 1, d.start_column) < (parent.start_position().row as i64, parent.start_position().column as i64)
                        }) { return Ok(Some(None)); }
                    }
                    node = parent;
                }
            }
        }
        let Some(decl) = decl else {
            let nodes = self.nodes_in_file(&r.file_path)?;
            let mut globals = Vec::new();
            for node in nodes.iter().filter(|n| n.name == r.reference_name && matches!(n.kind.as_str(), "function" | "method")) {
                if node.qualified_name == node.name && !self.is_lua_local(node) { globals.push(node.clone()); }
            }
            if globals.len() == 1 { return Ok(Some(Some(globals[0].clone()))); }
            for node in nodes.iter().filter(|n| n.name == r.reference_name && n.kind == "variable") {
                if self.is_lua_local(node) { return Ok(Some(None)); }
            }
            return Ok(None);
        };
        if matches!(decl.kind.as_str(), "function" | "method") { return Ok(Some(Some(decl))); }
        if let Some(source) = self.read_file(&r.file_path) {
            if let Some(tree) = self.parsed_tree(&source, r) {
                let mut queue = vec![tree.root_node()];
                while let Some(node) = queue.pop() {
                    if node.kind() == "assignment_statement" && !node.parent().is_some_and(|p| p.kind() == "variable_declaration") {
                        if let Some(vars) = named_children(node).into_iter().find(|n| n.kind() == "variable_list") {
                            let values = named_children(node).into_iter().find(|n| n.kind() == "expression_list").map(named_children).unwrap_or_default();
                            for (index, left) in named_children(vars).into_iter().enumerate() {
                                if decl.signature.is_none() && values.get(index).is_some_and(|n| n.kind() == "function_definition") { continue; }
                                if source.text()[left.start_byte()..left.end_byte()] != r.reference_name { continue; }
                                let mut assigned = r.clone();
                                assigned.line = left.start_position().row as i64 + 1;
                                assigned.column = left.start_position().column as i64;
                                if (assigned.line, assigned.column) > (decl.start_line, decl.start_column) && (assigned.line, assigned.column) < (r.line, r.column)
                                    && self.lua_local_decl(&r.reference_name, &assigned)?.is_some_and(|n| n.id == decl.id) { return Ok(Some(None)); }
                            }
                        }
                    }
                    queue.extend(named_children(node));
                }
            }
        }
        if decl.signature.is_none() {
            let mut closure = false;
            if let Some(source) = self.read_file(&r.file_path) {
                if let Some(tree) = self.parsed_tree(&source, r) {
                    let mut node = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
                    while let Some(parent) = node.parent() {
                        if matches!(parent.kind(), "function_declaration" | "function_definition") { closure = true; break; }
                        node = parent;
                    }
                }
            }
            let nodes = self.nodes_in_file(&r.file_path)?;
            let mut functions = Vec::new();
            for node in nodes.iter().filter(|n| n.kind == "function" && n.name == decl.name && (closure || (n.start_line, n.start_column) < (r.line, r.column))) {
                let mut defined = r.clone().at(node); defined.column = node.start_column;
                if self.lua_local_decl(&decl.name, &defined)?.is_some_and(|n| n.id == decl.id) { functions.push(node.clone()); }
            }
            return Ok(Some(if functions.len() == 1 { Some(functions[0].clone()) } else { None }));
        }
        let mut site = r.clone().at(&decl); site.column = decl.start_column;
        self.lua_alias_expr(decl.signature.as_deref().unwrap_or("").trim_start_matches('=').trim(), &site, &decl.name, 0)
    }

    fn lua_local_decl(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let nodes = self.nodes_in_file(&r.file_path)?;
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(None) };
        let declarations = if let Some(cached) = self.lua_declaration_memo.get(&r.file_path) { cached.clone() } else {
        // Nested locals are binding evidence even when extraction has no node for them.
        let mut declarations = nodes.as_ref().clone();
        let mut queue = vec![tree.root_node()];
        while let Some(node) = queue.pop() {
            if node.kind() == "variable_declaration" && source.text()[node.start_byte()..node.end_byte()].trim_start().starts_with("local ") {
                let assignment = named_children(node).into_iter().find(|n| n.kind() == "assignment_statement").unwrap_or(node);
                let vars = named_children(assignment).into_iter().find(|n| n.kind() == "variable_list");
                let values = named_children(assignment).into_iter().find(|n| n.kind() == "expression_list");
                if let (Some(vars), Some(template)) = (vars, nodes.first()) {
                    let values = values.map(named_children).unwrap_or_default();
                    for (index, left) in named_children(vars).into_iter().enumerate() {
                        let right = values.get(index);
                        let name = &source.text()[left.start_byte()..left.end_byte()];
                        let mut binding = template.as_ref().clone();
                        binding.id = format!("lua-binding:{}:{}", r.file_path, left.start_byte());
                        binding.kind = "variable".to_string(); binding.name = name.to_string();
                        binding.start_line = left.start_position().row as i64 + 1;
                        let line = source.get(left.start_position().row).map(String::as_str).unwrap_or("");
                        binding.start_column = utf16_len(&line[..left.start_position().column.min(line.len())]) as i64;
                        binding.signature = right.map(|n| source.text()[n.start_byte()..n.end_byte()].to_string());
                        declarations.retain(|n| !(n.kind == "variable" && n.name == name && n.start_line == binding.start_line && n.start_column == binding.start_column));
                        declarations.push(Arc::new(binding));
                    }
                }
            }
            queue.extend(named_children(node));
        }
            let declarations = Arc::new(declarations);
            self.lua_declaration_memo.insert(r.file_path.clone(), declarations.clone());
            declarations
        };
        let mut best = None;
        for n in declarations.iter().filter(|n| n.name == name && matches!(n.kind.as_str(), "variable" | "function" | "method") && (n.start_line < r.line || n.start_line == r.line && n.start_column < r.column)) {
            let mut at = descendant_for_position(tree.root_node(), source.text(), ((n.start_line - 1).max(0) as usize, n.start_column.max(0) as usize));
            let mut visible = true;
            let mut local = false;
            let position = (r.line - 1, r.column);
            while let Some(parent) = at.parent() {
                if matches!(parent.kind(), "variable_declaration" | "function_declaration") && source.text()[parent.start_byte()..parent.end_byte()].trim_start().starts_with("local ") {
                    local = true;
                    if parent.kind() == "variable_declaration" && position >= (parent.start_position().row as i64, parent.start_position().column as i64) && position < (parent.end_position().row as i64, parent.end_position().column as i64) { visible = false; break; }
                }
                if parent.kind() == "block" {
                    let point = |p: tree_sitter::Point| {
                        let line = source.get(p.row).map(String::as_str).unwrap_or("");
                        (p.row as i64, utf16_len(&line[..p.column.min(line.len())]) as i64)
                    };
                    if position < point(parent.start_position()) || position > point(parent.end_position()) { visible = false; break; }
                }
                at = parent;
            }
            if !local { continue; }
            if visible && best.as_ref().is_none_or(|b: &Arc<KNode>| (n.start_line, n.start_column) > (b.start_line, b.start_column)) { best = Some(n.clone()); }
        }
        Ok(best)
    }

    fn lua_alias_expr(&mut self, rhs: &str, site: &ResolveRefIn, name: &str, depth: usize) -> Res<Option<Option<Arc<KNode>>>> {
        if depth > 3 { return Ok(Some(None)); }
        if let Some((module, members)) = require_alias(rhs) { return self.lua_module_member(&module, &members, site, depth); }
        if !re!(r"^[A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)*$").is_match(rhs) { return Ok(None); }
        let names = path(rhs);
        let root = &names[0];
        let members = &names[1..];
        if root != name {
            if let Some(decl) = self.lua_local_decl(root, site)? {
                if let Some((module, mut prefix)) = require_alias(decl.signature.as_deref().unwrap_or("").trim_start_matches('=').trim()) {
                    if !members.is_empty() { prefix.extend_from_slice(members); return self.lua_module_member(&module, &prefix, &{ let mut at = site.clone().at(&decl); at.column = decl.start_column; at }, depth); }
                }
                let identity=decl.signature.as_deref().unwrap_or("").trim_start_matches('=').trim()==root
                    && (root=="kong" || super::method_call::LUA_LIBRARY_TABLES.contains(&root.as_str()));
                if !identity { return Ok(None); }
            }
        }
        if members.is_empty() { return Ok(GLOBALS.contains(&root.as_str()).then_some(None)); }
        if !super::method_call::LUA_LIBRARY_TABLES.contains(&root.as_str()) || (members.len()>1 && matches!(root.as_str(),"kong"|"ngx"|"vim")) {
            let member=members.last().unwrap(); let holder=if members.len()>1 {&members[members.len()-2]}else{root};
            let owned:Vec<_>=self.nodes_by_name(member)?.iter().filter(|n|matches!(n.language.as_str(),"lua"|"luau")&&n.kind=="method"&&super::method_call::shares_receiver_word(holder,n)&&!(is_test_path(&n.file_path)&&!is_test_path(&site.file_path))).cloned().collect();
            return Ok(if owned.len()==1{Some(Some(owned[0].clone()))}else{None});
        }
        if members.len() != 1 { return Ok(None); }
        let targets: Vec<_> = self.nodes_by_name(&members[0])?.iter().filter(|n|
            matches!(n.kind.as_str(), "function" | "method") && n.qualified_name.split("::").next().unwrap_or("").split('.').next() == Some(root.as_str()) && (n.file_path != site.file_path || (n.start_line, n.start_column) < (site.line, site.column)) && (n.file_path == site.file_path || !is_test_path(&n.file_path))).cloned().collect();
        Ok(Some(if targets.len() == 1 { Some(targets[0].clone()) } else { None }))
    }

    fn lua_module_member(&mut self, module: &str, members: &[String], site: &ResolveRefIn, depth: usize) -> Res<Option<Option<Arc<KNode>>>> {
        let mut imported = site.clone(); imported.reference_name = module.to_string(); imported.reference_kind = "imports".to_string();
        let Some(file) = self.resolve_lua_require(&imported)? else { return Ok(Some(None)) };
        if members.is_empty() { return Ok(None); }
        let key = format!("{}\0{}\0{depth}", file.node.file_path, members.join("."));
        if let Some(hit) = self.lua_member_memo.get(&key) { return Ok(Some(hit.clone())); }
        self.lua_member_memo.insert(key.clone(), None);
        let result = self.lua_member_in(&file.node.file_path, members, site, depth)?;
        self.lua_member_memo.insert(key, result.clone());
        Ok(Some(result))
    }

    fn lua_owner_written_after(&mut self, owner: &str, member: &str, returned: &ResolveRefIn, after: (i64, i64), binding: Option<&Arc<KNode>>) -> Res<bool> {
        let Some(source) = self.read_file(&returned.file_path) else { return Ok(false) };
        let Some(tree) = self.parsed_tree(&source, returned) else { return Ok(false) };
        let mut queue = vec![tree.root_node()];
        while let Some(node) = queue.pop() {
            if matches!(node.kind(), "function_declaration" | "function_definition") { continue; }
            if node.kind() == "assignment_statement" && !node.parent().is_some_and(|p| p.kind() == "variable_declaration" && source.text()[p.start_byte()..p.end_byte()].trim_start().starts_with("local ")) {
                if let Some(vars) = named_children(node).into_iter().find(|n| n.kind() == "variable_list") {
                    for left in named_children(vars) {
                        let text = &source.text()[left.start_byte()..left.end_byte()];
                        let requested = format!("{owner}.{member}");
                        if text != owner && text != requested && !requested.strip_prefix(text).is_some_and(|rest| rest.starts_with('.')) { continue; }
                        let mut write = returned.clone(); write.line = left.start_position().row as i64 + 1;
                        let line = source.get(left.start_position().row).map(String::as_str).unwrap_or("");
                        write.column = utf16_len(&line[..left.start_position().column.min(line.len())]) as i64;
                        if (write.line, write.column) > after && (write.line, write.column) < (returned.line, returned.column)
                            && self.lua_local_decl(owner, &write)?.as_ref().map(|n| &n.id) == binding.map(|n| &n.id) { return Ok(true); }
                    }
                }
            }
            queue.extend(named_children(node));
        }
        Ok(false)
    }

    fn lua_member_in(&mut self, file: &str, members: &[String], site: &ResolveRefIn, depth: usize) -> Res<Option<Arc<KNode>>> {
        if depth > 3 { return Ok(None); }
        let member = members.last().unwrap();
        let nodes = self.nodes_in_file(file)?;
        let Some(source) = self.read_file(file) else { return Ok(None) };
        let mut at = site.clone(); at.file_path = file.to_string();
        let Some(tree) = self.parsed_tree(&source, &at) else { return Ok(None) };
        let text = |n: tree_sitter::Node| &source.text()[n.start_byte()..n.end_byte()];
        // The chunk's own return defines its public module value.
        let returns: Vec<_> = named_children(tree.root_node()).into_iter().filter(|n| n.kind() == "return_statement").collect();
        if returns.len() != 1 { return Ok(None); }
        let returned = named_children(returns[0]).into_iter().find(|n| n.kind() == "expression_list").and_then(|n| n.named_child(0));
        let Some(returned) = returned else { return Ok(None) };
        let mut aliases = Vec::new();
        let mut fields = Vec::new();
        if returned.kind() == "table_constructor" {
            fields.extend(named_children(returned).into_iter().filter(|n| n.kind() == "field"));
        } else if returned.kind() == "identifier" {
            let owner = text(returned);
            let qualified = if members.len() == 1 { format!("{owner}::{member}") } else { format!("{}.{}::{member}", owner, members[..members.len()-1].join(".")) };
            let targets: Vec<_> = nodes.iter().filter(|n| n.qualified_name == qualified && matches!(n.kind.as_str(), "function" | "method")).cloned().collect();
            let mut returned_at = at.clone();
            returned_at.line = returned.start_position().row as i64 + 1;
            returned_at.column = returned.start_position().column as i64;
            let returned_owner = self.lua_local_decl(owner, &returned_at)?;
            if targets.len() == 1 {
                let mut method_at = at.clone().at(&targets[0]);
                method_at.column = targets[0].start_column;
                let method_owner = self.lua_local_decl(owner, &method_at)?;
                if returned_owner.as_ref().map(|n| &n.id) == method_owner.as_ref().map(|n| &n.id) {
                    if self.lua_owner_written_after(owner, &members.join("."), &returned_at, (targets[0].start_line, targets[0].start_column), returned_owner.as_ref())? { return Ok(None); }
                    return Ok(Some(targets[0].clone()));
                }
                return Ok(None);
            }
            if targets.len() > 1 { return Ok(None); }
            let mut queue = vec![tree.root_node()];
            while let Some(node) = queue.pop() {
                if matches!(node.kind(), "function_declaration" | "function_definition") { continue; }
                if node.kind() == "assignment_statement" {
                    let vars = named_children(node).into_iter().find(|n| n.kind() == "variable_list");
                    let values = named_children(node).into_iter().find(|n| n.kind() == "expression_list");
                    if let (Some(vars), Some(values)) = (vars, values) {
                        for (left, right) in named_children(vars).into_iter().zip(named_children(values)) {
                            let mut assignment_at = at.clone();
                            assignment_at.line = right.start_position().row as i64 + 1;
                            assignment_at.column = right.start_position().column as i64;
                            let declared_owner = text(left) == owner && node.parent().is_some_and(|p| p.kind() == "variable_declaration" && text(p).trim_start().starts_with("local "));
                            let assignment_owner_id = if declared_owner { Some(format!("lua-binding:{file}:{}", left.start_byte())) }
                                else { self.lua_local_decl(owner, &assignment_at)?.map(|n| n.id.clone()) };
                            if returned_owner.as_ref().map(|n| &n.id) != assignment_owner_id.as_ref() { continue; }
                            if text(left) == owner && right.kind() == "table_constructor" {
                                fields.extend(named_children(right).into_iter().filter(|n| n.kind() == "field"));
                            } else if text(left) == format!("{owner}.{}", members.join(".")) {
                                aliases.push((text(right).to_string(), right.start_position().row as i64 + 1, right.start_position().column as i64));
                            }
                        }
                    }
                }
                queue.extend(named_children(node));
            }
        } else { return Ok(None); }
        for key in &members[..members.len()-1] {
            let matched: Vec<_> = fields.iter().filter(|field| field.child_by_field_name("name").is_some_and(|n| text(n) == key)).copied().collect();
            if matched.len() != 1 { return Ok(None); }
            let Some(value) = matched[0].child_by_field_name("value").filter(|n| n.kind() == "table_constructor") else { return Ok(None) };
            fields = named_children(value).into_iter().filter(|n| n.kind() == "field").collect();
        }
        let pattern = format!(r"^{}\s*=\s*([A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)*)\s*$", regex::escape(member));
        for field in fields {
            if let Some(m) = Self::cached_regex(&pattern)?.captures(text(field).trim().trim_end_matches(',')) {
                let rhs = m[1].to_string();
                aliases.push((rhs, field.start_position().row as i64 + 1, field.start_position().column as i64));
            }
        }
        if aliases.len() != 1 { return Ok(None); }
        let (rhs, line, column) = &aliases[0];
        if returned.kind() == "identifier" {
            let mut returned_at = at.clone(); returned_at.line = returned.start_position().row as i64 + 1; returned_at.column = returned.start_position().column as i64;
            let owner = text(returned);
            let binding = self.lua_local_decl(owner, &returned_at)?;
            if self.lua_owner_written_after(owner, &members.join("."), &returned_at, (*line, *column), binding.as_ref())? { return Ok(None); }
        }
        at.line = *line; at.column = *column;
        if re!(r"^[A-Za-z_]\w*$").is_match(rhs) {
            if let Some(decl) = self.lua_local_decl(rhs, &at)? {
                if matches!(decl.kind.as_str(), "function" | "method") { return Ok(Some(decl)); }
            } else {
                let mut targets = Vec::new();
                for node in nodes.iter().filter(|n| n.name == *rhs && n.qualified_name == n.name && n.kind == "function") {
                    if !self.is_lua_local(node) { targets.push(node.clone()); }
                }
                if targets.len() == 1 { return Ok(Some(targets[0].clone())); }
            }
        }
        Ok(self.lua_alias_expr(rhs, &at, member, depth + 1)?.flatten())
    }
}
