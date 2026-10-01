//! C++ owner aliases are resolved at their declaration, with include visibility.
use super::*;

impl KernelResolver {
    pub(super) fn cpp_type_owner(&mut self, raw: &str, r: &ResolveRefIn, depth: u32, constructor: bool) -> Res<Option<Arc<KNode>>> {
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        self.cpp_type_owner_visible(raw, r, depth, constructor, &included)
    }

    pub(super) fn cpp_type_owner_visible(&mut self, raw: &str, r: &ResolveRefIn, depth: u32, constructor: bool, included: &HashSet<String>) -> Res<Option<Arc<KNode>>> {
        if depth > 4 { return Ok(None); }
        let ty = raw.trim_start_matches("::").split('<').next().unwrap_or(raw).trim();
        let scopes = if raw.starts_with("::") { Vec::new() } else {
            self.node_by_id(&r.from_node_id)?.map(|n| n.qualified_name.split("::").map(str::to_string).collect::<Vec<_>>()).unwrap_or_default()
        };
        let mut names: Vec<_> = (1..=scopes.len()).rev().map(|i| format!("{}::{ty}", scopes[..i].join("::"))).collect();
        names.push(ty.to_string());
        let simple = ty.rsplit("::").next().unwrap_or(ty);
        for name in names {
            let named = self.nodes_by_qualified_name(&name)?;
            let mut visible = Vec::new();
            for node in named.iter().filter(|n| n.language == "cpp" && matches!(n.kind.as_str(), "class" | "struct" | "union" | "type_alias" | "enum")) {
                if included.contains(&node.file_path) { visible.push(node.clone()); }
            }
            if visible.is_empty() { continue; }
            let local: Vec<_> = visible.iter().filter(|n| n.file_path == r.file_path).cloned().collect();
            if !local.is_empty() { visible = local; }
            let [owner] = visible.as_slice() else { return Ok(None) };
            return self.cpp_expand_owner(owner, r, depth, constructor, included);
        }
        // Namespace-opening macros can leave a header's indexed owner unqualified.
        if ty.contains("::") {
            let named = self.nodes_by_name(simple)?;
            let mut visible = Vec::new();
            for node in named.iter().filter(|n| n.language == "cpp" && n.qualified_name == simple && matches!(n.kind.as_str(), "class" | "struct" | "union" | "type_alias")) {
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
        let mut node = super::iteration::descendant_for_position(tree.root_node(), source.text(), ((owner.start_line - 1).max(0) as usize, owner.start_column.max(0) as usize + 1));
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

    fn cpp_expand_owner(&mut self, owner: &Arc<KNode>, r: &ResolveRefIn, depth: u32, constructor: bool, included: &HashSet<String>) -> Res<Option<Arc<KNode>>> {
        if owner.kind != "type_alias" { return Ok((owner.kind != "enum").then(|| owner.clone())); }
        let Some(source) = self.read_file(&owner.file_path) else { return Ok(None) };
        let mut lines = source.get((owner.start_line - 1).max(0) as usize..owner.end_line.max(0) as usize).unwrap_or(&[]).to_vec();
        if lines.is_empty() { return Ok(None); }
        let last = lines.len() - 1;
        let end = super::names::js_unit_to_byte(&lines[last], owner.end_column.max(0) as usize);
        lines[last].truncate(end);
        lines[0] = lines[0].get(super::names::js_unit_to_byte(&lines[0], owner.start_column.max(0) as usize)..).unwrap_or("").to_string();
        let text = lines.join("\n");
        let Some((_, rhs)) = text.split_once('=') else { return Ok(None) };
        let rhs = rhs.trim().trim_end_matches(';').trim();
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
