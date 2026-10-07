//! Namespace ownership supplied by a project's namespace-opening macros.
use super::*;

#[derive(Default)]
pub(super) struct NamespaceCache {
    ready: bool,
    openers: HashMap<(String,String),Vec<String>>,
    functions: HashSet<(String,String)>,
    closers: HashSet<(String,String)>,
    aliases: HashMap<String,HashMap<String,String>>,
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
            for m in re!(r"(?m)^\s*namespace\s+(\w+)\s*=\s*(?:::)?([\w:]+)\s*;").captures_iter(&code) {cache.aliases.entry(file.clone()).or_default().insert(m[1].to_string(),m[2].to_string());}
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
    pub(super) fn match_cpp_macro_namespaced(&mut self,r:&ResolveRefIn)->Res<Option<KCand>> {
        if !matches!(r.language.as_str(),"c"|"cpp") || !r.reference_name.trim_start_matches("::").contains("::") {return Ok(None);}
        self.prepare_namespace_macros()?;
        let visible=self.namespace_visible_files(&r.file_path,&r.language)?;
        let mut parts:Vec<String>=r.reference_name.split("::").filter(|p|!p.is_empty()).map(str::to_string).collect();
        if let Some(first)=parts.first() {
            let local=self.cpp_namespaces.aliases.get(&r.file_path).and_then(|a|a.get(first)).cloned();
            let aliases:HashSet<_>=visible.iter().filter_map(|file|self.cpp_namespaces.aliases.get(file).and_then(|a|a.get(first))).cloned().collect();
            if let Some(alias)=local.or_else(||(aliases.len()==1).then(||aliases.into_iter().next().unwrap())) {parts.splice(0..1,alias.split("::").map(str::to_string));}
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
        let args = if r.reference_kind=="calls" { self.call_arguments(r,name) } else { None };
        let mut best=None; let mut best_score=i64::MIN;let mut tied=false;
        for n in matches {
            if args.as_ref().is_some_and(|args| !self.cpp_overload_compatible(&n,args,r)) {continue;}
            let score=if is_test_path(&n.file_path){-10}else{0} + args.as_ref().map(|args|self.cpp_overload_fit(&n,args,r)).unwrap_or(0);
            if score>best_score {best_score=score;best=Some(n);tied=false;}else if score==best_score{tied=true;}
        }
        if tied{return Ok(None);}
        Ok(best.map(|node|KCand{node,confidence:0.8,resolved_by:"qualified-name"}))
    }
}
