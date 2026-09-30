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
        let Some(base_type) = self.infer_local_receiver_type(base, r, false)? else {
            return Ok(None);
        };
        // `\bFIELD\s+\*?\[?\]?TYPE`
        static FIELD_TYPE: LazyLock<Affix> =
            LazyLock::new(|| Affix::new("", r"\s+\*?\[?\]?([A-Za-z_][A-Za-z0-9_.]*)", true, false, false));
        let structs: Vec<Arc<KNode>> = prefer_call_site_file(
            self.nodes_by_name(&base_type)?
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "struct" | "class") && n.language == "go"
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
        let Some(mod_path) = self.go_module_path.clone() else {
            return Ok(None);
        };
        let source = self
            .import_mappings(file_path)?
            .iter()
            .find(|i| i.local_name == pkg)
            .map(|imp| imp.source.clone());
        Ok(match source {
            Some(src) if src == mod_path => Some(String::new()),
            Some(src) if src.starts_with(&format!("{}/", mod_path)) => Some(src[mod_path.len() + 1..].to_string()),
            _ => None,
        })
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
        if named.is_empty() {
            return self.resolve_method_on_type(type_name, method, r, 0.85, "instance-method", None);
        }
        let in_pkg: Vec<Arc<KNode>> =
            named.into_iter().filter(|m| pos_dirname(&m.file_path) == pkg_dir).collect();
        if in_pkg.is_empty() || (r.reference_kind == "function_ref" && in_pkg.len() != 1) {
            return Ok(None);
        }
        let ordered = prefer_call_site_file(in_pkg, &r.file_path);
        Ok(Some(KCand { node: ordered[0].clone(), confidence: 0.85, resolved_by: "instance-method" }))
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
        let Some(source) = self.read_file(&owner.file_path) else { return Ok(None) };
        let start = (owner.start_line - 1).max(0) as usize;
        let end = (owner.end_line as usize).min(source.len());
        let mut methods: Option<Vec<Arc<KNode>>> = None;
        for (i, raw) in source.get(start..end).unwrap_or_default().iter().enumerate() {
            let line_no = (start + i + 1) as i64;
            let line = strip_line_comments(raw);
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
                    if !ts_declares_field(methods, affix, field, &line, line_no, at, &line[gs..ge]) {
                        continue;
                    }
                    let rest = &line[m.end..];
                    return Ok(Some(TsFieldDecl {
                        ty: line[gs..ge].to_string(),
                        value_type: *value_type,
                        typed_collection: guard1_tail_re().is_match(rest),
                        nullable: nullable_union_tail_re().is_match(rest),
                    }));
                }
            }
        }
        Ok(None)
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
        for cls in &owners {
            let Some(decl) = self.ts_field_decl(cls, field)? else { continue };
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
                // The field's own file says which type it means: a local
                // declaration or an import pins the method's file.
                if let Some(pin) = self.ts_field_type_file(cls, m1, type_name, r)? {
                    let Some(type_file) = pin else { return Ok(None) };
                    // Overload signatures repeat the method in that file;
                    // the first stands for them, as the tie-break below does.
                    let Some(pinned) = declared.iter().find(|n| n.file_path == type_file) else {
                        return Ok(None);
                    };
                    return Ok(Some(KCand {
                        node: pinned.clone(),
                        confidence: 0.85,
                        resolved_by: "instance-method",
                    }));
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
                None,
            );
        }
        Ok(None)
    }

    /// The file declaring the type a TS field names, read from the owner
    /// class's file: `Some(Some(file))` when it declares the type itself or
    /// imports it from a project file, `Some(None)` when the import comes
    /// from outside the repository (no project method is meant), `None`
    /// when that file says nothing about the name.
    fn ts_field_type_file(
        &mut self,
        owner: &KNode,
        declared_type: &str,
        type_name: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<String>>> {
        let head = declared_type.split('.').next().unwrap_or("");
        if head == type_name
            && self.nodes_in_file(&owner.file_path)?.iter().any(|n| {
                n.name == type_name && matches!(n.kind.as_str(), "class" | "interface")
            })
        {
            return Ok(Some(Some(owner.file_path.clone())));
        }
        let Some(imp) = self
            .import_mappings(&owner.file_path)?
            .iter()
            .find(|m| m.local_name == head)
            .cloned()
        else {
            return Ok(None);
        };
        if self.is_external_import(&imp.source, &owner.language, &owner.file_path) {
            return Ok(Some(None));
        }
        let mut type_ref = r.clone().naming(declared_type, "references");
        type_ref.from_node_id = owner.id.clone();
        type_ref.file_path = owner.file_path.clone();
        type_ref.language = owner.language.clone();
        type_ref.line = owner.start_line;
        type_ref.column = owner.start_column;
        Ok(self
            .resolve_via_import(&type_ref)?
            .filter(|c| matches!(c.node.kind.as_str(), "class" | "interface"))
            .map(|c| Some(c.node.file_path.clone())))
    }
}

/// A TypeScript class-field declaration found by `ts_field_decl`.
pub(super) struct TsFieldDecl {
    /// The declared type text (`Mailer`, `ns.Mailer`, the `typeof` operand).
    pub(super) ty: String,
    /// `field: typeof Ns` — a value type, not a class.
    pub(super) value_type: bool,
    /// The type continues into `[]`, `|` or `&` (GUARD1_TAIL_RE).
    pub(super) typed_collection: bool,
    /// The only union members after the type are `undefined`/`null`.
    pub(super) nullable: bool,
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
    line: &str,
    line_no: i64,
    at: usize,
    ty: &str,
) -> bool {
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
            .and_then(|sig| affix.find_from(sig, field, 0).and_then(|hit| hit.group.map(|(s, e)| &sig[s..e] == ty)))
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
