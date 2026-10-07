//! C++ aliases are followed from their declaration scope, including nested owners.
use super::*;

pub(super) struct CppAlias {
    pub raw: Option<String>,
    pub site: ResolveRefIn,
    pub pointer: bool,
    pub reference: bool,
}

pub(super) fn type_segments(raw: &str) -> Option<Vec<String>> {
    let mut text = String::new();
    let mut depth = 0usize;
    for c in raw.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => text.push(c),
            _ => {},
        }
    }
    let text = re!(r"\b(?:const|volatile|mutable|typename|template|class|struct|union|enum)\b").replace_all(&text, " ");
    let text = text.replace(['*', '&'], " ");
    let parts: Vec<String> = text.trim().trim_start_matches("::").split("::").map(|p| p.trim().to_string()).collect();
    (!parts.is_empty() && parts.iter().all(|p| re!(r"^[A-Za-z_]\w*$").is_match(p))).then_some(parts)
}

fn pointer_type(raw: &str) -> bool { indirection_type(raw, '*') }

fn reference_type(raw: &str) -> bool { indirection_type(raw, '&') }

fn indirection_type(raw: &str, operator: char) -> bool {
    let mut depth = 0usize;
    for c in raw.chars() {
        match c { '<' => depth += 1, '>' => depth = depth.saturating_sub(1), c if depth == 0 && c == operator => return true, _ => {} }
    }
    false
}

fn alias_rhs(text: &str, name: &str) -> Option<String> {
    if !text.contains(name) || (!text.contains("using") && !text.contains("typedef")) { return None; }
    let using = KernelResolver::cached_regex(&format!(r"\busing\s+{}\s*(?:\[\[[^\]]*\]\]\s*)?=\s*([^;]+)", regex::escape(name))).ok()?;
    if let Some(hit) = using.captures(text) { return Some(hit[1].trim().to_string()); }
    let typedef = re!(r"\btypedef\b([^;]+)").captures(text)?;
    let mut depth = 0usize;
    let mut first = String::new();
    for c in typedef[1].chars() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => break,
            _ => {},
        }
        first.push(c);
    }
    let declared = KernelResolver::cached_regex(&format!(r"(?s)^(.+?)\b{}\s*$", regex::escape(name))).ok()?;
    declared.captures(&first).map(|m| m[1].trim().to_string())
}

impl KernelResolver {
    fn cpp_alias_text(&mut self, node: &KNode) -> Option<String> {
        let source = self.read_file(&node.file_path)?;
        let mut lines = source.get((node.start_line - 1).max(0) as usize..node.end_line.max(0) as usize)?.to_vec();
        let whole = lines.join("\n");
        let last = lines.len().checked_sub(1)?;
        let end = super::names::js_unit_to_byte(&lines[last], node.end_column.max(0) as usize);
        lines[last].truncate(end);
        let start = super::names::js_unit_to_byte(&lines[0], node.start_column.max(0) as usize);
        lines[0] = lines[0].get(start..)?.to_string();
        let cut = lines.join("\n");
        Some(if re!(r"\b(?:using|typedef)\b").is_match(&cut) { cut } else { whole })
    }

    fn cpp_alias_member(&mut self, scope: &str, name: &str, r: &ResolveRefIn, included: &HashSet<String>, depth: u32) -> Res<Option<Arc<KNode>>> {
        let qn = if scope.is_empty() { name.to_string() } else { format!("{scope}::{name}") };
        let nodes = self.nodes_by_qualified_name(&qn)?;
        let mut visible: Vec<_> = nodes.iter().filter(|n| matches!(n.language.as_str(), "cpp" | "c") && matches!(n.kind.as_str(), "type_alias" | "class" | "struct" | "union" | "enum" | "namespace") && included.contains(&n.file_path)).cloned().collect();
        let local: Vec<_> = visible.iter().filter(|n| n.file_path == r.file_path).cloned().collect();
        if !local.is_empty() { visible = local; }
        if let Some(cls) = visible.iter().find(|n| matches!(n.kind.as_str(), "class" | "struct" | "union")) { return Ok(Some(cls.clone())); }
        if let [only] = visible.as_slice() { return Ok(Some(only.clone())); }
        if !visible.is_empty() || depth >= 4 || scope.is_empty() { return Ok(None); }
        // Inheritance may not yet have been resolved when aliases are needed.
        let owners = self.nodes_by_qualified_name(scope)?;
        for owner in owners.iter().filter(|n| matches!(n.kind.as_str(), "class" | "struct" | "union")) {
            let Some(source) = self.read_file(&owner.file_path) else { continue; };
            let mut lines = source.get((owner.start_line - 1).max(0) as usize..((owner.start_line + 11).max(0) as usize).min(source.len())).unwrap_or(&[]).to_vec();
            if let Some(first) = lines.first_mut() {
                let start = super::names::js_unit_to_byte(first, owner.start_column.max(0) as usize);
                *first = first.get(start..).unwrap_or("").to_string();
            }
            let head = lines.join("\n");
            let Some((head, _)) = head.split_once('{') else { continue; };
            let head = head.replace("::", "@@");
            let Some((_, bases)) = head.split_once(':') else { continue; };
            let bases = bases.replace("@@", "::");
            for base in bases.split(',') {
                let base = re!(r"\b(?:public|private|protected|virtual)\b").replace_all(base, " ");
                let Some(parts) = type_segments(&base) else { continue; };
                if !base.trim_start().starts_with("::") && self.cpp_template_parameter(owner, &parts[0], r)? { continue; }
                let mut site = r.clone().at(owner); site.from_node_id = owner.id.clone();
                // Base specifiers belong to the enclosing scope. Searching the
                // class being defined re-enters this same inheritance lookup.
                let scopes: Vec<_> = owner.qualified_name.split("::").collect();
                for count in (0..if base.trim_start().starts_with("::") { 1 } else { scopes.len() }).rev() {
                    let prefix = scopes[..count].join("::");
                    let qualified = if prefix.is_empty() { format!("::{}", parts.join("::")) } else { format!("::{prefix}::{}", parts.join("::")) };
                    let Some(base) = self.cpp_type_owner_visible(&qualified, &site, depth + 1, false, included)? else { continue; };
                    if base.qualified_name != scope {
                        if let Some(found) = self.cpp_alias_member(&base.qualified_name, name, r, included, depth + 1)? { return Ok(Some(found)); }
                    }
                    break;
                }
            }
        }
        Ok(None)
    }

    pub(super) fn cpp_alias_expansion(&mut self, raw: &str, r: &ResolveRefIn, depth: u32, included: &HashSet<String>) -> Res<Option<CppAlias>> {
        if depth > 4 { return Ok(Some(CppAlias { raw: None, site: r.clone(), pointer: false, reference: false })); }
        let Some(parts) = type_segments(raw) else { return Ok(None); };
        let caller = self.node_by_id(&r.from_node_id)?;
        let mut local = None;
        if !raw.trim().starts_with("::") {
            if let Some(caller) = &caller {
                if matches!(caller.kind.as_str(), "function" | "method") && caller.file_path == r.file_path {
                    if let Some(source) = self.read_file(&r.file_path) {
                        for i in ((caller.start_line - 1).max(0) as usize..(r.line.max(0) as usize).min(source.len())).rev() {
                            let line = &source[i];
                            if !line.contains(&parts[0]) || (!line.contains("using") && !line.contains("typedef")) { continue; }
                            let Some(tree) = self.parsed_tree(&source, r) else { continue; };
                            for keyword in re!(r"\b(?:using|typedef)\b").find_iter(line) {
                                let column = line[..keyword.start()].encode_utf16().count();
                                let mut node = super::iteration::descendant_for_position(tree.root_node(), source.text(), (i, column + 1));
                                let mut code = true;
                                loop {
                                    if matches!(node.kind(), "comment" | "string_literal" | "raw_string_literal" | "char_literal") { code = false; break; }
                                    let Some(parent) = node.parent() else { break; }; node = parent;
                                }
                                if code {
                                    let declaration = &line[keyword.start()..];
                                    let declaration = declaration.split_once(';').map_or(declaration, |(head, _)| head);
                                    if let Some(rhs) = alias_rhs(declaration, &parts[0]) { local = Some((rhs, caller.clone())); break; }
                                }
                            }
                            if local.is_some() { break; }
                        }
                    }
                }
            }
        }
        let mut alias = local.map(|(rhs, node)| (rhs, node, 1usize));
        if alias.is_none() {
            let scopes: Vec<_> = if raw.trim().starts_with("::") { Vec::new() } else { caller.as_ref().map(|n| n.qualified_name.split("::").map(str::to_string).collect()).unwrap_or_default() };
            for i in (0..=scopes.len()).rev() {
                let scope = scopes[..i].join("::");
                let mut found = None;
                let mut consumed = 0;
                // Namespaces need not have indexed nodes; try complete prefixes.
                for count in 1..=parts.len() {
                    if let Some(node) = self.cpp_alias_member(&scope, &parts[..count].join("::"), r, included, depth)? { found = Some(node); consumed = count; break; }
                }
                let Some(mut found) = found else { continue; };
                while found.kind != "type_alias" && consumed < parts.len() {
                    let Some(next) = self.cpp_alias_member(&found.qualified_name, &parts[consumed], r, included, depth)? else { return Ok(None); };
                    found = next; consumed += 1;
                }
                if found.kind != "type_alias" { return Ok(None); }
                let text = self.cpp_alias_text(&found).unwrap_or_default();
                let rhs = alias_rhs(&text, &found.name).unwrap_or_default();
                alias = Some((rhs, found, consumed)); break;
            }
        }
        let Some((rhs, declaration, consumed)) = alias else { return Ok(None); };
        let mut site = r.clone().at(&declaration); site.from_node_id = declaration.id.clone(); site.column = declaration.start_column;
        let pointer = pointer_type(&rhs);
        let reference = reference_type(&rhs);
        let target = type_segments(&rhs);
        let dependent = match &target { Some(target) => !rhs.trim_start().starts_with("::") && self.cpp_template_parameter(&declaration, &target[0], r)?, None => false };
        if rhs.contains("typename") || dependent {
            return Ok(Some(CppAlias { raw: None, site, pointer, reference }));
        }
        let Some(mut target) = target else { return Ok(Some(CppAlias { raw: None, site, pointer, reference })); };
        target.extend_from_slice(&parts[consumed..]);
        let target = format!("{}{}", if rhs.trim_start().starts_with("::") { "::" } else { "" }, target.join("::"));
        if let Some(mut next) = self.cpp_alias_expansion(&target, &site, depth + 1, included)? { next.pointer |= pointer; next.reference |= reference; return Ok(Some(next)); }
        Ok(Some(CppAlias { raw: Some(target), site, pointer, reference }))
    }
}

#[cfg(test)]
mod tests {
    use super::type_segments;
    #[test]
    fn nested_type_segments() {
        for (raw, expected) in [("SkipList<const char*, KeyComparator>", "SkipList"), ("const ::leveldb::SkipList<Key, Cmp<int>>::Iterator&", "leveldb::SkipList::Iterator"), ("typename Base<T>::Iter", "Base::Iter"), ("struct Node*", "Node")] {
            assert_eq!(type_segments(raw).unwrap().join("::"), expected);
        }
        assert!(type_segments("unsigned int").is_none());
        assert!(type_segments("void (*)(int)").is_none());
    }
}

impl KernelResolver {
    /// Only the caller's function or class proves which declaration owns a receiver.
    pub(super) fn cpp_receiver_alias(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<CppAlias>> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None); };
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        let mut files = vec![r.file_path.clone()];
        for ext in [".h", ".hpp", ".hxx"] {
            let header = re!(r"(?i)\.(?:c|cc|cpp|cxx)$").replace(&r.file_path, ext).to_string();
            if header != r.file_path && self.file_exists(&header) { files.push(header); }
        }
        let escaped_receiver = regex::escape(receiver);
        for file in files {
            let Some(source) = self.read_file(&file) else { continue; };
            let indexes: Vec<_> = if file == r.file_path { (0..(r.line.max(0) as usize).min(source.len())).rev().collect() } else { (0..source.len()).collect() };
            for i in indexes {
                let line = &source[i];
                if !has_word(line, receiver) { continue; }
                let Some(raw) = self.cpp_declarator_match(line, &escaped_receiver)? else { continue; };
                let at = line.find(receiver).unwrap_or(line.len());
                if line[..at].contains("//") || re!(r"^\s*(?://|/\*|\*)").is_match(line) { return Ok(None); }
                let own_function = file == r.file_path && i as i64 + 1 >= caller.start_line;
                let mut own_class = false;
                if let Some((scope, _)) = caller.qualified_name.rsplit_once("::") {
                    own_class = self.nodes_by_qualified_name(scope)?.iter().any(|n| n.file_path == file && matches!(n.kind.as_str(), "class" | "struct" | "union") && i as i64 + 1 >= n.start_line && (i as i64) < n.end_line);
                }
                if !own_function && !own_class { return Ok(None); }
                let mut alias = self.cpp_alias_expansion(&raw, r, 0, &included)?;
                if let Some(alias) = &mut alias { alias.pointer |= pointer_type(&raw); alias.reference |= reference_type(&raw); }
                return Ok(alias);
            }
        }
        Ok(None)
    }

    /// Only alias dereference or a project template can outgrow the declared owner.
    pub(super) fn cpp_alias_allows_fallback(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "cpp" { return Ok(false); }
        let McShape::Parsed { receiver, .. } = self.method_call_shape(r)? else { return Ok(false); };
        let Some(alias) = self.cpp_receiver_alias(&receiver, r)? else { return Ok(false); };
        let Some(raw) = &alias.raw else { return Ok(false); };
        if !alias.pointer && self.cpp_receiver_operator_is(&receiver, "->", r) { return Ok(true); }
        self.cpp_alias_names_template(raw, &alias.site, r)
    }

    pub(super) fn cpp_receiver_operator_is(&mut self, receiver: &str, operator: &str, r: &ResolveRefIn) -> bool {
        self.read_file(&r.file_path).is_some_and(|lines| {
            let i = (r.line - 1).max(0) as usize;
            let Some(line) = lines.get(i) else { return false; };
            let at = super::names::js_unit_to_byte(line, r.column.max(0) as usize);
            let Some(rest) = line.get(at..).and_then(|s| s.strip_prefix(receiver)) else { return false; };
            if !rest.trim().is_empty() { return rest.trim_start().starts_with(operator); }
            lines.iter().skip(i + 1).take(3).find(|s| !s.trim().is_empty()).is_some_and(|s| s.trim_start().starts_with(operator))
        })
    }

    pub(super) fn cpp_alias_names_template(&mut self, raw: &str, site: &ResolveRefIn, r: &ResolveRefIn) -> Res<bool> {
        if let Some(owner) = self.cpp_type_owner(raw, site, 0, false)? {
            return self.cpp_alias_template_owner(&owner, r);
        }
        let name = raw.rsplit("::").next().unwrap_or(raw);
        let included = self.namespace_visible_files(&site.file_path, "cpp")?;
        let owners = self.nodes_by_name(name)?;
        for owner in owners.iter().filter(|n| n.language == "cpp" && matches!(n.kind.as_str(), "class" | "struct" | "union") && included.contains(&n.file_path) && n.qualified_name.ends_with(raw.trim_start_matches("::"))) {
            if self.cpp_alias_template_owner(owner, r)? { return Ok(true); }
        }
        Ok(false)
    }

    pub(super) fn cpp_alias_template_owner(&mut self, owner: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some(source) = self.read_file(&owner.file_path) else { return Ok(false); };
        let site = r.clone().at(owner);
        let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(false); };
        let mut node = super::iteration::descendant_for_position(tree.root_node(), source.text(), ((owner.start_line - 1).max(0) as usize, owner.start_column.max(0) as usize + 1));
        while let Some(parent) = node.parent() {
            if parent.kind() == "template_declaration" { return Ok(true); }
            node = parent;
        }
        Ok(false)
    }
}
