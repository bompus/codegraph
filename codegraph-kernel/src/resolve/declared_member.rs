//! Field and property receiver types, including inherited generic declarations.
use super::*;

fn class_kind(kind: &str) -> bool { matches!(kind, "class" | "struct" | "interface" | "enum" | "record" | "trait") }
fn angle_arguments(text: &str) -> Vec<String> {
    let Some(open) = text.find('<') else { return vec![] };
    let mut depth = 0usize;
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text[open + 1..].chars() {
        if ch == '<' { depth += 1; }
        else if ch == '>' { if depth == 0 { break; } depth -= 1; }
        if ch == ',' && depth == 0 { out.push(current.trim().to_string()); current.clear(); }
        else { current.push(ch); }
    }
    out.push(current.trim().to_string()); out
}
impl KernelResolver {
    pub(super) fn explicit_this_receiver(&mut self, receiver: &str, method: &str, r: &ResolveRefIn) -> bool {
        let Some(lines) = self.read_file(&r.file_path) else { return false };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return false };
        let call = format!("{receiver}.{method}");
        let mut hits = line.match_indices(&call).map(|(i, _)| &line[..i]).peekable();
        hits.peek().is_some() && hits.all(|before| before.ends_with("this."))
    }
    fn declaration_head(&mut self, n: &KNode, count: i64) -> String {
        let Some(lines) = self.read_file(&n.file_path) else { return String::new() };
        let from = ((n.start_line - 1).max(0) as usize).min(lines.len());
        let to = ((n.start_line + count).max(0) as usize).min(lines.len());
        super::awaited::strip_ts_comments(&lines[from..to.max(from)].join("\n"))
    }
    fn declared_member_type(&mut self, cls: &KNode, name: &str) -> Res<Option<String>> {
        let key = (cls.id.clone(), name.to_string());
        if let Some(hit) = self.declared_member_memo.get(&key) { return Ok(hit.clone()); }
        let escaped = regex::escape(name);
        let pattern = if cls.language == "kotlin" {
            format!(r"\b(?:val|var)\s+{escaped}\s*:\s*([A-Z][\w.]*)")
        } else {
            format!(r"(?:^|[\s(,])([A-Za-z_][\w.]*)\s*(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?(?:\s*\[[\s,]*\])*\??\s+{escaped}\s*(?:[=;,)]|\{{)")
        };
        let re = Self::cached_regex(&pattern)?;
        let text = self.declaration_head(cls, cls.end_line - cls.start_line);
        let text = super::awaited::blank_string_contents(&text);
        let mut depth = 0usize;
        let mut found = None;
        for line in text.lines() {
            if depth <= 1 {
                if let Some(m) = re.captures(line) {
                    let before = &line[..m.get(0).unwrap().start()];
                    let in_params = depth == 1 && before.matches('(').count() > before.matches(')').count();
                    if !in_params && !matches!(&m[1], "return" | "new" | "throw" | "var" | "val" | "get" | "set" | "init" | "class" | "interface" | "object" | "static" | "final") {
                        found = self.normalize_inferred_type_name(&m[1])?; break;
                    }
                }
            }
            for ch in line.chars() { if ch == '{' { depth += 1; } else if ch == '}' { depth = depth.saturating_sub(1); } }
        }
        self.declared_member_memo.insert(key, found.clone()); Ok(found)
    }
    fn declared_parameter_bound(&mut self, name: &str, cls: &KNode) -> Res<Option<Option<String>>> {
        let head = self.declaration_head(cls, 5);
        let head = head.split('{').next().unwrap_or("");
        let name = regex::escape(name);
        let bound = Self::cached_regex(&format!(r"[<,]\s*(?:in\s+|out\s+|reified\s+)?{name}\s*(?:extends|:)\s*([A-Z][\w.]*)|\bwhere\s+{name}\s*:\s*([A-Z][\w.]*)"))?;
        if let Some(m) = bound.captures(head) { return Ok(Some(m.get(1).or_else(|| m.get(2)).map(|v| v.as_str().rsplit('.').next().unwrap_or("").to_string()))); }
        Ok(Self::cached_regex(&format!(r"[<,]\s*(?:in\s+|out\s+|reified\s+)?{name}\s*[,>]"))?.is_match(head).then_some(None))
    }
    pub(super) fn infer_declared_member_receiver_type(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if !matches!(r.language.as_str(), "java" | "kotlin" | "csharp") { return Ok(None); }
        let name = receiver.strip_prefix("this.").unwrap_or(receiver);
        if !re!(r"^[A-Za-z_]\w*$").is_match(name) { return Ok(None); }
        let nodes = self.nodes_in_file(&r.file_path)?;
        let Some(cls) = nodes.iter().filter(|n| n.language == r.language && class_kind(&n.kind) && n.start_line <= r.line && n.end_line >= r.line).max_by_key(|n| n.start_line) else { return Ok(None) };
        if !receiver.starts_with("this.") {
            if let Some(f) = nodes.iter().filter(|n| n.language == r.language && matches!(n.kind.as_str(), "method" | "function") && n.start_line <= r.line && n.end_line >= r.line).max_by_key(|n| n.start_line) {
                let body = self.declaration_head(f, r.line - f.start_line);
                let n = regex::escape(name);
                if Self::cached_regex(&format!(r"\b(?:var|val|out\s+[\w.<>?]+|foreach\s*\(\s*[\w.<>?,\s]+?)\s+{n}\b|\bfor\s*\([^;)]*\s{n}\s*:|\b{n}\s*=>|[(,]\s*{n}\s*(?:,[^()]*)?\)\s*=>|\b{n}\s*(?:,[^{{}}]*)?->"))?.is_match(&body) { return Ok(None); }
            }
        }
        let mut queue = VecDeque::from([(cls.clone(), HashMap::<String, String>::new())]);
        let mut seen = HashSet::new();
        while seen.len() < 8 {
            let Some((decl, args)) = queue.pop_front() else { break };
            if !seen.insert(decl.id.clone()) { continue; }
            if let Some(found) = self.declared_member_type(&decl, name)? {
                if let Some(given) = args.get(&found) { return Ok(Some(given.clone())); }
                return Ok(Some(match self.declared_parameter_bound(&found, &decl)? { Some(Some(b)) => b, Some(None) => "object".to_string(), None => found }));
            }
            let head = self.declaration_head(&decl, 8);
            let flat = super::call_shape::flat_head(&head, false);
            let clause = re!(r"\b(?:extends|implements)\b([\s\S]*)$").captures(&flat).map(|m| m[1].to_string())
                .or_else(|| re!(r"\b(?:class|interface|struct|record|object)\s+\w+[^:]*:([\s\S]*)$").captures(&flat).map(|m| m[1].to_string())).unwrap_or_default();
            let clause = clause.split("where").next().unwrap_or("");
            for part in re!(r",|\bimplements\b").split(clause) {
                let Some(sup) = re!(r"([A-Z]\w*)\s*$").captures(part).map(|m| m[1].to_string()) else { continue };
                let at = Self::cached_regex(&format!(r"(?:[:,]|\bextends|\bimplements)\s*(?:[\w.]+\.)?{}\s*<", regex::escape(&sup)))?.find(&head).map(|m| m.start());
                let given: Vec<String> = at.map(|at| angle_arguments(&head[at..])).unwrap_or_default().iter().map(|a| {
                    let simple = a.split('<').next().unwrap_or("").rsplit('.').next().unwrap_or("").trim(); args.get(simple).cloned().unwrap_or_else(|| simple.to_string())
                }).collect();
                for parent in self.nodes_by_name(&sup)?.iter().filter(|n| n.language == r.language && class_kind(&n.kind)) {
                    let head = self.declaration_head(parent, 3);
                    let at = Self::cached_regex(&format!(r"\b{}\s*<", regex::escape(&parent.name)))?.find(&head).map(|m| m.start());
                    let params = at.map(|at| angle_arguments(&head[at..])).unwrap_or_default();
                    let mappings = params.iter().enumerate().filter_map(|(i, p)| {
                        let p = re!(r"^(?:in|out|reified)\s+").replace(p, "");
                        let name = re!(r"^([A-Za-z_]\w*)").captures(&p).map(|m| m[1].to_string())?;
                        given.get(i).filter(|a| !a.is_empty()).map(|a| (name, a.clone()))
                    }).collect();
                    queue.push_back((parent.clone(), mappings));
                }
            }
        }
        Ok(None)
    }
    pub(super) fn csharp_using_alias(&mut self, name: &str, file: &str) -> Option<String> {
        if !self.csharp_alias_memo.contains_key(file) {
            let text = self.read_file(file).map(|s| super::awaited::strip_ts_comments(s.text())).unwrap_or_default();
            let aliases = re!(r"(?m)^\s*(?:global\s+)?using\s+([A-Za-z_]\w*)\s*=\s*(?:global::)?([\w.]+)\s*(?:<[^;>]*>)?\s*;")
                .captures_iter(&text).map(|m| (m[1].to_string(), m[2].to_string())).collect();
            self.csharp_alias_memo.insert(file.to_string(), Rc::new(aliases));
        }
        self.csharp_alias_memo[file].get(name).cloned()
    }
    pub(super) fn inferred_member_type_bound(&mut self, ty: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if !matches!(r.language.as_str(), "java" | "kotlin" | "csharp") { return Ok(Some(ty.to_string())); }
        let simple = ty.rsplit('.').next().unwrap_or(ty);
        if simple.starts_with(|c: char| c.is_ascii_lowercase()) { return Ok(None); }
        if !self.nodes_by_name(simple)?.iter().any(|n| class_kind(&n.kind)) {
            let nodes = self.nodes_in_file(&r.file_path)?;
            let mut scopes: Vec<_> = nodes.iter().filter(|n| n.start_line <= r.line && n.end_line >= r.line
                && (class_kind(&n.kind) || matches!(n.kind.as_str(), "method" | "function"))).collect();
            scopes.sort_by_key(|n| (n.end_line - n.start_line, -n.start_line));
            for scope in scopes {
                if let Some(bound) = self.declared_parameter_bound(simple, scope)? { return Ok(bound); }
            }
        }
        Ok(Some(ty.to_string()))
    }
    pub(super) fn enum_constant_call(&mut self, receiver: &str, method: &str, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some((root, constant)) = receiver.split_once('.') else { return Ok(None) };
        if constant.contains('.') { return Ok(None); }
        let Some(ty) = self.resolve_bound_type(root, r, 0)? else { return Ok(None) };
        if ty.kind != "enum" { return Ok(None); }
        if !self.nodes_by_name(constant)?.iter().any(|n| n.kind == "enum_member" && n.file_path == ty.file_path && n.qualified_name == format!("{}::{constant}", ty.qualified_name)) { return Ok(None); }
        self.match_bound_type_member(root, method, r)
    }
}
