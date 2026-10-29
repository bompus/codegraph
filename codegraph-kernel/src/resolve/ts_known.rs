//! A TS/JS call on a value of a class its file names goes through that class
//! and the classes and interfaces it extends, each found the way its own file
//! names it (tsKnownTypeCall, tsMethodUpChain, tsNamedType and
//! tsOutsideTypeDeclaration of upstream's name-matcher.ts).

use super::fields::TS_BASE_WALK_DEPTH;
use super::*;

/// What a type name written in a file names.
#[derive(Clone)]
pub(super) enum TsNamedType {
    /// The class or interface the file imports or declares under it.
    Node(Arc<KNode>),
    /// A package outside the project, which no project file declares.
    Outside,
    /// Not found.
    Missing,
}

/// The kinds of node a TS/JS type name can name for a member call.
fn is_member_owner_kind(kind: &str) -> bool {
    matches!(kind, "class" | "interface")
}

/// Lines of a class header the type-parameter reader looks through: decorators, then `class C<…>` over several lines.
const TS_CLASS_HEAD_LINES: usize = 24;

impl KernelResolver {
    /// tsTypeDeclaredIn: the class or interface named `name` that `file` declares.
    pub(super) fn ts_type_declared_in(&mut self, file: &str, name: &str) -> Res<Option<Arc<KNode>>> {
        Ok(self.nodes_in_file(file)?.iter().find(|n| n.name == name && is_member_owner_kind(&n.kind)).cloned())
    }

    /// isOutOfRepoImport (index.ts): a package import no indexed file answers.
    pub(super) fn is_out_of_repo_import(&mut self, source: &str, from_file: &str, language: &str) -> Res<bool> {
        if !(self.is_external_import(source, language, from_file) || self.is_alias_prefix(source, from_file)) {
            return Ok(false);
        }
        if self.resolve_import_path(source, from_file, language)?.is_some() {
            return Ok(false);
        }
        Ok(self.is_declared_outside_package(source, from_file))
    }

    /// Does `at`'s file import the head of `written` from a package, not a project file?
    pub(super) fn ts_imports_package(&mut self, written: &str, at: &ResolveRefIn) -> Res<bool> {
        let head = written.split('.').next().unwrap_or("").trim().to_string();
        let Some(binding) = self.import_mappings(&at.file_path)?.iter().find(|m| m.local_name == head).cloned() else { return Ok(false) };
        Ok(self.is_external_import(&binding.source, &at.language, &at.file_path)
            && self.resolve_import_path(&binding.source, &at.file_path, &at.language)?.is_none())
    }

    /// tsImportedTypeFile: the file declaring the type a TS/JS file imports
    /// under the name a declaration in it writes (`store: DisposableStore`,
    /// `new async.Delayer()`), or None when the file doesn't import that name.
    pub(super) fn ts_imported_type_file(&mut self, written: &str, at: &ResolveRefIn) -> Res<Option<String>> {
        let Some(m) = re!(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*").find(written.trim()) else { return Ok(None) };
        let name = m.as_str().to_string();
        let key = (at.file_path.clone(), name.clone());
        if let Some(hit) = self.ts_imported_type_memo.get(&key) {
            return Ok(hit.clone());
        }
        let binding = name.split('.').next().unwrap_or("");
        let mut file = None;
        if self.import_mappings(&at.file_path)?.iter().any(|i| i.local_name == binding) {
            if let Some(hit) = self.resolve_via_import(&at.clone().naming(&name, "references"))? {
                if hit.node.kind != "file" && hit.node.file_path != at.file_path {
                    file = Some(hit.node.file_path.clone());
                }
            }
        }
        self.ts_imported_type_memo.insert(key, file.clone());
        Ok(file)
    }

    /// tsOutsideTypeDeclaration: for a type the file `at` names imports under
    /// the head of `written` from a package outside the project, the project
    /// file that declares that package with a `type_name` in it
    /// (`declare module 'pkg' { … }`, as `.d.ts` files and Flow libdefs do; a
    /// `node:` import is its bare module's). `Some(None)` when none does,
    /// `None` when the type isn't imported from outside the project.
    pub(super) fn ts_outside_type_declaration(&mut self, written: &str, type_name: &str, at: &ResolveRefIn) -> Res<Option<Option<String>>> {
        let Some(head) = re!(r"^[A-Za-z_$][A-Za-z0-9_$]*").find(written.trim()).map(|m| m.as_str().to_string()) else { return Ok(None) };
        let Some(binding) = self.import_mappings(&at.file_path)?.iter().find(|m| m.local_name == head).cloned() else { return Ok(None) };
        if !self.is_out_of_repo_import(&binding.source, &at.file_path, &at.language)? {
            return Ok(None);
        }
        if let Some(node) = self.ts_ambient_lookup(&binding.source, type_name, false, at)? { return Ok(Some(Some(node.file_path.clone()))); }
        let key = (format!("{}\0{}", binding.source, at.file_path), type_name.to_string());
        if let Some(hit) = self.ts_ambient_memo.get(&key) {
            return Ok(Some(hit.clone()));
        }
        let package = regex::escape(binding.source.strip_prefix("node:").unwrap_or(&binding.source));
        let declares = Self::cached_regex(&format!(r#"(?-u:\b)declare\s+module\s+(?:'(?:node:)?{package}'|"(?:node:)?{package}")"#))?;
        let mut file = None;
        for n in self.nodes_by_name(type_name)?.iter() {
            if !same_language_family(&n.language, &at.language) {
                continue;
            }
            if self.read_file(&n.file_path).is_some_and(|text| declares.is_match(text.text())) {
                file = Some(n.file_path.clone());
                break;
            }
        }
        self.ts_ambient_memo.insert(key, file.clone());
        Ok(Some(file))
    }

    /// tsHeadSupertypes: the supertypes a TS/JS class or interface head names,
    /// as written (`Disposable`, `lsp.Emitter`): what it extends (an interface
    /// may extend several), then what it implements. None when the head
    /// extends something that isn't a name, like a mixin call.
    fn ts_head_supertypes(&mut self, cls: &KNode) -> Option<Vec<String>> {
        let source = self.read_file(&cls.file_path)?;
        // Multiline interface heads can exceed the shared class-head scan's
        // eight-line budget; bound this read by the declaration's own extent.
        let head = source.iter().skip((cls.start_line - 1).max(0) as usize)
            .take((cls.end_line - cls.start_line + 1).clamp(1, 80) as usize).cloned().collect::<Vec<_>>().join(" ");
        let mut clean = String::new();
        let mut depth = 0usize;
        let mut previous = ' ';
        for ch in head.chars() {
            match ch {
                '<' => depth += 1,
                '>' if depth > 0 && previous != '=' => depth -= 1,
                '{' if depth == 0 => break,
                _ if depth == 0 => clean.push(ch),
                _ => {},
            }
            previous = ch;
        }
        let head = clean;
        let mut out = Vec::new();
        for (keyword, clause) in [
            ("extends", re!(r"(?-u:\b)extends(?-u:\b)([\s\S]*?)(?:(?-u:\b)(?:extends|implements)(?-u:\b)|$)")),
            ("implements", re!(r"(?-u:\b)implements(?-u:\b)([\s\S]*?)(?:(?-u:\b)(?:extends|implements)(?-u:\b)|$)")),
        ] {
            let Some(clause) = clause.captures(&head).map(|m| m[1].to_string()) else { continue };
            for part in clause.split(',') {
                let name = part.trim();
                if !re!(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*$").is_match(name) {
                    if keyword == "extends" {
                        return None;
                    }
                    continue;
                }
                out.push(name.to_string());
            }
        }
        Some(out)
    }

    /// tsNamedType: the class or interface `written` names in `from`'s file:
    /// the one an import leads to (aliases, re-exports, a namespace import's
    /// member), or one the file declares. `Outside` when the file imports it
    /// from a package outside the project that no project file declares.
    pub(super) fn ts_named_type(&mut self, written: &str, from: &KNode, at: &ResolveRefIn) -> Res<TsNamedType> {
        // The name is written in `from`'s head, so a parameter or local that
        // shadows it is judged at that line, not at the call's.
        let mut at = at.clone();
        at.line = from.start_line;
        at.column = from.start_column;
        self.ts_named_type_in(written, &from.file_path, &from.language, &from.id, &at)
    }

    /// ts_named_type for a name written in `file`, which `from_id` (a node
    /// there, or empty) is not itself.
    fn ts_named_type_in(&mut self, written: &str, file: &str, language: &str, from_id: &str, at: &ResolveRefIn) -> Res<TsNamedType> {
        let head = written.split('.').next().unwrap_or("");
        let mapping = self.import_mappings(file)?.iter().find(|i| i.local_name == head).cloned();
        // A name the file declares is looked up past `from_id` itself.
        let key = (file.to_string(), if mapping.is_some() { format!("{written}\0@{}", at.line) } else { format!("{written}\0{from_id}") });
        if let Some(hit) = self.ts_named_type_memo.get(&key) {
            return Ok(hit.clone());
        }
        let last = written.rsplit('.').next().unwrap_or(written).to_string();
        let mut site = at.clone();
        site.file_path = file.to_string();
        site.language = language.to_string();
        let found = if let Some(mapping) = mapping {
            if self.is_out_of_repo_import(&mapping.source, file, language)? {
                match self.ts_outside_type_declaration(written, &last, &site)? {
                    Some(None) => TsNamedType::Outside,
                    None => TsNamedType::Missing,
                    Some(Some(file)) => self.ts_type_declared_in(&file, &last)?.map_or(TsNamedType::Missing, TsNamedType::Node),
                }
            } else {
                match self.resolve_via_import(&site.naming(written, "references"))? {
                    None => TsNamedType::Missing,
                    // vscode's `ILogService` is also a constant, its
                    // dependency-injection decorator, which the import may
                    // name first: the type is the interface of that name
                    // beside it.
                    Some(hit) if is_member_owner_kind(&hit.node.kind) => TsNamedType::Node(hit.node),
                    Some(hit) => self.ts_type_declared_in(&hit.node.file_path, &hit.node.name)?.map_or(TsNamedType::Missing, TsNamedType::Node),
                }
            }
        } else if !written.contains('.') {
            self.nodes_in_file(file)?
                .iter()
                .find(|n| n.id != from_id && n.name == written && is_member_owner_kind(&n.kind))
                .cloned()
                .map_or(TsNamedType::Missing, TsNamedType::Node)
        } else {
            TsNamedType::Missing
        };
        self.ts_named_type_memo.insert(key, found.clone());
        Ok(found)
    }

    /// tsMethodUpChain: the method `name` of the TS/JS class or interface
    /// `cls`: its own, or else the nearest supertype's up its head's
    /// `extends` (then `implements`), each found the way its subtype's file
    /// names it. `complete` when every supertype was found in the project or
    /// is a package's class, so a missing method has no project declaration;
    /// a supertype that can't be found, or a property of that name (a
    /// function-typed field), leaves the answer incomplete.
    fn ts_method_up_chain(&mut self, cls: &Arc<KNode>, name: &str, at: &ResolveRefIn, depth: usize) -> Res<(Option<Arc<KNode>>, bool)> {
        let members: Vec<Arc<KNode>> = self
            .nodes_in_file(&cls.file_path)?
            .iter()
            .filter(|n| {
                n.name == name
                    && n.start_line >= cls.start_line
                    && n.end_line <= cls.end_line
                    && n.qualified_name[..n.qualified_name.rfind("::").unwrap_or(0)].rsplit(['.', ':']).next() == Some(cls.name.as_str())
            })
            .cloned()
            .collect();
        if let Some(own) = members.iter().find(|n| n.kind == "method") {
            return Ok((Some(own.clone()), true));
        }
        if let Some(scan) = self.ts_scope_scan(&cls.file_path) {
            if let Some(block) = scan.blocks.iter().find(|b| b.ambient && b.contains(cls.start_line, cls.start_column)) {
                let module = block.name.clone();
                for candidate in self.nodes_by_name(&cls.name)?.iter().filter(|n| n.id != cls.id && is_member_owner_kind(&n.kind)).cloned().collect::<Vec<_>>() {
                    if !self.ts_scope_scan(&candidate.file_path).is_some_and(|scan| scan.blocks.iter().any(|b| b.ambient && b.name == module && b.contains(candidate.start_line, candidate.start_column))) { continue; }
                    if let Some(node) = self.nodes_in_file(&candidate.file_path)?.iter().find(|n| n.name == name && n.kind == "method" && n.start_line >= candidate.start_line && n.end_line <= candidate.end_line).cloned() {
                        return Ok((Some(node), true));
                    }
                }
            }
        }
        // A property of that name is what the call calls, but no method to
        // link, so the answer is left to the lookups that follow.
        if members.iter().any(|n| matches!(n.kind.as_str(), "property" | "field")) || depth >= TS_BASE_WALK_DEPTH {
            return Ok((None, false));
        }
        let Some(supers) = self.ts_head_supertypes(cls) else { return Ok((None, false)) };
        let mut complete = true;
        for written in supers {
            match self.ts_named_type(&written, cls, at)? {
                TsNamedType::Outside => continue,
                TsNamedType::Missing => complete = false,
                TsNamedType::Node(base) if base.id == cls.id => complete = false,
                TsNamedType::Node(base) => {
                    let (method, up_complete) = self.ts_method_up_chain(&base, name, at, depth + 1)?;
                    if method.is_some() {
                        return Ok((method, up_complete));
                    }
                    complete &= up_complete;
                }
            }
        }
        Ok((None, complete))
    }

    /// tsKnownTypeCall: a TS/JS member call on a value of the class or
    /// interface `type_name` that `file` declares: that type's method or its
    /// nearest supertype's; `Some(None)` when the whole chain is known and has
    /// none, as when the method is a package's. `None` when `file` doesn't
    /// (nor what `at`'s file imports under that name) declare the type or a
    /// supertype can't be found, for the caller's own fallback.
    pub(super) fn ts_known_type_call(
        &mut self,
        type_name: &str,
        file: &str,
        method: &str,
        at: &ResolveRefIn,
        confidence: f64,
    ) -> Res<Option<Option<KCand>>> {
        let known = match self.ts_type_declared_in(file, type_name)? {
            Some(known) => known,
            // An import may rename it: `export { MessageSender as MessagingService }`.
            None => match self.ts_named_type_in(type_name, &at.file_path, &at.language, "", at)? {
                TsNamedType::Node(named) => named,
                _ => return Ok(None),
            },
        };
        let (found, complete) = self.ts_method_up_chain(&known, method, at, 0)?;
        Ok(match found {
            Some(node) => Some(Some(KCand { node, confidence, resolved_by: "instance-method" })),
            None if complete => Some(None),
            None => None,
        })
    }
}

/// What a TS annotation with brackets after the type settles for a call.
pub(super) enum TsBracketed {
    /// The call is settled: the array's own method links nothing, or the
    /// indexed property's type decides alone.
    Settled(Option<KCand>),
    /// The type to look the method up on.
    Type(String),
}

impl KernelResolver {
    /// A TS annotation with brackets after the type. An array of it (`items:
    /// Item[]`): a method arrays have is the array's, which no project class
    /// declares; any other is called on one of its elements under the same
    /// name (`d.forEach(d => d.dispose())`), so it takes the element's type.
    /// One of a type's properties (`editor: IChatWidget['inputEditor']`) has
    /// the type that property is declared with, an array of it the same way;
    /// that type decides alone. A property whose type isn't written there (a
    /// getter, `typeof`), or a key that isn't a name (`Parameters<typeof f>[0]`),
    /// says nothing new, and is read as the type it indexes.
    pub(super) fn ts_bracketed_type(&mut self, ty: &str, method: &str, r: &ResolveRefIn) -> Res<TsBracketed> {
        let element = re!(r"^(.+?)\s*\[\s*\]$").captures(ty).map(|m| m[1].to_string());
        let declared = match &element {
            Some(element) => Some((element.clone(), true, None)),
            None => self.ts_indexed_property_type(ty, r)?,
        };
        let Some((declared_type, array, file)) = declared else {
            return Ok(TsBracketed::Type(re!(r"\s*\[[^\]]*\]$").replace(ty, "").into_owned()));
        };
        if array && JS_ARRAY_METHODS.contains(method) {
            return Ok(TsBracketed::Settled(None));
        }
        let Some(name) = self.normalize_inferred_type_name(&declared_type)? else { return Ok(TsBracketed::Settled(None)) };
        if element.is_some() {
            // A method arrays don't have, called on the array's own name, is
            // a call on an element only inside a callback whose parameter
            // takes that name (`d.forEach(d => d.dispose())`). Anywhere else
            // the array is the receiver, which has no such method.
            if !self.ts_callback_shadows_receiver(r) {
                return Ok(TsBracketed::Settled(None));
            }
            return Ok(TsBracketed::Type(name));
        }
        Ok(TsBracketed::Settled(self.resolve_method_on_type(&name, method, r, 0.9, "instance-method", file.as_deref())?))
    }

    /// Does a callback written within a few lines before the call, or on its
    /// line, take the call's receiver name as a parameter (`d => d.dispose()`,
    /// `(d, i) => …`, `function (d) { … }`)? A declaration's
    /// own parameter list is no callback.
    fn ts_callback_shadows_receiver(&mut self, r: &ResolveRefIn) -> bool {
        let receiver = r.reference_name.split('.').next().unwrap_or("");
        if receiver.is_empty() || !re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(receiver) {
            return false;
        }
        let Some(file) = self.read_file(&r.file_path) else { return false };
        let call = (r.line.max(1) - 1) as usize;
        let window = file.text().lines().skip(call.saturating_sub(4)).take(call.min(4) + 1).collect::<Vec<_>>().join("\n");
        let name = regex::escape(receiver);
        let pattern = format!(
            r"(?:(?-u:\b){name}\s*=>|\([^()]*(?-u:\b){name}(?-u:\b)[^()]*\)\s*(?::[^=)]*)?=>|(?-u:\b)function\s*\([^()]*(?-u:\b){name}(?-u:\b))"
        );
        Self::cached_regex(&pattern).is_ok_and(|re| re.is_match(&window))
    }

    /// tsIndexedPropertyType: what a TS indexed type (`editor:
    /// IChatWidget['inputEditor']`) is: the type that property is declared
    /// with in the project class or interface it indexes, whether the property
    /// holds an array of it, and the file that says which class of that name it
    /// is. The indexed type is the one the call's file imports, when it
    /// imports one; the property's type is the one the declaring file imports,
    /// or else declares. None when the key isn't a string literal, the indexed
    /// type isn't the project's, or the property's type isn't written there.
    fn ts_indexed_property_type(&mut self, indexed: &str, r: &ResolveRefIn) -> Res<Option<(String, bool, Option<String>)>> {
        let Some(m) = re!(r#"^([A-Za-z_$][A-Za-z0-9_$]*)\s*\[\s*(?:'([A-Za-z0-9_$]+)'|"([A-Za-z0-9_$]+)")\s*\]$"#).captures(indexed) else { return Ok(None) };
        let (name, key) = (m[1].to_string(), m.get(2).or_else(|| m.get(3)).map(|k| k.as_str().to_string()).unwrap_or_default());
        let imported = self.ts_imported_type_file(&name, r)?;
        let owners = prefer_call_site_file(
            self.nodes_by_name(&name)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "class" | "interface" | "type_alias" | "component") && same_language_family(&n.language, &r.language)
                })
                .cloned()
                .collect(),
            imported.as_deref().unwrap_or(&r.file_path),
        );
        for owner in owners {
            let Some(decl) = self.ts_field_decl(&owner, &key)? else { continue };
            // A type parameter of the class names no type: `Emitter<T>`'s
            // `fire(event: T)`, which the reader takes for its `event`.
            if decl.value_type || self.ts_class_type_parameter(&owner, &decl.ty) {
                return Ok(None);
            }
            let mut at = r.clone();
            at.file_path = owner.file_path.clone();
            at.language = owner.language.clone();
            at.line = owner.start_line;
            at.column = owner.start_column;
            let file = self.ts_imported_type_file(&decl.ty, &at)?.unwrap_or_else(|| owner.file_path.clone());
            return Ok(Some((decl.ty, decl.array, Some(file))));
        }
        Ok(None)
    }

    /// Is `name` one of the type parameters the head of the TS class,
    /// interface or type alias `cls` declares (`class Emitter<T>`,
    /// `type State<T> = …`)? The list is read from the `class` / `interface` /
    /// `type` keyword to its own `>`, over several lines and through
    /// constraints and defaults that hold braces (`<T extends { id: string }>`).
    /// A list whose `>` is not found reads as declaring `name`: a caller drops
    /// the type rather than take a parameter for a project class.
    pub(super) fn ts_class_type_parameter(&mut self, cls: &KNode, name: &str) -> bool {
        let Some(source) = self.read_file(&cls.file_path) else { return false };
        let from = (cls.start_line - 1).max(0) as usize;
        let head = source.get(from..(from + TS_CLASS_HEAD_LINES).min(source.len())).unwrap_or_default().join("\n");
        // No `{`-cut: `class Shaped<Item extends { id: string }>` carries a
        // brace inside its parameter list; the `>` scan below is depth-aware.
        let head = head.as_str();
        let Ok(open) = Self::cached_regex(&format!(r"(?-u:\b)(?:class|interface|type)\s+{}\s*<", regex::escape(&cls.name))) else { return false };
        let Some(open) = open.find(head) else { return false };
        // The parameters between the `<` after the name and its own `>`, split
        // at their top level: `T`, `K extends keyof T = never`.
        let first = |param: &str| -> bool {
            let p = param.trim();
            let p = ["in ", "out ", "const "].iter().find_map(|k| p.strip_prefix(k)).map_or(p, str::trim_start);
            p.split(|c: char| c.is_whitespace() || c == '=').next() == Some(name)
        };
        let bytes = head.as_bytes();
        let mut depth = 0i32;
        let mut from = open.end();
        for i in open.end()..bytes.len() {
            match bytes[i] {
                b'<' | b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'>' if bytes[i - 1] != b'=' => {
                    if depth == 0 {
                        return first(&head[from..i]);
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => {
                    if first(&head[from..i]) {
                        return true;
                    }
                    from = i + 1;
                }
                _ => {}
            }
        }
        true
    }
}

impl KernelResolver {
    /// localTypeBinding (import-resolver.ts): for an unresolved direct member
    /// call on a local the file declares with a type it imports (`const cache
    /// = new LRUCache(10)`, `cache: LRUCache<K, V>`, a typed parameter, then
    /// `cache.get(k)`), the head of that type as written. Resolution reads the
    /// declaration the same way (infer_local_receiver_type), so a sync can park
    /// the call under the module its import names. None for a method arrays
    /// have called on an array (`results.push(…)`): resolution links that to
    /// no project class, so no edit of the type's module does.
    pub(super) fn local_type_head(&mut self, r: &ResolveRefIn) -> Res<Option<String>> {
        if r.reference_kind != "calls" || !is_esm_import_language(&r.language) {
            return Ok(None);
        }
        let Some(m) = re!(r"^([A-Za-z_$][A-Za-z0-9_$]*)\.([A-Za-z_$][A-Za-z0-9_$]*)$").captures(&r.reference_name) else { return Ok(None) };
        let (receiver, member) = (m[1].to_string(), m[2].to_string());
        if self.import_mappings(&r.file_path)?.is_empty() {
            return Ok(None);
        }
        let Some(ty) = self.infer_local_receiver_type(&receiver, r, false)? else { return Ok(None) };
        let Some(raw) = self.infer_local_receiver_type(&receiver, r, true)? else { return Ok(None) };
        if re!(r"^(.+?)\s*\[\s*\]$").is_match(&ty) && JS_ARRAY_METHODS.contains(member.as_str()) {
            return Ok(None);
        }
        Ok(raw.split(|c: char| c == '.' || c == '<' || c == '[' || c.is_whitespace()).next().filter(|h| !h.is_empty()).map(str::to_string))
    }
}

#[derive(Clone)]
pub(super) struct TsScopeBlock {
    name: String,
    start: (i64, i64),
    end: (i64, i64),
    byte_start: usize,
    byte_end: usize,
    pub(super) ambient: bool,
    global: bool,
    text: String,
    /// An ambient block's `export default X` / `export = X` / `export {Y as default}` name.
    default_name: Option<String>,
    /// An ambient block's `export { … }` clause names: exported name → local name.
    export_aliases: Vec<(String, String)>,
    /// The block has an `export { … }` clause (empty or not).
    export_clause: bool,
}
impl TsScopeBlock {
    pub(super) fn contains(&self, line: i64, column: i64) -> bool {
        self.start <= (line, column) && (line, column) <= self.end
    }
}

pub(super) struct TsScopeScan {
    module_file: bool,
    pub(super) blocks: Vec<TsScopeBlock>,
}

// ---- Tokenizer and scope scanners: ports of upstream ambient-modules.ts
// (scanAmbientModules, scanGlobalScope's script flag and scanNamespaceBlocks).
// They read tokens, never a tree, so a scope scan costs a text pass instead of
// a tree-sitter parse.

#[derive(Clone, Copy, PartialEq)]
enum TsTok { Word, Str, Tpl, Punct }

fn ts_word_start(c: u8) -> bool { c.is_ascii_uppercase() || c.is_ascii_lowercase() || c == b'_' || c == b'$' || c > 127 }
fn ts_word_char(c: u8) -> bool { ts_word_start(c) || c.is_ascii_digit() }

fn ts_regex_after(word: &str) -> bool {
    matches!(word, "return" | "typeof" | "case" | "do" | "else" | "in" | "of" | "instanceof" | "new" | "delete" | "void" | "throw" | "yield" | "await")
}

/// Words that start an `export default` declaration rather than name its value.
fn ts_default_word(word: &str) -> bool {
    matches!(word, "class" | "function" | "interface" | "abstract" | "async" | "enum")
}

/// Read a template literal's text from `i` up to its closing backtick or a `${`.
fn ts_template_text(b: &[u8], i: &mut usize, line: &mut i64, line_start: &mut usize, depth: u32, templates: &mut Vec<u32>) {
    let n = b.len();
    while *i < n {
        let c = b[*i];
        if c == b'\\' {
            if *i + 1 < n && b[*i + 1] == b'\n' { *line += 1; *line_start = *i + 2; }
            *i += 2;
            continue;
        }
        if c == b'`' { *i += 1; return; }
        if c == b'$' && *i + 1 < n && b[*i + 1] == b'{' { templates.push(depth); *i += 2; return; }
        if c == b'\n' { *line += 1; *line_start = *i + 1; }
        *i += 1;
    }
}

/// Feed `source`'s tokens to `visit`: kind, text, 1-based line, 0-based byte
/// column, byte offset and the brace depth outside the token — both braces of
/// a block carry the depth the block sits at.
fn ts_tokenize(source: &str, visit: &mut impl FnMut(TsTok, &str, i64, i64, usize, u32)) {
    let b = source.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut line = 1i64;
    let mut line_start = 0usize;
    let mut depth = 0u32;
    // The brace depth at each open `${`: the `}` that brings depth back to it ends the expression.
    let mut templates: Vec<u32> = Vec::new();
    // Whether a `/` here would start a regex: after an operator, a keyword or nothing.
    let mut regex_allowed = true;

    while i < n {
        let c = b[i];
        if c == b'\n' { line += 1; line_start = i + 1; i += 1; continue; }
        if c == b' ' || c == b'\t' || c == b'\r' { i += 1; continue; }
        if c == 0xef && i + 2 < n && b[i + 1] == 0xbb && b[i + 2] == 0xbf { i += 3; continue; }
        if c == 0xc2 && i + 1 < n && b[i + 1] == 0xa0 { i += 2; continue; }
        let column = i - line_start;
        if c == b'/' {
            let next = b.get(i + 1).copied().unwrap_or(0);
            if next == b'/' {
                while i < n && b[i] != b'\n' { i += 1; }
                continue;
            }
            if next == b'*' {
                i += 2;
                while i < n && !(b[i] == b'*' && i + 1 < n && b[i + 1] == b'/') {
                    if b[i] == b'\n' { line += 1; line_start = i + 1; }
                    i += 1;
                }
                i += 2;
                continue;
            }
            if regex_allowed {
                // A regex literal ends at an unescaped `/` outside a character class, or at the line's end.
                i += 1;
                let mut in_class = false;
                while i < n {
                    let r = b[i];
                    if r == b'\n' { break; }
                    if r == b'\\' { i += 1; }
                    else if r == b'[' { in_class = true; }
                    else if r == b']' { in_class = false; }
                    else if r == b'/' && !in_class { i += 1; break; }
                    i += 1;
                }
                while i < n && ts_word_char(b[i]) { i += 1; } // flags
                regex_allowed = false;
                visit(TsTok::Tpl, "", line, column as i64, i, depth);
                continue;
            }
        }
        if c == b'\'' || c == b'"' {
            let mut j = i + 1;
            let start = j;
            let mut text = String::new();
            let mut from = start;
            while j < n {
                let s = b[j];
                if s == c || s == b'\n' { break; }
                if s == b'\\' {
                    text.push_str(&source[from..j]);
                    j += 1;
                    if j < n {
                        if b[j] == b'\n' { line += 1; line_start = j + 1; }
                        j += 1;
                        while j < n && (b[j] & 0xC0) == 0x80 { j += 1; }
                    }
                    from = j;
                    continue;
                }
                j += 1;
            }
            text.push_str(&source[from..j.min(n)]);
            i = if j < n && b[j] == c { j + 1 } else { j };
            regex_allowed = false;
            visit(TsTok::Str, &text, line, column as i64, i, depth);
            continue;
        }
        if c == b'`' {
            i += 1;
            ts_template_text(b, &mut i, &mut line, &mut line_start, depth, &mut templates);
            regex_allowed = false;
            visit(TsTok::Tpl, "", line, column as i64, i, depth);
            continue;
        }
        if c == b'{' {
            visit(TsTok::Punct, "{", line, column as i64, i, depth);
            depth += 1;
            i += 1;
            regex_allowed = true;
            continue;
        }
        if c == b'}' {
            i += 1;
            if templates.last() == Some(&depth) {
                templates.pop();
                ts_template_text(b, &mut i, &mut line, &mut line_start, depth, &mut templates);
                regex_allowed = false;
                continue;
            }
            depth = depth.saturating_sub(1);
            visit(TsTok::Punct, "}", line, column as i64, i - 1, depth);
            regex_allowed = true;
            continue;
        }
        if ts_word_start(c) {
            let mut j = i + 1;
            while j < n && ts_word_char(b[j]) { j += 1; }
            let word = &source[i..j];
            i = j;
            regex_allowed = ts_regex_after(word);
            visit(TsTok::Word, word, line, column as i64, i, depth);
            continue;
        }
        if c.is_ascii_digit() {
            while i < n && (ts_word_char(b[i]) || b[i] == b'.') { i += 1; }
            regex_allowed = false;
            continue;
        }
        i += 1;
        regex_allowed = c != b')' && c != b']';
        visit(TsTok::Punct, &source[i - 1..i], line, column as i64, i - 1, depth);
    }
}

struct TsAmbientBlock {
    name: String,
    start: (i64, i64),
    end: (i64, i64),
    byte_start: usize,
    byte_end: usize,
    default_name: Option<String>,
    export_aliases: Vec<(String, String)>,
    export_clause: bool,
}

/// Every `declare module '<name>' { … }` block in `source`, and whether the
/// file is a script (no top-level `import` or `export`). Port of upstream's
/// `scanAmbientModules`; a block is kept at the depth it opened at so one
/// nested inside `declare global` still lands.
fn ts_scan_ambient(source: &str) -> (bool, Vec<TsAmbientBlock>) {
    let mut blocks: Vec<TsAmbientBlock> = Vec::new();
    let mut script = true;
    // The statement being read at the top level.
    let mut declare = 0u8; // 1: `declare`, 2: `declare module`, 3: `declare module '<name>'`
    let mut declare_pos = (0i64, 0i64, 0usize);
    let mut declare_name = String::new();
    // After a top-level `import`: the tokens seen so far, to tell `import x = N.y`
    // (an alias of a namespace, legal in a script) from every other form.
    let mut import_tokens: Option<Vec<(TsTok, String)>> = None;
    // The block being read, and the export statement being read inside it.
    let mut open: Option<(TsAmbientBlock, u32)> = None;
    let mut export_state = 0u8; // 1: export, 2: export default, 3: export default <kw>, 4: export =, 5: export {, 6: export type
    let mut clause: Vec<String> = Vec::new();

    let close_clause = |open: &mut TsAmbientBlock, clause: &mut Vec<String>| {
        let mut i = 0;
        while i < clause.len() {
            let local = clause[i].clone();
            if clause.get(i + 1).map(String::as_str) == Some("as") && clause.get(i + 2).is_some() {
                let exported = clause[i + 2].clone();
                if exported == "default" { open.default_name.get_or_insert(local); }
                else { open.export_aliases.push((exported, local)); }
                i += 3;
            } else {
                open.export_aliases.push((local.clone(), local));
                i += 1;
            }
        }
        clause.clear();
    };

    ts_tokenize(source, &mut |kind, text, line, column, offset, depth| {
        if let Some(t) = &mut import_tokens {
            t.push((kind, text.to_string()));
            let settle = t.len() >= 3 || t[0].0 != TsTok::Word || (t.len() == 2 && t[1].1 != "=");
            if settle {
                // `import(…)` and `import.meta` are expressions; `import x = N.y` aliases a namespace.
                let is_alias = t[0].0 == TsTok::Punct && (t[0].1 == "(" || t[0].1 == ".")
                    || t[0].0 == TsTok::Word && t.get(1).map(|x| x.1.as_str()) == Some("=") && t.get(2).map(|x| x.0) == Some(TsTok::Word) && t[2].1 != "require";
                if !is_alias { script = false; }
                import_tokens = None;
            }
        }
        if let Some((block, open_depth)) = &mut open {
            if kind == TsTok::Punct && text == "}" && depth == *open_depth {
                block.end = (line, column);
                block.byte_end = offset + 1;
                blocks.push(std::mem::replace(block, TsAmbientBlock {
                    name: String::new(), start: (0, 0), end: (0, 0), byte_start: 0, byte_end: 0,
                    default_name: None, export_aliases: Vec::new(), export_clause: false,
                }));
                open = None;
                export_state = 0;
                clause.clear();
                return;
            }
            // The export statement being read inside the block's top level.
            if export_state == 5 {
                if kind == TsTok::Punct && text == "}" && depth == *open_depth + 1 {
                    close_clause(block, &mut clause);
                    export_state = 0;
                } else if kind == TsTok::Word {
                    clause.push(text.to_string());
                }
                return;
            }
            if depth == *open_depth + 1 {
                match export_state {
                    0 => if kind == TsTok::Word && text == "export" { export_state = 1; },
                    1 => {
                        if kind == TsTok::Word && text == "default" { export_state = 2; }
                        else if kind == TsTok::Punct && text == "=" { export_state = 4; }
                        else if kind == TsTok::Punct && text == "{" { export_state = 5; block.export_clause = true; }
                        else if kind == TsTok::Word && text == "type" { export_state = 6; }
                        else { export_state = 0; }
                    }
                    2 => {
                        if kind == TsTok::Word && ts_default_word(text) { export_state = 3; }
                        else {
                            if kind == TsTok::Word { block.default_name.get_or_insert(text.to_string()); }
                            export_state = 0;
                        }
                    }
                    3 => {
                        if (kind == TsTok::Word && ts_default_word(text)) || (kind == TsTok::Punct && text == "*") { return; }
                        if kind == TsTok::Word { block.default_name.get_or_insert(text.to_string()); }
                        export_state = 0;
                    }
                    4 => {
                        if kind == TsTok::Word { block.default_name.get_or_insert(text.to_string()); }
                        export_state = 0;
                    }
                    6 => {
                        if kind == TsTok::Punct && text == "{" { export_state = 5; block.export_clause = true; }
                        else { export_state = 0; }
                    }
                    _ => {}
                }
            }
            return;
        }
        if depth != 0 {
            declare = 0;
            return;
        }
        if kind == TsTok::Word {
            if text == "import" {
                import_tokens = Some(Vec::new());
                declare = 0;
            } else if text == "export" {
                script = false;
                declare = 0;
            } else if text == "declare" {
                declare = 1;
                declare_pos = (line, column, offset);
            } else {
                declare = if declare == 1 && text == "module" { 2 } else { 0 };
            }
            return;
        }
        if kind == TsTok::Str && declare == 2 {
            declare = 3;
            declare_name = text.to_string();
            return;
        }
        if kind == TsTok::Punct && text == "{" && declare == 3 && !declare_name.is_empty() {
            open = Some((TsAmbientBlock {
                name: std::mem::take(&mut declare_name),
                start: (declare_pos.0, declare_pos.1), end: (0, 0),
                byte_start: declare_pos.2, byte_end: 0,
                default_name: None, export_aliases: Vec::new(), export_clause: false,
            }, depth));
        }
        declare = 0;
    });
    if let Some(t) = &import_tokens {
        let is_alias = t[0].0 == TsTok::Punct && (t[0].1 == "(" || t[0].1 == ".")
            || t[0].0 == TsTok::Word && t.get(1).map(|x| x.1.as_str()) == Some("=") && t.get(2).map(|x| x.0) == Some(TsTok::Word) && t[2].1 != "require";
        if !is_alias { script = false; }
    }
    (script, blocks)
}

enum TsNsFrame { Global(usize), Module, Ns(usize), Other }

/// Every namespace block (old-spelling `module A` included) and every
/// `declare global` block in `source`, with the global flag upstream gives
/// it: directly inside `declare global`, at a declaration script's top level
/// (`top_level_global`), or inside another global one. Port of upstream's
/// `scanNamespaceBlocks`; namespaces inside a `declare module 'x'` body are
/// module `x`'s, never global, and named as if at the file's top level.
fn ts_scan_namespaces(source: &str, top_level_global: bool, out: &mut Vec<TsScopeBlock>) {
    let mut open: Vec<TsNsFrame> = Vec::new();
    let mut names: Option<Vec<String>> = None; // the names after `namespace`/`module`, while its header is read
    let mut expect_name = false;
    let mut keyword = (0i64, 0i64, 0usize);
    let mut declare = 0u8; // 1: `declare`, 2: `declare global`
    let mut declare_kw = (0i64, 0i64, 0usize); // the `declare` keyword's position
    let mut ambient_module = false; // just read `module 'x'`
    let mut after_dot = false;

    ts_tokenize(source, &mut |kind, text, line, column, offset, _depth| {
        if kind == TsTok::Punct && text == "{" {
            let header = names.as_ref().filter(|n| !n.is_empty() && !expect_name).cloned();
            if declare == 2 {
                let idx = out.len();
                out.push(TsScopeBlock {
                    name: "global".to_string(), ambient: false, global: true,
                    start: (declare_kw.0, declare_kw.1), end: (0, 0),
                    byte_start: declare_kw.2, byte_end: 0, text: String::new(),
                    default_name: None, export_aliases: Vec::new(), export_clause: false,
                });
                open.push(TsNsFrame::Global(idx));
            } else if ambient_module {
                open.push(TsNsFrame::Module);
            } else if let Some(header) = header.filter(|_| !matches!(open.last(), Some(TsNsFrame::Other))) {
                let parent_path = match open.last() {
                    Some(TsNsFrame::Ns(i)) => Some(out[*i].name.clone()),
                    _ => None,
                };
                let idx = out.len();
                out.push(TsScopeBlock {
                    name: parent_path.map(|p| format!("{p}.")).unwrap_or_default() + &header.join("."),
                    ambient: false,
                    global: match open.last() {
                        Some(TsNsFrame::Ns(i)) => out[*i].global,
                        Some(TsNsFrame::Global(_)) => true,
                        None => top_level_global,
                        _ => false,
                    },
                    start: (keyword.0, keyword.1), end: (0, 0),
                    byte_start: keyword.2, byte_end: 0, text: String::new(),
                    default_name: None, export_aliases: Vec::new(), export_clause: false,
                });
                open.push(TsNsFrame::Ns(idx));
            } else {
                open.push(TsNsFrame::Other);
            }
            names = None;
            declare = 0;
            ambient_module = false;
            after_dot = false;
            return;
        }
        if kind == TsTok::Punct && text == "}" {
            match open.pop() {
                Some(TsNsFrame::Ns(i)) | Some(TsNsFrame::Global(i)) => {
                    out[i].end = (line, column);
                    out[i].byte_end = offset + 1;
                    out[i].text = source[out[i].byte_start..(offset + 1).min(source.len())].to_string();
                }
                _ => {}
            }
            names = None;
            declare = 0;
            ambient_module = false;
            after_dot = false;
            return;
        }
        ambient_module = names.as_ref().is_some_and(|n| expect_name && n.is_empty() && kind == TsTok::Str);
        if let Some(n) = &mut names {
            if expect_name && kind == TsTok::Word {
                n.push(text.to_string());
                expect_name = false;
                return;
            }
            if !expect_name && kind == TsTok::Punct && text == "." {
                expect_name = true;
                return;
            }
            names = None;
        }
        if kind == TsTok::Word {
            if (text == "namespace" || text == "module") && !after_dot {
                names = Some(Vec::new());
                expect_name = true;
                keyword = (line, column, offset);
            }
            if text == "declare" { declare_kw = (line, column, offset); }
            declare = if text == "declare" { 1 } else if declare == 1 && text == "global" { 2 } else { 0 };
            after_dot = false;
            return;
        }
        declare = 0;
        after_dot = kind == TsTok::Punct && text == ".";
    });
}

impl KernelResolver {
    pub(super) fn ts_scope_scan(&mut self, file: &str) -> Option<Rc<TsScopeScan>> {
        if let Some(memo) = self.ts_scope_memo.get(file) {
            return memo.clone();
        }
        let scan = self.ts_scope_scan_uncached(file).map(Rc::new);
        self.ts_scope_memo.insert(file.to_string(), scan.clone());
        scan
    }

    /// Port of upstream ambient-modules.ts: reads tokens, never a tree. A
    /// `declare module '<name>' { … }` in a script declaration file declares
    /// the module; in a module file it augments one.
    fn ts_scope_scan_uncached(&mut self, file: &str) -> Option<TsScopeScan> {
        let source = self.read_file(file)?;
        let text = source.text();
        // Nothing the scan can find: not a module file, no blocks to name.
        if !re!(r"\b(?:declare|namespace|module|import|export)\b").is_match(text) {
            return Some(TsScopeScan { module_file: false, blocks: Vec::new() });
        }
        let (script, ambient) = ts_scan_ambient(text);
        let top_level_global = re!(r"\.d\.[cm]?ts$").is_match(file) && script;
        let mut blocks: Vec<TsScopeBlock> = Vec::new();
        for b in ambient {
            blocks.push(TsScopeBlock {
                name: b.name, ambient: true, global: false,
                start: b.start, end: b.end,
                byte_start: b.byte_start, byte_end: b.byte_end,
                text: text[b.byte_start..b.byte_end.min(text.len())].to_string(),
                default_name: b.default_name, export_aliases: b.export_aliases, export_clause: b.export_clause,
            });
        }
        ts_scan_namespaces(text, top_level_global, &mut blocks);
        blocks.sort_by_key(|b| b.start);
        Some(TsScopeScan { module_file: !script, blocks })
    }

    fn ts_scope_candidate(&mut self, n: &KNode, r: &ResolveRefIn) -> bool {
        let Some(scan) = self.ts_scope_scan(&n.file_path) else { return false };
        let containing: Vec<_> = scan.blocks.iter().filter(|b| b.contains(n.start_line, n.start_column)).collect();
        // Upstream's `global` flag on a namespace says the namespace itself is
        // a global name (its members still need the qualifier); the bare-name
        // gates here need the literal `declare global` block, whose members
        // are globals — upstream's `globalWords` positions.
        let global_block = |b: &&TsScopeBlock| b.name == "global" && b.global;
        if n.file_path == r.file_path {
            return containing.iter().all(|b| global_block(b) || b.contains(r.line, r.column));
        }
        if is_test_path(&n.file_path) && !is_test_path(&r.file_path) { return false; }
        if containing.iter().any(|b| b.ambient || !global_block(b)) { return false; }
        containing.iter().any(global_block) || (!scan.module_file && re!(r"\.d\.[cm]?ts$").is_match(&n.file_path) && !n.qualified_name.contains("::"))
    }

    pub(super) fn ts_import_target_visible(&mut self, node: &KNode, source_file: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(scan) = self.ts_scope_scan(&node.file_path) else { return Ok(true) };
        let Some(block) = scan.blocks.iter().find(|b| b.ambient && b.contains(node.start_line, node.start_column)) else { return Ok(true) };
        Ok(self.import_mappings(source_file)?.iter().any(|m| m.source == block.name)
            || self.import_mappings(&r.file_path)?.iter().any(|m| m.source == block.name))
    }

    fn ts_namespace_member(&mut self, file: &str, written: &str, r: &ResolveRefIn, imported: bool) -> Res<Option<Arc<KNode>>> {
        let Some((owner, name)) = written.rsplit_once('.') else { return Ok(None) };
        let Some(scan) = self.ts_scope_scan(file) else { return Ok(None) };
        let nodes = self.nodes_in_file(file)?;
        Ok(nodes.iter().find(|n| n.name == name && scan.blocks.iter().any(|b| b.name == owner && b.contains(n.start_line, n.start_column)
            && (imported || file == r.file_path || scan.blocks.iter().any(|g| g.global && g.contains(b.start.0, b.start.1)) || !scan.module_file)))
            .cloned())
    }

    fn ts_forwarded_ambient_alias(&mut self, file: &str, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(source) = self.read_file(file) else { return Ok(None) };
        let mut site = r.clone();
        site.file_path = file.to_string();
        let Some(tree) = self.js_binding_tree(&source, &site) else { return Ok(None) };
        for export in super::iteration::named_children(tree.root_node()).into_iter().filter(|n| n.kind() == "export_statement") {
            for alias in super::iteration::named_children(export).into_iter().filter(|n| n.kind() == "import_alias") {
                let parts = super::iteration::named_children(alias);
                if parts.len() != 2 || &source.text()[parts[0].byte_range()] != name { continue; }
                let written = &source.text()[parts[1].byte_range()];
                let Some((head, member)) = written.split_once('.') else { continue };
                let mapping = self.import_mappings(file)?.iter().find(|m| m.local_name == head && m.is_namespace).cloned();
                if let Some(mapping) = mapping {
                    return self.ts_ambient_lookup(&mapping.source, member, false, &site);
                }
            }
        }
        Ok(None)
    }

    /// The declaration files the project holds, computed once per resolution.
    fn ts_dts_files(&mut self) -> Res<Vec<String>> {
        if self.ts_dts_files.is_none() {
            let files = match self.sorted_files() { Some(files) => files, None => Arc::new(self.table()?.files.iter().cloned().collect()) };
            self.ts_dts_files = Some(files.iter().filter(|f| re!(r"\.d\.[cm]?ts$").is_match(f)).cloned().collect());
        }
        Ok(self.ts_dts_files.clone().unwrap_or_default())
    }

    /// The files whose script `declare module '<specifier>'` blocks declare
    /// `specifier`, indexed by name — upstream's `scanned.bySpecifier`. Built
    /// on first lookup; each file's scope scan is one text pass, memoized.
    fn ts_ambient_files(&mut self, specifier: &str) -> Res<Vec<String>> {
        if self.ts_ambient_index.is_none() {
            let mut index: HashMap<String, Vec<String>> = HashMap::new();
            for file in self.ts_dts_files()? {
                let Some(scan) = self.ts_scope_scan(&file) else { continue };
                if scan.module_file { continue; }
                for name in scan.blocks.iter().filter(|b| b.ambient && !b.name.contains('*')).map(|b| b.name.strip_prefix("node:").unwrap_or(&b.name).to_string()) {
                    index.entry(name).or_default().push(file.clone());
                }
            }
            self.ts_ambient_index = Some(index);
        }
        Ok(self.ts_ambient_index.as_ref().unwrap().get(specifier).cloned().unwrap_or_default())
    }

    pub(super) fn ts_ambient_lookup(&mut self, specifier: &str, exported: &str, default: bool, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        if specifier.starts_with('.') || specifier.starts_with('/') { return Ok(None); }
        let specifier = specifier.strip_prefix("node:").unwrap_or(specifier);
        let memo_key = (format!("scope-export\0{specifier}\0{}", r.file_path), format!("{default}\0{exported}"));
        let cached = self.ts_ambient_memo.get(&memo_key).cloned();
        if cached == Some(None) { return Ok(None); }
        let files = self.ts_ambient_files(specifier)?;
        let mut hits = Vec::new();
        for file in files.iter().filter(|f| cached.as_ref().is_none_or(|cached| cached.as_ref() == Some(*f))) {
            if is_test_path(file) && !is_test_path(&r.file_path) { continue; }
            let Some(scan) = self.ts_scope_scan(file) else { continue };
            for block in scan.blocks.iter().filter(|b| b.ambient && b.name.strip_prefix("node:").unwrap_or(&b.name) == specifier) {
                if exported == "*" {
                    if let Some(node) = self.nodes_in_file(file)?.iter().find(|n| n.kind == "file") { hits.push(node.clone()); }
                    continue;
                }
                let mut local = exported.to_string();
                if default {
                    let Some(name) = block.default_name.clone() else { continue };
                    local = name;
                } else {
                    for (exported_name, local_name) in &block.export_aliases {
                        if exported_name == exported { local = local_name.clone(); }
                    }
                }
                if !default && block.export_clause && local == exported {
                    let direct = Self::cached_regex(&format!(r"\bexport\s+(?:(?:declare|abstract)\s+)?(?:interface|class|type|function|const|let|var|enum|namespace)\s+{}\b", regex::escape(exported)))?;
                    let listed = Self::cached_regex(&format!(r"(?:\{{|,)\s*(?:type\s+)?{}\s*(?:,|\}})", regex::escape(exported)))?;
                    if !direct.is_match(&block.text) && !listed.is_match(&block.text) { continue; }
                }
                let nodes = self.nodes_in_file(file)?;
                let named: Vec<_> = nodes.iter().filter(|n| n.name == local && block.contains(n.start_line, n.start_column)
                    && !scan.blocks.iter().any(|b| !b.ambient && b.name != "global" && b.contains(n.start_line, n.start_column) && b.start > block.start && (n.start_line, n.start_column) > b.start)).cloned().collect();
                hits.extend(named);
            }
        }
        hits.sort_by_key(|n| (is_test_path(&n.file_path), std::cmp::Reverse(n.file_path.split('/').zip(r.file_path.split('/')).take_while(|(a,b)| a == b).count()), n.file_path.clone(), n.start_line));
        let hit = hits.into_iter().next();
        self.ts_ambient_memo.insert(memo_key, hit.as_ref().map(|n| n.file_path.clone()));
        Ok(hit)
    }

    fn ts_declared_ambient_receiver(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.js_binding_tree(&source, r) else { return Ok(None) };
        let mut class = super::iteration::descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        while !matches!(class.kind(), "class_declaration" | "class") {
            let Some(parent) = class.parent() else { return Ok(None) }; class = parent;
        }
        let mut pending = super::iteration::named_children(class);
        while let Some(node) = pending.pop() {
            if (node.kind() == "public_field_definition" || (matches!(node.kind(), "required_parameter" | "optional_parameter") && super::iteration::named_children(node).iter().any(|n| n.kind() == "accessibility_modifier" || n.kind() == "readonly")))
                && node.child_by_field_name("name").or_else(|| node.child_by_field_name("pattern")).is_some_and(|n| &source.text()[n.byte_range()] == receiver) {
                    if let Some(annotation) = node.child_by_field_name("type") {
                        let written = source.text()[annotation.byte_range()].trim_start_matches(':').trim();
                        let head = written.split('.').next().unwrap_or(written);
                        if let Some(mapping) = self.import_mappings(&r.file_path)?.iter().find(|m| m.local_name == head).cloned() {
                            let name = if mapping.is_namespace { written.rsplit('.').next().unwrap_or(written) } else { &mapping.exported_name };
                            return self.ts_ambient_lookup(&mapping.source, name, mapping.is_default, r);
                        }
                    }
            }
            if !matches!(node.kind(), "statement_block" | "class_declaration" | "class") { pending.extend(super::iteration::named_children(node)); }
        }
        Ok(None)
    }

    /// Type names occupy a different namespace from parameters and local values.
    /// A `typeof` type query still names a value and keeps the ordinary binding veto.
    pub(super) fn ts_reference_is_type_position(&mut self, r: &ResolveRefIn) -> bool {
        if !matches!(r.language.as_str(), "typescript" | "tsx") || r.reference_kind != "references" { return false; }
        let Some(source) = self.read_file(&r.file_path) else { return false };
        let Some(tree) = self.js_binding_tree(&source, r) else { return false };
        let mut node = super::iteration::descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        let mut type_name = false;
        loop {
            if node.kind() == "type_query" { return false; }
            type_name |= node.kind() == "type_identifier";
            if matches!(node.kind(), "statement_block" | "program") { return type_name; }
            let Some(parent) = node.parent() else { return type_name }; node = parent;
        }
    }

    fn ts_lexical_type(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.js_binding_tree(&source, r) else { return Ok(None) };
        let mut scope = Some(super::iteration::descendant_for_position(tree.root_node(), source.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize)));
        while let Some(at) = scope {
            if matches!(at.kind(), "statement_block" | "program") {
                for child in super::iteration::named_children(at) {
                    let declaration = if child.kind() == "export_statement" {
                        child.child_by_field_name("declaration").unwrap_or(child)
                    } else { child };
                    if !matches!(declaration.kind(), "type_alias_declaration" | "interface_declaration" | "class_declaration" | "enum_declaration") { continue; }
                    let Some(binding) = declaration.child_by_field_name("name") else { continue };
                    if &source.text()[binding.byte_range()] != name { continue; }
                    let node = self.nodes_in_file(&r.file_path)?.iter().find(|n| n.name == name
                        && n.start_line == declaration.start_position().row as i64 + 1
                        && matches!(n.kind.as_str(), "type_alias" | "interface" | "class" | "enum")).cloned();
                    return Ok(Some(node.map(|node| KCand { node, confidence: 0.95, resolved_by: "qualified-name" })));
                }
            }
            scope = at.parent();
        }
        Ok(None)
    }

    /// An authoritative TS import or type scope, before ordinary name matching.
    /// None leaves other languages and value references to the existing pipeline.
    pub(super) fn ts_scoped_reference(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if !is_js_family(&r.language) { return Ok(None); }
        if r.reference_kind == "calls" {
            if let Some((receiver, method)) = r.reference_name.strip_prefix("this.").and_then(|name| name.rsplit_once('.')) {
                if !receiver.contains('.') {
                    if let Some(ty) = self.ts_declared_ambient_receiver(receiver, r)? {
                        let (node, _) = self.ts_method_up_chain(&ty, method, r, 0)?;
                        return Ok(Some(node.map(|node| KCand { node, confidence: 0.95, resolved_by: "instance-method" })));
                    }
                }
            }
        }
        let type_ref = self.ts_reference_is_type_position(r) || is_inheritance_ref(&r.reference_kind);
        let imported = r.reference_kind == "imports";
        if r.reference_kind == "references" && !type_ref { return Ok(None); }
        let mappings = self.import_mappings(&r.file_path)?;
        let mut written = r.reference_name.clone();
        if type_ref {
            if let Some(source) = self.read_file(&r.file_path) {
                if let Some(line) = source.get((r.line - 1).max(0) as usize) {
                    let before = super::names::js_prefix(line, r.column.max(0) as usize);
                    if let Some(qualifier) = re!(r"([A-Za-z_$][\w$]*(?:\s*\.\s*[A-Za-z_$][\w$]*)*)\s*\.\s*$").captures(before) {
                        written = format!("{}.{}", qualifier[1].split_whitespace().collect::<String>(), written);
                    }
                }
                if let Some(tree) = self.js_binding_tree(&source, r) {
                    let mut node = super::iteration::descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
                    loop {
                        if let Some(parameters) = node.child_by_field_name("type_parameters") {
                            let pattern = Self::cached_regex(&format!(r"(?:<|,)\s*{}\s*(?:[,>=]|extends\b)", regex::escape(&r.reference_name)))?;
                            if pattern.is_match(&source.text()[parameters.byte_range()]) { return Ok(Some(None)); }
                        }
                        let Some(parent) = node.parent() else { break }; node = parent;
                    }
                }
            }
        }
        if imported && mappings.iter().any(|m| m.source == written) {
            if let Some(node) = self.ts_ambient_lookup(&written, "*", false, r)? {
                return Ok(Some(Some(KCand { node, confidence: 0.95, resolved_by: "import" })));
            }
            if !written.starts_with('.') && self.resolve_import_path(&written, &r.file_path, &r.language)?.is_none() { return Ok(Some(None)); }
        }
        // TypeScript's import-equals aliases can name a namespace member.
        if type_ref && !written.contains('.') {
            if let Some(source) = self.read_file(&r.file_path) {
                let alias = Self::cached_regex(&format!(r"\bimport\s+{}\s*=\s*([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)+)\s*;", regex::escape(&written)))?;
                if let Some(hit) = alias.captures(source.text()) { written = hit[1].to_string(); }
            }
        }
        if r.reference_kind == "calls" && !written.contains('.') && self.js_root_binding_scope(r, &written).is_some()
            && mappings.iter().any(|m| m.local_name == written) { return Ok(None); }
        if type_ref && !written.contains('.') {
            if let Some(local) = self.ts_lexical_type(&written, r)? { return Ok(Some(local)); }
        }
        let head = written.split('.').next().unwrap_or(&written);
        let binding = mappings.iter().find(|m| m.local_name == head).cloned();
        if let Some(binding) = binding {
            if r.reference_kind == "calls" && matches!(self.repository_package(package_name_of(&binding.source), &r.file_path), RepositoryPackage::Manifest | RepositoryPackage::Mapped) { return Ok(None); }
            if r.reference_kind == "calls" && self.resolve_via_import(r)?
                .is_some_and(|hit| !re!(r"\.d\.[cm]?ts$").is_match(&hit.node.file_path)) { return Ok(None); }
            let member = written.strip_prefix(&format!("{head}."));
            let exported = member.unwrap_or(&binding.exported_name);
            if let Some(node) = self.ts_ambient_lookup(&binding.source, exported, binding.is_default && member.is_none(), r)? {
                return Ok(Some(Some(KCand { node, confidence: 0.95, resolved_by: "import" })));
            }
            if imported {
                if let (Some(hit), Some(file)) = (self.resolve_via_import(r)?, self.resolve_import_path(&binding.source, &r.file_path, &r.language)?) {
                    if !self.ts_import_target_visible(&hit.node, &file, r)? { return Ok(Some(None)); }
                }
                return Ok(None);
            }
            if type_ref {
                if self.ts_ambient_lookup(&binding.source, "*", false, r)?.is_some() { return Ok(Some(None)); }
                if let Some(file) = self.resolve_import_path(&binding.source, &r.file_path, &r.language)? {
                    if self.nodes_in_file(&file)?.iter().any(|n| n.language == "arkts") { return Ok(None); }
                    if member.is_none() && !binding.is_namespace {
                        if let Some(node) = self.ts_forwarded_ambient_alias(&file, &binding.exported_name, r)? {
                            return Ok(Some(Some(KCand { node, confidence: 0.95, resolved_by: "import" })));
                        }
                        let want = ExportWant { is_default: binding.is_default, is_namespace: false, exported_name: binding.exported_name.clone(), member_name: None };
                        if let Some(node) = self.find_exported_symbol(&file, &want, &r.language, &mut HashSet::new(), 0)? {
                            if !self.ts_import_target_visible(&node, &file, r)? { return Ok(Some(None)); }
                            return Ok(Some(Some(KCand { node, confidence: 0.95, resolved_by: "import" })));
                        }
                    }
                    let qualified = if binding.is_namespace { member.map(str::to_string) } else { member.map(|m| format!("{}.{m}", binding.exported_name)) };
                    if let Some(qualified) = qualified {
                        if let Some(node) = self.ts_namespace_member(&file, &qualified, r, true)? {
                            return Ok(Some(Some(KCand { node, confidence: 0.95, resolved_by: "import" })));
                        }
                    }
                }
                let hit = self.resolve_via_import(&r.clone().naming(&written, "references"))?;
                if hit.as_ref().is_some_and(|hit| hit.node.language == "arkts") { return Ok(Some(hit)); }
                if let (Some(hit), Some(file)) = (&hit, self.resolve_import_path(&binding.source, &r.file_path, &r.language)?) {
                    if !self.ts_import_target_visible(&hit.node, &file, r)? { return Ok(Some(None)); }
                }
                if hit.is_none() && self.resolve_import_path(&binding.source, &r.file_path, &r.language)?.is_some()
                    && (binding.is_namespace || member.is_some() || binding.exported_name == binding.local_name) { return Ok(Some(None)); }
                if hit.is_none() && !self.is_declared_outside_package(&binding.source, &r.file_path) { return Ok(None); }
                return Ok(Some(hit));
            }
            if imported { return Ok(None); }
        }
        if !type_ref { return Ok(None); }
        if let Some(scan) = self.ts_scope_scan(&r.file_path) {
            for block in scan.blocks.iter().filter(|b| b.ambient && b.contains(r.line, r.column)) {
                if block.name.starts_with('.') {
                    if let Some(file) = self.resolve_import_path(&block.name, &r.file_path, &r.language)? {
                        if let Some(node) = self.nodes_in_file(&file)?.iter().find(|n| n.name == written) {
                            return Ok(Some(Some(KCand { node: node.clone(), confidence: 0.95, resolved_by: "import" })));
                        }
                    }
                }
            }
        }
        if written.contains('.') {
            let files = match self.sorted_files() { Some(files) => files, None => Arc::new(self.table()?.files.iter().cloned().collect()) };
            for file in files.iter().filter(|f| **f == r.file_path || re!(r"\.d\.[cm]?ts$").is_match(f)) {
                if is_test_path(file) && !is_test_path(&r.file_path) { continue; }
                if let Some(node) = self.ts_namespace_member(file, &written, r, false)? {
                    return Ok(Some(Some(KCand { node, confidence: 0.95, resolved_by: "qualified-name" })));
                }
            }
        }
        if let Some(scan) = self.ts_scope_scan(&r.file_path) {
            for block in scan.blocks.iter().filter(|b| b.ambient && b.contains(r.line, r.column)) {
                if let Some(node) = self.ts_ambient_lookup(&block.name, &written, false, r)? {
                    return Ok(Some(Some(KCand { node, confidence: 0.95, resolved_by: "import" })));
                }
            }
        }
        let leaf = written.rsplit('.').next().unwrap_or(&written);
        let mut candidates: Vec<_> = self.nodes_by_name(leaf)?.iter().filter(|n| is_js_family(&n.language)
            && matches!(n.kind.as_str(), "class" | "interface" | "type_alias" | "enum" | "enum_member" | "namespace")).cloned().collect();
        if written.contains('.') {
            let qualified = written.replace('.', "::");
            candidates.retain(|n| (n.qualified_name == qualified || n.qualified_name.ends_with(&format!("::{qualified}"))) && self.ts_scope_candidate(n, r));
        } else {
            candidates.retain(|n| self.ts_scope_candidate(n, r));
        }
        candidates.sort_by_key(|n| (n.file_path != r.file_path, is_test_path(&n.file_path), n.start_line));
        Ok(Some(candidates.into_iter().next().map(|node| KCand { node, confidence: 0.95, resolved_by: "qualified-name" })))
    }
}
