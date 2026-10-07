//! Where a name reached by name alone can land in Dart, Kotlin, Ruby and
//! CFML (name-matcher.ts isDartMethodInScope / nearestDartMembers,
//! isKotlinTopLevelVisible, isRubyMethodInScope, isCfmlMethodInScope). Each
//! rule only removes candidates; none picks one.

use super::*;

const DART_EXTENSION_RANK: u32 = 1000;
const DART_UNREACHED: u32 = u32::MAX;

fn is_dart_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "interface" | "enum" | "mixin" | "extension" | "struct" | "trait")
}

/// A member of a Dart type: a method, or an abstract member written without
/// a body (extracted as a `function` owned by the type).
pub(super) fn is_dart_member(n: &KNode) -> bool {
    n.kind == "method" || (n.kind == "function" && n.qualified_name.contains("::"))
}

const KOTLIN_DEFAULT_IMPORTS: &[&str] = &[
    "kotlin", "kotlin.annotation", "kotlin.collections", "kotlin.comparisons", "kotlin.io", "kotlin.ranges",
    "kotlin.sequences", "kotlin.text", "kotlin.jvm", "java.lang", "kotlin.js",
];

fn is_kotlin_enclosing_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "interface" | "enum" | "struct" | "trait" | "protocol" | "module" | "namespace" | "function" | "method"
    )
}

fn is_kotlin_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "interface" | "enum" | "struct" | "trait")
}

/// A Kotlin file's `package` and the names and packages its `import`s bring in.
#[derive(Default)]
pub(crate) struct KotlinFileScope {
    pub(super) pkg: String,
    pub(super) imports: HashSet<String>,
    pub(super) stars: HashSet<String>,
}

fn collect_kotlin_file_scope(text: &str) -> KotlinFileScope {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = re!(r"/\*(?s:.)*?\*/").replace_all(text, " ").replace('`', "");
    let pkg = re!(r"(?m)^\s*package\s+([A-Za-z0-9_.]+)")
        .captures(&text)
        .map(|m| m[1].to_string())
        .unwrap_or_default();
    let mut scope = KotlinFileScope { pkg, ..Default::default() };
    for m in re!(r"(?m)^\s*import\s+([A-Za-z0-9_.]+?)(\.\*)?(?:\s+as\s+[A-Za-z0-9_]+)?\s*;?\s*(?://.*)?$").captures_iter(&text) {
        if m.get(2).is_some() {
            scope.stars.insert(m[1].to_string());
        } else {
            scope.imports.insert(m[1].to_string());
        }
    }
    scope
}

/// A bare `calls` ref of `language` whose name is one identifier.
fn is_bare_call_of(r: &ResolveRefIn, language: &str) -> bool {
    r.language == language && r.reference_kind == "calls" && re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(&r.reference_name)
}

fn is_bare_ruby_call(r: &ResolveRefIn) -> bool {
    r.language == "ruby" && r.reference_kind == "calls" && re!(r"^[A-Za-z_][A-Za-z0-9_]*[?!]?$").is_match(&r.reference_name)
}

fn is_bare_cfml_call(r: &ResolveRefIn) -> bool {
    matches!(r.language.as_str(), "cfml" | "cfscript")
        && r.reference_kind == "calls"
        && re!(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(&r.reference_name)
}

/// Where a ref's name starts on its line, as a byte offset: at the ref's
/// column, or just before it (a Dart ref's column sits past the name).
pub(super) fn name_start_at_column(line: &str, name: &str, column: usize) -> Option<usize> {
    if js_slice(line, column).starts_with(name) {
        return Some(js_unit_to_byte(line, column));
    }
    let len = utf16_len(name);
    if column >= len && js_slice(line, column - len).starts_with(name) {
        return Some(js_unit_to_byte(line, column - len));
    }
    None
}

fn ends_with_member_dot(before: &str) -> bool {
    before.trim_end().ends_with('.')
}

/// The Dart type declaration head: the supertypes it names, and whether it is
/// an `extension` (whose one supertype is the type it extends).
pub(super) struct DartHead {
    pub(super) supers: Vec<String>,
    extension: bool,
}

impl KernelResolver {
    /// The candidates the language's scope rules allow for a name reached by
    /// name alone (matchByExactName / matchFuzzy filters, name-matcher.ts).
    pub(super) fn retain_lang_scope(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        Ok(self.retain_lang_scope_tracked(candidates, r)?.0)
    }

    /// retain_lang_scope, also telling whether the Kotlin visibility rule or
    /// a VB.NET, C# or Objective-C member rule removed a candidate.
    /// Elimination is no evidence for what is left: the caller then keeps a
    /// lone survivor below the trusted range unless `is_lang_survivor_bound`
    /// binds it.
    pub(super) fn retain_lang_scope_tracked(
        &mut self,
        candidates: Vec<Arc<KNode>>,
        r: &ResolveRefIn,
    ) -> Res<(Vec<Arc<KNode>>, bool)> {
        let shape_call = matches!(r.language.as_str(), "rust" | "go" | "swift" | "scala" | "kotlin")
            && r.reference_kind == "calls" && re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(&r.reference_name);
        let solidity_bare = r.language == "solidity" && self.is_receiver_less_call(r)?;
        let ruby_bare = is_bare_ruby_call(r);
        let cfml_bare = is_bare_cfml_call(r);
        let kotlin_call = is_bare_call_of(r, "kotlin") && !self.is_kotlin_qualified_call(r);
        let dart_bare = is_bare_call_of(r, "dart") && self.is_receiver_less_dart_call(r);
        let member_site = matches!(r.language.as_str(), "vbnet" | "csharp" | "objc")
            .then(|| self.member_site(r))
            .filter(|s| s.is_judged());
        if !shape_call && !solidity_bare && !ruby_bare && !cfml_bare && !kotlin_call && !dart_bare && member_site.is_none() {
            return Ok((candidates, false));
        }
        let mut shrank = false;
        let mut kept = Vec::with_capacity(candidates.len());
        for n in candidates {
            if solidity_bare && !self.solidity_member_in_scope(&n,r)? { shrank = true; continue; }
            if shape_call && !self.call_shape_target(&n, r)? { shrank = true; continue; }
            if let Some(site) = &member_site {
                if !self.is_member_in_reach(&n, site, r)? {
                    shrank = true;
                    continue;
                }
            }
            if ruby_bare && n.kind == "method" && !self.is_ruby_method_in_scope(&n, r)? {
                continue;
            }
            if cfml_bare && n.kind == "method" && !self.is_cfml_method_in_scope(&n, r)? {
                continue;
            }
            if kotlin_call && (!self.is_kotlin_top_level_visible(&n, r)? || !self.kotlin_member_reachable(&n, r)?) {
                shrank = true;
                continue;
            }
            if dart_bare && is_dart_member(&n) && self.dart_member_depth(&n, r)? == DART_UNREACHED {
                continue;
            }
            kept.push(n);
        }
        if kotlin_call { kept = self.lexical_kotlin_members(kept, r)?; }
        Ok((kept, shrank))
    }

    /// Whether the one candidate `retain_lang_scope_tracked` left after
    /// removing others is bound to the call site (see
    /// `is_kotlin_survivor_in_scope` and `is_member_survivor_bound`).
    pub(super) fn is_lang_survivor_bound(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.file_path == r.file_path { return Ok(true); }
        if r.language == "kotlin" {
            return self.is_kotlin_survivor_in_scope(n, r);
        }
        if r.language == "swift" { return self.swift_scope_bound(n, r); }
        let site = self.member_site(r);
        self.is_member_survivor_bound(n, &site, r)
    }

    /// A Kotlin call written after a qualifier (`io.javalin.config.Key<String>(…)`,
    /// `pkg.helper()`, a chain's later link such as `.where { … }`) reaches the resolver as its bare
    /// name, but the qualifier, not the file's imports, says what it names.
    /// `this.` and `super.` calls are still judged.
    pub(super) fn is_kotlin_qualified_call(&mut self, r: &ResolveRefIn) -> bool {
        if self.kotlin_infix_site(r).is_some() { return true; }
        let Some(lines) = self.read_file(&r.file_path) else { return false };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return false };
        let name = r.reference_name.as_str();
        let Some(at) = name_start_at_column(line, name, r.column.max(0) as usize).or_else(|| {
            let call = Self::cached_regex(&format!(r"{}\s*[(<{{]", regex::escape(name))).ok()?;
            let found = call
                .find_iter(line)
                .map(|m| m.start())
                .find(|&at| !line[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_'));
            found
        }) else {
            // Not on its line at all: a link of a chain written across lines.
            return true;
        };
        let before = &line[..at];
        if !ends_with_member_dot(before) {
            return false;
        }
        let prefix = before.trim_end().trim_end_matches('.').trim_end();
        !re!(r"(?-u:\b)(?:this|super)(?:@[A-Za-z_][A-Za-z0-9_]*)?$").is_match(prefix)
    }

    /// Whether the one candidate left after the Kotlin rule is bound to the
    /// call site rather than merely left over: a declaration in the calling
    /// file, a top-level declaration the rule itself admitted through the
    /// file's package or imports, or a member of the class hierarchy the call
    /// is written in (as `is_java_method_in_scope` does for Java).
    pub(super) fn is_kotlin_survivor_in_scope(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.file_path == r.file_path {
            return Ok(true);
        }
        if n.language != "kotlin" {
            return Ok(false);
        }
        if !self.is_kotlin_member(n)? {
            return Ok(true);
        }
        let Some(cut) = n.qualified_name.rfind("::") else { return Ok(false) };
        let owner = n.qualified_name[..cut].rsplit("::").next().unwrap_or("").to_string();
        let mut queue: VecDeque<String> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|t| is_kotlin_type_kind(&t.kind) && t.start_line <= r.line && t.end_line >= r.line)
            .map(|t| t.name.clone())
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        while seen.len() < 40 {
            let Some(name) = queue.pop_front() else { break };
            if !seen.insert(name.clone()) {
                continue;
            }
            if name == owner {
                return Ok(true);
            }
            queue.extend(self.kotlin_supertypes_of(&name)?.iter().cloned());
        }
        Ok(false)
    }

    /// Whether a Kotlin declaration sits inside another declaration (the
    /// geometry isKotlinTopLevelVisible reads).
    fn is_kotlin_member(&mut self, n: &KNode) -> Res<bool> {
        Ok(self.nodes_in_file(&n.file_path)?.iter().any(|o| {
            o.id != n.id
                && is_kotlin_enclosing_kind(&o.kind)
                && o.start_line <= n.start_line
                && o.end_line >= n.end_line
                && (o.start_line < n.start_line || o.end_line > n.end_line)
        }))
    }

    /// A bare Solidity method must belong to the enclosing contract or a bound ancestor.
    fn solidity_member_in_scope(&mut self,n:&KNode,r:&ResolveRefIn)->Res<bool> {
        if n.kind != "method" || n.language != "solidity" { return Ok(true); }
        let Some((owner,_)) = n.qualified_name.rsplit_once("::") else { return Ok(true) };
        let mut pending: VecDeque<Arc<KNode>> = self.nodes_in_file(&r.file_path)?.iter().filter(|c| is_class_like(&c.kind) && c.start_line<=r.line && c.end_line>=r.line).cloned().collect();
        let mut seen = HashSet::new();
        while seen.len()<60 {
            let Some(decl)=pending.pop_front() else {break};
            if !seen.insert(decl.id.clone()) {continue;}
            if decl.qualified_name==owner && decl.file_path==n.file_path {return Ok(true);}
            for id in self.outgoing_edge_targets(&decl.id, &["extends"])? {
                if let Some(parent)=self.node_by_id(&id)? { pending.push_back(parent); }
            }
            let Some(lines)=self.read_file(&decl.file_path) else {continue};
            let from=(decl.start_line-1).max(0) as usize;let to=((decl.start_line+10).max(0) as usize).min(lines.len());
            let head=super::call_shape::flat_head(&lines[from.min(to)..to].join("\n"),false);
            let Some(m)=re!(r"\bis\b([\s\S]*)$").captures(&head) else {continue};
            for name in m[1].split(',').map(str::trim) {
                let mut site=r.clone();site.file_path=decl.file_path.clone();site.line=decl.start_line;site.column=decl.start_column;
                if let Some(parent)=self.solidity_parent(name,&site)? {pending.push_back(parent);}
            }
        }
        Ok(false)
    }

    fn solidity_parent(&mut self,name:&str,site:&ResolveRefIn)->Res<Option<Arc<KNode>>> {
        // Generic Solidity extraction retains symbolic imports in the import
        // node's signature rather than lexical binding rows.
        for import in self.nodes_in_file(&site.file_path)?.iter().filter(|n|n.kind=="import") {
            let Some(signature)=import.signature.as_deref() else {continue};
            let Some(m)=re!(r#"\bimport\s*\{([^}]*)\}\s*from\s*["']([^"']+)["']"#).captures(signature) else {continue};
            for symbol in m[1].split(',') {
                let words:Vec<_>=symbol.split_whitespace().collect();
                let (exported,local)=match words.as_slice() {[exported] => (*exported,*exported),[exported,"as",local] => (*exported,*local),_=>continue};
                if local!=name {continue;}
                let Some(file)=self.resolve_import_path(&m[2],&site.file_path,"solidity")? else {return Ok(None)};
                let parents:Vec<_>=self.nodes_in_file(&file)?.iter().filter(|n|n.language=="solidity" && is_class_like(&n.kind) && n.name==exported).cloned().collect();
                return Ok(match parents.as_slice() {[parent]=>Some(parent.clone()),_=>None});
            }
        }
        self.resolve_bound_type(name,site,0)
    }

    /// The simple names a Kotlin type's declarations list after `:` in their
    /// head (`class A(x: Int) : B(x), C by d`), constructor arguments and type
    /// arguments dropped.
    pub(super) fn kotlin_supertypes_of(&mut self, type_name: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.kotlin_supers_memo.get(type_name) {
            return Ok(hit.clone());
        }
        let decls: Vec<Arc<KNode>> = self
            .nodes_by_name(type_name)?
            .iter()
            .filter(|d| d.language == "kotlin" && is_kotlin_type_kind(&d.kind))
            .cloned()
            .collect();
        let mut names = Vec::new();
        for decl in decls {
            let Some(lines) = self.read_file(&decl.file_path) else { continue };
            let from = (decl.start_line - 1).max(0) as usize;
            let to = (decl.end_line.min(decl.start_line + 10).max(0) as usize).min(lines.len());
            let text = if from < to { lines[from..to].join("\n") } else { String::new() };
            let text = re!(r"/\*(?s:.)*?\*/").replace_all(&text, " ");
            let text = re!(r"//[^\n]*").replace_all(&text, " ");
            // Blank nested `(…)` and `<…>` so only the head's own `:` and `,` remain.
            let mut depth = 0usize;
            let mut flat = String::new();
            for ch in text.chars() {
                match ch {
                    '(' | '<' => depth += 1,
                    ')' | '>' => depth = depth.saturating_sub(1),
                    '{' if depth == 0 => break,
                    _ if depth == 0 => flat.push(ch),
                    _ => {}
                }
            }
            let Some(kw) = re!(r"(?-u:\b)(?:class|interface|object)(?-u:\b)").find(&flat) else { continue };
            let head = &flat[kw.end()..];
            let Some(colon) = head.find(':') else { continue };
            for part in head[colon + 1..].split(',') {
                if let Some(m) = re!(r"[A-Za-z_][A-Za-z0-9_]*(?:\s*\.\s*[A-Za-z_][A-Za-z0-9_]*)*").find(part) {
                    let simple = m.as_str().rsplit('.').next().unwrap_or("").trim();
                    if !simple.is_empty() {
                        names.push(simple.to_string());
                    }
                }
            }
        }
        let names = Rc::new(names);
        self.kotlin_supers_memo.insert(type_name.to_string(), names.clone());
        Ok(names)
    }

    /// nearestDartMembers: of the in-scope members a bare Dart call could
    /// mean, the nearest — a subclass's override, or the class that
    /// implements what an interface only declares.
    pub(super) fn nearest_dart_members(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        if !is_bare_call_of(r, "dart") || !self.is_receiver_less_dart_call(r) {
            return Ok(candidates);
        }
        if candidates.iter().filter(|n| is_dart_member(n)).count() < 2 {
            return Ok(candidates);
        }
        let mut depths: HashMap<String, u32> = HashMap::new();
        for n in candidates.iter().filter(|n| is_dart_member(n)) {
            let d = self.dart_member_depth(n, r)?;
            depths.insert(n.id.clone(), d);
        }
        let nearest = depths.values().copied().min().unwrap_or(DART_UNREACHED);
        Ok(candidates.into_iter().filter(|n| depths.get(&n.id).is_none_or(|&d| d == nearest)).collect())
    }

    /// isReceiverLessDartCall: the extractor keeps one receiver level, so the
    /// later links of a chain arrive as bare names; their line shows `.name(`.
    fn is_receiver_less_dart_call(&mut self, r: &ResolveRefIn) -> bool {
        let Some(lines) = self.read_file(&r.file_path) else { return true };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return true };
        let name = r.reference_name.as_str();
        let start = name_start_at_column(line, name, r.column.max(0) as usize).or_else(|| line.find(name));
        match start {
            Some(at) => !ends_with_member_dot(&line[..at]),
            None => true,
        }
    }

    pub(super) fn dart_bare_member_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if !is_bare_call_of(r, "dart") || !self.is_receiver_less_dart_call(r)
            || !n.qualified_name.contains("::") { return Ok(true); }
        Ok(self.dart_member_depth(n, r)? != DART_UNREACHED)
    }

    pub(super) fn dart_unnamed_extension(&mut self, n: &KNode) -> bool {
        if n.language != "dart" || n.kind != "class" { return false; }
        self.read_file(&n.file_path).is_some_and(|lines| {
            let text = lines.iter().skip((n.start_line - 1).max(0) as usize)
                .take((n.end_line - n.start_line + 1).clamp(1, 6) as usize)
                .cloned().collect::<Vec<_>>().join("\n");
            let text = re!(r"/\*(?s:.)*?\*/|//[^\n]*").replace_all(&text, " ");
            let head = text.split(['{', ';']).next().unwrap_or("");
            re!(r"\bextension\s*(?:<[^>]*>\s*)?on\b").is_match(head)
        })
    }

    /// dartMemberDepth: supertype steps from the class a call is written in
    /// to `method`'s owner; an extension `on` a type of that hierarchy ranks
    /// after every real member; DART_UNREACHED outside the hierarchy.
    fn dart_member_depth(&mut self, method: &KNode, r: &ResolveRefIn) -> Res<u32> {
        let Some(cut) = method.qualified_name.rfind("::") else { return Ok(0) };
        let owner = method.qualified_name[..cut].rsplit("::").next().unwrap_or("").to_string();
        let hierarchy = self.dart_hierarchy_at(r)?;
        if let Some(&own) = hierarchy.get(&owner) {
            return Ok(own);
        }
        let decl = self.nodes_in_file(&method.file_path)?.iter().find(|n| {
            n.name == owner && is_dart_type_kind(&n.kind) && n.start_line <= method.start_line && n.end_line >= method.end_line
        }).cloned();
        let Some(decl) = decl else { return Ok(DART_UNREACHED) };
        let head = self.dart_head_of(&decl);
        if !head.extension {
            return Ok(DART_UNREACHED);
        }
        let on = head.supers.iter().filter_map(|t| hierarchy.get(t).copied()).min();
        Ok(on.map_or(DART_UNREACHED, |d| DART_EXTENSION_RANK + d))
    }

    /// dartHierarchyAt: every type the classes around a Dart call site are,
    /// by supertype distance (at most 40).
    fn dart_hierarchy_at(&mut self, r: &ResolveRefIn) -> Res<Rc<HashMap<String, u32>>> {
        let key = (r.file_path.clone(), r.line);
        if let Some(hit) = self.dart_hierarchy_memo.get(&key) {
            return Ok(hit.clone());
        }
        let mut queue: VecDeque<(String, u32)> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| is_dart_type_kind(&n.kind) && n.start_line <= r.line && n.end_line >= r.line)
            .map(|n| (n.name.clone(), 0))
            .collect();
        let mut depths: HashMap<String, u32> = HashMap::new();
        while depths.len() < 40 {
            let Some((name, depth)) = queue.pop_front() else { break };
            if depths.contains_key(&name) {
                continue;
            }
            depths.insert(name.clone(), depth);
            for sup in self.dart_supertypes_of(&name)?.iter() {
                queue.push_back((sup.clone(), depth + 1));
            }
        }
        let depths = Rc::new(depths);
        self.dart_hierarchy_memo.insert(key, depths.clone());
        Ok(depths)
    }

    /// dartSupertypesOf: the simple names a Dart type's declarations extend,
    /// mix in, implement, or (an extension / mixin) sit `on`.
    fn dart_supertypes_of(&mut self, type_name: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.dart_supers_memo.get(type_name) {
            return Ok(hit.clone());
        }
        let decls: Vec<Arc<KNode>> = self
            .nodes_by_name(type_name)?
            .iter()
            .filter(|d| d.language == "dart" && is_dart_type_kind(&d.kind))
            .cloned()
            .collect();
        let mut names = Vec::new();
        for decl in decls {
            names.extend(self.dart_head_of(&decl).supers);
        }
        let names = Rc::new(names);
        self.dart_supers_memo.insert(type_name.to_string(), names.clone());
        Ok(names)
    }

    /// dartHeadOf: a declaration's head read from source up to its body, with
    /// comments and type arguments dropped first.
    pub(super) fn dart_head_of(&mut self, decl: &KNode) -> DartHead {
        let Some(lines) = self.read_file(&decl.file_path) else { return DartHead { supers: Vec::new(), extension: false } };
        let from = (decl.start_line - 1).max(0) as usize;
        let to = (decl.end_line.min(decl.start_line + 40).max(0) as usize).min(lines.len());
        let text = if from < to { lines[from..to].join("\n") } else { String::new() };
        let text = re!(r"/\*(?s:.)*?\*/").replace_all(&text, " ");
        let text = re!(r"//[^\n]*").replace_all(&text, " ");
        let mut depth = 0usize;
        let mut flat = String::new();
        for ch in text.chars() {
            if ch == '<' {
                depth += 1;
            } else if ch == '>' {
                depth = depth.saturating_sub(1);
            } else if depth == 0 {
                if ch == '{' || ch == ';' {
                    break;
                }
                flat.push(ch);
            }
        }
        let keyword = re!(r"(?-u:\b)(class|mixin|extension|enum)(?-u:\b)").captures(&flat);
        let head = keyword.as_ref().map_or(flat.as_str(), |k| &flat[k.get(0).unwrap().start()..]);
        let clause = re!(r"(?:(?-u:\b)(?:extends|with|implements|on)(?-u:\b)|=)((?s:.)*)$")
            .captures(head)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let supers = re!(r"[A-Za-z_$][A-Za-z0-9_$]*")
            .find_iter(&clause)
            .map(|m| m.as_str())
            .filter(|w| !matches!(*w, "extends" | "with" | "implements" | "on"))
            .map(str::to_string)
            .collect();
        let extension = keyword.as_ref().is_some_and(|k| &k[1] == "extension")
            && !re!(r"^extension\s+type(?-u:\b)").is_match(head);
        DartHead { supers, extension }
    }

    /// isKotlinTopLevelVisible: a top-level Kotlin declaration can be named
    /// from its own package, an `import` of it, a star import of its package,
    /// or a default import. A member of a class is not judged here.
    fn is_kotlin_top_level_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.language != "kotlin" || n.file_path == r.file_path {
            return Ok(true);
        }
        let enclosed = self.nodes_in_file(&n.file_path)?.iter().any(|o| {
            o.id != n.id
                && is_kotlin_enclosing_kind(&o.kind)
                && o.start_line <= n.start_line
                && o.end_line >= n.end_line
                && (o.start_line < n.start_line || o.end_line > n.end_line)
        });
        if enclosed {
            return Ok(true);
        }
        let pkg = self.kotlin_file_scope(&n.file_path).pkg.clone();
        let here = self.kotlin_file_scope(&r.file_path);
        let named = if pkg.is_empty() { n.name.clone() } else { format!("{pkg}.{}", n.name) };
        Ok(pkg == here.pkg
            || here.stars.contains(&pkg)
            || here.imports.contains(&named)
            || KOTLIN_DEFAULT_IMPORTS.contains(&pkg.as_str()))
    }

    pub(super) fn kotlin_file_scope(&mut self, file: &str) -> Rc<KotlinFileScope> {
        if let Some(hit) = self.kotlin_scope_memo.get(file) {
            return hit.clone();
        }
        let scope = Rc::new(self.read_file(file).map(|f| collect_kotlin_file_scope(f.text())).unwrap_or_default());
        self.kotlin_scope_memo.insert(file.to_string(), scope.clone());
        scope
    }

    /// isRubyMethodInScope: a receiver-less call inside a class body is a
    /// call on `self`, so it reaches the class's own methods, its
    /// superclasses', and those of the modules any of them mixes in. A call
    /// in a module body, a top-level block or a script is not judged.
    fn is_ruby_method_in_scope(&mut self, method: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some(cut) = method.qualified_name.rfind("::") else { return Ok(true) };
        let here = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| matches!(n.kind.as_str(), "class" | "module") && n.start_line <= r.line && n.end_line >= r.line)
            .min_by_key(|n| n.end_line - n.start_line)
            .cloned();
        let Some(here) = here else { return Ok(true) };
        if here.kind != "class" {
            return Ok(true);
        }
        Ok(self.ruby_ancestry(&here.qualified_name)?.contains(&method.qualified_name[..cut]))
    }

    /// rubyAncestry: a class's qualified name, its superclasses', and those of
    /// every module mixed into any of them (at most 60).
    fn ruby_ancestry(&mut self, qn: &str) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = self.ruby_ancestry_memo.get(qn) {
            return Ok(hit.clone());
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::from([qn.to_string()]);
        while seen.len() < 60 {
            let Some(q) = queue.pop_front() else { break };
            if !seen.insert(q.clone()) {
                continue;
            }
            let decls: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&q)?
                .iter()
                .filter(|d| d.language == "ruby" && matches!(d.kind.as_str(), "class" | "module"))
                .cloned()
                .collect();
            for decl in decls {
                let Some(lines) = self.read_file(&decl.file_path) else { continue };
                let outer = q.rfind("::").map_or("", |i| &q[..i]).to_string();
                let header = lines.get((decl.start_line - 1).max(0) as usize).map(String::as_str).unwrap_or("");
                if let Some(sup) = re!(r"^\s*class\s+[A-Za-z0-9_:]+\s*<\s*(::)?([A-Z][A-Za-z0-9_:]*)").captures(header) {
                    let scope = if sup.get(1).is_some() { String::new() } else { outer.clone() };
                    let name = sup[2].to_string();
                    queue.push_back(self.ruby_constant_qn(&name, &scope)?);
                }
                let from = (decl.start_line.max(0) as usize).min(lines.len());
                let to = (decl.end_line.max(0) as usize).min(lines.len());
                let mixes: Vec<String> = lines[from..to.max(from)]
                    .iter()
                    .filter_map(|line| {
                        re!(r"^\s*(?:include|extend|prepend)\s+([A-Z:][A-Za-z0-9_:]*(?:\s*,\s*[A-Z:][A-Za-z0-9_:]*)*)")
                            .captures(line)
                            .map(|m| m[1].to_string())
                    })
                    .collect();
                for mix in mixes {
                    for name in re!(r"\s*,\s*").split(&mix) {
                        let target = match name.strip_prefix("::") {
                            Some(root) => self.ruby_constant_qn(root, "")?,
                            None => self.ruby_constant_qn(name, &q)?,
                        };
                        queue.push_back(target);
                    }
                }
            }
        }
        let seen = Rc::new(seen);
        self.ruby_ancestry_memo.insert(qn.to_string(), seen.clone());
        Ok(seen)
    }

    /// The Ruby class or module a constant written at `r` names, by lexical
    /// lookup from the class or module around it outward (`Sub` inside
    /// top-level `class App` is `::Sub`, never `Other::Sub`); None when no
    /// indexed class or module has that qualified name.
    pub(super) fn ruby_lexical_constant(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if let Some(root) = name.strip_prefix("::") {
            return self.ruby_known_constant(root);
        }
        // Module.nesting: the classes and modules whose bodies enclose the
        // site, innermost first. `class Other::App` opens `Other::App` alone,
        // so `Other` is not searched.
        let mut nesting: Vec<Arc<KNode>> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| matches!(n.kind.as_str(), "class" | "module") && n.start_line <= r.line && n.end_line >= r.line)
            .cloned()
            .collect();
        nesting.sort_by_key(|n| n.end_line - n.start_line);
        for scope in &nesting {
            if let Some(qn) = self.ruby_known_constant(&format!("{}::{name}", scope.qualified_name))? {
                return Ok(Some(qn));
            }
        }
        // Then the ancestors of the class around the site (`Base::Sub` for
        // `Sub` inside `class Child < Base`); more than one is ambiguous.
        if let Some(class) = nesting.first().filter(|n| n.kind == "class") {
            let ancestry = self.ruby_ancestry(&class.qualified_name)?;
            let mut inherited: Vec<String> = Vec::new();
            for a in ancestry.iter().filter(|a| **a != class.qualified_name) {
                if let Some(qn) = self.ruby_known_constant(&format!("{a}::{name}"))? {
                    if !inherited.contains(&qn) {
                        inherited.push(qn);
                    }
                }
            }
            match inherited.len() {
                0 => {}
                1 => return Ok(inherited.pop()),
                _ => return Ok(None),
            }
        }
        self.ruby_known_constant(name)
    }

    /// `qn` when an indexed Ruby class or module has that qualified name.
    fn ruby_known_constant(&mut self, qn: &str) -> Res<Option<String>> {
        let known = self
            .nodes_by_qualified_name(qn)?
            .iter()
            .any(|n| n.language == "ruby" && matches!(n.kind.as_str(), "class" | "module"));
        Ok(known.then(|| qn.to_string()))
    }

    /// Among several same-named Ruby classes or modules, a constant written
    /// at `r` means the one lexical lookup finds; the rest are dropped.
    pub(super) fn retain_ruby_lexical_constant(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let is_const = |n: &KNode| n.language == "ruby" && matches!(n.kind.as_str(), "class" | "module");
        if r.language != "ruby" || r.reference_kind == "calls" || candidates.iter().filter(|n| is_const(n)).count() < 2 {
            return Ok(candidates);
        }
        // `Other::Sub.new` reaches here as `Sub`; the spelling decides.
        let spelled = self
            .read_file(&r.file_path)
            .and_then(|lines| qualified_spelling(&lines, r))
            .unwrap_or_else(|| r.reference_name.clone());
        let Some(qn) = self.ruby_lexical_constant(&spelled, r)? else {
            return Ok(candidates);
        };
        Ok(candidates.into_iter().filter(|n| !is_const(n) || n.qualified_name == qn).collect())
    }

    /// rubyConstantQn: the class or module a constant written inside `scope`
    /// names — the nearest enclosing namespace that has it, else the name.
    fn ruby_constant_qn(&mut self, name: &str, scope: &str) -> Res<String> {
        let mut prefix = scope.to_string();
        loop {
            let qn = if prefix.is_empty() { name.to_string() } else { format!("{prefix}::{name}") };
            if self
                .nodes_by_qualified_name(&qn)?
                .iter()
                .any(|n| n.language == "ruby" && matches!(n.kind.as_str(), "class" | "module"))
            {
                return Ok(qn);
            }
            if prefix.is_empty() {
                return Ok(name.to_string());
            }
            prefix = prefix.rfind("::").map_or(String::new(), |i| prefix[..i].to_string());
        }
    }

    /// isCfmlMethodInScope: a bare call in a component reaches its own
    /// component's methods and those of the components it `extends`. A call
    /// in a `.cfm` template, or one that is a chain's later link, is not judged.
    fn is_cfml_method_in_scope(&mut self, method: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if !r.file_path.to_ascii_lowercase().ends_with(".cfc") || !self.has_no_receiver_on_line(r) {
            return Ok(true);
        }
        Ok(self.cfml_chain(&r.file_path)?.contains(&method.file_path))
    }

    /// cfmlChain: a component file and every component file it extends (at most 30).
    fn cfml_chain(&mut self, file: &str) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = self.cfml_chain_memo.get(file) {
            return Ok(hit.clone());
        }
        let mut chain: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::from([file.to_string()]);
        while chain.len() < 30 {
            let Some(f) = queue.pop_front() else { break };
            if !chain.insert(f.clone()) {
                continue;
            }
            let ext = self.read_file(&f).and_then(|src| {
                let text = src.text();
                let cut = re!(r"(?i)(?-u:\b)function(?-u:\b)|<cffunction").find(text).map(|m| m.start()).filter(|&at| at > 0);
                let head = &text[..cut.unwrap_or(text.len())];
                re!(r#"(?i)(?-u:\b)extends\s*=\s*["']?([A-Za-z0-9_./:-]+)["']?"#).captures(head).map(|m| m[1].to_string())
            });
            if let Some(ext) = ext {
                queue.extend(self.cfml_component_files(&ext, &f)?);
            }
        }
        let chain = Rc::new(chain);
        self.cfml_chain_memo.insert(file.to_string(), chain.clone());
        Ok(chain)
    }

    /// cfmlComponentFiles: the indexed `.cfc` files a component path names —
    /// the same directory first, else the longest path suffix.
    fn cfml_component_files(&mut self, dotted: &str, from: &str) -> Res<Vec<String>> {
        let dotted = dotted.replace('/', ".");
        let segments: Vec<&str> = dotted.split('.').filter(|s| !s.is_empty()).collect();
        let Some(last) = segments.last() else { return Ok(Vec::new()) };
        let mut files: Vec<String> = Vec::new();
        for n in self.nodes_by_lower_name(&last.to_ascii_lowercase())?.iter() {
            if n.kind == "class" && n.file_path.to_ascii_lowercase().ends_with(".cfc") && !files.contains(&n.file_path) {
                files.push(n.file_path.clone());
            }
        }
        if files.is_empty() {
            return Ok(files);
        }
        let dir_of = |f: &str| f.rfind('/').map_or(String::new(), |i| f[..=i].to_string());
        if segments.len() == 1 {
            let here = dir_of(from);
            let local: Vec<String> = files.iter().filter(|f| dir_of(f) == here).cloned().collect();
            return Ok(if local.is_empty() { files } else { local });
        }
        for take in (1..=segments.len()).rev() {
            let suffix = format!("/{}.cfc", segments[segments.len() - take..].join("/").to_ascii_lowercase());
            let hits: Vec<String> =
                files.iter().filter(|f| format!("/{}", f.to_ascii_lowercase()).ends_with(&suffix)).cloned().collect();
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        Ok(files)
    }

    /// hasNoReceiverOnLine: the name, case aside, is not preceded by a `.` on
    /// its line (true when the line can't tell; false when the name is not on
    /// its line at all — a link of a chain written across lines).
    pub(super) fn has_no_receiver_on_line(&mut self, r: &ResolveRefIn) -> bool {
        let Some(lines) = self.read_file(&r.file_path) else { return true };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return true };
        let lower = line.to_ascii_lowercase();
        let name = r.reference_name.to_ascii_lowercase();
        let start = name_start_at_column(&lower, &name, r.column.max(0) as usize).or_else(|| {
            let call = Self::cached_regex(&format!(r"{}\s*\(", regex::escape(&name))).ok()?;
            let found = call
                .find_iter(&lower)
                .map(|m| m.start())
                .find(|&at| !lower[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$'));
            found
        });
        match start {
            Some(at) => !ends_with_member_dot(&lower[..at]),
            None => false,
        }
    }
}

pub(super) fn is_dart_member_read(r: &ResolveRefIn) -> bool {
    r.language == "dart" && r.reference_kind == "references"
        && re!(r"^[A-Za-z_$][A-Za-z0-9_$]*\.[A-Za-z_$][A-Za-z0-9_$]*$")
            .is_match(&r.reference_name)
}

impl KernelResolver {
    pub(super) fn dart_lineage(&mut self, type_name: &str) -> Res<Rc<HashMap<String, u32>>> {
        if let Some(hit) = self.dart_lineage_memo.get(type_name) { return Ok(hit.clone()); }
        let mut queue = VecDeque::from([(type_name.to_string(), 0u32)]);
        let mut depths = HashMap::new();
        while depths.len() < 40 {
            let Some((name, depth)) = queue.pop_front() else { break };
            if depths.contains_key(&name) { continue; }
            depths.insert(name.clone(), depth);
            for sup in self.dart_supertypes_of(&name)?.iter() {
                queue.push_back((sup.clone(), depth + 1));
            }
        }
        let depths = Rc::new(depths);
        self.dart_lineage_memo.insert(type_name.to_string(), depths.clone());
        Ok(depths)
    }

    fn dart_extension_owner(&mut self, member: &KNode) -> Res<Option<(Vec<String>, bool)>> {
        if let Some(hit) = self.dart_extension_owner_memo.get(&member.id) { return Ok(hit.clone()); }
        let found = if let Some(cut) = member.qualified_name.rfind("::") {
            let qn = &member.qualified_name[..cut];
            let owner = self.nodes_in_file(&member.file_path)?.iter().find(|n|
                n.qualified_name == qn && n.kind == "class"
                && n.start_line <= member.start_line && n.end_line >= member.start_line
            ).cloned();
            if let Some(owner) = owner {
                let head = self.dart_head_of(&owner);
                if head.extension {
                    // An unnamed extension is `extension on T`; a named one is
                    // `extension Name<T> on T`. Extension types are excluded by dart_head_of.
                    let named = self.read_file(&owner.file_path).is_some_and(|lines| {
                        let from = (owner.start_line - 1).max(0) as usize;
                        let to = (owner.start_line + 5).max(0) as usize;
                        let text = lines.iter().skip(from).take(to.saturating_sub(from)).cloned().collect::<Vec<_>>().join("\n");
                        let text = re!(r"/\*(?s:.)*?\*/|//[^\n]*").replace_all(&text, " ");
                        re!(r"(?-u:\b)extension\s+([A-Za-z_$][A-Za-z0-9_$]*)")
                            .captures(&text).is_some_and(|m| &m[1] != "on" && &m[1] != "type")
                    });
                    Some((head.supers, named))
                } else { None }
            } else { None }
        } else { None };
        self.dart_extension_owner_memo.insert(member.id.clone(), found.clone());
        Ok(found)
    }

    pub(super) fn is_dart_getter(&mut self, n: &KNode) -> bool {
        if n.language != "dart" || n.kind != "method" { return false; }
        if let Some(hit) = self.dart_getter_memo.get(&n.id) { return *hit; }
        let getter = self.read_file(&n.file_path).is_some_and(|lines| {
            let first = lines.get((n.start_line - 1).max(0) as usize).map(String::as_str).unwrap_or("");
            let next = lines.get(n.start_line.max(0) as usize).map(String::as_str).unwrap_or("");
            let head = format!("{} {}", super::names::js_slice(first, n.start_column.max(0) as usize), next);
            Self::cached_regex(&format!(r"^[^={{;]*?(?-u:\b)get\s+{}(?:$|[^A-Za-z0-9_$])", regex::escape(&n.name)))
                .ok().is_some_and(|pattern| pattern.is_match(&head))
        });
        self.dart_getter_memo.insert(n.id.clone(), getter);
        getter
    }

    /// Returns the member and whether an extension supplies it.
    pub(super) fn dart_member_of(&mut self, type_name: &str, name: &str,
        r: &ResolveRefIn, getters_only: bool) -> Res<Option<(Arc<KNode>, bool)>> {
        self.dart_member_of_in_library(type_name, name, r, getters_only, None)
    }

    /// Keep an inferred type tied to its declaring library while looking up members.
    pub(super) fn dart_member_of_in_library(&mut self, type_name: &str, name: &str,
        r: &ResolveRefIn, getters_only: bool, library: Option<&str>) -> Res<Option<(Arc<KNode>, bool)>> {
        let lineage = self.dart_lineage(type_name)?;
        let candidates: Vec<_> = self.nodes_by_name(name)?.iter().cloned().collect();
        let mut best: Option<(Arc<KNode>, u32)> = None;
        for n in candidates {
            if n.language != "dart" || !is_dart_member(&n)
                || (getters_only && !self.is_dart_getter(&n)) { continue; }
            let rank = if let Some((on, named)) = self.dart_extension_owner(&n)? {
                if !named && !self.dart_same_library(&r.file_path, &n.file_path) { continue; }
                let Some(depth) = on.iter().filter_map(|t| lineage.get(t)).min() else { continue };
                DART_EXTENSION_RANK + *depth
            } else {
                let owner = n.qualified_name.rsplit_once("::").map(|(o, _)| o.rsplit("::").next().unwrap_or(o)).unwrap_or("");
                let Some(depth) = lineage.get(owner) else { continue };
                if let Some(library) = library {
                    if (*depth == 0 && !self.dart_same_library(library, &n.file_path))
                        || (*depth != 0 && !self.dart_visible(library, &n.file_path, owner, "")) {
                        continue;
                    }
                }
                *depth
            };
            let replace = best.as_ref().is_none_or(|(old, old_rank)| {
                rank < *old_rank || (rank == *old_rank && (
                    (n.file_path == r.file_path && old.file_path != r.file_path)
                    || (old.file_path != r.file_path && n.file_path != r.file_path
                        && super::names::compute_path_proximity(&r.file_path, &n.file_path)
                            > super::names::compute_path_proximity(&r.file_path, &old.file_path))
                ))
            });
            if replace { best = Some((n, rank)); }
        }
        if getters_only {
            if let Some(depth) = self.dart_field_depth(type_name, name, r)? {
                if best.as_ref().is_some_and(|(_, rank)| depth <= *rank) { return Ok(None); }
            }
        }
        Ok(best.map(|(n, rank)| (n, rank >= DART_EXTENSION_RANK)))
    }

    /// Dart fields have no graph nodes. Reuse the source-backed member scan.
    pub(super) fn infer_dart_field_receiver_type(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if r.language != "dart" || !re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(receiver) { return Ok(None); }
        let nodes = self.nodes_in_file(&r.file_path)?;
        let cls = nodes.iter().filter(|n| n.language == "dart" && is_dart_type_kind(&n.kind)
            && n.start_line <= r.line && n.end_line >= r.line).max_by_key(|n| n.start_line).cloned();
        let Some(cls) = cls else { return Ok(None) };
        // An untyped local/parameter must not revive an enclosing field.
        if let Some(f) = nodes.iter().filter(|n| n.language == "dart" && matches!(n.kind.as_str(), "method" | "function")
            && n.start_line <= r.line && n.end_line >= r.line).max_by_key(|n| n.start_line) {
            if let Some(lines) = self.read_file(&f.file_path) {
                let body = lines.iter().skip((f.start_line - 1).max(0) as usize)
                    .take((r.line - f.start_line + 1).max(0) as usize).cloned().collect::<Vec<_>>().join("\n");
                let pattern = Self::cached_regex(&format!(r"(?-u:\b)(?:var|final|dynamic)\s+{}(?:$|[^A-Za-z0-9_$])", regex::escape(receiver)))?;
                if pattern.is_match(&body) { return Ok(None); }
            }
        }
        let mut queue = VecDeque::from([cls]);
        let mut seen = HashSet::new();
        while seen.len() < 8 {
            let Some(decl) = queue.pop_front() else { break };
            if !seen.insert(decl.id.clone()) { continue; }
            if let Some(ty) = self.declared_member_type(&decl, receiver)? { return Ok(Some(ty)); }
            for sup in self.dart_head_of(&decl).supers {
                queue.extend(self.nodes_by_name(&sup)?.iter().filter(|n| n.language == "dart" && is_dart_type_kind(&n.kind)).cloned());
            }
        }
        Ok(None)
    }

    pub(super) fn match_dart_member_read(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some((receiver, member)) = r.reference_name.split_once('.') else { return Ok(None) };
        let candidates: Vec<_> = self.nodes_by_name(member)?.iter().cloned().collect();
        if !candidates.iter().any(|n| self.is_dart_getter(n)) { return Ok(None); }
        let names = self.nodes_by_name(receiver)?;
        let project_type = receiver.starts_with(|c: char| c.is_ascii_uppercase())
            && names.iter().any(|n| n.language == "dart" && is_dart_type_kind(&n.kind));
        let ty = if project_type { Some(receiver.to_string()) } else {
            match self.infer_local_receiver_type(receiver, r, false)? {
                Some(ty) => Some(ty),
                None => self.infer_dart_field_receiver_type(receiver, r)?,
            }
        };
        let Some(ty) = ty else { return Ok(None) };
        Ok(self.dart_member_of(&ty, member, r, true)?.map(|(node, _)| KCand {
            node, confidence: 0.9, resolved_by: "instance-method",
        }))
    }
}
