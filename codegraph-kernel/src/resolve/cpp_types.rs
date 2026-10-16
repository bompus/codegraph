//! C++ owner aliases are resolved at their declaration, with include visibility.
use super::*;

impl KernelResolver {
    pub(super) fn resolve_cpp_supertype(&mut self, r: &ResolveRefIn) -> Res<ResolveOutcome> {
        let Some(derived) = self.node_by_id(&r.from_node_id)? else { return Ok(ResolveOutcome::unresolved()); };
        if !matches!(derived.kind.as_str(), "class" | "struct" | "union") { return Ok(ResolveOutcome::unresolved()); }
        let mut written = r.reference_name.trim().to_string();
        if let Some(lines) = self.read_file(&r.file_path) {
            let text = lines.iter().skip((r.line - 1).max(0) as usize).take(12).cloned().collect::<Vec<_>>().join("\n");
            let at = super::names::js_unit_to_byte(&text, r.column.max(0) as usize).min(text.len());
            let mut depth = 0usize;
            let mut end = text.len();
            for (i, ch) in text[at..].char_indices() {
                match ch {
                    '<' | '(' => depth += 1,
                    '>' | ')' => depth = depth.saturating_sub(1),
                    ',' | '{' | ';' if depth == 0 => { end = at + i; break; }
                    _ => (),
                }
            }
            let raw = text[at..end].trim();
            if super::cpp_aliases::type_segments(raw) == super::cpp_aliases::type_segments(&written) { written = raw.to_string(); }
        }
        let Some(parts) = super::cpp_aliases::type_segments(&written) else { return Ok(ResolveOutcome::unresolved()); };
        if self.cpp_template_parameter(&derived, &written, r)? { return Ok(ResolveOutcome::unresolved()); }
        let files = match self.sorted_files() { Some(files) => files, None => Arc::new(self.table()?.files.iter().cloned().collect()) };
        let visible: HashSet<_> = files.iter().filter(|file| **file == r.file_path
            || !re!(r"(?i)\.(?:c|cc|cpp|cxx|c\+\+|cu|metal)$").is_match(file)).cloned().collect();
        let mut target = self.cpp_type_owner_visible(&written, r, 0, false, &visible)?;
        if target.is_none() { target = self.cpp_type_via_using(&written, &parts, r, &visible)?; }
        let mut confidence = 0.9;
        let mut resolved_by = "qualified-name";
        if target.is_none() {
            let tail = parts.join("::");
            let candidates: Vec<_> = self.nodes_by_name(parts.last().unwrap())?.iter()
                .filter(|n| matches!(n.language.as_str(), "c" | "cpp") && matches!(n.kind.as_str(), "class" | "struct" | "union")
                    && visible.contains(&n.file_path) && (n.qualified_name == tail || n.qualified_name.ends_with(&format!("::{tail}"))))
                .cloned().collect();
            let own: Vec<_> = candidates.iter().filter(|n| n.file_path == r.file_path).cloned().collect();
            let candidates = if own.is_empty() { candidates } else { own };
            if let [only] = candidates.as_slice() { target = Some(only.clone()); confidence = 0.7; resolved_by = "exact-match"; }
        }
        match target {
            Some(node) => self.finish_pre_framework(r, KCand { node, confidence, resolved_by }),
            None => Ok(ResolveOutcome::unresolved()),
        }
    }

    /// The type `written` names through a `using namespace`, `using ns::Name` or
    /// namespace alias that precedes `r` in an enclosing scope, innermost first.
    pub(super) fn cpp_type_via_using(&mut self, written: &str, parts: &[String], r: &ResolveRefIn, visible: &HashSet<String>) -> Res<Option<Arc<KNode>>> {
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let mut directives = Vec::new();
        if let Some(tree) = self.parsed_tree(&lines, r) {
            let mut site = super::iteration::descendant_for_position(tree.root_node(), lines.text(),
                ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
            let before = site.start_byte();
            loop {
                if matches!(site.kind(), "translation_unit" | "declaration_list" | "field_declaration_list" | "compound_statement") {
                    let mut cursor = site.walk();
                    let mut local: Vec<_> = site.named_children(&mut cursor)
                        .filter(|n| n.end_byte() <= before && matches!(n.kind(), "using_declaration" | "namespace_alias_definition"))
                        .map(|n| lines.text()[n.byte_range()].to_string()).collect();
                    local.reverse();
                    directives.extend(local);
                }
                let Some(parent) = site.parent() else { break; };
                site = parent;
            }
        }
        for line in &directives {
            let spelled = if let Some(hit) = re!(r"\bnamespace\s+([A-Za-z_]\w*)\s*=\s*((?:::)?[\w:]+)\s*;").captures(line) {
                (hit[1] == parts[0]).then(|| format!("{}{}", &hit[2], written.trim_start_matches("::").strip_prefix(&parts[0]).unwrap_or("")))
            } else if !written.starts_with("::") {
                if let Some(hit) = re!(r"\busing\s+namespace\s+((?:::)?[\w:]+)\s*;").captures(line) {
                    Some(format!("{}::{written}", &hit[1]))
                } else if let Some(hit) = re!(r"\busing\s+((?:::)?[\w:]+)::([A-Za-z_]\w*)\s*;").captures(line) {
                    (hit[2] == parts[0]).then(|| format!("{}::{written}", &hit[1]))
                } else { None }
            } else { None };
            if let Some(spelled) = spelled {
                let target = self.cpp_type_owner_visible(&spelled, r, 0, false, visible)?;
                if target.is_some() { return Ok(target); }
            }
        }
        Ok(None)
    }

    /// The visible types `ty` names, found in the scopes around the caller, innermost
    /// first; the first scope that has any answers, preferring those in the caller's file.
    pub(super) fn cpp_scoped_types(&mut self, raw: &str, ty: &str, r: &ResolveRefIn, included: &HashSet<String>) -> Res<Vec<Arc<KNode>>> {
        let mut scopes = if raw.starts_with("::") { Vec::new() } else {
            self.node_by_id(&r.from_node_id)?.map(|n| n.qualified_name.split("::").map(str::to_string).collect::<Vec<_>>()).unwrap_or_default()
        };
        if is_inheritance_ref(&r.reference_kind) { scopes.pop(); }
        let mut names: Vec<_> = (1..=scopes.len()).rev().map(|i| format!("{}::{ty}", scopes[..i].join("::"))).collect();
        names.push(ty.to_string());
        for name in names {
            let named = self.nodes_by_qualified_name(&name)?;
            let mut visible = Vec::new();
            for node in named.iter().filter(|n| matches!(n.language.as_str(), "c" | "cpp") && matches!(n.kind.as_str(), "class" | "struct" | "union" | "type_alias" | "enum")) {
                if included.contains(&node.file_path) { visible.push(node.clone()); }
            }
            if visible.is_empty() { continue; }
            let local: Vec<_> = visible.iter().filter(|n| n.file_path == r.file_path).cloned().collect();
            return Ok(if local.is_empty() { visible } else { local });
        }
        Ok(Vec::new())
    }

    pub(super) fn cpp_type_owner(&mut self, raw: &str, r: &ResolveRefIn, depth: u32, constructor: bool) -> Res<Option<Arc<KNode>>> {
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        self.cpp_type_owner_visible(raw, r, depth, constructor, &included)
    }

    pub(super) fn cpp_type_owner_visible(&mut self, raw: &str, r: &ResolveRefIn, depth: u32, constructor: bool, included: &HashSet<String>) -> Res<Option<Arc<KNode>>> {
        if depth > 4 { return Ok(None); }
        if let Some(alias) = self.cpp_alias_expansion(raw, r, depth, included)? {
            if constructor && (alias.pointer || alias.reference) { return Ok(None); }
            let target = match alias.raw { Some(raw) => self.cpp_type_owner_visible(&raw, &alias.site, depth + 1, constructor, included)?, None => None };
            if target.is_some() || !is_inheritance_ref(&r.reference_kind) { return Ok(target); }
            return Ok(self.node_by_id(&alias.site.from_node_id)?.filter(|n| n.kind == "type_alias"));
        }
        let normalized = super::cpp_aliases::type_segments(raw).map(|p| p.join("::")).unwrap_or_else(|| raw.to_string());
        let ty = normalized.trim();
        let simple = ty.rsplit("::").next().unwrap_or(ty);
        let visible = self.cpp_scoped_types(raw, ty, r, included)?;
        if !visible.is_empty() {
            let [owner] = visible.as_slice() else { return Ok(None) };
            return self.cpp_expand_owner(owner, r, depth, constructor, included);
        }
        // A name C++ finds in a class's base: `Derived::Nested` for a `Nested` the base declares.
        if let Some(found) = self.cpp_type_through_bases(raw, r, included)? {
            return self.cpp_expand_owner(&found, r, depth, constructor, included);
        }
        // A name written through a macro that stands for a namespace, or the other way round.
        if ty.contains("::") {
            if let Some(spelled) = self.cpp_name_macro_spelling(&r.file_path, ty)? {
                let raw = if raw.trim_start().starts_with("::") { format!("::{spelled}") } else { spelled.clone() };
                if let [owner] = self.cpp_scoped_types(&raw, &spelled, r, included)?.as_slice() { return self.cpp_expand_owner(owner, r, depth, constructor, included); }
            }
        }
        // A name written through a namespace alias another file declares: protobuf hpb's
        // `backend::Arena` under `namespace backend = hpb::internal::backend::upb;`.
        if let Some((head, rest)) = ty.split_once("::").filter(|_| !raw.trim_start().starts_with("::")) {
            if let Some(target) = self.cpp_namespace_alias(&r.file_path, head, r.line)? {
                if let Some(found) = self.cpp_type_owner_visible(&format!("{target}::{rest}"), r, depth + 1, constructor, included)? { return Ok(Some(found)); }
            }
        }
        // Namespace-opening macros can leave a header's indexed owner unqualified.
        if ty.contains("::") {
            let named = self.nodes_by_name(simple)?;
            let mut visible = Vec::new();
            for node in named.iter().filter(|n| matches!(n.language.as_str(), "c" | "cpp") && (n.qualified_name == simple || ty.ends_with(&format!("::{}", n.qualified_name))) && matches!(n.kind.as_str(), "class" | "struct" | "union" | "type_alias")) {
                if !included.contains(&node.file_path) { continue; }
                let frames = self.namespace_frames(&node.file_path)?;
                let mut frames: Vec<_> = frames.into_iter().filter(|(start, end, _)| *start <= node.start_line && *end >= node.start_line).collect();
                frames.sort_by_key(|(start, _, _)| *start);
                let mut namespace: Vec<String> = frames.into_iter().flat_map(|(_, _, p)| p).collect();
                if let Some(source) = self.read_file(&node.file_path) {
                    let mut site = r.clone().at(node); site.from_node_id = node.id.clone(); site.column = node.start_column;
                    if let Some(tree) = self.parsed_tree(&source, &site) {
                        let mut at = super::iteration::descendant_for_position(tree.root_node(), source.text(), ((node.start_line - 1).max(0) as usize, node.start_column.max(0) as usize));
                        let mut ordinary = Vec::new();
                        while let Some(parent) = at.parent() {
                            if parent.kind() == "namespace_definition" {
                                if let Some(name) = parent.child_by_field_name("name") { ordinary.push(source.text()[name.start_byte()..name.end_byte()].to_string()); }
                            }
                            if parent.kind() == "function_definition" {
                                if let Some(body) = parent.child_by_field_name("body") {
                                    let head = &source.text()[parent.start_byte()..body.start_byte()];
                                    if let Some(namespace) = re!(r"(?:^|\n)\s*namespace\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*$").captures(head) { ordinary.push(namespace[1].to_string()); }
                                }
                            }
                            at = parent;
                        }
                        namespace.extend(ordinary.into_iter().rev());
                    }
                }
                namespace.push(node.name.clone());
                if namespace.join("::") == ty { visible.push(node.clone()); }
            }
            if let [owner] = visible.as_slice() { return self.cpp_expand_owner(owner, r, depth, constructor, included); }
        }
        Ok(None)
    }

    pub(super) fn cpp_template_parameter(&mut self, owner: &KNode, ty: &str, r: &ResolveRefIn) -> Res<bool> {
        if ty.starts_with("::") { return Ok(false); }
        let root = ty.split("::").next().unwrap_or(ty);
        let Some(source) = self.read_file(&owner.file_path) else { return Ok(false) };
        let mut site = r.clone().at(owner); site.from_node_id = owner.id.clone(); site.column = owner.start_column;
        let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(false) };
        let mut node = super::iteration::descendant_for_position(tree.root_node(), source.text(), ((owner.start_line - 1).max(0) as usize, owner.start_column.max(0) as usize));
        while let Some(parent) = node.parent() {
            if parent.kind() == "template_declaration" {
                if let Some(parameters) = parent.child_by_field_name("parameters") {
                    let text = &source.text()[parameters.start_byte()..parameters.end_byte()];
                    if re!(r"\b(?:class|typename)\s*(?:\.\.\.\s*)?([A-Za-z_]\w*)").captures_iter(text).any(|m| &m[1] == root) { return Ok(true); }
                }
            }
            node = parent;
        }
        Ok(false)
    }

    /// Whether the alias `owner` names a pointer or a reference (`using Ptr = Widget*;`), itself or
    /// through the aliases it names (`using Ptr2 = Ptr;`).
    pub(super) fn cpp_alias_indirect(&mut self, owner: &KNode, r: &ResolveRefIn) -> bool {
        let mut current = owner.clone();
        for _ in 0..8 {
            let Some(rhs) = self.cpp_alias_rhs(&current) else { return false };
            if super::cpp_aliases::indirect_type(&rhs) { return true; }
            let named = re!(r"^(?:const|volatile)\s+").replace(&rhs, "").to_string();
            if !re!(r"^(?:::)?[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*$").is_match(&named) { return false; }
            // The alias it names, looked up from where this one is declared outward (a name
            // written with a leading `::` only from the global scope).
            let global = named.starts_with("::");
            let named = named.trim_start_matches("::");
            let mut scopes: Vec<String> = Vec::new();
            if !global {
                let mut scope = current.qualified_name.as_str();
                while let Some(cut) = scope.rfind("::") { scope = &scope[..cut]; scopes.push(scope.to_string()); }
            }
            scopes.push(String::new());
            let mut next = None;
            for scope in &scopes {
                let Ok(found) = self.cpp_scope_types(scope, named, r, 0) else { return false };
                if let Some(hit) = super::cpp_construction::pick(found, &r.file_path) { next = Some(hit); break; }
            }
            match next { Some(n) if n.kind == "type_alias" => current = (*n).clone(), _ => return false }
        }
        false
    }

    /// What the alias `owner` is declared as: the text after its `=`.
    fn cpp_alias_rhs(&mut self, owner: &KNode) -> Option<String> {
        let source = self.read_file(&owner.file_path)?;
        // (From the code with comments masked: `using Plain = Widget /* an object * */;`.)
        let mut lines = source.cpp_code_lines().get((owner.start_line - 1).max(0) as usize..owner.end_line.max(0) as usize).unwrap_or(&[]).to_vec();
        if lines.is_empty() { return None; }
        let last = lines.len() - 1;
        let end = super::names::js_unit_to_byte(&lines[last], owner.end_column.max(0) as usize);
        lines[last].truncate(end);
        lines[0] = lines[0].get(super::names::js_unit_to_byte(&lines[0], owner.start_column.max(0) as usize)..).unwrap_or("").to_string();
        let text = lines.join("\n");
        let (_, rhs) = text.split_once('=')?;
        Some(rhs.trim().trim_end_matches(';').trim().to_string())
    }

    pub(super) fn cpp_expand_owner(&mut self, owner: &Arc<KNode>, r: &ResolveRefIn, depth: u32, constructor: bool, included: &HashSet<String>) -> Res<Option<Arc<KNode>>> {
        if owner.kind != "type_alias" { return Ok((owner.kind != "enum").then(|| owner.clone())); }
        let Some(rhs) = self.cpp_alias_rhs(owner) else { return Ok(None) };
        let rhs = rhs.as_str();
        if constructor && rhs.contains(['*', '&']) { return Ok(None); }
        let rhs = re!(r"^(?:const|volatile)\s+").replace(rhs, "");
        let rhs = rhs.trim_end_matches(['&', '*']).trim();
        if !re!(r"^(?:::)?[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*(?:\s*<[^;{}()]*>)?$").is_match(rhs) { return Ok(None); }
        let target = rhs.split('<').next().unwrap_or(rhs).trim();
        if self.cpp_template_parameter(owner, target, r)? { return Ok(None); }
        let mut site = r.clone().at(owner); site.from_node_id = owner.id.clone(); site.column = owner.start_column;
        self.cpp_type_owner_visible(target, &site, depth + 1, constructor, included)
    }
}
