//! C++ declaration parameter facts and defaults shared by exact overload families.
use super::overloads_upstream::split_top_level;
use super::*;

#[derive(Clone)]
struct CppParameters {
    params: Vec<String>,
    types: Vec<String>,
    defaults: Vec<bool>,
    suffix: String,
    owner: Vec<String>,
}

pub(super) struct CppDefaultDeclarationIndex {
    source: Rc<SourceFile>,
    by_name: HashMap<String, Vec<(usize, usize, CppParameters)>>,
    includes: Vec<(i64, i64, String)>,
    namespaces: Vec<NamespaceScope>,
}

pub(super) struct CppDeclarationCache {
    source: Rc<SourceFile>,
    raw: Option<CppParameters>,
}

fn parameter_end(node: tree_sitter::Node<'_>, text: &str) -> usize {
    node.child_by_field_name("default_value")
        .map(|value| {
            text[node.start_byte()..value.start_byte()]
                .rfind('=')
                .map(|i| node.start_byte() + i)
                .unwrap_or(value.start_byte())
        })
        .unwrap_or(node.end_byte())
}

fn without_spans(text: &str, start: usize, end: usize, mut removed: Vec<(usize, usize)>) -> String {
    removed.sort_unstable();
    let mut result = String::new();
    let mut position = start;
    for (lo, hi) in removed {
        if lo >= position && hi <= end {
            result.push_str(&text[position..lo]);
            position = hi;
        }
    }
    result.push_str(&text[position..end]);
    result.chars().filter(|c| !c.is_whitespace()).collect()
}

fn parameter_type(node: tree_sitter::Node<'_>, text: &str) -> String {
    let mut binder = node.child_by_field_name("declarator");
    let mut removed = Vec::new();
    let mut compound = false;
    let mut last_pointer = None;
    let mut reference_or_array = false;
    while let Some(current) = binder {
        if current.kind() == "identifier" {
            removed.push((current.start_byte(), current.end_byte()));
            break;
        }
        if current.kind() == "pointer_declarator" {
            last_pointer = Some(current);
        }
        reference_or_array |= matches!(
            current.kind(),
            "reference_declarator" | "array_declarator" | "function_declarator"
        );
        compound |= matches!(
            current.kind(),
            "pointer_declarator"
                | "reference_declarator"
                | "array_declarator"
                | "function_declarator"
        );
        binder = current.child_by_field_name("declarator").or_else(|| {
            (current.kind() == "parenthesized_declarator")
                .then(|| current.named_child(0))
                .flatten()
        });
    }
    // Value parameter cv does not distinguish a function signature. Preserve
    // qualifiers on the pointee and on reference parameter types.
    if !compound {
        removed.extend(
            super::iteration::named_children(node)
                .into_iter()
                .filter(|child| child.kind() == "type_qualifier")
                .map(|child| (child.start_byte(), child.end_byte())),
        );
    }
    if !reference_or_array {
        if let Some(pointer) = last_pointer {
            removed.extend(
                super::iteration::named_children(pointer)
                    .into_iter()
                    .filter(|child| child.kind() == "type_qualifier")
                    .map(|child| (child.start_byte(), child.end_byte())),
            );
        }
    }
    without_spans(text, node.start_byte(), parameter_end(node, text), removed)
}

#[derive(Clone)]
struct NamespaceScope {
    start: tree_sitter::Point,
    end: tree_sitter::Point,
    names: Vec<String>,
}

fn physical_namespace_scopes(
    source: &SourceFile,
    file: &str,
    root: tree_sitter::Node<'_>,
) -> Vec<NamespaceScope> {
    // Keep original byte offsets. CST literal tokens distinguish character
    // literals from numeric digit separators; the C++ lexer covers raw strings.
    let mut code = super::cpp::mask_cpp_raw_strings(source.text())
        .as_bytes()
        .to_vec();
    for byte in &mut code {
        if *byte == 0 {
            *byte = b' ';
        }
    }
    let mut ast_namespaces = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "comment" | "string_literal" | "raw_string_literal" | "char_literal"
        ) {
            for byte in &mut code[node.start_byte()..node.end_byte()] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
            continue;
        }
        if node.kind() == "namespace_definition" {
            ast_namespaces.push((node.start_position(), node.end_position()));
        }
        pending.extend(super::iteration::named_children(node));
    }
    let mut directive = false;
    let mut start = 0;
    for end in source
        .text()
        .match_indices('\n')
        .map(|(at, _)| at + 1)
        .chain(std::iter::once(source.text().len()))
    {
        let line = &code[start..end];
        let first = line.iter().position(|b| !b.is_ascii_whitespace());
        let current = directive || first.is_some_and(|at| line[at] == b'#');
        directive = current && line.iter().rfind(|b| !b.is_ascii_whitespace()) == Some(&b'\\');
        if current {
            for byte in &mut code[start..end] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
        }
        start = end;
    }
    let code = String::from_utf8(code).unwrap_or_default();
    let starts: Vec<usize> = std::iter::once(0)
        .chain(source.text().match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let point = |offset: usize| {
        let row = starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        tree_sitter::Point::new(row, offset - starts[row])
    };
    let mut scopes = Vec::new();
    let mut braces: Vec<(tree_sitter::Point, Option<Vec<String>>)> = Vec::new();
    for token in
        re!(r"\b(?:inline\s+)?namespace(?:\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*))?\s*\{|[{}]")
            .captures_iter(&code)
    {
        let found = token.get(0).unwrap();
        if found.as_str() == "}" {
            if let Some((start, Some(names))) = braces.pop() {
                let end = point(found.start());
                // A sound AST namespace takes precedence over a contradictory
                // physical brace extent at the same declaration.
                if ast_namespaces.iter().all(|(ast_start, ast_end)| {
                    *ast_start != start
                        || (ast_end.row == end.row && ast_end.column == end.column + 1)
                }) {
                    scopes.push(NamespaceScope { start, end, names });
                }
            }
        } else {
            let names = if found.as_str() == "{" {
                None
            } else {
                Some(
                    token
                        .get(1)
                        .map(|name| name.as_str().split("::").map(str::to_string).collect())
                        .unwrap_or_else(|| vec![format!("anonymous:{file}")]),
                )
            };
            braces.push((point(found.start()), names));
        }
    }
    scopes
}

fn namespace_wrapper(
    node: tree_sitter::Node<'_>,
    source: &SourceFile,
    namespaces: &[NamespaceScope],
) -> bool {
    let function = if node.kind() == "compound_statement" {
        node.parent()
    } else {
        Some(node)
    };
    let Some(function) = function.filter(|n| n.kind() == "function_definition") else {
        return false;
    };
    let Some(name) = function.child_by_field_name("declarator") else {
        return false;
    };
    if &source.text()[name.start_byte()..name.end_byte()] != "namespace" {
        return false;
    }
    let Some(body) = function.child_by_field_name("body") else {
        return false;
    };
    namespaces.iter().any(|scope| {
        scope.start >= function.start_position()
            && scope.start < body.start_position()
            && scope.end.row == body.end_position().row
            && scope.end.column + 1 == body.end_position().column
    })
}

fn cpp_function_identity(
    function: tree_sitter::Node<'_>,
    source: &SourceFile,
) -> Option<(String, Vec<String>)> {
    let declarator = function.child_by_field_name("declarator")?;
    if !matches!(
        declarator.kind(),
        "identifier"
            | "field_identifier"
            | "qualified_identifier"
            | "operator_name"
            | "destructor_name"
    ) {
        return None;
    }
    let spelling = &source.text()[declarator.start_byte()..declarator.end_byte()];
    // An export macro can be parsed as the return type, leaving the actual
    // return type and function name together in a qualified declarator.
    if let Some(found) =
        re!(r"^([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s+([A-Za-z_]\w*)$").captures(spelling)
    {
        let declaration = function.parent()?;
        let ty = declaration.child_by_field_name("type")?;
        let prefix = &source.text()[ty.start_byte()..ty.end_byte()];
        if declaration.kind() == "declaration" && re!(r"^[A-Z_][A-Z_0-9]*$").is_match(prefix) {
            return Some((found[2].to_string(), Vec::new()));
        }
        return None;
    }
    let spelling: String = spelling.chars().filter(|c| !c.is_whitespace()).collect();
    let mut pieces: Vec<_> = spelling.split("::").map(str::to_string).collect();
    let name = pieces.pop()?;
    Some((name, pieces))
}

fn cpp_parameter_shape_complete(params: tree_sitter::Node<'_>) -> bool {
    let mut pending = vec![params];
    while let Some(node) = pending.pop() {
        if node.is_error() || node.is_missing() {
            return false;
        }
        // Empty braced defaults can carry a recovered missing expression type.
        // The signature needs the parameter's type/declarator and a written
        // initializer, independently of that initializer's expression type.
        let default = node
            .child_by_field_name("default_value")
            .filter(|value| !value.is_missing() && value.start_byte() < value.end_byte());
        if let Some(value) = default.filter(|value| value.has_error()) {
            let children = super::iteration::named_children(value);
            let recovered_empty_type = value.kind() == "compound_literal_expression"
                && children.len() == 2
                && children[0].kind() == "type_identifier"
                && children[0].is_missing()
                && children[0].start_byte() == children[0].end_byte()
                && children[1].kind() == "initializer_list"
                && !children[1].has_error();
            if !recovered_empty_type {
                return false;
            }
        }
        let mut cursor = node.walk();
        pending.extend(
            node.children(&mut cursor)
                .filter(|child| default.is_none_or(|value| value.id() != child.id())),
        );
    }
    true
}

fn cpp_parameters(
    function: tree_sitter::Node<'_>,
    params: tree_sitter::Node<'_>,
    source: &SourceFile,
    file: &str,
    macro_frames: &[(i64, i64, Vec<String>)],
    namespaces: &[NamespaceScope],
) -> CppParameters {
    let mut cursor = params.walk();
    let children: Vec<_> = params
        .children(&mut cursor)
        .filter(|p| !p.kind().contains("comment") && (p.is_named() || p.kind() == "..."))
        .collect();
    let excluded_returns = super::iteration::named_children(function)
        .into_iter()
        .filter(|node| node.kind() == "trailing_return_type")
        .map(|node| (node.start_byte(), node.end_byte()))
        .collect();
    let mut suffix = without_spans(
        source.text(),
        params.end_byte(),
        function.end_byte(),
        excluded_returns,
    );
    let mut parent = function.parent();
    while let Some(node) = parent {
        if node.kind() == "template_declaration" {
            for child in super::iteration::named_children(node) {
                if matches!(child.kind(), "template_parameter_list" | "requires_clause") {
                    suffix.push_str(
                        &source.text()[child.start_byte()..child.end_byte()]
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect::<String>(),
                    );
                }
            }
            break;
        }
        if node.kind() == "compound_statement" {
            break;
        }
        parent = node.parent();
    }
    let mut scopes: Vec<(usize, usize, Vec<String>)> = macro_frames
        .iter()
        .filter(|(start, end, _)| {
            *start <= function.start_position().row as i64 + 1
                && (function.start_position().row as i64) < *end
        })
        .map(|(start, _, names)| ((*start - 1).max(0) as usize, 0, names.clone()))
        .collect();
    scopes.extend(
        namespaces
            .iter()
            .filter(|scope| {
                scope.start <= function.start_position() && function.start_position() < scope.end
            })
            .map(|scope| (scope.start.row, scope.start.column, scope.names.clone())),
    );
    let mut parent = function.parent();
    while let Some(scope) = parent {
        if matches!(
            scope.kind(),
            "namespace_definition" | "class_specifier" | "struct_specifier"
        ) {
            let physical = scope.kind() == "namespace_definition"
                && namespaces.iter().any(|frame| {
                    frame.start == scope.start_position()
                        && frame.end.row == scope.end_position().row
                        && frame.end.column + 1 == scope.end_position().column
                });
            if physical {
                parent = scope.parent();
                continue;
            }
            let names = if let Some(name) = scope.child_by_field_name("name") {
                source.text()[name.start_byte()..name.end_byte()]
                    .split("::")
                    .map(str::to_string)
                    .collect()
            } else if scope.kind() == "namespace_definition" {
                vec![format!("anonymous:{}", file)]
            } else {
                Vec::new()
            };
            scopes.push((
                scope.start_position().row,
                scope.start_position().column,
                names,
            ));
        }
        parent = scope.parent();
    }
    scopes.sort_by_key(|(row, column, _)| (*row, *column));
    let mut owner: Vec<String> = scopes.into_iter().flat_map(|(_, _, names)| names).collect();
    if let Some((_, qualifiers)) = cpp_function_identity(function, source) {
        owner.extend(qualifiers);
    }
    CppParameters {
        owner,
        params: children
            .iter()
            .map(|p| source.text()[p.start_byte()..parameter_end(*p, source.text())].to_string())
            .collect(),
        types: children
            .iter()
            .map(|p| parameter_type(*p, source.text()))
            .collect(),
        defaults: children
            .iter()
            .map(|p| p.child_by_field_name("default_value").is_some())
            .collect(),
        suffix,
    }
}

impl KernelResolver {
    pub(super) fn cpp_declaration_owner(&mut self, n: &KNode) -> Option<Vec<String>> {
        self.cpp_parameter_facts(n).map(|facts| facts.owner.clone())
    }

    fn cpp_parameter_facts(&mut self, n: &KNode) -> Option<CppParameters> {
        let source = self.read_file(&n.file_path)?;
        if let Some(cached) = self.cpp_declaration_memo.get(&n.id) {
            if Rc::ptr_eq(&cached.source, &source) {
                return cached.raw.clone();
            }
        }
        let site = ResolveRefIn {
            row_id: None,
            from_node_id: n.id.clone(),
            reference_name: n.name.clone(),
            reference_kind: "references".to_string(),
            line: n.start_line,
            column: n.start_column,
            candidates: None,
            file_path: n.file_path.clone(),
            language: n.language.clone(),
            failure_reason: None,
        };
        self.ensure_cpp_default_index(&n.file_path, &site)?;
        let namespaces = self
            .cpp_default_declaration_memo
            .get(&n.file_path)?
            .namespaces
            .clone();
        let tree = self.parsed_tree(&source, &site)?;
        let first = source.get((n.start_line - 1).max(0) as usize)?;
        let last = source.get((n.end_line - 1).max(0) as usize)?;
        let symbol_start = tree_sitter::Point::new(
            (n.start_line - 1).max(0) as usize,
            super::names::js_unit_to_byte(first, n.start_column.max(0) as usize),
        );
        let symbol_end = tree_sitter::Point::new(
            (n.end_line - 1).max(0) as usize,
            super::names::js_unit_to_byte(last, n.end_column.max(0) as usize),
        );
        let mut queue = vec![tree.root_node()];
        let mut best = None;
        while let Some(node) = queue.pop() {
            if node.end_position().row < (n.start_line - 1).max(0) as usize
                || node.start_position().row >= n.end_line.max(n.start_line) as usize
            {
                continue;
            }
            if node.kind() == "function_declarator"
                && node.start_position() >= symbol_start
                && node.end_position() <= symbol_end
            {
                if let (Some(_name), Some(params)) = (
                    node.child_by_field_name("declarator"),
                    node.child_by_field_name("parameters"),
                ) {
                    if cpp_function_identity(node, &source).is_some_and(|(name, _)| name == n.name)
                        && best.is_none_or(
                            |(prior, _): (tree_sitter::Node<'_>, tree_sitter::Node<'_>)| {
                                node.start_byte() < prior.start_byte()
                            },
                        )
                    {
                        best = Some((node, params));
                    }
                }
            }
            queue.extend(super::iteration::named_children(node));
        }
        let macro_frames = self.namespace_frames(&n.file_path).ok()?;
        let raw = best
            .filter(|(_, params)| cpp_parameter_shape_complete(*params))
            .map(|(function, params)| {
                cpp_parameters(
                    function,
                    params,
                    &source,
                    &n.file_path,
                    &macro_frames,
                    &namespaces,
                )
            });
        self.cpp_declaration_memo.insert(
            n.id.clone(),
            CppDeclarationCache {
                source,
                raw: raw.clone(),
            },
        );
        raw
    }

    fn ensure_cpp_default_index(&mut self, file: &str, r: &ResolveRefIn) -> Option<()> {
        let source = self.read_file(file)?;
        let valid = self
            .cpp_default_declaration_memo
            .get(file)
            .is_some_and(|cached| Rc::ptr_eq(&cached.source, &source));
        if !valid {
            let mut site = r.clone();
            site.file_path = file.to_string();
            site.language = "cpp".to_string();
            let tree = self.parsed_tree(&source, &site)?;
            let frames = self.namespace_frames(file).ok()?;
            let namespaces = physical_namespace_scopes(&source, file, tree.root_node());
            let mut by_name: HashMap<String, Vec<(usize, usize, CppParameters)>> = HashMap::new();
            let mut pending = vec![tree.root_node()];
            while let Some(node) = pending.pop() {
                // Block-local declarations cannot supply namespace/class defaults.
                if matches!(node.kind(), "function_definition" | "compound_statement")
                    && !namespace_wrapper(node, &source, &namespaces)
                {
                    continue;
                }
                if node.kind() == "function_declarator" {
                    if let (Some(_declarator), Some(params)) = (
                        node.child_by_field_name("declarator"),
                        node.child_by_field_name("parameters"),
                    ) {
                        let identity = cpp_function_identity(node, &source);
                        let mut parent = node.parent();
                        while parent.is_some_and(|p| {
                            matches!(p.kind(), "pointer_declarator" | "reference_declarator")
                        }) {
                            parent = parent.and_then(|p| p.parent());
                        }
                        let prototype = identity.is_some()
                            && parent.is_some_and(|p| {
                                matches!(p.kind(), "declaration" | "field_declaration")
                            });
                        if prototype
                            && cpp_parameter_shape_complete(params)
                            && super::iteration::named_children(params)
                                .iter()
                                .any(|p| p.child_by_field_name("default_value").is_some())
                        {
                            let position = node.start_position();
                            by_name.entry(identity.unwrap().0).or_default().push((
                                position.row,
                                position.column,
                                cpp_parameters(node, params, &source, file, &frames, &namespaces),
                            ));
                        }
                    }
                }
                pending.extend(super::iteration::named_children(node));
            }
            let specs: Vec<_> = self
                .nodes_in_file(file)
                .ok()?
                .iter()
                .filter(|node| node.kind == "import")
                .filter_map(|node| {
                    let captures = re!(r#"#\s*include\s*([<"])([^>"]+)[>"]"#)
                        .captures(node.signature.as_deref()?)?;
                    Some((
                        node.start_line,
                        node.start_column,
                        captures[1].chars().next()?,
                        captures[2].to_string(),
                    ))
                })
                .collect();
            let mut includes = Vec::new();
            for (line, column, quote, spec) in specs {
                if let Some(target) = self.resolve_cpp_include(file, quote, &spec, "cpp").ok()? {
                    includes.push((line, column, target));
                }
            }
            self.cpp_default_declaration_memo.insert(
                file.to_string(),
                CppDefaultDeclarationIndex {
                    source: source.clone(),
                    by_name,
                    includes,
                    namespaces,
                },
            );
        }
        Some(())
    }

    fn cpp_default_visible_files(&mut self, r: &ResolveRefIn) -> Option<HashSet<String>> {
        let mut visible = HashSet::new();
        let mut pending = vec![r.file_path.clone()];
        while let Some(file) = pending.pop() {
            if !visible.insert(file.clone()) {
                continue;
            }
            self.ensure_cpp_default_index(&file, r)?;
            let index = self.cpp_default_declaration_memo.get(&file)?;
            pending.extend(
                index
                    .includes
                    .iter()
                    .filter(|(line, column, _)| {
                        file != r.file_path || (*line, *column) < (r.line, r.column)
                    })
                    .map(|(_, _, target)| target.clone()),
            );
        }
        Some(visible)
    }

    fn cpp_visible_default_declarations(
        &mut self,
        file: &str,
        name: &str,
        r: &ResolveRefIn,
    ) -> Option<Vec<CppParameters>> {
        self.ensure_cpp_default_index(file, r)?;
        let source = &self.cpp_default_declaration_memo.get(file)?.source;
        let call_row = (r.line - 1).max(0) as usize;
        let call_column = source
            .get(call_row)
            .map(|line| super::names::js_unit_to_byte(line, r.column.max(0) as usize))
            .unwrap_or(0);
        Some(
            self.cpp_default_declaration_memo
                .get(file)?
                .by_name
                .get(name)
                .into_iter()
                .flatten()
                .filter(|(row, column, _)| {
                    file != r.file_path || (*row, *column) < (call_row, call_column)
                })
                .map(|(_, _, facts)| facts.clone())
                .collect(),
        )
    }

    pub(super) fn declaration_parameters(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<Vec<String>> {
        if n.language == "cpp" {
            if let Some(facts) = self.cpp_parameter_facts(n) {
                let mut params = facts.params.clone();
                let visible = self.cpp_default_visible_files(r)?;
                let mut defaults = if visible.contains(&n.file_path)
                    && (n.file_path != r.file_path
                        || (n.start_line, n.start_column) < (r.line, r.column))
                {
                    facts.defaults.clone()
                } else {
                    vec![false; facts.params.len()]
                };
                for file in visible {
                    let Some(declarations) =
                        self.cpp_visible_default_declarations(&file, &n.name, r)
                    else {
                        continue;
                    };
                    for other in declarations {
                        if facts.types != other.types
                            || facts.suffix != other.suffix
                            || facts.owner != other.owner
                        {
                            continue;
                        }
                        for (default, declared) in defaults.iter_mut().zip(other.defaults) {
                            *default |= declared;
                        }
                    }
                }
                for (param, default) in params.iter_mut().zip(defaults) {
                    if default {
                        param.push_str(" = default");
                    }
                }
                return Some(params);
            }
        }
        self.paren_list_after(&n.file_path, n.start_line, 0, &n.name)
            .map(|list| split_top_level(&list))
    }

    pub(super) fn declaration_arity(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<(usize, usize)> {
        let params: Vec<_> = self
            .declaration_parameters(n, r)?
            .into_iter()
            .filter(|p| p != "void")
            .collect();
        let pack = |p: &str| {
            let mut flat = p.to_string();
            loop {
                let next = re!(r"<[^<>]*>").replace_all(&flat, "").into_owned();
                if next == flat {
                    break;
                }
                flat = next;
            }
            flat.contains("...") || re!(r"\b(?:params|vararg)\s").is_match(&flat)
        };
        Some((
            params
                .iter()
                .filter(|p| !p.contains('=') && !pack(p))
                .count(),
            if params.iter().any(|p| pack(p)) {
                usize::MAX
            } else {
                params.len()
            },
        ))
    }
}
