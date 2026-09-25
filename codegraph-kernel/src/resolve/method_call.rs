//! matchMethodCall and the bound-receiver claim.

use super::*;

impl KernelResolver {
    /// The br:factory tail of matchBoundReceiverCall's ESM arm — receiver's
    /// initializer ends in a call/new-factory expression whose return type
    /// carries the method.
    pub(super) fn esm_factory_tail(
        &mut self,
        binding: &KBinding,
        root: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Result<McRes> {
        let value = self.node_by_opt_id(binding.node_id.as_deref())?;
        // `\b(?:const|let|var)\s+ROOT\s*=` and its `(=…)` capture form.
        static DECLARES: LazyLock<Affix> =
            LazyLock::new(|| Affix::new(r"(?-u:\b)(?:const|let|var)\s+", r"\s*=", false, false, false));
        static DECLARED_INIT: LazyLock<Affix> =
            LazyLock::new(|| Affix::new(r"(?-u:\b)(?:const|let|var)\s+", r"\s*(=[\s\S]+)", false, false, false));
        let lines = self.read_file(&r.file_path);
        let declares_value = lines
            .as_ref()
            .and_then(|ls| ls.get((binding.line - 1) as usize))
            .is_some_and(|l| DECLARES.is_match(l, root));
        let declaration = if declares_value {
            lines.as_ref().map(|ls| {
                let lo = (binding.line - 1).max(0) as usize;
                let hi = ((binding.line + 2) as usize).min(ls.len());
                ls[lo..hi].join("\n")
            })
        } else {
            None
        };
        let signature = match value.as_ref().and_then(|v| v.signature.clone()) {
            Some(s) => Some(s),
            None => declaration
                .as_deref()
                .and_then(|d| DECLARED_INIT.capture(d, root).map(str::to_string)),
        };
        let init = signature.unwrap_or_default();
        // parensEnd — UTF-16 unit index one past the ')' that closes the '('
        // at `from - 1`, or -1 when it never closes (JS string indexing).
        let parens_end = |s: &str, from: usize| -> i64 {
            let mut depth = 1i64;
            let mut i = from;
            let mut units = 0usize;
            for ch in s.chars() {
                let start = units;
                units += ch.len_utf16();
                if start < from || depth == 0 {
                    continue;
                }
                if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    depth -= 1;
                }
                i = units;
            }
            if depth != 0 {
                -1
            } else {
                i as i64
            }
        };
        // ^[ \t]*(?:;|\r?\n(?![ \t]*[.(\[?])) — the lookahead is emulated:
        // `;` always ends the initializer; a newline does unless a chained
        // `.`/`(`/`[`/`?` follows its leading whitespace.
        let ends_initializer = |init_s: &str, call_end: i64| -> bool {
            if call_end < 0 {
                return false;
            }
            let tail = Self::js_slice(init_s, call_end as usize);
            if tail.is_empty() {
                return true;
            }
            let t = tail.trim_start_matches([' ', '\t']);
            if t.starts_with(';') {
                return true;
            }
            if let Some(rest) = t.strip_prefix("\r\n").or_else(|| t.strip_prefix('\n')) {
                return !rest
                    .trim_start_matches([' ', '\t'])
                    .chars()
                    .next()
                    .is_some_and(|c| matches!(c, '.' | '(' | '[' | '?'));
            }
            false
        };
        let awaited_re = re!(r"^=\s*await(?-u:\b)");
        let awaited = awaited_re.is_match(&init);
        let mut callee_name: Option<String> = None;
        let mut owner_name: Option<String> = None;
        let factory_re =
            re!(r"^=\s*(await\s+)?([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(");
        if let Some(fm) = factory_re.captures(&init) {
            let end = parens_end(&init, Self::utf16_len(&init[..fm.get(0).unwrap().end()]));
            if ends_initializer(&init, end) {
                callee_name = fm.get(2).map(|g| g.as_str().to_string());
            }
        }
        if callee_name.is_none() {
            let ctor_re = re!(r"^=\s*(?:await\s+)?new\s+([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(");
            if let Some(cm) = ctor_re.captures(&init) {
                let ctor_end =
                    parens_end(&init, Self::utf16_len(&init[..cm.get(0).unwrap().end()]));
                if ctor_end >= 0 {
                    let member_re = re!(r"^\s*\.\s*([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(");
                    let tail = Self::js_slice(&init, ctor_end as usize);
                    if let Some(mm) = member_re.captures(tail) {
                        let m_end = parens_end(
                            &init,
                            ctor_end as usize
                                + Self::utf16_len(&tail[..mm.get(0).unwrap().end()]),
                        );
                        if ends_initializer(&init, m_end) {
                            owner_name = cm.get(1).map(|g| g.as_str().to_string());
                            callee_name = mm.get(1).map(|g| g.as_str().to_string());
                        }
                    }
                }
            }
        }
        let Some(callee_name) = callee_name else {
            return Ok(McRes::Null);
        };
        let bindings = self.bindings(&r.file_path)?;
        let callee: Option<Arc<KNode>> = if let Some(owner_name) = owner_name {
            let owner_binding =
                Self::innermost_binding(&bindings, &owner_name, Some(binding.line)).cloned();
            let owner_id = match &owner_binding {
                Some(b) if b.kind == "import" => {
                    let mut ref2 = r.clone();
                    ref2.line = binding.line;
                    ref2.reference_name = owner_name.clone();
                    ref2.reference_kind = "references".to_string();
                    match self.resolve_via_import(&ref2)? {
                        Some(c) => Some(c.node.id.clone()),
                        None => None,
                    }
                }
                Some(b) => b.node_id.clone(),
                None => None,
            };
            let owner = self.node_by_opt_id(owner_id.as_deref())?;
            let Some(owner) = owner else { return Ok(McRes::Null) };
            if !matches!(owner.kind.as_str(), "class" | "interface" | "component") {
                return Ok(McRes::Null);
            }
            self.nodes_by_qualified_name(&format!(
                "{}::{}",
                owner.qualified_name, callee_name
            ))?
            .iter()
            .find(|n| n.kind == "method" && n.file_path == owner.file_path)
            .cloned()
        } else {
            let factory_binding =
                Self::innermost_binding(&bindings, &callee_name, Some(binding.line))
                    .cloned();
            let callee_id = match &factory_binding {
                Some(b) if b.kind == "import" => {
                    let mut ref2 = r.clone();
                    ref2.line = binding.line;
                    ref2.reference_name = callee_name.clone();
                    match self.resolve_via_import(&ref2)? {
                        Some(c) => Some(c.node.id.clone()),
                        None => None,
                    }
                }
                Some(b) => b.node_id.clone(),
                None => None,
            };
            self.node_by_opt_id(callee_id.as_deref())?
        };
        let Some(callee) = callee else { return Ok(McRes::Null) };
        let ret_re = re!(r"\)\s*:\s*([A-Za-z0-9_$]+(?:<[A-Za-z0-9_$]+>)?)\s*$");
        let return_type = callee.return_type.clone().or_else(|| {
            callee
                .signature
                .as_deref()
                .and_then(|s| ret_re.captures(s).map(|c| c[1].to_string()))
        });
        // `!returnType` — an empty annotation/returnType fails the same way.
        let Some(return_type) = return_type.filter(|t| !t.is_empty()) else {
            return Ok(McRes::Null);
        };
        let promise_re = re!(r"^Promise<(.+)>$");
        let ty = if awaited {
            promise_re
                .replace(&return_type, "$1")
                .to_string()
        } else {
            return_type
        };
        let mut site = r.clone();
        site.file_path = callee.file_path.clone();
        site.line = callee.start_line;
        self.match_bound_type_member(&ty, method, &site)
    }

    /// Cheap raw-source gate for inferEsmAwaitedCallType — the awaited arm can
    /// only engage when the file binds `receiver` in an `= await x(` shape.
    /// True → punt (the arm needs sanitized scope parsing); false → provable
    /// null, continue natively.
    pub(super) fn mc_await_gate(&mut self, receiver: &str, r: &ResolveRefIn) -> Result<bool> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        // `\b(?:const|let|var)\s+RECV\s*=\s*await\s+x\s*\(`
        static AWAITED: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new(r"(?-u:\b)(?:const|let|var)\s+", r"\s*=\s*await\s+[A-Za-z0-9_$]+\s*\(", false, false, false)
        });
        Ok(AWAITED.any_line(lines.iter().map(String::as_str), receiver))
    }

    /// Cheap gate for inferIterationReceiver — kotlin/go only, fires only
    /// when its declaration preconditions can hold; tree-sitter stays in TS.
    pub(super) fn mc_iteration_gate(&mut self, receiver: &str, r: &ResolveRefIn) -> Result<bool> {
        if r.language != "kotlin" && r.language != "go" {
            return Ok(false);
        }
        let bindings = self.bindings(&r.file_path)?;
        let mut best: Option<&KBinding> = None;
        for b in bindings.iter() {
            if b.name != receiver || b.scope_start > r.line || b.scope_end < r.line {
                continue;
            }
            if best.is_none_or(|x| b.scope_end - b.scope_start < x.scope_end - x.scope_start) {
                best = Some(b);
            }
        }
        let declaration = match best {
            Some(b) => self
                .read_file(&r.file_path)
                .and_then(|ls| ls.get((b.line - 1) as usize).cloned()),
            None => None,
        };
        if r.language == "go" {
            // `!declaration?.includes('range')` → null → provable miss.
            return Ok(declaration.is_some_and(|d| d.contains("range")));
        }
        // kotlin: `receiver !== 'it' && !declaration?.includes('->')` → null.
        Ok(receiver == "it" || declaration.is_some_and(|d| d.contains("->")))
    }

    /// The un-receiver-typed c/cpp field fallback: every same-language
    /// `field` node carrying `member`'s name, resolved only when the set is
    /// a singleton. Field nodes exist only for callable function-pointer
    /// members, so any singleton is a callable member by construction.
    pub(super) fn unique_field_candidate(
        &mut self,
        member: &str,
        r: &ResolveRefIn,
    ) -> Result<Option<KCand>> {
        let fields: Vec<Arc<KNode>> = self
            .nodes_by_name(member)?
            .iter()
            .filter(|n| {
                n.kind == "field" && same_language_family(&n.language, &r.language)
            })
            .cloned()
            .collect();
        Ok(if fields.len() == 1 {
            Some(KCand {
                node: fields.into_iter().next().unwrap(),
                confidence: 0.7,
                resolved_by: "field-call",
            })
        } else {
            None
        })
    }

    /// The shared head of matchMethodCall's two arms: the PHP
    /// `$this->prop.method` declared-type path and the Rust `Self::item` path
    /// (both settle here), then the receiver/method split —
    /// `dotMatch || colonMatch || luaColonMatch || rDollarMatch`. Every shape
    /// but `::` is a receiver whose type the local declaration can name
    /// (`inferable`).
    pub(super) fn method_call_shape(&mut self, r: &ResolveRefIn) -> Result<McShape> {
        // PHP `$this->prop->method()` — exclusive declared-type path.
        if r.language == "php" {
            let re = re!(r"^(this->[A-Za-z0-9_]+)\.([A-Za-z0-9_]+)$");
            if let Some(m) = re.captures(&r.reference_name) {
                let receiver = m[1].to_string();
                let php_method = m[2].to_string();
                let Some(inferred) =
                    self.infer_local_receiver_type(&receiver, r, false)?
                else {
                    return Ok(McShape::Done(McRes::Null));
                };
                let fqn = self.imported_fqn_of(&inferred, r)?;
                return Ok(McShape::Done(self.resolve_method_on_type(
                    &inferred,
                    &php_method,
                    r,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                )?));
            }
        }

        // Rust `Self::item` — associated-item path binding `Self` to the
        // caller's impl owner. Before the `!matched` bail so deeper paths
        // (`Self::Assoc::m`) reach it; a miss falls through like TS.
        if let McRes::Hit(c) = self.match_rust_self_path(r)? {
            return Ok(McShape::Done(McRes::Hit(c)));
        }

        let dot_re = re!(r"^([A-Za-z0-9_.]+)\.([A-Za-z0-9_]+:?(?:[A-Za-z0-9_]+:)*)$");
        let mut dot_match = dot_re.captures(&r.reference_name);
        if dot_match.is_none() && r.language == "cpp" {
            let op_re = re!(r"^([A-Za-z0-9_.]+)\.(operator[^A-Za-z0-9_\s.]+)$");
            dot_match = op_re.captures(&r.reference_name);
        }
        let colon_match = re!(r"^([A-Za-z0-9_]+)::([A-Za-z0-9_]+)$")
            .captures(&r.reference_name);
        // `match = dotMatch || colonMatch || luaColonMatch || rDollarMatch`;
        // every shape but `::` is a receiver whose type the local declaration
        // can name (`inferableReceiver`).
        let (receiver, method, inferable) = if let Some(m) = dot_match.as_ref() {
            (m[1].to_string(), m[2].to_string(), true)
        } else if let Some(m) = colon_match.as_ref() {
            (m[1].to_string(), m[2].to_string(), false)
        } else if let Some((recv, method)) =
            self.lua_colon_shape(r)?.or(self.r_dollar_shape(r)?)
        {
            (recv, method, true)
        } else {
            return Ok(McShape::Done(McRes::Null));
        };
        Ok(McShape::Parsed { receiver, method, inferable, dotted: dot_match.is_some() })
    }

    /// matchMethodCall(ref, context, requireReceiverEvidence=true) — the
    /// boundReceiver evidence slice. Punt points: php instanceof guards,
    /// go/kotlin iteration constructs, ESM awaited inference, and every
    /// member-miss that would walk live supertype edges.
    pub(super) fn match_method_call(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        let (object_or_class, method_name, inferable, dotted) = match self.method_call_shape(r)? {
            McShape::Parsed { receiver, method, inferable, dotted } => (receiver, method, inferable, dotted),
            McShape::Done(res) => return Ok(res),
        };

        let bindings = self.bindings(&r.file_path)?;
        let binding =
            Self::innermost_binding(&bindings, &object_or_class, Some(r.line)).cloned();

        if inferable {
            // inferGuardedReceiver is php-only and needs a tree parse — punt
            // when its cheap precondition can hold, else provable null.
            if r.language == "php" {
                let guarded = self
                    .read_file(&r.file_path)
                    .is_some_and(|ls| ls.iter().any(|l| l.contains("instanceof")));
                if guarded {
                    return Ok(McRes::Punt("mc-guarded"));
                }
            }
            let mut site = r.clone();
            if let Some(b) = &binding {
                if b.kind != "import" {
                    site.line = b.line;
                    if let Some(nid) = &b.node_id {
                        site.from_node_id = nid.clone();
                    }
                }
            }
            // TS passes requireReceiverEvidence (=true) as preserveQualifiedName
            // to both inferrers here — qualified names stay intact.
            let mut inferred = if r.language == "cpp" {
                self.infer_cpp_receiver_type(&object_or_class, r, 0, true)?
            } else {
                self.infer_local_receiver_type(&object_or_class, &site, true)?
            };
            if inferred.is_none() && r.language == "go" {
                match self.match_go_factory_receiver(&object_or_class, &method_name, r)? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {}
                }
            }
            if inferred.is_none() {
                if self.mc_iteration_gate(&object_or_class, r)? {
                    return Ok(McRes::Punt("mc-iteration"));
                }
                if is_esm_family(&r.language)
                    && self.mc_await_gate(&object_or_class, r)?
                {
                    return Ok(McRes::Punt("mc-await"));
                }
                // `recv->fp(...)` / `x.fp(...)` with an unrecoverable
                // receiver type: the member is still provable when exactly
                // one same-language callable `field` carries its name.
                // Ambiguous or absent → fall through to the name arms (and
                // ultimately unresolved), never a guess.
                if r.language == "c" || r.language == "cpp" {
                    if let Some(hit) = self.unique_field_candidate(&method_name, r)? {
                        return Ok(McRes::Hit(hit));
                    }
                }
            }
            if let Some(t) = inferred.take() {
                let mut bsite = r.clone();
                if let Some(b) = &binding {
                    bsite.line = b.line;
                }
                return self.match_bound_type_member(&t, &method_name, &bsite);
            }
        }

        // Go 2-hop field chain — exclusive branch.
        if r.language == "go" && dotted && object_or_class.contains('.') {
            return self.match_go_field_chain_call(&object_or_class, &method_name, r);
        }
        // rust field/self arms: rust is unmigrated — dead.
        // this.field arms: `this.` receivers are excluded by the claim gate —
        // dead inside boundReceiver.

        if (r.language == "java" || r.language == "kotlin") && dotted {
            if let Some(res) = self.jvm_field_receiver(&object_or_class, &method_name, r)? {
                return Ok(res);
            }
        }

        // mc-literal — OBJECT_LITERAL_LANGUAGES is the ESM set.
        if dotted
            && !object_or_class.contains('.')
            && is_object_literal_language(&r.language)
        {
            let holders: Vec<Arc<KNode>> = prefer_call_site_file(
                self.nodes_by_name(&object_or_class)?
                    .iter()
                    .filter(|n| {
                        matches!(n.kind.as_str(), "constant" | "variable")
                            && n.file_path == r.file_path
                    })
                    .cloned()
                    .collect(),
                &r.file_path,
            );
            for holder in holders {
                // `binding?.nodeId !== holder.id` — an absent binding or
                // node_id skips every holder, exactly like TS.
                let bound_id = binding.as_ref().and_then(|b| b.node_id.as_deref());
                if bound_id != Some(holder.id.as_str()) {
                    continue;
                }
                if let Some(hit) = self.resolve_object_literal_member(
                    &holder,
                    &method_name,
                    r,
                    0.85,
                    "instance-method",
                )? {
                    return Ok(McRes::Hit(hit));
                }
            }
        }

        // Strategy 1 — under requireReceiverEvidence the first candidate
        // passing the binding filter returns matchBoundTypeMember(receiver).
        let class_candidates = prefer_call_site_file(
            self.nodes_by_name(&object_or_class)?
                .iter()
                .cloned()
                .collect(),
            &r.file_path,
        );
        for c in &class_candidates {
            if let Some(b) = &binding {
                if b.node_id.as_deref() != Some(c.id.as_str()) {
                    continue;
                }
            }
            return self.match_bound_type_member(&object_or_class, &method_name, r);
        }
        Ok(McRes::Null)
    }

    /// matchMethodCall(ref, context, requireReceiverEvidence=false) — the
    /// member-tail arm of matchReference, reached by refs the boundReceiver
    /// claim never took (`this.`/`self.` roots, non-call kinds, deep
    /// receivers). Same pattern prelude and unconditional sub-arms as the
    /// bound path; the evidence-gated arms (mc-guarded, gofactory, iteration,
    /// the btm terminal) never run here — inferred types terminal-match via
    /// rmot, and the name-similarity strategies close the arm.
    pub(super) fn match_method_call_free(&mut self, r: &ResolveRefIn) -> Result<McRes> {
        // PHP `$this->prop.method` takes the declared-type path in both
        // modes (inside method_call_shape).
        let (object_or_class, method_name, inferable, dotted) = match self.method_call_shape(r)? {
            McShape::Parsed { receiver, method, inferable, dotted } => (receiver, method, inferable, dotted),
            McShape::Done(res) => return Ok(res),
        };

        if inferable {
            // No binding anchor under requireReceiverEvidence=false — the
            // inferrers run at the ref's own site with qualified names
            // normalized (preserveQualifiedName=false).
            let inferred = if r.language == "cpp" {
                self.infer_cpp_receiver_type(&object_or_class, r, 0, false)?
            } else {
                self.infer_local_receiver_type(&object_or_class, r, false)?
            };
            // mc-guarded/gofactory/iteration are evidence-gated in TS and
            // never run here; mc-await still does.
            if inferred.is_none()
                && is_esm_family(&r.language)
                && self.mc_await_gate(&object_or_class, r)?
            {
                return Ok(McRes::Punt("mc-await"));
            }
            // Same unique-field fallback as the bound arm — `recv->fp(...)`
            // proves its field member when exactly one exists.
            if inferred.is_none() && (r.language == "c" || r.language == "cpp") {
                if let Some(hit) = self.unique_field_candidate(&method_name, r)? {
                    return Ok(McRes::Hit(hit));
                }
            }
            if let Some(t) = inferred {
                // Java/Kotlin: the file's import pins WHICH same-named class.
                let fqn = if r.language == "java" || r.language == "kotlin" {
                    self.imported_fqn_of(&t, r)?
                } else {
                    None
                };
                match self.resolve_method_on_type(
                    &t,
                    &method_name,
                    r,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                )? {
                    McRes::Hit(c) => return Ok(McRes::Hit(c)),
                    McRes::Punt(p) => return Ok(McRes::Punt(p)),
                    McRes::Null => {
                        // A known builtin/primitive receiver is external when
                        // it has no project method — TS returns null here
                        // rather than letting Strategy 3 guess an unrelated
                        // `get`/`split`/`has`.
                        if is_esm_family(&r.language)
                            && (JS_BUILT_INS.contains(t.as_str())
                                || TS_PRIMITIVE_TYPES.contains(t.as_str()))
                        {
                            return Ok(McRes::Null);
                        }
                    }
                }
            }
        }

        // Go 2-hop field chain — EXCLUSIVE for chained Go receivers.
        if r.language == "go" && dotted && object_or_class.contains('.') {
            return self.match_go_field_chain_call(&object_or_class, &method_name, r);
        }
        // Rust `self.<field>.<method>` — EXCLUSIVE: validated field-type
        // inference or nothing (a null is the ref's verdict, not a fall-
        // through — the bare-name strategies fabricate this shape).
        if r.language == "rust" && dotted && object_or_class.starts_with("self.") {
            return self.match_rust_self_field_call(
                &object_or_class["self.".len()..],
                &method_name,
                r,
            );
        }
        // Rust `self.<method>` — EXCLUSIVE for the same reason: the owner
        // is the enclosing impl type on the caller's qualified name.
        if r.language == "rust" && dotted && object_or_class == "self" {
            return self.match_rust_self_call(&method_name, r);
        }

        // TS/JS `this.field.method` — EXCLUSIVE; the field's declared type
        // off the enclosing class, validated by rmot, or nothing.
        if matches!(r.language.as_str(), "typescript" | "javascript" | "tsx" | "jsx")
            && dotted
            && object_or_class.starts_with("this.")
        {
            return self.match_ts_this_field_call(
                &object_or_class["this.".len()..],
                &method_name,
                r,
            );
        }

        // Java/Kotlin field receiver inference — non-exclusive (a miss still
        // reaches the name strategies, exactly like TS).
        if (r.language == "java" || r.language == "kotlin") && dotted {
            if let Some(res) = self.jvm_field_receiver(&object_or_class, &method_name, r)? {
                return Ok(res);
            }
        }

        // Object-literal namespace receiver — same-file const/variable
        // holders; under requireReceiverEvidence=false there is no binding
        // filter — every holder gets its object-literal member scan.
        if dotted
            && !object_or_class.contains('.')
            && is_object_literal_language(&r.language)
        {
            let holders = prefer_call_site_file(
                self.nodes_by_name(&object_or_class)?
                    .iter()
                    .filter(|n| {
                        matches!(n.kind.as_str(), "constant" | "variable")
                            && n.file_path == r.file_path
                    })
                    .cloned()
                    .collect(),
                &r.file_path,
            );
            for holder in &holders {
                if let Some(hit) = self.resolve_object_literal_member(
                    holder,
                    &method_name,
                    r,
                    0.85,
                    "instance-method",
                )? {
                    return Ok(McRes::Hit(hit));
                }
            }
        }

        // Strategy 1 — direct class-name match, call site's file first.
        if let Some(hit) = self.class_method_scan(&object_or_class, &method_name, r, 0.85, "qualified-name")? {
            return Ok(McRes::Hit(hit));
        }

        // Strategy 2 — capitalized receiver (`permissionEngine` →
        // `PermissionEngine`) against the same class scan.
        let mut cap_bytes = object_or_class.clone().into_bytes();
        if let Some(b) = cap_bytes.first_mut() {
            *b = b.to_ascii_uppercase();
        }
        let capitalized = String::from_utf8(cap_bytes).unwrap_or_default();
        if capitalized != object_or_class {
            if let Some(hit) = self.class_method_scan(&capitalized, &method_name, r, 0.8, "instance-method")? {
                return Ok(McRes::Hit(hit));
            }
        }

        // Strategy 3 — methods by name across the codebase, scored by
        // receiver-word overlap with the containing class name.
        if !method_name.is_empty() {
            let method_candidates = self.nodes_by_name(&method_name)?;
            // Ubiquitous-method ceiling: bail before the O(K) work.
            if method_candidates.len() as i64 > self.ambiguous_ceiling {
                return Ok(McRes::Null);
            }
            let methods: Vec<Arc<KNode>> = method_candidates
                .iter()
                .filter(|n| n.kind == "method" && n.name == method_name)
                .cloned()
                .collect();
            let same_lang: Vec<Arc<KNode>> = methods
                .iter()
                .filter(|m| m.language == r.language)
                .cloned()
                .collect();
            let target = if !same_lang.is_empty() {
                &same_lang
            } else {
                &methods
            };
            if target.len() == 1 && target[0].language == r.language {
                return Ok(McRes::Hit(KCand {
                    node: target[0].clone(),
                    confidence: 0.7,
                    resolved_by: "instance-method",
                }));
            }
            if target.len() > 1 {
                let receiver_words = split_camel_case(&object_or_class);
                // Same-file candidates first, so a score tie resolves to the
                // call site's own file (`score > bestScore` keeps first seen).
                let ordered = prefer_call_site_file(target.clone(), &r.file_path);
                let mut best: Option<Arc<KNode>> = None;
                let mut best_score = 0i64;
                for m in &ordered {
                    let class_words = split_camel_case(&m.qualified_name);
                    let mut score = receiver_words
                        .iter()
                        .filter(|w| {
                            class_words
                                .iter()
                                .any(|cw| cw.eq_ignore_ascii_case(w))
                        })
                        .count() as i64;
                    if m.language == r.language {
                        score += 1;
                    }
                    if score > best_score {
                        best_score = score;
                        best = Some(m.clone());
                    }
                }
                if let Some(bm) = best {
                    if best_score >= 2 {
                        return Ok(McRes::Hit(KCand {
                            node: bm,
                            confidence: 0.65,
                            resolved_by: "instance-method",
                        }));
                    }
                }
            }
        }
        Ok(McRes::Null)
    }

    /// Java/Kotlin field receiver inference — non-exclusive: `Some` settles
    /// the ref, `None` lets the name strategies run, exactly like TS.
    pub(super) fn jvm_field_receiver(&mut self, receiver: &str, method: &str, r: &ResolveRefIn) -> Result<Option<McRes>> {
        let Some(inferred) = self.infer_java_field_receiver_type(receiver, r)? else {
            return Ok(None);
        };
        let fqn = self.imported_fqn_of(&inferred, r)?;
        Ok(match self.resolve_method_on_type(&inferred, method, r, 0.9, "instance-method", fqn.as_deref())? {
            McRes::Null => None,
            res => Some(res),
        })
    }

    /// matchMethodCall's class scan (Strategies 1 and 2): a same-language
    /// class, struct, union or interface named `class_name`, call site's file
    /// first, whose file holds a method `method` qualified under it.
    pub(super) fn class_method_scan(
        &mut self,
        class_name: &str,
        method: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
    ) -> Result<Option<KCand>> {
        let candidates = prefer_call_site_file(self.nodes_by_name(class_name)?.iter().cloned().collect(), &r.file_path);
        for c in &candidates {
            if !matches!(c.kind.as_str(), "class" | "struct" | "union" | "interface") || c.language != r.language {
                continue;
            }
            let in_file = self.nodes_in_file(&c.file_path)?;
            if let Some(mn) = in_file
                .iter()
                .find(|n| n.kind == "method" && n.name == method && n.qualified_name.contains(c.name.as_str()))
            {
                return Ok(Some(KCand { node: mn.clone(), confidence, resolved_by }));
            }
        }
        Ok(None)
    }

    /// matchMethodCall's `luaColonMatch` — Lua/Luau method calls use a single
    /// colon (`lg:log`); recognized so receiver-type inference applies to
    /// them (#1108). `(receiver, method)`; `None` for every other language.
    pub(super) fn lua_colon_shape(&mut self, r: &ResolveRefIn) -> Result<Option<(String, String)>> {
        if r.language != "lua" && r.language != "luau" {
            return Ok(None);
        }
        let re = re!(r"^([A-Za-z0-9_.]+):([A-Za-z0-9_]+)$");
        Ok(re
            .captures(&r.reference_name)
            .map(|c| (c[1].to_string(), c[2].to_string())))
    }

    /// matchMethodCall's `rDollarMatch` — R member access is `lg$log`.
    pub(super) fn r_dollar_shape(&mut self, r: &ResolveRefIn) -> Result<Option<(String, String)>> {
        if r.language != "r" {
            return Ok(None);
        }
        let re = re!(r"^([A-Za-z0-9_.]+)\$([A-Za-z0-9_]+)$");
        Ok(re
            .captures(&r.reference_name)
            .map(|c| (c[1].to_string(), c[2].to_string())))
    }

    pub(super) fn bound_receiver_claim(&mut self, r: &ResolveRefIn) -> Result<BoundClaim> {
        // `^(.+)\.([\w$]+)$` is guaranteed by the gate — split at the LAST dot.
        let dot = r.reference_name.rfind('.').unwrap();
        let receiver = &r.reference_name[..dot];
        let method = &r.reference_name[dot + 1..];
        let root = receiver.split('.').next().unwrap_or(receiver);
        let bindings = self.bindings(&r.file_path)?;
        let binding = Self::innermost_binding(&bindings, root, Some(r.line)).cloned();

        if !is_esm_family(&r.language) {
            // `binding?.kind === 'import' && !phpVariable` → br:import;
            // anything else is matchMethodCall receiver inference (source).
            let php_variable =
                r.language == "php" && self.ref_line_starts_with_dollar(r);
            if binding.as_ref().is_some_and(|b| b.kind == "import") && !php_variable {
                // java/kotlin bound-type resolution runs before the import
                // descent: an owner means btm owns the ref (or refuses a
                // deeper receiver); a miss falls through to br:import.
                if r.language == "java" || r.language == "kotlin" {
                    match self.resolve_bound_type(root, r, 0)? {
                        BtRes::Owner(_) => {
                            return Ok(if receiver == root {
                                Self::mc_to_claim(
                                    self.match_bound_type_member(root, method, r)?,
                                )
                            } else {
                                BoundClaim::Refused
                            });
                        }
                        BtRes::Null => {}
                        BtRes::Punt(p) => return Ok(BoundClaim::Punt(p)),
                    }
                }
                return Ok(match self.resolve_via_import_member(r)? {
                    ViaImport::Hit(c) => {
                        if matches!(
                            c.node.kind.as_str(),
                            "function" | "method" | "class" | "component"
                        ) {
                            BoundClaim::Hit(c)
                        } else {
                            BoundClaim::Refused
                        }
                    }
                    ViaImport::Miss => BoundClaim::Refused,
                    ViaImport::Punt(reason) => BoundClaim::Punt(reason),
                });
            }
            return Ok(Self::mc_to_claim(probe!(r, "brc:match_method_call", self.match_method_call(r)?)));
        }

        if binding.as_ref().is_some_and(|b| b.kind == "import") {
            // The import resolver descends one member — a deeper receiver
            // must not mistake the first member for the call.
            if receiver.contains('.') {
                return Ok(BoundClaim::Refused);
            }
            return Ok(match self.resolve_via_import_member(r)? {
                ViaImport::Hit(c) => {
                    if matches!(
                        c.node.kind.as_str(),
                        "function" | "method" | "class" | "component"
                    ) || ((c.node.kind == "constant" || c.node.kind == "variable")
                        && method == "getState")
                    {
                        BoundClaim::Hit(c)
                    } else {
                        BoundClaim::Refused
                    }
                }
                ViaImport::Miss => BoundClaim::Refused,
                ViaImport::Punt(reason) => BoundClaim::Punt(reason),
            });
        }
        let Some(binding) = binding else {
            return Ok(BoundClaim::Refused);
        };
        if receiver.contains('.') {
            let parts: Vec<&str> = receiver.split('.').collect();
            if parts.len() != 2 {
                return Ok(BoundClaim::Refused);
            }
            // br:fieldinfer — root's declared type anchored at the binding
            // site (preserve qualified names), then the field on that owner.
            let mut site = r.clone();
            site.line = binding.line;
            if let Some(nid) = &binding.node_id {
                site.from_node_id = nid.clone();
            }
            let Some(ty) = self.infer_local_receiver_type(root, &site, true)? else {
                return Ok(BoundClaim::Refused);
            };
            let type_binding = Self::innermost_binding(
                &bindings,
                ty.split('.').next().unwrap_or(&ty),
                Some(binding.line),
            )
            .cloned();
            let owner_id = match &type_binding {
                Some(b) if b.kind == "import" => {
                    // `{ ...ref, referenceName: type, 'references' }` — the
                    // ORIGINAL ref, not the anchored site.
                    let mut ref2 = r.clone();
                    ref2.reference_name = ty.clone();
                    ref2.reference_kind = "references".to_string();
                    let via = if ty.contains('.') {
                        match self.resolve_via_import_member(&ref2)? {
                            ViaImport::Hit(c) => Some(c),
                            ViaImport::Miss => None,
                            ViaImport::Punt(p) => return Ok(BoundClaim::Punt(p)),
                        }
                    } else {
                        self.resolve_via_import(&ref2)?
                    };
                    via.map(|c| c.node.id.clone())
                }
                Some(b) => b.node_id.clone(),
                None => None,
            };
            let owner = self.node_by_opt_id(owner_id.as_deref())?;
            return Ok(match owner {
                Some(o)
                    if matches!(
                        o.kind.as_str(),
                        "class" | "interface" | "component" | "type_alias"
                    ) =>
                {
                    Self::mc_to_claim(
                        self.match_ts_field_call_bound(o.as_ref(), parts[1], method, r)?,
                    )
                }
                _ => BoundClaim::Refused,
            });
        }
        match probe!(r, "brc:match_method_call", self.match_method_call(r)?) {
            McRes::Hit(c) => return Ok(BoundClaim::Hit(c)),
            McRes::Punt(p) => return Ok(BoundClaim::Punt(p)),
            McRes::Null => {}
        }
        if binding.kind == "param" {
            return Ok(BoundClaim::Refused);
        }
        Ok(Self::mc_to_claim(
            self.esm_factory_tail(&binding, root, method, r)?,
        ))
    }
}
