//! Implicit Kotlin receivers: type bodies, extensions, anonymous objects and DSL lambdas.
use super::*;
use super::call_shape::flat_head;

pub(crate) struct KotlinFrame { start: i64, end: i64, names: Vec<String> }

/// The name a head-only frame scan gives a DSL lambda block it did not type; not a Kotlin identifier.
const UNTYPED_LAMBDA: &str = "?lambda";

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
        let frames = Rc::new(self.scan_kotlin_frames(file, true));
        self.kotlin_frames_memo.insert(file.to_string(), frames.clone()); frames
    }
    /// The frames `head_names` and `with`/`apply`/`run` give, without typing DSL lambdas:
    /// a block whose receiver only a lambda lookup could name is kept as an `UNTYPED_LAMBDA` frame.
    fn kotlin_head_frames(&mut self, file: &str) -> Rc<Vec<KotlinFrame>> {
        if let Some(hit) = self.kotlin_head_frames_memo.get(file) { return hit.clone(); }
        let frames = Rc::new(self.scan_kotlin_frames(file, false));
        self.kotlin_head_frames_memo.insert(file.to_string(), frames.clone()); frames
    }
    /// Whether a declaration lies in a companion object body. Companion frames come from
    /// declaration heads alone, so the full frames, whose lambda lookups parse the files
    /// every called DSL function lives in, are built only when a DSL lambda encloses it and
    /// some lambda type could name `Companion` as its receiver.
    fn in_kotlin_companion(&mut self, n: &KNode) -> bool {
        let encloses = |f: &&KotlinFrame| f.start <= n.start_line && f.end >= n.end_line;
        let head = self.kotlin_head_frames(&n.file_path);
        if head.iter().filter(encloses).any(|f| f.names.iter().any(|name| name == "Companion")) { return true; }
        if !head.iter().filter(encloses).any(|f| f.names.iter().any(|name| name == UNTYPED_LAMBDA)) { return false; }
        if !self.kotlin_companion_lambda_receiver() { return false; }
        self.kotlin_frames(&n.file_path).iter().filter(encloses).any(|f| f.names.iter().any(|name| name == "Companion"))
    }
    /// Whether any Kotlin source spells `Companion.(` (type arguments allowed): the only text
    /// from which `kotlin_lambda_type_receiver` can read `Companion` as a lambda receiver,
    /// directly or through a type alias.
    fn kotlin_companion_lambda_receiver(&mut self) -> bool {
        if let Some(hit) = self.kotlin_companion_receiver_memo { return hit; }
        let files = match self.sorted_files() {
            Some(files) => files,
            None => Arc::new(self.table().map(|t| t.files.iter().cloned().collect()).unwrap_or_default()),
        };
        let found = files.iter().filter(|f| f.ends_with(".kt") || f.ends_with(".kts")).any(|file| {
            self.read_file(file).is_some_and(|source| re!(r"Companion\s*(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\s*\.\s*\(").is_match(source.text()))
        });
        self.kotlin_companion_receiver_memo = Some(found); found
    }
    fn scan_kotlin_frames(&mut self, file: &str, type_lambdas: bool) -> Vec<KotlinFrame> {
        let source = self.read_file(file).map(|s| super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(s.text()))).unwrap_or_default();
        let mut frames = Vec::new();
        let mut stack = Vec::new();
        let mut pending = String::new();
        let mut line = 1;
        for ch in source.chars() {
            if ch == '\n' { line += 1; }
            match ch {
                '{' => {
                    let mut names = if re!(r"(?:\bwith\s*\([^{}]*\)|\.\s*(?:apply|run)(?:\s*<[^<>]*>)?)\s*$").is_match(&pending) {
                        vec!["*".to_string()]
                    } else { head_names(&pending) };
                    if names.is_empty() {
                        let call = re!(r"(?:^|[^\w$])([a-z_]\w*)\s*(?:<[^<>{}]*>)?\s*(?:\([^(){}]*\))?\s*$").captures(&pending).map(|m| m[1].to_string());
                        if let Some(call) = call.filter(|name| !matches!(name.as_str(), "if" | "else" | "for" | "while" | "do" | "when" | "try" | "catch" | "finally" | "init" | "get" | "set" | "constructor" | "fun" | "class" | "object" | "interface" | "return" | "by" | "lazy" | "apply" | "run" | "also" | "let" | "with" | "use")) {
                            if !type_lambdas { stack.push((line, vec![UNTYPED_LAMBDA.to_string()])); pending.clear(); continue; }
                            let site = ResolveRefIn { row_id: None, from_node_id: String::new(), reference_name: call.clone(), reference_kind: "calls".to_string(), line, column: 0, candidates: None, file_path: file.to_string(), language: "kotlin".to_string(), failure_reason: None, generated: None };
                            let mut owners: HashSet<String> = stack.iter().flat_map(|(_, names): &(i64, Vec<String>)| names.iter().filter(|name| name.as_str() != "*").cloned()).collect();
                            // An explicit typed receiver also proves dispatch: m.single { ... }, m: Module.
                            // One shared pattern with the callee captured, not a pattern compiled per call name:
                            // the callee before the trailing arguments is the same identifier `call` names.
                            if let Some(receiver) = re!(r"(?:^|[^\w$])([A-Za-z_]\w*)\s*\.\s*([a-z_]\w*)\s*(?:<[^<>{}]*>)?\s*(?:\([^(){}]*\))?\s*$").captures(&pending).filter(|m| m[2] == call) {
                                if let Ok(Some(ty)) = self.infer_local_receiver_type(&receiver[1], &site, true) {
                                    owners.insert(ty.split('<').next().unwrap_or(&ty).trim_end_matches('?').to_string());
                                }
                            }
                            let mut queue: VecDeque<_> = owners.iter().cloned().collect();
                            while owners.len() < 60 {
                                let Some(owner) = queue.pop_front() else { break };
                                if let Ok(supers) = self.kotlin_supertypes_of(&owner) {
                                    for parent in supers.iter() { if owners.insert(parent.clone()) { queue.push_back(parent.clone()); } }
                                }
                            }

                            if let Ok(Some(receiver)) = self.kotlin_lambda_receiver(&call, &site, &owners) { names.push(receiver); }
                        }
                    }
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
        frames
    }
    pub(super) fn lexical_kotlin_members(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        if candidates.len() < 2 { return Ok(candidates); }
        let mut queue: VecDeque<String> = self.nodes_in_file(&r.file_path)?.iter().filter(|n| n.start_line <= r.line && n.end_line >= r.line).filter_map(|n| {
            if super::call_shape::type_kind(&n.kind) { Some(n.name.clone()) }
            else if matches!(n.kind.as_str(), "function" | "method") { super::call_shape::owner(n).map(str::to_string) } else { None }
        }).collect();
        for frame in self.kotlin_frames(&r.file_path).iter().filter(|f| f.start <= r.line && r.line <= f.end) { queue.extend(frame.names.iter().cloned()); }
        let mut hierarchy = HashSet::new();
        while hierarchy.len() < 60 {
            let Some(name) = queue.pop_front() else { break };
            if name == "*" { return Ok(candidates); }
            if !hierarchy.insert(name.clone()) { continue; }
            queue.extend(self.kotlin_supertypes_of(&name)?.iter().cloned());
            queue.extend(platform_supers(&name).iter().map(|s| s.to_string()));
        }
        let lexical: Vec<_> = candidates.iter().filter(|n| n.kind == "method" && n.qualified_name.rsplit_once("::").is_some_and(|(path, _)| {
            let mut owners = path.rsplit("::");
            let owner = owners.next().unwrap_or("");
            hierarchy.contains(if owner == "Companion" { owners.next().unwrap_or("") } else { owner })
        })).cloned().collect();
        Ok(if lexical.is_empty() { candidates } else { lexical })
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
        if r.file_path.ends_with(".kts") && matches!(n.kind.as_str(), "method" | "field" | "property") {
            let Some(owner) = super::call_shape::owner(n) else { return Ok(false) };
            if !self.nodes_by_name(owner)?.iter().any(|c| super::call_shape::type_kind(&c.kind)) { return Ok(true); }
            // Build logic can introduce a project-owned DSL receiver, e.g. testDb { dialects(...) }.
            let mut queue: VecDeque<String> = self.kotlin_frames(&r.file_path).iter().filter(|f| f.start <= r.line && r.line <= f.end).flat_map(|f| f.names.iter().filter(|name| name.as_str() != "*").cloned()).collect();
            let mut seen = HashSet::new();
            while seen.len() < 60 {
                let Some(name) = queue.pop_front() else { break };
                if !seen.insert(name.clone()) { continue; }
                if name == owner { return Ok(true); }
                queue.extend(self.kotlin_supertypes_of(&name)?.iter().cloned());
            }
            return Ok(false);
        }
        if n.kind != "method" || !matches!(n.language.as_str(), "kotlin" | "java") { return Ok(true); }
        if self.kotlin_precondition(r) { return Ok(false); }
        // Top-level extensions use package imports; their receiver is not an enclosing class.
        let dispatch=self.nodes_in_file(&n.file_path)?.iter().filter(|o| {
            o.id != n.id && matches!(o.kind.as_str(), "class" | "interface" | "object" | "enum" | "struct" | "trait")
                && o.start_line <= n.start_line && o.end_line >= n.end_line
        }).min_by_key(|o|o.end_line-o.start_line).cloned();
        if n.language == "kotlin" && dispatch.is_none() { return Ok(true); }
        let Some((path, _)) = n.qualified_name.rsplit_once("::") else { return Ok(true) };
        let mut parts = path.rsplit([':', '.']).filter(|v| !v.is_empty());
        let mut owner = parts.next().unwrap_or("");
        let companion = owner == "Companion";
        if companion { owner = parts.next().unwrap_or(owner); }
        let member_extension=dispatch.as_ref().is_some_and(|decl|decl.name!=owner && !companion);
        if member_extension {owner=&dispatch.as_ref().unwrap().name;}
        if n.name == owner { return Ok(true); }
        let mut queue: VecDeque<String> = self.nodes_in_file(&r.file_path)?.iter().filter(|t| t.start_line <= r.line && t.end_line >= r.line)
            .filter_map(|t| if matches!(t.kind.as_str(), "class" | "interface" | "enum" | "struct" | "trait") { Some(t.name.clone()) }
                else if matches!(t.kind.as_str(), "function" | "method") { t.qualified_name.rsplit_once("::").map(|(p, _)| p.rsplit([':', '.']).next().unwrap_or("").to_string()) } else { None }).collect();
        for frame in self.kotlin_frames(&r.file_path).iter().filter(|f| f.start <= r.line && f.end >= r.line) { queue.extend(frame.names.iter().cloned()); }
        if !member_extension && self.kotlin_receiver_types()?.contains(owner) { return Ok(true); }
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
        let source_companion = self.in_kotlin_companion(n);
        let here = self.kotlin_file_scope(&r.file_path);
        Ok(here.imports.contains(&format!("{object}.{}", n.name)) || here.stars.contains(&object)
            || (!companion && source_companion && (here.imports.contains(&format!("{object}.Companion.{}", n.name)) || here.stars.contains(&format!("{object}.Companion")))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A declaration in a DSL block the head-only scan leaves untyped, in a project whose
    /// source spells `Companion.(`: the check falls back to the full frames, which type the
    /// block by its function's lambda receiver.
    #[test]
    fn companion_check_types_dsl_blocks_when_a_lambda_receiver_names_companion() {
        let dir = std::env::temp_dir().join(format!("cg-kotlin-companion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dsl.kt"), "fun companionScope(block: Companion.() -> Unit) {}\nfun builderScope(block: Builder.() -> Unit) {}\n\nval a = companionScope {\n    fun inCompanion() {}\n}\nval b = builderScope {\n    fun inBuilder() {}\n}\n").unwrap();
        let db = dir.join("graph.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, \
             qualified_name TEXT NOT NULL, file_path TEXT NOT NULL, language TEXT NOT NULL, \
             start_line INTEGER NOT NULL, end_line INTEGER NOT NULL, start_column INTEGER NOT NULL, \
             end_column INTEGER NOT NULL, signature TEXT, visibility TEXT, \
             is_exported INTEGER NOT NULL DEFAULT 0, return_type TEXT, type_parameters TEXT, decorators TEXT, \
             is_async INTEGER NOT NULL DEFAULT 0, is_static INTEGER NOT NULL DEFAULT 0); \
             CREATE TABLE files (path TEXT PRIMARY KEY); \
             INSERT INTO files(path) VALUES ('dsl.kt');",
        ).unwrap();
        for (id, name, line, column) in [("n1", "companionScope", 1, 0), ("n2", "builderScope", 2, 0), ("n3", "inCompanion", 5, 4), ("n4", "inBuilder", 8, 4)] {
            conn.execute(
                "INSERT INTO nodes(id, kind, name, qualified_name, file_path, language, start_line, \
                 end_line, start_column, end_column) VALUES (?1, 'function', ?2, ?2, 'dsl.kt', 'kotlin', ?3, ?3, ?4, ?4)",
                rusqlite::params![id, name, line, column],
            ).unwrap();
        }
        drop(conn);
        let mut resolver = KernelResolver::new(KernelResolverConfig {
            db_path: db.to_string_lossy().into_owned(), project_root: dir.to_string_lossy().into_owned(), aliases: None,
            scoped_aliases: None, workspaces: None, go_modules: None, cpp_include_dirs: None, node_builtin_specifiers: Vec::new(),
            frameworks_active: false, framework_names: None, ambiguous_name_ceiling: None, generation: None,
            supertypes_complete: None, snapshot: Some(true), query_lookups: None,
        }).unwrap();
        let node = |r: &KernelResolver, name: &str| r.nodes_by_name(name).ok().and_then(|l| l.first().cloned()).unwrap();
        let (in_companion, in_builder) = (node(&resolver, "inCompanion"), node(&resolver, "inBuilder"));
        assert!(resolver.in_kotlin_companion(&in_companion));
        assert!(!resolver.in_kotlin_companion(&in_builder));
        drop(resolver);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
