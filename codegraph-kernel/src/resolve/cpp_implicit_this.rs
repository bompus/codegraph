//! A C++ call written with no receiver, or on `this`, in a member function — or in a lambda in one,
//! whose calls the extractor gives to the function — calls what C++ name lookup finds there: a
//! member of the function's class, else of a class it derives from, else of a class it is nested
//! in, before anything at namespace scope.
//!
//! Port of upstream's `matchCppImplicitThisCall` (name-matcher.ts at fa56256c); read the original
//! with `git show fa56256c:src/resolution/name-matcher.ts`.
//!  - Bases are the class's own base edges, so a namesake class elsewhere lends it nothing. A bare
//!    call skips a base that depends on the class's template parameters, which C++ doesn't look in
//!    (`this->` is how such a member is called).
//!  - `this` is an object of the innermost class: never an enclosing class's.
//!  - A parameter or local the function declares before a bare call is what it calls.
//!  - A class that may declare the name through a macro (`Get##name`), which no node shows, ends
//!    the walk.
use super::overloads_upstream::split_top_level;
use super::receivers::cpp_last_segment;
use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Receiver {
    Bare,
    This,
}

/// What one class scope gives the lookup.
enum Lookup {
    Found(Arc<KNode>),
    /// The class and its bases have no method of that name.
    Missing,
    /// The class may declare the name through a macro: lookup stops.
    Stop,
}

/// One class C++ lookup passes through, with the declarations that are the class there.
struct ClassScope {
    qualified_name: String,
    decls: Vec<Arc<KNode>>,
    /// `decls[0]` holds the code the lookup starts from.
    holds: bool,
    /// Other classes of that qualified name were left out: another translation unit's.
    narrowed: bool,
}

fn class_kind(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "union")
}

fn c_family(language: &str) -> bool {
    matches!(language, "c" | "cpp")
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn range_within(inner: &KNode, outer: &KNode) -> bool {
    if inner.start_line < outer.start_line || inner.end_line > outer.end_line {
        return false;
    }
    if inner.start_line == outer.start_line && inner.start_column < outer.start_column {
        return false;
    }
    !(inner.end_line == outer.end_line && inner.end_column > outer.end_column)
}

fn same_range(a: &KNode, b: &KNode) -> bool {
    a.start_line == b.start_line && a.start_column == b.start_column && a.end_line == b.end_line && a.end_column == b.end_column
}

/// Whether `text` from `from` on is an argument list, after any template arguments (`<Foo, 3>(`).
fn argument_list_follows(text: &str, from: usize) -> bool {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
    if bytes.get(i) == Some(&b'<') {
        let mut depth = 0i32;
        while i < bytes.len() {
            match bytes[i] {
                b'<' => depth += 1,
                b'>' => { depth -= 1; if depth == 0 { break; } }
                b';' | b'{' | b'}' => return false,
                _ => {}
            }
            i += 1;
        }
        if depth != 0 { return false; }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
    }
    bytes.get(i) == Some(&b'(')
}

/// stripCppTemplateArguments: the text with every `<…>` span removed.
fn strip_template_arguments(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(word_char) && !text[at + word.len()..].chars().next().is_some_and(word_char)
    })
}

impl KernelResolver {
    /// The method a receiver-less (or `this`) C++ call reaches from the member function it is in.
    pub(super) fn cpp_implicit_this(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let name = r.reference_name.as_str();
        if r.language != "cpp" || r.reference_kind != "calls" || !re!(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(name) { return Ok(None); }
        let Some(receiver) = self.cpp_implicit_receiver(r) else { return Ok(None) };
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
        let Some(owner) = self.cpp_member_owner(&caller)? else { return Ok(None) };
        let mut found = Lookup::Missing;
        for scope in self.cpp_enclosing_classes(&owner, &caller)? {
            found = self.cpp_scope_method(&scope, name, receiver, r)?;
            if !matches!(found, Lookup::Missing) || receiver == Receiver::This { break; }
        }
        let Lookup::Found(node) = found else { return Ok(None) };
        // (A recursive call names the function itself, which no local shadows.)
        if receiver == Receiver::Bare && name != caller.name && self.cpp_local_name(name, &caller, r)? { return Ok(None); }
        Ok(Some(KCand { node, confidence: 0.9, resolved_by: "instance-method" }))
    }

    /// How a C++ call the extractor recorded by its bare name is written, read at its column:
    /// bare, on `this->` / `(*this).`, or neither. The extractor also drops receivers it can't
    /// spell (`arr_[0].Foo()`, `(p_)->Foo()`, `this->p_->Foo()`), whose calls are no member of the
    /// caller's class; and a name its argument list doesn't follow is no call of it.
    fn cpp_implicit_receiver(&mut self, r: &ResolveRefIn) -> Option<Receiver> {
        let source = self.read_file(&r.file_path)?;
        let line = source.get((r.line - 1).max(0) as usize)?.as_str();
        let name = r.reference_name.as_str();
        let at = |byte: usize| -> Option<Receiver> {
            if !line.is_char_boundary(byte) { return None; }
            let text = &line[byte..];
            let self_len = re!(r"^(?:this\s*->|\(\s*\*\s*this\s*\)\s*\.)\s*(?:template\s+)?").find(text).map_or(0, |m| m.end());
            let before = line[..byte].trim_end();
            // A column past the receiver: on `this`, or some other object's call.
            let on_this = self_len > 0 || re!(r"(?:^|[^A-Za-z0-9_$])(?:this\s*->|\(\s*\*\s*this\s*\)\s*\.)$").is_match(before);
            if !on_this && (before.ends_with('.') || before.ends_with("->") || before.ends_with("::")) { return None; }
            let rest = &text[self_len..];
            if !rest.starts_with(name) || rest[name.len()..].chars().next().is_some_and(word_char) { return None; }
            argument_list_follows(rest, name.len()).then_some(if on_this { Receiver::This } else { Receiver::Bare })
        };
        let column = r.column.max(0) as usize;
        let verdict = at(names::js_unit_to_byte(line, column));
        if verdict.is_some() || line.is_ascii() { return verdict; }
        // The column may count UTF-8 bytes, which wider characters before the call outnumber.
        at(column)
    }

    fn cpp_class_decls(&mut self, qualified_name: &str) -> Res<Vec<Arc<KNode>>> {
        Ok(self.nodes_by_qualified_name(qualified_name)?.iter().filter(|n| class_kind(&n.kind) && c_family(&n.language)).cloned().collect())
    }

    /// The qualified name of the class whose scope a C++ caller's body is in: a member function's,
    /// defined in the class or out of line (`void Api::InternalSwap(…) {…}`), static or not — or a
    /// class's own, for code in its body. A function node counts only when its definition is
    /// written `Class::name(…)`. None for a free function, and for one defined through a namespace
    /// (`void detail::helper() {}` is a method node of no class).
    fn cpp_member_owner(&mut self, caller: &KNode) -> Res<Option<String>> {
        if class_kind(&caller.kind) { return Ok(Some(caller.qualified_name.clone())); }
        if caller.kind != "method" && caller.kind != "function" { return Ok(None); }
        let Some(cut) = caller.qualified_name.rfind("::").filter(|c| *c > 0) else { return Ok(None) };
        let owner = &caller.qualified_name[..cut];
        if caller.kind == "function" {
            let Some(source) = self.read_file(&caller.file_path) else { return Ok(None) };
            let from = (caller.start_line - 1).max(0) as usize;
            let head = (from..from + 3).filter_map(|i| source.get(i).map(String::as_str)).collect::<Vec<_>>().join(" ");
            let definition = shared_regex(&format!(
                r"\b{}\s*(?:<[^<>;{{}}]*>\s*)?::\s*~?{}\s*\(",
                regex::escape(&cpp_last_segment(owner)),
                regex::escape(&caller.name),
            ))?;
            if !definition.is_match(&head) { return Ok(None); }
        }
        if !self.cpp_class_decls(owner)?.is_empty() { return Ok(Some(owner.to_string())); }
        // A class declared in another and defined elsewhere, which the index has no node for:
        // protobuf's `Any::_Internal`, declared in `Any`.
        let outer = owner.rfind("::").filter(|o| *o > 0);
        match outer {
            Some(outer) if !self.cpp_class_decls(&owner[..outer])?.is_empty() => Ok(Some(owner.to_string())),
            _ => Ok(None),
        }
    }

    /// The classes a C++ caller's name lookup passes through, innermost first: the caller's class,
    /// then each class it is nested in. After a class declared in a function body comes that
    /// function's class. Several classes can share a qualified name — another translation unit's
    /// local fixture or helper — so each scope keeps only the declaration holding the code, else
    /// those in its file, else those its file includes, else all of them.
    fn cpp_enclosing_classes(&mut self, owner: &str, caller: &Arc<KNode>) -> Res<Vec<ClassScope>> {
        let mut scopes = Vec::new();
        let mut qualified_name = owner.to_string();
        let mut inner = caller.clone();
        let mut hops = 0;
        while hops < 8 && !qualified_name.is_empty() {
            let decls = self.cpp_class_decls(&qualified_name)?;
            if decls.is_empty() && hops > 0 {
                // A namespace: lookup leaves the classes. Unless it is the function a local class
                // is declared in (`Clear::Local`), whose class comes next.
                let last = cpp_last_segment(&qualified_name);
                let function = self.nodes_in_file(&inner.file_path)?.iter()
                    .find(|n| n.name == last && (n.kind == "method" || n.kind == "function") && range_within(&inner, n) && !same_range(&inner, n))
                    .cloned();
                let function_owner = match &function { Some(f) => self.cpp_member_owner(f)?, None => None };
                let (Some(function), Some(function_owner)) = (function, function_owner) else { break };
                qualified_name = function_owner;
                inner = function;
                hops += 1;
                continue;
            }
            let scope = self.cpp_scope_declarations(&qualified_name, decls, &inner)?;
            if let Some(first) = scope.decls.first() { inner = first.clone(); }
            scopes.push(scope);
            qualified_name = match qualified_name.rfind("::").filter(|c| *c > 0) {
                Some(cut) => qualified_name[..cut].to_string(),
                None => String::new(),
            };
            hops += 1;
        }
        Ok(scopes)
    }

    /// A class scope's declarations as `cpp_enclosing_classes` keeps them.
    fn cpp_scope_declarations(&mut self, qualified_name: &str, decls: Vec<Arc<KNode>>, inner: &KNode) -> Res<ClassScope> {
        let total = decls.len();
        let scope = |kept: Vec<Arc<KNode>>, holds: bool| ClassScope { qualified_name: qualified_name.to_string(), narrowed: kept.len() < total, decls: kept, holds };
        // (Code in a class's own body, a member's initializer, has that class as `inner`.)
        let holding: Vec<_> = decls.iter().filter(|d| d.file_path == inner.file_path && range_within(inner, d)).cloned().collect();
        if !holding.is_empty() { return Ok(scope(holding, true)); }
        let local: Vec<_> = decls.iter().filter(|d| d.file_path == inner.file_path).cloned().collect();
        if !local.is_empty() || total < 2 { return Ok(scope(if local.is_empty() { decls } else { local }, false)); }
        let visible = self.namespace_visible_files(&inner.file_path, "cpp")?;
        let included: Vec<_> = decls.iter().filter(|d| visible.contains(&d.file_path)).cloned().collect();
        Ok(scope(if included.is_empty() { decls } else { included }, false))
    }

    /// The method `name` a class scope gives C++ lookup: the class's own — first one written in
    /// the very declaration holding the code, as an inline member — else the nearest one of a class
    /// it derives from, through the kept declarations' base edges.
    fn cpp_scope_method(&mut self, scope: &ClassScope, name: &str, receiver: Receiver, r: &ResolveRefIn) -> Res<Lookup> {
        let candidates: Vec<_> = self.nodes_by_qualified_name(&format!("{}::{name}", scope.qualified_name))?
            .iter().filter(|n| n.kind == "method" && c_family(&n.language)).cloned().collect();
        let mut own = Vec::new();
        for n in candidates {
            // A namesake class's members are defined where it is: leveldb's fault-injection test
            // declares a `FileState` of its own, with no `Truncate`, beside memenv.cc's.
            let mut belongs = !scope.narrowed;
            for d in &scope.decls {
                if belongs { break; }
                belongs = d.file_path == n.file_path || self.namespace_visible_files(&d.file_path, "cpp")?.contains(&n.file_path);
            }
            if belongs { own.push(n); }
        }
        let holder = if scope.holds { scope.decls.first() } else { None };
        let (inline, rest): (Vec<_>, Vec<_>) = match holder {
            Some(h) => own.into_iter().partition(|n| n.file_path == h.file_path && range_within(n, h)),
            None => (Vec::new(), own),
        };
        // A class with no name (a partial specialization, an unnamed struct) shares its qualified
        // name with every other: only its inline members are its own.
        let out_of_line = if cpp_last_segment(&scope.qualified_name) == "<anonymous>" { Vec::new() } else { rest };
        if !inline.is_empty() || !out_of_line.is_empty() {
            let mut overloads = inline;
            overloads.extend(prefer_call_site_file(out_of_line, &r.file_path));
            return Ok(Lookup::Found(self.cpp_overload_for(overloads, name, r)));
        }
        for decl in &scope.decls {
            let words = self.cpp_member_macro_arguments(decl);
            if words.iter().any(|a| name.starts_with(a.as_str()) || name.ends_with(a.as_str())) { return Ok(Lookup::Stop); }
        }
        if !self.supertypes_complete { return Ok(Lookup::Missing); }
        for decl in &scope.decls {
            for base in self.supertype_nodes(&decl.id)? {
                if !class_kind(&base.kind) { continue; }
                if receiver == Receiver::Bare && self.cpp_dependent_base(decl, &base)? { continue; }
                let mut seen = HashSet::from([scope.qualified_name.clone()]);
                let Some(inherited) = self.cpp_method_of(&base, name, r, 1, &mut seen)? else { continue };
                // Its class's other overloads, which the arguments may fit better (rocksdb's
                // `RegisterOptions(name, &opts, &info)` is not the two-parameter template beside it).
                let siblings: Vec<_> = self.nodes_by_qualified_name(&inherited.qualified_name)?
                    .iter().filter(|n| n.id != inherited.id && n.kind == "method" && c_family(&n.language)).cloned().collect();
                let mut overloads = vec![inherited];
                overloads.extend(prefer_call_site_file(siblings, &r.file_path));
                return Ok(Lookup::Found(self.cpp_overload_for(overloads, name, r)));
            }
        }
        Ok(Lookup::Missing)
    }

    /// C++ class `cls`'s method `name`: its own, else the nearest one of a class it derives from,
    /// through the base edges every declaration of `cls` has.
    fn cpp_method_of(&mut self, cls: &KNode, name: &str, r: &ResolveRefIn, depth: usize, seen: &mut HashSet<String>) -> Res<Option<Arc<KNode>>> {
        let own: Vec<_> = self.nodes_by_qualified_name(&format!("{}::{name}", cls.qualified_name))?
            .iter().filter(|n| n.kind == "method" && c_family(&n.language)).cloned().collect();
        if !own.is_empty() { return Ok(prefer_call_site_file(own, &r.file_path).into_iter().next()); }
        seen.insert(cls.qualified_name.clone());
        if depth >= 4 || !self.supertypes_complete { return Ok(None); }
        for decl in self.cpp_class_decls(&cls.qualified_name)? {
            for base in self.supertype_nodes(&decl.id)? {
                if !class_kind(&base.kind) || seen.contains(&base.qualified_name) { continue; }
                if let Some(inherited) = self.cpp_method_of(&base, name, r, depth + 1, seen)? { return Ok(Some(inherited)); }
            }
        }
        Ok(None)
    }

    /// Of one class's overloads of `name` (in preference order), the one the call's arguments fit best.
    fn cpp_overload_for(&mut self, overloads: Vec<Arc<KNode>>, name: &str, r: &ResolveRefIn) -> Arc<KNode> {
        let mut best = overloads[0].clone();
        if overloads.len() == 1 { return best; }
        let Some(args) = self.call_arguments(r, name) else { return best };
        let mut best_fit = self.cpp_overload_fit(&best, &args, r);
        for n in overloads.into_iter().skip(1) {
            let fit = self.cpp_overload_fit(&n, &args, r);
            if fit > best_fit {
                best = n;
                best_fit = fit;
            }
        }
        best
    }

    /// The words in the arguments of the macros a C++ class declaration calls among its members,
    /// like `Message` in protobuf's `LOCAL_VAR_ACCESSOR(Message*, Message);`: such a macro can
    /// declare a member the index has no node for (`Get##name` makes `GetMessage`).
    fn cpp_member_macro_arguments(&mut self, decl: &KNode) -> Rc<Vec<String>> {
        if let Some(hit) = self.cpp_macro_arguments_memo.get(&decl.id) { return hit.clone(); }
        let mut words = Vec::new();
        if let Some(source) = self.read_file(&decl.file_path) {
            let lines = source.cpp_code_lines();
            let first = (decl.start_line - 1).max(0) as usize;
            let mut depth = 0i32;
            for (i, line) in lines.iter().enumerate().take(decl.end_line.max(0) as usize).skip(first) {
                let code = if i == first { line.get(names::js_unit_to_byte(line, decl.start_column.max(0) as usize)..).unwrap_or("") } else { line.as_str() };
                if depth == 1 {
                    if let Some(call) = re!(r"^\s*[A-Z][A-Z0-9_]*\s*\(([^;{}]*)\)\s*;?\s*$").captures(code) {
                        words.extend(call[1].split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| w.len() >= 3).map(str::to_string));
                    }
                }
                for c in code.chars() {
                    match c { '{' => depth += 1, '}' => depth -= 1, _ => {} }
                }
            }
        }
        let words = Rc::new(words);
        self.cpp_macro_arguments_memo.insert(decl.id.clone(), words.clone());
        words
    }

    /// The base specifiers a C or C++ class declaration writes (`public Base<T>`, `private
    /// Mixin`), read from its own head; none for a forward declaration.
    fn cpp_base_specifiers(&mut self, decl: &KNode) -> Vec<String> {
        let Some(source) = self.read_file(&decl.file_path) else { return Vec::new() };
        let lines = source.cpp_code_lines();
        let first = (decl.start_line - 1).max(0) as usize;
        let head: Vec<&str> = (first..(first + 12).min(lines.len())).map(|i| {
            let line = lines[i].as_str();
            if i == first { line.get(names::js_unit_to_byte(line, decl.start_column.max(0) as usize)..).unwrap_or("") } else { line }
        }).collect();
        let text = head.join("\n");
        let Some(brace) = text.find('{') else { return Vec::new() };
        let before = &text[..brace];
        let bytes = before.as_bytes();
        // The first single colon: not half of a `::`.
        let colon = (0..bytes.len()).find(|&i| bytes[i] == b':' && (i == 0 || bytes[i - 1] != b':') && bytes.get(i + 1) != Some(&b':'));
        let Some(colon) = colon.filter(|_| !before.contains(';')) else { return Vec::new() };
        split_top_level(&text[colon + 1..brace]).into_iter()
            .map(|s| re!(r"(?-u:\b)(?:public|protected|private|virtual)(?-u:\b)").replace_all(&s, " ").trim().to_string())
            .collect()
    }

    /// Whether `base`, a base of the C++ class declaration `decl`, depends on the template
    /// parameters of `decl` or a class around it: `template <class T> class Foo : public Base<T>`.
    /// A bare name is not looked up in such a base.
    fn cpp_dependent_base(&mut self, decl: &KNode, base: &KNode) -> Res<bool> {
        let parameters = self.cpp_template_parameter_names(decl)?;
        if parameters.is_empty() { return Ok(false); }
        let spec = self.cpp_base_specifiers(decl).into_iter().find(|s| cpp_last_segment(strip_template_arguments(s).trim()) == base.name);
        Ok(spec.is_some_and(|spec| parameters.iter().any(|p| contains_word(&spec, p))))
    }

    /// The template parameters in scope where `node` is declared: its own `template <…>` header
    /// and those of the class templates around it.
    fn cpp_template_parameter_names(&mut self, node: &KNode) -> Res<HashSet<String>> {
        let mut names = HashSet::new();
        let Some(source) = self.read_file(&node.file_path) else { return Ok(names) };
        collect_template_header(&source, node.start_line, node.start_column, &mut names);
        let mut scope = node.qualified_name.as_str();
        while let Some(cut) = scope.rfind("::").filter(|c| *c > 0) {
            scope = &scope[..cut];
            for cls in self.cpp_class_decls(scope)? {
                if cls.file_path != node.file_path || node.start_line < cls.start_line || node.start_line > cls.end_line { continue; }
                collect_template_header(&source, cls.start_line, cls.start_column, &mut names);
            }
        }
        Ok(names)
    }

    /// Whether the C++ function `caller` declares `name` before the call at `r` — a parameter, a
    /// local, a lambda's parameter, a range-`for` variable, a structured binding — which a bare
    /// call then calls, not a member. A declaration in an earlier block the call is outside of
    /// counts too: the name strategies decide, as before.
    fn cpp_local_name(&mut self, name: &str, caller: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if caller.file_path != r.file_path || class_kind(&caller.kind) { return Ok(false); }
        let Some(source) = self.read_file(&r.file_path) else { return Ok(false) };
        let lines = source.cpp_code_lines();
        let escaped = regex::escape(name);
        let word = shared_regex(&format!(r"(?-u:\b){escaped}(?-u:\b)"))?;
        let rebinds = shared_regex(&format!(r"(?-u:\b)for\s*\(.*(?-u:\b){escaped}\s*:([^:]|$)|(?-u:\b)auto\s*&{{0,2}}\s*\[[^\]]*(?-u:\b){escaped}(?-u:\b)[^\]]*\]"))?;
        let from = (caller.start_line - 1).max(0) as usize;
        for code in lines.iter().take(lines.len().min(r.line.max(0) as usize)).skip(from) {
            if !word.is_match(code) { continue; }
            if rebinds.is_match(code) { return Ok(true); }
            // `std::function<void()> done;` reads as a declaration once its template arguments are gone.
            for variant in [code.clone(), strip_template_arguments(code)] {
                let Some(ty) = self.cpp_declarator_match(&variant, &escaped)? else { continue };
                // `Owner::name(` is a definition's own declarator; `return name(` no declaration.
                if !ty.trim_end().ends_with("::") && self.normalize_cpp_type_name(&ty, false)?.is_some() { return Ok(true); }
            }
        }
        Ok(false)
    }
}

/// Add the parameter names of the `template <…>` header that ends right where a declaration starts.
fn collect_template_header(source: &SourceFile, start_line: i64, start_column: i64, names: &mut HashSet<String>) {
    let line = (start_line - 1).max(0) as usize;
    let mut before: Vec<&str> = (line.saturating_sub(3)..line).filter_map(|i| source.get(i).map(String::as_str)).collect();
    let current = source.get(line).map(String::as_str).unwrap_or("");
    before.push(&current[..names::js_unit_to_byte(current, start_column.max(0) as usize)]);
    let before = before.join("\n");
    let Some(at) = before.rfind("template") else { return };
    let Some(header) = re!(r"(?s)^template\s*<(.*)>\s*$").captures(&before[at..]) else { return };
    for item in split_top_level(&header[1]) {
        // `typename T = int`, `template <typename> class Policy`, `typename... Ts`, `int N`.
        let stripped = strip_template_arguments(&item);
        let declared = stripped.split('=').next().unwrap_or("");
        if let Some(name) = re!(r"([A-Za-z_][A-Za-z0-9_]*)\s*$").captures(declared).map(|m| m[1].to_string()) {
            if name != "typename" && name != "class" { names.insert(name); }
        }
    }
}
