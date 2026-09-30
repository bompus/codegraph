//! Implicit Kotlin receivers: type bodies, extensions, anonymous objects and DSL lambdas.
use super::*;
use super::call_shape::flat_head;

pub(crate) struct KotlinFrame { start: i64, end: i64, names: Vec<String> }

fn head_names(head: &str) -> Vec<String> {
    if re!(r"\bcompanion\s+object(?:\s+\w+)?\s*$").is_match(head) { return vec!["Companion".to_string()]; }
    let flat = flat_head(head, false);
    if let Some(m) = re!(r"\b(?:class|interface|object)\b(?:\s+([A-Za-z_]\w*))?([^=]*)$").captures(&flat) {
        let mut names = m.get(1).map(|n| vec![n.as_str().to_string()]).unwrap_or_default();
        let tail = m.get(2).map(|v| v.as_str()).unwrap_or("");
        if let Some((_, supers)) = tail.split_once(':') {
            for part in supers.split("where").next().unwrap_or("").split(',') {
                if let Some(m) = re!(r"(?:^|\.)([A-Z]\w*)\s*(?:by\b.*)?$").captures(part.trim()) { names.push(m[1].to_string()); }
            }
        }
        return names;
    }
    re!(r"\bfun\s+(?:<[^>]*>\s*)?([A-Z]\w*)(?:<[^>]*>)?\??\.[A-Za-z_]\w*\s*\(")
        .captures(head).map(|m| vec![m[1].to_string()]).unwrap_or_default()
}
fn platform_supers(name: &str) -> &'static [&'static str] {
    match name {
        "AppCompatActivity" => &["FragmentActivity"], "FragmentActivity" => &["ComponentActivity"],
        "ComponentActivity" => &["Activity", "LifecycleOwner", "ViewModelStoreOwner", "SavedStateRegistryOwner"],
        "Activity" => &["ContextThemeWrapper", "ComponentCallbacks2"], "ContextThemeWrapper" => &["ContextWrapper"],
        "ContextWrapper" => &["Context"], "Application" | "Service" => &["ContextWrapper", "ComponentCallbacks2"],
        "ComponentCallbacks2" => &["ComponentCallbacks"],
        "Fragment" => &["ComponentCallbacks", "LifecycleOwner", "ViewModelStoreOwner", "SavedStateRegistryOwner"],
        "DialogFragment" => &["Fragment"], "AppCompatDialogFragment" => &["DialogFragment"],
        "BottomSheetDialogFragment" => &["AppCompatDialogFragment"], "AndroidViewModel" => &["ViewModel"], _ => &[],
    }
}
impl KernelResolver {
    fn kotlin_frames(&mut self, file: &str) -> Rc<Vec<KotlinFrame>> {
        if let Some(hit) = self.kotlin_frames_memo.get(file) { return hit.clone(); }
        let source = self.read_file(file).map(|s| super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(s.text()))).unwrap_or_default();
        let mut frames = Vec::new();
        let mut stack = Vec::new();
        let mut pending = String::new();
        let mut line = 1;
        for ch in source.chars() {
            if ch == '\n' { line += 1; }
            match ch {
                '{' => {
                    let names = if re!(r"(?:\bwith\s*\([^{}]*\)|\.\s*(?:apply|run)(?:\s*<[^<>]*>)?)\s*$").is_match(&pending) {
                        vec!["*".to_string()]
                    } else { head_names(&pending) };
                    stack.push((line, names)); pending.clear();
                }
                '}' => {
                    if let Some((start, names)) = stack.pop() { if !names.is_empty() { frames.push(KotlinFrame { start, end: line, names }); } }
                    pending.clear();
                }
                ';' => pending.clear(),
                _ => { pending.push(ch); if pending.len() > 600 { let at = pending.char_indices().nth(300).map(|(i, _)| i).unwrap_or(0); pending.drain(..at); } }
            }
        }
        let frames = Rc::new(frames);
        self.kotlin_frames_memo.insert(file.to_string(), frames.clone()); frames
    }
    fn kotlin_receiver_types(&mut self) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = &self.kotlin_receiver_types_memo { return Ok(hit.clone()); }
        let mut names = HashSet::new();
        let mut outside = HashSet::new();
        let files = match self.sorted_files() {
            Some(files) => files,
            None => {
                let mut files: Vec<_> = self.table()?.files.iter().cloned().collect();
                files.sort(); Arc::new(files)
            }
        };
        {
            for file in files.iter().filter(|f| f.ends_with(".kt") || f.ends_with(".kts")) {
                let Some(source) = self.read_file(file) else { continue };
                let text = super::awaited::strip_ts_comments(source.text());
                for sam in re!(r"\bfun\s+interface\s+\w+[^{}]*\{([^{}]*)\}").captures_iter(&text) {
                    for receiver in re!(r"\bfun\s+([A-Z]\w*)(?:<[^<>]*>)?\.\w+\s*\(").captures_iter(&sam[1]) {
                        names.insert(receiver[1].to_string());
                    }
                }
                for m in re!(r"\b([A-Z]\w*)(?:<[^<>()]*(?:<[^<>()]*>[^<>()]*)*>)?\s*\.\s*\(").captures_iter(&text) { names.insert(m[1].to_string()); }
                for m in re!(r"\bfun\s+(?:<[^>]*>\s*)?([A-Z]\w*(?:\.[A-Z]\w*)*)(?:<[^<>()]*(?:<[^<>()]*>[^<>()]*)*>)?\??\.[A-Za-z_`][\w`]*\s*\(").captures_iter(&text) {
                    outside.extend(m[1].split('.').map(str::to_string));
                }
            }
        }
        for name in outside {
            if !self.nodes_by_name(&name)?.iter().any(|n| matches!(n.language.as_str(), "kotlin" | "java") && matches!(n.kind.as_str(), "class" | "interface" | "enum" | "struct" | "trait")) { names.insert(name); }
        }
        let mut queue: VecDeque<String> = names.iter().cloned().collect();
        while names.len() < 5000 {
            let Some(name) = queue.pop_front() else { break };
            for sup in self.kotlin_supertypes_of(&name)?.iter() { if names.insert(sup.clone()) { queue.push_back(sup.clone()); } }
        }
        let names = Rc::new(names); self.kotlin_receiver_types_memo = Some(names.clone()); Ok(names)
    }
    fn kotlin_precondition(&mut self, r: &ResolveRefIn) -> bool {
        if !matches!(r.reference_name.as_str(), "require" | "check" | "assert") { return false; }
        let Some(lines) = self.read_file(&r.file_path) else { return false };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return false };
        let Ok(re) = Self::cached_regex(&format!(r"(?:^|[^\w.]){}\s*\(", regex::escape(&r.reference_name))) else { return false };
        let Some(at) = re.find(line) else { return false };
        let tail = &line[at.end()..];
        let mut depth = 0usize;
        let mut end = tail.len();
        for (i, ch) in tail.char_indices() {
            if ch == '(' { depth += 1; }
            else if ch == ')' { if depth == 0 { end = i; break; } depth -= 1; }
        }
        re!(r"[<>=!]=|&&|\|\||(?:^|[\s(])!|\s[<>]\s|\bis\b|\bin\b|\btrue\b|\bfalse\b").is_match(&tail[..end])
            || tail.get(end + 1..).is_some_and(|rest| rest.trim_start().starts_with('{'))
    }
    pub(super) fn kotlin_member_reachable(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.kind != "method" || !matches!(n.language.as_str(), "kotlin" | "java") { return Ok(true); }
        if self.kotlin_precondition(r) { return Ok(false); }
        // Top-level extensions use package imports; their receiver is not an enclosing class.
        if n.language == "kotlin" && !self.nodes_in_file(&n.file_path)?.iter().any(|o| {
            o.id != n.id && matches!(o.kind.as_str(), "class" | "interface" | "object" | "enum" | "struct" | "trait")
                && o.start_line <= n.start_line && o.end_line >= n.end_line
        }) { return Ok(true); }
        if r.file_path.ends_with(".kts") { return Ok(true); }
        let Some((path, _)) = n.qualified_name.rsplit_once("::") else { return Ok(true) };
        let mut parts = path.rsplit([':', '.']).filter(|v| !v.is_empty());
        let mut owner = parts.next().unwrap_or("");
        let companion = owner == "Companion";
        if companion { owner = parts.next().unwrap_or(owner); }
        if n.name == owner { return Ok(true); }
        let mut queue: VecDeque<String> = self.nodes_in_file(&r.file_path)?.iter().filter(|t| t.start_line <= r.line && t.end_line >= r.line)
            .filter_map(|t| if matches!(t.kind.as_str(), "class" | "interface" | "enum" | "struct" | "trait") { Some(t.name.clone()) }
                else if matches!(t.kind.as_str(), "function" | "method") { t.qualified_name.rsplit_once("::").map(|(p, _)| p.rsplit([':', '.']).next().unwrap_or("").to_string()) } else { None }).collect();
        for frame in self.kotlin_frames(&r.file_path).iter().filter(|f| f.start <= r.line && f.end >= r.line) { queue.extend(frame.names.iter().cloned()); }
        if self.kotlin_receiver_types()?.contains(owner) { return Ok(true); }
        let mut seen = HashSet::new();
        while seen.len() < 60 {
            let Some(name) = queue.pop_front() else { break };
            if !seen.insert(name.clone()) { continue; }
            if name == owner || name == "*" { return Ok(true); }
            queue.extend(platform_supers(&name).iter().map(|s| (*s).to_string()));
            queue.extend(self.kotlin_supertypes_of(&name)?.iter().cloned());
        }
        let pkg = self.kotlin_file_scope(&n.file_path).pkg.clone();
        let object = format!("{}{}{}", if pkg.is_empty() { String::new() } else { format!("{pkg}.") }, owner, if companion { ".Companion" } else { "" });
        let source_companion = self.kotlin_frames(&n.file_path).iter().any(|f| f.start <= n.start_line && f.end >= n.end_line && f.names.iter().any(|name| name == "Companion"));
        let here = self.kotlin_file_scope(&r.file_path);
        Ok(here.imports.contains(&format!("{object}.{}", n.name)) || here.stars.contains(&object)
            || (!companion && source_companion && (here.imports.contains(&format!("{object}.Companion.{}", n.name)) || here.stars.contains(&format!("{object}.Companion")))))
    }
}
