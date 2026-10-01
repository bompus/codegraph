//! Swift constructed receivers, typed properties and argument labels.
use super::*;
impl KernelResolver {
    pub(super) fn swift_type_closure(&mut self, name: &str) -> Res<HashSet<String>> {
        let mut queue = VecDeque::from([name.to_string()]);
        let mut seen = HashSet::new();
        while seen.len() < 40 {
            let Some(name) = queue.pop_front() else { break };
            if !seen.insert(name.clone()) {
                continue;
            }
            queue.extend(self.swift_decl(&name)?.0);
        }
        Ok(seen)
    }
    pub(super) fn swift_property_receiver_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let name = receiver.strip_prefix("self.").unwrap_or(receiver);
        if !re!(r"^[A-Za-z_]\w*$").is_match(name) {
            return Ok(None);
        }
        let classes: Vec<_> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| {
                n.language == "swift"
                    && super::call_shape::type_kind(&n.kind)
                    && n.start_line <= r.line
                    && n.end_line >= r.line
            })
            .cloned()
            .collect();
        for class in classes {
            let Some(source) = self.read_file(&class.file_path) else {
                continue;
            };
            let from = (class.start_line - 1).max(0) as usize;
            let to = (class.end_line.max(0) as usize).min(source.len());
            let pattern = Self::cached_regex(&format!(
                r"\b(?:var|let)\s+{}\s*:\s*([A-Z]\w*(?:\.[A-Z]\w*)*)(?:\s*[<!?]|\b)",
                regex::escape(name)
            ))?;
            for declaration in self.nodes_in_file(&class.file_path)?.iter().filter(|n| {
                n.name == name
                    && matches!(
                        n.kind.as_str(),
                        "property" | "field" | "variable" | "constant"
                    )
                    && n.start_line >= class.start_line
                    && n.end_line <= class.end_line
            }) {
                if let Some(line) = source.get((declaration.start_line - 1).max(0) as usize) {
                    if let Some(hit) = pattern.captures(line) {
                        return Ok(Some(hit[1].rsplit('.').next().unwrap_or("").to_string()));
                    }
                }
            }
            // Some grammars do not expose stored properties as nodes.
            if from < to {
                let text = source[from..to].join("\n");
                if let Some(hit) = pattern.captures(&text) {
                    return Ok(Some(hit[1].rsplit('.').next().unwrap_or("").to_string()));
                }
            }
        }
        Ok(None)
    }

    pub(super) fn swift_overload_labels_fit(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
    ) -> Option<bool> {
        let call_source = self.read_file(&r.file_path)?;
        let declaration_source = self.read_file(&n.file_path)?;
        let call_from = (r.line - 1).max(0) as usize;
        let declaration_from = (n.start_line - 1).max(0) as usize;
        let call = call_source
            .get(call_from..call_source.len().min(call_from + 12))?
            .join("\n");
        let declaration = declaration_source
            .get(declaration_from..declaration_source.len().min(declaration_from + 12))?
            .join("\n");
        let arguments = swift_paren_list(
            &call,
            &n.name,
            js_unit_to_byte(&call, r.column.max(0) as usize),
        )?;
        let parameters = swift_paren_list(&declaration, &n.name, 0)?;
        let labels: Vec<_> = swift_split_list(arguments)
            .iter()
            .map(|arg| {
                re!(r"^\s*([A-Za-z_]\w*)\s*:")
                    .captures(arg)
                    .map(|m| m[1].to_string())
                    .unwrap_or_else(|| "_".to_string())
            })
            .collect();
        let mut at = 0usize;
        for parameter in swift_split_list(parameters) {
            let label = re!(r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:inout\s+)?([A-Za-z_]\w*)(?:\s+([A-Za-z_]\w*))?\s*:").captures(parameter).map(|m| m[1].to_string()).unwrap_or_else(|| "_".to_string());
            if labels.get(at) == Some(&label) {
                at += 1;
                continue;
            }
            if !parameter.contains('=') && !parameter.contains("...") {
                return Some(false);
            }
        }
        Some(at == labels.len())
    }
}
fn swift_paren_list<'a>(text: &'a str, name: &str, column: usize) -> Option<&'a str> {
    let pattern = KernelResolver::cached_regex(&format!(
        r"\b{}\s*(?:<[^<>()]*>)?\s*\(",
        regex::escape(name)
    ))
    .ok()?;
    let found = pattern.find(text.get(column..)?)?;
    let open = column + found.end() - 1;
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escape = false;
    for (offset, ch) in text[open..].char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && quoted {
            escape = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(&text[open + 1..open + offset]);
            }
        }
    }
    None
}
fn swift_split_list(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut quoted = false;
    let mut escape = false;
    for (offset, ch) in text.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && quoted {
            escape = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        if matches!(ch, '(' | '[' | '{' | '<') {
            depth += 1;
        } else if matches!(ch, ')' | ']' | '}' | '>') {
            depth = depth.saturating_sub(1);
        } else if ch == ',' && depth == 0 {
            parts.push(text[start..offset].trim());
            start = offset + 1;
        }
    }
    if !text[start..].trim().is_empty() {
        parts.push(text[start..].trim());
    }
    parts
}
