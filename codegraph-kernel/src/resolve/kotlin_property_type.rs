//! Kotlin property, chain and call-result declared types.
use super::*;

/// Stands for "a type the project does not declare" — a library class, or the
/// result of a call on one — so the caller can refuse a name-only guess.
pub(super) const KOTLIN_EXTERNAL: &str = "\u{0}external";

/// Library types whose member is one of their type arguments: `uiState.value`
/// on a `StateFlow<UiState>` is a `UiState`.
fn kotlin_holder_index(name: &str, member: &str) -> Option<usize> {
    let holders: &[(&str, &[(&str, usize)])] = &[
        ("StateFlow", &[("value", 0)]),
        ("MutableStateFlow", &[("value", 0)]),
        ("State", &[("value", 0)]),
        ("MutableState", &[("value", 0)]),
        ("LiveData", &[("value", 0)]),
        ("MutableLiveData", &[("value", 0)]),
        ("Lazy", &[("value", 0)]),
        ("Pair", &[("first", 0), ("second", 1)]),
        ("Triple", &[("first", 0), ("second", 1), ("third", 2)]),
    ];
    holders
        .iter()
        .find(|(ty, _)| *ty == name)
        .and_then(|(_, members)| members.iter().find(|(m, _)| *m == member).map(|(_, i)| *i))
}

/// Node kinds that declare a type a receiver can have.
fn declared_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "interface" | "struct" | "enum" | "type_alias" | "trait" | "protocol" | "union"
    )
}

/// The declared type of a Kotlin property named `name` in `text`.
pub(super) enum KotlinDeclared {
    Type(String),
    External,
}

impl KernelResolver {
    /// A Kotlin type written in source (`Lease?`, `HardwareLock.Lease`,
    /// `List<Foo>`) as a name `resolve_method_on_type` matches: nullability
    /// and type arguments dropped, a nested type joined with `::`, a package
    /// prefix (lowercase segments) dropped. `None` when it is not a type name.
    pub(super) fn kotlin_type_name(raw: &str) -> Option<String> {
        let cleaned = re!(r"<[^>]*>").replace_all(raw, "").replace('?', "");
        let segments: Vec<&str> = cleaned.trim().split('.').filter(|s| !s.is_empty()).collect();
        let type_seg = re!(r"^[A-Z]\w*$");
        let mut first = segments.len();
        while first > 0 && type_seg.is_match(segments[first - 1]) {
            first -= 1;
        }
        let type_segments = &segments[first..];
        (!type_segments.is_empty()).then(|| type_segments.join("::"))
    }

    /// Whether the project declares `type_name` (simple, or `Outer::Inner`) in
    /// the ref's language family. False for library types (`Regex`).
    pub(super) fn is_project_type(&mut self, type_name: &str, r: &ResolveRefIn) -> Res<bool> {
        let simple = type_name.rsplit("::").next().unwrap_or(type_name);
        let nested = type_name.contains("::");
        Ok(self.nodes_by_name(simple)?.iter().any(|n| {
            declared_type_kind(&n.kind)
                && same_language_family(&n.language, &r.language)
                && (!nested
                    || n.qualified_name == type_name
                    || n.qualified_name.ends_with(&format!("::{type_name}")))
        }))
    }

    fn kotlin_file_lines(&mut self, file: &str) -> Option<Rc<SourceFile>> {
        let lines = self.read_file(file)?;
        (!lines.is_empty()).then_some(lines)
    }

    /// A Kotlin class's lines before its first member: its annotations, name,
    /// primary constructor and supertype list.
    fn kotlin_class_header(&mut self, owner: &KNode, in_file: &[Arc<KNode>], lines: &SourceFile) -> String {
        let owner_end = owner.end_line;
        let mut first_member = owner_end + 1;
        for n in in_file {
            if n.id == owner.id || n.kind == "file" {
                continue;
            }
            if n.start_line > owner.start_line && n.start_line <= owner_end && n.start_line < first_member {
                first_member = n.start_line;
            }
        }
        let from = ((owner.start_line - 1).max(0) as usize).min(lines.len());
        let to = ((first_member - 1).max(0) as usize).clamp(from, lines.len());
        lines[from..to].join("\n")
    }

    /// The supertype names a Kotlin class header lists after its primary
    /// constructor (`class A(…) : Base(…), Iface<T>, Api by impl {`), as
    /// `kotlin_type_name` gives them. Colons and commas inside the
    /// constructor or type arguments are skipped; a `where` clause or the
    /// body ends the list.
    fn kotlin_supertype_names(header: &str, class_name: &str) -> Vec<String> {
        let Ok(decl) = Self::cached_regex(&format!(
            r"\b(?:class|object|interface)\s+{}\b",
            regex::escape(class_name)
        )) else {
            return Vec::new();
        };
        let Some(decl) = decl.find(header) else { return Vec::new() };
        let bytes = header.as_bytes();
        let mut entries: Vec<String> = Vec::new();
        let mut paren = 0i32;
        let mut angle = 0i32;
        let mut start: i64 = -1;
        let mut i = decl.end();
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c == '(' {
                paren += 1;
            } else if c == ')' {
                paren -= 1;
            } else if paren > 0 {
            } else if c == '<' {
                angle += 1;
            } else if c == '>' {
                angle -= 1;
            } else if angle > 0 {
            } else if c == '{' {
                // The class body ends the supertype list.
                break;
            } else if start >= 0
                && c == 'w'
                && header[i..].starts_with("where")
                && !bytes[i.saturating_sub(1)].is_ascii_alphanumeric() && bytes[i.saturating_sub(1)] != b'_'
            {
                break;
            } else if c == ':' && start < 0 {
                start = i as i64 + 1;
            } else if c == ',' && start >= 0 {
                entries.push(header[start as usize..i].to_string());
                start = i as i64 + 1;
            }
            i += 1;
            // Multi-byte characters: advance a whole char.
            while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                i += 1;
            }
        }
        if start >= 0 {
            entries.push(header[start as usize..i.min(bytes.len())].to_string());
        }
        let mut names = Vec::new();
        for entry in entries {
            let Some(m) = re!(r"^\s*(?:@\w+\s+)*([A-Za-z_][\w.]*)").captures(&entry) else { continue };
            if let Some(ty) = Self::kotlin_type_name(&m[1]) {
                names.push(ty);
            }
        }
        names
    }

    /// The declared type of Kotlin property `name` in class `owner`: from the
    /// property's own declaration lines when it is a node (`lateinit var
    /// repo: Repo`, `val repo = Repo(…)`), otherwise from the class header,
    /// where a primary-constructor property (`class A(private val repo:
    /// Repo)`) lives. The header is the class's lines before its first
    /// member, so a same-named local inside a method body is never read as
    /// the property. `None` when no declaration names a type.
    pub(super) fn kotlin_property_declared_type(
        &mut self,
        name: &str,
        property: Option<&Arc<KNode>>,
        owner: &KNode,
        in_file: &[Arc<KNode>],
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let Some(lines) = self.kotlin_file_lines(&r.file_path) else {
            return Ok(None);
        };
        let mut at = r.clone();
        let text = match property {
            Some(property) => {
                at.line = property.start_line;
                let from = ((property.start_line - 1).max(0) as usize).min(lines.len());
                let to = (property.end_line.max(0) as usize).clamp(from, lines.len());
                lines[from..to].join("\n")
            }
            None => {
                at.line = owner.start_line;
                self.kotlin_class_header(owner, in_file, &lines)
            }
        };
        Ok(self
            .kotlin_declared_type_in(&text, name, &at, 0)?
            .map(|d| match d {
                KotlinDeclared::Type(t) => t,
                KotlinDeclared::External => KOTLIN_EXTERNAL.to_string(),
            }))
    }

    /// The declared type of Kotlin property `name` if class `owner` declares
    /// it — as a member node or a primary-constructor `val`/`var` — read in the
    /// owner's own file. `None` (outer) when the owner does not declare it;
    /// `Some(None)` when it does without a type the declaration names.
    fn kotlin_own_property_type(
        &mut self,
        name: &str,
        owner: &KNode,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<String>>> {
        let in_file = self.nodes_in_file(&owner.file_path)?;
        let qualified = format!("{}::{}", owner.qualified_name, name);
        let property = in_file
            .iter()
            .find(|n| {
                matches!(n.kind.as_str(), "field" | "constant" | "variable")
                    && n.qualified_name == qualified
            })
            .cloned();
        if property.is_none() {
            let Some(lines) = self.kotlin_file_lines(&owner.file_path) else {
                return Ok(Some(None));
            };
            let header = self.kotlin_class_header(owner, &in_file, &lines);
            let declared = Self::cached_regex(&format!(
                r"\b(?:val|var)\s+{}\b",
                regex::escape(name)
            ))?;
            if !declared.is_match(&header) {
                return Ok(None);
            }
        }
        // Read the declaration in the owner's file, as if the ref sat there.
        let mut at = r.clone();
        at.file_path = owner.file_path.clone();
        at.line = property.as_ref().map(|p| p.start_line).unwrap_or(owner.start_line);
        let result = match &property {
            Some(property) => {
                let lines = self.kotlin_file_lines(&owner.file_path);
                let Some(lines) = lines else { return Ok(Some(None)) };
                let from = ((property.start_line - 1).max(0) as usize).min(lines.len());
                let to = (property.end_line.max(0) as usize).clamp(from, lines.len());
                let text = lines[from..to].join("\n");
                self.kotlin_declared_type_in(&text, name, &at, 0)?
            }
            None => {
                let lines = self.kotlin_file_lines(&owner.file_path);
                let Some(lines) = lines else { return Ok(Some(None)) };
                let header = self.kotlin_class_header(owner, &in_file, &lines);
                self.kotlin_declared_type_in(&header, name, &at, 0)?
            }
        };
        Ok(Some(result.map(|d| match d {
            KotlinDeclared::Type(t) => t,
            KotlinDeclared::External => KOTLIN_EXTERNAL.to_string(),
        })))
    }

    /// The declared type of Kotlin property `name` that class `owner`
    /// inherits: the nearest project supertype (breadth-first, at most four
    /// levels) declaring it, with supertypes read from each class header. A
    /// supertype the project does not declare, or whose name is ambiguous, is
    /// not walked — the property may come from it — so the result is then
    /// None (unknown), never external.
    fn kotlin_inherited_property_type(
        &mut self,
        name: &str,
        owner: &KNode,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let mut seen: HashSet<String> = HashSet::from([owner.id.clone()]);
        let mut level: Vec<Arc<KNode>> = vec![Arc::new(owner.clone())];
        for _depth in 0..4 {
            if level.is_empty() {
                break;
            }
            let mut next: Vec<Arc<KNode>> = Vec::new();
            for cls in &level {
                let Some(lines) = self.kotlin_file_lines(&cls.file_path) else { continue };
                let in_file = self.nodes_in_file(&cls.file_path)?;
                let header = self.kotlin_class_header(cls, &in_file, &lines);
                for super_name in Self::kotlin_supertype_names(&header, &cls.name) {
                    let simple = super_name.rsplit("::").next().unwrap_or(&super_name).to_string();
                    let supers: Vec<Arc<KNode>> = self
                        .nodes_by_name(&simple)?
                        .iter()
                        .filter(|n| {
                            matches!(n.kind.as_str(), "class" | "interface")
                                && n.language == "kotlin"
                                && (n.qualified_name == super_name
                                    || n.qualified_name.ends_with(&format!("::{super_name}")))
                        })
                        .cloned()
                        .collect();
                    let [sup] = supers.as_slice() else { continue };
                    if !seen.insert(sup.id.clone()) {
                        continue;
                    }
                    if let Some(declared) = self.kotlin_own_property_type(name, sup, r)? {
                        return Ok(declared);
                    }
                    next.push(sup.clone());
                }
            }
            level = next;
        }
        Ok(None)
    }

    /// The type a Kotlin declaration of `name` in `text` gives it: `name:
    /// Type`, a constructor call `name = Type(…)`, or a call `name =
    /// Owner.fn(…)` / `name = fn(…)`, typed by `fn`'s declared return type.
    /// `External` when the owner is not a project type; `None` when nothing
    /// names a type.
    fn kotlin_declared_type_in(
        &mut self,
        text: &str,
        name: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Res<Option<KotlinDeclared>> {
        let escaped = regex::escape(name);
        // A package written before the type is dropped by kotlin_type_name.
        let typed = Self::cached_regex(&format!(
            r"\b(?:val|var)\s+{escaped}\s*:\s*((?:[a-z_]\w*\.)*[A-Z][\w.]*)"
        ))?;
        if let Some(m) = typed.captures(text) {
            if let Some(ty) = Self::kotlin_type_name(&m[1]) {
                return Ok(Some(KotlinDeclared::Type(ty)));
            }
        }
        // An alias of another value: `val old = ws`, `val s = current ?: return`.
        let alias = Self::cached_regex(&format!(
            r"(?m)\b(?:val|var)\s+{escaped}\s*=\s*([a-z_]\w*)\s*(?:!!|\?:\s*(?:return|continue|break|throw)\b[^\n]*)?\s*$"
        ))?;
        if let Some(m) = alias.captures(text) {
            let other = m[1].to_string();
            if other != name && depth < 3 {
                return self.kotlin_receiver_declared_type(&other, r, depth + 1);
            }
        }
        // `(` or a trailing lambda `{`: `Thread { … }`, `thread { … }`.
        // Type arguments are skipped: `WebRtcPlaneSession<IceCandidate>(plane)`.
        let assigned = Self::cached_regex(&format!(
            r"\b(?:val|var)\s+{escaped}\s*=\s*([A-Za-z_][\w.]*)\s*(?:<[\w\s.,?*<>]*>\s*)?[(\{{]"
        ))?;
        let Some(assigned) = assigned.captures(text) else {
            // An enum entry or other constant of a type: `var mode = GpsMode.ON`.
            let entry = Self::cached_regex(&format!(
                r"\b(?:val|var)\s+{escaped}\s*=\s*([A-Z]\w*(?:\.[A-Z]\w*)*)\.[A-Z][A-Z0-9_]*"
            ))?;
            let Some(m) = entry.captures(text) else { return Ok(None) };
            let after = text[m.get(0).unwrap().end()..].chars().next();
            if matches!(after, Some('.') | Some('(')) {
                return Ok(None);
            }
            // Only an entry of a project enum names its type; `Limits.MAX` is
            // an Int.
            let Some(ty) = Self::kotlin_type_name(&m[1]) else { return Ok(None) };
            let simple = ty.rsplit("::").next().unwrap_or(&ty).to_string();
            let is_enum = self
                .nodes_by_name(&simple)?
                .iter()
                .any(|n| n.kind == "enum" && same_language_family(&n.language, &r.language));
            return Ok(is_enum.then_some(KotlinDeclared::Type(ty)));
        };
        let full = assigned[1].to_string();
        let mut segments: Vec<&str> = full.split('.').collect();
        let last = segments.pop().unwrap_or(&full).to_string();
        if last.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return Ok(Self::kotlin_type_name(&full).map(KotlinDeclared::Type));
        }
        // A call. Only an owner written as a type chain (`Lock.tryBegin`) or
        // none at all (`beginOp()`) can be typed; `a.b.fn()` goes through
        // values.
        if !segments.is_empty()
            && !segments
                .iter()
                .all(|seg| seg.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        {
            return Ok(None);
        }
        const ARGUMENT_PASSTHROUGH: [&str; 2] = ["requireNotNull", "checkNotNull"];
        if segments.is_empty() && ARGUMENT_PASSTHROUGH.contains(&last.as_str()) {
            let arg = Self::cached_regex(&format!(r"\b{last}\s*\(\s*([A-Za-z_]\w*)\s*[,)]"))?
                .captures(text)
                .map(|m| m[1].to_string());
            let Some(arg) = arg.filter(|arg| arg != name) else { return Ok(None) };
            let arg_type = self.infer_local_receiver_type(&arg, r, false)?;
            return Ok(arg_type
                .filter(|t| t.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
                .map(KotlinDeclared::Type));
        }
        let owner = (!segments.is_empty()).then(|| segments.join("::"));
        self.kotlin_qualified_call_result_type(owner.as_deref(), &last, r)
    }

    /// The declared return type of Kotlin `owner.fn` (or of an unqualified
    /// `fn`, a method of the ref's own class first, then one in the same
    /// file). Every candidate must declare the same return type. A return
    /// type nested in the callee's owner keeps that owner (`Lease` in
    /// `HardwareLock` → `HardwareLock::Lease`). `External` for a call on a
    /// type the project does not declare.
    fn kotlin_qualified_call_result_type(
        &mut self,
        owner: Option<&str>,
        func: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KotlinDeclared>> {
        if let Some(owner) = owner {
            if !self.is_project_type(owner, r)? {
                return Ok(Some(KotlinDeclared::External));
            }
        }
        let mut candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(func)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "method" | "function")
                    && n.language == "kotlin"
                    && n.signature.is_some()
            })
            .cloned()
            .collect();
        if let Some(owner) = owner {
            candidates.retain(|n| n.qualified_name.ends_with(&format!("{owner}::{func}")));
        } else {
            let in_file = self.nodes_in_file(&r.file_path)?;
            let own_class = in_file
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "class" | "interface")
                        && n.start_line <= r.line
                        && n.end_line >= r.line
                })
                .max_by_key(|n| n.start_line);
            let in_own_class: Vec<Arc<KNode>> = own_class
                .map(|own| {
                    candidates
                        .iter()
                        .filter(|n| n.qualified_name == format!("{}::{func}", own.qualified_name))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let in_file_calls: Vec<Arc<KNode>> = candidates
                .iter()
                .filter(|n| n.file_path == r.file_path)
                .cloned()
                .collect();
            candidates = if !in_own_class.is_empty() {
                in_own_class
            } else if !in_file_calls.is_empty() {
                in_file_calls
            } else {
                candidates
            };
        }
        // No project function of that name. A known stdlib factory returns a
        // library type; any other library function may return a project
        // value, so its result stays unknown.
        if candidates.is_empty() {
            const LIBRARY_FACTORIES: [&str; 34] = [
                "listOf", "mutableListOf", "arrayListOf", "emptyList", "listOfNotNull",
                "setOf", "mutableSetOf", "hashSetOf", "linkedSetOf", "sortedSetOf", "emptySet",
                "mapOf", "mutableMapOf", "hashMapOf", "linkedMapOf", "sortedMapOf", "emptyMap",
                "arrayOf", "arrayOfNulls", "emptyArray", "intArrayOf", "longArrayOf", "byteArrayOf",
                "sequenceOf", "emptySequence", "buildList", "buildSet", "buildMap", "buildString",
                "lazy", "thread", "mutableStateOf", "Channel", "MutableStateFlow",
            ];
            let library = owner.is_none() && LIBRARY_FACTORIES.contains(&func);
            return Ok(library.then_some(KotlinDeclared::External));
        }
        let mut result: Option<String> = None;
        for c in &candidates {
            let Some(sig) = c.signature.as_deref() else { continue };
            let Some(at) = sig.rfind("):") else { return Ok(None) };
            let Some(mut ty) = Self::kotlin_type_name(&sig[at + 2..]) else { return Ok(None) };
            if !ty.contains("::") {
                let container = c.qualified_name.rsplit_once("::").map(|(o, _)| o).unwrap_or_default();
                let container_name = container.rsplit("::").next().unwrap_or(container);
                if !container_name.is_empty()
                    && self
                        .nodes_by_qualified_name(&format!("{container}::{ty}"))?
                        .iter()
                        .any(|n| declared_type_kind(&n.kind))
                {
                    ty = format!("{container_name}::{ty}");
                }
            }
            if result.as_ref().is_some_and(|r| r != &ty) {
                return Ok(None);
            }
            result = Some(ty);
        }
        Ok(result.map(KotlinDeclared::Type))
    }

    /// The tightest Kotlin class or interface enclosing `r`'s line.
    fn kotlin_enclosing_class(&mut self, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        Ok(self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "class" | "interface")
                    && n.language == "kotlin"
                    && n.start_line <= r.line
                    && n.end_line >= r.line
            })
            .max_by_key(|n| n.start_line)
            .cloned())
    }

    /// The 0-based lines a Kotlin local visible at `r` may be declared on,
    /// nearest first: the enclosing function above the call, then — when that
    /// function is nested in another (a member of an anonymous object, a local
    /// function) — each outer function above the nested one, whose locals it
    /// captures. Lines of a function nested in a scanned one that does not
    /// enclose `r` are skipped: its locals are not visible. Outside any
    /// function, every line above the call.
    fn kotlin_local_scan_lines(&mut self, r: &ResolveRefIn, line_count: usize) -> Res<Vec<usize>> {
        let call_idx = ((r.line - 1).max(0) as usize).min(line_count.saturating_sub(1));
        let fns: Vec<Arc<KNode>> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "function" | "method") && n.language == "kotlin"
            })
            .cloned()
            .collect();
        let mut enclosing: Vec<Arc<KNode>> = fns
            .iter()
            .filter(|n| n.start_line <= r.line && n.end_line >= r.line)
            .cloned()
            .collect();
        enclosing.sort_by_key(|n| std::cmp::Reverse(n.start_line));
        let mut out: Vec<usize> = Vec::new();
        if enclosing.is_empty() {
            return Ok((0..=call_idx).rev().collect());
        }
        let mut hi = call_idx;
        for func in &enclosing {
            let fn_end = func.end_line;
            let hidden: Vec<&Arc<KNode>> = fns
                .iter()
                .filter(|n| {
                    n.start_line > func.start_line
                        && n.end_line <= fn_end
                        && !(n.start_line <= r.line && n.end_line >= r.line)
                })
                .collect();
            let low = (func.start_line - 1).max(0) as usize;
            let mut i = hi as i64;
            while i >= low as i64 {
                let line = i + 1;
                if !hidden.iter().any(|n| line >= n.start_line && line <= n.end_line) {
                    out.push(i as usize);
                }
                i -= 1;
            }
            hi = hi.min(low.saturating_sub(1));
        }
        Ok(out)
    }

    /// Whether an enclosing Kotlin lambda binds `name` — one of its declared
    /// parameters, or the implicit `it`. Such a name shadows every property the
    /// enclosing class declares, so it is the claim model's, not a receiver a
    /// property or chain types.
    pub(super) fn kotlin_lambda_binds(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(file) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(tree) = self.parsed_tree(&file, r) else { return Ok(false) };
        let source = file.text();
        let mut node = super::iteration::descendant_for_position(
            tree.root_node(),
            source,
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        );
        while let Some(parent) = node.parent() {
            if parent.kind() == "lambda_literal"
                && self
                    .kotlin_lambda_names(parent, source, r)?
                    .contains(&name)
            {
                return Ok(true);
            }
            if matches!(parent.kind(), "function_declaration" | "class_declaration") {
                break;
            }
            node = parent;
        }
        Ok(false)
    }

    /// The declared type of Kotlin receiver `name` at `r`: the nearest local
    /// declaration in the enclosing scope (typed, constructed, or bound to a
    /// call), else a property of the enclosing class, else a parameter, else a
    /// property the enclosing class inherits. `None` when none names a type.
    pub(super) fn kotlin_receiver_declared_type(
        &mut self,
        name: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Res<Option<KotlinDeclared>> {
        let mut local_declared = false;
        if let Some(lines) = self.kotlin_file_lines(&r.file_path) {
            let declares = Self::cached_regex(&format!(r"\b(?:val|var)\s+{}\b", regex::escape(name)))?;
            let scanned = self.kotlin_local_scan_lines(r, lines.len())?;
            for i in scanned {
                let Some(line) = lines.get(i) else { continue };
                if line.len() > 10_000 || !declares.is_match(line) {
                    continue;
                }
                if let Some(local) = self.kotlin_declared_type_in(line, name, r, depth)? {
                    return Ok(Some(local));
                }
                local_declared = true;
                break;
            }
        }
        if let Some(property) = self.infer_java_field_receiver_type(name, r)? {
            return Ok(Some(if property == KOTLIN_EXTERNAL {
                KotlinDeclared::External
            } else {
                KotlinDeclared::Type(property)
            }));
        }
        // A typed parameter of the enclosing function or lambda.
        if let Some(param) = self.infer_local_receiver_type(name, r, false)? {
            return Ok(param
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
                .then_some(KotlinDeclared::Type(param)));
        }
        if local_declared {
            return Ok(None);
        }
        // A property of a superclass: `viewModel` declared in `BaseActivity`.
        let Some(owner) = self.kotlin_enclosing_class(r)? else { return Ok(None) };
        if self.kotlin_own_property_type(name, &owner, r)?.is_some() {
            return Ok(None);
        }
        Ok(self
            .kotlin_inherited_property_type(name, &owner, r)?
            .map(KotlinDeclared::Type))
    }

    /// What `name.member` is when `name` is declared in `text` as a holder of
    /// `kotlin_holder_index`: the type argument its declaration writes.
    fn kotlin_held_type(&mut self, text: &str, name: &str, member: &str) -> Option<String> {
        let m = Self::cached_regex(&format!(
            r"\b{}\s*:\s*(?:[a-z_]\w*\.)*([A-Z]\w*)\s*<",
            regex::escape(name)
        ))
        .ok()?
        .captures(text)?;
        let index = kotlin_holder_index(&m[1], member)?;
        let args = super::declared_member::angle_arguments(&text[m.get(0).unwrap().start()..]);
        Self::kotlin_type_name(args.get(index)?)
    }

    /// The source a Kotlin value `name` of a chain is declared in: `owner`'s
    /// lines for a property, else the enclosing class's and the lines above the
    /// call.
    fn kotlin_declaring_text(&mut self, owner: Option<&Arc<KNode>>, r: &ResolveRefIn) -> String {
        let file = owner.map(|o| o.file_path.clone()).unwrap_or_else(|| r.file_path.clone());
        let Some(lines) = self.kotlin_file_lines(&file) else { return String::new() };
        if let Some(owner) = owner {
            let from = ((owner.start_line - 1).max(0) as usize).min(lines.len());
            let to = (owner.end_line.max(0) as usize).clamp(from, lines.len());
            return lines[from..to].join("\n");
        }
        let around = self.kotlin_enclosing_class(r).ok().flatten();
        let from = around.as_ref().map(|a| ((a.start_line - 1).max(0) as usize).min(lines.len())).unwrap_or(0);
        let to = (around.as_ref().map(|a| a.end_line).unwrap_or(r.line).max(0) as usize).clamp(from, lines.len());
        lines[from..to].join("\n")
    }

    /// The declared type of a Kotlin receiver chain `a.b.c` / `this.a.b`: the
    /// first value typed as a single receiver (`this` is the enclosing class,
    /// a type name its object or companion), each next segment as a property
    /// or enum entry declared in the class of the type before it, read from
    /// that class's own file. `External` as soon as a segment has a type the
    /// project does not declare; `None` when any segment's type is unknown.
    pub(super) fn kotlin_chain_declared_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KotlinDeclared>> {
        let segments: Vec<&str> = receiver.split('.').collect();
        if segments.len() < 2 || segments.len() > 4 {
            return Ok(None);
        }
        let mut owner: Option<Arc<KNode>> = None;
        if segments[0] == "this" {
            owner = self.kotlin_enclosing_class(r)?;
            if owner.is_none() {
                return Ok(None);
            }
        }
        let mut ty: Option<String> = if owner.is_some() {
            None
        } else {
            match self.kotlin_receiver_declared_type(segments[0], r, 0)? {
                Some(KotlinDeclared::Type(t)) => Some(t),
                Some(KotlinDeclared::External) => return Ok(Some(KotlinDeclared::External)),
                None => None,
            }
        };
        if owner.is_none() && ty.is_none() && segments[0].chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            // An object or a class's companion: `Registry.current.load()`,
            // `Mode.ON.next()`.
            let classes: Vec<Arc<KNode>> = self
                .nodes_by_name(segments[0])?
                .iter()
                .filter(|n| declared_type_kind(&n.kind) && n.language == "kotlin")
                .cloned()
                .collect();
            let [only] = classes.as_slice() else { return Ok(None) };
            owner = Some(only.clone());
        }
        let mut holder: Option<Arc<KNode>> = None;
        for i in 1..segments.len() {
            let segment = segments[i];
            if owner.is_none() {
                let Some(t) = ty.clone() else { return Ok(None) };
                if t != KOTLIN_EXTERNAL
                    && kotlin_holder_index(&t, segment).is_some()
                    && !self.is_project_type(&t, r)?
                {
                    // A holder's member is its type argument: `uiState.value`
                    // on a `StateFlow<UiState>` is a `UiState`.
                    let text = self.kotlin_declaring_text(holder.as_ref(), r);
                    ty = match self.kotlin_held_type(&text, segments[i - 1], segment) {
                        Some(t) => Some(t),
                        None => return Ok(Some(KotlinDeclared::External)),
                    };
                    holder = None;
                    continue;
                }
                if t == KOTLIN_EXTERNAL || !self.is_project_type(&t, r)? {
                    return Ok(Some(KotlinDeclared::External));
                }
                let simple = t.rsplit("::").next().unwrap_or(&t).to_string();
                let classes: Vec<Arc<KNode>> = self
                    .nodes_by_name(&simple)?
                    .iter()
                    .filter(|n| {
                        declared_type_kind(&n.kind)
                            && n.language == "kotlin"
                            && (n.qualified_name == t || n.qualified_name.ends_with(&format!("::{t}")))
                    })
                    .cloned()
                    .collect();
                let [only] = classes.as_slice() else { return Ok(None) };
                owner = Some(only.clone());
            }
            let owner_node = owner.clone().unwrap();
            let qualified = format!("{}::{segment}", owner_node.qualified_name);
            let in_file = self.nodes_in_file(&owner_node.file_path)?;
            if in_file.iter().any(|n| n.kind == "enum_member" && n.qualified_name == qualified) {
                // An enum entry has its enum's type (`Outer::Mode`, package
                // dropped).
                ty = Self::kotlin_type_name(&owner_node.qualified_name.replace("::", "."));
                owner = None;
                continue;
            }
            let own = self.kotlin_own_property_type(segment, &owner_node, r)?;
            ty = match own {
                Some(t) => t,
                None => self.kotlin_inherited_property_type(segment, &owner_node, r)?,
            };
            holder = Some(owner_node.clone());
            // A type parameter is no project type: `interface Box<T> { val
            // item: T }` leaves `box.item` untyped rather than reaching a
            // project class that happens to be named `T`.
            if let Some(name) = ty.clone() {
                if !name.contains("::") && self.declared_parameter_bound(&name, &owner_node)?.is_some() {
                    ty = self.declared_parameter_bound(&name, &owner_node)?.flatten();
                }
            }
            owner = None;
        }
        Ok(ty.map(KotlinDeclared::Type))
    }

    /// Whether the project declares a Kotlin extension `method` on a type it
    /// does not declare itself (`fun Context.dp(…)`, indexed as
    /// `Context::dp`).
    pub(super) fn kotlin_declares_library_extension(
        &mut self,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<bool> {
        let names: Vec<Arc<KNode>> = self.nodes_by_name(method)?.iter().cloned().collect();
        for n in names {
            if !matches!(n.kind.as_str(), "method" | "function") || n.language != "kotlin" {
                continue;
            }
            let segs: Vec<&str> = n.qualified_name.split("::").collect();
            let owner = segs.get(segs.len().saturating_sub(2)).copied().unwrap_or("");
            if !owner.is_empty()
                && owner.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && !self.is_project_type(owner, r)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl KernelResolver {
    /// matchKotlinReceiverChain (name-matcher.ts): a Kotlin call through a
    /// receiver chain (`engine.pump.drain()`), resolved on the chain's
    /// declared type. None — no edge — when the chain ends in a type the
    /// project does not declare (or passes through one) and the project has
    /// no extension of that name on a library type. An untyped chain resolves
    /// as the bare method name the extractor emitted before it kept the chain.
    /// `None` (outer) for a ref that is not a receiver chain.
    pub(super) fn match_kotlin_receiver_chain(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<ResolveOutcome>> {
        if r.language != "kotlin" || r.reference_kind != "calls" {
            return Ok(None);
        }
        let Some(m) = re!(r"^((?:this|[A-Za-z_]\w*)(?:\.[A-Za-z_]\w*){1,3})\.(\w+)$")
            .captures(&r.reference_name)
        else {
            return Ok(None);
        };
        let receiver = m[1].to_string();
        let method = m[2].to_string();
        let declared = self.kotlin_chain_declared_type(&receiver, r)?;
        // No declared type at all: the bare method's own name answers the
        // ref, as the extractor emitted before it kept the chain (`app.config
        // .Key(…)` names its qualifier in source).
        let Some(declared) = declared else {
            return Ok(Some(self.resolve_ref(&r.clone().naming(&method, "calls"))?));
        };
        if let KotlinDeclared::Type(declared) = &declared {
            if let Some(c) =
                self.resolve_method_on_type(declared, &method, r, 0.9, "instance-method", None)?
            {
                return Ok(Some(self.finish_pre_framework(r, c)?));
            }
            if self.is_project_type(declared, r)? {
                return Ok(None);
            }
        }
        // A library type still reaches the project's own extensions of
        // library types (`view.context.dp(8)` → `fun Context.dp()`).
        if self.kotlin_declares_library_extension(&method, r)? {
            return Ok(None);
        }
        // The fork's recovered-member rule: a chain that starts on a type the
        // project does not declare (an unread parameter's type) stays a
        // name-only target below the trust line, where no declared type could
        // be read. A chain that starts in project code and passes through a
        // library type is that library object's, and gets no edge.
        let first = receiver.split('.').next().unwrap_or(&receiver);
        let recovered = match self.kotlin_receiver_declared_type(first, r, 0)? {
            Some(KotlinDeclared::Type(first_type)) => !self.is_project_type(&first_type, r)?,
            _ => !first.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
        };
        if recovered {
            // The bare method's own name, answered by the ordinary arms and
            // kept below the trust line: the chain said nothing of its type.
            let mut outcome = self.resolve_ref(&r.clone().naming(&method, "calls"))?;
            if outcome.candidates.is_none() {
                outcome.confidence = outcome.confidence.map(|c| c.min(0.7));
            }
            return Ok(Some(outcome));
        }
        Ok(Some(ResolveOutcome::unresolved()))
    }
}

impl KernelResolver {
    /// `Modifier.pad()` after `import androidx.compose.ui.Modifier`: the type
    /// is outside the project, but Kotlin lets the project extend it. The call
    /// reaches a project extension on that type — `fun Modifier.pad()`,
    /// indexed as `Modifier::pad` — that the call site can see (its file, its
    /// package, an import of it by name or `.*`) and whose own file imports
    /// the same type. Compose's own `Modifier.fillMaxSize()` matches none and
    /// stays unlinked.
    pub(super) fn kotlin_extension_on_imported_type(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if r.language != "kotlin" || r.reference_kind != "calls" {
            return Ok(None);
        }
        let Some(call) = re!(r"^(\w+)\.(\w+)$").captures(&r.reference_name) else {
            return Ok(None);
        };
        let (receiver, method) = (call[1].to_string(), call[2].to_string());
        let Some(binding) = self
            .import_mappings(&r.file_path)?
            .iter()
            .find(|m| m.local_name == receiver)
            .cloned()
        else {
            return Ok(None);
        };
        let pkg = binding.source.rsplit_once('.').map(|(p, _)| p.to_string()).unwrap_or_default();
        let mut extensions: Vec<Arc<KNode>> = Vec::new();
        for n in self
            .nodes_by_qualified_name(&format!("{}::{}", binding.exported_name, method))?
            .iter()
        {
            if n.kind != "method" || n.language != "kotlin" {
                continue;
            }
            if !self.is_kotlin_top_level_visible(n, r)? {
                continue;
            }
            let scope = self.kotlin_file_scope(&n.file_path);
            if scope.imports.contains(&binding.source) || scope.stars.contains(&pkg) {
                extensions.push(n.clone());
            }
        }
        let chosen: Vec<Arc<KNode>> = if extensions.len() > 1 {
            extensions
                .into_iter()
                .filter(|n| n.file_path == r.file_path)
                .collect()
        } else {
            extensions
        };
        Ok(match chosen.as_slice() {
            [only] => Some(KCand {
                node: only.clone(),
                confidence: 0.9,
                resolved_by: "qualified-name",
            }),
            _ => None,
        })
    }
}
