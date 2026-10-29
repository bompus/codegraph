//! Types written on Rust bindings and signatures, read from their lexical grammar scopes.
use super::super::rust_modules::rust_paths::*;
use super::super::*;

#[derive(Clone, Debug)]
pub(super) struct Written {
    pub(super) text: String,
    pub(super) file: String,
    pub(super) at: usize,
}
impl Written {
    fn with(&self, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..self.clone()
        }
    }
}
fn split_types(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if i == 0 || text.as_bytes()[i - 1] != b'-' => depth -= 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if c == separator && depth == 0 {
            parts.push(text[start..i].trim().into());
            start = i + c.len_utf8();
        }
    }
    if start < text.len() {
        parts.push(text[start..].trim().into());
    }
    parts
}
fn type_head(text: &str) -> (String, Vec<String>, bool) {
    let mut text = text.trim();
    let by_ref = text.starts_with('&');
    while let Some(rest) = text.strip_prefix('&') {
        text = rest.trim_start();
        if text.starts_with('\'') {
            text = text
                .split_once(char::is_whitespace)
                .map(|(_, s)| s)
                .unwrap_or("");
        }
        text = text.strip_prefix("mut ").unwrap_or(text).trim_start();
    }
    let text = text.strip_prefix("mut ").unwrap_or(text);
    if let Some(open) = text.find('<') {
        let end = text.rfind('>').unwrap_or(text.len());
        return (
            text[..open].trim().trim_end_matches("::").into(),
            split_types(&text[open + 1..end], ','),
            by_ref,
        );
    }
    (text.into(), Vec::new(), by_ref)
}
fn type_leaf(text: &str) -> String {
    let (head, _, _) = type_head(text);
    head.rsplit("::").next().unwrap_or(&head).to_string()
}
fn generic_substitute(text: &str, params: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut start = 0;
    for m in re!(r"\b[A-Za-z_]\w*\b").find_iter(text) {
        out.push_str(&text[start..m.start()]);
        out.push_str(
            params
                .get(m.as_str())
                .map(String::as_str)
                .unwrap_or(m.as_str()),
        );
        start = m.end();
    }
    out.push_str(&text[start..]);
    out
}
fn rust_std_inherent(ty: &str, name: &str) -> bool {
    let names=match ty {
        "Arc" => "new new_cyclic new_uninit new_uninit_slice new_zeroed pin try_new try_unwrap into_inner unwrap_or_clone into_raw as_ptr from_raw increment_strong_count decrement_strong_count downgrade weak_count strong_count get_mut make_mut ptr_eq downcast assume_init",
        "Rc" => "new new_cyclic new_uninit new_uninit_slice new_zeroed pin try_new try_unwrap into_inner unwrap_or_clone into_raw as_ptr from_raw increment_strong_count decrement_strong_count downgrade weak_count strong_count get_mut make_mut ptr_eq downcast assume_init",
        "Weak" => "new upgrade strong_count weak_count as_ptr into_raw from_raw ptr_eq",
        "Box" => "new new_uninit new_uninit_slice new_zeroed pin try_new from_raw into_raw from_non_null into_non_null leak into_pin into_boxed_slice into_inner as_ptr as_mut_ptr downcast assume_init write",
        "NonNull" => "new new_unchecked dangling as_ptr as_ref as_mut cast add sub offset byte_add byte_sub byte_offset offset_from read read_unaligned read_volatile write write_bytes write_unaligned write_volatile replace swap copy_to copy_from copy_to_nonoverlapping copy_from_nonoverlapping drop_in_place from_ref from_mut slice_from_raw_parts len addr with_addr map_addr align_offset is_aligned",
        "Pin" => "new new_unchecked into_inner into_inner_unchecked static_ref static_mut as_ref as_mut as_deref_mut set get_ref get_mut get_unchecked_mut into_ref map_unchecked map_unchecked_mut",
        "Cell" => "new set swap replace into_inner get update as_ptr get_mut from_mut take",
        "RefCell" => "new into_inner replace replace_with swap borrow try_borrow borrow_mut try_borrow_mut as_ptr get_mut take",
        "Mutex" => "new lock try_lock is_poisoned clear_poison into_inner get_mut",
        "RwLock" => "new read try_read write try_write is_poisoned clear_poison into_inner get_mut",
        "Vec" => "new with_capacity from_raw_parts capacity reserve reserve_exact try_reserve try_reserve_exact shrink_to_fit shrink_to into_boxed_slice truncate as_slice as_mut_slice as_ptr as_mut_ptr set_len swap_remove insert remove retain retain_mut dedup_by_key dedup_by dedup push pop pop_if append drain clear len is_empty split_off resize_with resize leak spare_capacity_mut extend_from_slice extend_from_within splice extract_if into_flattened",
        "String" => "new with_capacity from_utf8 from_utf8_lossy from_utf8_unchecked from_utf16 from_utf16_lossy from_raw_parts into_bytes as_str as_mut_str push_str extend_from_within capacity reserve reserve_exact try_reserve try_reserve_exact shrink_to_fit shrink_to push as_bytes truncate pop remove retain insert insert_str as_mut_vec len is_empty split_off clear drain replace_range into_boxed_str leak",
        "Option" => "is_some is_some_and is_none is_none_or as_ref as_mut as_pin_ref as_pin_mut as_slice as_mut_slice expect unwrap unwrap_or unwrap_or_else unwrap_or_default unwrap_unchecked map inspect map_or map_or_else ok_or ok_or_else as_deref as_deref_mut iter iter_mut and and_then filter or or_else xor insert get_or_insert get_or_insert_default get_or_insert_with take take_if replace zip unzip copied cloned transpose flatten",
        "Result" => "is_ok is_ok_and is_err is_err_and ok err as_ref as_mut map map_or map_or_else map_err inspect inspect_err as_deref as_deref_mut iter iter_mut expect unwrap unwrap_or_default expect_err unwrap_err into_ok into_err and and_then or or_else unwrap_or unwrap_or_else unwrap_unchecked unwrap_err_unchecked copied cloned transpose flatten",
        _ => "",
    };
    names.split_whitespace().any(|s| s == name)
}

fn own_wrapper_method(ty: &str, name: &str) -> bool {
    let names=match ty {
        "Pin" => "as_ref as_mut as_deref_mut set get_ref get_mut get_unchecked_mut into_ref map_unchecked map_unchecked_mut deref deref_mut fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string",
        "Box" => "clone clone_from as_ref borrow deref fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string as_mut borrow_mut deref_mut",
        "Rc" => "clone clone_from as_ref borrow deref fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string",
        "Arc" => "clone clone_from as_ref borrow deref fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string",
        "ManuallyDrop" => "clone clone_from as_ref borrow deref fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string deref_mut",
        "Cow" => "clone clone_from as_ref borrow deref fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string into_owned to_mut is_borrowed is_owned",
        "MutexGuard" => "deref deref_mut fmt",
        "RwLockReadGuard" => "deref fmt",
        "RwLockWriteGuard" => "deref deref_mut fmt",
        "Ref" => "deref fmt",
        "RefMut" => "deref deref_mut fmt",
        _ => "",
    };
    names.split_whitespace().any(|s| s == name)
}

fn rust_named_call<'a>(
    at: tree_sitter::Node<'a>,
    text: &str,
    reference: &str,
) -> Option<tree_sitter::Node<'a>> {
    let leaf = reference.rsplit('.').next()?.rsplit("::").next()?;
    let mut cursor = at;
    loop {
        if cursor.kind() == "call_expression" {
            if let Some(mut f) = cursor.child_by_field_name("function") {
                if f.kind() == "generic_function" {
                    f = f.child_by_field_name("function").unwrap_or(f);
                }
                let name = f
                    .child_by_field_name("field")
                    .or_else(|| f.child_by_field_name("name"))
                    .unwrap_or(f);
                if rust_text(name, text) == leaf || f.kind() == "parenthesized_expression" {
                    return Some(cursor);
                }
            }
        }
        cursor = cursor.parent()?;
    }
}

impl KernelResolver {
    pub(in super::super) fn rust_generic_traits(
        &mut self,
        name: &str,
        file: &str,
        at: usize,
    ) -> Option<Vec<String>> {
        let (source, tree) = self.rust_tree_for(file)?;
        let text = source.text();
        let mut cursor = tree
            .root_node()
            .named_descendant_for_byte_range(at.min(text.len()), at.min(text.len()))?;
        loop {
            if matches!(
                cursor.kind(),
                "function_item"
                    | "function_signature_item"
                    | "impl_item"
                    | "trait_item"
                    | "struct_item"
            ) {
                let params = cursor.child_by_field_name("type_parameters");
                if let Some(param) = params.and_then(|p| {
                    rust_children(p).into_iter().find(|n| {
                        n.child_by_field_name("name")
                            .is_some_and(|n| rust_text(n, text) == name)
                            || rust_text(*n, text) == name
                    })
                }) {
                    let mut bounds = param
                        .child_by_field_name("bounds")
                        .map(|n| {
                            rust_text(n, text)
                                .trim_start_matches(':')
                                .trim()
                                .to_string()
                        })
                        .unwrap_or_default();
                    for wh in rust_children(cursor)
                        .into_iter()
                        .filter(|n| n.kind() == "where_clause")
                    {
                        for predicate in rust_children(wh) {
                            if predicate
                                .child_by_field_name("left")
                                .is_some_and(|n| rust_text(n, text) == name)
                                || rust_text(predicate, text)
                                    .trim_start()
                                    .starts_with(&format!("{name}:"))
                            {
                                if let Some((_, b)) = rust_text(predicate, text).split_once(':') {
                                    if !bounds.is_empty() {
                                        bounds.push('+');
                                    }
                                    bounds.push_str(b);
                                }
                            }
                        }
                    }
                    return Some(
                        split_types(&bounds, '+')
                            .into_iter()
                            .filter(|s| {
                                !s.starts_with('\'') && !s.starts_with('?') && !s.is_empty()
                            })
                            .collect(),
                    );
                }
            }
            cursor = cursor.parent()?;
        }
    }
    pub(super) fn rust_type_decls(&mut self, w: &Written) -> Res<Vec<Arc<KNode>>> {
        let (head, _, _) = type_head(&w.text);
        let leaf = head.rsplit("::").next().unwrap_or(&head);
        let mut path: Vec<String> = head.split("::").map(str::to_string).collect();
        let mut from = if let Some((s, t)) = self.rust_tree_for(&w.file) {
            let at = t
                .root_node()
                .named_descendant_for_byte_range(w.at.min(s.text().len()), w.at.min(s.text().len()))
                .unwrap_or(t.root_node());
            RustModule {
                file: w.file.clone(),
                inline: rust_inline_at(at, s.text()),
            }
        } else {
            RustModule {
                file: w.file.clone(),
                inline: Vec::new(),
            }
        };
        for (local, p, m) in self.rust_visible_uses(&w.file, w.at) {
            if Some(&local) == path.first() {
                let mut full = p;
                full.extend_from_slice(&path[1..]);
                path = full;
                from = m;
                break;
            }
        }
        if let Some(items) = self.rust_path_items(&path, &from, false, &mut HashSet::new())? {
            let types: Vec<_> = items
                .into_iter()
                .filter(|n| {
                    matches!(
                        n.kind.as_str(),
                        "struct" | "enum" | "union" | "trait" | "type_alias"
                    )
                })
                .collect();
            if !types.is_empty() {
                return Ok(types);
            }
        }
        let facts = self.rust_use_facts(&w.file).unwrap_or_default();
        if path.len() > 1
            && (matches!(path[0].as_str(), "std" | "core" | "alloc")
                || facts.external.contains(&path[0]))
        {
            return Ok(Vec::new());
        }
        let candidates: Vec<_> = self
            .nodes_by_name(leaf)?
            .iter()
            .filter(|n| {
                n.language == "rust"
                    && matches!(
                        n.kind.as_str(),
                        "struct" | "enum" | "union" | "trait" | "type_alias"
                    )
                    && !n.qualified_name.ends_with(&format!("::{}", n.name))
            })
            .cloned()
            .collect();
        let local: Vec<_> = candidates
            .iter()
            .filter(|n| n.file_path == w.file)
            .cloned()
            .collect();
        if !local.is_empty() {
            return Ok(local);
        }
        let mut imported = Vec::new();
        for (local, p, m) in self.rust_visible_uses(&w.file, w.at) {
            if local == "*" {
                if let Some(module) = self.rust_walk_module(&p, &m, 0) {
                    imported.extend(self.rust_items_in_module(
                        &module,
                        leaf,
                        true,
                        &mut HashSet::new(),
                    )?);
                }
            }
        }
        if !imported.is_empty() {
            return Ok(imported);
        }
        if facts.external.contains(leaf) && !facts.bound.contains(leaf) {
            return Ok(Vec::new());
        }
        Ok(candidates)
    }
    fn rust_declaration_node<'a>(
        &mut self,
        n: &KNode,
        t: &'a tree_sitter::Tree,
        s: &str,
        kinds: &[&str],
    ) -> Option<tree_sitter::Node<'a>> {
        rust_ancestor(rust_at(t, s, n.start_line, n.start_column), kinds)
    }
    fn rust_self_written(&mut self, r: &ResolveRefIn) -> Option<Written> {
        let (source, tree) = self.rust_tree_for(&r.file_path)?;
        let text = source.text();
        let at = rust_at(&tree, text, r.line, r.column);
        let block = rust_ancestor(at, &["impl_item", "trait_item"])?;
        let ty = block
            .child_by_field_name("type")
            .or_else(|| block.child_by_field_name("name"))?;
        let base = rust_text(ty, text).to_string();
        let mut written = Written {
            text: base.clone(),
            file: r.file_path.clone(),
            at: at.start_byte(),
        };
        if let Some(function) = rust_ancestor(at, &["function_item"]) {
            if let Some(params) = function.child_by_field_name("parameters") {
                for param in rust_children(params) {
                    if param.kind() == "parameter"
                        && param
                            .child_by_field_name("pattern")
                            .is_some_and(|p| rust_text(p, text) == "self")
                    {
                        if let Some(ty) = param.child_by_field_name("type") {
                            written.text = rust_text(ty, text).replace("Self", &base);
                        }
                    }
                }
            }
        }
        Some(written)
    }
    fn rust_pattern_type(
        &mut self,
        pattern: tree_sitter::Node<'_>,
        name: &str,
        value: Option<Written>,
        r: &ResolveRefIn,
        text: &str,
        depth: usize,
    ) -> Res<Option<Option<Written>>> {
        if depth > 24 {
            return Ok(None);
        }
        match pattern.kind() {
            "identifier" | "self" => Ok((rust_text(pattern, text) == name).then_some(value)),
            "mut_pattern" | "ref_pattern" | "reference_pattern" => {
                for c in rust_children(pattern) {
                    if let Some(w) =
                        self.rust_pattern_type(c, name, value.clone(), r, text, depth + 1)?
                    {
                        return Ok(Some(w));
                    }
                }
                Ok(None)
            }
            "tuple_pattern" | "slice_pattern" => {
                for (i, c) in rust_children(pattern).into_iter().enumerate() {
                    let ty = value.as_ref().and_then(|v| {
                        let t = v.text.trim().trim_start_matches('&').trim();
                        let t = t.strip_prefix('(')?.strip_suffix(')')?;
                        split_types(t, ',').get(i).map(|s| v.with(s.clone()))
                    });
                    if let Some(found) = self.rust_pattern_type(c, name, ty, r, text, depth + 1)? {
                        return Ok(Some(found));
                    }
                }
                Ok(None)
            }
            "tuple_struct_pattern" | "struct_pattern" => {
                let ty = pattern
                    .child_by_field_name("type")
                    .or_else(|| pattern.child_by_field_name("path"))
                    .or_else(|| pattern.named_child(0));
                let path = ty.map(|t| rust_text(t, text)).unwrap_or("");
                let leaf = path.rsplit("::").next().unwrap_or(path);
                let fields: Vec<_> = (0..pattern.child_count()).filter_map(|i| pattern.child(i))
                    .filter(|n| (n.is_named() || n.kind() == "_") && Some(n.id()) != ty.map(|n| n.id()))
                    .collect();
                for (i, c) in fields.into_iter().enumerate() {
                    let field_name = c.child_by_field_name("name").map(|n| rust_text(n, text));
                    let field_pattern = c.child_by_field_name("pattern").unwrap_or(c);
                    let ty = if matches!(leaf, "Some" | "Ok" | "Err") {
                        value.as_ref().and_then(|v| {
                            type_head(&v.text)
                                .1
                                .get(if leaf == "Err" { 1 } else { 0 })
                                .map(|a| v.with(a.clone()))
                        })
                    } else {
                        self.rust_pattern_field(path, field_name, i, value.as_ref(), r, depth + 1)?
                    };
                    if let Some(found) =
                        self.rust_pattern_type(field_pattern, name, ty, r, text, depth + 1)?
                    {
                        return Ok(Some(found));
                    }
                    if field_name == Some(name) {
                        return Ok(Some(value.clone()));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
    fn rust_pattern_field(
        &mut self,
        path: &str,
        field: Option<&str>,
        index: usize,
        value: Option<&Written>,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Written>> {
        let leaf = path.rsplit("::").next().unwrap_or(path);
        let nodes: Vec<_> = self
            .nodes_by_name(leaf)?
            .iter()
            .filter(|n| n.language == "rust" && matches!(n.kind.as_str(), "enum_member" | "struct"))
            .cloned()
            .collect();
        for node in nodes {
            if path.contains("::") && !node.qualified_name.ends_with(path) {
                continue;
            }
            let Some((s, t)) = self.rust_tree_for(&node.file_path) else {
                continue;
            };
            let Some(decl) =
                self.rust_declaration_node(&node, &t, s.text(), &["enum_variant", "struct_item"])
            else {
                continue;
            };
            let Some(body) = decl.child_by_field_name("body") else {
                continue;
            };
            let fields = rust_children(body);
            let ty = if let Some(name) = field {
                fields
                    .iter()
                    .find(|n| {
                        n.child_by_field_name("name")
                            .is_some_and(|n| rust_text(n, s.text()) == name)
                    })
                    .and_then(|n| n.child_by_field_name("type"))
            } else {
                fields.get(index).map(|n| {
                    if n.kind() == "field_declaration" {
                        n.child_by_field_name("type").unwrap_or(*n)
                    } else {
                        *n
                    }
                })
            };
            if let Some(ty) = ty {
                let w = Written {
                    text: rust_text(ty, s.text()).into(),
                    file: node.file_path.clone(),
                    at: decl.start_byte(),
                };
                let params = if decl.kind() == "enum_variant" {
                    rust_ancestor(decl, &["enum_item"]).unwrap_or(decl)
                } else {
                    decl
                };
                return self.rust_instantiate_type(w, value, params, s.text(), r, depth);
            }
        }
        Ok(None)
    }
    fn rust_instantiate_type(
        &mut self,
        w: Written,
        value: Option<&Written>,
        decl: tree_sitter::Node<'_>,
        text: &str,
        _r: &ResolveRefIn,
        _depth: usize,
    ) -> Res<Option<Written>> {
        let mut params = HashMap::new();
        if let (Some(value), Some(generics)) = (value, decl.child_by_field_name("type_parameters"))
        {
            let args = type_head(&value.text).1;
            for (p, a) in rust_children(generics).into_iter().zip(args) {
                let name = p.child_by_field_name("name").unwrap_or(p);
                params.insert(rust_text(name, text).into(), a);
            }
        }
        Ok(Some(w.with(generic_substitute(&w.text, &params))))
    }
    fn rust_binding_type(
        &mut self,
        name: &str,
        at: usize,
        file: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Option<Written>>> {
        if depth > 24 {
            return Ok(Some(None));
        }
        if name == "self" {
            return Ok(Some(self.rust_self_written(r)));
        }
        let Some((source, tree)) = self.rust_tree_for(file) else {
            return Ok(None);
        };
        let text = source.text();
        let mut scope = tree
            .root_node()
            .named_descendant_for_byte_range(at.min(text.len()), at.min(text.len()))
            .unwrap_or(tree.root_node());
        loop {
            if matches!(scope.kind(), "block" | "source_file") {
                for decl in rust_children(scope)
                    .into_iter()
                    .rev()
                    .filter(|n| n.kind() == "let_declaration" && n.end_byte() <= at)
                {
                    let Some(pattern) = decl.child_by_field_name("pattern") else {
                        continue;
                    };
                    let value = if let Some(ty) = decl.child_by_field_name("type") {
                        Some(Written {
                            text: rust_text(ty, text).into(),
                            file: file.into(),
                            at: decl.start_byte(),
                        })
                    } else if let Some(v) = decl.child_by_field_name("value") {
                        self.rust_expr_type(v, text, file, r, depth + 1)?
                    } else {
                        None
                    };
                    if let Some(w) =
                        self.rust_pattern_type(pattern, name, value, r, text, depth + 1)?
                    {
                        return Ok(Some(w));
                    }
                }
            }
            if matches!(
                scope.kind(),
                "function_item" | "function_signature_item" | "closure_expression"
            ) {
                if let Some(params) = scope.child_by_field_name("parameters") {
                    for param in rust_children(params) {
                        let pattern = param.child_by_field_name("pattern").unwrap_or(param);
                        let ty = param.child_by_field_name("type").map(|t| Written {
                            text: rust_text(t, text).into(),
                            file: file.into(),
                            at: scope.start_byte(),
                        });
                        if let Some(w) =
                            self.rust_pattern_type(pattern, name, ty, r, text, depth + 1)?
                        {
                            return Ok(Some(w));
                        }
                    }
                }
            }
            if scope.kind() == "for_expression" {
                if let Some(pattern) = scope.child_by_field_name("pattern") {
                    let ty = if let Some(value) = scope.child_by_field_name("value") {
                        self.rust_iter_type(value, text, file, r, depth + 1)?
                    } else {
                        None
                    };
                    if let Some(w) =
                        self.rust_pattern_type(pattern, name, ty, r, text, depth + 1)?
                    {
                        return Ok(Some(w));
                    }
                }
            }
            if scope.kind() == "match_arm" {
                let scrutinee = scope
                    .parent()
                    .and_then(|n| n.parent())
                    .and_then(|n| n.child_by_field_name("value"));
                let value = if let Some(v) = scrutinee {
                    self.rust_expr_type(v, text, file, r, depth + 1)?
                } else {
                    None
                };
                if let Some(pattern) = scope.child_by_field_name("pattern") {
                    for p in if pattern.kind() == "match_pattern" {
                        rust_children(pattern)
                    } else {
                        vec![pattern]
                    } {
                        if let Some(w) =
                            self.rust_pattern_type(p, name, value.clone(), r, text, depth + 1)?
                        {
                            return Ok(Some(w));
                        }
                    }
                }
            }
            if matches!(scope.kind(), "if_expression" | "while_expression") {
                if let Some(condition) = scope.child_by_field_name("condition") {
                    if condition.kind() == "let_condition" {
                        if let Some(pattern) = condition.child_by_field_name("pattern") {
                            let ty = if let Some(v) = condition.child_by_field_name("value") {
                                self.rust_expr_type(v, text, file, r, depth + 1)?
                            } else {
                                None
                            };
                            if let Some(w) =
                                self.rust_pattern_type(pattern, name, ty, r, text, depth + 1)?
                            {
                                return Ok(Some(w));
                            }
                        }
                    }
                }
            }
            // A named nested function does not capture its enclosing function's locals.
            if scope.kind() == "function_item" {
                break;
            }
            let Some(p) = scope.parent() else { break };
            scope = p;
        }
        Ok(None)
    }
    fn rust_expr_type(
        &mut self,
        n: tree_sitter::Node<'_>,
        text: &str,
        file: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Written>> {
        if depth > 24 {
            return Ok(None);
        }
        let written = |text: String| Written {
            text,
            file: file.into(),
            at: n.start_byte(),
        };
        match n.kind() {
            "identifier" | "self" => {
                let name = rust_text(n, text);
                if let Some(binding) =
                    self.rust_binding_type(name, n.start_byte(), file, r, depth + 1)?
                {
                    return Ok(binding);
                }
                if name.chars().next().is_some_and(char::is_uppercase) {
                    return Ok(Some(written(name.into())));
                }
                Ok(None)
            }
            "reference_expression"
            | "parenthesized_expression"
            | "try_expression"
            | "await_expression" => {
                let child = n
                    .child_by_field_name("value")
                    .or_else(|| rust_children(n).first().copied());
                let Some(child) = child else { return Ok(None) };
                let Some(mut w) = self.rust_expr_type(child, text, file, r, depth + 1)? else {
                    return Ok(None);
                };
                if n.kind() == "try_expression" {
                    if let Some(arg) = type_head(&w.text).1.first() {
                        w = w.with(arg.clone());
                    } else {
                        return Ok(None);
                    }
                }
                if n.kind() == "await_expression" {
                    if let Some(x) = w.text.strip_prefix("$future$") {
                        w = w.with(x.to_string());
                    } else if let Some(m) = re!(r"\bOutput\s*=\s*(.+)>\s*$").captures(&w.text) {
                        w = w.with(m[1].to_string());
                    } else {
                        return Ok(None);
                    }
                }
                if n.kind() == "reference_expression" {
                    w = w.with(format!("&{}", w.text));
                }
                Ok(Some(w))
            }
            "scoped_identifier" => Ok(Some(written(rust_text(n, text).into()))),
            "struct_expression" => {
                let Some(ty) = n
                    .child_by_field_name("name")
                    .or_else(|| n.child_by_field_name("type"))
                else {
                    return Ok(None);
                };
                let raw = rust_text(ty, text);
                let parts: Vec<_> = raw.split("::").collect();
                if parts.len() > 1
                    && self
                        .nodes_by_name(parts[parts.len() - 1])?
                        .iter()
                        .any(|n| n.kind == "enum_member" && n.qualified_name == raw)
                {
                    return Ok(Some(written(parts[..parts.len() - 1].join("::"))));
                }
                Ok(Some(written(raw.into())))
            }
            "index_expression" => {
                let Some(base) = n.child_by_field_name("value").or_else(|| n.named_child(0)) else { return Ok(None) };
                let Some(w) = self.rust_expr_type(base, text, file, r, depth + 1)? else { return Ok(None) };
                Ok(self.rust_item_type(&w, None))
            }
            "field_expression" => {
                let Some(base) = n.child_by_field_name("value") else {
                    return Ok(None);
                };
                let Some(field) = n.child_by_field_name("field") else {
                    return Ok(None);
                };
                let Some(w) = self.rust_expr_type(base, text, file, r, depth + 1)? else {
                    return Ok(None);
                };
                self.rust_field_written(&w, rust_text(field, text), r, depth + 1)
            }
            "call_expression" => {
                let Some(mut function) = n.child_by_field_name("function") else {
                    return Ok(None);
                };
                let args = rust_children(n.child_by_field_name("arguments").unwrap_or(n));
                let mut generics = Vec::new();
                if function.kind() == "generic_function" {
                    generics = function
                        .child_by_field_name("type_arguments")
                        .map(|a| {
                            rust_children(a)
                                .iter()
                                .map(|n| rust_text(*n, text).into())
                                .collect()
                        })
                        .unwrap_or_default();
                    function = function.child_by_field_name("function").unwrap_or(function);
                }
                if function.kind() == "field_expression" {
                    let Some(base) = function.child_by_field_name("value") else {
                        return Ok(None);
                    };
                    let Some(name) = function.child_by_field_name("field") else {
                        return Ok(None);
                    };
                    let Some(w) = self.rust_expr_type(base, text, file, r, depth + 1)? else {
                        return Ok(None);
                    };
                    let name = rust_text(name, text);
                    let (head, type_args, _) = type_head(&w.text);
                    let leaf = head.rsplit("::").next().unwrap_or(&head);
                    if name == "project" && leaf == "Pin" {
                        if let Some(inner) = type_args.first() {
                            return Ok(Some(w.with(format!("$project${}", type_head(inner).0))));
                        }
                    }
                    if matches!(name, "as_mut" | "as_ref") && !self.rust_project_named(leaf)? {
                        return Ok(Some(if matches!(leaf, "Box" | "Rc" | "Arc" | "NonNull") {
                            w.with(type_args.first().cloned().unwrap_or_default())
                        } else {
                            w
                        }));
                    }
                    if matches!(name, "unwrap" | "expect") && matches!(leaf, "Option" | "Result") {
                        return Ok(type_args.first().map(|x| w.with(x.clone())));
                    }
                    let target = self.rust_method_on(&w, name, r, depth + 1)?;
                    if let Some(target) = target {
                        return self.rust_return_written(
                            &target,
                            Some(&w),
                            &generics,
                            &args,
                            (text, file),
                            (r, depth + 1),
                        );
                    }
                    if name == "clone" {
                        for declaration in self.rust_type_decls(&w)? {
                            let Some((source, tree)) = self.rust_tree_for(&declaration.file_path) else { continue };
                            let Some(mut node) = self.rust_declaration_node(&declaration, &tree, source.text(), &["struct_item", "enum_item"]) else { continue };
                            while let Some(attribute) = node.prev_named_sibling().filter(|n| n.kind() == "attribute_item") {
                                if re!(r"\bderive\s*\([^)]*\bClone\b").is_match(rust_text(attribute, source.text())) { return Ok(Some(w)); }
                                node = attribute;
                            }
                        }
                    }
                    return Ok(None);
                }
                let raw = rust_text(function, text);
                let path: Vec<String> = raw.split("::").map(str::to_string).collect();
                if path.len() > 1 {
                    let owner = path[..path.len() - 1].join("::");
                    let name = path.last().unwrap();
                    if matches!(owner.as_str(), "Box" | "Rc" | "Arc" | "Pin")
                        && matches!(name.as_str(), "new" | "pin")
                    {
                        if let Some(arg) = args.first() {
                            if let Some(inner) =
                                self.rust_expr_type(*arg, text, file, r, depth + 1)?
                            {
                                return Ok(Some(written(format!("{owner}<{}>", inner.text))));
                            }
                        }
                    }
                    let owner = written(owner);
                    if matches!(name.as_str(), "new" | "default" | "with_capacity")
                        && matches!(
                            owner.text.as_str(),
                            "String" | "Vec" | "HashMap" | "BTreeMap"
                        )
                    {
                        return Ok(Some(owner));
                    }

                    if let Some(target) = self.rust_method_on(&owner, name, r, depth + 1)? {
                        return self.rust_return_written(
                            &target,
                            Some(&owner),
                            &generics,
                            &args,
                            (text, file),
                            (r, depth + 1),
                        );
                    }
                    if self
                        .nodes_by_name(name)?
                        .iter()
                        .any(|n| n.kind == "enum_member" && n.qualified_name == raw)
                    {
                        return Ok(Some(owner));
                    }
                }
                if path.len() == 1 {
                    let last = path[0].as_str();
                    // A tuple struct (`Wrapper(x)`) or a `Some`/`Ok`/`Err`
                    // variant makes a value of its own type; a SCREAMING name
                    // is a const's call on nothing the index types.
                    if last.starts_with(|c: char| c.is_ascii_uppercase()) {
                        if last.len() > 1
                            && last.chars().all(|c| {
                                c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
                            })
                        {
                            return Ok(None);
                        }
                        let ty = match last {
                            "Some" => "Option<_>",
                            "Ok" | "Err" => "Result<_, _>",
                            _ => last,
                        };
                        return Ok(Some(written(ty.to_string())));
                    }
                    let mut scope = n;
                    loop {
                        let local = rust_children(scope).into_iter().find(|item|
                            item.kind() == "function_item" && item.child_by_field_name("name")
                                .is_some_and(|name| rust_text(name, text) == raw));
                        if let Some(item) = local {
                            let nodes = self.nodes_by_name(raw)?;
                            if let Some(target) = nodes.iter().find(|node| node.file_path == file
                                && node.start_line == item.start_position().row as i64 + 1) {
                                return self.rust_return_written(target, None, &generics, &args,
                                    (text, file), (r, depth + 1));
                            }
                        }
                        let Some(parent) = scope.parent() else { break };
                        scope = parent;
                    }
                }
                let mut full = path.clone();
                let mut from = self.rust_module_at(
                    file,
                    n.start_position().row as i64 + 1,
                    n.start_position().column as i64,
                );
                for (local, p, m) in self.rust_visible_uses(file, n.start_byte()) {
                    if Some(&local) == full.first() {
                        let mut expanded = p;
                        expanded.extend_from_slice(&full[1..]);
                        full = expanded;
                        from = m;
                        break;
                    }
                }
                let functions = self
                    .rust_path_items(&full, &from, false, &mut HashSet::new())?
                    .unwrap_or_default();
                if let Some(target) = functions.into_iter().find(|n| n.kind == "function") {
                    return self.rust_return_written(
                        &target,
                        None,
                        &generics,
                        &args,
                        (text, file),
                        (r, depth + 1),
                    );
                }
                Ok(None)
            }
            "tuple_expression" => {
                let mut parts = Vec::new();
                for child in rust_children(n) {
                    let Some(w) = self.rust_expr_type(child, text, file, r, depth + 1)? else {
                        return Ok(None);
                    };
                    parts.push(w.text);
                }
                Ok(Some(written(format!("({})", parts.join(",")))))
            }
            _ => Ok(None),
        }
    }
    fn rust_field_written(
        &mut self,
        w: &Written,
        field: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Written>> {
        if let Ok(index) = field.parse::<usize>() {
            let text = w.text.trim().trim_start_matches('&').trim();
            if let Some(tuple) = text.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
                return Ok(split_types(tuple, ',')
                    .get(index)
                    .map(|s| w.with(s.clone())));
            }
        }
        let projected = w.text.starts_with("$project$");
        let mut w = w.with(w.text.trim_start_matches("$project$"));
        let (head, args, _) = type_head(&w.text);
        if matches!(head.as_str(), "Pin" | "Box" | "Rc" | "Arc") {
            if let Some(arg) = args.first() {
                w = w.with(arg.clone());
            }
        }
        let decls = self.rust_type_decls(&w)?;
        if projected && decls.is_empty() {
            if let Some((source, tree)) = self.rust_tree_for(&w.file) {
                let mut stack = vec![tree.root_node()];
                while let Some(node) = stack.pop() {
                    if node.kind() == "struct_item"
                        && node
                            .child_by_field_name("name")
                            .is_some_and(|n| rust_text(n, source.text()) == type_leaf(&w.text))
                    {
                        if let Some(body) = node.child_by_field_name("body") {
                            for member in rust_children(body) {
                                if member
                                    .child_by_field_name("name")
                                    .is_some_and(|n| rust_text(n, source.text()) == field)
                                {
                                    if let Some(ty) = member.child_by_field_name("type") {
                                        let pinned = member.prev_named_sibling().is_some_and(|n| {
                                            n.kind() == "attribute_item"
                                                && rust_text(n, source.text()).contains("pin")
                                        });
                                        let text = if pinned {
                                            format!("Pin<&mut {}>", rust_text(ty, source.text()))
                                        } else {
                                            rust_text(ty, source.text()).into()
                                        };
                                        return Ok(Some(Written {
                                            text,
                                            file: w.file.clone(),
                                            at: node.start_byte(),
                                        }));
                                    }
                                }
                            }
                        }
                    }
                    if !matches!(node.kind(), "function_item" | "function_signature_item") {
                        stack.extend(rust_children(node));
                    }
                }
            }
        }
        for decl in decls {
            let Some((source, tree)) = self.rust_tree_for(&decl.file_path) else {
                continue;
            };
            let Some(node) = self.rust_declaration_node(
                &decl,
                &tree,
                source.text(),
                &["struct_item", "enum_item"],
            ) else {
                continue;
            };
            let Some(body) = node.child_by_field_name("body") else {
                continue;
            };
            // `self.N` — a tuple struct's fields are unnamed; the Nth
            // positional member's type is the field's type.
            if let Ok(index) = field.parse::<usize>() {
                if body.kind() == "ordered_field_declaration_list" {
                    let members: Vec<_> = rust_children(body)
                        .into_iter()
                        .filter(|n| !matches!(n.kind(), "visibility_modifier" | "attribute_item"))
                        .collect();
                    if let Some(ty) = members.get(index).map(|n| {
                        n.child_by_field_name("type").unwrap_or(*n)
                    }) {
                        let field_type = rust_text(ty, source.text()).into();
                        return self.rust_instantiate_type(
                            Written {
                                text: field_type,
                                file: decl.file_path.clone(),
                                at: node.start_byte(),
                            },
                            Some(&w),
                            node,
                            source.text(),
                            r,
                            depth,
                        );
                    }
                }
            }
            let field_node = rust_children(body).into_iter().find(|n| {
                n.child_by_field_name("name")
                    .is_some_and(|n| rust_text(n, source.text()) == field)
            });
            if let Some(ty) = field_node.and_then(|n| n.child_by_field_name("type")) {
                let pinned = projected
                    && field_node
                        .and_then(|n| n.prev_named_sibling())
                        .is_some_and(|n| {
                            n.kind() == "attribute_item"
                                && rust_text(n, source.text()).contains("pin")
                        });
                let field_type = if pinned {
                    format!("Pin<&mut {}>", rust_text(ty, source.text()))
                } else {
                    rust_text(ty, source.text()).into()
                };
                return self.rust_instantiate_type(
                    Written {
                        text: field_type,
                        file: decl.file_path.clone(),
                        at: node.start_byte(),
                    },
                    Some(&w),
                    node,
                    source.text(),
                    r,
                    depth,
                );
            }
        }
        Ok(None)
    }
    fn rust_iter_type(
        &mut self,
        value: tree_sitter::Node<'_>,
        text: &str,
        file: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Written>> {
        if depth > 24 {
            return Ok(None);
        }
        if value.kind() == "call_expression" {
            if let Some(function) = value
                .child_by_field_name("function")
                .filter(|n| n.kind() == "field_expression")
            {
                if let (Some(base), Some(name)) = (
                    function.child_by_field_name("value"),
                    function.child_by_field_name("field"),
                ) {
                    let name = rust_text(name, text);
                    if name == "enumerate" {
                        return Ok(self
                            .rust_iter_type(base, text, file, r, depth + 1)?
                            .map(|w| w.with(format!("(usize,{})", w.text))));
                    }
                    if matches!(
                        name,
                        "iter" | "iter_mut" | "into_iter" | "keys" | "values" | "values_mut"
                    ) {
                        if let Some(w) = self.rust_expr_type(base, text, file, r, depth + 1)? {
                            return Ok(self.rust_item_type(&w, Some(name)));
                        }
                    }
                }
            }
        }
        let Some(w) = self.rust_expr_type(value, text, file, r, depth + 1)? else {
            return Ok(None);
        };
        Ok(self.rust_item_type(&w, None))
    }
    fn rust_item_type(&self, w: &Written, projection: Option<&str>) -> Option<Written> {
        let (head, args, _) = type_head(&w.text);
        if let Some(inner) = head.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            return Some(w.with(inner.split(';').next()?.trim()));
        }
        let leaf = head.rsplit("::").next()?;
        if let Some(m) = re!(r"\bItem\s*=\s*(.+)>\s*$").captures(&w.text) {
            return Some(w.with(m[1].to_string()));
        }
        if matches!(leaf, "HashMap" | "BTreeMap") {
            return match projection {
                Some("keys") => args.first().map(|s| w.with(s.clone())),
                Some("values" | "values_mut") => args.get(1).map(|s| w.with(s.clone())),
                _ => Some(w.with(format!("({},{})", args.first()?, args.get(1)?))),
            };
        }
        if matches!(leaf, "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "Option") {
            return args.first().map(|s| w.with(s.clone()));
        }
        None
    }
    fn rust_return_written(
        &mut self,
        target: &KNode,
        receiver: Option<&Written>,
        generics: &[String],
        args: &[tree_sitter::Node<'_>],
        call_source: (&str, &str),
        site: (&ResolveRefIn, usize),
    ) -> Res<Option<Written>> {
        let (r, depth) = site;
        let (call_text, call_file) = call_source;
        let Some((source, tree)) = self.rust_tree_for(&target.file_path) else {
            return Ok(None);
        };
        let text = source.text();
        let Some(function) = self.rust_declaration_node(
            target,
            &tree,
            text,
            &["function_item", "function_signature_item"],
        ) else {
            return Ok(None);
        };
        let Some(ret) = function.child_by_field_name("return_type") else {
            return Ok(None);
        };
        let mut subs = HashMap::new();
        if let Some(receiver) = receiver {
            let (head, type_args, _) = type_head(&receiver.text);
            subs.insert("Self".into(), head);
            if let Some(imp) = rust_ancestor(function, &["impl_item"]) {
                if let Some(params) = imp.child_by_field_name("type_parameters") {
                    for (p, a) in rust_children(params).into_iter().zip(type_args) {
                        let name = p.child_by_field_name("name").unwrap_or(p);
                        subs.insert(rust_text(name, text).into(), a);
                    }
                }
            }
        }
        if let Some(params) = function.child_by_field_name("type_parameters") {
            for (p, a) in rust_children(params).into_iter().zip(generics) {
                let name = p.child_by_field_name("name").unwrap_or(p);
                subs.insert(rust_text(name, text).into(), a.clone());
            }
        }
        if let Some(params) = function.child_by_field_name("parameters") {
            let params: Vec<_> = rust_children(params)
                .into_iter()
                .filter(|n| {
                    n.kind() == "parameter"
                        && !n
                            .child_by_field_name("pattern")
                            .is_some_and(|p| rust_text(p, text) == "self")
                })
                .collect();
            for (param, arg) in params.into_iter().zip(args) {
                if let Some(ty) = param.child_by_field_name("type") {
                    let ty = rust_text(ty, text);
                    if re!(r"^[A-Za-z_]\w*$").is_match(ty) && !subs.contains_key(ty) {
                        if let Some(actual) =
                            self.rust_expr_type(*arg, call_text, call_file, r, depth + 1)?
                        {
                            subs.insert(ty.into(), actual.text);
                        }
                    }
                }
            }
        }
        let out = generic_substitute(rust_text(ret, text), &subs);
        let out = if target.is_async
            || rust_children(function)
                .iter()
                .any(|n| n.kind() == "function_modifiers" && rust_text(*n, text).contains("async"))
        {
            format!("$future${out}")
        } else {
            out
        };
        // A substituted Self or generic is named at the calling site, while
        // an un-substituted signature names types in its declaration module.
        let substituted = subs.keys().any(|key| rust_text(ret, text).trim() == key);
        Ok(Some(Written {
            text: out,
            file: if substituted {
                call_file.into()
            } else {
                target.file_path.clone()
            },
            at: if substituted {
                args.first().map(|n| n.start_byte()).unwrap_or(0)
            } else {
                function.start_byte()
            },
        }))
    }
    pub(super) fn rust_project_named(&mut self, name: &str) -> Res<bool> {
        Ok(self.nodes_by_name(name)?.iter().any(|n| {
            n.language == "rust"
                && matches!(
                    n.kind.as_str(),
                    "struct" | "enum" | "union" | "type_alias" | "trait"
                )
        }))
    }
    fn rust_methods_named(&mut self, owner: &str, name: &str) -> Res<Vec<Arc<KNode>>> {
        let q = format!("{owner}::{name}");
        Ok(self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| {
                n.language == "rust"
                    && matches!(n.kind.as_str(), "method" | "function")
                    && (n.qualified_name == q || n.qualified_name.ends_with(&format!("::{q}")))
            })
            .cloned()
            .collect())
    }
    fn rust_trait_methods(
        &mut self,
        w: &Written,
        traits: &[String],
        name: &str,
        depth: usize,
    ) -> Res<Vec<Arc<KNode>>> {
        if depth > 12 {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for tr in traits {
            let tw = w.with(tr.clone());
            let decls = self.rust_type_decls(&tw)?;
            for decl in decls.into_iter().filter(|n| n.kind == "trait") {
                let methods = self.rust_methods_named(&decl.name, name)?;
                found.extend(methods.into_iter().filter(|n| {
                    n.file_path == decl.file_path
                        && decl.start_line <= n.start_line
                        && decl.end_line >= n.start_line
                }));
                let Some((source, tree)) = self.rust_tree_for(&decl.file_path) else {
                    continue;
                };
                if let Some(item) =
                    self.rust_declaration_node(&decl, &tree, source.text(), &["trait_item"])
                {
                    if let Some(bounds) = item.child_by_field_name("bounds") {
                        found.extend(self.rust_trait_methods(
                            &Written {
                                text: tr.clone(),
                                file: decl.file_path.clone(),
                                at: item.start_byte(),
                            },
                            &split_types(
                                rust_text(bounds, source.text()).trim_start_matches(':'),
                                '+',
                            ),
                            name,
                            depth + 1,
                        )?);
                    }
                }
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        found.dedup_by(|a, b| a.id == b.id);
        Ok(found)
    }
    fn rust_method_on(
        &mut self,
        w: &Written,
        name: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Arc<KNode>>> {
        if depth > 24 {
            return Ok(None);
        }
        let (head, args, by_ref) = type_head(&w.text);

        let leaf = head.rsplit("::").next().unwrap_or(&head);
        if leaf == "Self" {
            if let Some(cx) = self.rust_self_context(r) {
                return self.rust_method_on(&w.with(cx.owner), name, r, depth + 1);
            }
            return Ok(None);
        }
        if leaf.starts_with('(') || leaf.starts_with('[') || leaf.starts_with("$future$") {
            return Ok(None);
        }
        if head.starts_with("impl ") || head.starts_with("dyn ") {
            let traits = split_types(
                head.trim_start_matches("impl ").trim_start_matches("dyn "),
                '+',
            );
            let methods = self.rust_trait_methods(w, &traits, name, 0)?;
            return Ok(if methods.len() == 1 {
                methods.first().cloned()
            } else {
                None
            });
        }
        if let Some(traits) = self.rust_generic_traits(leaf, &w.file, w.at) {
            let methods = self.rust_trait_methods(w, &traits, name, 0)?;
            return Ok(if methods.len() == 1 {
                methods.first().cloned()
            } else {
                None
            });
        }
        let decls = self.rust_type_decls(w)?;
        if let Some(decl) = decls.first().filter(|d| d.name != leaf) {
            let text = if args.is_empty() {
                decl.name.clone()
            } else {
                format!("{}<{}>", decl.name, args.join(","))
            };
            return self.rust_method_on(
                &Written {
                    text,
                    file: decl.file_path.clone(),
                    at: 0,
                },
                name,
                r,
                depth + 1,
            );
        }
        let is_std = decls.is_empty();
        let mut owned = self.rust_methods_named(leaf, name)?;
        if !decls.is_empty() {
            let mut retained = Vec::new();
            for method in owned {
                let Some((source, tree)) = self.rust_tree_for(&method.file_path) else {
                    continue;
                };
                let Some(function) = self.rust_declaration_node(
                    &method,
                    &tree,
                    source.text(),
                    &["function_item", "function_signature_item"],
                ) else {
                    continue;
                };
                let Some(imp) = rust_ancestor(function, &["impl_item"]) else {
                    continue;
                };
                let Some(ty) = imp.child_by_field_name("type") else {
                    continue;
                };
                let written = Written {
                    text: rust_text(ty, source.text()).into(),
                    file: method.file_path.clone(),
                    at: imp.start_byte(),
                };
                let owners = self.rust_type_decls(&written)?;
                if owners.iter().any(|n| decls.iter().any(|d| n.id == d.id)) {
                    retained.push(method);
                }
            }
            owned = retained;
        }
        let mut inherent = Vec::new();
        let mut impl_methods = Vec::new();
        for method in owned {
            let Some((source, tree)) = self.rust_tree_for(&method.file_path) else {
                continue;
            };
            let Some(function) = self.rust_declaration_node(
                &method,
                &tree,
                source.text(),
                &["function_item", "function_signature_item"],
            ) else {
                continue;
            };
            let Some(imp) = rust_ancestor(function, &["impl_item"]) else {
                continue;
            };
            let Some(ty) = imp.child_by_field_name("type") else {
                continue;
            };
            let (_, impl_args, impl_ref) = type_head(rust_text(ty, source.text()));
            if impl_ref != by_ref && is_std {
                continue;
            }
            if is_std
                && matches!(leaf, "Pin" | "Rc" | "Arc" | "Box")
                && method
                    .signature
                    .as_deref()
                    .is_some_and(|s| s.contains(&format!("self: {leaf}<")))
            {
                continue;
            }

            if !impl_args.is_empty() && !args.is_empty() {
                let generic_names: HashSet<_> = imp
                    .child_by_field_name("type_parameters")
                    .map(|p| {
                        rust_children(p)
                            .into_iter()
                            .filter_map(|n| {
                                n.child_by_field_name("name")
                                    .map(|n| rust_text(n, source.text()).to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if impl_args
                    .iter()
                    .zip(&args)
                    .any(|(a, b)| !generic_names.contains(a) && type_leaf(a) != type_leaf(b))
                {
                    continue;
                }
            }
            if let Some(tr) = imp.child_by_field_name("trait") {
                if is_std {
                    let body = function
                        .child_by_field_name("body")
                        .map(|n| rust_text(n, source.text()))
                        .unwrap_or("");
                    if body.contains(&format!("Self::{name}(self"))
                        || body.contains(&format!("self.{name}("))
                    {
                        continue;
                    }
                }
                let (_, trait_args, _) = type_head(rust_text(tr, source.text()));
                if !trait_args.is_empty() {
                    if let Some((call_source, call_tree)) = self.rust_tree_for(&r.file_path) {
                        if let Some(call) = rust_named_call(
                            rust_at(&call_tree, call_source.text(), r.line, r.column),
                            call_source.text(),
                            &r.reference_name,
                        ) {
                            if let Some(arg) = call
                                .child_by_field_name("arguments")
                                .and_then(|a| rust_children(a).first().copied())
                            {
                                if let Some(actual) = self.rust_expr_type(
                                    arg,
                                    call_source.text(),
                                    &r.file_path,
                                    r,
                                    depth + 1,
                                )? {
                                    if type_leaf(&trait_args[0]) != type_leaf(&actual.text) {
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                }
                let tr = type_leaf(rust_text(tr, source.text()));
                if !self.rust_trait_in_scope_for_type(&tr, r, &method)? {
                    continue;
                }
                impl_methods.push(method);
            } else {
                inherent.push(method);
            }
        }
        if inherent.len() == 1 {
            return Ok(inherent.pop());
        }
        if inherent.len() > 1 {
            return Ok(None);
        }
        // An explicit wrapper receiver belongs to the wrapped type before
        // methods supplied on the wrapper through an ordinary self parameter.
        if is_std && matches!(leaf, "Pin" | "Box" | "Rc" | "Arc" | "NonNull") {
            let inner = args.first().map(|a| w.with(a.clone()));
            if let Some(inner) = &inner {
                let inner_name = type_leaf(&inner.text);
                let mut through = Vec::new();
                for method in self.rust_methods_named(&inner_name, name)? {
                    if method
                        .signature
                        .as_deref()
                        .is_some_and(|s| s.contains(&format!("self: {leaf}<")))
                    {
                        through.push(method);
                    }
                }
                if through.len() == 1 {
                    return Ok(through.pop());
                }
                if through.len() > 1 {
                    let current = self.rust_impl_start(&r.file_path, r.line, r.column);
                    let mut same: Vec<_> = through
                        .into_iter()
                        .filter(|n| {
                            n.file_path == r.file_path
                                && self.rust_impl_start(&n.file_path, n.start_line, n.start_column)
                                    == current
                        })
                        .collect();
                    if same.len() == 1 {
                        return Ok(same.pop());
                    }
                }
            }
            if own_wrapper_method(leaf, name) {
                return Ok(None);
            }
            if impl_methods.len() == 1 {
                return Ok(impl_methods.pop());
            }
            if let Some(inner) = inner {
                return self.rust_method_on(&inner, name, r, depth + 1);
            }
            return Ok(None);
        }
        if is_std && rust_std_inherent(leaf, name) {
            return Ok(None);
        }
        if impl_methods.len() == 1 {
            let method = impl_methods.pop().unwrap();
            if is_std
                && method.name
                    == self
                        .node_by_id(&r.from_node_id)?
                        .map(|n| n.name.clone())
                        .unwrap_or_default()
            {
                return Ok(None);
            }
            return Ok(Some(method));
        }
        if impl_methods.len() > 1 {
            let current = self.rust_impl_start(&r.file_path, r.line, r.column);
            let mut same: Vec<_> = impl_methods
                .into_iter()
                .filter(|n| {
                    n.file_path == r.file_path
                        && self.rust_impl_start(&n.file_path, n.start_line, n.start_column)
                            == current
                })
                .collect();
            if same.len() == 1 {
                return Ok(same.pop());
            }
            return Ok(None);
        }
        let mut alias_seen = HashSet::new();
        for decl in &decls {
            if !alias_seen.insert(decl.file_path.clone()) {
                continue;
            }
            let aliases: Vec<_> = self
                .nodes_in_file(&decl.file_path)?
                .iter()
                .filter(|n| n.kind == "type_alias" && n.qualified_name == n.name)
                .cloned()
                .collect();
            for alias in aliases {
                let Some((s, t)) = self.rust_tree_for(&alias.file_path) else {
                    continue;
                };
                if let Some(item) = self.rust_declaration_node(&alias, &t, s.text(), &["type_item"])
                {
                    if let Some(ty) = item.child_by_field_name("type") {
                        let (target, args_alias, _) = type_head(rust_text(ty, s.text()));
                        if type_leaf(&target) == leaf
                            && args_alias == args
                            && !self.rust_methods_named(&alias.name, name)?.is_empty()
                        {
                            return self.rust_method_on(
                                &Written {
                                    text: alias.name.clone(),
                                    file: alias.file_path.clone(),
                                    at: item.start_byte(),
                                },
                                name,
                                r,
                                depth + 1,
                            );
                        }
                    }
                }
            }
        }
        for decl in &decls {
            if decl.kind == "type_alias" {
                if let Some((s, t)) = self.rust_tree_for(&decl.file_path) {
                    if let Some(alias) =
                        self.rust_declaration_node(decl, &t, s.text(), &["type_item"])
                    {
                        if let Some(target) = alias.child_by_field_name("type") {
                            return self.rust_method_on(
                                &Written {
                                    text: rust_text(target, s.text()).into(),
                                    file: decl.file_path.clone(),
                                    at: alias.start_byte(),
                                },
                                name,
                                r,
                                depth + 1,
                            );
                        }
                    }
                }
            }
        }
        // Provided trait methods require an implementation for this type.
        let mut provided = Vec::new();
        let mut impls_seen = HashSet::new();
        for decl in &decls {
            if !impls_seen.insert(decl.file_path.clone()) {
                continue;
            }
            let Some((s, t)) = self.rust_tree_for(&decl.file_path) else {
                continue;
            };
            let mut stack = vec![t.root_node()];
            while let Some(item) = stack.pop() {
                if item.kind() == "impl_item"
                    && item
                        .child_by_field_name("type")
                        .is_some_and(|n| type_leaf(rust_text(n, s.text())) == leaf)
                {
                    if let Some(tr) = item.child_by_field_name("trait") {
                        let tr = rust_text(tr, s.text()).to_string();
                        if self.rust_trait_in_scope_for_type(&type_leaf(&tr), r, decl)? {
                            let methods = self.rust_trait_methods(
                                &Written {
                                    text: tr.clone(),
                                    file: decl.file_path.clone(),
                                    at: item.start_byte(),
                                },
                                &[tr],
                                name,
                                0,
                            )?;
                            provided.extend(methods.into_iter().filter(|m| {
                                m.signature
                                    .as_deref()
                                    .is_some_and(|s| !s.trim_end().ends_with(';'))
                            }));
                        }
                    }
                }
                if !matches!(item.kind(), "function_item" | "function_signature_item") {
                    stack.extend(rust_children(item));
                }
            }
        }
        provided.sort_by(|a, b| a.id.cmp(&b.id));
        provided.dedup_by(|a, b| a.id == b.id);
        Ok(if provided.len() == 1 {
            provided.pop()
        } else {
            None
        })
    }
    pub(super) fn rust_impl_start(&mut self, file: &str, line: i64, col: i64) -> Option<usize> {
        let (s, t) = self.rust_tree_for(file)?;
        rust_ancestor(
            rust_at(&t, s.text(), line, col),
            &["impl_item", "trait_item"],
        )
        .map(|n| n.start_byte())
    }
    fn rust_trait_in_scope_for_type(
        &mut self,
        name: &str,
        r: &ResolveRefIn,
        sample: &KNode,
    ) -> Res<bool> {
        if sample.file_path == r.file_path {
            return Ok(true);
        }
        let Some((s, t)) = self.rust_tree_for(&r.file_path) else {
            return Ok(false);
        };
        let at = rust_at(&t, s.text(), r.line, r.column).start_byte();
        for (local, path, m) in self.rust_visible_uses(&r.file_path, at) {
            if local == name {
                return Ok(true);
            }
            if local == "*" {
                if let Some(module) = self.rust_walk_module(&path, &m, 0) {
                    if !self
                        .rust_items_in_module(&module, name, true, &mut HashSet::new())?
                        .is_empty()
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(self
            .nodes_by_name(name)?
            .iter()
            .any(|n| n.kind == "trait" && n.file_path == r.file_path))
    }

    /// Some(None) stops name guessing once a grammar binding or explicit receiver owns the call.
    pub(in super::super) fn rust_written_call(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<KCand>>> {
        if r.language != "rust" || r.reference_kind != "calls" {
            return Ok(None);
        }
        // `inner().method` — the ref's position anchors on the INNER call,
        // so the source anchor below would answer for the wrong link; the
        // chain arm types each link's return by name instead.
        if call_chain_re().is_match(&r.reference_name) {
            if let Some(hit) = self.match_call_chain(r)? {
                self.mark_rust_typed(r);
                return Ok(Some(Some(hit)));
            }
        }
        let Some((source, tree)) = self.rust_tree_for(&r.file_path) else {
            return Ok(None);
        };
        let text = source.text();
        // Existing Self arms prove owner identity, variants, and associated-type paths.
        if r.reference_name.starts_with("Self::") || r.reference_name.contains("Self::") {
            return Ok(None);
        }
        let at = rust_at(&tree, text, r.line, r.column);
        let Some(call) = rust_named_call(at, text, &r.reference_name) else {
            return Ok(None);
        };
        let Some(mut function) = call.child_by_field_name("function") else {
            return Ok(None);
        };
        if function.kind() == "generic_function" {
            function = function.child_by_field_name("function").unwrap_or(function);
        }
        let mut typed = None;
        let mut method = String::new();
        if function.kind() == "field_expression" {
            let Some(base) = function.child_by_field_name("value") else {
                return Ok(None);
            };
            let Some(field) = function.child_by_field_name("field") else {
                return Ok(None);
            };
            method = rust_text(field, text).into();
            if rust_text(base, text).contains("Self::") {
                return Ok(None);
            }
            if base.kind() == "self" {
                if let Some(w) = self.rust_self_written(r) {
                    if !matches!(type_head(&w.text).0.as_str(), "Pin" | "Box" | "Rc" | "Arc") {
                        if let Some(cx) = self.rust_self_context(r) {
                            if !self.rust_outside_self(&cx, r)? {
                                let mut self_ref = r.clone();
                                self_ref.reference_name = format!("Self::{method}");
                                return self.rust_simple_self_call(&self_ref);
                            }
                        }
                    }
                }
            }
            typed = self.rust_expr_type(base, text, &r.file_path, r, 0)?;
            if typed.is_none() {
                if base.kind() == "identifier" {
                    let bound = self.rust_binding_type(
                        rust_text(base, text),
                        call.start_byte(),
                        &r.file_path,
                        r,
                        0,
                    )?;
                    if bound.is_some() {
                        return Ok(Some(None));
                    }
                    return Ok(None);
                }
                if base.kind() == "call_expression" && base.child_by_field_name("function")
                    .is_some_and(|f| matches!(f.kind(), "scoped_identifier" | "generic_function")) {
                    if let Some(hit) = self.match_call_chain(r)? {
                        self.mark_rust_typed(r);
                        return Ok(Some(Some(hit)));
                    }
                }
                return Ok(Some(None));
            }
        } else if function.kind() == "parenthesized_expression" {
            return Ok(Some(None));
        } else if function.kind() == "scoped_identifier" {
            let Some(path) = function.child_by_field_name("path") else {
                return Ok(None);
            };
            let Some(name) = function.child_by_field_name("name") else {
                return Ok(None);
            };
            method = rust_text(name, text).into();
            let owner = rust_text(path, text);
            if self.nodes_by_name(&method)?.iter().any(|n| n.kind == "enum_member"
                && n.qualified_name == format!("{owner}::{method}")) {
                return Ok(None);
            }
            if super::STD_TYPES.contains(&type_leaf(owner).as_str())
                && !self.rust_project_named(&type_leaf(owner))? && method == "from" {
                return Ok(None);
            }
            if owner == "Self" {
                typed = self.rust_self_written(r).map(|w| {
                    let (h, _, _) = type_head(&w.text);
                    w.with(h)
                });
            } else {
                let w = Written {
                    text: owner.into(),
                    file: r.file_path.clone(),
                    at: call.start_byte(),
                };
                let leaf = type_leaf(owner);
                if !self.rust_project_named(&leaf)? && rust_std_inherent(&leaf, &method) {
                    return Ok(Some(None));
                }
                if self
                    .rust_generic_traits(&leaf, &r.file_path, call.start_byte())
                    .is_some()
                    || self.rust_project_named(&leaf)?
                    || super::STD_TYPES.contains(&leaf.as_str())
                {
                    typed = Some(w);
                } else {
                    return Ok(None);
                }
            }
        }
        let Some(written) = typed else {
            return Ok(None);
        };
        let mut unread = written.clone();
        for _ in 0..8 {
            let (head, args, _) = type_head(&unread.text);
            if matches!(head.as_str(), "Box" | "Rc" | "Arc" | "Pin") {
                if let Some(arg) = args.first() {
                    unread = unread.with(arg.clone());
                    continue;
                }
            }
            break;
        }
        let (head, _, _) = type_head(&unread.text);
        if head.starts_with("dyn ") || head.starts_with("impl ") {
            let trait_type =
                unread.with(head.trim_start_matches("dyn ").trim_start_matches("impl "));
            if self.rust_type_decls(&trait_type)?.is_empty() {
                return Ok(None);
            }
        }
        // A field chain or struct literal receiver — `vis.map.get("a")`,
        // `Xyz { x: 1.0 }.to_val()` — whose type the receiver is written with
        // says what the call reaches (matchRustWrittenReceiverCall). A type no
        // declaration the index holds names — one a macro declares, like
        // tokio's `type AtomicUnsignedShort = …` inside `cfg_has_atomic_u64!`
        // — says nothing of it, unless a wrapper around it has the method
        // itself (`Arc::clone`): the name strategies decide.
        let leaf = head.rsplit("::").next().unwrap_or(&head);
        if function.kind() == "field_expression" {
            // `read.kind === 'named'`: a plain type path — not `Self`, a
            // `&[T]`, a generic parameter's bound (`R: AsyncRead`), or a type
            // std or a dependency owns (rust_written_outside).
            let named = leaf != "Self"
                && re!(r"^[A-Za-z_]\w*(?:::\s*[A-Za-z_]\w*)*$").is_match(&head)
                && self
                    .rust_generic_traits(leaf, &written.file, written.at)
                    .is_none()
                && !self.rust_written_outside(&written, leaf, &head)?;
            if named
                && !own_wrapper_method(&type_head(&written.text).0, &method)
                && !self.rust_project_named(leaf)?
                && self.rust_type_decls(&written)?.is_empty()
                && self.rust_methods_named(leaf, &method)?.is_empty()
            {
                return Ok(None);
            }
        }
        let node = match self.rust_method_on(&written, &method, r, 0)? {
            Some(node) => Some(node),
            // A provided method of a project trait the type derives counts
            // (axum-extra's #[derive(TypedPath)]).
            None if function.kind() == "field_expression" => {
                self.rust_derived_trait_method(&written, &method, r)?
            }
            None => None,
        };
        if node.is_some() {
            self.mark_rust_typed(r);
        }
        Ok(Some(node.map(|node| KCand {
            node,
            confidence: 0.9,
            resolved_by: if function.kind() == "field_expression" { "instance-method" } else { "qualified-name" },
        })))
    }

    /// The names a Rust type's `#[derive(…)]` attributes write, each by its
    /// path's last segment.
    fn rust_derived_names(&mut self, decl: &KNode) -> Res<Vec<String>> {
        let Some((source, tree)) = self.rust_tree_for(&decl.file_path) else {
            return Ok(Vec::new());
        };
        let text = source.text();
        let Some(node) = self.rust_declaration_node(
            decl,
            &tree,
            text,
            &["struct_item", "enum_item", "union_item"],
        ) else {
            return Ok(Vec::new());
        };
        let mut names = Vec::new();
        let mut attr = node.prev_named_sibling();
        while let Some(a) = attr {
            if a.kind() != "attribute_item" {
                break;
            }
            for m in re!(r"\bderive\s*\(([^()]*)\)").captures_iter(rust_text(a, text)) {
                for part in m[1].split(',') {
                    if let Some(name) = part
                        .trim()
                        .rsplit("::")
                        .next()
                        .map(str::trim)
                        .filter(|s| re!(r"^[A-Za-z_]\w*$").is_match(s))
                    {
                        names.push(name.to_string());
                    }
                }
            }
            attr = a.prev_named_sibling();
        }
        Ok(names)
    }

    /// Whether a Rust method's declaration writes a body — a provided trait
    /// method is a `function_item`; a required one a `function_signature_item`.
    fn rust_has_body(&mut self, m: &KNode) -> bool {
        let Some((source, tree)) = self.rust_tree_for(&m.file_path) else {
            return false;
        };
        self.rust_declaration_node(m, &tree, source.text(), &["function_item"])
            .is_some()
    }

    /// The method a Rust project trait that a type derives provides, for a
    /// call on a value of the type: `#[derive(TypedPath)] struct UsersShow {…}`
    /// reaches axum-extra's `TypedPath::with_query_params` through the impl
    /// the derive writes, which the index doesn't hold. None unless exactly
    /// one derived trait in scope at the call declares the method with a body.
    fn rust_derived_trait_method(
        &mut self,
        w: &Written,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        let decls = self.rust_type_decls(w)?;
        let types: Vec<_> = decls
            .into_iter()
            .filter(|n| matches!(n.kind.as_str(), "struct" | "enum" | "union"))
            .collect();
        if types.len() != 1 {
            return Ok(None);
        }
        let mut found: Vec<Arc<KNode>> = Vec::new();
        for name in self.rust_derived_names(&types[0])? {
            let traits: Vec<_> = self
                .nodes_by_name(&name)?
                .iter()
                .filter(|n| n.kind == "trait" && n.language == "rust" && n.name == name)
                .cloned()
                .collect();
            if traits.is_empty() {
                continue;
            }
            for m in self.rust_trait_methods(w, &[name], method, 0)? {
                let Some(trait_node) = self.rust_trait_of(&m)? else {
                    continue;
                };
                if self.rust_has_body(&m)
                    && self.rust_trait_in_scope(&trait_node.name, r, &m)?
                {
                    found.push(m);
                }
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        found.dedup_by(|a, b| a.id == b.id);
        let mut owners = HashSet::new();
        for m in &found {
            if let Some(t) = self.rust_trait_of(m)? {
                owners.insert(t.id.clone());
            }
        }
        Ok((owners.len() == 1).then(|| found.first().cloned()).flatten())
    }
}
