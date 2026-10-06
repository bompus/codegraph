use super::*;

#[derive(Clone, Debug)]
pub(in crate::resolve) struct VbType {
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) interface_spelling: bool,
    pub(in crate::resolve) qualifier: Vec<String>,
    pub(in crate::resolve) args: Vec<VbType>,
    pub(in crate::resolve) array: bool,
    pub(in crate::resolve) file: String,
    pub(in crate::resolve) line: i64,
}
#[derive(Clone)]
pub(in crate::resolve) struct VbAncestor {
    pub(in crate::resolve) node: Arc<KNode>,
    pub(in crate::resolve) args: HashMap<String, VbType>,
}
#[derive(Clone, Default)]
pub(in crate::resolve) struct VbHeader {
    pub(in crate::resolve) root: Vec<String>,
    pub(in crate::resolve) imports: Vec<String>,
    pub(in crate::resolve) aliases: HashMap<String, VbType>,
}
#[derive(Default)]
pub(in crate::resolve) struct VbMemo {
    pub(in crate::resolve) code: HashMap<String, Rc<Vec<String>>>,
    pub(in crate::resolve) projects: HashMap<String, Option<String>>,
    pub(in crate::resolve) headers: HashMap<String, Rc<VbHeader>>,
}
#[derive(Clone)]
pub(in crate::resolve) enum VbBinding {
    Type(VbType),
    Call(Option<String>, String),
    Each(String),
    Unknown,
}
#[derive(Clone)]
pub(in crate::resolve) struct VbMember {
    pub(in crate::resolve) node: Arc<KNode>,
    pub(in crate::resolve) args: HashMap<String, VbType>,
}
pub(in crate::resolve) struct VbHit {
    pub(in crate::resolve) candidate: KCand,
    pub(in crate::resolve) edge_kind: Option<String>,
    pub(in crate::resolve) also: Vec<String>,
}

pub(in crate::resolve) type VbTypes = (Vec<Arc<KNode>>, bool); // owners, ambiguous
pub(in crate::resolve) fn vb_type_kind(k: &str) -> bool {
    matches!(k, "class" | "struct" | "interface")
}
pub(in crate::resolve) fn vb_like_kind(k: &str) -> bool {
    vb_type_kind(k) || matches!(k, "enum" | "type_alias")
}
pub(in crate::resolve) fn vb_value_kind(k: &str) -> bool {
    matches!(k, "field" | "property" | "constant" | "variable")
}
pub(in crate::resolve) fn vb_member_kind(k: &str) -> bool {
    vb_value_kind(k) || matches!(k, "method" | "enum_member")
}
pub(in crate::resolve) fn vb_key(t: &VbType) -> String {
    let lower = t.name.to_ascii_lowercase();
    let name = match lower.as_str() {
        "int16" => "short",
        "int32" => "integer",
        "int64" => "long",
        "uint16" => "ushort",
        "uint32" => "uinteger",
        "uint64" => "ulong",
        "datetime" => "date",
        _ => &lower,
    };
    format!("{}{}", name, if t.array { "[]" } else { "" })
}
pub(in crate::resolve) fn vb_builtin(t: &VbType) -> bool {
    !t.array
        && matches!(
            vb_key(t).as_str(),
            "boolean"
                | "byte"
                | "char"
                | "date"
                | "decimal"
                | "double"
                | "integer"
                | "long"
                | "object"
                | "sbyte"
                | "short"
                | "single"
                | "string"
                | "uinteger"
                | "ulong"
                | "ushort"
        )
}
pub(in crate::resolve) fn vb_close(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, b) in text.as_bytes().iter().enumerate().skip(open) {
        if *b == b'(' {
            depth += 1;
        } else if *b == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}
pub(in crate::resolve) fn vb_args(text: &str) -> Vec<String> {
    let mut depth = 0;
    let mut start = 0;
    let mut out = Vec::new();
    for (i, ch) in text.char_indices() {
        match ch {
            '(' | '{' => depth += 1,
            ')' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(text[start..].trim().to_string());
    out
}
pub(in crate::resolve) fn vb_type(raw: &str, file: &str, line: i64) -> Option<VbType> {
    let head = re!(r"^\s*([A-Za-z_][A-Za-z0-9_.]*)").captures(raw)?;
    let mut parts: Vec<String> = head[1].split('.').map(str::to_ascii_lowercase).collect();
    if parts.first().is_some_and(|p| p == "global") {
        parts.remove(0);
    }
    let name = parts.pop()?;
    if matches!(
        name.as_str(),
        "new" | "of" | "as" | "in" | "out" | "from" | "with"
    ) {
        return None;
    }
    let mut at = head.get(0)?.end();
    let mut args = Vec::new();
    if re!(r"(?i)^\s*\(\s*Of\b").is_match(&raw[at..]) {
        let open = at + raw[at..].find('(')?;
        let close = vb_close(raw, open)?;
        for arg in vb_args(&raw[open + 1..close]) {
            let arg = re!(r"(?i)^Of\s+").replace(&arg, "");
            args.push(vb_type(&arg, file, line).unwrap_or(VbType {
                name: "?".into(),
                interface_spelling: false,
                qualifier: vec![],
                args: vec![],
                array: false,
                file: file.into(),
                line,
            }));
        }
        at = close + 1;
    }
    Some(VbType {
        name,
        interface_spelling: head[1]
            .rsplit('.')
            .next()
            .is_some_and(vb_interface_spelling),
        qualifier: parts,
        args,
        array: re!(r"^\s*\??\s*\(\s*,*\s*\)").is_match(&raw[at..]),
        file: file.into(),
        line,
    })
}
pub(in crate::resolve) fn vb_interface_spelling(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next() == Some('I') && chars.next().is_some_and(|c| c.is_ascii_uppercase())
}
pub(in crate::resolve) fn vb_simple(name: &str, r: &ResolveRefIn) -> VbType {
    VbType {
        name: name.to_ascii_lowercase(),
        interface_spelling: vb_interface_spelling(name),
        qualifier: vec![],
        args: vec![],
        array: false,
        file: r.file_path.clone(),
        line: r.line,
    }
}
pub(in crate::resolve) fn vb_substitute(t: &VbType, args: &HashMap<String, VbType>) -> VbType {
    if !t.array && t.qualifier.is_empty() {
        if let Some(t) = args.get(&t.name.to_ascii_lowercase()) {
            return t.clone();
        }
    }
    t.clone()
}
pub(in crate::resolve) fn vb_clean(raw: &str) -> String {
    let mut chars = raw.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '‘' | '’' | '\r' => break,
            '"' => {
                out.push_str("\"\"");
                while let Some(c) = chars.next() {
                    if c == '"' {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}
pub(in crate::resolve) fn vb_segments(qn: &str) -> Vec<String> {
    qn.replace("::", ".")
        .split('.')
        .map(str::to_ascii_lowercase)
        .collect()
}
pub(in crate::resolve) fn vb_parent_qn(n: &KNode) -> Option<&str> {
    n.qualified_name.rsplit_once("::").map(|(owner, _)| owner)
}
pub(in crate::resolve) fn vb_tail(full: &[String], tail: &[String]) -> bool {
    full.len() >= tail.len() && full[full.len() - tail.len()..] == *tail
}

impl KernelResolver {
    pub(in crate::resolve) fn vb_lines(&mut self, file: &str) -> Rc<Vec<String>> {
        if let Some(hit) = self.vb.code.get(file) {
            return hit.clone();
        }
        let out = Rc::new(
            self.read_file(file)
                .map_or_else(Vec::new, |f| f.iter().map(|l| vb_clean(l)).collect()),
        );
        if self.vb.code.len() >= 1024 {
            self.vb.code.clear();
        }
        self.vb.code.insert(file.into(), out.clone());
        out
    }
    pub(in crate::resolve) fn vb_project(&mut self, file: &str) -> Option<String> {
        let mut dir = pos_dirname(file).to_string();
        if dir == "." {
            dir.clear();
        }
        let mut walked = Vec::new();
        let answer = loop {
            if let Some(hit) = self.vb.projects.get(&dir) {
                break hit.clone();
            }
            walked.push(dir.clone());
            let abs = std::path::Path::new(&self.project_root).join(&dir);
            let found = std::fs::read_dir(abs)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok())
                .any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(".vbproj")
                });
            if found {
                break Some(dir.clone());
            }
            if dir.is_empty() {
                break None;
            }
            dir = dir
                .rsplit_once('/')
                .map_or(String::new(), |(p, _)| p.to_string());
        };
        if self.vb.projects.len() >= 4096 {
            self.vb.projects.clear();
        }
        for d in walked {
            self.vb.projects.insert(d, answer.clone());
        }
        answer
    }
    pub(in crate::resolve) fn vb_same_project(&mut self, a: &str, b: &str) -> bool {
        let project = self.vb_project(a);
        project.is_some() && project == self.vb_project(b)
    }
    pub(in crate::resolve) fn vb_header(&mut self, file: &str) -> Rc<VbHeader> {
        if let Some(hit) = self.vb.headers.get(file) {
            return hit.clone();
        }
        let mut h = VbHeader::default();
        if let Some(dir) = self.vb_project(file) {
            let abs = std::path::Path::new(&self.project_root).join(dir);
            let project = std::fs::read_dir(&abs)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok())
                .find(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(".vbproj")
                });
            if let Some(project) = project {
                let project_path = project.path();
                let relative = project_path.strip_prefix(&self.project_root).ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"));
                if let Some(source) = relative.and_then(|p| self.read_file(&p)) {
                    let text = source.text();
                    let root = re!(r"(?i)<RootNamespace>\s*([\w.]*)\s*</RootNamespace>")
                        .captures(text)
                        .map(|m| m[1].to_string())
                        .unwrap_or_else(|| {
                            project
                                .file_name()
                                .to_string_lossy()
                                .trim_end_matches(".vbproj")
                                .into()
                        });
                    h.root = root
                        .split('.')
                        .filter(|s| !s.is_empty())
                        .map(str::to_ascii_lowercase)
                        .collect();
                    for m in re!(r#"(?i)<Import\s+Include\s*=\s*"([\w.]+)""#).captures_iter(text) {
                        h.imports.push(m[1].to_ascii_lowercase());
                    }
                }
            }
        }
        for l in self.vb_lines(file).iter() {
            if re!(r"(?i)^\s*(?:Namespace|Module|Class|Structure|Interface|Enum|Delegate|Public|Friend|Private|Protected|Partial|NotInheritable|MustInherit)\b").is_match(l){break;}
            if let Some(m) = re!(r"(?i)^\s*Imports\s+([A-Za-z_]\w*)\s*=\s*(.+)$").captures(l) {
                if let Some(t) = vb_type(&m[2], file, 0) {
                    h.aliases.insert(m[1].to_ascii_lowercase(), t);
                }
            } else if let Some(m) = re!(r"(?i)^\s*Imports\s+([\w.]+)\s*$").captures(l) {
                h.imports.push(
                    m[1].to_ascii_lowercase()
                        .trim_start_matches("global.")
                        .into(),
                );
            }
        }
        let out = Rc::new(h);
        if self.vb.headers.len() >= 1024 {
            self.vb.headers.clear();
        }
        self.vb.headers.insert(file.into(), out.clone());
        out
    }
    pub(in crate::resolve) fn vb_unalias(&mut self, t: &VbType) -> VbType {
        let h = self.vb_header(&t.file);
        let mut out = t.clone();
        if t.qualifier.is_empty() {
            if let Some(alias) = h.aliases.get(&t.name) {
                out.name = alias.name.clone();
                out.qualifier = alias.qualifier.clone();
            }
        } else if let Some(alias) = h.aliases.get(&t.qualifier[0]) {
            out.qualifier = alias.qualifier.clone();
            out.qualifier.push(alias.name.clone());
            out.qualifier.extend_from_slice(&t.qualifier[1..]);
        }
        out
    }
    pub(in crate::resolve) fn vb_full(&mut self, n: &KNode) -> Vec<String> {
        let mut full = self.vb_header(&n.file_path).root.clone();
        full.extend(vb_segments(&n.qualified_name));
        full
    }
    pub(in crate::resolve) fn vb_around(&self, file: &str, line: i64) -> Res<Vec<Arc<KNode>>> {
        let mut out: Vec<_> = self
            .nodes_in_file(file)?
            .iter()
            .filter(|n| {
                n.language == "vbnet"
                    && vb_type_kind(&n.kind)
                    && n.start_line <= line
                    && n.end_line >= line
            })
            .cloned()
            .collect();
        out.sort_by_key(|n| std::cmp::Reverse(n.start_line));
        Ok(out)
    }
    pub(in crate::resolve) fn vb_module(&mut self, n: &KNode) -> bool {
        n.kind == "class"
            && self
                .vb_lines(&n.file_path)
                .iter()
                .skip((n.start_line - 1).max(0) as usize)
                .take(3)
                .any(|l| {
                    re!(r"(?i)^\s*(?:<[^>]*>\s*)*(?:(?:Public|Friend|Private|Partial)\s+)*Module\s")
                        .is_match(l)
                })
    }
    pub(in crate::resolve) fn vb_enclosing(&mut self, n: &KNode) -> Res<Option<Arc<KNode>>> {
        let Some(qn) = vb_parent_qn(n) else {
            return Ok(None);
        };
        for p in self.nodes_by_qualified_name(qn)?.iter() {
            if p.language == "vbnet" && vb_type_kind(&p.kind)
                && self.vb_declaration_project(p, n) && !self.vb_module(p) {
                return Ok(Some(p.clone()));
            }
        }
        Ok(None)
    }
    pub(in crate::resolve) fn vb_supers(&mut self, n: &KNode) -> Res<Vec<(VbType, bool)>> {
        let parts: Vec<_> = self
            .nodes_by_qualified_name(&n.qualified_name)?
            .iter()
            .filter(|p| p.language == "vbnet" && vb_type_kind(&p.kind))
            .cloned()
            .collect();
        let mut out = Vec::new();
        for part in parts {
            if !self.vb_declaration_project(&part, n) { continue; }
            let lines = self.vb_lines(&part.file_path);
            'scan: for l in lines
                .iter()
                .skip((part.start_line - 1).max(0) as usize)
                .take((part.end_line - part.start_line + 1).clamp(0, 13) as usize)
            {
                for statement in l.split(':') {
                    let s = statement.trim();
                    if s.is_empty()
                        || s.starts_with('#')
                        || re!(r"^<.*>$").is_match(s)
                        || re!(r"(?i)\b(?:Class|Structure|Interface|Module)\s+[A-Za-z_]")
                            .is_match(s)
                    {
                        continue;
                    }
                    let Some(m) = re!(r"(?i)^(Inherits|Implements)\s+(.+)$").captures(s) else {
                        break 'scan;
                    };
                    for raw in vb_args(&m[2]) {
                        if let Some(t) = vb_type(&raw, &part.file_path, part.start_line) {
                            out.push((t, m[1].eq_ignore_ascii_case("Implements")));
                        }
                    }
                }
            }
        }
        Ok(out)
    }
    pub(in crate::resolve) fn vb_base_names(&mut self, file: &str, line: i64) -> Res<HashSet<String>> {
        let mut names = HashSet::new();
        let mut queue = self.vb_around(file, line)?;
        for _ in 0..5 {
            let mut next = Vec::new();
            for n in queue {
                if !names.insert(n.name.to_ascii_lowercase()) {
                    continue;
                }
                for (sup, implemented) in self.vb_supers(&n)? {
                    if !implemented {
                        next.extend(
                            self.nodes_by_lower_name(&sup.name)?
                                .iter()
                                .filter(|n| n.language == "vbnet" && vb_type_kind(&n.kind))
                                .cloned(),
                        );
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            queue = next;
        }
        Ok(names)
    }
    pub(in crate::resolve) fn vb_types_at(&mut self, written: &VbType, with_enum: bool) -> Res<VbTypes> {
        let t = self.vb_unalias(written);
        let mut candidates = Vec::new();
        for n in self.nodes_by_lower_name(&t.name)?.iter() {
            if n.language != "vbnet" || !(vb_type_kind(&n.kind) || (with_enum && n.kind == "enum"))
            {
                continue;
            }
            let full = self.vb_full(n);
            if t.qualifier.is_empty() || vb_tail(&full[..full.len() - 1], &t.qualifier) {
                candidates.push(n.clone());
            }
        }
        let around = self.vb_around(&t.file, t.line)?;
        let site = around.first();
        let site_path = match site {
            Some(n) => self.vb_full(n),
            None => self.vb_header(&t.file).root.clone(),
        };
        let mut tier = Vec::new();
        let mut best = None;
        for n in &candidates {
            let full = self.vb_full(n);
            let container = &full[..full.len() - 1];
            if site_path.starts_with(container) {
                if best.is_none_or(|b| container.len() > b) {
                    tier.clear();
                    best = Some(container.len());
                }
                if best == Some(container.len()) {
                    tier.push(n.clone());
                }
            }
        }
        if tier.is_empty() && t.qualifier.is_empty() {
            let bases = self.vb_base_names(&t.file, t.line)?;
            for n in &candidates {
                if self
                    .vb_enclosing(n)?
                    .is_some_and(|p| bases.contains(&p.name.to_ascii_lowercase()))
                {
                    tier.push(n.clone());
                }
            }
        }
        if tier.is_empty() {
            let imports = self.vb_header(&t.file).imports.clone();
            for n in &candidates {
                let full = self.vb_full(n);
                if imports.contains(&full[..full.len() - 1].join(".")) {
                    tier.push(n.clone());
                }
            }
        }
        if tier.is_empty() {
            for n in &candidates {
                if !t.qualifier.is_empty() || self.vb_enclosing(n)?.is_none() {
                    tier.push(n.clone());
                }
            }
        }
        let mut own = Vec::new();
        for n in &tier {
            if self.vb_same_project(&n.file_path, &t.file) {
                own.push(n.clone());
            }
        }
        if !own.is_empty() {
            tier = own;
        }
        let mut distinct = HashSet::new();
        for n in &tier {
            distinct.insert((
                self.vb_project(&n.file_path),
                n.qualified_name.to_ascii_lowercase(),
            ));
        }
        tier.sort_by_key(|n| n.file_path != t.file);
        Ok((tier, distinct.len() > 1))
    }
    pub(in crate::resolve) fn vb_params(&mut self, n: &KNode) -> Res<Vec<(String, Option<VbType>)>> {
        let code = self
            .vb_lines(&n.file_path)
            .iter()
            .skip((n.start_line - 1).max(0) as usize)
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let re = Self::cached_regex(&format!(
            r"(?i)\b(?:Class|Structure|Interface|Module|Function|Sub)\s+{}\s*",
            regex::escape(&n.name)
        ))?;
        let Some(m) = re.find(&code) else {
            return Ok(vec![]);
        };
        let rest = &code[m.end()..];
        if !re!(r"(?i)^\(\s*Of\b").is_match(rest) {
            return Ok(vec![]);
        }
        let Some(close) = vb_close(rest, 0) else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for p in vb_args(&rest[1..close]) {
            if let Some(m) =
                re!(r"(?i)^(?:Of\s+)?(?:In\s+|Out\s+)?([A-Za-z_]\w*)(?:\s+As\s+(.+))?$")
                    .captures(&p)
            {
                let raw = m
                    .get(2)
                    .map(|m| m.as_str())
                    .unwrap_or("")
                    .trim_matches(['{', '}']);
                let constraint = raw
                    .split(',')
                    .map(str::trim)
                    .find(|s| !s.is_empty() && !re!(r"(?i)^(New|Class|Structure)$").is_match(s))
                    .and_then(|s| vb_type(s, &n.file_path, n.start_line));
                out.push((m[1].to_ascii_lowercase(), constraint));
            }
        }
        Ok(out)
    }
    pub(in crate::resolve) fn vb_ancestry(&mut self, n: &Arc<KNode>) -> Res<Vec<VbAncestor>> {
        let mut queue = VecDeque::from([VbAncestor {
            node: n.clone(),
            args: HashMap::new(),
        }]);
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        while let Some(current) = queue.pop_front() {
            if out.len() >= 16 {
                break;
            }
            if !seen.insert(current.node.qualified_name.to_ascii_lowercase()) {
                continue;
            }
            let supers = self.vb_supers(&current.node)?;
            for (sup, implemented) in supers {
                if implemented {
                    continue;
                }
                let (owners, ambiguous) = self.vb_types_at(&sup, false)?;
                if ambiguous {
                    continue;
                }
                for base in owners {
                    let params = self.vb_params(&base)?;
                    let args = params
                        .iter()
                        .enumerate()
                        .filter_map(|(i, (p, _))| {
                            sup.args
                                .get(i)
                                .map(|t| (p.clone(), vb_substitute(t, &current.args)))
                        })
                        .collect();
                    queue.push_back(VbAncestor { node: base, args });
                }
            }
            out.push(current);
        }
        Ok(out)
    }
    fn vb_declaration_project(&mut self, part: &KNode, owner: &KNode) -> bool {
        part.file_path == owner.file_path
            || self.vb_project(&part.file_path) == self.vb_project(&owner.file_path)
    }
    pub(in crate::resolve) fn vb_members(&mut self, owner: &KNode, name: &str) -> Res<Vec<Arc<KNode>>> {
        let qn = format!("{}::{}", owner.qualified_name, name).to_ascii_lowercase();
        let candidates: Vec<_> = self.nodes_by_lower_name(name)?.iter()
            .filter(|n| n.language == "vbnet" && n.qualified_name.to_ascii_lowercase() == qn)
            .cloned().collect();
        Ok(candidates.into_iter().filter(|n| self.vb_declaration_project(n, owner)).collect())
    }
    pub(in crate::resolve) fn vb_prefer(
        &mut self,
        nodes: Vec<Arc<KNode>>,
        r: &ResolveRefIn,
    ) -> Vec<Arc<KNode>> {
        let mut same = Vec::new();
        let mut own = Vec::new();
        let mut rest = Vec::new();
        for n in nodes {
            if n.file_path == r.file_path {
                same.push(n);
            } else if self.vb_same_project(&n.file_path, &r.file_path) {
                own.push(n);
            } else {
                rest.push(n);
            }
        }
        same.extend(own);
        same.extend(rest);
        same
    }
    pub(in crate::resolve) fn vb_member_on(
        &mut self,
        owners: &[Arc<KNode>],
        name: &str,
        r: &ResolveRefIn,
        values: bool,
        typed: Option<&VbType>,
        reads: bool,
    ) -> Res<Option<VbMember>> {
        for level in [0, 1] {
            for owner in self.vb_prefer(owners.to_vec(), r) {
                let given: HashMap<_, _> = self
                    .vb_params(&owner)?
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (p, _))| {
                        typed
                            .and_then(|t| t.args.get(i))
                            .map(|t| (p.clone(), t.clone()))
                    })
                    .collect();
                for (i, ancestor) in self.vb_ancestry(&owner)?.into_iter().enumerate() {
                    if (level == 0) != (i == 0) {
                        continue;
                    }
                    let candidates = self
                        .vb_members(&ancestor.node, name)?
                        .into_iter()
                        .filter(|n| {
                            n.kind == "method"
                                || (values && vb_value_kind(&n.kind))
                                || (reads && (n.kind == "enum_member" || vb_like_kind(&n.kind)))
                        })
                        .filter(|n| !reads || n.id != r.from_node_id)
                        .collect();
                    if let Some(node) = self.vb_prefer(candidates, r).into_iter().next() {
                        let args = if level == 0 {
                            given.clone()
                        } else {
                            ancestor
                                .args
                                .iter()
                                .map(|(p, t)| (p.clone(), vb_substitute(t, &given)))
                                .collect()
                        };
                        return Ok(Some(VbMember { node, args }));
                    }
                }
            }
        }
        Ok(None)
    }
}
