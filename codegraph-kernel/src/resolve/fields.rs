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
    ) -> Result<McRes> {
        let segs: Vec<&str> = chain.split('.').collect();
        if segs.len() != 2 || segs[0].is_empty() || segs[1].is_empty() {
            return Ok(McRes::Null);
        }
        let (base, field) = (segs[0], segs[1]);
        let Some(base_type) = self.infer_local_receiver_type(base, r, false)? else {
            return Ok(McRes::Null);
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
            for raw in &source[start..end] {
                let line = strip_line_comments(raw);
                let Some(raw_type) = FIELD_TYPE.capture(&line, field).map(str::to_string) else {
                    continue;
                };
                if raw_type.contains('.') {
                    let pkg = raw_type.split('.').next().unwrap_or("");
                    let in_module = match self.go_module_path.clone() {
                        Some(mod_path) => self
                            .import_mappings(&s.file_path)?
                            .iter()
                            .find(|i| i.local_name == pkg)
                            .is_some_and(|imp| {
                                imp.source == mod_path
                                    || imp.source.starts_with(&format!("{}/", mod_path))
                            }),
                        None => false,
                    };
                    if !in_module {
                        continue;
                    }
                }
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
                match self.resolve_method_on_type(
                    field_type,
                    method,
                    r,
                    0.85,
                    "instance-method",
                    None,
                )? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {}
                }
            }
        }
        Ok(McRes::Null)
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
    ) -> Result<McRes> {
        let Some(source) = self.read_file(&owner.file_path) else {
            return Ok(McRes::Null);
        };
        let tail_re = re!(r"^[\s]*(?:<[^>]*>)?\s*[\[|&]");
        let start = (owner.start_line - 1).max(0) as usize;
        let end = (owner.end_line as usize).min(source.len());
        for raw in &source[start..end] {
            let line = strip_line_comments(raw);
            for (affix, value_type) in TS_FIELD_TYPE_PATTERNS.iter() {
                let Some(m) = affix.find_from(&line, field, 0) else {
                    continue;
                };
                let Some((gs, ge)) = m.group else { continue };
                let m1 = &line[gs..ge];
                if m1.is_empty() {
                    continue;
                }
                if tail_re.is_match(&line[m.end..]) {
                    return Ok(McRes::Null);
                }
                if *value_type {
                    let cls_bindings = self.bindings(&owner.file_path)?;
                    let row = Self::innermost_binding(
                        &cls_bindings,
                        m1,
                        Some(owner.start_line),
                    )
                    .cloned();
                    let holder_id = match &row {
                        Some(b) if b.kind == "import" => {
                            let mut ref2 = r.clone();
                            ref2.file_path = owner.file_path.clone();
                            ref2.line = owner.start_line;
                            ref2.reference_name = m1.to_string();
                            ref2.reference_kind = "references".to_string();
                            match self.resolve_via_import_member(&ref2)? {
                                ViaImport::Hit(c) => Some(c.node.id.clone()),
                                ViaImport::Miss => None,
                                ViaImport::Punt(p) => return Ok(McRes::Punt(p)),
                            }
                        }
                        Some(b) => b.node_id.clone(),
                        None => None,
                    };
                    let holder = self.node_by_opt_id(holder_id.as_deref())?;
                    return Ok(match holder {
                        Some(h) => match self.resolve_object_literal_member(
                            &h,
                            method,
                            r,
                            0.85,
                            "instance-method",
                        )? {
                            Some(c) => McRes::Hit(c),
                            None => McRes::Null,
                        },
                        None => McRes::Null,
                    });
                }
                let type_name = m1.split('.').next_back().unwrap_or("");
                if !type_name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                {
                    return Ok(McRes::Null);
                }
                let mut bsite = r.clone();
                bsite.file_path = owner.file_path.clone();
                bsite.line = owner.start_line;
                return self.match_bound_type_member(
                    m1,
                    method,
                    &bsite,
                );
            }
        }
        Ok(McRes::Null)
    }

    /// matchRustSelfCall (name-matcher.ts): `self.method()` — the method on
    /// the type the call sits inside. The owner is the calling method's
    /// qualified-name prefix; a free fn has no `self`. Exactly one candidate
    /// must belong to that owner — two same-named methods on the same type
    /// is the fabrication this declines instead of.
    pub(super) fn match_rust_self_call(&mut self, method: &str, r: &ResolveRefIn) -> Result<McRes> {
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null);
        }
        let owner = caller.qualified_name[..sep].to_string();
        Ok(match self.resolve_rust_self_member(&owner, method, &caller, &["method"])? {
            Some(node) => McRes::Hit(KCand { node, confidence: 0.9, resolved_by: "qualified-name" }),
            None => McRes::Null,
        })
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
    pub(super) fn match_rust_self_path(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        if r.language != "rust" || !r.reference_name.starts_with("Self::") {
            return Ok(McRes::Null);
        }

        let strip = re!(r"<[^>]*>");
        let name = strip.replace_all(&r.reference_name, "");
        let segs: Vec<&str> = name.split("::").filter(|s| !s.is_empty()).collect();
        if segs[0] != "Self" || (segs.len() != 2 && segs.len() != 3) {
            return Ok(McRes::Null);
        }
        let mut leaf = segs[segs.len() - 1].to_string();
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null);
        }
        // `Self::Assoc::leaf` — the associated type binds in the caller's
        // enclosing `impl` block (`type Assoc = X`), not on the type.
        let mut owner: String = if segs.len() == 2 {
            caller.qualified_name[..sep].to_string()
        } else {
            match self.rust_assoc_type_binding(&caller, segs[1])? {
                Some(bound) => bound,
                None => return Ok(McRes::Null),
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
                return Ok(McRes::Null);
            };
            let Some(sig) = recv.signature.as_deref() else {
                return Ok(McRes::Null);
            };
            let Some(arrow) = sig.rfind("->") else {
                return Ok(McRes::Null);
            };
            let raw_ret = sig[arrow + 2..].trim();
            owner = if raw_ret == "Self" {
                match recv.qualified_name.rfind("::") {
                    Some(rs) => recv.qualified_name[..rs].to_string(),
                    None => return Ok(McRes::Null),
                }
            } else {
                match self.normalize_inferred_type_name(raw_ret)? {
                    Some(t) => t,
                    None => return Ok(McRes::Null),
                }
            };
            leaf = chained[2].to_string();
        } else if !leaf.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Ok(McRes::Null);
        }

        const MEMBER_KINDS: &[&str] = &["method", "enum_member", "constant"];
        let Some(node) = self.resolve_rust_self_member(&owner, &leaf, &caller, MEMBER_KINDS)?
        else {
            return Ok(McRes::Null);
        };
        Ok(McRes::Hit(KCand {
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
    ) -> Result<Option<Arc<KNode>>> {
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
    pub(super) fn match_rust_bare_self(&mut self, r: &ResolveRefIn) -> Result<Option<KCand>> {
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
    ) -> Result<Option<String>> {
        let Some(lines) = self.read_file(&caller.file_path) else {
            return Ok(None);
        };
        // Backward brace scan: the first `{` whose net depth goes negative
        // opens the block enclosing the caller — for a method, the impl.
        let at = |i: i64| -> String {
            if i < 0 {
                String::new()
            } else {
                strip_line_comments(lines.get(i as usize).map(|s| s.as_str()).unwrap_or(""))
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
    ) -> Result<McRes> {
        if field.is_empty() || field.contains('.') {
            return Ok(McRes::Null);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null);
        }
        let Some(owner) = caller.qualified_name[..sep].split("::").last() else {
            return Ok(McRes::Null);
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
            for raw in &source[start..end] {
                let line = thread_regex(&RUST_LINE_COMMENTS).replace_all(raw, "");
                let Some(declared) = RUST_FIELD_TYPE.capture(&line, field) else {
                    continue;
                };
                // The field is declared here; whether or not its type names a
                // project symbol, this owner is the answer — terminal.
                let Some(field_type) = rust_field_type_name(declared) else {
                    return Ok(McRes::Null);
                };
                return self.resolve_method_on_type(
                    &field_type,
                    method,
                    r,
                    0.85,
                    "instance-method",
                    None,
                );
            }
        }
        Ok(McRes::Null)
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
    ) -> Result<McRes> {
        if field.is_empty() || field.contains('.') {
            return Ok(McRes::Null);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(McRes::Null);
        };
        let Some(sep) = caller.qualified_name.rfind("::") else {
            return Ok(McRes::Null);
        };
        if sep == 0 {
            return Ok(McRes::Null); // not inside a class
        }
        let owner = caller.qualified_name[..sep]
            .split("::")
            .last()
            .unwrap_or("");
        if owner.is_empty() {
            return Ok(McRes::Null);
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
    ) -> Result<McRes> {
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
            let Some(source) = self.read_file(&cls.file_path) else {
                continue;
            };
            let start = (cls.start_line - 1).max(0) as usize;
            let end = (cls.end_line as usize).min(source.len());
            for raw in &source[start..end] {
                let line = strip_line_comments(raw);
                for (affix, value_type) in TS_FIELD_TYPE_PATTERNS.iter() {
                    let Some(m) = affix.find_from(&line, field, 0) else {
                        continue;
                    };
                    let Some((gs, ge)) = m.group else { continue };
                    let m1 = &line[gs..ge];
                    if m1.is_empty() {
                        continue;
                    }
                    // No tail_re check — that guard is `boundOwner &&` in TS.
                    if *value_type {
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
                                return Ok(McRes::Hit(hit));
                            }
                        }
                        return Ok(McRes::Null);
                    }
                    // `ns.Mailer` → `Mailer`; a primitive or builtin names no
                    // project type.
                    let type_name = m1.split('.').next_back().unwrap_or("");
                    if !type_name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_uppercase())
                    {
                        return Ok(McRes::Null);
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
                        let call_dirs: Vec<&str> = {
                            let mut v: Vec<&str> = r.file_path.split('/').collect();
                            v.pop();
                            v
                        };
                        let shared = |fp: &str| shared_dir_prefix(&call_dirs, fp);
                        let max_shared =
                            declared.iter().map(|n| shared(&n.file_path)).max().unwrap_or(0);
                        let nearest: Vec<&Arc<KNode>> = declared
                            .iter()
                            .filter(|n| shared(&n.file_path) == max_shared)
                            .collect();
                        if nearest.len() > 1 {
                            // TS tiebreaks by localeCompare, which this port
                            // cannot model exactly — let the TS spine pick.
                            return Ok(McRes::Punt("mc-tfield-ambig"));
                        }
                        return Ok(McRes::Hit(KCand {
                            node: nearest[0].clone(),
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
            }
        }
        Ok(McRes::Null)
    }
}
