//! A TS/JS call on a value of a class its file names goes through that class
//! and the classes and interfaces it extends, each found the way its own file
//! names it (tsKnownTypeCall, tsMethodUpChain, tsNamedType and
//! tsOutsideTypeDeclaration of upstream's name-matcher.ts).

use super::fields::{ts_class_head, TS_BASE_WALK_DEPTH};
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
        let key = (binding.source.clone(), type_name.to_string());
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
        let head = ts_class_head(&source, cls);
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
        self.ts_named_type_in(written, &from.file_path, &from.language, &from.id, at)
    }

    /// ts_named_type for a name written in `file`, which `from_id` (a node
    /// there, or empty) is not itself.
    fn ts_named_type_in(&mut self, written: &str, file: &str, language: &str, from_id: &str, at: &ResolveRefIn) -> Res<TsNamedType> {
        let head = written.split('.').next().unwrap_or("");
        let mapping = self.import_mappings(file)?.iter().find(|i| i.local_name == head).cloned();
        // A name the file declares is looked up past `from_id` itself.
        let key = (file.to_string(), if mapping.is_some() { written.to_string() } else { format!("{written}\0{from_id}") });
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
            let file = self.ts_imported_type_file(&decl.ty, &at)?.unwrap_or_else(|| owner.file_path.clone());
            return Ok(Some((decl.ty, decl.array, Some(file))));
        }
        Ok(None)
    }

    /// Is `name` one of the type parameters the head of the TS class or
    /// interface `cls` declares (`class Emitter<T>`)?
    fn ts_class_type_parameter(&mut self, cls: &KNode, name: &str) -> bool {
        let Some(source) = self.read_file(&cls.file_path) else { return false };
        let from = (cls.start_line - 1).max(0) as usize;
        let head = source.get(from..(from + 3).min(source.len())).unwrap_or_default().join(" ");
        let head = head.split('{').next().unwrap_or("");
        let Ok(open) = Self::cached_regex(&format!(r"(?-u:\b){}\s*<", regex::escape(&cls.name))) else { return false };
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
        false
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
