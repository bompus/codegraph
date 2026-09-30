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
    namespace: String,
    uses: HashMap<String, String>,
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
    for m in re!(r"(?m)^\s*use\s+(?:function\s+|const\s+)?([A-Za-z0-9_\\]+)(?:\s+as\s+([A-Za-z0-9_]+))?\s*;")
        .captures_iter(&text[..header_end])
    {
        let fqn = m[1].strip_prefix('\\').unwrap_or(&m[1]).to_string();
        let alias = m
            .get(2)
            .map(|a| a.as_str().to_string())
            .unwrap_or_else(|| fqn.rsplit('\\').next().unwrap_or("").to_string());
        uses.insert(alias, fqn);
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
    pub(super) fn is_php_method_in_scope(&mut self, method: &KNode, r: &ResolveRefIn, via: PhpVia) -> Res<bool> {
        let Some(cut) = method.qualified_name.rfind("::") else { return Ok(true) };
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
        let Some(enclosing) = enclosing.filter(|e| e.kind != "trait") else { return Ok(true) };
        let start = match via {
            PhpVia::Parent => self.php_supertype_qns(&enclosing)?.as_ref().clone(),
            PhpVia::SelfClass => vec![enclosing.qualified_name.clone()],
        };
        let (up, leaves_repo) = self.php_ancestry(start)?;
        if up.contains(&owner) {
            return Ok(true);
        }
        // A base class may call what a subclass defines: the owner descends
        // from the caller.
        if via == PhpVia::SelfClass && self.php_ancestry(vec![owner.clone()])?.0.contains(&enclosing.qualified_name) {
            return Ok(true);
        }
        // Past an ancestor outside the repository its members are unseen, the
        // repository's traits it uses among them: a trait's method may still
        // be the one meant, an unrelated class's never is.
        Ok(leaves_repo && self.nodes_by_qualified_name(&owner)?.iter().any(|d| d.kind == "trait"))
    }

    /// phpAncestry: every type `start` reaches through `extends` and trait
    /// `use`, and whether it left the repository on the way.
    fn php_ancestry(&mut self, start: Vec<String>) -> Res<(HashSet<String>, bool)> {
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

    /// phpTypeQn: the qualified name (`A\B::C`) a PHP class name written in
    /// `file` refers to.
    fn php_type_qn(&mut self, name: &str, file: &str) -> String {
        let fqn = if let Some(abs) = name.strip_prefix('\\') {
            abs.to_string()
        } else {
            let source = self.read_file(file);
            let scope = source.as_ref().map(|f| f.php_file_scope());
            let (head, rest) = match name.find('\\') {
                Some(at) => (&name[..at], &name[at..]),
                None => (name, ""),
            };
            if let Some(imported) = scope.and_then(|s| s.uses.get(head)) {
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

    /// phpSupertypeQns: the qualified names a PHP class or trait extends and
    /// the traits it uses.
    fn php_supertype_qns(&mut self, decl: &KNode) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.php_supers_memo.get(&decl.id) {
            return Ok(hit.clone());
        }
        let mut names: Vec<String> = Vec::new();
        if let Some(lines) = self.read_file(&decl.file_path) {
            let len = lines.len();
            let from = ((decl.start_line - 1).max(0) as usize).min(len);
            let to = ((decl.start_line + 4).max(0) as usize).min(len);
            let head = lines[from..to.max(from)].join(" ");
            if let Some(m) = re!(r"(?-u:\b)extends\s+([^{]*?)(?:(?-u:\b)implements(?-u:\b)|\{)").captures(&head) {
                for t in re!(r"\\?[A-Za-z_][A-Za-z0-9_\\]*").find_iter(&m[1]) {
                    names.push(t.as_str().to_string());
                }
            }
            // `use StringTranslationTrait, MessengerTrait;` at the top of the
            // body — one per line or one list across several.
            let body_from = (decl.start_line.max(0) as usize).min(len);
            let body_to = (decl.end_line.min(decl.start_line + 80).max(0) as usize).min(len);
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
        }
        let qns: Vec<String> = names.iter().map(|n| self.php_type_qn(n, &decl.file_path)).collect();
        let qns = Rc::new(qns);
        self.php_supers_memo.insert(decl.id.clone(), qns.clone());
        Ok(qns)
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
        lines
            .get((candidate.start_line - 1).max(0) as usize)
            .is_some_and(|line| re!(r"^\s*local(?-u:\b)").is_match(line))
    }
}
