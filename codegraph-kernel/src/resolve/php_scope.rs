//! Scope rules a bare method name obeys in PHP (`$this->m()`, `self::m()`,
//! `parent::m()`: name-matcher.ts phpSelfReceiver / isPhpMethodInScope), in
//! a Vue Options API component (`this.m()` inside it: isVueComponentMethod)
//! and in Lua (a `local` belongs to its chunk: isLuaLocal).

use super::*;

const PHP_ANCESTRY_CAP: usize = 80;

/// A PHP file's `namespace` and its header `use A\B\C [as D];` imports,
/// alias → fully qualified name (phpFileScope).
#[derive(Default)]
pub(crate) struct PhpFileScope {
    pub(crate) namespace: String,
    pub(crate) uses: HashMap<String, String>,
}

pub(super) fn collect_php_file_scope(text: &str) -> PhpFileScope {
    let namespace = re!(r"(?m)^\s*namespace\s+([A-Za-z0-9_\\]+)\s*[;{]")
        .captures(text)
        .map(|m| m[1].to_string())
        .unwrap_or_default();
    // File-level imports sit before the first type; a trait `use` inside a
    // class body is not one.
    let header_end = re!(r"(?m)^\s*(?:(?:abstract|final|readonly)\s+)*(?:class|trait|interface|enum)\s")
        .find(text)
        .map(|m| m.start())
        .filter(|&at| at > 0)
        .unwrap_or(text.len());
    let mut uses = HashMap::new();
    // `use A\B [as C], D\E;` and the grouped `use A\{B, C as D};`.
    for m in re!(r"(?m)^\s*use\s+(?:function\s+|const\s+)?([A-Za-z0-9_\\\s,{}]+?)\s*;")
        .captures_iter(&text[..header_end])
    {
        let body = &m[1];
        let (prefix, items) = match (body.find('{'), body.rfind('}')) {
            (Some(open), Some(close)) if open < close => (body[..open].trim(), &body[open + 1..close]),
            _ => ("", body),
        };
        for item in items.split(',') {
            let Some(c) = re!(r"^\s*([A-Za-z0-9_\\]+)(?:\s+as\s+([A-Za-z0-9_]+))?\s*$").captures(item) else { continue };
            let joined = format!("{prefix}{}", &c[1]);
            let fqn = joined.strip_prefix('\\').unwrap_or(&joined).to_string();
            let alias = c
                .get(2)
                .map(|a| a.as_str().to_string())
                .unwrap_or_else(|| fqn.rsplit('\\').next().unwrap_or("").to_string());
            // PHP names resolve case-insensitively (`new field\X()` finds the
            // `Field` alias), so the map keys the alias lowercased.
            uses.insert(alias.to_lowercase(), fqn);
        }
    }
    PhpFileScope { namespace, uses }
}

fn is_php_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "trait" | "interface" | "enum")
}

/// How a PHP method call was written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PhpVia {
    /// `$this->m()` / `self::m()` / `static::m()`: the enclosing class's own
    /// or inherited method.
    SelfClass,
    /// `parent::m()`: an ancestor's.
    Parent,
}

/// How a PHP method relates to the class hierarchy around a `$this`/`self`/
/// `parent` call.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PhpScope {
    /// A method of the enclosing class, a repository ancestor, a trait one of
    /// them uses, or a subclass (for `$this`/`self`).
    Bound,
    /// A repository trait's method, admitted only because the hierarchy
    /// reaches a class outside the repository whose traits are unseen.
    UnseenAncestor,
    /// Anything else.
    Out,
}

/// A method a Vue Options API component declares for itself
/// (`index::handleLogin` in `index.vue`).
pub(super) fn is_vue_component_method(n: &KNode) -> bool {
    if n.kind != "method" {
        return false;
    }
    let Some(stem) = n.file_path.strip_suffix(".vue") else { return false };
    let base = stem.rsplit('/').next().unwrap_or(stem);
    n.qualified_name.len() == base.len() + 2 + n.name.len()
        && n.qualified_name.starts_with(base)
        && n.qualified_name[base.len()..].starts_with("::")
        && n.qualified_name.ends_with(n.name.as_str())
}

impl KernelResolver {
    pub(super) fn php_class_visible(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "php" || n.language != "php" || !is_php_type_kind(&n.kind)
            || !re!(r"^[A-Za-z_]\w*$").is_match(&r.reference_name)
            || re!(r"(?i)^(?:self|static|parent)$").is_match(&r.reference_name) { return Ok(true); }
        // Extraction keeps a PHP type path's leaf; honor the written qualifier.
        if let Some(source)=self.read_file(&r.file_path) {
            if let Some(line)=source.get((r.line-1).max(0) as usize) {
                let pattern=Self::cached_regex(&format!(r"(?:^|[^\w\\])((?:[A-Za-z_]\w*\\)+{})\b",regex::escape(&r.reference_name)))?;
                if let Some(path)=pattern.captures(line) {return Ok(n.qualified_name.replace("::","\\").eq_ignore_ascii_case(&path[1]));}
            }
        }
        let source = self.read_file(&r.file_path);
        let scope = source.as_ref().map(|s| s.php_file_scope());
        let Some(scope) = scope else { return Ok(true); };
        let fqn = n.qualified_name.replace("::", "\\");
        let expected = scope.uses.get(&r.reference_name.to_lowercase()).cloned().unwrap_or_else(|| if scope.namespace.is_empty() { r.reference_name.clone() } else { format!("{}\\{}",scope.namespace,r.reference_name) });
        Ok(fqn.eq_ignore_ascii_case(&expected))
    }

    /// phpSelfReceiver (name-matcher.ts): how a bare PHP method name was
    /// written at its call site; `None` for anything but `$this->`,
    /// `self::`, `static::` and `parent::`.
    pub(super) fn php_self_receiver(&mut self, r: &ResolveRefIn) -> Res<Option<PhpVia>> {
        if r.language != "php" || r.reference_kind != "calls" || r.reference_name.is_empty() || !r.reference_name.bytes().all(is_word_byte) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(None) };
        let name = &r.reference_name;
        if Self::cached_regex(&format!(
            r"\$this\s*\??->\s*{name}\s*\(|(?-u:\b)(?:self|static)\s*::\s*{name}\s*\("
        ))?
        .is_match(line)
        {
            return Ok(Some(PhpVia::SelfClass));
        }
        if Self::cached_regex(&format!(r"(?-u:\b)parent\s*::\s*{name}\s*\("))?.is_match(line) {
            return Ok(Some(PhpVia::Parent));
        }
        Ok(None)
    }

    /// isPhpMethodInScope (name-matcher.ts): `method` belongs to the class
    /// the call is written in, one it extends, or a trait any of them uses —
    /// read from source and resolved the way PHP resolves a class name,
    /// through the file's `namespace` and `use` imports. A parent outside the
    /// repository ends the chain.
    pub(super) fn php_method_scope(&mut self, method: &KNode, r: &ResolveRefIn, via: PhpVia) -> Res<PhpScope> {
        let Some(cut) = method.qualified_name.rfind("::") else { return Ok(PhpScope::Bound) };
        let owner = method.qualified_name[..cut].to_string();
        let mut enclosing: Option<Arc<KNode>> = None;
        for n in self.nodes_in_file(&r.file_path)?.iter() {
            if is_php_type_kind(&n.kind)
                && n.start_line <= r.line
                && n.end_line >= r.line
                && enclosing.as_ref().is_none_or(|e| n.start_line > e.start_line)
            {
                enclosing = Some(n.clone());
            }
        }
        // Inside a trait, `$this` is whichever class uses it.
        let Some(enclosing) = enclosing.filter(|e| e.kind != "trait") else { return Ok(PhpScope::Bound) };
        let start = match via {
            PhpVia::Parent => self.php_supertype_qns(&enclosing)?.as_ref().clone(),
            PhpVia::SelfClass => vec![enclosing.qualified_name.clone()],
        };
        let (up, leaves_repo) = self.php_ancestry(start)?;
        if up.contains(&owner) {
            return Ok(PhpScope::Bound);
        }
        // A base class may call what a subclass defines: the owner descends
        // from the caller.
        if via == PhpVia::SelfClass && self.php_ancestry(vec![owner.clone()])?.0.contains(&enclosing.qualified_name) {
            return Ok(PhpScope::Bound);
        }
        // Past an ancestor outside the repository its members are unseen, the
        // repository's traits it uses among them (Orchestra's TestCase uses
        // Laravel's testing traits): a trait's method may still be the one
        // meant, an unrelated class's never is. Nothing in the repository
        // binds it, so the caller keeps it only below the trusted confidence.
        // Nor a trait's, for a call written `parent::x()`: that is what the
        // outside parent declares.
        if via == PhpVia::SelfClass && leaves_repo && self.nodes_by_qualified_name(&owner)?.iter().any(|d| d.kind == "trait") {
            return Ok(PhpScope::UnseenAncestor);
        }
        Ok(PhpScope::Out)
    }

    /// phpReceiverReaches (name-matcher.ts): whether a PHP receiver is named
    /// after a class that has `method` in its ancestry — the whole name or
    /// its last camel word (`$page->save()` → Page, which extends Entity;
    /// `$newRole->users()` → Role). BookStack's `$role->save()` is not
    /// Entity's: Role is a Model.
    pub(super) fn php_receiver_reaches(&mut self, receiver: &str, method: &KNode) -> Res<bool> {
        let Some(cut) = method.qualified_name.rfind("::") else { return Ok(false) };
        let owner = &method.qualified_name[..cut];
        let last = receiver.rsplit('.').next().unwrap_or("");
        let last = last.strip_prefix('$').unwrap_or(last);
        if last.is_empty() {
            return Ok(false);
        }
        let words = split_camel_case(last);
        let mut names: Vec<String> = vec![capitalize_first(last)];
        if let Some(tail) = words.last().map(|w| capitalize_first(w)) {
            if !names.contains(&tail) {
                names.push(tail);
            }
        }
        for name in names {
            let decls: Vec<String> = self
                .nodes_by_name(&name)?
                .iter()
                .filter(|d| d.language == "php" && is_php_type_kind(&d.kind))
                .map(|d| d.qualified_name.clone())
                .collect();
            for qn in decls {
                if self.php_ancestry(vec![qn])?.0.contains(owner) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// phpAncestry: every type `start` reaches through `extends` and trait
    /// `use`, and whether it left the repository on the way.
    pub(super) fn php_ancestry(&mut self, start: Vec<String>) -> Res<(HashSet<String>, bool)> {
        let mut qns: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = start.into();
        let mut leaves_repo = false;
        while qns.len() < PHP_ANCESTRY_CAP {
            let Some(qn) = queue.pop_front() else { break };
            if !qns.insert(qn.clone()) {
                continue;
            }
            let decls: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&qn)?
                .iter()
                .filter(|d| d.language == "php" && is_php_type_kind(&d.kind))
                .cloned()
                .collect();
            if decls.is_empty() {
                leaves_repo = true;
            }
            for decl in decls {
                queue.extend(self.php_supertype_qns(&decl)?.iter().cloned());
            }
        }
        Ok((qns, leaves_repo))
    }

    /// The qualified name of the project type a PHP class name written in
    /// `file` means (its namespace, or the `use` that imports it, alias
    /// included), when the index holds one.
    pub(super) fn php_declared_type_qn(&mut self, name: &str, file: &str) -> Res<Option<String>> {
        let qn = self.php_type_qn(name, file);
        let known = self
            .nodes_by_qualified_name(&qn)?
            .iter()
            .any(|n| n.language == "php" && is_php_type_kind(&n.kind));
        Ok(known.then_some(qn))
    }

    /// Among several same-named PHP types, a class name written at `r` means
    /// the one its file's namespace or `use` names (`Sub` in a file with no
    /// namespace is the global `Sub`, never `Other\Sub`). Other candidates,
    /// and a name that names no indexed type, are left alone.
    pub(super) fn retain_php_declared_type(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let is_type = |n: &KNode| n.language == "php" && is_php_type_kind(&n.kind);
        if r.language != "php" || candidates.iter().filter(|n| is_type(n)).count() < 2 {
            return Ok(candidates);
        }
        // `Other\Sub $s` reaches here as `Sub`; the spelling decides.
        let spelled = self
            .read_file(&r.file_path)
            .and_then(|lines| names::qualified_spelling(&lines, r))
            .unwrap_or_else(|| r.reference_name.clone());
        let Some(qn) = self.php_declared_type_qn(&spelled, &r.file_path)? else {
            return Ok(candidates);
        };
        Ok(candidates.into_iter().filter(|n| !is_type(n) || n.qualified_name == qn).collect())
    }

    /// phpTypeQn: the qualified name (`A\B::C`) a PHP class name written in
    /// `file` refers to.
    pub(super) fn php_type_qn(&mut self, name: &str, file: &str) -> String {
        let fqn = if let Some(abs) = name.strip_prefix('\\') {
            abs.to_string()
        } else {
            let source = self.read_file(file);
            let scope = source.as_ref().map(|f| f.php_file_scope());
            let (head, rest) = match name.find('\\') {
                Some(at) => (&name[..at], &name[at..]),
                None => (name, ""),
            };
            if let Some(imported) = scope.and_then(|s| s.uses.get(&head.to_lowercase())) {
                format!("{imported}{rest}")
            } else if let Some(s) = scope.filter(|s| !s.namespace.is_empty()) {
                format!("{}\\{name}", s.namespace)
            } else {
                name.to_string()
            }
        };
        match fqn.rfind('\\') {
            Some(at) => format!("{}::{}", &fqn[..at], &fqn[at + 1..]),
            None => fqn,
        }
    }

    /// phpSupertypeQns: the qualified names of the traits a PHP class or
    /// trait uses, then those it extends — the order PHP finds a method in.
    /// A declaration starts at its attributes, and its ancestors are read
    /// through the imports of the file declaring it.
    fn php_supertype_qns(&mut self, decl: &KNode) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.php_supers_memo.get(&decl.id) {
            return Ok(hit.clone());
        }
        let mut names: Vec<String> = Vec::new();
        if let Some(lines) = self.read_file(&decl.file_path) {
            let len = lines.len();
            let start = ((decl.start_line - 1).max(0) as usize).min(len);
            let end = (decl.end_line.max(0) as usize).clamp(start, len);
            // A declaration starts at its attributes: `#[ContentEntityType(…)]`
            // may run for dozens of lines above `class Term extends …`.
            let named = Self::cached_regex(&format!(
                r"(?i)(?-u:\b)(?:class|interface|trait|enum)\s+{}(?-u:\b)",
                regex::escape(&decl.name)
            ))?;
            let mut at = start;
            while at + 1 < end && !named.is_match(lines.get(at).map(String::as_str).unwrap_or("")) {
                at += 1;
            }
            if at + 1 >= end {
                at = start;
            }
            // `use StringTranslationTrait, MessengerTrait;` at the top of the
            // body — one per line or one list across several.
            let body_from = (at + 1).min(len);
            let body_to = len.min(at + 81);
            let body_lines = &lines[body_from..body_to.max(body_from)];
            let first_function = body_lines
                .iter()
                .position(|l| re!(r"(?-u:\b)function(?-u:\b)").is_match(l))
                .unwrap_or(body_lines.len());
            let body = body_lines[..first_function].join("\n");
            for used in re!(r"(?m)^\s*use\s+([A-Za-z0-9_\\,\s]+?)\s*[;{]").captures_iter(&body) {
                for t in used[1].split(',') {
                    let t = t.trim();
                    if !t.is_empty() {
                        names.push(t.to_string());
                    }
                }
            }
            let head = lines[at.min(len)..len.min(at + 12)].join(" ");
            if let Some(m) = re!(r"(?-u:\b)extends\s+([^{]*?)(?:(?-u:\b)implements(?-u:\b)|\{)").captures(&head) {
                for t in re!(r"\\?[A-Za-z_][A-Za-z0-9_\\]*").find_iter(&m[1]) {
                    names.push(t.as_str().to_string());
                }
            }
        }
        let qns: Vec<String> = names.iter().map(|n| self.php_type_qn(n, &decl.file_path)).collect();
        let qns = Rc::new(qns);
        self.php_supers_memo.insert(decl.id.clone(), qns.clone());
        Ok(qns)
    }

    /// The method `name` a PHP type declares itself, in its own file.
    pub(super) fn php_own_method(&mut self, cls: &KNode, name: &str) -> Res<Option<Arc<KNode>>> {
        let owner = format!("{}::{name}", cls.qualified_name);
        Ok(self
            .nodes_in_file(&cls.file_path)?
            .iter()
            .find(|n| n.kind == "method" && n.name == name && n.qualified_name == owner)
            .cloned())
    }

    /// phpAncestorMethod: the method `name` a PHP type reaches past its own —
    /// in a trait it uses, then up what it extends, each found the way PHP
    /// finds a class name. Laravel's `Pivot` takes `getForeignKey` from its
    /// trait `AsPivot`, not from `Model` above it.
    pub(super) fn php_ancestor_method(
        &mut self,
        decl: &KNode,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Res<Option<Arc<KNode>>> {
        let supertypes = self.php_supertype_qns(decl)?;
        for qn in supertypes.iter() {
            if !seen.insert(qn.clone()) {
                continue;
            }
            let decls: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(qn)?
                .iter()
                .filter(|n| n.language == "php" && is_php_type_kind(&n.kind))
                .cloned()
                .collect();
            for up in decls {
                if let Some(found) = self.php_own_method(&up, name)? {
                    return Ok(Some(found));
                }
                if let Some(found) = self.php_ancestor_method(&up, name, seen)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    /// phpTypeMethod: the method `name` of the PHP type qualified `qn`, its own
    /// or one it reaches. `Some(None)` when the project declares the type and
    /// it has no such method, `None` when it is no project type.
    pub(super) fn php_type_method(&mut self, qn: &str, name: &str) -> Res<Option<Option<Arc<KNode>>>> {
        let key = (qn.to_string(), name.to_string());
        if let Some(hit) = self.php_type_methods_memo.get(&key) {
            return Ok(hit.clone());
        }
        let decls: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(qn)?
            .iter()
            .filter(|n| n.language == "php" && is_php_type_kind(&n.kind))
            .cloned()
            .collect();
        let mut found = if decls.is_empty() { None } else { Some(None) };
        for decl in decls {
            let mut method = self.php_own_method(&decl, name)?;
            if method.is_none() {
                method = self.php_ancestor_method(&decl, name, &mut HashSet::new())?;
            }
            if let Some(method) = method {
                found = Some(Some(method));
                break;
            }
        }
        self.php_type_methods_memo.insert(key, found.clone());
        Ok(found)
    }

    /// Whether the PHP file names `cls` by its short name: its own namespace or
    /// a `use` that imports it. A variable named after a class the file can't
    /// name is named after a word.
    pub(super) fn php_file_names_class(&mut self, cls: &KNode, file: &str) -> Res<bool> {
        let Some(source) = self.read_file(file) else { return Ok(false) };
        let scope = source.php_file_scope();
        let fqn = cls.qualified_name.replace("::", "\\").to_ascii_lowercase();
        let short = if scope.namespace.is_empty() {
            cls.name.to_ascii_lowercase()
        } else {
            format!("{}\\{}", scope.namespace, cls.name).to_ascii_lowercase()
        };
        if fqn == short {
            return Ok(true);
        }
        Ok(scope.uses.values().any(|used| used.to_ascii_lowercase() == fqn))
    }

    /// phpUnnamedClassMethod: the method a variable named after a PHP class
    /// reaches when the calling file can't name that class, so that only a
    /// word ties them: the class's own, or what it inherits when it is the one
    /// class of that name.
    pub(super) fn php_unnamed_class_method(
        &mut self,
        cls: &KNode,
        name: &str,
        only_of_name: bool,
    ) -> Res<Option<Arc<KNode>>> {
        if let Some(own) = self.php_own_method(cls, name)? {
            return Ok(Some(own));
        }
        if only_of_name {
            return self.php_ancestor_method(cls, name, &mut HashSet::new());
        }
        Ok(None)
    }

    /// phpFacadeMethod: the method a Laravel facade hands a static call on to —
    /// the class `getFacadeAccessor` returns, as in `return ParallelTesting::class;`.
    pub(super) fn php_facade_method(&mut self, facade: &KNode, name: &str) -> Res<Option<Arc<KNode>>> {
        if !self.php_supertype_qns(facade)?.iter().any(|qn| qn.ends_with("::Facade")) {
            return Ok(None);
        }
        let Some(accessor) = self.php_own_method(facade, "getFacadeAccessor")? else { return Ok(None) };
        let Some(lines) = self.read_file(&facade.file_path) else { return Ok(None) };
        let from = ((accessor.start_line - 1).max(0) as usize).min(lines.len());
        let to = (accessor.end_line.max(0) as usize).clamp(from, lines.len());
        let text = lines[from..to].join("\n");
        let Some(named) = re!(r"(?-u:\b)return\s+(\\?[A-Za-z_][A-Za-z0-9_\\]*)::class\s*;")
            .captures(&text)
            .map(|c| c[1].to_string())
        else {
            return Ok(None);
        };
        let qn = self.php_type_qn(&named, &facade.file_path);
        Ok(self.php_type_method(&qn, name)?.flatten())
    }

    /// Eloquent's static methods that hand back a model of the class they are
    /// called on: `$post = PostStringPrimaryKey::query()->create([…])`.
    pub(super) fn php_model_factory_local_type(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if !re!(r"^\w+$").is_match(receiver) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let from = (self.enclosing_scope_start_line(&r.file_path, &r.language, r.line)? - 1).max(0) as usize;
        let to = ((r.line - 1).max(0) as usize).min(lines.len());
        let variable = format!("${receiver}");
        let pattern = Self::cached_regex(
            r"^\s*(?:(?:\$\w+\s*=\s*)*)(\\?[A-Z][\w\\]*)::(?:query\(\)\s*->\s*)?(?:create|forceCreate|firstOrCreate|firstOrNew|updateOrCreate|find|findOrFail)\s*\(",
        )?;
        for i in (from..=to).rev() {
            let Some(line) = lines.get(i) else { continue };
            let Some(at) = line.find(&variable) else { continue };
            if !line[..at].trim().is_empty() {
                continue;
            }
            let Some(value) = re!(r"^\s*=\s*([^=].*)$").captures(&line[at + variable.len()..]) else { continue };
            let Some(named) = pattern.captures(&value[1]).map(|c| c[1].to_string()) else { return Ok(None) };
            let qn = self.php_type_qn(&named, &r.file_path);
            let decls: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&qn)?
                .iter()
                .filter(|n| n.language == "php" && n.kind == "class")
                .cloned()
                .collect();
            for decl in decls {
                if self.php_ancestry(vec![decl.qualified_name.clone()])?.0.iter().any(|q| q.ends_with("::Model")) {
                    return Ok(Some(named));
                }
            }
            return Ok(None);
        }
        Ok(None)
    }

    /// isThisCallInOwnFile: is `r` written as `this.<name>(` in the file
    /// that declares `n`?
    pub(super) fn is_this_call_in_own_file(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if n.file_path != r.file_path {
            return Ok(false);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(false) };
        let re = Self::cached_regex(&format!(r"(?-u:\b)this\s*\??\.\s*{}\s*\(", regex::escape(&n.name)))?;
        Ok(re.is_match(line))
    }

    /// isLuaLocal: a Lua variable or function declared `local` (`local x =
    /// …`, `local function f`, `local a, x = …`).
    pub(super) fn is_lua_local(&mut self, candidate: &KNode) -> bool {
        if !matches!(candidate.kind.as_str(), "variable" | "constant" | "function") {
            return false;
        }
        let Some(lines) = self.read_file(&candidate.file_path) else { return false };
        let site = ResolveRefIn { row_id: None, from_node_id: candidate.id.clone(), reference_name: candidate.name.clone(), reference_kind: "references".to_string(), line: candidate.start_line, column: candidate.start_column, candidates: None, file_path: candidate.file_path.clone(), language: candidate.language.clone(), failure_reason: None, generated: None };
        if let Some(tree) = self.parsed_tree(&lines, &site) {
            let mut node = super::iteration::descendant_for_position(tree.root_node(), lines.text(), ((site.line - 1).max(0) as usize, site.column.max(0) as usize));
            while let Some(parent) = node.parent() {
                if matches!(parent.kind(), "variable_declaration" | "function_declaration") {
                    return lines.text()[parent.start_byte()..parent.end_byte()].trim_start().starts_with("local ");
                }
                node = parent;
            }
        }
        lines
            .get((candidate.start_line - 1).max(0) as usize)
            .is_some_and(|line| re!(r"^\s*local(?-u:\b)").is_match(line))
    }
}
