//! Recover member receiver evidence when extraction retained only the method name.
use super::*;
use super::iteration::{descendant_for_position, named_children};

impl KernelResolver {
    pub(super) fn cpp_complex_call(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "cpp" || r.reference_kind != "calls" || !re!(r"^[A-Za-z_]\w*$").is_match(&r.reference_name) { return Ok(None); }
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1));
        loop {
            if node.kind() == "call_expression" {
                if let Some(function) = node.child_by_field_name("function").filter(|n| n.kind() == "field_expression") {
                    let field = function.child_by_field_name("field").or_else(|| function.named_child(1));
                    if field.is_some_and(|n| source.text()[n.start_byte()..n.end_byte()] == r.reference_name) {
                        let Some(receiver) = function.child_by_field_name("argument").or_else(|| function.named_child(0)) else { return Ok(Some(None)) };
                        let mut shape = receiver;
                        while shape.kind() == "parenthesized_expression" {
                            let children = named_children(shape);
                            let [only] = children.as_slice() else { return Ok(None) }; shape = *only;
                        }
                        if !matches!(shape.kind(), "subscript_expression" | "field_expression") { return Ok(None); }
                        let Some(owner) = self.cpp_expression_owner(receiver, &source, r, 0)? else { return Ok(Some(None)) };
                        let methods = self.nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, r.reference_name))?;
                        let methods: Vec<_> = methods.iter().filter(|n| n.kind == "method" && n.file_path == owner.file_path).collect();
                        let [method] = methods.as_slice() else { return Ok(Some(None)) };
                        return Ok(Some(Some(KCand { node: (*method).clone(), confidence: 0.9, resolved_by: "instance-method" })));
                    }
                }
                return Ok(None);
            }
            let Some(parent) = node.parent() else { break }; node = parent;
        }
        Ok(None)
    }

    fn cpp_expression_owner(&mut self, node: tree_sitter::Node, source: &SourceFile, r: &ResolveRefIn, depth: u32) -> Res<Option<Arc<KNode>>> {
        if depth > 4 { return Ok(None); }
        let text = |n: tree_sitter::Node| &source.text()[n.start_byte()..n.end_byte()];
        match node.kind() {
            "identifier" => {
                // A range variable's type comes from the declared standard container.
                let mut at = node;
                while let Some(parent) = at.parent() {
                    if let Some(raw) = cpp_binding_type(parent, at, text(node), source) {
                        let ty = if raw == "auto" && parent.kind() == "compound_statement" { self.infer_cpp_receiver_type(text(node), r, 0, true)? } else { Some(raw) };
                        let Some(ty) = ty else { return Ok(None) };
                        let primary = ty.split('<').next().unwrap_or(&ty).trim();
                        if let Some(caller) = self.node_by_id(&r.from_node_id)? { if self.cpp_template_parameter(&caller, primary, r)? { return Ok(None); } }
                        return self.cpp_type_owner(primary, r, 0, false);
                    }
                    if parent.kind() == "for_range_loop" && parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id())
                        && parent.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_name(n, source) == text(node)) {
                        if let Some(right) = parent.child_by_field_name("right").filter(|n| n.kind() == "identifier") {
                            return self.cpp_container_element(text(right), r);
                        }
                        return Ok(None);
                    }
                    at = parent;
                }
                let owner = self.nodes_in_file(&r.file_path)?.iter().filter(|n| matches!(n.kind.as_str(), "class" | "struct" | "union") && (n.start_line, n.start_column) <= (r.line, r.column) && (n.end_line, n.end_column) >= (r.line, r.column)).min_by_key(|n| (n.end_line - n.start_line, n.end_column - n.start_column)).cloned();
                if let Some(owner) = owner { if let Some(field) = self.cpp_field_owner(&owner, text(node), r)? { return Ok(Some(field)); } }
                if let Some(ty) = self.infer_cpp_receiver_type(text(node), r, 0, true)? {
                    if let Some(owner) = self.cpp_type_owner(&ty, r, 0, false)? { return Ok(Some(owner)); }
                }
                Ok(None)
            }
            "subscript_expression" => {
                let Some(argument) = node.child_by_field_name("argument").or_else(|| node.named_child(0)) else { return Ok(None) };
                if argument.kind() == "identifier" && !self.cpp_range_binding(argument, source) {
                    if let Some(element) = self.cpp_container_element(text(argument), r)? { return Ok(Some(element)); }
                }
                let Some(owner) = self.cpp_expression_owner(argument, source, r, depth + 1)? else { return Ok(None) };
                let mut site = r.clone().at(&owner); site.from_node_id = owner.id.clone(); site.column = owner.start_column;
                let mut results = Vec::new();
                for member in self.nodes_by_qualified_name(&format!("{}::operator[]", owner.qualified_name))?.iter().filter(|n| n.kind == "method" && n.file_path == owner.file_path) {
                    let Some(ty) = self.cpp_method_type(member, &site)?.or_else(|| member.return_type.clone()) else { return Ok(None) };
                    if self.cpp_template_parameter(member, &ty, &site)? { return Ok(None); }
                    let included = self.namespace_visible_files(&r.file_path, "cpp")?;
                    let Some(result) = self.cpp_type_owner_visible(&ty, &site, 0, false, &included)? else { return Ok(None) };
                    if !results.iter().any(|n: &Arc<KNode>| n.id == result.id) { results.push(result); }
                }
                Ok(match results.as_slice() { [only] => Some(only.clone()), _ => None })
            }
            "field_expression" => {
                let Some(argument) = node.child_by_field_name("argument").or_else(|| node.named_child(0)) else { return Ok(None) };
                let Some(field) = node.child_by_field_name("field").or_else(|| node.named_child(1)) else { return Ok(None) };
                let Some(owner) = self.cpp_expression_owner(argument, source, r, depth + 1)? else { return Ok(None) };
                self.cpp_field_owner(&owner, text(field), r)

            }
            "parenthesized_expression" => match named_children(node).as_slice() { [only] => self.cpp_expression_owner(*only, source, r, depth + 1), _ => Ok(None) },
            _ => Ok(None),
        }
    }

    fn cpp_method_type(&mut self, method: &KNode, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(source) = self.read_file(&method.file_path) else { return Ok(None) };
        let mut site = r.clone().at(method); site.from_node_id = method.id.clone(); site.column = method.start_column;
        let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((method.start_line - 1).max(0) as usize, method.start_column.max(0) as usize + 1));
        loop {
            if matches!(node.kind(), "function_definition" | "field_declaration" | "declaration") {
                return Ok(node.child_by_field_name("type").map(|ty| source.text()[ty.start_byte()..ty.end_byte()].to_string()));
            }
            let Some(parent) = node.parent() else { return Ok(None) }; node = parent;
        }
    }

    fn cpp_field_tree(&mut self, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Option<Rc<tree_sitter::Tree>> {
        let normalized = source.cpp_field_tree.get_or_init(|| {
            // Match extraction's padded access-macro rewrite; byte offsets stay fixed.
            let text = re!(r"(?m)^([ \t]*)([A-Z][A-Z0-9]*_[A-Z0-9_]*)([ \t]*):([ \t]*(?://[^\n]*)?\r?$)").replace_all(source.text(), |m: &regex::Captures| {
                let name = &m[2];
                let keyword = if name.contains("PRIVATE") { "private" } else if name.contains("PROTECTED") { "protected" }
                    else if name.contains("PUBLIC") || matches!(name, "Q_SIGNALS" | "Q_SLOTS") { "public" } else { return m[0].to_string(); };
                let width = name.len() + m[3].len();
                if keyword.len() > width { return m[0].to_string(); }
                format!("{}{}{}:{}", &m[1], keyword, " ".repeat(width - keyword.len()), &m[4])
            });
            if text.as_ref() == source.text() { return None; }
            crate::tree::parse_with_cached_parser(text.as_ref(), "cpp").ok().map(Rc::new)
        }).clone();
        normalized.or_else(|| self.parsed_tree(source, r))
    }

    fn cpp_field_owner(&mut self, owner: &Arc<KNode>, field: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(source) = self.read_file(&owner.file_path) else { return Ok(None) };
        let mut site = r.clone().at(owner); site.from_node_id = owner.id.clone(); site.column = owner.start_column;
        let Some(tree) = self.cpp_field_tree(&source, &site) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((owner.start_line - 1).max(0) as usize, owner.start_column.max(0) as usize + 1));
        while !matches!(node.kind(), "class_specifier" | "struct_specifier" | "union_specifier") {
            let Some(parent) = node.parent() else { break }; node = parent;
        }
        let mut declarations = Vec::new();
        if node.child_by_field_name("name").is_some_and(|name| source.text()[name.start_byte()..name.end_byte()] == owner.name) {
            if let Some(body) = node.child_by_field_name("body") { declarations.extend(named_children(body).into_iter().filter(|n| n.kind() == "field_declaration")); }
        }
        let mut types = Vec::new();
        for declaration in declarations {
            let Some(ty) = declaration.child_by_field_name("type") else { continue };
            let mut cursor = declaration.walk();
            for (index, child) in declaration.children(&mut cursor).enumerate() {
                if declaration.field_name_for_child(index as u32) == Some("declarator") && cpp_declarator_name(child, &source) == field {
                    types.push(source.text()[ty.start_byte()..ty.end_byte()].to_string());
                }
            }
        }
        let [ty] = types.as_slice() else { return Ok(None) };
        let primary = ty.split('<').next().unwrap_or(ty).trim();
        if self.cpp_template_parameter(owner, primary, &site)? { return Ok(None); }
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        self.cpp_type_owner_visible(primary, &site, 0, false, &included)
    }

    fn cpp_range_binding(&self, node: tree_sitter::Node, source: &SourceFile) -> bool {
        let mut at = node;
        while let Some(parent) = at.parent() {
            if parent.kind() == "for_range_loop" && parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id())
                && parent.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_name(n, source) == &source.text()[node.start_byte()..node.end_byte()]) { return true; }
            at = parent;
        }
        false
    }

    fn cpp_container_element(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        if let Some(tree) = self.parsed_tree(&source, r) {
            let mut at = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1));
            while let Some(parent) = at.parent() {
                if let Some(raw) = cpp_binding_type(parent, at, name, &source) {
                    let Some(capture) = re!(r"^std::(?:vector|array|deque|span)\s*<\s*([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*[,>]").captures(&raw) else { return Ok(None) };
                    return self.cpp_type_owner(&capture[1], r, 0, false);
                }
                at = parent;
            }
        }
        let start = self.enclosing_scope_start_line(&r.file_path, "cpp", r.line)?.saturating_sub(1).max(0) as usize;
        for line in source.iter().take(r.line.max(0) as usize).skip(start).rev() {
            if let Some(raw) = self.cpp_declarator_match(line, &regex::escape(name))? {
                let Some(capture) = re!(r"^std::(?:vector|array|deque|span)\s*<\s*([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*[,>]").captures(&raw) else { return Ok(None) };
                return self.cpp_type_owner(&capture[1], r, 0, false);
            }
        }
        Ok(None)
    }
}

fn cpp_declarator_name<'a>(mut node: tree_sitter::Node, source: &'a SourceFile) -> &'a str {
    while matches!(node.kind(), "reference_declarator" | "pointer_declarator" | "parenthesized_declarator" | "init_declarator") {
        let Some(inner) = node.child_by_field_name("declarator").or_else(|| node.named_child(0)) else { return "" }; node = inner;
    }
    if matches!(node.kind(), "identifier" | "field_identifier") { &source.text()[node.start_byte()..node.end_byte()] } else { "" }
}

fn cpp_binding_type(parent: tree_sitter::Node, at: tree_sitter::Node, name: &str, source: &SourceFile) -> Option<String> {
    if parent.kind() == "compound_statement" {
        for declaration in named_children(parent).into_iter().rev().filter(|n| n.kind() == "declaration" && n.end_byte() <= at.start_byte()) {
            let mut cursor = declaration.walk();
            if declaration.children(&mut cursor).enumerate().any(|(i, child)| declaration.field_name_for_child(i as u32) == Some("declarator") && cpp_declarator_name(child, source) == name) {
                return Some(declaration.child_by_field_name("type").map(|ty| source.text()[ty.start_byte()..ty.end_byte()].to_string()).unwrap_or_default());
            }
        }
    }
    if matches!(parent.kind(), "lambda_expression" | "function_definition") {
        let mut queue = named_children(parent).into_iter().filter(|n| n.kind() != "compound_statement").collect::<Vec<_>>();
        while let Some(parameter) = queue.pop() {
            if parameter.kind() == "parameter_declaration" && parameter.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_name(n, source) == name) {
                return Some(parameter.child_by_field_name("type").map(|ty| source.text()[ty.start_byte()..ty.end_byte()].to_string()).unwrap_or_default());
            }
            if parameter.kind() != "compound_statement" { queue.extend(named_children(parameter)); }
        }
    }
    None
}
