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

    /// The overload a Swift call means when its last argument says which: of
    /// a type's overloads alike in their labels, a closure — trailing or in
    /// the parentheses — goes to the one whose last parameter takes a
    /// function, and any other value to the one whose last parameter doesn't.
    /// None when the overloads don't differ that way.
    pub(super) fn swift_overload_for_closure(
        &mut self,
        target: &KNode,
        siblings: &[Arc<KNode>],
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        let overloads: Vec<Arc<KNode>> = std::iter::once(Arc::new(target.clone())).chain(siblings.iter().cloned()).collect();
        let params_of = |me: &mut Self, n: &KNode| -> Option<Vec<String>> {
            let lines = me.read_file(&n.file_path)?;
            let from = (n.start_line - 1).max(0) as usize;
            let declaration = lines.get(from..lines.len().min(from + 12))?.join("\n");
            let list = swift_paren_list(&declaration, &n.name, 0)?;
            Some(swift_split_list(list).into_iter().map(str::to_string).collect())
        };
        if !overloads.iter().any(|n| params_of(self, n).is_some_and(|p| p.iter().any(|x| is_swift_function_parameter(x)))) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let from = (r.line - 1).max(0) as usize;
        let text = lines.get(from..lines.len().min(from + 12)).map(|l| l.join("\n")).unwrap_or_default();
        let start = js_unit_to_byte(&text, r.column.max(0) as usize);
        let pattern = Self::cached_regex(&format!(r"\b{}\s*(?:<[^<>()]*>)?\s*([({{])", regex::escape(&target.name)))?;
        let tail = text.get(start..).unwrap_or("");
        let Some(named) = pattern.captures(tail) else { return Ok(None) };
        let Some(open) = named.get(1) else { return Ok(None) };
        let after = start + open.start();
        let bytes = text.as_bytes();
        let mut close = after.saturating_sub(1);
        if bytes.get(after) == Some(&b'(') {
            let mut depth = 0i32;
            close = after;
            while close < bytes.len() {
                match bytes[close] {
                    b'(' => depth += 1,
                    b')' => { depth -= 1; if depth == 0 { break; } }
                    _ => {}
                }
                close += 1;
            }
            if close >= bytes.len() { return Ok(None); }
        }
        let written = text.get(after + 1..close.max(after + 1)).unwrap_or("").to_string();
        let args: Vec<String> = if written.trim().is_empty() { Vec::new() } else { swift_split_list(&written).into_iter().map(str::to_string).collect() };
        let label_of = |arg: &str| -> String {
            let Some(m) = re!(r"^\s*([A-Za-z_]\w*)\s*:").find(arg) else { return "_".to_string() };
            if arg.as_bytes().get(m.end()) == Some(&b':') { return "_".to_string(); }
            arg[m.start()..m.end()].trim().trim_end_matches(':').to_string()
        };
        // The `{` after a condition is the statement's own: Swift takes no
        // trailing closure there.
        let trailing = text.get(close + 1..).is_some_and(|rest| re!(r"^\s*\{").is_match(rest))
            && !lines.get(from).is_some_and(|line| re!(r"^\s*(?:\}\s*else\s+)?(?:if|guard|while|for|switch)\b").is_match(line));
        if !trailing && args.is_empty() { return Ok(None); }
        let closure = trailing || args.last().is_some_and(|arg| re!(r"^\s*(?:[A-Za-z_]\w*\s*:)?\s*\{").is_match(arg));
        let labels: Vec<String> = args.iter().map(|a| label_of(a)).collect();
        let takes_function = |me: &mut Self, n: &KNode| -> Option<bool> {
            let params = params_of(me, n)?;
            let mut next = 0usize;
            let mut bound: Option<String> = None;
            let mut trailed = !trailing;
            for param in params {
                if next < labels.len() && swift_label_fits(&labels[next], &param) {
                    next += 1;
                    if next == labels.len() && !trailing { bound = Some(param.clone()); }
                } else if !trailed && next == labels.len() && is_swift_function_parameter(&param) {
                    trailed = true;
                    bound = Some(param.clone());
                } else if !swift_label_fits("", &param) {
                    return None;
                }
            }
            (next == labels.len()).then(|| bound.is_some_and(|b| is_swift_function_parameter(&b)))
        };
        let mut chosen = Vec::new();
        for n in &overloads {
            if takes_function(self, n) == Some(closure) { chosen.push(n.clone()); }
        }
        Ok((chosen.len() == 1).then(|| chosen[0].clone()))
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


/// Whether a Swift parameter (`_ body: @escaping (Int) -> Void`) takes a
/// function: its type has a `->` outside any brackets of the type's own.
fn is_swift_function_parameter(param: &str) -> bool {
    let Some((_, ty)) = param.split_once(':') else { return false };
    let ty = ty.split('=').next().unwrap_or("").trim();
    ty.contains("->")
        && !ty.contains("@autoclosure")
        && !ty.starts_with('[')
        && !re!(r"^[A-Za-z_][\w.]*\s*<").is_match(ty)
}

/// Whether a call argument with `label` binds the Swift parameter written
/// `name:`, `_ name:`, `@escaping name:`; an optional or variadic parameter
/// may be skipped.
fn swift_label_fits(label: &str, param: &str) -> bool {
    let head = re!(r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:inout\s+)?([A-Za-z_]\w*)(?:\s+([A-Za-z_]\w*))?\s*:").captures(param);
    let param_label = head.and_then(|m| m.get(1)).map_or("_", |m| m.as_str());
    let optional = param.contains('=') || param.contains("...");
    param_label == label || (label.is_empty() && optional)
}
