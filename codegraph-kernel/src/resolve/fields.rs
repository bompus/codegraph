//! Field-chain arms: Go, TypeScript and Rust `self.` field receivers.

use super::*;

impl KernelResolver {
    /// matchGoFieldChainCall — Go 2-hop `base.field.Method`: base's type from
    /// the enclosing scope, field's declared type from the struct's own lines.
    pub(super) fn match_go_field_chain_call(
        &mut self,
        chain: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let segs: Vec<&str> = chain.split('.').collect();
        if segs.len() != 2 || segs[0].is_empty() || segs[1].is_empty() {
            return Ok(None);
        }
        let (base, field) = (segs[0], segs[1]);
        // A local bound from a type assertion has the asserted type.
        let asserted = match self.go_asserted_local_type(base, r) {
            Some(written) => match self.go_written_type(&written, r)? {
                Some(super::go_asserted::GoAsserted::Project { name, dir }) => Some((name, dir)),
                Some(super::go_asserted::GoAsserted::Outside) => return Ok(None),
                None => None,
            },
            None => None,
        };
        let (base_type, base_dir) = match asserted {
            Some(found) => found,
            None => {
                let Some(base_type) = self.infer_local_receiver_type(base, r, true)? else {
                    return Ok(None);
                };
                let Some(found) = self.go_type_package(&base_type, &r.file_path)? else { return Ok(None); };
                found
            }
        };
        // `\bFIELD\s+\*?\[?\]?TYPE`
        static FIELD_TYPE: LazyLock<Affix> =
            LazyLock::new(|| Affix::new("", r"\s+\*?\[?\]?([A-Za-z_][A-Za-z0-9_.]*)", true, false, false));
        let structs: Vec<Arc<KNode>> = prefer_call_site_file(
            self.nodes_by_name(&base_type)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "struct" | "class") && n.language == "go" && pos_dirname(&n.file_path) == base_dir
                })
                .cloned()
                .collect(),
            &r.file_path,
        );
        for s in structs {
            let Some(source) = self.read_file(&s.file_path) else {
                continue;
            };
            let start = (s.start_line - 1).max(0) as usize;
            let end = (s.end_line as usize).min(source.len());
            for raw in source.get(start..end).unwrap_or_default() {
                let line = strip_line_comments(raw);
                let Some(raw_type) = FIELD_TYPE.capture(&line, field).map(str::to_string) else {
                    continue;
                };
                // The package directory that declares the field's type: the
                // imported package for `pkg.Type`, else the struct's own.
                let pkg_dir = if raw_type.contains('.') {
                    let pkg = raw_type.split('.').next().unwrap_or("");
                    match self.go_imported_package_dir(&s.file_path, pkg)? {
                        Some(dir) => dir,
                        None => continue,
                    }
                } else {
                    pos_dirname(&s.file_path).to_string()
                };
                let Some(field_type) = raw_type.split('.').next_back() else {
                    continue;
                };
                if !field_type
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    || GO_BUILTIN_FIELD_TYPES.contains(field_type)
                {
                    continue;
                }
                if let Some(c) = self.go_method_in_package(field_type, method, &pkg_dir, r)? {
                    return Ok(Some(c));
                }
            }
        }
        Ok(None)
    }

    /// The module directory of the package `file_path` imports as `pkg`:
    /// `None` when `pkg` is no import there or names a package outside the
    /// module.
    pub(super) fn go_imported_package_dir(&mut self, file_path: &str, pkg: &str) -> Res<Option<String>> {
        let source = self.import_mappings(file_path)?.iter()
            .find(|i| i.local_name == pkg).map(|i| i.source.clone());
        Ok(source.and_then(|src| self.go_package_dir(&src, file_path)))
    }

    pub(super) fn go_package_dir(&self, import_path: &str, from_file: &str) -> Option<String> {
        let own = self.go_modules.iter().filter(|m| m.dir.is_empty() || from_file.starts_with(&format!("{}/", m.dir)))
            .max_by_key(|m| m.dir.len());
        let mut best: Option<&KernelGoModule> = None;
        for m in &self.go_modules {
            let hidden = m.dir.split('/').any(|d| d == "testdata" || d.starts_with(['.', '_']));
            let is_own = own.is_some_and(|o| std::ptr::eq(o, m));
            if hidden && !is_own { continue; }
            if import_path != m.module_path && !import_path.starts_with(&format!("{}/", m.module_path)) { continue; }
            if best.is_none_or(|b| m.module_path.len() > b.module_path.len() || (m.module_path.len() == b.module_path.len() && is_own)) { best = Some(m); }
        }
        let m = best?;
        let rest = import_path.strip_prefix(&m.module_path)?.trim_start_matches('/');
        Some(if m.dir.is_empty() { rest.to_string() } else if rest.is_empty() { m.dir.clone() } else { format!("{}/{rest}", m.dir) })
    }

    /// `Type::method` among Go methods declared in `pkg_dir`: a Go method
    /// lives in its receiver type's package, so a same-named type in another
    /// package is never the answer. With no `Type::method` anywhere the
    /// method may be promoted from an embedded type, so the supertype walk
    /// of resolve_method_on_type runs.
    pub(super) fn go_method_in_package(
        &mut self,
        type_name: &str,
        method: &str,
        pkg_dir: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let want = format!("{}::{}", type_name, method);
        let suffix = format!("::{}", want);
        let named: Vec<Arc<KNode>> = self
            .nodes_by_name(method)?
            .iter()
            .filter(|m| {
                m.kind == "method"
                    && m.language == "go"
                    && (m.qualified_name == want || m.qualified_name.ends_with(&suffix))
            })
            .cloned()
            .collect();
        let in_pkg: Vec<Arc<KNode>> =
            named.into_iter().filter(|m| pos_dirname(&m.file_path) == pkg_dir).collect();
        if in_pkg.is_empty() { return self.go_promoted_method(type_name, method, pkg_dir, r); }
        if r.reference_kind == "function_ref" && in_pkg.len() != 1 { return Ok(None); }
        let ordered = prefer_call_site_file(in_pkg, &r.file_path);
        Ok(Some(KCand { node: ordered[0].clone(), confidence: 0.85, resolved_by: "instance-method" }))
    }

    pub(super) fn go_type_package(
        &mut self,
        raw: &str,
        file: &str,
    ) -> Res<Option<(String, String)>> {
        let raw = raw.trim();
        let raw = raw.strip_prefix('&').unwrap_or(raw).trim();
        let Some(hit) = re!(
            r"^\*?([A-Za-z_]\w*(?:\.[A-Za-z_]\w*)?)(?:\[[\s\S]*\])?$"
        ).captures(raw) else { return Ok(None) };
        let named = &hit[1];
        if let Some((pkg, name)) = named.split_once('.') {
            return Ok(self.go_imported_package_dir(file, pkg)?
                .map(|dir| (name.to_string(), dir)));
        }
        Ok(Some((named.to_string(), pos_dirname(file).to_string())))
    }

    /// A named term in an interface is a supertype only if its declaration
    /// reaches an interface. Named scalar/struct terms constrain a type set.
    pub(super) fn go_interface_target(&mut self, target: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let mut target = target.clone();
        let mut seen = HashSet::new();
        loop {
            if target.language != "go" || !seen.insert(target.id.clone()) { return Ok(false); }
            if target.kind == "interface" { return Ok(true); }
            if target.kind != "type_alias" { return Ok(false); }
            let Some(source) = self.read_file(&target.file_path) else { return Ok(false); };
            let site = r.clone().at(&target);
            let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(false); };
            let mut declaration = super::iteration::descendant_for_position(
                tree.root_node(), source.text(),
                ((target.start_line - 1).max(0) as usize, target.start_column.max(0) as usize),
            );
            while !matches!(declaration.kind(), "type_spec" | "type_alias") {
                let Some(parent) = declaration.parent() else { return Ok(false); };
                declaration = parent;
            }
            let Some(mut ty) = declaration.child_by_field_name("type") else { return Ok(false); };
            if ty.kind() == "interface_type" { return Ok(true); }
            if ty.kind() == "generic_type" {
                let Some(base) = ty.child_by_field_name("type") else { return Ok(false); };
                ty = base;
            }
            if !matches!(ty.kind(), "type_identifier" | "qualified_type") { return Ok(false); }
            let raw = &source.text()[ty.start_byte()..ty.end_byte()];
            let Some((name, dir)) = self.go_type_package(raw, &target.file_path)? else { return Ok(false); };
            let candidates: Vec<_> = self.nodes_by_name(&name)?.iter()
                .filter(|n| n.language == "go" && pos_dirname(&n.file_path) == dir
                    && matches!(n.kind.as_str(), "interface" | "struct" | "type_alias"))
                .cloned().collect();
            let [only] = candidates.as_slice() else { return Ok(false); };
            target = (**only).clone();
        }
    }

    pub(super) fn go_method_on_declared_type(
        &mut self,
        raw: &str,
        decl_file: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let Some((ty, dir)) = self.go_type_package(raw, decl_file)? else {
            return Ok(None);
        };
        self.go_method_in_package(&ty, method, &dir, r)
    }

    fn go_embedded_types(&mut self, ty: &str, dir: &str) -> Res<Vec<(String, String)>> {
        let owners: Vec<_> = self.nodes_by_name(ty)?.iter()
            .filter(|n| n.language == "go" && matches!(n.kind.as_str(), "struct" | "interface" | "type_alias")
                && pos_dirname(&n.file_path) == dir).cloned().collect();
        let mut out = Vec::new();
        for owner in owners {
            let Some(lines) = self.read_file(&owner.file_path) else { continue };
            let text = lines.iter().skip((owner.start_line - 1).max(0) as usize)
                .take((owner.end_line - owner.start_line + 1).max(0) as usize)
                .map(|line| strip_line_comments(line)).collect::<Vec<_>>().join("\n");
            if owner.kind == "type_alias" {
                // Alias method sets follow their targets; defined types do not.
                let site = ResolveRefIn { from_node_id: owner.id.clone(), reference_name: owner.name.clone(), reference_kind: "references".to_string(), line: owner.start_line, column: owner.start_column, file_path: owner.file_path.clone(), language: "go".to_string(), row_id: None, candidates: None, failure_reason: None };
                if let Some(tree) = self.parsed_tree(&lines, &site) {
                    let mut declaration = super::iteration::descendant_for_position(
                        tree.root_node(), lines.text(),
                        ((owner.start_line - 1).max(0) as usize, owner.start_column.max(0) as usize),
                    );
                    while !matches!(declaration.kind(), "type_alias" | "type_spec") {
                        let Some(parent) = declaration.parent() else { break; };
                        declaration = parent;
                    }
                    let is_alias = declaration.kind() == "type_alias" || {
                        let mut cursor = declaration.walk();
                        let found = declaration.children(&mut cursor).any(|n| n.kind() == "ERROR"
                            && lines.text()[n.start_byte()..n.end_byte()].trim() == "=");
                        found
                    };
                    if is_alias {
                        if let Some(ty) = declaration.child_by_field_name("type") {
                            let raw = &lines.text()[ty.start_byte()..ty.end_byte()];
                            if let Some(base) = self.go_type_package(raw, &owner.file_path)? { out.push(base); }
                        }
                        continue;
                    }
                }
            }
            let Some(body) = text.split_once('{').map(|(_, b)| b.rsplit_once('}').map_or(b, |(b, _)| b)) else { continue };
            // Hide nested anonymous struct/interface bodies while preserving declaration separators.
            let mut depth = 0usize;
            let mut flat = String::new();
            for ch in body.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    '\n' => flat.push(ch),
                    _ if depth == 0 => flat.push(ch),
                    _ => (),
                }
            }
            for line in flat.split(['\n', ';']) {
                let Some(hit) = re!(r"^\s*\*?([A-Za-z_]\w*(?:\.[A-Za-z_]\w*)?)(?:\[[^\]]*\])?\s*(?:`[^`]*`)?\s*$").captures(line) else { continue };
                if let Some(base) = self.go_type_package(&hit[1], &owner.file_path)? { out.push(base); }
            }
        }
        Ok(out)
    }

    fn go_promoted_method(
        &mut self,
        ty: &str,
        method: &str,
        dir: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        // Each path gets its own visited set. Shared bases reached by two paths
        // must still contribute two candidates for ambiguity detection.
        let mut initial = HashSet::new();
        initial.insert((dir.to_string(), ty.to_string()));
        let mut level = vec![(ty.to_string(), dir.to_string(), initial)];
        for _depth in 0..4 {
            let mut next = Vec::new();
            let mut hits = Vec::new();
            for (owner, package, visited) in level {
                for (base, base_dir) in self.go_embedded_types(&owner, &package)? {
                    if visited.contains(&(base_dir.clone(), base.clone())) { continue; }
                    let mut path = visited.clone();
                    path.insert((base_dir.clone(), base.clone()));
                    let want = format!("{base}::{method}");
                    let members: Vec<_> = self.nodes_by_name(method)?.iter()
                        .filter(|n| n.language == "go" && n.kind == "method"
                            && (n.qualified_name == want
                                || n.qualified_name.ends_with(&format!("::{want}")))
                            && pos_dirname(&n.file_path) == base_dir)
                        .cloned().collect();
                    hits.extend(members);
                    next.push((base, base_dir, path));
                }
            }
            if !hits.is_empty() {
                if r.reference_kind == "function_ref" && hits.len() != 1 {
                    return Ok(None);
                }
                let ordered = prefer_call_site_file(hits, &r.file_path);
                return Ok(Some(KCand {
                    node: ordered[0].clone(), confidence: 0.85,
                    resolved_by: "instance-method",
                }));
            }
            if next.is_empty() { break; }
            level = next;
        }
        Ok(None)
    }

    /// matchTsFieldCall restricted to the boundOwner path (br:fieldchain) —
    /// the unbound owners-by-name branch only runs for `this.`-rooted refs,
    /// which isBindingReceiverCall excludes before this arm.
    pub(super) fn match_ts_field_call_bound(
        &mut self,
        owner: &KNode,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let Some(decl) = self.ts_field_decl(owner, field)? else {
            return Ok(None);
        };
        // An array/union/intersection-typed field names no single owner;
        // `conn: Conn | null` still dereferences to `Conn`.
        if decl.typed_collection && !decl.nullable {
            return Ok(None);
        }
        let m1 = decl.ty.as_str();
        if decl.value_type {
            let cls_bindings = self.bindings(&owner.file_path)?;
            let row = innermost_binding(
                &cls_bindings,
                m1,
                Some(owner.start_line),
            )
            .cloned();
            let holder_id = self.binding_target_id(row.as_ref(), |s| {
                s.resolve_via_import_member(&r.clone().at(owner).naming(m1, "references"))
            })?;
            let holder = self.node_by_opt_id(holder_id.as_deref())?;
            return match holder {
                Some(h) => self.resolve_object_literal_member(&h, method, r, 0.85, "instance-method"),
                None => Ok(None),
            };
        }
        let type_name = m1.split('.').next_back().unwrap_or("");
        if !type_name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        {
            return Ok(None);
        }
        self.match_bound_type_member(m1, method, &r.clone().at(owner))
    }

    /// The first field declaration of `field` in `owner`'s body lines
    /// (comment-stripped), in TS_FIELD_TYPE_PATTERNS order per line. A match
    /// inside one of the class's methods is a parameter or an object key, not
    /// the field, and is skipped; in the constructor only a parameter
    /// property (`private readonly field: T`) declares the field.
    pub(super) fn ts_field_decl(&mut self, owner: &KNode, field: &str) -> Res<Option<TsFieldDecl>> {
        // Unread while it is being read: a wrapper's holder field can lead back here.
        let key = (owner.id.clone(), field.to_string());
        if !self.ts_field_reading.insert(key.clone()) {
            return Ok(None);
        }
        let found = self.ts_field_decl_uncached(owner, field);
        self.ts_field_reading.remove(&key);
        found
    }

    fn ts_field_decl_uncached(&mut self, owner: &KNode, field: &str) -> Res<Option<TsFieldDecl>> {
        let Some(source) = self.read_file(&owner.file_path) else { return Ok(None) };
        let start = (owner.start_line - 1).max(0) as usize;
        let end = (owner.end_line as usize).min(source.len());
        let mut methods: Option<Vec<Arc<KNode>>> = None;
        let head = ts_class_head(&source, owner);
        let inherits = re!(r"\bextends\b").is_match(&head);
        let mut forwarded: Option<bool> = None;
        for (i, raw) in source.get(start..end).unwrap_or_default().iter().enumerate() {
            let line_no = (start + i + 1) as i64;
            let line = strip_line_comments(raw);
            // `get field(): T` exposes `field` as a property of type `T`.
            for at in occurrences(&line, field, 0) {
                let gap = &line[..at];
                let before = gap.trim_end();
                let is_getter = gap.len() > before.len()
                    && before.ends_with("get")
                    && !before[..before.len() - 3].bytes().next_back().is_some_and(|b| is_word_byte(b) || b == b'.' || b == b'#');
                if !is_getter || line[at + field.len()..].bytes().next().is_some_and(is_word_byte) {
                    continue;
                }
                let Some(caps) = re!(r"^\s*\(\s*\)\s*:\s*(?:readonly\s+)?([A-Za-z_$][A-Za-z0-9_.$]*)").captures(&line[at + field.len()..]) else {
                    continue;
                };
                // The getter's own node starts at `get` on this line; any other method around it holds a local.
                let pos = (line_no, at as i64);
                let methods = match &methods {
                    Some(v) => v,
                    None => methods.insert(self.class_methods(owner)?),
                };
                if methods.iter().any(|m| pos > (m.start_line, m.start_column) && pos <= (m.end_line, m.end_column) && !(m.name == field && m.start_line == line_no)) {
                    continue;
                }
                let rest = &line[at + field.len() + caps.get(0).map_or(0, |m| m.end())..];
                return Ok(Some(TsFieldDecl {
                    ty: caps[1].to_string(),
                    value_type: false,
                    typed_collection: guard1_tail_re().is_match(rest),
                        array: ts_array_suffix_re().is_match(rest),
                    nullable: nullable_union_tail_re().is_match(rest),
                }));
            }
            for (affix, value_type) in TS_FIELD_TYPE_PATTERNS.iter() {
                let (_, tail) = affix.local();
                for at in occurrences(&line, field, 0) {
                    let Some(m) = affix.finish_at(&line, field, at, None, tail.as_deref()) else {
                        continue;
                    };
                    let Some((gs, ge)) = m.group else { continue };
                    if gs == ge {
                        continue;
                    }
                    let methods = match &methods {
                        Some(v) => v,
                        None => methods.insert(self.class_methods(owner)?),
                    };
                    let hands_to_super = inherits && *forwarded.get_or_insert_with(|| ctor_forwards_to_super(&source, methods, field));
                    if !ts_declares_field(methods, affix, field, FieldSite { line: &line, line_no, at }, &line[gs..ge], hands_to_super) {
                        continue;
                    }
                    let rest = &line[m.end..];
                    return Ok(Some(TsFieldDecl {
                        ty: line[gs..ge].to_string(),
                        value_type: *value_type,
                        typed_collection: guard1_tail_re().is_match(rest),
                        array: ts_array_suffix_re().is_match(rest),
                        nullable: nullable_union_tail_re().is_match(rest),
                    }));
                }
            }
            // `_onDidChange = this._register(new Emitter())`, `x = inject(Http)`:
            // an X when the wrapper returns its argument, or the injector
            // hands back an instance of the class it is given.
            if let Some(init) = ts_init_on_line(&line, field) {
                let methods = match &methods {
                    Some(v) => v,
                    None => methods.insert(self.class_methods(owner)?),
                };
                let hands_to_super = inherits && *forwarded.get_or_insert_with(|| ctor_forwards_to_super(&source, methods, field));
                let site = FieldSite { line: &line, line_no, at: init.field_at };
                let declares = ts_declares_field(methods, &TS_FIELD_TYPE_PATTERNS[2].0, field, site, &init.ty, hands_to_super);
                if declares && self.ts_init_is_read(owner, &source, start + i, &init)? {
                    let rest = &line[init.end..];
                    return Ok(Some(TsFieldDecl {
                        ty: init.ty,
                        value_type: false,
                        typed_collection: guard1_tail_re().is_match(rest),
                        array: ts_array_suffix_re().is_match(rest),
                        nullable: nullable_union_tail_re().is_match(rest),
                    }));
                }
            }
        }
        Ok(None)
    }

    /// tsFieldRead (name-matcher.ts): a wrapped `new X(…)` is an X only when
    /// the wrapper's call ends the statement and the wrapper's declaration
    /// returns its argument; an injected one when the injector hands back an
    /// instance of the class it is given.
    fn ts_init_is_read(&mut self, cls: &KNode, source: &SourceFile, row: usize, init: &TsInit) -> Res<bool> {
        if !ts_call_ends_statement(source, row, init.open) {
            return Ok(false);
        }
        if let Some((holder, method)) = &init.via {
            if !self.ts_wrapper_returns_argument(cls, holder.as_deref(), method)? {
                return Ok(false);
            }
        }
        if let Some(injector) = init.injector {
            return self.ts_injects_instance(cls, injector);
        }
        Ok(true)
    }

    /// tsInjectsInstance (name-matcher.ts): Angular's `inject(X)` when the
    /// file imports `inject` from `@angular/core` (Vue's `inject(key)` returns
    /// whatever was provided under the key); `createInstance(X, …)` when the
    /// project declares one that returns an `InstanceType` of its constructor
    /// argument, as vscode's `IInstantiationService` does.
    fn ts_injects_instance(&mut self, cls: &KNode, injector: TsInjector) -> Res<bool> {
        if injector == TsInjector::Inject {
            return Ok(self.import_mappings(&cls.file_path)?.iter().any(|i| i.local_name == "inject" && i.source == "@angular/core"));
        }
        if let Some(declared) = self.ts_create_instance_declared {
            return Ok(declared);
        }
        let mut declared = false;
        for n in self.nodes_by_name("createInstance")?.clone().iter() {
            if !matches!(n.kind.as_str(), "method" | "function") || !is_esm_family(&n.language) {
                continue;
            }
            let Some(src) = self.read_file(&n.file_path) else { continue };
            let from = (n.start_line - 1).max(0) as usize;
            let head = src.get(from..(from + 3).min(src.len())).unwrap_or_default().join(" ");
            if re!(r"\bInstanceType\s*<").is_match(&head) {
                declared = true;
                break;
            }
        }
        self.ts_create_instance_declared = Some(declared);
        Ok(declared)
    }

    /// tsWrapperReturnsArgument (name-matcher.ts): does the wrapper a field
    /// initializer of class `cls` calls return its argument? `this.<method>`
    /// is the method `cls` declares or inherits, and `this.<holder>.<method>`
    /// that method of the class the holder field is declared as.
    fn ts_wrapper_returns_argument(&mut self, cls: &KNode, holder: Option<&str>, method: &str) -> Res<bool> {
        let at = ResolveRefIn {
            row_id: None,
            from_node_id: cls.id.clone(),
            reference_name: method.to_string(),
            reference_kind: "calls".to_string(),
            line: cls.start_line,
            column: cls.start_column,
            candidates: None,
            file_path: cls.file_path.clone(),
            language: cls.language.clone(),
            failure_reason: None,
        };
        let mut owner: Option<Arc<KNode>> = Some(Arc::new(cls.clone()));
        if let Some(holder) = holder {
            let held = self.ts_field_through_bases(&Arc::new(cls.clone()), holder, &at, 0)?;
            owner = match held {
                Some((decl, declaring)) if !decl.value_type && !decl.typed_collection => self.ts_named_class(&decl.ty, &declaring, &at)?,
                _ => None,
            };
        }
        let Some(owner) = owner else { return Ok(false) };
        let methods = self.ts_class_methods(&owner, method, &at, 0)?;
        Ok(!methods.is_empty() && methods.iter().all(|m| self.ts_returns_its_argument(m)))
    }

    /// tsClassMethods (name-matcher.ts): the TS methods named `name` that
    /// `cls` declares, or else the nearest class it extends that declares one.
    fn ts_class_methods(&mut self, cls: &Arc<KNode>, name: &str, at: &ResolveRefIn, depth: usize) -> Res<Vec<Arc<KNode>>> {
        let own: Vec<Arc<KNode>> = self
            .nodes_in_file(&cls.file_path)?
            .iter()
            .filter(|n| {
                n.kind == "method"
                    && n.name == name
                    && n.start_line >= cls.start_line
                    && n.end_line <= cls.end_line
                    && n.qualified_name[..n.qualified_name.rfind("::").unwrap_or(0)].rsplit(['.', ':']).next() == Some(cls.name.as_str())
            })
            .cloned()
            .collect();
        if !own.is_empty() || depth >= TS_BASE_WALK_DEPTH {
            return Ok(own);
        }
        let Some(source) = self.read_file(&cls.file_path) else { return Ok(own) };
        let head = ts_class_head(&source, cls);
        let Some(base_name) = re!(r"\bextends\s+([A-Za-z0-9_$.]+)").captures(&head).map(|m| m[1].to_string()) else {
            return Ok(own);
        };
        let Some(base) = self.ts_named_class(&base_name, cls, at)? else { return Ok(own) };
        if base.id == cls.id {
            return Ok(own);
        }
        self.ts_class_methods(&base, name, at, depth + 1)
    }

    /// tsReturnsItsArgument (name-matcher.ts): one parameter, typed by a type
    /// parameter of the method's own, which it returns — vscode's
    /// `_register<T extends IDisposable>(o: T): T` and `DisposableStore.add`.
    fn ts_returns_its_argument(&mut self, f: &KNode) -> bool {
        let Some(caps) = re!(r"^\(\s*[A-Za-z0-9_$]+\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\)\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)$")
            .captures(f.signature.as_deref().unwrap_or("").trim())
        else {
            return false;
        };
        if caps[1] != caps[2] {
            return false;
        }
        let Some(source) = self.read_file(&f.file_path) else { return false };
        let from = (f.start_line - 1).max(0) as usize;
        let head = source.get(from..(from + 3).min(source.len())).unwrap_or_default().join(" ");
        let Ok(rest) = Self::cached_regex(&format!(r"^\s*<\s*(?:const\s+)?{}\b", regex::escape(&caps[1]))) else { return false };
        let declared = occurrences(&head, &f.name, 0).any(|at| {
            !head[..at].bytes().next_back().is_some_and(|b| is_word_byte(b) || b == b'$' || b == b'#') && rest.is_match(&head[at + f.name.len()..])
        });
        declared
    }



    /// The method nodes declared in `owner`'s body.
    fn class_methods(&mut self, owner: &KNode) -> Res<Vec<Arc<KNode>>> {
        let prefix = format!("{}::", owner.qualified_name);
        Ok(self
            .nodes_in_file(&owner.file_path)?
            .iter()
            .filter(|n| {
                n.kind == "method"
                    && n.qualified_name.starts_with(&prefix)
                    && n.start_line >= owner.start_line
                    && n.end_line <= owner.end_line
            })
            .cloned()
            .collect())
    }

    /// matchRustSelfCall (name-matcher.ts): `self.method()` — the method on
    /// the type the call sits inside. The owner is the calling method's
    /// qualified-name prefix; a free fn has no `self`. Exactly one candidate
    /// must belong to that owner — two same-named methods on the same type
    /// is the fabrication this declines instead of.
    pub(super) fn match_rust_self_call(&mut self, method: &str, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(None);
        };
        if sep == 0 {
            return Ok(None);
        }
        let owner = caller.qualified_name[..sep].to_string();
        Ok(self
            .resolve_rust_self_member(&owner, method, &caller, &["method"])?
            .map(|node| KCand { node, confidence: 0.9, resolved_by: "qualified-name" }))
    }

    /// matchRustSelfPath (name-matcher.ts): `Self::item` associated-item
    /// path — `Self` binds to the caller qualified-name owner exactly as
    /// match_rust_self_call derives it, then the leaf resolves by
    /// `owner::leaf` qualified name over the prefixed member kinds (method,
    /// enum_member, constant). Advisory: Null falls through to the normal
    /// strategies so a ref today's bare-name arm resolves keeps its verdict.
    /// Two segments after the non-nested turbofish strip, plus the
    /// three-segment associated-type path `Self::Assoc::m` (`type Assoc = X`
    /// in the caller's enclosing impl block binds the middle segment, then
    /// `X::m` resolves like any owner path). A `Self::f().tail` leaf
    /// resolves through the receiver method's declared return type.
    pub(super) fn match_rust_self_path(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.language != "rust" || !r.reference_name.starts_with("Self::") {
            return Ok(None);
        }

        let strip = re!(r"<[^>]*>");
        let name = strip.replace_all(&r.reference_name, "");
        let segs: Vec<&str> = name.split("::").filter(|s| !s.is_empty()).collect();
        if segs[0] != "Self" || (segs.len() != 2 && segs.len() != 3) {
            return Ok(None);
        }
        let mut leaf = segs[segs.len() - 1].to_string();
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(None);
        };
        if sep == 0 {
            return Ok(None);
        }
        // `Self::Assoc::leaf` — the associated type binds in the caller's
        // enclosing `impl` block (`type Assoc = X`), not on the type.
        let mut owner: String = if segs.len() == 2 {
            caller.qualified_name[..sep].to_string()
        } else {
            match self.rust_assoc_type_binding(&caller, segs[1])? {
                Some(bound) => bound,
                None => return Ok(None),
            }
        };

        // `Self::f().tail` — a call-chained member: the leaf carries
        // `f().tail`, so the receiver method resolves first and its
        // declared return type binds the tail (`-> Self` means the
        // RECEIVER's owner, not the caller's). Only an empty-arg call
        // chain is read; anything else declines.
        let chain_re = re!(r"^(\w+)\(\)\.(\w+)$");
        if let Some(chained) = chain_re.captures(&leaf) {
            const METHOD: &[&str] = &["method"];
            let Some(recv) = self.resolve_rust_self_member(&owner, &chained[1], &caller, METHOD)?
            else {
                return Ok(None);
            };
            let Some(sig) = recv.signature.as_deref() else {
                return Ok(None);
            };
            let Some(arrow) = sig.rfind("->") else {
                return Ok(None);
            };
            let raw_ret = sig[arrow + 2..].trim();
            owner = if raw_ret == "Self" {
                match recv.qualified_name.rfind("::") {
                    Some(rs) => recv.qualified_name[..rs].to_string(),
                    None => return Ok(None),
                }
            } else {
                match self.normalize_inferred_type_name(raw_ret)? {
                    Some(t) => t,
                    None => return Ok(None),
                }
            };
            leaf = chained[2].to_string();
        } else if !leaf.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Ok(None);
        }

        const MEMBER_KINDS: &[&str] = &["method", "enum_member", "constant"];
        let Some(node) = self.resolve_rust_self_member(&owner, &leaf, &caller, MEMBER_KINDS)?
        else {
            return Ok(None);
        };
        Ok(Some(KCand {
            node,
            confidence: 0.9,
            resolved_by: "qualified-name",
        }))
    }

    /// resolveRustSelfMember (name-matcher.ts): the `owner::leaf`
    /// qualified-name lookup shared by `Self::item` and the `Self::f().tail`
    /// chain. Rust qualified names omit module paths, so two same-named
    /// owners need the caller's file to pin one (match_rust_self_call's
    /// disambiguation).
    pub(super) fn resolve_rust_self_member(
        &mut self,
        owner: &str,
        leaf: &str,
        caller: &Arc<KNode>,
        kinds: &[&str],
    ) -> Res<Option<Arc<KNode>>> {
        let want = format!("{}::{}", owner, leaf);
        let mut owned: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&want)?
            .iter()
            .filter(|n| {
                kinds.contains(&n.kind.as_str())
                    && n.language == "rust"
                    && n.qualified_name == want
            })
            .cloned()
            .collect();
        let owners: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(owner)?
            .iter()
            .filter(|n| {
                n.language == "rust"
                    && matches!(
                        n.kind.as_str(),
                        "struct" | "enum" | "union" | "trait" | "class"
                    )
            })
            .cloned()
            .collect();
        if owners.len() > 1 {
            if owners.iter().filter(|n| n.file_path == caller.file_path).count() != 1 {
                return Ok(None);
            }
            owned.retain(|n| n.file_path == caller.file_path);
        }
        if owned.len() != 1 {
            return Ok(None);
        }
        Ok(Some(owned[0].clone()))
    }

    /// matchRustBareSelf (name-matcher.ts): a bare `Self` ref names the
    /// enclosing type — `Self { .. }` constructions, `-> Self` positions
    /// and `Self(..)` calls all mean the caller qualified-name's owner.
    /// Only a concrete owner binds (struct/enum/union/class): inside a
    /// `trait` body `Self` is the abstract implementor and declines. A
    /// type-level caller (`struct S { next: Option<Self> }`) binds to
    /// itself. Same file-pin disambiguation as the member arms.
    pub(super) fn match_rust_bare_self(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        const TYPE_KINDS: &[&str] = &["struct", "enum", "union", "class"];
        let sep = caller.qualified_name.rfind("::");
        let owner: &str = match sep {
            Some(0) | None if TYPE_KINDS.contains(&caller.kind.as_str()) => {
                &caller.qualified_name
            }
            Some(0) | None => return Ok(None),
            Some(s) => &caller.qualified_name[..s],
        };
        let mut owners: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(owner)?
            .iter()
            .filter(|n| {
                n.language == "rust"
                    && TYPE_KINDS.contains(&n.kind.as_str())
                    && n.qualified_name == owner
            })
            .cloned()
            .collect();
        if owners.len() > 1 {
            owners.retain(|n| n.file_path == caller.file_path);
        }
        if owners.len() != 1 {
            return Ok(None);
        }
        Ok(Some(KCand {
            node: owners[0].clone(),
            confidence: 0.9,
            resolved_by: "qualified-name",
        }))
    }

    /// rustAssocTypeBinding (name-matcher.ts): the concrete type an
    /// associated-type name binds to inside the caller's enclosing `impl`
    /// block — `Self::Assoc` in `impl Tr for T` means that impl's
    /// `type Assoc = X` decl (trait defaults `type Assoc;` carry no `=` and
    /// miss). The impl block is found by brace-counting backward from the
    /// caller's first line to the enclosing block opener — only an `impl`
    /// opener qualifies — then the decl is matched per line at depth 1 (or
    /// on the opener line) and normalized like any inferred type name.
    pub(super) fn rust_assoc_type_binding(
        &mut self,
        caller: &Arc<KNode>,
        assoc_name: &str,
    ) -> Res<Option<String>> {
        let Some(file) = self.read_file(&caller.file_path) else {
            return Ok(None);
        };
        // Backward brace scan: the first `{` whose net depth goes negative
        // opens the block enclosing the caller — for a method, the impl.
        // Comments and literal contents are masked first, across lines, so a
        // `{` in `#[doc = "{"]`, `'{'` or a multi-line (raw) string is not code.
        let lines = file.rust_code_lines();
        let at = |i: i64| -> String {
            if i < 0 {
                String::new()
            } else {
                lines.get(i as usize).cloned().unwrap_or_default()
            }
        };
        let mut depth = 0i32;
        let mut block_idx = -1i64;
        for i in (0..caller.start_line - 1).rev() {
            let code = at(i);
            for ch in code.chars() {
                if ch == '{' {
                    depth -= 1;
                } else if ch == '}' {
                    depth += 1;
                }
            }
            if depth < 0 {
                block_idx = i;
                break;
            }
        }
        let impl_re = re!(r"(?-u:\b)impl(?-u:\b)");
        // Single-line impls put the opener on the caller's own line
        // (`impl T { type A = X; fn m(&self) { ... } }`).
        if block_idx < 0 {
            let own = at(caller.start_line - 1);
            if let Some(pos) = own.find('{') {
                if impl_re.is_match(&own[..pos]) {
                    block_idx = caller.start_line - 1;
                }
            }
        }
        if block_idx < 0 {
            return Ok(None);
        }
        // The opener must head an `impl` — check the line's pre-`{` head,
        // then brace-free continuation lines above it (`impl Tr for T\n{`);
        // a line bearing `{`/`}` belongs to a different construct.
        let mut is_impl = false;
        for j in ((block_idx - 4).max(0)..=block_idx).rev() {
            let code = at(j);
            if j == block_idx {
                let head = &code[..code.find('{').unwrap_or(0)];
                if impl_re.is_match(head) {
                    is_impl = true;
                }
                continue;
            }
            if code.contains('{') || code.contains('}') {
                break;
            }
            if impl_re.is_match(&code) {
                is_impl = true;
                break;
            }
        }
        if !is_impl {
            return Ok(None);
        }
        // Forward: `type <assoc> = X;` is a direct member — match at depth 1
        // or on the opener line itself, stop when the block closes.
        // `\btype\s+ASSOC\s*=\s*([^;]+);`
        static ASSOC_TYPE: LazyLock<Affix> =
            LazyLock::new(|| Affix::new(r"(?-u:\b)type\s+", r"\s*=\s*([^;]+);", false, false, false));
        depth = 0;
        let mut opened = false;
        let mut bound: Option<String> = None;
        for i in block_idx..lines.len() as i64 {
            let code = at(i);
            if (opened && depth == 1) || (i == block_idx && code.contains('{')) {
                if let Some(t) = ASSOC_TYPE.capture(&code, assoc_name) {
                    bound = Some(t.to_string());
                    break;
                }
            }
            for ch in code.chars() {
                if ch == '{' {
                    depth += 1;
                    opened = true;
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            if opened && depth <= 0 {
                break;
            }
        }
        let Some(bound) = bound else { return Ok(None) };
        self.normalize_inferred_type_name(&bound)
    }

    /// matchRustSelfFieldCall (name-matcher.ts): `self.<field>.<method>()`,
    /// exclusive for `self.<field>` receivers — the field's declared type
    /// off the owner struct's OWN declaration lines (comment-stripped,
    /// line by line), validated by rmot, or nothing. Rust struct fields are
    /// not graph nodes; the declaration text is the only place the type
    /// lives.
    pub(super) fn match_rust_self_field_call(
        &mut self,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if field.is_empty() || field.contains('.') {
            return Ok(None);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(None);
        };
        if sep == 0 {
            return Ok(None);
        }
        let Some(owner) = caller.qualified_name[..sep].split("::").last() else {
            return Ok(None);
        };
        let owners = prefer_call_site_file(
            self.nodes_by_name(owner)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "struct" | "union" | "class")
                        && n.language == "rust"
                })
                .cloned()
                .collect(),
            &r.file_path,
        );
        // `\bFIELD\s*:\s*([^,{}]+)`
        static RUST_FIELD_TYPE: LazyLock<Affix> =
            LazyLock::new(|| Affix::new("", r"\s*:\s*([^,{}]+)", true, false, false).lead(b":"));
        for s in owners {
            let Some(source) = self.read_file(&s.file_path) else {
                continue;
            };
            let start = (s.start_line - 1).max(0) as usize;
            let end = (s.end_line as usize).min(source.len());
            if field.bytes().all(|b| b.is_ascii_digit()) {
                // `self.N` — the Nth positional field of a tuple struct
                // (`struct Wrap(pub Table)`), which the `name:` scan below
                // cannot see.
                let idx: usize = field.parse().unwrap_or(usize::MAX);
                let head = source.get(start..end).unwrap_or_default().join("\n");
                let Some(m) = re!(r"\bstruct\s+[A-Za-z_]\w*\s*(?:<[^>]*>)?\s*\(([^)]*)\)").captures(&head) else {
                    continue;
                };
                let types: Vec<&str> = m[1].split(',').collect();
                let Some(decl) = types.get(idx) else { return Ok(None) };
                let decl = decl.trim().trim_start_matches("pub").trim().to_string();
                let Some(field_type) = rust_field_type_name(&decl) else {
                    return Ok(None);
                };
                return self.rust_field_method(&decl, &field_type, method, &s, r);
            }
            for raw in source.get(start..end).unwrap_or_default() {
                let line = strip_line_comments(raw);
                let Some(declared) = RUST_FIELD_TYPE.capture(&line, field) else {
                    continue;
                };
                // The field is declared here; whether or not its type names a
                // project symbol, this owner is the answer — terminal.
                let Some(field_type) = rust_field_type_name(declared) else {
                    return Ok(None);
                };
                let declared = declared.to_string();
                return self.rust_field_method(&declared, &field_type, method, &s, r);
            }
        }
        Ok(None)
    }

    /// `Type::method` for a Rust field type. Qualified names carry no module
    /// path, so when several types share the name the one the field means
    /// is pinned by where it is declared: an inline path (`inner::Inner`),
    /// the struct's own file, or the struct file's `use` of it. Without
    /// that evidence it declines instead of taking the first file.
    fn rust_field_method(
        &mut self,
        declared: &str,
        type_name: &str,
        method: &str,
        owner: &KNode,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let want = format!("{}::{}", type_name, method);
        let suffix = format!("::{}", want);
        let named: Vec<Arc<KNode>> = self
            .nodes_by_name(method)?
            .iter()
            .filter(|m| {
                m.kind == "method"
                    && m.language == "rust"
                    && (m.qualified_name == want || m.qualified_name.ends_with(&suffix))
            })
            .cloned()
            .collect();
        if named.len() <= 1 {
            return self.resolve_method_on_type(type_name, method, r, 0.85, "instance-method", None);
        }
        let Some(type_file) = self.rust_field_type_file(declared, type_name, owner)? else {
            return Ok(None);
        };
        let pinned: Vec<Arc<KNode>> =
            named.into_iter().filter(|m| m.file_path == type_file).collect();
        if pinned.len() != 1 {
            return Ok(None);
        }
        Ok(Some(KCand { node: pinned[0].clone(), confidence: 0.85, resolved_by: "instance-method" }))
    }

    /// The file declaring the type a Rust field names (see rust_field_method).
    fn rust_field_type_file(
        &mut self,
        declared: &str,
        type_name: &str,
        owner: &KNode,
    ) -> Res<Option<String>> {
        // `a::b::Type` written in the field: the path's module file.
        if let Some(at) = declared.find(&format!("::{}", type_name)) {
            let head = &declared[..at];
            let start = head
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                .map_or(0, |i| i + 1);
            let segs: Vec<&str> = head[start..].split("::").filter(|s| !s.is_empty()).collect();
            if !segs.is_empty() {
                return self.resolve_rust_module_file(&segs, &owner.file_path);
            }
        }
        let declares = self.nodes_in_file(&owner.file_path)?.iter().any(|n| {
            n.name == type_name
                && matches!(n.kind.as_str(), "struct" | "enum" | "union" | "trait" | "type_alias")
        });
        if declares {
            return Ok(Some(owner.file_path.clone()));
        }
        let Some(content) = self.read_file(&owner.file_path) else { return Ok(None) };
        let Some(path) = content.rust_uses().get(type_name).cloned() else { return Ok(None) };
        let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
        if segs.len() < 2 {
            return Ok(None);
        }
        self.resolve_rust_module_file(&segs[..segs.len() - 1], &owner.file_path)
    }

    /// matchTsThisFieldCall — the `this.field.method` entry point of
    /// matchMethodCall's requireReceiverEvidence=false arm. The owner is the
    /// enclosing class written on the calling method's qualified name, so it
    /// is not a guess — same exclusive discipline as the go/rust field arms.
    pub(super) fn match_ts_this_field_call(
        &mut self,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if field.is_empty() || field.contains('.') {
            return Ok(None);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        let caller = self.this_scope_caller(caller)?;
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(None);
        };
        if sep == 0 {
            return Ok(None); // not inside a class
        }
        let owner = caller.qualified_name[..sep]
            .split("::")
            .last()
            .unwrap_or("");
        if owner.is_empty() {
            return Ok(None);
        }
        self.match_ts_field_call_free(owner, field, method, r)
    }

    /// matchTsFieldCall without boundOwner — the unbound variant used by the
    /// member-tail arm. Owners are named classes/components visible to the
    /// call site (call-site file first); the declared-type tail resolves
    /// through rmot, never btm. A `typeof` field still routes through the
    /// object-literal member scan, but by plain name lookup + same-family
    /// filter rather than the binding row the bound variant uses.
    pub(super) fn match_ts_field_call_free(
        &mut self,
        owner: &str,
        field: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let owners = prefer_call_site_file(
            self.nodes_by_name(owner)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "class" | "component")
                        && same_language_family(&n.language, &r.language)
                })
                .cloned()
                .collect(),
            &r.file_path,
        );
        // The calling class declares the field, or a class it extends does;
        // else a same-named class elsewhere, but only with a type the call's
        // file imports or declares: another app's class of that name says
        // nothing about this one's field.
        let mut found: Vec<(TsFieldDecl, Arc<KNode>)> = Vec::new();
        for cls in owners.iter().filter(|c| c.file_path == r.file_path) {
            if let Some(hit) = self.ts_field_through_bases(cls, field, r, 0)? {
                found.push(hit);
                break;
            }
        }
        if found.is_empty() {
            for cls in owners.iter().filter(|c| c.file_path != r.file_path) {
                let Some(decl) = self.ts_field_decl(cls, field)? else { continue };
                if self.ts_type_visible_in(&decl.ty, r)? {
                    // The type is the class the call's file names.
                    let mut at = (**cls).clone();
                    at.file_path = r.file_path.clone();
                    at.language = r.language.clone();
                    found.push((decl, Arc::new(at)));
                    break;
                }
            }
        }
        if let Some((decl, cls)) = found.first() {
            // `items: Mailer[]` names no single owner (`push` is the
            // array's); `conn: Conn | null` still dereferences to `Conn`.
            if decl.typed_collection && !decl.nullable {
                return Ok(None);
            }
            let m1 = decl.ty.as_str();
            if decl.value_type {
                // `field: typeof Ns` — the namespace value's members
                // are bare-named functions inside a const/variable.
                let holder_name =
                    m1.split('.').next_back().unwrap_or("");
                let holders = prefer_call_site_file(
                    self.nodes_by_name(holder_name)?
                        .iter()
                        .filter(|n| {
                            matches!(n.kind.as_str(), "constant" | "variable")
                                && same_language_family(&n.language, &r.language)
                        })
                        .cloned()
                        .collect(),
                    &r.file_path,
                );
                for holder in &holders {
                    if let Some(hit) = self.resolve_object_literal_member(
                        holder,
                        method,
                        r,
                        0.85,
                        "instance-method",
                    )? {
                        return Ok(Some(hit));
                    }
                }
                return Ok(None);
            }
            // `ns.Mailer` → `Mailer`; a primitive or builtin names no
            // project type.
            let type_name = m1.split('.').next_back().unwrap_or("");
            if !type_name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
            {
                return Ok(None);
            }
            // The file the field is written in names its type: it imports
            // the one the field holds, or declares it, so that one's method
            // is the call's. One it imports from a package outside the
            // project, which no project file declares, has no project method.
            let at = if cls.file_path == r.file_path { r.clone() } else { Self::ref_from_class(r, cls) };
            let mut imported = self.ts_imported_type_file(m1, &at)?;
            if imported.is_none() {
                match self.ts_outside_type_declaration(m1, type_name, &at)? {
                    Some(None) => return Ok(None),
                    Some(Some(file)) => imported = Some(file),
                    None if cls.file_path != r.file_path => imported = Some(cls.file_path.clone()),
                    // A package import no manifest lists: still outside the
                    // project, with no project method (fork: kept from before
                    // the merge; upstream falls through to same-named classes).
                    None if self.ts_imports_package(m1, &at)? => return Ok(None),
                    None => {}
                }
            }
            // That class decides, through its own supertypes: a same-named
            // class elsewhere answered when it didn't declare the method
            // itself.
            if let Some(known) = self.ts_known_type_call(type_name, imported.as_deref().unwrap_or(&at.file_path), method, &at, 0.85)? {
                return Ok(known);
            }
            // Two apps in one repo may each declare the type. Among
            // its declarations of the method prefer the one closest
            // to the call site's directory — never index order.
            let declared: Vec<Arc<KNode>> = self
                .nodes_by_name(method)?
                .iter()
                .filter(|n| {
                    n.kind == "method"
                        && same_language_family(&n.language, &r.language)
                        && (n.qualified_name == format!("{type_name}::{method}")
                            || n.qualified_name
                                .ends_with(&format!("::{type_name}::{method}")))
                })
                .cloned()
                .collect();
            if declared.len() > 1 {
                if let Some(own) = imported.as_ref().and_then(|file| declared.iter().find(|n| &n.file_path == file)) {
                    return Ok(Some(KCand { node: own.clone(), confidence: 0.85, resolved_by: "instance-method" }));
                }
                let call_dirs: Vec<&str> = {
                    let mut v: Vec<&str> = r.file_path.split('/').collect();
                    v.pop();
                    v
                };
                let shared = |fp: &str| shared_dir_prefix(&call_dirs, fp);
                let max_shared =
                    declared.iter().map(|n| shared(&n.file_path)).max().unwrap_or(0);
                // Ties break by code-unit order (JS `<` on the paths); the
                // first of equal paths wins, as the stable sort keeps it.
                let nearest = declared
                    .iter()
                    .filter(|n| shared(&n.file_path) == max_shared)
                    .min_by(|a, b| a.file_path.encode_utf16().cmp(b.file_path.encode_utf16()))
                    .unwrap();
                return Ok(Some(KCand {
                    node: nearest.clone(),
                    confidence: 0.85,
                    resolved_by: "instance-method",
                }));
            }
            return self.resolve_method_on_type(
                type_name,
                method,
                r,
                0.85,
                "instance-method",
                imported.as_deref(),
            );
        }
        Ok(None)
    }

    /// `r` made from the file of `cls`.
    fn ref_from_class(r: &ResolveRefIn, cls: &KNode) -> ResolveRefIn {
        let mut at = r.clone();
        at.file_path = cls.file_path.clone();
        at.language = cls.language.clone();
        at.line = cls.start_line;
        at.column = cls.start_column;
        at
    }

    /// tsFieldThroughBases (name-matcher.ts): `field`'s declaration on `cls`
    /// or on the classes it extends, nearest first, with the class it is
    /// written in. Each base is the class its subclass's file names.
    fn ts_field_through_bases(&mut self, cls: &Arc<KNode>, field: &str, r: &ResolveRefIn, depth: usize) -> Res<Option<(TsFieldDecl, Arc<KNode>)>> {
        if let Some(decl) = self.ts_field_decl(cls, field)? {
            return Ok(Some((decl, cls.clone())));
        }
        if depth >= TS_BASE_WALK_DEPTH {
            return Ok(None);
        }
        let Some(source) = self.read_file(&cls.file_path) else { return Ok(None) };
        let head = ts_class_head(&source, cls);
        let Some(base_name) = re!(r"\bextends\s+([A-Za-z0-9_$.]+)").captures(&head).map(|m| m[1].to_string()) else {
            return Ok(None);
        };
        let Some(base) = self.ts_named_class(&base_name, cls, r)? else { return Ok(None) };
        if base.id == cls.id {
            return Ok(None);
        }
        self.ts_field_through_bases(&base, field, r, depth + 1)
    }

    /// tsNamedClass (name-matcher.ts): the class `name` names in `from`'s
    /// file: the one that file imports under it, followed through re-exports
    /// and aliases, or else a class the file declares.
    fn ts_named_class(&mut self, name: &str, from: &KNode, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let imported = self.import_mappings(&from.file_path)?.iter().any(|m| m.local_name == name);
        if imported {
            let mut at = r.clone().naming(name, "references");
            at.from_node_id = from.id.clone();
            at.file_path = from.file_path.clone();
            at.language = from.language.clone();
            at.line = from.start_line;
            at.column = from.start_column;
            return Ok(self
                .resolve_via_import(&at)?
                .map(|c| c.node)
                .filter(|n| matches!(n.kind.as_str(), "class" | "component")));
        }
        Ok(self
            .nodes_in_file(&from.file_path)?
            .iter()
            .find(|n| n.kind == "class" && n.name == name)
            .cloned())
    }

    /// tsTypeVisibleIn (name-matcher.ts): does the call's file import a type
    /// named `ty` from the project, or declare one?
    fn ts_type_visible_in(&mut self, ty: &str, r: &ResolveRefIn) -> Res<bool> {
        let head = ty.split('.').next().unwrap_or("");
        if self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == head) {
            return Ok(true);
        }
        Ok(self.nodes_in_file(&r.file_path)?.iter().any(|n| {
            n.name == head && matches!(n.kind.as_str(), "class" | "interface" | "type_alias" | "constant" | "variable")
        }))
    }
}

/// The class heading up to its body, type parameters removed: `class A<T extends X>`
/// does not extend anything, `class A extends B` does.
pub(super) fn ts_class_head(source: &SourceFile, owner: &KNode) -> String {
    // The head may run over several lines and sit below decorators, whose own
    // braces are not the body.
    let start = (owner.start_line - 1).max(0) as usize;
    let text = source
        .get(start..(start + 8).min(source.len()))
        .unwrap_or_default()
        .iter()
        .map(|l| strip_line_comments(l).to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let named = re!(r"(?-u:\b)(?:class|interface)\s+([A-Za-z0-9_$]+)")
        .captures_iter(&text)
        .find(|c| c[1] == owner.name)
        .and_then(|c| c.get(0).map(|m| m.start()))
        .or_else(|| re!(r"(?-u:\b)class(?-u:\b)").find(&text).map(|m| m.start()))
        .unwrap_or(0);
    let mut head = String::new();
    let mut depth = 0usize;
    let mut prev = ' ';
    for c in text[named..].chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 && prev != '=' => depth -= 1,
            '{' if depth == 0 => break,
            _ if depth == 0 => head.push(c),
            _ => {}
        }
        prev = c;
    }
    head
}

/// How many classes up its `extends` chain a TS class's fields and wrapper
/// methods are looked for: vscode's editors declare emitters through
/// `_register`, which `Disposable` declares up to eight classes above them.
pub(super) const TS_BASE_WALK_DEPTH: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TsInjector {
    Inject,
    CreateInstance,
}

/// A field initializer that wraps or injects a class: `this._register(new X())`,
/// `inject(X)`.
struct TsInit {
    ty: String,
    /// The wrapper's holder (`this.<holder>.<method>(`) and method.
    via: Option<(Option<String>, String)>,
    injector: Option<TsInjector>,
    /// Byte offsets in the line: the field, the call's `(` and the end of the match.
    field_at: usize,
    open: usize,
    end: usize,
}

/// The first wrapped or injected declaration of `field` on one line.
fn ts_init_on_line(line: &str, field: &str) -> Option<TsInit> {
    let token = |b: u8| is_word_byte(b) || b == b'$' || b == b'#';
    for at in occurrences(line, field, 0) {
        let after = at + field.len();
        if line[..at].bytes().next_back().is_some_and(token) || line[after..].bytes().next().is_some_and(token) {
            continue;
        }
        let rest = &line[after..];
        if let Some(m) = re!(r"^\s*=\s*this\.(?:([A-Za-z0-9_$#]+)\.)?([A-Za-z0-9_$#]+)\(\s*(?:new\s+|((?:this\.)?(?:[A-Za-z0-9_$#]+\.)*createInstance)\s*\(\s*)([A-Za-z_$][A-Za-z0-9_.$]*)").captures(rest) {
            let whole = m.get(0)?;
            return Some(TsInit {
                ty: m[4].to_string(),
                via: Some((m.get(1).map(|h| h.as_str().to_string()), m[2].to_string())),
                injector: m.get(3).map(|_| TsInjector::CreateInstance),
                field_at: at,
                open: after + whole.as_str().find('(')?,
                end: after + whole.end(),
            });
        }
        if let Some(m) = re!(r"^\s*=\s*(inject|(?:this\.)?(?:[A-Za-z0-9_$#]+\.)*createInstance)\s*(?:<\s*([A-Z][A-Za-z0-9_.$]*)[^;=(){}]*>)?\s*\(\s*([A-Z][A-Za-z0-9_.$]*)").captures(rest) {
            let whole = m.get(0)?;
            // An injector's type argument is the type it returns: `inject<Params>(DIALOG_DATA)`.
            let ty = m.get(2).or_else(|| m.get(3))?.as_str().to_string();
            // The first `(` after the field is the injector's: a type argument list holds none.
            return Some(TsInit {
                ty,
                via: None,
                injector: Some(if &m[1] == "inject" { TsInjector::Inject } else { TsInjector::CreateInstance }),
                field_at: at,
                open: after + whole.as_str().find('(')?,
                end: after + whole.end(),
            });
        }
    }
    None
}

/// tsCallEndsStatement (name-matcher.ts): does the call whose `(` is at
/// `open` on line `row` end its statement, so that what it returns is what
/// the statement assigns? Nothing but `;` may come after its `)`, nor start
/// the next line — `.event`, `?.x` or `[0]` would take a member of what it
/// returns. Strings are skipped; an unclosed one, or a call still open after
/// 12 lines, isn't read.
fn ts_call_ends_statement(source: &SourceFile, row: usize, open: usize) -> bool {
    let mut depth = 0i32;
    let mut r = row;
    let mut from = open;
    while r < source.len() && r < row + 12 {
        let line = strip_line_comments(&source[r]);
        let bytes = line.as_bytes();
        let mut i = from;
        while i < bytes.len() {
            let c = bytes[i];
            if matches!(c, b'"' | b'\'' | b'`') {
                let mut close = i + 1;
                while close < bytes.len() && bytes[close] != c {
                    close += if bytes[close] == b'\\' { 2 } else { 1 };
                }
                if close >= bytes.len() {
                    return false;
                }
                i = close;
            } else if c == b'(' {
                depth += 1;
            } else if c == b')' {
                depth -= 1;
                if depth == 0 {
                    let rest = line[i + 1..].trim();
                    if !rest.is_empty() {
                        return rest.starts_with(';');
                    }
                    let next = (r + 1..(r + 4).min(source.len())).map(|k| strip_line_comments(&source[k])).find(|l| !l.trim().is_empty());
                    return next.is_none_or(|n| !re!(r"^\s*(?:[.?\[(!,:|&+*/%<>=-]|as\b|satisfies\b)").is_match(&n));
                }
            }
            i += 1;
        }
        r += 1;
        from = 0;
    }
    false
}

/// Where a candidate field declaration sits: its line text, line number and byte offset.
#[derive(Clone, Copy)]
struct FieldSite<'a> {
    line: &'a str,
    line_no: i64,
    at: usize,
}

/// What follows a field's type when the field holds an array of it: `[]`, after any type arguments.
fn ts_array_suffix_re() -> Rc<Regex> {
    re!(r"^\s*(?:<[^;=(){}<>]*(?:<[^;=(){}<>]*>[^;=(){}<>]*)*>)?\s*\[\s*\]")
}

/// A TypeScript class-field declaration found by `ts_field_decl`.
pub(super) struct TsFieldDecl {
    /// The declared type text (`Mailer`, `ns.Mailer`, the `typeof` operand).
    pub(super) ty: String,
    /// `field: typeof Ns` — a value type, not a class.
    pub(super) value_type: bool,
    /// The type continues into `[]`, `|` or `&` (GUARD1_TAIL_RE).
    pub(super) typed_collection: bool,
    /// The field holds an array of the type: `[]` follows it, after any type arguments.
    pub(super) array: bool,
    /// The only union members after the type are `undefined`/`null`.
    pub(super) nullable: bool,
}

/// Whether the class's constructor passes `field` as an argument of `super(...)`,
/// the only way a plain constructor parameter names an inherited field. Only a
/// bare top-level argument counts: `options.field`, `wrap(field)`, `{ field }`
/// and a `super(field)` inside a string literal do not.
fn ctor_forwards_to_super(source: &SourceFile, methods: &[Arc<KNode>], field: &str) -> bool {
    let Some(ctor) = methods.iter().find(|m| m.name == "constructor") else { return false };
    let from = (ctor.start_line - 1).max(0) as usize;
    let to = (ctor.end_line as usize).min(source.len());
    let lines: Vec<String> = source.get(from..to).unwrap_or_default().iter().map(|l| strip_line_comments(l)).collect();
    let body = blank_string_literals(&lines.join("\n"));
    let forwarded = occurrences(&body, "super", 0).any(|at| {
        if at > 0 && is_word_byte(body.as_bytes()[at - 1]) {
            return false;
        }
        let Some(rest) = body[at + 5..].trim_start().strip_prefix('(') else { return false };
        let mut depth = 1;
        let mut arg_start = 0;
        for (i, c) in rest.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 1 => {
                    if rest[arg_start..i].trim() == field {
                        return true;
                    }
                    arg_start = i + 1;
                }
                _ => {}
            }
            if depth == 0 {
                return rest[arg_start..i].trim() == field;
            }
        }
        false
    });
    forwarded
}

/// `text` with the contents of every quoted string and template literal
/// replaced by spaces, so a `super(` or a parameter name inside one is not
/// read as code. An unterminated quote blanks to the end.
fn blank_string_literals(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in text.chars() {
        match quote {
            None => {
                if matches!(c, '\'' | '"' | '`') {
                    quote = Some(c);
                }
                out.push(c);
            }
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                    out.push(c);
                    continue;
                }
                out.push(if c == '\n' { '\n' } else { ' ' });
            }
        }
    }
    out
}

/// Whether a match at byte `at` of line `line_no` declares the class field
/// `field` with type `ty`: outside every method of the class, a
/// `this.field = new T` assignment, or a constructor parameter (read back
/// from the constructor's signature, so an object key in its body is not
/// one). Anything else inside a method is a parameter, local or object key.
/// The member's own name (a property arrow function extracted as a method)
/// sits at the method's start and counts as outside.
fn ts_declares_field(
    methods: &[Arc<KNode>],
    affix: &'static Affix,
    field: &str,
    site: FieldSite,
    ty: &str,
    inherits: bool,
) -> bool {
    let FieldSite { line, line_no, at } = site;
    let pos = (line_no, at as i64);
    let inside = methods.iter().find(|m| {
        pos > (m.start_line, m.start_column) && pos <= (m.end_line, m.end_column)
    });
    match inside {
        None => true,
        Some(_) if line[..at].ends_with("this.") => true,
        Some(m) if m.name == "constructor" => m
            .signature
            .as_deref()
            .and_then(|sig| {
                let (s, e) = affix.find_from(sig, field, 0)?.group?;
                if &sig[s..e] != ty {
                    return None;
                }
                // A parameter property (`private x: T`) declares a field. A plain
                // `x: T` parameter is a local, unless the class extends another
                // and hands it to `super` for an inherited field (`inherits`).
                let head = sig[..s].rfind(field).map(|at| &sig[..at])?;
                let param = head.rsplit([',', '(']).next().unwrap_or("");
                Some(inherits || re!(r"(?:^|\s)(?:public|private|protected|readonly)\s+(?:(?:public|private|protected|readonly|override)\s+)*$").is_match(param))
            })
            .unwrap_or(false),
        Some(_) => false,
    }
}

/// A Rust file with comments and the contents of string, raw-string and
/// char literals blanked (delimiters and newlines kept), so braces inside
/// them are not code. One pass over the whole text: a string or block
/// comment spanning lines stays masked on every line, a `"` in a `//`
/// comment opens nothing, and `//` inside `r#"…"#` is not a comment. A
/// lifetime (`'a`) is not a char literal and stays.
pub(super) fn mask_rust_code(text: &str) -> String {
    let s: Vec<char> = text.chars().collect();
    let n = s.len();
    let mut out: Vec<char> = s.clone();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for c in &mut out[from.min(n)..to.min(n)] {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let mut i = 0;
    while i < n {
        match s[i] {
            '/' if s.get(i + 1) == Some(&'/') => {
                let mut j = i;
                while j < n && s[j] != '\n' {
                    j += 1;
                }
                blank(&mut out, i, j);
                i = j;
            }
            '/' if s.get(i + 1) == Some(&'*') => {
                let mut j = i + 2;
                let mut depth = 1;
                while j < n && depth > 0 {
                    if s[j] == '/' && s.get(j + 1) == Some(&'*') {
                        depth += 1;
                        j += 2;
                    } else if s[j] == '*' && s.get(j + 1) == Some(&'/') {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            '"' => {
                // `r"…"` / `r#"…"#` / `br"…"` / `cr#"…"#`: no escapes, closed
                // by `"` plus as many `#` as opened it. `b"…"` and `c"…"` are
                // plain strings.
                let mut hashes = 0;
                while i > hashes && s[i - 1 - hashes] == '#' {
                    hashes += 1;
                }
                let ident = |k: usize| s[k].is_alphanumeric() || s[k] == '_';
                let raw = i.checked_sub(hashes + 1).is_some_and(|k| {
                    let k0 = if k > 0 && matches!(s[k - 1], 'b' | 'c') { k - 1 } else { k };
                    s[k] == 'r' && (k0 == 0 || !ident(k0 - 1))
                });
                let mut j = i + 1;
                if raw {
                    while j < n && !(s[j] == '"' && (1..=hashes).all(|h| s.get(j + h) == Some(&'#'))) {
                        j += 1;
                    }
                } else {
                    while j < n && s[j] != '"' {
                        j += if s[j] == '\\' { 2 } else { 1 };
                    }
                }
                blank(&mut out, i + 1, j);
                i = j + 1 + if raw { hashes } else { 0 };
            }
            '\'' => {
                // `'x'` or `'\n'`/`'\u{..}'`; anything else is a lifetime.
                let close = if s.get(i + 1) == Some(&'\\') {
                    (i + 3..(i + 12).min(n)).find(|&k| s[k] == '\'')
                } else if s.get(i + 2) == Some(&'\'') {
                    Some(i + 2)
                } else {
                    None
                };
                match close {
                    Some(k) => {
                        blank(&mut out, i + 1, k);
                        i = k + 1;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::mask_rust_code;

    #[test]
    fn mask_rust_code_blanks_literals_across_lines() {
        let src = "let a = \"x {\n }\";\nlet b = r#\"{ \" // }\"#;\nlet c = c\"{\";\nlet d = cr#\"}\" {\"#; // \"\nlet e = '{'; fn f<'a>() {}\n";
        let masked = mask_rust_code(src);
        assert_eq!(masked.lines().count(), src.lines().count());
        let code: String = masked.chars().filter(|c| matches!(c, '{' | '}')).collect();
        assert_eq!(code, "{}");
    }
}
