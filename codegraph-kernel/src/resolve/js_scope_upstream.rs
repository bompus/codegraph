//! JavaScript declaration scopes, indexed once for each immutable source file.
use super::*;
use tree_sitter::{Node, Point};

pub(super) struct JsBindingIndex {
    source: Rc<SourceFile>,
    by_name: HashMap<String, Vec<JsBindingFact>>,
}

struct JsBindingFact {
    start: Point,
    end: Point,
    scope: Option<(Point, Point)>,
}

pub(super) fn binding_scope(node: Node<'_>) -> Option<(Point, Point)> {
    let function_scoped = node.kind() == "variable_declarator"
        && node
            .parent()
            .is_some_and(|p| p.kind() == "variable_declaration");
    let mut current = node.parent();
    while let Some(scope) = current {
        let lexical = matches!(
            scope.kind(),
            "statement_block" | "for_statement" | "for_in_statement"
        );
        let function = matches!(
            scope.kind(),
            "function_declaration"
                | "function_expression"
                | "method_definition"
                | "generator_function_declaration"
                | "generator_function"
                | "arrow_function"
        );
        if function || (lexical && !function_scoped) {
            return Some((scope.start_position(), scope.end_position()));
        }
        current = scope.parent();
    }
    None
}

fn visible(scope: Option<(Point, Point)>, point: Point) -> bool {
    scope.is_none_or(|(start, end)| point >= start && point <= end)
}

impl KernelResolver {
    /// Parse embedded scripts with the original file's byte and line positions.
    pub(super) fn js_binding_tree(&mut self, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Option<Rc<tree_sitter::Tree>> {
        if !is_sfc_language(&r.language) {
            return self.parsed_tree(source, r);
        }
        let key = format!("sfc-script\0{}\0{}", r.language, r.file_path);
        if let Some((cached, tree)) = self.tree_cache.get(&key) {
            if Rc::ptr_eq(cached, source) { return tree.clone(); }
        }
        // The extractors use these same script tags and Astro line fences.
        // Mask each non-script byte while preserving newlines and UTF-8 offsets.
        let text = source.text();
        let mut bytes: Vec<u8> = text.bytes().map(|b| if matches!(b, b'\r' | b'\n') { b } else { b' ' }).collect();
        for capture in re!(r"<script(?:\s[^>]*)?>([\s\S]*?)</script>").captures_iter(text) {
            if let Some(content) = capture.get(1) {
                bytes[content.start()..content.end()].copy_from_slice(content.as_str().as_bytes());
            }
        }
        if r.language == "astro" {
            let mut offset = 0;
            let mut start = None;
            for line in text.split_inclusive('\n') {
                if let Some(from) = start {
                    if line.trim() == "---" {
                        bytes[from..offset].copy_from_slice(&text.as_bytes()[from..offset]);
                        break;
                    }
                } else if !line.trim().is_empty() {
                    if line.trim() != "---" { break; }
                    start = Some(offset + line.len());
                }
                offset += line.len();
            }
        }
        let script = String::from_utf8(bytes).ok()?;
        let tree = crate::tree::parse_with_cached_parser(&script, "typescript").ok().map(Rc::new);
        self.tree_cache.put(key, (source.clone(), tree.clone()));
        tree
    }
    /// Parameter patterns bind names only within their enclosing function.
    pub(super) fn js_parameter_binds(&mut self, r: &ResolveRefIn, name: &str) -> bool {
        self.js_parameter_scope(r, name).is_some()
    }

    pub(super) fn js_parameter_scope(&mut self, r: &ResolveRefIn, name: &str) -> Option<(Point, Point)> {
        let source = self.read_file(&r.file_path)?;
        let tree = self.js_binding_tree(&source, r)?;
        let line = source.get((r.line - 1).max(0) as usize)?;
        let point = Point::new((r.line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, r.column.max(0) as usize));
        let mut current = tree.root_node().descendant_for_point_range(point, point);
        while let Some(node) = current {
            let parameters = node.child_by_field_name("parameters")
                .or_else(|| node.child_by_field_name("parameter"));
            if parameters.is_some_and(|p| destructured_binding_property(p, name, source.text()).is_some()) {
                return Some((node.start_position(), node.end_position()));
            }
            current = node.parent();
        }
        None
    }

    pub(super) fn js_typed_destructured_member(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        if !is_js_family(&r.language)
            || matches!(r.language.as_str(), "javascript" | "jsx")
            || r.reference_kind != "calls"
            || !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name)
            || !self.is_receiver_less_call(r)?
        {
            return Ok(None);
        }
        self.js_source_local_binding_within(r, None);
        let Some(source) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let Some(tree) = self.js_binding_tree(&source, r) else {
            return Ok(None);
        };
        let Some(line) = source.get((r.line - 1).max(0) as usize) else {
            return Ok(None);
        };
        let point = Point::new(
            (r.line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, r.column.max(0) as usize),
        );
        let mut current = tree.root_node().descendant_for_point_range(point, point);
        while let Some(node) = current {
            if node
                .child_by_field_name("parameter")
                .is_some_and(|parameter| {
                    destructured_binding_property(parameter, &r.reference_name, source.text())
                        .is_some()
                })
            {
                return Ok(None);
            }
            if let Some(parameters) = node.child_by_field_name("parameters") {
                for parameter in super::iteration::named_children(parameters) {
                    let Some(pattern) = parameter
                        .child_by_field_name("pattern")
                        .or_else(|| parameter.child_by_field_name("name"))
                    else {
                        continue;
                    };
                    let Some(property) =
                        destructured_binding_property(pattern, &r.reference_name, source.text())
                    else {
                        continue;
                    };
                    let Some(property) = property else {
                        return Ok(None);
                    };
                    let key = format!("{}\0{}", r.language, r.file_path);
                    if self
                        .upstream_js_bindings
                        .get(&key)
                        .and_then(|index| index.by_name.get(&r.reference_name))
                        .is_some_and(|facts| {
                            facts.iter().any(|fact| {
                                fact.scope.is_some_and(|(start, end)| {
                                    node.start_position() <= start
                                        && end <= node.end_position()
                                        && visible(fact.scope, point)
                                })
                            })
                        })
                    {
                        return Ok(None);
                    }
                    let Some(annotation) = parameter.child_by_field_name("type") else {
                        return Ok(None);
                    };
                    let Some(ty) = annotation.named_child(0) else {
                        return Ok(None);
                    };
                    if ty.kind() != "type_identifier" {
                        return Ok(None);
                    }
                    let type_name = &source.text()[ty.start_byte()..ty.end_byte()];
                    let own: Vec<_> = self
                        .nodes_in_file(&r.file_path)?
                        .iter()
                        .filter(|n| {
                            n.name == type_name
                                && matches!(n.kind.as_str(), "type_alias" | "interface" | "class")
                        })
                        .cloned()
                        .collect();
                    let owner = if let [owner] = own.as_slice() {
                        Some(owner.clone())
                    } else if !own.is_empty() {
                        None
                    } else {
                        let imports = self.import_mappings(&r.file_path)?;
                        let Some(mapping) = imports
                            .iter()
                            .find(|mapping| mapping.local_name == type_name)
                        else {
                            return Ok(None);
                        };
                        if self.is_external_import(&mapping.source, &r.language, &r.file_path) {
                            return Ok(None);
                        }
                        let type_ref = r.clone().naming(type_name, "references");
                        self.resolve_via_import(&type_ref)?
                            .map(|hit| hit.node)
                            .filter(|n| {
                                matches!(n.kind.as_str(), "type_alias" | "interface" | "class")
                            })
                    };
                    let Some(owner) = owner else { return Ok(None) };
                    if !self.is_lexically_reachable(&owner, r)? {
                        return Ok(None);
                    }
                    let qualified = format!("{}::{property}", owner.qualified_name);
                    let members: Vec<_> = self
                        .nodes_by_qualified_name(&qualified)?
                        .iter()
                        .filter(|n| {
                            n.file_path == owner.file_path
                                && n.visibility.as_deref() != Some("private")
                                && matches!(n.kind.as_str(), "method" | "property" | "field")
                        })
                        .cloned()
                        .collect();
                    let members: Vec<_> = members
                        .into_iter()
                        .filter(|member| self.js_callable_type_member(member, r))
                        .collect();
                    return Ok(if let [member] = members.as_slice() {
                        Some(member.clone())
                    } else {
                        None
                    });
                }
            }
            current = node.parent();
        }
        Ok(None)
    }

    fn js_callable_type_member(&mut self, member: &KNode, r: &ResolveRefIn) -> bool {
        if member.kind == "method" {
            return true;
        }
        let Some(source) = self.read_file(&member.file_path) else {
            return false;
        };
        let site = r.clone().at(member);
        let Some(tree) = self.js_binding_tree(&source, &site) else {
            return false;
        };
        let Some(line) = source.get((member.start_line - 1).max(0) as usize) else {
            return false;
        };
        let point = Point::new(
            (member.start_line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, member.start_column.max(0) as usize),
        );
        let mut current = tree.root_node().descendant_for_point_range(point, point);
        while let Some(node) = current {
            if matches!(
                node.kind(),
                "property_signature" | "public_field_definition"
            ) {
                return node.child_by_field_name("name").is_some_and(|name| {
                    source.text()[name.start_byte()..name.end_byte()] == member.name
                }) && node
                    .child_by_field_name("type")
                    .and_then(|annotation| annotation.named_child(0))
                    .is_some_and(|ty| ty.kind() == "function_type");
            }
            current = node.parent();
        }
        false
    }

    pub(super) fn js_declaration_reachable(&mut self, n: &KNode, r: &ResolveRefIn) -> Option<bool> {
        if !is_js_family(&n.language)
            || n.file_path != r.file_path
            || !matches!(n.kind.as_str(), "function" | "variable" | "constant")
        {
            return None;
        }
        let source = self.read_file(&n.file_path)?;
        let tree = self.js_binding_tree(&source, r)?;
        let line = source.get((r.line - 1).max(0) as usize)?;
        let point = Point::new(
            (r.line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, r.column.max(0) as usize),
        );
        let line = source.get((n.start_line - 1).max(0) as usize)?;
        let declaration = Point::new(
            (n.start_line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, n.start_column.max(0) as usize),
        );
        let mut current = tree
            .root_node()
            .descendant_for_point_range(declaration, declaration);
        while let Some(node) = current {
            if matches!(
                node.kind(),
                "variable_declarator" | "function_declaration" | "generator_function_declaration"
            ) && node.start_position() <= declaration
                && declaration < node.end_position()
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| source.text()[name.start_byte()..name.end_byte()] == n.name)
            {
                return Some(visible(binding_scope(node), point));
            }
            current = node.parent();
        }
        None
    }

    pub(super) fn js_source_local_binding(&mut self, r: &ResolveRefIn) -> bool {
        self.js_source_local_binding_within(r, None)
    }

    pub(super) fn js_source_local_binding_within(
        &mut self,
        r: &ResolveRefIn,
        caller: Option<&KNode>,
    ) -> bool {
        if !is_js_family(&r.language) || !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name) {
            return false;
        }
        if self.resolve_via_import(r).ok().flatten().is_some() {
            return false;
        }
        let Some((key, point)) = self.js_binding_index(r) else { return false };
        self.upstream_js_bindings
            .get(&key)
            .and_then(|index| index.by_name.get(&r.reference_name))
            .is_some_and(|facts| {
                facts.iter().any(|fact| {
                    caller.is_none_or(|f| {
                        fact.start.row >= (f.start_line - 1).max(0) as usize
                            && fact.end.row < f.end_line.max(0) as usize
                    }) && visible(fact.scope, point)
                })
            })
    }
    fn js_binding_index(&mut self, r: &ResolveRefIn) -> Option<(String, Point)> {
        let source = self.read_file(&r.file_path)?;
        let key = format!("{}\0{}", r.language, r.file_path);
        if !self
            .upstream_js_bindings
            .get(&key)
            .is_some_and(|index| Rc::ptr_eq(&index.source, &source))
        {
            let tree = self.js_binding_tree(&source, r)?;
            let mut by_name: HashMap<String, Vec<JsBindingFact>> = HashMap::new();
            let mut queue = vec![tree.root_node()];
            while let Some(node) = queue.pop() {
                if matches!(
                    node.kind(),
                    "function_declaration"
                        | "generator_function_declaration"
                        | "function_expression"
                        | "generator_function"
                ) {
                    if let Some(name) = node.child_by_field_name("name") {
                        let scope = if matches!(
                            node.kind(),
                            "function_expression" | "generator_function"
                        ) {
                            Some((node.start_position(), node.end_position()))
                        } else {
                            binding_scope(node)
                        };
                        by_name
                            .entry(source.text()[name.start_byte()..name.end_byte()].to_string())
                            .or_default()
                            .push(JsBindingFact {
                                start: node.start_position(),
                                end: node.end_position(),
                                scope,
                            });
                    }
                }
                if node.kind() == "variable_declarator" {
                    if let Some(pattern) = node.child_by_field_name("name") {
                        let mut names = vec![pattern];
                        let scope = binding_scope(node);
                        while let Some(name) = names.pop() {
                            if matches!(
                                name.kind(),
                                "identifier" | "shorthand_property_identifier_pattern"
                            ) {
                                by_name
                                    .entry(
                                        source.text()[name.start_byte()..name.end_byte()]
                                            .to_string(),
                                    )
                                    .or_default()
                                    .push(JsBindingFact {
                                        start: node.start_position(),
                                        end: node.end_position(),
                                        scope,
                                    });
                            } else if name.kind() == "pair_pattern" {
                                if let Some(value) = name.child_by_field_name("value") {
                                    names.push(value);
                                }
                            } else if matches!(
                                name.kind(),
                                "assignment_pattern" | "object_assignment_pattern"
                            ) {
                                if let Some(left) = name.child_by_field_name("left") {
                                    names.push(left);
                                }
                            } else {
                                names.extend(super::iteration::named_children(name));
                            }
                        }
                    }
                }
                queue.extend(super::iteration::named_children(node));
            }
            self.upstream_js_bindings.insert(
                key.clone(),
                JsBindingIndex {
                    source: source.clone(),
                    by_name,
                },
            );
        }
        let line = source.get((r.line - 1).max(0) as usize)?;
        let point = Point::new(
            (r.line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, r.column.max(0) as usize),
        );
        Some((key, point))
    }

    pub(super) fn js_root_binding_scope(&mut self, r: &ResolveRefIn, name: &str) -> Option<(Point, Point)> {
        let parameter = self.js_parameter_scope(r, name);
        let (key, point) = self.js_binding_index(r)?;
        self.upstream_js_bindings.get(&key).and_then(|index| index.by_name.get(name))
            .into_iter().flatten().filter_map(|fact| fact.scope)
            .filter(|scope| visible(Some(*scope), point)).chain(parameter)
            .max_by_key(|(start, _)| *start)
    }

}

fn destructured_binding_property(
    pattern: Node<'_>,
    name: &str,
    text: &str,
) -> Option<Option<String>> {
    match pattern.kind() {
        "required_parameter" | "optional_parameter" => {
            destructured_binding_property(pattern.child_by_field_name("pattern")
                .or_else(|| pattern.child_by_field_name("name"))?, name, text)
        }
        "shorthand_property_identifier_pattern"
            if &text[pattern.start_byte()..pattern.end_byte()] == name =>
        {
            Some(Some(name.to_string()))
        }
        "identifier" if &text[pattern.start_byte()..pattern.end_byte()] == name => Some(None),
        "pair_pattern" => {
            let value = pattern.child_by_field_name("value")?;
            if value.kind() == "identifier" && &text[value.start_byte()..value.end_byte()] == name {
                let key = pattern.child_by_field_name("key")?;
                Some(
                    matches!(key.kind(), "property_identifier" | "identifier")
                        .then(|| text[key.start_byte()..key.end_byte()].to_string()),
                )
            } else {
                destructured_binding_property(value, name, text).map(|_| None)
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            destructured_binding_property(pattern.child_by_field_name("left")?, name, text)
                .map(|_| None)
        }
        "object_pattern" | "array_pattern" | "rest_pattern" | "formal_parameters" => {
            super::iteration::named_children(pattern)
                .into_iter()
                .find_map(|child| {
                    let property = destructured_binding_property(child, name, text)?;
                    Some(if pattern.kind() == "object_pattern" {
                        property
                    } else {
                        None
                    })
                })
        }
        _ => None,
    }
}
