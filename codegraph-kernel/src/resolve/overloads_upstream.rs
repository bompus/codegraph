//! Source argument shape disambiguates a method's overload family.
use super::*;

pub(super) fn split_top_level(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i64;
    let mut quote = None;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                out.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !text[start..].trim().is_empty() {
        out.push(text[start..].trim().to_string());
    }
    out
}

impl KernelResolver {
    pub(super) fn paren_list_after(
        &mut self,
        file: &str,
        line: i64,
        column: i64,
        name: &str,
    ) -> Option<String> {
        let lines = self.read_file(file)?;
        let text = lines
            .iter()
            .skip((line - 1).max(0) as usize)
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let col = super::names::js_unit_to_byte(&text, column.max(0) as usize).min(text.len());
        let pat = Self::cached_regex(&format!(
            r"\b{}\s*(?:<[^<>()]*>)?\s*\(",
            regex::escape(name)
        ))
        .ok()?;
        let found = pat.find(&text[col..])?;
        let open = col + found.end() - 1;
        let mut depth = 0;
        let mut quote = None;
        let mut escaped = false;
        for (offset, ch) in text[open..].char_indices() {
            if let Some(q) = quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == q {
                    quote = None;
                }
                continue;
            }
            if ch == '\'' || ch == '"' {
                quote = Some(ch);
                continue;
            }
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    return Some(text[open + 1..open + offset].to_string());
                }
            }
        }
        None
    }

    pub(super) fn call_arguments(&mut self, r: &ResolveRefIn, name: &str) -> Option<Vec<String>> {
        if let Some(source) = self.read_file(&r.file_path) {
            if let Some(tree) = self.parsed_tree(&source, r) {
                if let Some(line) = source.get((r.line - 1).max(0) as usize) {
                    let point = tree_sitter::Point::new(
                        (r.line - 1).max(0) as usize,
                        super::names::js_unit_to_byte(line, r.column.max(0) as usize),
                    );
                    let mut current = tree.root_node().descendant_for_point_range(point, point);
                    while let Some(node) = current {
                        if matches!(
                            node.kind(),
                            "call_expression" | "method_invocation" | "invocation_expression"
                        ) {
                            let callee = node
                                .child_by_field_name("name")
                                .or_else(|| node.child_by_field_name("function"))
                                .or_else(|| node.child_by_field_name("expression"));
                            let named = callee.is_some_and(|callee| {
                                let text = &source.text()[callee.start_byte()..callee.end_byte()];
                                Self::cached_regex(&format!(
                                    r"(?:^|[.:>])\s*{}(?:\s*<.*>)?\s*$",
                                    regex::escape(name)
                                ))
                                .is_ok_and(|pattern| pattern.is_match(text))
                            });
                            if !named {
                                current = node.parent();
                                continue;
                            }
                            if let Some(args) = node.child_by_field_name("arguments") {
                                return Some(
                                    super::iteration::named_children(args)
                                        .iter()
                                        .filter(|a| !a.kind().contains("comment"))
                                        .map(|a| {
                                            source.text()[a.start_byte()..a.end_byte()].to_string()
                                        })
                                        .collect(),
                                );
                            }
                        }
                        current = node.parent();
                    }
                }
            }
        }
        self.paren_list_after(&r.file_path, r.line, r.column, name)
            .map(|args| split_top_level(&args))
    }

    pub(super) fn retarget_overload(&mut self, mut hit: KCand, r: &ResolveRefIn) -> Res<KCand> {
        if r.language == "cpp"
            && r.reference_kind == "calls"
            && hit.node.kind == "method"
            && !super::cpp::is_cpp_constructor_ref(r)
        {
            if let Some((owner, name)) = hit.node.qualified_name.rsplit_once("::") {
                if owner.rsplit("::").next() == Some(name) {
                    if let Some(args) = self.call_arguments(r, name) {
                        let constructor = r
                            .clone()
                            .naming(&format!("::{owner}::{name}/{}", args.len()), "calls");
                        if let Some(candidate) = self.match_cpp_constructor(&constructor)? {
                            return Ok(candidate);
                        }
                    }
                }
            }
        }
        if r.reference_kind != "calls"
            || !matches!(
                r.language.as_str(),
                "csharp"
                    | "java"
                    | "kotlin"
                    | "swift"
                    | "cpp"
                    | "scala"
                    | "dart"
                    | "vbnet"
                    | "solidity"
            )
            || !matches!(hit.node.kind.as_str(), "function" | "method")
        {
            return Ok(hit);
        }
        let owner = hit
            .node
            .qualified_name
            .rsplit_once("::")
            .map(|(o, _)| o)
            .unwrap_or("");
        let siblings: Vec<_> = self
            .nodes_in_file(&hit.node.file_path)?
            .iter()
            .filter(|n| {
                n.id != hit.node.id
                    && n.name == hit.node.name
                    && matches!(n.kind.as_str(), "function" | "method")
                    && n.qualified_name
                        .rsplit_once("::")
                        .map(|(o, _)| o)
                        .unwrap_or("")
                        == owner
            })
            .cloned()
            .collect();
        if siblings.is_empty() {
            return Ok(hit);
        }
        if r.language == "swift" {
            if self.swift_overload_labels_fit(&hit.node, r) != Some(false) {
                return Ok(hit);
            }
            let fits: Vec<_> = siblings
                .into_iter()
                .filter(|n| self.swift_overload_labels_fit(n, r) == Some(true))
                .collect();
            if fits.len() == 1 {
                hit.node = fits[0].clone();
            }
            return Ok(hit);
        }
        let Some(own) = self.declaration_arity(&hit.node, r) else {
            return Ok(hit);
        };
        let Some(args) = self.call_arguments(r, &hit.node.name) else {
            return Ok(hit);
        };
        if let Some(Some(selected)) = self.cpp_scalar_overload_choice(&hit.node, r) {
            hit.node = selected;
            return Ok(hit);
        }
        let argc = args.len();
        if argc >= own.0 && argc <= own.1 {
            return Ok(hit);
        }
        let fits: Vec<_> = siblings
            .into_iter()
            .filter(|n| {
                self.declaration_arity(n, r)
                    .is_some_and(|(min, max)| argc >= min && argc <= max)
            })
            .collect();
        if fits.len() == 1 {
            hit.node = fits[0].clone();
        }
        Ok(hit)
    }

    fn cpp_scalar_argument_shape(
        &mut self,
        node: tree_sitter::Node,
        source: &SourceFile,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Option<bool> {
        if depth > 4 {
            return None;
        }
        let text = |n: tree_sitter::Node| &source.text()[n.start_byte()..n.end_byte()];
        match node.kind() {
            "string_literal" | "raw_string_literal"
                if text(node).starts_with('"') || text(node).starts_with("R\"") =>
            {
                Some(true)
            }
            "concatenated_string" => {
                let children: Vec<_> = super::iteration::named_children(node).into_iter().filter(|n| !n.kind().contains("comment")).collect();
                (!children.is_empty()
                    && children.into_iter().all(|child| {
                        self.cpp_scalar_argument_shape(child, source, r, depth + 1) == Some(true)
                    }))
                .then_some(true)
            }
            "number_literal" if re!(r"^[+-]?(?:0[xX][0-9a-fA-F](?:'?[0-9a-fA-F])*|0[bB][01](?:'?[01])*|[0-9](?:'?[0-9])*)(?:[uU](?:ll|LL|l|L)?|(?:ll|LL|l|L)[uU]?|[zZ][uU]?|[uU][zZ])?$").is_match(text(node)) => Some(false),
            "unary_expression" => {
                let argument = node.child_by_field_name("argument")?;
                let operator = source.text()[node.start_byte()..argument.start_byte()].trim();
                if matches!(operator, "+" | "-") && matches!(argument.kind(), "number_literal" | "unary_expression")
                    && self.cpp_scalar_argument_shape(argument, source, r, depth + 1) == Some(false) { Some(false) } else { None }
            }
            "true" | "false" => Some(false),
            "parenthesized_expression" => {
                self.cpp_scalar_argument_shape(node.named_child(0)?, source, r, depth + 1)
            }
            "conditional_expression" => {
                let left = self.cpp_scalar_argument_shape(
                    node.child_by_field_name("consequence")?,
                    source,
                    r,
                    depth + 1,
                )?;
                let right = self.cpp_scalar_argument_shape(
                    node.child_by_field_name("alternative")?,
                    source,
                    r,
                    depth + 1,
                )?;
                (left == right).then_some(left)
            }
            "identifier" => {
                let mut at = node;
                while let Some(parent) = at.parent() {
                    if let Some(shape) =
                        cpp_scalar_binding(parent, at, text(node), source, node.start_byte())
                    {
                        return shape;
                    }
                    at = parent;
                }
                None
            }
            "call_expression" => {
                let callee = node.child_by_field_name("function")?;
                let argc = super::iteration::named_children(node.child_by_field_name("arguments")?)
                    .into_iter()
                    .filter(|n| !n.kind().contains("comment"))
                    .count();
                let candidates = if callee.kind() == "field_expression" {
                    let receiver = callee.child_by_field_name("argument")?;
                    if receiver.kind() != "identifier" {
                        return None;
                    }
                    let mut at = receiver;
                    let owner = loop {
                        let parent = at.parent()?;
                        if let Some(raw) = cpp_visible_binding_type(
                            parent,
                            at,
                            text(receiver),
                            source,
                            receiver.start_byte(),
                        ) {
                            let raw = raw?;
                            if raw == "auto" {
                                return None;
                            }
                            break self.cpp_type_owner(&raw, r, 0, false).ok()??;
                        }
                        at = parent;
                    };
                    let name = text(callee.child_by_field_name("field")?);
                    return self.cpp_getter_return_shape(&owner, name, argc, r);
                } else if callee.kind() == "identifier" {
                    let mut at = callee;
                    while let Some(parent) = at.parent() {
                        if cpp_scalar_binding(parent, at, text(callee), source, callee.start_byte())
                            .is_some()
                        {
                            return None;
                        }
                        at = parent;
                    }
                    self.nodes_by_name(text(callee))
                        .ok()?
                        .iter()
                        .filter(|n| {
                            n.language == "cpp"
                                && n.file_path == r.file_path
                                && n.kind == "function"
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    return None;
                };
                let mut fits = Vec::new();
                let caller = self.node_by_id(&r.from_node_id).ok()??;
                let caller_owner = self.cpp_declaration_owner(&caller)?;
                for candidate in candidates {
                    if callee.kind() == "identifier" {
                        let owner = self.cpp_declaration_owner(&candidate)?;
                        if !caller_owner.starts_with(&owner) {
                            continue;
                        }
                    }
                    if !self.is_lexically_reachable(&candidate, r).ok()?
                        || !self
                            .declaration_arity(&candidate, r)
                            .is_some_and(|(min, max)| argc >= min && argc <= max)
                    {
                        continue;
                    }
                    fits.push(candidate);
                }
                let [method] = fits.as_slice() else {
                    return None;
                };
                let declaration_source = self.read_file(&method.file_path)?;
                let site = r.clone().at(method);
                let tree = self.parsed_tree(&declaration_source, &site)?;
                let mut declaration = super::iteration::descendant_for_position(
                    tree.root_node(),
                    declaration_source.text(),
                    (
                        (method.start_line - 1).max(0) as usize,
                        method.start_column.max(0) as usize + 1,
                    ),
                );
                loop {
                    if matches!(
                        declaration.kind(),
                        "function_definition" | "field_declaration" | "declaration"
                    ) {
                        let ty = declaration.child_by_field_name("type")?;
                        let raw = &declaration_source.text()[ty.start_byte()..ty.end_byte()];
                        let declarator = declaration.child_by_field_name("declarator")?;
                        let head = declaration_source.text()
                            [declarator.start_byte()..declarator.end_byte()]
                            .split('(')
                            .next()?;
                        if head.contains('*') {
                            return (raw.trim() == "char"
                                && head.matches('*').count() == 1
                                && !head.contains('['))
                            .then_some(true);
                        }
                        return cpp_scalar_type_shape(raw);
                    }
                    declaration = declaration.parent()?;
                }
            }
            _ => None,
        }
    }

    fn cpp_getter_return_shape(
        &mut self,
        owner: &KNode,
        name: &str,
        argc: usize,
        _r: &ResolveRefIn,
    ) -> Option<bool> {
        let source = self.read_file(&owner.file_path)?;
        let first = (owner.start_line - 1).max(0) as usize;
        let last = (owner.end_line - 1).max(0) as usize;
        let mut lines = source.get(first..=last)?.to_vec();
        let begin =
            super::names::js_unit_to_byte(lines.first()?, owner.start_column.max(0) as usize);
        let end = super::names::js_unit_to_byte(lines.last()?, owner.end_column.max(0) as usize);
        if first == last {
            lines[0] = lines[0].get(begin..end)?.to_string();
        } else {
            let last_line = lines.last()?.get(..end)?.to_string();
            *lines.last_mut()? = last_line;
            lines[0] = lines[0].get(begin..)?.to_string();
        }
        let code = lines.join("\n");
        let key = (
            format!("__cpp_scalar_getter:{}:{}:{}", owner.id, name, argc),
            code.clone(),
        );
        if let Some(cached) = self.declared_member_memo.get(&key) {
            return cached.as_deref().map(|shape| shape == "string");
        }
        self.declared_member_memo.insert(key.clone(), None);
        // Export decorators misparse as class names. Pad only the decorator
        // immediately before this already identified declaration's actual name.
        let export = Self::cached_regex(&format!(
            r"^\s*(?:class|struct)\s+(GTEST_API_\s+){}\b",
            regex::escape(&owner.name)
        ))
        .ok()?;
        let code = export.replace(&code, |caps: &regex::Captures| {
            let modifier = caps.get(1).unwrap();
            format!(
                "{}{}{}",
                &caps[0][..modifier.start() - caps.get(0).unwrap().start()],
                " ".repeat(modifier.as_str().len()),
                &caps[0][modifier.end() - caps.get(0).unwrap().start()..]
            )
        });
        let code = format!("{};", code);
        let tree = crate::tree::parse_with_cached_parser(&code, "cpp").ok()?;
        let text = |n: tree_sitter::Node| &code[n.start_byte()..n.end_byte()];
        let class = super::iteration::named_children(tree.root_node())
            .into_iter()
            .flat_map(|n| {
                if n.kind() == "declaration" {
                    super::iteration::named_children(n)
                } else {
                    vec![n]
                }
            })
            .find(|n| matches!(n.kind(), "class_specifier" | "struct_specifier"))?;
        if class
            .child_by_field_name("name")
            .is_none_or(|n| text(n) != owner.name)
        {
            return None;
        }
        let mut fits = Vec::new();
        for declaration in super::iteration::named_children(class.child_by_field_name("body")?) {
            if !matches!(
                declaration.kind(),
                "field_declaration" | "function_definition"
            ) {
                continue;
            }
            let Some(mut declarator) = declaration.child_by_field_name("declarator") else {
                continue;
            };
            let mut pointers = 0;
            while declarator.kind() != "function_declarator" {
                pointers += usize::from(declarator.kind() == "pointer_declarator");
                let Some(inner) = declarator
                    .child_by_field_name("declarator")
                    .or_else(|| declarator.named_child(0))
                else {
                    break;
                };
                declarator = inner;
            }
            if declarator.kind() != "function_declarator"
                || declarator
                    .child_by_field_name("declarator")
                    .is_none_or(|n| text(n) != name)
            {
                continue;
            }
            let mut params: Vec<_> =
                super::iteration::named_children(declarator.child_by_field_name("parameters")?)
                    .into_iter()
                    .filter(|n| !n.kind().contains("comment"))
                    .collect();
            if params.len() == 1 && text(params[0]).trim() == "void" {
                params.clear();
            }
            if params.iter().any(|n| {
                !matches!(
                    n.kind(),
                    "parameter_declaration" | "optional_parameter_declaration"
                )
            }) {
                continue;
            }
            let min = params
                .iter()
                .filter(|n| n.child_by_field_name("default_value").is_none())
                .count();
            if argc < min || argc > params.len() {
                continue;
            }
            let ty = declaration.child_by_field_name("type")?;
            let shape = if pointers == 1 && text(ty) == "char" {
                Some(true)
            } else if pointers == 0 {
                cpp_scalar_type_shape(text(ty))
            } else {
                None
            };
            fits.push(shape);
        }
        let [shape] = fits.as_slice() else {
            return None;
        };
        self.declared_member_memo.insert(
            key,
            shape.map(|value| if value { "string" } else { "integral" }.to_string()),
        );
        *shape
    }

    fn cpp_scalar_overload_choice(
        &mut self,
        hit: &KNode,
        r: &ResolveRefIn,
    ) -> Option<Option<Arc<KNode>>> {
        if r.language != "cpp" {
            return None;
        }
        let owner = self.cpp_declaration_owner(hit)?;
        let candidates = self.nodes_in_file(&hit.file_path).ok()?;
        let mut family = Vec::new();
        for n in candidates.iter().filter(|n| {
            n.qualified_name == hit.qualified_name
                && matches!(n.kind.as_str(), "function" | "method")
        }) {
            if self.cpp_declaration_owner(n).as_ref() == Some(&owner) {
                family.push(n.clone());
            }
        }
        let parameters: Vec<_> = family
            .iter()
            .map(|n| self.declaration_parameters(n, r))
            .collect::<Option<_>>()?;
        let first = parameters.first()?;
        if family.len() < 2 || parameters.iter().any(|p| p.len() != first.len()) {
            return None;
        }
        let mut discriminants = Vec::new();
        for i in 0..first.len() {
            let shapes: Vec<_> = parameters
                .iter()
                .map(|p| cpp_scalar_type_shape(&p[i]))
                .collect::<Option<_>>()
                .unwrap_or_default();
            if shapes.contains(&true) && shapes.contains(&false) {
                discriminants.push(i);
            } else {
                let key = cpp_parameter_type_key(&first[i]);
                if parameters
                    .iter()
                    .any(|p| cpp_parameter_type_key(&p[i]) != key)
                {
                    return None;
                }
            }
        }
        if discriminants.is_empty() {
            return None;
        }
        let source = self.read_file(&r.file_path)?;
        let tree = self.parsed_tree(&source, r)?;
        let mut call = super::iteration::descendant_for_position(
            tree.root_node(),
            source.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1),
        );
        let callee_pattern = Self::cached_regex(&format!(
            r"(?:^|[.:>])\s*{}(?:\s*<.*>)?\s*$",
            regex::escape(&hit.name)
        ))
        .ok()?;
        loop {
            if call.kind() == "call_expression"
                && call.child_by_field_name("function").is_some_and(|callee| {
                    callee_pattern.is_match(&source.text()[callee.start_byte()..callee.end_byte()])
                })
            {
                break;
            }
            call = call.parent()?;
        }
        let args: Vec<_> = super::iteration::named_children(call.child_by_field_name("arguments")?)
            .into_iter()
            .filter(|n| !n.kind().contains("comment"))
            .collect();
        let mut fits = Vec::new();
        for (n, params) in family.iter().zip(parameters) {
            if !self
                .declaration_arity(n, r)
                .is_some_and(|(min, max)| args.len() >= min && args.len() <= max)
            {
                continue;
            }
            let mut mismatch = false;
            for i in &discriminants {
                if let Some(arg) = args.get(*i) {
                    if let Some(actual) = self.cpp_scalar_argument_shape(*arg, &source, r, 0) {
                        if cpp_scalar_type_shape(&params[*i]) != Some(actual) {
                            mismatch = true;
                            break;
                        }
                    }
                }
            }
            if !mismatch {
                fits.push(n.clone());
            }
        }
        Some(match fits.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        })
    }

    pub(super) fn cpp_overload_compatible(
        &mut self,
        n: &KNode,
        args: &[String],
        r: &ResolveRefIn,
    ) -> bool {
        if !matches!(n.kind.as_str(), "function" | "method") {
            return true;
        }
        if self
            .declaration_arity(n, r)
            .is_some_and(|(min, max)| args.len() < min || args.len() > max)
        {
            return false;
        }
        let Some(params) = self.declaration_parameters(n, r) else {
            return true;
        };
        for (arg, param) in args.iter().zip(params) {
            let flat = re!(r"<[^<>]*>").replace_all(&param, "");
            if flat.contains("...") {
                break;
            }
            let compiled = re!(r#"\bFMT_COMPILE\s*\(|"\s*_cf\b"#).is_match(arg);
            if compiled && re!(r"\b(?:basic_format_string|format_string|wformat_string|basic_string_view|string_view|wstring_view|text_style|locale_ref|(?:basic_)?ostream|FILE)\b").is_match(&param) {
                return false;
            }
        }
        true
    }

    pub(super) fn cpp_overload_fit(&mut self, n: &KNode, args: &[String], r: &ResolveRefIn) -> i64 {
        if !matches!(n.kind.as_str(), "function" | "method") {
            return -1;
        }
        let Some(params) = self.declaration_parameters(n, r) else {
            return 0;
        };
        let mut score = 0;
        if self
            .declaration_arity(n, r)
            .is_some_and(|(min, max)| args.len() < min || args.len() > max)
        {
            score -= 3;
        }
        for (arg, param) in args.iter().zip(params.iter()) {
            let flat = re!(r"<[^<>]*>").replace_all(param, "");
            if flat.contains("...") {
                break;
            }
            if !re!(r#"^(?:u8|u|U|L)?"|^FMT_STRING\s*\("#).is_match(arg) {
                continue;
            }
            let wide_arg = arg.starts_with("L\"");
            let wide_param =
                re!(r"\bw(?:string|char_t|format|string_view)|wchar_t").is_match(param);
            if wide_arg != wide_param && re!(r"string|char|Char|format").is_match(param) {
                score -= 2;
            } else {
                score += if re!(r"string|char|Char|\bstr\b").is_match(param) {
                    3
                } else if re!(r"^(?:const\s+)?[A-Z]\w{0,2}\s*[&*]{0,2}\s*\w*$").is_match(param) {
                    1
                } else {
                    -2
                };
            }
        }
        score
    }

    pub(super) fn name_post_guard(&mut self, hit: &KCand, r: &ResolveRefIn) -> Res<bool> {
        if self.is_import_binding_call_target(&hit.node, r)? {
            return Ok(false);
        }
        if self.java_outside_import(r)? || !self.language_type_visible(&hit.node, r)? {
            return Ok(false);
        }
        if r.language == "cpp"
            && r.reference_kind == "calls"
            && !super::cpp::is_cpp_constructor_ref(r)
        {
            let candidate = self.retarget_overload(
                KCand {
                    node: hit.node.clone(),
                    confidence: hit.confidence,
                    resolved_by: hit.resolved_by,
                },
                r,
            )?;
            if matches!(
                self.cpp_scalar_overload_choice(&candidate.node, r),
                Some(None)
            ) {
                return Ok(false);
            }
            if let Some(args) = self.call_arguments(r, &candidate.node.name) {
                if !self.cpp_overload_compatible(&candidate.node, &args, r) {
                    return Ok(false);
                }
            }
        }
        let grounded_js = is_js_family(&r.language)
            && r.reference_kind == "calls"
            && (self
                .match_js_store_binding_call(r)?
                .is_some_and(|bound| bound.node.id == hit.node.id)
                || self
                    .resolve_via_import(r)?
                    .is_some_and(|imported| imported.node.id == hit.node.id)
                || self
                    .js_typed_destructured_member(r)?
                    .is_some_and(|member| member.id == hit.node.id));
        if r.language == "python"
            && hit.node.file_path != r.file_path
            && r.reference_kind == "calls"
            && !r.reference_name.contains(['.', ':'])
            && self.python_locally_bound(&r.reference_name, r)?
            && !self.python_fixture_reachable(&hit.node, &r.file_path)
        {
            return Ok(false);
        }
        if !grounded_js
            && is_js_family(&r.language)
            && hit.node.file_path != r.file_path
            && r.reference_kind == "calls"
            && !r.reference_name.contains(['.', ':'])
            && (self.is_locally_bound_js_name(&r.reference_name, &r.file_path, Some(r.line))?
                || self.js_source_local_binding(r))
        {
            return Ok(false);
        }
        if !grounded_js
            && hit.node.name == r.reference_name
            && self.outside_js_local(&hit.node, r)?
        {
            return Ok(false);
        }
        if r.reference_kind == "calls" && r.language == "c" && hit.node.kind == "method" {
            return Ok(false);
        }
        if r.reference_kind == "calls"
            && r.language == "r"
            && matches!(hit.node.kind.as_str(), "variable" | "constant")
            && [
                "c",
                "t",
                "q",
                "df",
                "dt",
                "data",
                "list",
                "length",
                "names",
                "max",
                "min",
                "sum",
                "mean",
                "range",
                "rev",
                "sort",
                "order",
                "rep",
                "seq",
                "cat",
                "print",
                "paste",
                "paste0",
                "format",
                "levels",
                "factor",
                "matrix",
                "vector",
                "table",
                "scale",
                "sample",
                "exp",
                "log",
                "abs",
                "all",
                "any",
                "which",
                "nchar",
                "summary",
                "file",
                "dir",
                "identity",
                "unique",
                "nrow",
                "ncol",
                "rownames",
                "colnames",
                "array",
                "character",
                "numeric",
                "integer",
                "logical",
                "mode",
                "class",
                "body",
                "args",
                "environment",
                "search",
                "diff",
                "round",
                "sign",
                "trunc",
                "var",
                "sd",
                "median",
                "quantile",
                "weights",
            ]
            .contains(&r.reference_name.as_str())
        {
            let function = self
                .read_file(&hit.node.file_path)
                .and_then(|s| {
                    s.get((hit.node.start_line - 1).max(0) as usize)
                        .map(|l| re!(r"(?:<<?-|=)\s*(?:function\b|\\\s*\()").is_match(l))
                })
                .unwrap_or(false);
            if !function {
                return Ok(false);
            }
        }
        if hit.node.id == r.from_node_id && r.reference_kind == "calls" {
            if matches!(
                hit.node.kind.as_str(),
                "variable" | "constant" | "field" | "property"
            ) {
                return Ok(false);
            }
            if !self.same_owner_receiver_proven(&hit.node, r) && self.collapsed_non_recursion(r)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// bareCallReceiver: the text a call recorded by its bare name is written
    /// on, read from the call site (`this.container.classList` for
    /// `this.container.classList.toggle()`); None for a call written bare or
    /// not found.
    fn bare_call_receiver(&mut self, r: &ResolveRefIn) -> Res<Option<(Rc<SourceFile>, String)>> {
        if !re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let whole = lines
            .iter()
            .skip((r.line - 1).max(0) as usize)
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let col = super::names::js_unit_to_byte(&whole, r.column.max(0) as usize).min(whole.len());
        let text = &whole[col..];
        let pat = Self::cached_regex(&format!(
            r"(?:^|[^\w$])({})\s*(?:<[^<>()]*>|\[[^\[\]]*\])?\s*[({{]",
            regex::escape(&r.reference_name)
        ))?;
        let Some(found) = pat.captures(text).and_then(|m| m.get(1)) else {
            return Ok(None);
        };
        let before = text[..found.start()].trim_end();
        let head = before
            .strip_suffix('.')
            .map(|s| s.trim_end_matches('?').trim_end().to_string());
        Ok(head.map(|h| (lines, h)))
    }

    /// isPythonSelfCall: a Python call recorded by its bare name but written
    /// on the instance, `self.get_ip(request)`, which a same-named import must
    /// not claim.
    pub(super) fn is_python_self_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "python" || r.reference_kind != "calls" {
            return Ok(false);
        }
        Ok(self.bare_call_receiver(r)?.is_some_and(|(_, head)| is_self_receiver(&head)))
    }

    /// The innermost class around a call site.
    fn python_enclosing_class(&mut self, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        Ok(self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| n.kind == "class" && n.start_line <= r.line && n.end_line >= r.line)
            .max_by_key(|n| n.start_line)
            .cloned())
    }

    /// Whether a class binds `name` as an attribute: an assignment or import
    /// in its body (`get_ip = staticmethod(get_ip)`, `from m import get_ip`)
    /// or an assignment to `self.get_ip` anywhere in it. A local of the same
    /// name inside a method does not count.
    fn python_class_rebinds(&mut self, cls: &KNode, name: &str) -> Res<bool> {
        let key = (cls.id.clone(), name.to_string());
        if let Some(&hit) = self.python_rebinds.get(&key) {
            return Ok(hit);
        }
        let hit = self.python_class_rebinds_uncached(cls, name)?;
        self.python_rebinds.insert(key, hit);
        Ok(hit)
    }

    fn python_class_rebinds_uncached(&mut self, cls: &KNode, name: &str) -> Res<bool> {
        let Some(lines) = self.read_file(&cls.file_path) else {
            return Ok(false);
        };
        let from = (cls.start_line - 1).max(0) as usize;
        let to = (cls.end_line.max(0) as usize).min(lines.len());
        let lines = &lines[from.min(to)..to];
        let assign = r"\s*(?::[^=]*)?=(?:[^=]|$)";
        let on_self = Self::cached_regex(&format!(r"^\s*self\s*\.\s*{}{assign}", regex::escape(name)))?;
        if lines.iter().any(|l| on_self.is_match(l)) {
            return Ok(true);
        }
        let Some(body) = python_class_body(lines, &cls.name) else {
            return Ok(false);
        };
        let bare = Self::cached_regex(&format!(r"^\s*{}{assign}", regex::escape(name)))?;
        let indent = |l: &str| l.len() - l.trim_start().len();
        let code = |l: &str| l.split('#').next().unwrap_or("").trim().to_string();
        let Some(body_indent) = lines[body..]
            .iter()
            .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .map(|l| indent(l))
        else {
            return Ok(false);
        };
        let mut at = body;
        while at < lines.len() {
            let line = &lines[at];
            at += 1;
            if indent(line) != body_indent || line.trim().is_empty() {
                continue;
            }
            if bare.is_match(line) {
                return Ok(true);
            }
            let stmt = code(line);
            if !(stmt.starts_with("import ") || stmt.starts_with("from ")) {
                continue;
            }
            // `from m import (a,  # note\n b)` runs on to its closing
            // parenthesis; each line's comment ends at its own line.
            let mut text = stmt;
            while text.contains('(') && !text.contains(')') && at < lines.len() {
                text.push(' ');
                text.push_str(&code(&lines[at]));
                at += 1;
            }
            if python_import_binds(&text, name) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A class and its in-repo ancestors, nearest first.
    fn python_class_hierarchy(&mut self, cls: Arc<KNode>) -> Res<Rc<Vec<Arc<KNode>>>> {
        if let Some(hit) = self.python_hierarchies.get(&cls.id) {
            return Ok(hit.clone());
        }
        let key = cls.id.clone();
        let mut queue = VecDeque::from([(cls, 0)]);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        while let Some((cls, depth)) = queue.pop_front() {
            if depth > 5 || !seen.insert(cls.id.clone()) {
                continue;
            }
            let parents = self.supertype_nodes(&cls.id)?;
            queue.extend(parents.into_iter().filter(|n| n.language == "python").map(|n| (n, depth + 1)));
            out.push(cls);
        }
        let out = Rc::new(out);
        self.python_hierarchies.insert(key, out.clone());
        Ok(out)
    }

    /// Whether `self.<name>()` must not resolve through a same-named import:
    /// it is written inside a class, and neither that class nor an in-repo
    /// ancestor binds the name as an attribute (which may hold the import).
    pub(super) fn python_self_skips_import(&mut self, r: &ResolveRefIn) -> Res<bool> {
        let Some(cls) = self.python_enclosing_class(r)? else {
            return Ok(false);
        };
        for cls in self.python_class_hierarchy(cls)?.iter() {
            if self.python_class_rebinds(cls, &r.reference_name)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The member `self.<name>()` reaches, written on plain `self`: the
    /// nearest method or nested class of that name along the class around
    /// the call and a chain of single in-repo bases. None when a class on
    /// the way binds the name as an attribute, or lists other than one base,
    /// where only the full MRO could tell.
    pub(super) fn python_self_method(&mut self, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        if !self.bare_call_receiver(r)?.is_some_and(|(_, head)| re!(r"(?:^|[^\w$.])self$").is_match(&head)) {
            return Ok(None);
        }
        let Some(mut cls) = self.python_enclosing_class(r)? else {
            return Ok(None);
        };
        let name = r.reference_name.as_str();
        for _ in 0..6 {
            if self.python_class_rebinds(&cls, name)? {
                return Ok(None);
            }
            let members = self.nodes_in_file(&cls.file_path)?;
            if let Some(m) = members.iter().find(|m| {
                (m.kind == "method" || m.kind == "class")
                    && m.name == name
                    && m.qualified_name.rsplit_once("::").is_some_and(|(owner, _)| owner == cls.qualified_name)
            }) {
                return Ok(Some(m.clone()));
            }
            let Some(lines) = self.read_file(&cls.file_path) else {
                return Ok(None);
            };
            let from = (cls.start_line - 1).max(0) as usize;
            let to = (cls.end_line.max(0) as usize).min(lines.len());
            if python_base_count(&lines[from.min(to)..to], &cls.name) != 1 {
                return Ok(None);
            }
            let parents: Vec<_> = self
                .supertype_nodes(&cls.id)?
                .into_iter()
                .filter(|n| n.language == "python" && n.kind == "class")
                .collect();
            let [parent] = parents.as_slice() else {
                return Ok(None);
            };
            cls = parent.clone();
        }
        Ok(None)
    }

    fn collapsed_non_recursion(&mut self, r: &ResolveRefIn) -> Res<bool> {
        let Some((lines, head)) = self.bare_call_receiver(r)? else {
            return Ok(false);
        };
        let head = head.as_str();
        if is_self_receiver(head) {
            return Ok(false);
        }
        // Same-type field recursion (`this.next.visit()` on Node.next: Node).
        if is_js_family(&r.language) {
            if let Some(chain) =
                re!(r"(?:^|[^\w$.#])(?:this|super)\s*\??\.\s*#?([\w$]+)$").captures(head)
            {
                let field = &chain[1];
                let Some(caller) = self.node_by_id(&r.from_node_id)? else {
                    return Ok(true);
                };
                let owner = caller
                    .qualified_name
                    .rsplit_once("::")
                    .map(|(o, _)| o.rsplit("::").next().unwrap_or(o))
                    .unwrap_or("");
                if let Some(cls) = self
                    .nodes_in_file(&r.file_path)?
                    .iter()
                    .find(|n| {
                        n.kind == "class"
                            && n.name == owner
                            && n.start_line <= r.line
                            && n.end_line >= r.line
                    })
                    .cloned()
                {
                    let body = lines
                        .iter()
                        .skip((cls.start_line - 1).max(0) as usize)
                        .take((cls.end_line - cls.start_line + 1).max(0) as usize)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    let declared = Self::cached_regex(&format!(
                        r"(?:^|[\s(,])#?{}\s*[?!]?\s*:\s*([A-Za-z_$][\w$]*)",
                        regex::escape(field)
                    ))?
                    .captures(&body)
                    .map(|m| m[1].to_string());
                    let assigned = Self::cached_regex(&format!(
                        r"\bthis\.{}\s*=\s*new\s+([A-Za-z_$][\w$]*)",
                        regex::escape(field)
                    ))?
                    .captures(&body)
                    .map(|m| m[1].to_string());
                    if declared.or(assigned).as_deref() == Some(owner) {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    pub(super) fn other_supertype_named(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let name = r
            .reference_name
            .rsplit(['.', ':'])
            .next()
            .unwrap_or(&r.reference_name);
        let source = self.read_file(&r.file_path);
        let qualifier = source
            .as_ref()
            .and_then(|s| s.get((r.line - 1).max(0) as usize))
            .and_then(|line| {
                let col =
                    super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len());
                Self::cached_regex(&format!(r"^\s*((?:[\w$]+\.)+){}\b", regex::escape(name)))
                    .ok()?
                    .captures(&line[col..])
                    .map(|m| m[1].trim_end_matches('.').to_string())
            })
            .unwrap_or_default();
        let mut pool = Vec::new();
        for n in self.nodes_by_name(name)?.iter() {
            if n.id != r.from_node_id
                && is_supertype_target(n)
                && same_language_family(&n.language, &r.language)
                && self.language_type_visible(n, r)?
            {
                if !qualifier.is_empty() {
                    let package = self
                        .read_file(&n.file_path)
                        .map(|s| {
                            re!(r"(?m)^\s*package\s+([\w.]+)\s*;?\s*$")
                                .captures_iter(s.text())
                                .map(|m| m[1].to_string())
                                .collect::<Vec<_>>()
                                .join(".")
                        })
                        .unwrap_or_default();
                    let owners = n
                        .qualified_name
                        .rsplit_once("::")
                        .map(|(o, _)| o.replace("::", "."))
                        .unwrap_or_default();
                    let owner = if package.is_empty() || owners.starts_with(&package) {
                        owners
                    } else if owners.is_empty() {
                        package
                    } else {
                        format!("{package}.{owners}")
                    };
                    if owner != qualifier && !owner.ends_with(&format!(".{qualifier}")) {
                        continue;
                    }
                }
                pool.push(n.clone());
            }
        }
        if pool.len() > 1 {
            let same: Vec<_> = pool
                .iter()
                .filter(|n| n.file_path == r.file_path)
                .cloned()
                .collect();
            if !same.is_empty() {
                pool = same;
            }
        }
        if pool.len() > 1 && r.reference_kind == "implements" {
            let interfaces: Vec<_> = pool
                .iter()
                .filter(|n| matches!(n.kind.as_str(), "interface" | "protocol" | "trait"))
                .cloned()
                .collect();
            if !interfaces.is_empty() {
                pool = interfaces;
            }
        }
        Ok((pool.len() == 1).then(|| KCand {
            node: pool[0].clone(),
            confidence: 0.8,
            resolved_by: "qualified-name",
        }))
    }
}

impl KernelResolver {
    /// A self-named call may recurse through a nested value when source
    /// proves that value has the caller's owner type.
    pub(super) fn same_owner_receiver_proven(&mut self, method: &KNode, r: &ResolveRefIn) -> bool {
        if r.language == "csharp" {
            return self
                .csharp_extension_recursion_proven(method, r)
                .unwrap_or(false);
        }
        let owner = method
            .qualified_name
            .rsplit_once("::")
            .map(|(p, _)| p.rsplit("::").next().unwrap_or(p))
            .unwrap_or("");
        if owner.is_empty() {
            return false;
        }
        let Some(source) = self.read_file(&r.file_path) else {
            return false;
        };
        let Some(line) = source.get((r.line - 1).max(0) as usize) else {
            return false;
        };
        if is_js_family(&r.language) {
            let Some(tree) = self.parsed_tree(&source, r) else {
                return false;
            };
            let point = tree_sitter::Point::new(
                (r.line - 1).max(0) as usize,
                super::names::js_unit_to_byte(line, r.column.max(0) as usize),
            );
            let mut queue = vec![tree.root_node()];
            while let Some(node) = queue.pop() {
                if node.start_position() > point || node.end_position() < point {
                    continue;
                }
                if node.kind() == "call_expression" {
                    if let Some(member) = node.child_by_field_name("function") {
                        if member.kind() == "member_expression"
                            && member.child_by_field_name("property").is_some_and(|p| {
                                source.text()[p.start_byte()..p.end_byte()] == method.name
                            })
                        {
                            if let Some(mut receiver) = member.child_by_field_name("object") {
                                while receiver.kind() == "parenthesized_expression" {
                                    let Some(inner) = receiver.named_child(0) else {
                                        break;
                                    };
                                    receiver = inner;
                                }
                                if receiver.kind() == "as_expression" {
                                    if let Some(ty) = receiver
                                        .named_child(receiver.named_child_count().saturating_sub(1))
                                    {
                                        let text = &source.text()[ty.start_byte()..ty.end_byte()];
                                        if text.split('<').next().unwrap_or(text).trim() == owner {
                                            return true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                queue.extend(super::iteration::named_children(node));
            }
            return false;
        }
        if r.language != "rust" {
            return false;
        }
        let Ok(member) = Self::cached_regex(&format!(
            r"([A-Za-z_]\w*)(?:\[[^\]]+\])?\.{}\s*\(",
            regex::escape(&method.name)
        )) else {
            return false;
        };
        let Some(call) = member.captures(line) else {
            return false;
        };
        let receiver = &call[1];
        let Ok(pattern) = Self::cached_regex(&format!(
            r"\b{}::([A-Za-z_]\w*)\s*(?:\([^)]*\b{}\b|\{{[^}}]*\b{}\b)",
            regex::escape(owner),
            regex::escape(receiver),
            regex::escape(receiver)
        )) else {
            return false;
        };
        let Some(pattern) = pattern.captures(line) else {
            return false;
        };
        let variant = &pattern[1];
        let Some(tree) = self.parsed_tree(&source, r) else {
            return false;
        };
        let mut queue = vec![tree.root_node()];
        while let Some(node) = queue.pop() {
            if node.kind() == "enum_item"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|n| &source.text()[n.start_byte()..n.end_byte()] == owner)
            {
                let mut variants = super::iteration::named_children(node);
                while let Some(v) = variants.pop() {
                    if v.kind() == "enum_variant"
                        && v.child_by_field_name("name").is_some_and(|n| {
                            source.text()[n.start_byte()..n.end_byte()] == *variant
                        })
                    {
                        let text = &source.text()[v.start_byte()..v.end_byte()];
                        let Ok(same) = Self::cached_regex(&format!(
                            r"^(?:Box|Vec)\s*<\s*{}\s*>$",
                            regex::escape(owner)
                        )) else {
                            return false;
                        };
                        if text.contains('{') {
                            let Ok(field) = Self::cached_regex(&format!(
                                r"\b{}\s*:\s*(?:Box\s*<\s*{}\s*>|{})",
                                regex::escape(receiver),
                                regex::escape(owner),
                                regex::escape(owner)
                            )) else {
                                return false;
                            };
                            return field.is_match(text);
                        }
                        let Ok(bindings) = Self::cached_regex(&format!(
                            r"\b{}::{}\s*\(([^)]*)\)",
                            regex::escape(owner),
                            regex::escape(variant)
                        )) else {
                            return false;
                        };
                        let Some(bindings) = bindings.captures(line) else {
                            return false;
                        };
                        let bindings = split_top_level(&bindings[1]);
                        if bindings.iter().any(|b| b.contains("..")) {
                            return false;
                        }
                        let Ok(binding) = Self::cached_regex(&format!(
                            r"^(?:ref\s+)?(?:mut\s+)?{}$",
                            regex::escape(receiver)
                        )) else {
                            return false;
                        };
                        let Some(index) = bindings.iter().position(|b| binding.is_match(b.trim()))
                        else {
                            return false;
                        };
                        let Some((_, fields)) = text.split_once('(') else {
                            return false;
                        };
                        let Some((fields, _)) = fields.rsplit_once(')') else {
                            return false;
                        };
                        let fields = split_top_level(fields);
                        return fields.get(index).is_some_and(|field| same.is_match(field));
                    }
                    variants.extend(super::iteration::named_children(v));
                }
            }
            queue.extend(super::iteration::named_children(node));
        }
        false
    }
}

fn cpp_scalar_type_shape(raw: &str) -> Option<bool> {
    let raw = raw.split('=').next()?.trim();
    if raw.contains('&') && !re!(r"\bconst\b").is_match(raw) {
        return None;
    }
    if re!(r"^(?:const\s+)?std::string\s*(?:const\s*)?[&]{0,2}(?:\s+[A-Za-z_]\w*)?$").is_match(raw)
    {
        return Some(true);
    }
    if re!(r"^(?:const\s+)?(?:int|bool|short|long(?:\s+long)?|unsigned(?:\s+(?:int|long|short))?)\s*(?:const\s*)?&?(?:\s+[A-Za-z_]\w*)?$").is_match(raw) { return Some(false); }
    None
}

fn cpp_parameter_type_key(raw: &str) -> String {
    let raw = raw.split('=').next().unwrap_or(raw).trim();
    re!(r"\s+[A-Za-z_]\w*$")
        .replace(raw, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn cpp_visible_binding_type(
    parent: tree_sitter::Node,
    _at: tree_sitter::Node,
    name: &str,
    source: &SourceFile,
    point: usize,
) -> Option<Option<String>> {
    let text = |n: tree_sitter::Node| &source.text()[n.start_byte()..n.end_byte()];
    if parent.kind() == "lambda_expression" {
        let mut queue = super::iteration::named_children(parent)
            .into_iter()
            .filter(|n| n.kind() == "lambda_capture_specifier")
            .collect::<Vec<_>>();
        while let Some(capture) = queue.pop() {
            if capture.kind() == "lambda_capture_initializer"
                && capture
                    .child_by_field_name("left")
                    .is_some_and(|left| text(left) == name && left.end_byte() <= point)
            {
                return Some(None);
            }
            queue.extend(super::iteration::named_children(capture));
        }
    }
    let mut declarations = Vec::new();
    if parent.kind() == "compound_statement" {
        declarations.extend(
            super::iteration::named_children(parent)
                .into_iter()
                .rev()
                .filter(|n| n.kind() == "declaration" && n.start_byte() <= point),
        );
    } else if matches!(parent.kind(), "function_definition" | "lambda_expression") {
        let mut queue = super::iteration::named_children(parent);
        while let Some(node) = queue.pop() {
            if matches!(
                node.kind(),
                "compound_statement" | "lambda_capture_specifier"
            ) {
                continue;
            }
            if matches!(
                node.kind(),
                "parameter_declaration" | "optional_parameter_declaration"
            ) {
                declarations.push(node);
            } else {
                queue.extend(super::iteration::named_children(node));
            }
        }
    }
    if matches!(
        parent.kind(),
        "for_statement"
            | "for_range_loop"
            | "if_statement"
            | "switch_statement"
            | "while_statement"
            | "catch_clause"
    ) {
        if parent.kind() == "for_range_loop" {
            declarations.push(parent);
        }
        let mut queue = ["initializer", "condition", "parameters"]
            .into_iter()
            .filter_map(|field| parent.child_by_field_name(field))
            .filter(|n| n.start_byte() <= point)
            .collect::<Vec<_>>();
        while let Some(node) = queue.pop() {
            if matches!(node.kind(), "compound_statement" | "lambda_expression") {
                continue;
            }
            if matches!(
                node.kind(),
                "declaration" | "parameter_declaration" | "optional_parameter_declaration"
            ) {
                declarations.push(node);
            } else {
                queue.extend(super::iteration::named_children(node));
            }
        }
    }
    for declaration in declarations {
        let mut cursor = declaration.walk();
        let mut declarators = Vec::new();
        if cursor.goto_first_child() {
            loop {
                if cursor.field_name() == Some("declarator") {
                    declarators.push(cursor.node());
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        for declarator in declarators {
            if declarator.kind() == "structured_binding_declarator"
                && super::iteration::named_children(declarator)
                    .iter()
                    .any(|n| text(*n) == name && n.end_byte() <= point)
            {
                return Some(None);
            }
            let mut binder = declarator;
            let mut scalar = true;
            let mut pointers = 0;
            let mut non_pointer_compound = false;
            while !matches!(binder.kind(), "identifier" | "field_identifier") {
                if binder.kind() == "structured_binding_declarator"
                    && super::iteration::named_children(binder)
                        .iter()
                        .any(|n| text(*n) == name && n.end_byte() <= point)
                {
                    return Some(None);
                }
                pointers += usize::from(binder.kind() == "pointer_declarator");
                non_pointer_compound |=
                    matches!(binder.kind(), "array_declarator" | "function_declarator");
                scalar &= !matches!(
                    binder.kind(),
                    "pointer_declarator" | "array_declarator" | "function_declarator"
                );
                let Some(inner) = binder
                    .child_by_field_name("declarator")
                    .or_else(|| binder.named_child(0))
                else {
                    break;
                };
                binder = inner;
            }
            if text(binder) != name || binder.end_byte() > point {
                continue;
            }
            if !scalar {
                let const_char = pointers == 1
                    && !non_pointer_compound
                    && declaration
                        .child_by_field_name("type")
                        .is_some_and(|ty| text(ty) == "char")
                    && super::iteration::named_children(declaration)
                        .iter()
                        .any(|n| n.kind() == "type_qualifier" && text(*n) == "const");
                return Some(const_char.then(|| "const char*".to_string()));
            }
            return Some(
                declaration
                    .child_by_field_name("type")
                    .map(|ty| text(ty).to_string()),
            );
        }
    }
    None
}

fn cpp_scalar_binding(
    parent: tree_sitter::Node,
    at: tree_sitter::Node,
    name: &str,
    source: &SourceFile,
    point: usize,
) -> Option<Option<bool>> {
    cpp_visible_binding_type(parent, at, name, source, point).map(|raw| {
        raw.as_deref().and_then(|raw| {
            if raw == "const char*" {
                Some(true)
            } else {
                cpp_scalar_type_shape(raw)
            }
        })
    })
}

/// The bases a Python class header lists, keyword arguments
/// (`metaclass=…`) and `**` unpacking left out; 0 when the header is not
/// found, has no parentheses, or unpacks a sequence of bases (`*bases`).
fn python_base_count(lines: &[String], name: &str) -> usize {
    let text = lines.iter().take(20).map(String::as_str).collect::<Vec<_>>().join("\n");
    let Ok(head) = KernelResolver::cached_regex(&format!(r"(?m)^\s*class\s+{}\s*\(", regex::escape(name))) else {
        return 0;
    };
    let Some(m) = head.find(&text) else {
        return 0;
    };
    let (mut depth, mut count, mut part, mut unpacked) = (0usize, 0usize, String::new(), false);
    let flush = |part: &mut String, count: &mut usize, unpacked: &mut bool| {
        let p = part.trim();
        if p.starts_with('*') && !p.starts_with("**") {
            *unpacked = true;
        } else if !p.is_empty() && !p.starts_with('*') && !re!(r"^[A-Za-z_]\w*\s*=").is_match(p) {
            *count += 1;
        }
        part.clear();
    };
    for c in text[m.end()..].chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => {
                flush(&mut part, &mut count, &mut unpacked);
                return if unpacked { 0 } else { count };
            }
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                flush(&mut part, &mut count, &mut unpacked);
                continue;
            }
            _ => {}
        }
        part.push(c);
    }
    0
}

/// The index of the first line of a Python class's body, past a header
/// that may run over several lines (`class A(\n    Base,\n):`); None when
/// the header is not found or the body shares its line.
fn python_class_body(lines: &[String], name: &str) -> Option<usize> {
    let head = KernelResolver::cached_regex(&format!(r"^\s*class\s+{}\b", regex::escape(name))).ok()?;
    let at = lines.iter().position(|l| head.is_match(l))?;
    let mut depth = 0usize;
    for (i, line) in lines.iter().enumerate().skip(at) {
        for (j, c) in line.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                '#' => break,
                ':' if depth == 0 => {
                    let rest = line[j + 1..].trim();
                    return (rest.is_empty() || rest.starts_with('#')).then_some(i + 1);
                }
                _ => {}
            }
        }
    }
    None
}

/// Whether an `import` or `from … import` statement binds `name`.
fn python_import_binds(stmt: &str, name: &str) -> bool {
    let (names, from) = if let Some(rest) = stmt.strip_prefix("from ") {
        match rest.split_once(" import ") {
            Some((_, names)) => (names, true),
            None => return false,
        }
    } else if let Some(rest) = stmt.strip_prefix("import ") {
        (rest, false)
    } else {
        return false;
    };
    let names = names.replace(['(', ')', '\\'], " ");
    names.split(',').any(|part| {
        let mut words = part.split_whitespace();
        let Some(first) = words.next() else {
            return false;
        };
        let bound = match (words.next(), words.next()) {
            (Some("as"), Some(alias)) => alias,
            _ if from => first,
            _ => first.split('.').next().unwrap_or(first),
        };
        bound == name
    })
}

/// `this.m()` / `self.m()` / `super.m()` / `super().m()`: a call on the caller's own object.
fn is_self_receiver(head: &str) -> bool {
    re!(r"(?:^|[^\w$.])(?:this|self|super|Self)$|(?:^|[^\w$.])super\s*\([^()]*\)$").is_match(head)
}
