//! Python module globals as method-value receivers (#1820, #2074):
//! `pool.submit(settings.conn.fetch)`, where `conn = None` is rebound by
//! `global conn; conn = Store()`. The global has no static type, so its type
//! is the set of classes its module, and production code in other modules,
//! assign to it. Anything else that may rebind it (an opaque value, a tuple
//! target, `for`/`with`/import, a star import, `setattr`, a namespace-dict
//! write) leaves the type unknown, and the ref resolves as it did before
//! globals were typed. A known type that can't name one callable member
//! gives no edge: a wrong callback edge is worse than none.

use super::bound::{python_def_params, python_param_names, PyFile};
use super::*;
use std::collections::VecDeque;

/// The declarations `python_shared_declaration` walks before it gives up.
const DECLARATION_WALK_LIMIT: usize = 32;

/// The `python_write_patterns` sets kept before the memo starts over.
const WRITE_PATTERN_LIMIT: usize = 1024;

/// The `attr_writers` key for files with a `setattr`, `patch.object` or
/// `__dict__` write whose computed key may name any global.
const COMPUTED_WRITE: &str = "*";

/// A `setattr`, `patch.object` or `__dict__` write with a computed key (an
/// expression, or a quoted key with an escape), on any receiver (statement
/// text with strings kept).
const COMPUTED_KEY: &str = r#"\b(?:[\w.]+\.)?(?:setattr|patch\.object)\s*\(\s*[^\s,()'"]+\s*,\s*(?:[^'"\s]|['"][^'"]*\\|['"][^'"]*['"]\s*[^,)\s])|\S+?\.__dict__\s*(?:\[\s*(?:[^'"\s]|['"][^'"]*\\|['"][^'"]*['"]\s*[^\]\s])|\.\s*update\s*\()"#;

/// Every attribute name the indexed Python files may write through
/// `x.name = …`, `setattr`, `patch.object` or `__dict__`: the files that
/// do, by name, numbered by their place in `files`.
pub(super) struct PyAttrWriters {
    files: Arc<Vec<String>>,
    by_name: HashMap<String, Vec<u32>>,
}

/// Per-run memos for module-global typing.
#[derive(Default)]
pub(super) struct PyGlobalsMemo {
    /// The classes a global can hold (`None`: unknown), by global id.
    classes: HashMap<String, Option<Rc<Vec<Arc<KNode>>>>>,
    /// Writes to a global from other files, by global id.
    external: HashMap<String, Rc<ExternalWrites>>,
    /// `python_global_bindings`, by (file, name).
    bindings: HashMap<(String, String), Rc<Vec<PyBinding>>>,
    /// The statement lines of a file's top-level `__main__` blocks, by file.
    script: HashMap<String, Rc<HashSet<usize>>>,
    /// The files a dotted module path names, by (dotted path, the importing
    /// file's directory for a relative path, else empty).
    module_files: HashMap<(String, String), Rc<Vec<String>>>,
    /// This resolver's handle on `PyAttrWriters`.
    attr_writers: Option<Arc<PyAttrWriters>>,
    /// `python_explicit_alias`, by (file, module, local name).
    explicit: HashMap<(String, String, String), bool>,
    /// `python_test_writer`, by (global id, file).
    test_writers: HashMap<(String, String), bool>,
    /// `python_write_patterns`, by receiver spelling and name.
    patterns: HashMap<String, Rc<[Regex; 5]>>,
}

/// Writes to a module global from other production files: constructor
/// writes (type, file, statement line), and whether any other write makes
/// the type unknown.
#[derive(Default)]
pub(super) struct ExternalWrites {
    writes: Vec<(String, String, usize)>,
    unknown: bool,
}

/// `python_file_writes` for one file: `ExternalWrites`' fields, and for a
/// test file whether it writes the global at all.
#[derive(Default)]
struct FileWrites {
    writes: Vec<(String, String, usize)>,
    unknown: bool,
    writer: bool,
}

/// How one statement binds a name.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum PyBinding {
    /// `global name`.
    Global,
    /// `name = value` or `name: T [= value]` with one plain target: the
    /// annotation, the value's code, and the statement's line.
    Assign { ty: Option<String>, value: String, line: usize },
    /// An import that binds the name.
    Import,
    /// A `def` or `class` of the name.
    Def,
    /// Any other binding: a tuple or augmented target, a loop, `with`/`except`
    /// `as`, a `case` pattern, a lambda parameter, `:=`, `del`, `nonlocal`, a
    /// star import.
    Other,
}

impl KernelResolver {
    /// `mod.global.member` or an imported `global.member`, never a deeper
    /// chain: the global's member, when the receiver names an imported module
    /// global the file imports from one source, never rebinds other than by
    /// import, and the calling functions don't bind. `mod` may be a dotted
    /// module spelled in full (`import pkg.settings; pkg.settings.conn`).
    /// `None`: not such a global, so the import strategies decide.
    pub(super) fn python_imported_global(&mut self, receiver: &str, member: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let segments: Vec<&str> = receiver.split('.').collect();
        if segments.len() > 2 {
            // `import pkg.settings as pkg` binds `pkg` to the settings module.
            let module = &receiver[..receiver.rfind('.').unwrap_or(0)];
            let plain = self.import_mappings(&r.file_path)?.iter().any(|m| m.is_namespace && m.local_name == segments[0] && m.source == module);
            if !plain || self.python_explicit_alias(&r.file_path, module, segments[0])? {
                return Ok(None);
            }
        }
        // `import calendar; calendar.main`: the receiver is the module, not
        // the module's own `calendar` global.
        if segments.len() == 1 && self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == receiver && m.is_namespace) {
            return Ok(None);
        }
        let Some(hit) = self.resolve_via_import(&r.clone().naming(receiver, "references"))? else {
            return Ok(None);
        };
        let global = hit.node;
        let root = segments[0];
        if Some(&global.name.as_str()) != segments.last()
            || !self.is_python_module_global(&global)?
            || !self.python_global_typed(&global, r)?
            || self.python_import_sources(root, &r.file_path)? != 1
            || self.python_global_bindings(root, &r.file_path)?.iter().any(|b| *b != PyBinding::Import)
            || self.python_binds_locally(root, &r.file_path, r.line, false)?
        {
            return Ok(None);
        }
        self.python_global_members(&global, member, r)
    }

    /// `global.member` for a module global of the ref's own file the calling
    /// functions don't bind. `None`: no such global.
    pub(super) fn python_same_file_global(&mut self, receiver: &str, member: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let candidates: Vec<Arc<KNode>> =
            self.nodes_in_file(&r.file_path)?.iter().filter(|n| n.name == receiver).cloned().collect();
        for global in candidates {
            if self.is_python_module_global(&global)? {
                if !self.python_global_typed(&global, r)? || self.python_binds_locally(receiver, &r.file_path, r.line, true)? {
                    return Ok(None);
                }
                return self.python_global_members(&global, member, r);
            }
        }
        Ok(None)
    }

    /// A module-scope Python variable, not a class attribute or a local, that
    /// no module-scope `class` or `def` of the same name rebinds.
    fn is_python_module_global(&mut self, node: &KNode) -> Res<bool> {
        if node.language != "python" || !(node.kind == "variable" || node.kind == "constant") {
            return Ok(false);
        }
        let nodes = self.nodes_in_file(&node.file_path)?;
        let nested = |line: i64| {
            nodes.iter().any(|n| {
                matches!(n.kind.as_str(), "class" | "function" | "method") && n.start_line < line && n.end_line >= line
            })
        };
        let rebound = nodes
            .iter()
            .any(|n| n.name == node.name && matches!(n.kind.as_str(), "class" | "function") && !nested(n.start_line));
        Ok(!rebound && !nested(node.start_line))
    }

    /// Whether `python_global_members` can settle a ref to `global` from
    /// `r`'s file: the global's classes are known, or the file is a test
    /// that installs a double. Both are memoized, so callers ask this before
    /// the per-ref shadow checks.
    fn python_global_typed(&mut self, global: &KNode, r: &ResolveRefIn) -> Res<bool> {
        Ok(self.python_test_writer(global, &r.file_path)? || self.python_global_classes(global, r)?.is_some())
    }

    /// The member a module global's value can dispatch to: one class's own
    /// member; for several, the nearest declaration they all inherit. A ref
    /// in a test file that installs a double on the global, or one where any
    /// of the classes lacks a callable member, resolves nothing. `None`: the
    /// global's classes are unknown, so the other strategies decide, as they
    /// did before globals were typed.
    fn python_global_members(&mut self, global: &KNode, member: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if self.python_test_writer(global, &r.file_path)? {
            return Ok(Some(None));
        }
        let Some(classes) = self.python_global_classes(global, r)? else {
            return Ok(None);
        };
        let mut targets: Vec<Arc<KNode>> = Vec::new();
        for cls in classes.iter() {
            let members = self.python_members(cls, member, r, &mut HashSet::new())?;
            if members.is_empty() || members.iter().any(|n| !(n.kind == "function" || n.kind == "method") || self.is_python_property(n)) {
                return Ok(Some(None));
            }
            for n in members {
                if !targets.iter().any(|t| t.id == n.id) {
                    targets.push(n);
                }
            }
        }
        if targets.len() > 1 {
            if let Some(shared) = self.python_shared_declaration(&classes, &targets, member, r)? {
                targets = vec![shared];
            }
        }
        Ok(Some(self.unique_member(targets, r, 0.9)?))
    }

    /// The nearest `member` declaration every class in `classes` inherits,
    /// whether or not one of them overrides it, found from the candidate
    /// `targets` and their classes' bases.
    fn python_shared_declaration(
        &mut self,
        classes: &[Arc<KNode>],
        targets: &[Arc<KNode>],
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        let mut declarations: Vec<(Arc<KNode>, Arc<KNode>)> = Vec::new();
        let mut queue: VecDeque<Arc<KNode>> = targets.iter().cloned().collect();
        while declarations.len() < DECLARATION_WALK_LIMIT {
            let Some(decl) = queue.pop_front() else { break };
            if declarations.iter().any(|(d, _)| d.id == decl.id) {
                continue;
            }
            let owner = self
                .nodes_in_file(&decl.file_path)?
                .iter()
                .find(|c| c.kind == "class" && decl.qualified_name == format!("{}::{member}", c.qualified_name))
                .cloned();
            let Some(owner) = owner else { continue };
            for base in self.python_bases(&owner, r)? {
                queue.extend(self.python_members(&base, member, r, &mut HashSet::new())?);
            }
            declarations.push((decl, owner));
        }
        let mut shared = Vec::new();
        for (decl, owner) in &declarations {
            let mut all = true;
            for c in classes {
                if !self.python_inherits(c, owner, r)? {
                    all = false;
                    break;
                }
            }
            if all {
                shared.push((decl.clone(), owner.clone()));
            }
        }
        let mut nearest = Vec::new();
        for (decl, owner) in &shared {
            let mut all = true;
            for (_, other) in &shared {
                if !self.python_inherits(owner, other, r)? {
                    all = false;
                    break;
                }
            }
            if all {
                nearest.push(decl.clone());
            }
        }
        Ok(if nearest.len() == 1 { nearest.pop() } else { None })
    }

    fn python_inherits(&mut self, cls: &KNode, base: &KNode, r: &ResolveRefIn) -> Res<bool> {
        Ok(cls.id == base.id || self.python_derives_from(cls, base, r, &mut HashSet::new())?)
    }

    /// The classes a module global can hold, each resolved in the file that
    /// writes it: its module's constructor bindings (`conn: Base = make()`
    /// trusts the annotation, `conn: A = B()` contradicts it) and other
    /// production modules' constructor writes. `None`: any other binding or
    /// write, or a namespace-dict write in its module, makes it unknown.
    fn python_global_classes(&mut self, global: &KNode, r: &ResolveRefIn) -> Res<Option<Rc<Vec<Arc<KNode>>>>> {
        if let Some(hit) = self.py_globals.classes.get(&global.id) {
            return Ok(hit.clone());
        }
        let external = self.python_external_writes(global)?;
        let mut writes = external.writes.clone();
        let mut known = !external.unknown && !self.python_dynamic_global_write(&global.name, &global.file_path)?;
        if known {
            for b in self.python_global_bindings(&global.name, &global.file_path)?.iter() {
                let PyBinding::Assign { ty, value, line } = b else {
                    known = false;
                    break;
                };
                let constructor = if value.is_empty() || value == "None" { None } else { python_constructor_call(value) };
                if let Some(ty) = ty {
                    if constructor.as_deref().is_some_and(|c| c != ty) {
                        known = false;
                        break;
                    }
                    writes.push((ty.clone(), global.file_path.clone(), *line));
                    continue;
                }
                if value == "None" {
                    continue;
                }
                let Some(constructor) = constructor else {
                    known = false;
                    break;
                };
                writes.push((constructor, global.file_path.clone(), *line));
            }
        }
        let mut classes: Vec<Arc<KNode>> = Vec::new();
        if known {
            for (ty, file, line) in writes {
                let mut site = r.clone();
                site.file_path = file;
                site.line = line as i64 + 1;
                site.column = 0;
                // The class's name as the writing function sees it: not a
                // parameter or local, and imported from one source only (a
                // lazy `from decoy import Decoy as Store` beside a module-level
                // `Store` names another class), and bound at module scope by
                // imports alone or by its one `class` (`Store = Decoy`, or a
                // `class Store(Decoy)` replacing the import, names another).
                let root = ty.split('.').next().unwrap_or(&ty);
                let bindings = self.python_global_bindings(root, &site.file_path)?;
                let defs = bindings.iter().filter(|b| **b == PyBinding::Def).count();
                if self.python_binds_locally(root, &site.file_path, site.line, false)?
                    || self.python_import_sources(root, &site.file_path)? > 1
                    || bindings.iter().any(|b| !matches!(b, PyBinding::Import | PyBinding::Def))
                    || defs > 0 && bindings.len() > 1
                {
                    known = false;
                    break;
                }
                let Some(cls) = self.python_ref_class(&ty, &site)? else {
                    known = false;
                    break;
                };
                if !classes.iter().any(|c| c.id == cls.id) {
                    classes.push(cls);
                }
            }
        }
        let out = known.then(|| Rc::new(classes));
        self.py_globals.classes.insert(global.id.clone(), out.clone());
        Ok(out)
    }

    /// Every binding of `name` in `file_path` at module scope and in each
    /// function or class body that declares it `global`, outside `__main__`
    /// blocks.
    fn python_global_bindings(&mut self, name: &str, file_path: &str) -> Res<Rc<Vec<PyBinding>>> {
        let key = (file_path.to_string(), name.to_string());
        if let Some(hit) = self.py_globals.bindings.get(&key) {
            return Ok(hit.clone());
        }
        let script = self.python_script_lines(file_path);
        let mut out = Vec::new();
        if let Some(src) = self.read_file(file_path) {
            let file = src.python_file();
            // `global` applies in a class body as in a function.
            let mut regions: HashSet<Option<usize>> = HashSet::from([None]);
            for (i, code) in &file.stmts {
                if re!(r"\bglobal\b").is_match(code) && python_stmt_bindings(code, code, name, *i)?.contains(&PyBinding::Global) {
                    if let Some(h) = file.scope[*i] {
                        regions.insert(Some(h));
                    }
                }
            }
            for (k, (i, code)) in file.stmts.iter().enumerate() {
                if !regions.contains(&file.scope[*i]) || script.contains(i) || !mentions(code, name) {
                    continue;
                }
                let raw = python_raw_stmt(file, &src, k);
                out.extend(python_stmt_bindings(code, &raw, name, *i)?.into_iter().filter(|b| *b != PyBinding::Global));
            }
        }
        let out = Rc::new(out);
        self.py_globals.bindings.insert(key, out.clone());
        Ok(out)
    }

    /// Whether `name`, read at `line` of `file_path`, is bound by the calling function or one
    /// around it (a parameter, an assignment, a loop, `as`, a lambda, an
    /// import) rather than being the module global; a `global name` there
    /// ends the search. With `imports_bind` false, an import of the name is
    /// no shadow: it binds the module the file imports.
    fn python_binds_locally(&mut self, name: &str, file_path: &str, line: i64, imports_bind: bool) -> Res<bool> {
        let Some(src) = self.read_file(file_path) else {
            return Ok(false);
        };
        let file = src.python_file();
        let at = (line - 1).max(0) as usize;
        let def = re!(r"^\s*(?:async\s+)?def\b");
        let start = match file.stmt(at) {
            Some((s, code)) if def.is_match(code) => Some(*s),
            _ => file.scope.get(at).copied().flatten(),
        };
        for d in file.functions_around(start) {
            let mut own = Vec::new();
            // A one-line body (`def cb(pool): conn = Decoy(); …`) sits on
            // the header's own line.
            if let Some((_, h)) = file.stmt(d) {
                if let Some(body) = header_colon(h).map(|at| &h[at + 1..]).filter(|b| !b.trim().is_empty()) {
                    own.extend(python_stmt_bindings(body, body, name, d)?);
                }
            }
            for (k, (i, code)) in file.stmts.iter().enumerate() {
                if file.scope[*i] != Some(d) || !mentions(code, name) {
                    continue;
                }
                let raw = python_raw_stmt(file, &src, k);
                own.extend(python_stmt_bindings(code, &raw, name, *i)?);
            }
            if own.contains(&PyBinding::Global) {
                return Ok(false);
            }
            if file.stmt(d).is_some_and(|(_, h)| python_def_params(h).is_some_and(|p| python_param_names(&h[p]).contains(&name))) {
                return Ok(true);
            }
            if own.iter().any(|b| *b != PyBinding::Import || imports_bind) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// How many distinct sources `file_path` imports `name` from, at any
    /// scope: `import a.b` and `import a.c` both bind package `a`.
    fn python_import_sources(&mut self, name: &str, file_path: &str) -> Res<usize> {
        let keys: HashSet<String> = self
            .import_mappings(file_path)?
            .iter()
            .filter(|m| m.local_name == name)
            .map(|m| {
                if !m.is_namespace {
                    format!("{}:{}", m.source, m.exported_name)
                } else if m.source.split('.').next() == Some(name) {
                    format!("{name}:*")
                } else {
                    format!("{}:*", m.source)
                }
            })
            .collect();
        Ok(keys.len())
    }

    /// Writes to `global` from other production files. A write
    /// `<module>.<name> = Cls(...)` adds a type; any other write (another
    /// value, a tuple, `for`, `with` or `del` target, `setattr`,
    /// `patch.object`, a `__dict__` write, a computed key, a spelling that
    /// may name another module, a receiver the writing function binds
    /// itself) makes it unknown. Test files install doubles instead (see
    /// `python_test_writer`). Writes in a `__main__` block are script code,
    /// not module state.
    fn python_external_writes(&mut self, global: &KNode) -> Res<Rc<ExternalWrites>> {
        if let Some(hit) = self.py_globals.external.get(&global.id) {
            return Ok(hit.clone());
        }
        let index = self.python_attr_writers()?;
        let files = &index.files;
        let mut out = ExternalWrites::default();
        let mut candidates: Vec<u32> =
            [global.name.as_str(), COMPUTED_WRITE].iter().flat_map(|k| index.by_name.get(*k).into_iter().flatten().copied()).collect();
        candidates.sort_unstable();
        candidates.dedup();
        for f in candidates {
            let file_path = &files[f as usize];
            if is_python_test_file(file_path) {
                continue;
            }
            let found = self.python_file_writes(global, file_path, false)?;
            out.writes.extend(found.writes);
            if found.unknown {
                out.unknown = true;
                break;
            }
        }
        let out = Rc::new(out);
        self.py_globals.external.insert(global.id.clone(), out.clone());
        Ok(out)
    }

    /// Whether `file_path` is a test file (or a `conftest.py`) that writes
    /// `global` through any receiver, installing a double: its own refs to
    /// the global then resolve nothing. Only the ref's own file matters, so
    /// this is asked per file rather than collected for every global.
    fn python_test_writer(&mut self, global: &KNode, file_path: &str) -> Res<bool> {
        // The module's own writes are its bindings.
        if !is_python_test_file(file_path) || file_path == global.file_path {
            return Ok(false);
        }
        let key = (global.id.clone(), file_path.to_string());
        if let Some(hit) = self.py_globals.test_writers.get(&key) {
            return Ok(*hit);
        }
        let index = self.python_attr_writers()?;
        let listed = index.files.binary_search_by(|f| f.as_str().cmp(file_path)).is_ok_and(|f| {
            [global.name.as_str(), COMPUTED_WRITE].iter().any(|k| index.by_name.get(*k).is_some_and(|fs| fs.binary_search(&(f as u32)).is_ok()))
        });
        let writer = listed && self.python_file_writes(global, file_path, true)?.writer;
        self.py_globals.test_writers.insert(key, writer);
        Ok(writer)
    }

    /// The writes to `global` in one file. A production file writes through
    /// a spelling of the global's module and stops at the first write that
    /// makes it unknown; a test file writes through any receiver.
    fn python_file_writes(&mut self, global: &KNode, file_path: &str, test: bool) -> Res<FileWrites> {
        let name = global.name.as_str();
        let mut out = FileWrites::default();
        let (aliases, ambiguous) = self.python_module_aliases(file_path, &global.file_path)?;
        if !test && aliases.is_empty() && ambiguous.is_empty() {
            return Ok(out);
        }
        let roots: Vec<String> = [&aliases[..], &ambiguous[..]].concat().iter().map(|a| a.split('.').next().unwrap_or(a).to_string()).collect();
        let spell = |names: &[String]| names.iter().map(|a| regex::escape(a)).collect::<Vec<_>>().join("|");
        let receiver = if test { r"[\p{XID_Continue}.]+".to_string() } else { spell(&[aliases.clone(), ambiguous].concat()) };
        let exact = match (test, aliases.is_empty()) {
            (true, _) => receiver.clone(),
            (false, true) => r"\x00".to_string(),
            (false, false) => spell(&aliases),
        };
        let mut patterns: Option<Rc<[Regex; 5]>> = None;
        let script = self.python_script_lines(file_path);
        let Some(src) = self.read_file(file_path) else { return Ok(out) };
        let file = src.python_file();
        for (k, (i, code)) in file.stmts.iter().enumerate() {
            if script.contains(i) || !code.contains(name) && !python_dynamic_stmt(file, &src, k, code) {
                continue;
            }
            if patterns.is_none() {
                patterns = Some(self.python_write_patterns(&receiver, &exact, name)?);
            }
            let Some([target, exact, setattr, dict, computed]) = patterns.as_deref() else { continue };
            let raw = python_raw_stmt(file, &src, k);
            // A match inside a plain string (a docstring's example) runs nothing.
            let executed = std::cell::OnceCell::new();
            let live = |at: usize| executed.get_or_init(|| python_executed(&raw)).as_bytes().get(at).is_some_and(|b| *b != b' ');
            let computed = computed.find_iter(&raw).any(|m| live(m.start()) && !follows_name(&raw, m.start()));
            if !raw.contains(name) && !computed {
                continue;
            }
            let assignment = python_assignment_split(code);
            let assigned = assignment.as_ref().is_some_and(|a| a.targets.iter().any(|t| attribute_write(target, code[t.clone()].trim())));
            let rebound = python_other_targets(code).iter().any(|t| attribute_write(target, t));
            if !assigned
                && !rebound
                && !computed
                && !setattr.find_iter(&raw).any(|m| live(m.start()))
                && !dict.find_iter(&raw).any(|m| live(m.start()) && !follows_name(&raw, m.start()))
            {
                continue;
            }
            if test {
                out.writer = true;
                return Ok(out);
            }
            // `def reset(settings): settings.conn = …` writes the
            // parameter, a module-level `settings = SimpleNamespace()`
            // another object, and `import other as settings` another module;
            // each may or may not be the module.
            let mut shadowed = false;
            for root in &roots {
                shadowed |= self.python_binds_locally(root, file_path, *i as i64 + 1, true)?
                    || self.python_global_bindings(root, file_path)?.iter().any(|b| *b != PyBinding::Import)
                    || self.python_import_sources(root, file_path)? > 1;
            }
            if shadowed || rebound {
                out.unknown = true;
                return Ok(out);
            }
            let value = match &assignment {
                Some(a) if a.targets.len() == 1 && !a.augmented && exact.is_match(code[a.targets[0].clone()].trim()) => code[a.value.clone()].trim(),
                _ => "",
            };
            if value == "None" {
                continue;
            }
            let Some(constructor) = python_constructor_call(value) else {
                out.unknown = true;
                return Ok(out);
            };
            out.writes.push((constructor, file_path.to_string(), *i));
        }
        Ok(out)
    }

    /// The patterns `python_file_writes` reads a file with: an attribute
    /// write, an exact one, a `setattr`/`patch.object` with the name, a
    /// `__dict__` write, and a computed key. They depend on the receiver
    /// spelling and the name alone, so files sharing them share one set;
    /// the memo is kept apart from the shared regex cache.
    fn python_write_patterns(&mut self, receiver: &str, exact: &str, name: &str) -> Res<Rc<[Regex; 5]>> {
        let key = format!("{receiver}\0{exact}\0{name}");
        if let Some(hit) = self.py_globals.patterns.get(&key) {
            return Ok(hit.clone());
        }
        let n = regex::escape(name);
        let set = Rc::new([
            one_off_regex(&format!(r"(?:{receiver})\.{n}\b"))?,
            one_off_regex(&format!(r"^(?:{exact})\.{n}(?:\s*:[^=]*)?$"))?,
            one_off_regex(&format!(r#"\b(?:setattr|patch\.object)\s*\(\s*(?:{receiver})\s*,\s*['"]{n}['"]"#))?,
            one_off_regex(&format!(r"(?:{receiver})\.__dict__\s*(?:\[|\.\s*update\s*\()"))?,
            // A key the code computes, or a quoted one with an escape
            // (`"\x63onn"`), may name any global.
            one_off_regex(&format!(
                r#"\b(?:[\w.]+\.)?(?:setattr|patch\.object)\s*\(\s*(?:{receiver})\s*,\s*(?:[^'"\s]|['"][^'"]*\\|['"][^'"]*['"]\s*[^,)\s])|(?:{receiver})\.__dict__\s*(?:\[\s*(?:[^'"\s]|['"][^'"]*\\|['"][^'"]*['"]\s*[^\]\s])|\.\s*update\s*\()"#
            ))?,
        ]);
        if self.py_globals.patterns.len() >= WRITE_PATTERN_LIMIT {
            self.py_globals.patterns.clear();
        }
        self.py_globals.patterns.insert(key, set.clone());
        Ok(set)
    }

    /// `PyAttrWriters` for the indexed files, built once per node table: the
    /// run's workers share it, and the first to ask builds it while the
    /// others wait. A resolver without a node table builds its own.
    fn python_attr_writers(&mut self) -> Res<Arc<PyAttrWriters>> {
        if let Some(hit) = &self.py_globals.attr_writers {
            return Ok(hit.clone());
        }
        let index = match self.sorted_files() {
            Some(files) => Arc::new(self.python_attr_index(files)?),
            None => {
                let table = self.table()?;
                let mut slot = table.python_attr_writers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match &*slot {
                    Some(hit) => hit.clone(),
                    None => {
                        let mut files: Vec<String> = table.files.iter().cloned().collect();
                        files.sort();
                        let built = Arc::new(self.python_attr_index(Arc::new(files))?);
                        *slot = Some(built.clone());
                        built
                    }
                }
            }
        };
        self.py_globals.attr_writers = Some(index.clone());
        Ok(index)
    }

    /// Reads `files` for `PyAttrWriters`.
    fn python_attr_index(&self, files: Arc<Vec<String>>) -> Res<PyAttrWriters> {
        let mut index: HashMap<String, Vec<u32>> = HashMap::new();
        let attrs = re!(r"\.\s*([^\W\d]\w*)");
        for (f, path) in files.iter().enumerate() {
            if !(path.ends_with(".py") || path.ends_with(".pyi")) {
                continue;
            }
            let Some(src) = self.read_file_uncached(path) else { continue };
            let file = src.python_file();
            let mut names: HashSet<&str> = HashSet::new();
            let mut dynamic: Vec<String> = Vec::new();
            for (k, (_, code)) in file.stmts.iter().enumerate() {
                // `x.name = …` and the other attribute targets need a `.`.
                if code.contains('.') {
                    if let Some(a) = python_assignment_split(code) {
                        for t in &a.targets {
                            names.extend(attrs.captures_iter(&code[t.clone()]).filter_map(|c| c.get(1)).map(|m| m.as_str()));
                        }
                    }
                    for t in python_other_targets(code) {
                        names.extend(attrs.captures_iter(t).filter_map(|c| c.get(1)).map(|m| m.as_str()));
                    }
                }
                if python_dynamic_stmt(file, &src, k, code) {
                    let raw = python_raw_stmt(file, &src, k);
                    dynamic.extend(re!(r"\w+").find_iter(&raw).map(|m| m.as_str().to_string()));
                    let executed = std::cell::OnceCell::new();
                    let live = |at: usize| executed.get_or_init(|| python_executed(&raw)).as_bytes().get(at).is_some_and(|b| *b != b' ');
                    if KernelResolver::cached_regex(COMPUTED_KEY)?.find_iter(&raw).any(|m| live(m.start()) && !follows_name(&raw, m.start())) {
                        dynamic.push(COMPUTED_WRITE.to_string());
                    }
                }
            }
            for name in names.into_iter().map(str::to_string).chain(dynamic).collect::<HashSet<String>>() {
                index.entry(name).or_default().push(f as u32);
            }
        }
        Ok(PyAttrWriters { files, by_name: index })
    }

    /// How `file_path` spells the module file `module_file`: `aliases` name
    /// exactly that file, `ambiguous` may name another file sharing its
    /// dotted tail. `import a.b` binds `a`, so that module is spelled `a.b`;
    /// `import a.b as c` binds `c`.
    fn python_module_aliases(&mut self, file_path: &str, module_file: &str) -> Res<(Vec<String>, Vec<String>)> {
        let (mut aliases, mut ambiguous) = (Vec::new(), Vec::new());
        let imports = self.import_mappings(file_path)?;
        for m in imports.iter() {
            let dotted = if m.is_namespace {
                m.source.clone()
            } else if m.source.bytes().all(|b| b == b'.') {
                format!("{}{}", m.source, m.exported_name)
            } else {
                format!("{}.{}", m.source, m.exported_name)
            };
            let files = self.python_module_files(&dotted, file_path)?;
            if !files.iter().any(|f| f == module_file) {
                continue;
            }
            let first = m.source.split('.').next().unwrap_or("");
            let plain_dotted = m.is_namespace
                && m.source.contains('.')
                && (m.local_name == first || Some(m.local_name.as_str()) == m.source.rsplit('.').next())
                && !self.python_explicit_alias(file_path, &m.source, &m.local_name)?;
            let spelled = if plain_dotted { m.source.clone() } else { m.local_name.clone() };
            let into = if files.len() == 1 { &mut aliases } else { &mut ambiguous };
            if !into.contains(&spelled) {
                into.push(spelled);
            }
        }
        Ok((aliases, ambiguous))
    }

    /// Whether `file_path` imports exactly `module` as `local`
    /// (`import pkg.settings as settings`), outside strings: the import
    /// mapping alone can't tell it from a plain `import pkg.settings`.
    fn python_explicit_alias(&mut self, file_path: &str, module: &str, local: &str) -> Res<bool> {
        let key = (file_path.to_string(), module.to_string(), local.to_string());
        if let Some(hit) = self.py_globals.explicit.get(&key) {
            return Ok(*hit);
        }
        let explicit = one_off_regex(&format!(r"\bimport\s(?:.*[^\w.])?{}\s+as\s+{}\b", regex::escape(module), regex::escape(local)))?;
        let found = self.read_file(file_path).is_some_and(|src| src.python_file().stmts.iter().any(|(_, code)| explicit.is_match(code)));
        self.py_globals.explicit.insert(key, found);
        Ok(found)
    }

    /// The indexed files a Python module path names from `from_file`: a
    /// relative path (`..settings`) exactly, an absolute one by path suffix,
    /// so a source root (`src/`) still resolves and two files sharing the
    /// tail (`x/settings.py`, `y/settings.py`) both come back.
    fn python_module_files(&mut self, dotted: &str, from_file: &str) -> Res<Rc<Vec<String>>> {
        let dots = dotted.len() - dotted.trim_start_matches('.').len();
        let mut dir = String::new();
        if dots > 0 {
            let mut d = pos_dirname(from_file);
            for _ in 1..dots {
                d = pos_dirname(d);
            }
            dir = d.to_string();
        }
        let key = (dotted.to_string(), dir.clone());
        if let Some(hit) = self.py_globals.module_files.get(&key) {
            return Ok(hit.clone());
        }
        let parts: Vec<&str> = dotted[dots..].split('.').filter(|p| !p.is_empty()).collect();
        let mut out = Vec::new();
        if let Some(last) = parts.last() {
            let rel = std::iter::once(dir.as_str()).chain(parts.iter().copied()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("/");
            let wants = [format!("{rel}.py"), format!("{rel}.pyi"), format!("{rel}/__init__.py")];
            let matches = |f: &str| {
                wants.iter().any(|w| f == w || (dots == 0 && f.len() > w.len() && f.ends_with(w.as_str()) && f[..f.len() - w.len()].ends_with('/')))
            };
            for name in [format!("{last}.py"), format!("{last}.pyi"), "__init__.py".to_string()] {
                for n in self.nodes_by_name(&name)?.iter() {
                    if n.kind == "file" && matches(&n.file_path) && !out.contains(&n.file_path) {
                        out.push(n.file_path.clone());
                    }
                }
            }
        }
        let out = Rc::new(out);
        self.py_globals.module_files.insert(key, out.clone());
        Ok(out)
    }

    /// Whether `file_path` can write its global `name` through its namespace
    /// dict: `globals()`, `vars()` at module scope (in a function it is the
    /// locals) or `sys.modules[__name__]` used as anything but a literal-key
    /// read or `.get`, or a literal-key write of `name`. Read per statement,
    /// strings ignored, `__main__` blocks skipped.
    fn python_dynamic_global_write(&mut self, name: &str, file_path: &str) -> Res<bool> {
        let script = self.python_script_lines(file_path);
        let Some(src) = self.read_file(file_path) else { return Ok(false) };
        let file = src.python_file();
        for (k, (i, code)) in file.stmts.iter().enumerate() {
            if script.contains(i) {
                continue;
            }
            let namespace = if file.scope[*i].is_none() {
                re!(r"\bglobals\(\s*\)|\bvars\(\s*\)|\bsys\.modules\s*\[\s*__name__\s*\]")
            } else {
                re!(r"\bglobals\(\s*\)|\bsys\.modules\s*\[\s*__name__\s*\]")
            };
            if !namespace.is_match(code) && !python_stmt_lines_contain(file, &src, k, &["globals", "vars", "sys.modules"]) {
                continue;
            }
            // The code that runs, an f-string's `{…}` fields included.
            let raw = python_raw_stmt(file, &src, k);
            let executed = python_executed(&raw);
            let uses = namespace.find_iter(&executed).count();
            if uses == 0 {
                continue;
            }
            // `globals()["conn"] = x`, `del globals()["conn"]`, `for globals()["conn"] in …`.
            let mut targets = python_assignment_split(code).map(|a| a.targets).unwrap_or_default();
            for t in python_other_targets(code) {
                let at = t.as_ptr() as usize - code.as_ptr() as usize;
                targets.push(at..at + t.len());
            }
            let mut safe = 0;
            for c in re!(r#"\bglobals\(\s*\)\s*(?:\[\s*(?:'(\w+)'|"(\w+)")\s*\]|\.\s*get\s*\()"#).captures_iter(&raw) {
                let m = c.get(0).expect("whole match");
                // `globals()` inside a plain string is no use.
                if !executed[m.start()..].starts_with("globals") {
                    continue;
                }
                let written = targets.iter().any(|t| t.start <= m.start() && m.end() <= t.end);
                if written && c.get(1).or(c.get(2)).is_some_and(|key| key.as_str() == name) {
                    return Ok(true);
                }
                safe += 1;
            }
            if uses > safe {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The statement lines inside a file's top-level
    /// `if __name__ == "__main__":` blocks, the one-line form included.
    fn python_script_lines(&mut self, file_path: &str) -> Rc<HashSet<usize>> {
        if let Some(hit) = self.py_globals.script.get(file_path) {
            return hit.clone();
        }
        let mut inside = HashSet::new();
        if let Some(src) = self.read_file(file_path) {
            let code = &src.python_file().code;
            let guard = re!(r#"^if\s+(?:__name__\s*==\s*(?:'__main__'|"__main__")|(?:'__main__'|"__main__")\s*==\s*__name__)\s*:"#);
            for (i, line) in src.iter().enumerate() {
                // A guard-shaped line inside a string starts no statement.
                if !code.get(i).is_some_and(|(start, c)| *start && c.starts_with("if")) {
                    continue;
                }
                let Some(m) = guard.find(line) else { continue };
                if code.get(i).and_then(|(_, c)| c.get(m.end()..)).is_some_and(|rest| !rest.trim().is_empty()) {
                    inside.insert(i);
                }
                for (j, (_, c)) in code.iter().enumerate().skip(i + 1) {
                    if c.starts_with(|ch: char| !ch.is_whitespace()) {
                        break;
                    }
                    inside.insert(j);
                }
            }
        }
        let inside = Rc::new(inside);
        self.py_globals.script.insert(file_path.to_string(), inside.clone());
        inside
    }
}

/// A Python assignment statement split at its top-level `=` signs: each
/// target's byte range, the value's, and whether it is augmented.
pub(super) struct PyAssignment {
    pub(super) targets: Vec<std::ops::Range<usize>>,
    pub(super) value: std::ops::Range<usize>,
    pub(super) augmented: bool,
}

/// Split `a = b = value` (code with strings blanked) at its top-level
/// assignment operators; `None` when it assigns nothing. Comparisons and
/// the walrus are no assignment.
pub(super) fn python_assignment_split(code: &str) -> Option<PyAssignment> {
    let b = code.as_bytes();
    let (mut targets, mut depth, mut start, mut augmented) = (Vec::new(), 0usize, 0usize, false);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'=' if depth == 0 => {
                let prev = if i > 0 { b[i - 1] } else { 0 };
                if b.get(i + 1) == Some(&b'=') {
                    i += 2;
                    continue;
                }
                let comparison = matches!(prev, b'<' | b'>') && (i < 2 || b[i - 2] != prev);
                if !matches!(prev, b'!' | b':') && !comparison {
                    let seg = &code[start..i];
                    let op = ["//", "**", ">>", "<<"]
                        .iter()
                        .find(|o| seg.ends_with(**o))
                        .map(|o| o.len())
                        .or_else(|| seg.ends_with(['-', '+', '*', '/', '%', '&', '|', '^', '@']).then_some(1))
                        .unwrap_or(0);
                    augmented |= op > 0;
                    targets.push(start..i - op);
                    start = i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    (!targets.is_empty()).then_some(PyAssignment { targets, value: start..code.len(), augmented })
}

/// `Cls(...)` or `pkg.mod.Cls(...)` as the whole value (code with strings
/// blanked), parentheses around it peeled: the callee. `None` for anything
/// else (`Cls() if x else y`, a factory, a literal).
pub(super) fn python_constructor_call(value: &str) -> Option<String> {
    let mut text = value.trim();
    while text.starts_with('(') {
        let mut depth = 0usize;
        let mut close = None;
        for (i, c) in text.bytes().enumerate() {
            match c {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        if close != Some(text.len() - 1) {
            break;
        }
        text = text[1..text.len() - 1].trim();
    }
    let c = re!(r"^((?:[A-Za-z_]\w*\.)*[A-Z]\w*)\s*\(").captures(text)?;
    let open = c.get(0)?.end() - 1;
    let mut depth = 0usize;
    for (i, b) in text.bytes().enumerate().skip(open) {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return re!(r"^[\s\\]*$").is_match(&text[i + 1..]).then(|| c[1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Every way a statement binds `name`: `code` with strings blanked, `raw`
/// the same bytes with strings kept (a quoted annotation), `line` its first
/// line.
fn python_stmt_bindings(code: &str, raw: &str, name: &str, line: usize) -> Res<Vec<PyBinding>> {
    // `def f(): x = 1; conn = 2` binds `conn` in `f`'s own scope; a
    // `global conn` there writes the module's.
    if let Some(c) = re!(r"^\s*(?:async\s+)?(?:def|class)\s+(\w+)").captures(code) {
        if &c[1] == name {
            return Ok(vec![PyBinding::Def]);
        }
        // A default (`def f(x=(conn := Decoy()))`) runs in the enclosing scope.
        let header = header_colon(code).map_or(code, |at| &code[..at]);
        if word_hits(header, name).any(|(_, e)| header[e..].trim_start_matches([' ', '\t']).starts_with(":=")) {
            return Ok(vec![PyBinding::Other]);
        }
        let body = header_colon(code).map_or("", |at| &code[at + 1..]);
        let global = re!(r"\bglobal\b").is_match(body) && bare_word(body, name);
        return Ok(if global { vec![PyBinding::Other] } else { Vec::new() });
    }
    // `global conn; conn = Store()`: each `;`-separated statement on its own
    // (`code` has strings blanked, so every `;` there separates statements).
    if let Some(at) = code.find(';') {
        let mut out = python_stmt_bindings(&code[..at], raw.get(..at).unwrap_or(raw), name, line)?;
        out.extend(python_stmt_bindings(&code[at + 1..], raw.get(at + 1..).unwrap_or(raw), name, line)?);
        return Ok(out);
    }
    // `if flag: conn = Decoy()`: the header and its one-line suite apart.
    if re!(r"^\s*(?:(?:async\s+)?(?:for|with)|if|elif|else|while|try|except|finally)\b").is_match(code) {
        if let Some(at) = header_colon(code).filter(|&at| !code[at + 1..].trim().is_empty()) {
            let mut out = python_stmt_bindings(&code[..at], raw.get(..at).unwrap_or(raw), name, line)?;
            out.extend(python_stmt_bindings(&code[at + 1..], raw.get(at + 1..).unwrap_or(raw), name, line)?);
            return Ok(out);
        }
    }
    let mut out = Vec::new();
    if let Some(c) = re!(r"^\s*(?:from\s+([\w.]+)\s+)?import\s+(.*)$").captures(code) {
        let from = c.get(1).is_some();
        let names = c.get(2).map_or("", |m| m.as_str());
        if from && names.trim() == "*" {
            return Ok(vec![PyBinding::Other]);
        }
        let names = names.replace(['(', ')', '\\'], " ");
        for part in names.split(',') {
            let Some(m) = re!(r"^([\w.]+)(?:\s+as\s+(\w+))?$").captures(part.trim()) else { continue };
            let local = match m.get(2) {
                Some(alias) => alias.as_str(),
                None if from => &m[1],
                None => m[1].split('.').next().unwrap_or(""),
            };
            if local == name {
                out.push(PyBinding::Import);
            }
        }
        return Ok(out);
    }
    if !mentions(code, name) || !bare_word(code, name) {
        return Ok(out);
    }
    if let Some(c) = re!(r"^\s*(global|nonlocal)\s+([\w\s,]+)$").captures(code) {
        if c[2].split(',').any(|s| s.trim() == name) {
            out.push(if &c[1] == "global" { PyBinding::Global } else { PyBinding::Other });
        }
        return Ok(out);
    }
    // A `case` pattern can capture the name (`case [name]:`, `case name:`).
    if re!(r"^\s*case\b").is_match(code) {
        return Ok(vec![PyBinding::Other]);
    }
    if let Some(a) = python_assignment_split(code) {
        let single = a.targets.len() == 1 && !a.augmented;
        let target = if single { code[a.targets[0].clone()].trim() } else { "" };
        let annotated = if single {
            let raw_target = raw.get(a.targets[0].clone()).unwrap_or(target);
            annotation_of(raw_target, name)
        } else {
            None
        };
        if target == name || annotated.is_some() {
            out.push(PyBinding::Assign { ty: annotated, value: code[a.value.clone()].trim().to_string(), line });
        } else {
            for t in &a.targets {
                // `obj.attr: T = v` binds nothing named in its annotation.
                let text = &code[t.clone()];
                let text = if single { text.split(':').next().unwrap_or(text) } else { text };
                if bare_word(text, name) {
                    out.push(PyBinding::Other);
                    break;
                }
            }
        }
    } else if let Some(ty) = annotation_of(raw, name) {
        out.push(PyBinding::Assign { ty: Some(ty), value: String::new(), line });
    } else if re!(r"^\s*del\b").is_match(code) {
        out.push(PyBinding::Other);
    }
    // `name := …` or `… as name`.
    let walrus_or_as = word_hits(code, name).any(|(s, e)| {
        let before = code[..s].trim_end_matches([' ', '\t']);
        code[e..].trim_start_matches([' ', '\t']).starts_with(":=")
            || before.len() < s && before.ends_with("as") && !before[..before.len() - 2].ends_with(is_word)
    });
    if walrus_or_as {
        out.push(PyBinding::Other);
    }
    if as_targets(code).iter().any(|t| t.starts_with(['(', '[']) && bare_word(t, name)) {
        out.push(PyBinding::Other);
    }
    for c in re!(r"\bfor\s+([^:]+?)\s+in\b").captures_iter(code) {
        if bare_word(&c[1], name) {
            out.push(PyBinding::Other);
        }
    }
    for c in re!(r"\blambda\b([^:]*):").captures_iter(code) {
        if bare_word(&c[1], name) {
            out.push(PyBinding::Other);
        }
    }
    Ok(out)
}

/// The targets a statement binds other than by `=`: `for … in`, `as …`
/// and `del …` (code with strings blanked).
fn python_other_targets(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    if code.contains("for") {
        out.extend(re!(r"\bfor\s+([^:]+?)\s+in\b").captures_iter(code).filter_map(|c| c.get(1)).map(|m| m.as_str()));
    }
    if code.contains("as") {
        out.extend(as_targets(code));
    }
    if code.contains("del") {
        out.extend(re!(r"^\s*del\s+(.+)$").captures_iter(code).filter_map(|c| c.get(1)).map(|m| m.as_str()));
    }
    out
}

/// What each `as` in a statement binds: a dotted name, or a bracketed
/// target list with its brackets (`as (a, (b, obj.attr))`).
fn as_targets(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for m in re!(r"\bas\s*([\w.]+|[(\[])").captures_iter(code).filter_map(|c| c.get(1)) {
        if !m.as_str().starts_with(['(', '[']) {
            out.push(m.as_str());
            continue;
        }
        let mut depth = 0usize;
        for (i, b) in code.bytes().enumerate().skip(m.start()) {
            match b {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(&code[m.start()..=i]);
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// The colon ending a compound statement's header (code with strings
/// blanked): the first one outside brackets that is no `:=`.
fn header_colon(code: &str) -> Option<usize> {
    let b = code.as_bytes();
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && b.get(i + 1) != Some(&b'=') => return Some(i),
            _ => {}
        }
    }
    None
}

/// A regex built for one name or one receiver spelling, kept out of the
/// shared cache so it doesn't evict the patterns other strategies reuse.
fn one_off_regex(pattern: &str) -> Res<Regex> {
    Ok(Regex::new(pattern).map_err(|e| Error::from_reason(e.to_string()))?)
}

/// `T` in `name: T` or `name: "T"`, the whole of `text` but surrounding
/// whitespace.
fn annotation_of(text: &str, name: &str) -> Option<String> {
    let rest = text.trim().strip_prefix(name)?.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest);
    let end = rest.find(|c: char| !(is_word(c) || c == '.')).unwrap_or(rest.len());
    let tail = &rest[end..];
    (end > 0 && tail.strip_prefix(['"', '\'']).unwrap_or(tail).is_empty()).then(|| rest[..end].to_string())
}

/// A character that continues a Python name (combining marks and `·`
/// included).
fn is_word(c: char) -> bool {
    if c.is_ascii() {
        c == '_' || c.is_ascii_alphanumeric()
    } else {
        re!(r"^\p{XID_Continue}$").is_match(c.encode_utf8(&mut [0; 4]))
    }
}

/// Each byte range where `name` stands as a whole word in `text`.
fn word_hits<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
    text.match_indices(name)
        .map(|(at, _)| (at, at + name.len()))
        .filter(|&(s, e)| !text[..s].ends_with(is_word) && !text[e..].starts_with(is_word))
}

/// A test file, or any `conftest.py`.
fn is_python_test_file(path: &str) -> bool {
    super::tables::is_test_path(path) || pos_basename(path) == "conftest.py"
}

/// Whether a statement can bind `name` at all: it names it or imports.
fn mentions(code: &str, name: &str) -> bool {
    code.contains(name) || code.contains("import")
}

/// `name` as a bare variable in `text`: not an attribute (`x.name`), and
/// not itself read through `.` or `[`.
fn bare_word(text: &str, name: &str) -> bool {
    word_hits(text, name).any(|(s, e)| !follows_name(text, s) && !text[e..].trim_start().starts_with(['.', '[']))
}

/// Whether byte `at` of `text` continues a name or attribute chain.
fn follows_name(text: &str, at: usize) -> bool {
    text[..at].ends_with(|c: char| c == '.' || is_word(c))
}

/// Whether an assignment target writes `<receiver>.<name>` (from `target`'s
/// pattern), not an attribute of it (`<receiver>.<name>.x`, `[…]`, a call).
fn attribute_write(target: &Regex, text: &str) -> bool {
    target
        .find_iter(text)
        .any(|m| !follows_name(text, m.start()) && !text[m.end()..].trim_start().starts_with(['.', '[', '(']))
}

/// Whether statement `k` (`code`, strings blanked) calls `setattr` or
/// `patch.object` or uses `__dict__`, inside an f-string's `{…}` fields
/// included.
fn python_dynamic_stmt(file: &PyFile, lines: &[String], k: usize, code: &str) -> bool {
    let dynamic = re!(r"\b(?:setattr|patch\.object)\b|__dict__");
    dynamic.is_match(code)
        || python_stmt_lines_contain(file, lines, k, &["setattr", "patch.object", "__dict__"])
            && dynamic.is_match(&python_executed(&python_raw_stmt(file, lines, k)))
}

/// Whether any source line of statement `k`, strings and comments kept,
/// contains one of `needles`.
fn python_stmt_lines_contain(file: &PyFile, lines: &[String], k: usize, needles: &[&str]) -> bool {
    let Some(&(start, _)) = file.stmts.get(k) else { return false };
    let end = file.stmts.get(k + 1).map_or(file.code.len(), |(j, _)| *j);
    lines.get(start..end.min(lines.len())).is_some_and(|ls| ls.iter().any(|l| needles.iter().any(|n| l.contains(n))))
}

/// `raw` (statement text with strings kept) with every string's text
/// blanked, byte for byte, except the code in the `{…}` fields of f- and
/// t-strings: the code that runs.
fn python_executed(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        if matches!(b[i], b'\'' | b'"') {
            i = python_string_end(b, i, Some(&mut out));
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

/// The index after the string whose opening quote is at `q`, blanking its
/// text in `out` (fields kept) when given.
fn python_string_end(b: &[u8], q: usize, mut out: Option<&mut Vec<u8>>) -> usize {
    let fields = super::member_fn_ref::python_interpolates(b, q);
    let quote = b[q];
    let triple = b.get(q + 1) == Some(&quote) && b.get(q + 2) == Some(&quote);
    let mut i = q + if triple { 3 } else { 1 };
    let blank = |out: &mut Option<&mut Vec<u8>>, from: usize, to: usize| {
        if let Some(o) = out.as_deref_mut() {
            let end = to.min(o.len());
            o[from..end].fill(b' ');
        }
    };
    while i < b.len() {
        match b[i] {
            // `\{x}` keeps the backslash and still opens a field.
            b'\\' if fields && b.get(i + 1) == Some(&b'{') => {
                blank(&mut out, i, i + 1);
                i += 1;
            }
            b'\\' => {
                blank(&mut out, i, i + 2);
                i += 2;
            }
            c if c == quote && (!triple || b.get(i + 1) == Some(&quote) && b.get(i + 2) == Some(&quote)) => {
                return i + if triple { 3 } else { 1 };
            }
            b'{' if fields && b.get(i + 1) == Some(&b'{') => {
                blank(&mut out, i, i + 2);
                i += 2;
            }
            b'{' if fields => i = python_field_end(b, i + 1, &mut out),
            _ => {
                blank(&mut out, i, i + 1);
                i += 1;
            }
        }
    }
    b.len()
}

/// The index after the `}` closing the f-string field whose text starts at
/// `i`; strings nested in it are blanked the same way.
fn python_field_end(b: &[u8], mut i: usize, out: &mut Option<&mut Vec<u8>>) -> usize {
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                i = python_string_end(b, i, out.as_deref_mut());
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'}' if depth == 0 => return i + 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Statement `k`'s raw text, aligned byte for byte with its code in
/// `PyFile::stmts`: string contents kept, comments cut, continuation lines
/// joined the same way.
fn python_raw_stmt(file: &PyFile, lines: &[String], k: usize) -> String {
    let Some(&(start, _)) = file.stmts.get(k) else { return String::new() };
    let end = file.stmts.get(k + 1).map_or(file.code.len(), |(j, _)| *j);
    let mut s = String::new();
    for i in start..end {
        let len = file.code.get(i).map_or(0, |(_, c)| c.len());
        let raw = lines.get(i).map_or("", |l| l.get(..len).unwrap_or(l));
        if i > start {
            if s.ends_with('\\') {
                s.pop();
                s.push(' ');
            }
            s.push(' ');
        }
        s.push_str(raw);
    }
    s
}
