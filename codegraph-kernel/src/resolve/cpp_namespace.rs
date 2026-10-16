//! Namespace ownership supplied by a project's namespace-opening macros.
use super::*;

#[derive(Default)]
pub(super) struct NamespaceCache {
    ready: bool,
    openers: HashMap<(String,String),Vec<String>>,
    functions: HashSet<(String,String)>,
    closers: HashSet<(String,String)>,
    /// File → the namespace aliases it declares at namespace scope: name → (target, line).
    aliases: HashMap<String,HashMap<String,(String,i64)>>,
    /// File → the aliases a function or other block declares: (name, target, first line, last line).
    local_aliases: HashMap<String,Vec<(String,String,i64,i64)>>,
    includes: HashMap<String,HashSet<String>>,
    replacements: HashMap<(String,String),String>,
    frames: HashMap<String,Vec<(i64,i64,Vec<String>)>>,
}
impl KernelResolver {
    fn prepare_namespace_macros(&mut self)->Res<()> {
        if self.cpp_namespaces.ready {return Ok(());}
        let mut cache=NamespaceCache::default();
        let mut bodies=Vec::new();
        let files = match self.sorted_files() {Some(files)=>files, None=>Arc::new(self.table()?.files.iter().cloned().collect())};
        for file in files.iter() {
            if !re!(r"(?i)\.(?:h|hh|hpp|hxx|inl|ipp|tcc|c|cc|cpp|cxx)$").is_match(file) {continue;}
            let Some(source)=self.read_file(file) else {continue};
            let code=super::awaited::strip_ts_comments(&source.text().replace("\\\r\n"," ").replace("\\\n"," "));
            // (Of a file's alternatives for one build or another, `#if … namespace backend = …upb;
            // #elif … namespace backend = …cpp;`, the first.)
            if namespace_alias_pattern().is_match(&code) {
                for (name,target,line,end) in cpp_namespace_alias_declarations(&super::awaited::strip_ts_comments(source.text())) {
                    match end {
                        Some(end)=>cache.local_aliases.entry(file.clone()).or_default().push((name,target,line,end)),
                        None=>{cache.aliases.entry(file.clone()).or_default().entry(name).or_insert((target,line));}
                    }
                }
            }
            for m in re!(r"(?m)^[ \t]*#[ \t]*define[ \t]+([A-Za-z_]\w*)(\(\s*([A-Za-z_]\w*)?\s*\))?[ \t]+([^\n]*)$").captures_iter(&code) {
                let body=m[4].trim();
                if re!(r"^(?:[A-Za-z_]\w*\s+)*\}(?:\s*\})*\s*;?$").is_match(body) {cache.closers.insert((file.clone(),m[1].to_string()));}
                else if m.get(2).is_some() {
                    if let Some(param)=m.get(3) {
                        if Self::cached_regex(&format!(r"^namespace\s+{}\s*\{{[\w\s]*$",regex::escape(param.as_str())))?.is_match(body) {cache.functions.insert((file.clone(),m[1].to_string()));}
                    }
                } else if re!(r"^[A-Za-z_]\w*$").is_match(body) {cache.replacements.insert((file.clone(),m[1].to_string()),body.to_string());}
                else if re!(r"^(?:inline\s+namespace\s+[^{}]*\{\s*|namespace\s+[A-Za-z_]\w*\s*\{\s*)+[\w\s]*$").is_match(body) {bodies.push((file.clone(),m[1].to_string(),body.to_string()));}
            }
        }
        cache.ready=true;self.cpp_namespaces=cache;
        for (file,name,body) in bodies {
            let ordinary=re!(r"inline\s+namespace\s+[^{}]*\{").replace_all(&body,"");
            let mut path=Vec::new();
            for m in re!(r"namespace\s+([A-Za-z_]\w*)").captures_iter(&ordinary) {path.push(self.namespace_replacement(&file,&m[1])?);}
            if !path.is_empty() {self.cpp_namespaces.openers.insert((file,name),path);}
        }
        Ok(())
    }
    fn namespace_replacement(&mut self,file:&str,name:&str)->Res<String> {
        if let Some(hit)=self.cpp_namespaces.replacements.get(&(file.to_string(),name.to_string())) {return Ok(hit.clone());}
        let visible=self.namespace_visible_files(file,"cpp")?;
        let values:HashSet<_>=self.cpp_namespaces.replacements.iter().filter(|((f,n),_)|visible.contains(f) && n==name).map(|(_,v)|v.clone()).collect();
        Ok(if values.len()==1 {values.into_iter().next().unwrap()} else {name.to_string()})
    }
    /// cppProjectScopeNames (name-matcher.ts): every name a qualifier may start with from some
    /// scope in this project — each segment of the qualified names of the types its C and C++ code
    /// declares, the outermost namespaces its namespace-opening macros open (`fmt` for
    /// `FMT_BEGIN_NAMESPACE`, which no qualified name shows), and its namespace aliases. Not the
    /// names of functions: protobuf's conversion operators read as methods named
    /// `google::protobuf::absl::Cord`.
    pub(super) fn cpp_project_scope_names(&mut self) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = &self.cpp_scope_names { return Ok(hit.clone()); }
        self.prepare_namespace_macros()?;
        let mut names: HashSet<String> = self.cpp_namespaces.openers.values().filter_map(|path| path.first().cloned()).collect();
        names.extend(self.cpp_namespaces.aliases.values().flat_map(|aliases| aliases.keys().cloned()));
        names.extend(self.cpp_namespaces.local_aliases.values().flat_map(|aliases| aliases.iter().map(|(name,..)|name.clone())));
        for node in self.table()?.nodes.iter() {
            if matches!(node.language.as_str(), "c" | "cpp") && matches!(node.kind.as_str(), "class" | "struct" | "union" | "enum" | "type_alias") {
                names.extend(node.qualified_name.split("::").filter(|s| !s.is_empty()).map(str::to_string));
            }
        }
        let names = Rc::new(names);
        self.cpp_scope_names = Some(names.clone());
        Ok(names)
    }
    pub(super) fn namespace_frames(&mut self,file:&str)->Res<Vec<(i64,i64,Vec<String>)>> {
        self.prepare_namespace_macros()?;
        if let Some(hit)=self.cpp_namespaces.frames.get(file) {return Ok(hit.clone());}
        let visible=self.namespace_visible_files(file,"cpp")?;
        let mut frames=Vec::new();let mut open:Vec<(i64,Vec<String>)>=Vec::new();
        if let Some(lines)=self.read_file(file) {
            for (i,line) in lines.iter().enumerate() {
                let Some(m)=re!(r"^[ \t]*([A-Z_][A-Z0-9_]*)(?:\(\s*([A-Za-z_]\w*)?\s*\))?[ \t]*;?[ \t]*$").captures(line) else {continue};
                let token=&m[1];
                let mut paths:HashSet<Vec<String>>=self.cpp_namespaces.openers.iter().filter(|((f,name),_)|visible.contains(f) && name==token).map(|(_,path)|path.clone()).collect();
                if self.cpp_namespaces.functions.iter().any(|(f,name)|visible.contains(f) && name==token) {
                    if let Some(arg)=m.get(2) {paths.insert(vec![self.namespace_replacement(file,arg.as_str())?]);}
                }
                let path=(paths.len()==1).then(||paths.into_iter().next().unwrap());
                if let Some(path)=path {open.push((i as i64+1,path));}
                else if self.cpp_namespaces.closers.iter().any(|(f,name)|visible.contains(f) && name==token) {
                    // Each namespace macro frame owns its complete namespace path,
                    // including transparent inline namespaces.
                    if let Some((start,path))=open.pop() {frames.push((start,i as i64+1,path));}
                }
            }
            for (start,path) in open {frames.push((start,lines.len() as i64,path));}
        }
        self.cpp_namespaces.frames.insert(file.to_string(),frames.clone());Ok(frames)
    }
    pub(super) fn namespace_visible_files(&mut self,file:&str,language:&str)->Res<HashSet<String>> {
        if let Some(hit)=self.cpp_namespaces.includes.get(file) {return Ok(hit.clone());}
        let mut seen=HashSet::new();let mut pending=vec![file.to_string()];
        while let Some(file)=pending.pop() {
            if !seen.insert(file.clone()) {continue;}
            for node in self.nodes_in_file(&file)?.iter().filter(|n| n.kind == "import" && matches!(n.language.as_str(), "c" | "cpp")) {
                let delimiter = if node.signature.as_deref().is_some_and(|s| s.contains('<')) { '<' } else { '"' };
                if let Some(target)=self.resolve_cpp_include(&file,delimiter,&node.name,language)? {pending.push(target);}
            }
        }
        self.cpp_namespaces.includes.insert(file.to_string(),seen.clone());Ok(seen)
    }
    /// The namespace `name` is an alias of where line `line` of `file` writes it: `pybind11` for
    /// `py` under `namespace py = pybind11;`, declared before it in the block around it or in the
    /// file itself, else alike in the files it includes before it. (`::lib` for an alias of the
    /// global `lib`.)
    pub(super) fn cpp_namespace_alias(&mut self,file:&str,name:&str,line:i64)->Res<Option<String>> {
        self.prepare_namespace_macros()?;
        if let Some((_,target,..))=self.cpp_namespaces.local_aliases.get(file).and_then(|a|a.iter().rfind(|(n,_,from,to)|n==name && *from<=line && line<=*to)) {return Ok(Some(target.clone()));}
        if let Some((target,_))=self.cpp_namespaces.aliases.get(file).and_then(|a|a.get(name)).filter(|(_,at)|*at<=line) {return Ok(Some(target.clone()));}
        let visible=self.namespace_visible_files(file,"cpp")?;
        let declaring:Vec<(String,String)>=visible.iter().filter(|f|f.as_str()!=file).filter_map(|f|self.cpp_namespaces.aliases.get(f).and_then(|a|a.get(name)).map(|(target,_)|(f.clone(),target.clone()))).collect();
        let mut aliases=HashSet::new();
        for (declarer,target) in declaring {
            if self.cpp_included_before(file,line,&declarer,"cpp")? {aliases.insert(target);}
        }
        Ok((aliases.len()==1).then(||aliases.into_iter().next().unwrap()))
    }
    /// Whether the `#include`s `file` writes before line `line` bring in `target`, directly or
    /// through what those files include. An include in a branch only a build with a flag nothing
    /// sets compiles (`#if 0`) counts only for a line in such a branch too.
    pub(super) fn cpp_included_before(&mut self,file:&str,line:i64,target:&str,language:&str)->Res<bool> {
        let imports:Vec<(i64,char,String)>=self.nodes_in_file(file)?.iter()
            .filter(|n|n.kind=="import" && matches!(n.language.as_str(),"c"|"cpp") && n.start_line<line)
            .map(|n|(n.start_line,if n.signature.as_deref().is_some_and(|s|s.contains('<')) {'<'} else {'"'},n.name.clone())).collect();
        if imports.is_empty() {return Ok(false);}
        let off_here=self.is_off_by_default(file,line);
        for (at,delimiter,spec) in imports {
            let Some(included)=self.resolve_cpp_include(file,delimiter,&spec,language)? else {continue};
            if included!=target && !self.namespace_visible_files(&included,language)?.contains(target) {continue;}
            if off_here || !self.is_off_by_default(file,at) {return Ok(true);}
        }
        Ok(false)
    }
    /// `qualified` spelled through the name macros `file` sees, when that differs: rocksdb's types
    /// are `ROCKSDB_NAMESPACE::DB` in the index and `rocksdb::DB` to code that names them, under
    /// `#define ROCKSDB_NAMESPACE rocksdb`; a tree declaring `namespace rocksdb` itself is named
    /// `ROCKSDB_NAMESPACE::Cache` the other way round.
    pub(super) fn cpp_name_macro_spelling(&mut self,file:&str,qualified:&str)->Res<Option<String>> {
        self.prepare_namespace_macros()?;
        if self.cpp_namespaces.replacements.is_empty() {return Ok(None);}
        let visible=self.namespace_visible_files(file,"cpp")?;
        let spelled:Vec<String>=qualified.split("::").map(|segment| {
            let forward:HashSet<_>=self.cpp_namespaces.replacements.iter().filter(|((f,name),_)|visible.contains(f) && name==segment).map(|(_,body)|body.clone()).collect();
            if forward.len()==1 {return forward.into_iter().next().unwrap();}
            let reverse:HashSet<_>=self.cpp_namespaces.replacements.iter().filter(|((f,_),body)|visible.contains(f) && *body==segment).map(|((_,name),_)|name.clone()).collect();
            if reverse.len()==1 {reverse.into_iter().next().unwrap()} else {segment.to_string()}
        }).collect();
        let spelled=spelled.join("::");
        Ok((spelled!=qualified).then_some(spelled))
    }
    pub(super) fn match_cpp_macro_namespaced(&mut self,r:&ResolveRefIn)->Res<Option<KCand>> {
        if !matches!(r.language.as_str(),"c"|"cpp") || !r.reference_name.trim_start_matches("::").contains("::") {return Ok(None);}
        self.prepare_namespace_macros()?;
        let visible=self.namespace_visible_files(&r.file_path,&r.language)?;
        let mut parts:Vec<String>=r.reference_name.split("::").filter(|p|!p.is_empty()).map(str::to_string).collect();
        if let Some(first)=parts.first().cloned() {
            if let Some(alias)=self.cpp_namespace_alias(&r.file_path,&first,r.line)? {parts.splice(0..1,alias.split("::").filter(|p|!p.is_empty()).map(str::to_string));}
        }
        let Some(name)=parts.last() else {return Ok(None)};
        let candidates=self.nodes_by_name(name)?;
        let mut matches=Vec::new();
        for n in candidates.iter().filter(|n|matches!(n.language.as_str(),"c"|"cpp") && n.kind=="function" && visible.contains(&n.file_path)) {
            let frames=self.namespace_frames(&n.file_path)?;
            let mut path=Vec::new();
            let mut containing:Vec<_>=frames.iter().filter(|(start,end,_)|*start<=n.start_line && *end>=n.start_line).collect();
            containing.sort_by_key(|(start,_,_)|*start);
            for (_,_,p) in containing {path.extend(p.iter().cloned());}
            path.extend(n.qualified_name.split("::").map(str::to_string));
            if path==parts {matches.push(n.clone());}
        }
        let matches=self.cpp_specialization_pool(matches,r);
        let args = if r.reference_kind=="calls" { self.call_arguments(r,name) } else { None };
        let mut best:Vec<Arc<KNode>>=Vec::new(); let mut best_score=i64::MIN;
        for n in matches {
            if args.as_ref().is_some_and(|args| !self.cpp_overload_compatible(&n,args,r)) {continue;}
            let score=if is_test_path(&n.file_path){-10}else{0} + args.as_ref().map(|args|self.cpp_overload_fit(&n,args,r)).unwrap_or(0);
            if score>best_score {best_score=score;best=vec![n];}else if score==best_score{best.push(n);}
        }
        // Overloads the arguments fit alike by count are told apart by the types they have (fmt's
        // `write(out, count)` on an `int count`), else the call is left alone.
        let best=match best.len() {
            0=>None,
            1=>best.pop(),
            _ if args.is_some()=>self.cpp_argument_type_choice(best,name,r)?,
            _=>None,
        };
        Ok(best.map(|node|KCand{node,confidence:0.8,resolved_by:"qualified-name"}))
    }
}

/// `namespace name = target;` at the start of a line (`target` keeps a leading `::`).
fn namespace_alias_pattern() -> Rc<Regex> {
    re!(r"(?m)^\s*namespace\s+(\w+)\s*=\s*((?:::)?[\w:]+)\s*;")
}

/// The namespace aliases `text` (comments blanked) declares, in order: name, target, line, and
/// for one a function or other block declares, the last line of that block, where it stops
/// applying. One a namespace's body or `extern "C"` declares is the file's.
fn cpp_namespace_alias_declarations(text: &str) -> Vec<(String, String, i64, Option<i64>)> {
    let found: Vec<(usize, String, String)> = namespace_alias_pattern().captures_iter(text).filter_map(|m| Some((m.get(1)?.start(), m[1].to_string(), m[2].to_string()))).collect();
    let bytes = text.as_bytes();
    let mut out: Vec<(String, String, i64, Option<i64>)> = Vec::new();
    // Each open block: whether it is a namespace's (or `extern "C"`'s).
    let mut blocks: Vec<bool> = Vec::new();
    // Aliases a block declares, with how many blocks were open around them.
    let mut pending: Vec<(usize, usize)> = Vec::new();
    let (mut next, mut line, mut statement, mut i) = (0usize, 1i64, 0usize, 0usize);
    while i < bytes.len() {
        while next < found.len() && found[next].0 <= i {
            let (_, name, target) = &found[next];
            out.push((name.clone(), target.clone(), line, None));
            if blocks.iter().any(|namespace| !namespace) { pending.push((out.len() - 1, blocks.len())); }
            next += 1;
        }
        match bytes[i] {
            b'\n' => line += 1,
            quote @ (b'"' | b'\'') => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != quote && bytes[j] != b'\n' { j += if bytes[j] == b'\\' { 2 } else { 1 }; }
                i = j.min(bytes.len() - 1);
                if bytes[i] == b'\n' { continue; }
            }
            b'{' => {
                blocks.push(re!(r#"(?:\bnamespace\b[^;{}]*|\bextern\s*"[^"]*"\s*)$"#).is_match(&text[statement..i]));
                statement = i + 1;
            }
            b'}' => {
                blocks.pop();
                pending.retain(|&(at, depth)| if depth > blocks.len() { out[at].3 = Some(line); false } else { true });
                statement = i + 1;
            }
            b';' => statement = i + 1,
            _ => {}
        }
        i += 1;
    }
    for (at, _) in pending { out[at].3 = Some(line); }
    out
}
