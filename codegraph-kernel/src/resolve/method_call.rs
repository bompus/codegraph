//! matchMethodCall and the bound-receiver claim.

use super::ts_known::TsBracketed;
use super::*;

impl KernelResolver {
    pub(super) fn csharp_extension_recursion_proven(
        &mut self,
        method: &KNode,
        r: &ResolveRefIn,
    ) -> Res<bool> {
        if r.language != "csharp" || method.id != r.from_node_id {
            return Ok(false);
        }
        let Some(source) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        let Some(tree) = self.parsed_tree(&source, r) else {
            return Ok(false);
        };
        let Some(line) = source.get((r.line - 1).max(0) as usize) else {
            return Ok(false);
        };
        let point = tree_sitter::Point::new(
            (r.line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, r.column.max(0) as usize),
        );
        let text = |node: tree_sitter::Node<'_>| &source.text()[node.start_byte()..node.end_byte()];
        let mut current = tree.root_node().descendant_for_point_range(point, point);
        let mut receiver = None;
        let mut argument_count = None;
        let mut lambda = None;
        let mut declaration = None;
        while let Some(node) = current {
            if receiver.is_none() && node.kind() == "invocation_expression" {
                if let Some(member) = node.child_by_field_name("function") {
                    if member.kind() == "member_access_expression"
                        && member
                            .child_by_field_name("name")
                            .is_some_and(|name| text(name) == method.name)
                    {
                        receiver = member
                            .child_by_field_name("expression")
                            .filter(|node| node.kind() == "identifier")
                            .map(text);
                        argument_count = node
                            .child_by_field_name("arguments")
                            .map(|arguments| arguments.named_child_count());
                    }
                }
            }
            if lambda.is_none() && node.kind() == "lambda_expression" {
                let Some(parameters) = node.child_by_field_name("parameters") else {
                    return Ok(false);
                };
                let Some(receiver) = receiver else {
                    return Ok(false);
                };
                if parameters.kind() != "implicit_parameter" || text(parameters) != receiver {
                    return Ok(false);
                }
                lambda = Some(node);
            }
            if node.kind() == "method_declaration" {
                declaration = Some(node);
                break;
            }
            current = node.parent();
        }
        let (Some(lambda), Some(declaration)) = (lambda, declaration) else {
            return Ok(false);
        };
        let Some(parameters) = declaration.child_by_field_name("parameters") else {
            return Ok(false);
        };
        let Some(first) = super::iteration::named_children(parameters)
            .into_iter()
            .find(|node| node.kind() == "parameter")
        else {
            return Ok(false);
        };
        if !re!(r"^\s*this\b").is_match(text(first)) {
            return Ok(false);
        }
        let Some(extension_type) = first.child_by_field_name("type") else {
            return Ok(false);
        };
        let Some(argument) = lambda.parent().filter(|node| node.kind() == "argument") else {
            return Ok(false);
        };
        let Some(arguments) = argument
            .parent()
            .filter(|node| node.kind() == "argument_list")
        else {
            return Ok(false);
        };
        if arguments.named_child_count() != 1 {
            return Ok(false);
        }
        let Some(select) = arguments
            .parent()
            .filter(|node| node.kind() == "invocation_expression")
        else {
            return Ok(false);
        };
        let Some(member) = select
            .child_by_field_name("function")
            .filter(|node| node.kind() == "member_access_expression")
        else {
            return Ok(false);
        };
        if !member
            .child_by_field_name("name")
            .is_some_and(|node| text(node) == "Select")
        {
            return Ok(false);
        }
        if self.nodes_by_name("Select")?.iter().any(|node| {
            node.language == "csharp" && matches!(node.kind.as_str(), "method" | "function")
        }) {
            return Ok(false);
        }
        let Some(collection) = member
            .child_by_field_name("expression")
            .filter(|node| node.kind() == "member_access_expression")
        else {
            return Ok(false);
        };
        let Some(root) = collection
            .child_by_field_name("expression")
            .filter(|node| node.kind() == "identifier")
        else {
            return Ok(false);
        };
        let Some(property) = collection.child_by_field_name("name") else {
            return Ok(false);
        };
        let mut root_type = None;
        let mut scope = select.parent();
        while let Some(node) = scope {
            if node.kind() == "if_statement"
                && node
                    .child_by_field_name("consequence")
                    .is_some_and(|branch| {
                        branch.start_byte() <= select.start_byte()
                            && select.end_byte() <= branch.end_byte()
                    })
            {
                let mut patterns: Vec<_> = node
                    .child_by_field_name("condition")
                    .filter(|condition| condition.kind() == "is_pattern_expression")
                    .map(super::iteration::named_children)
                    .unwrap_or_default();
                while let Some(pattern) = patterns.pop() {
                    if pattern.kind() == "declaration_pattern"
                        && pattern
                            .child_by_field_name("name")
                            .is_some_and(|name| text(name) == text(root))
                    {
                        root_type = pattern.child_by_field_name("type").map(text);
                        break;
                    }
                }
                if root_type.is_some() {
                    break;
                }
            }
            if node.kind() == "method_declaration" {
                break;
            }
            scope = node.parent();
        }
        let Some(root_type) = root_type else {
            return Ok(false);
        };
        let Some(owner) = self.resolve_bound_type(root_type, r, 0)? else {
            return Ok(false);
        };
        if !self.language_type_visible(&owner, r)? {
            return Ok(false);
        }
        let fields: Vec<_> = self
            .nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, text(property)))?
            .iter()
            .filter(|node| {
                node.file_path == owner.file_path
                    && matches!(node.kind.as_str(), "property" | "field")
            })
            .cloned()
            .collect();
        let [field] = fields.as_slice() else {
            return Ok(false);
        };
        let Some(signature) = &field.signature else {
            return Ok(false);
        };
        let Some(element) = re!(r"^(?:System\.Collections\.Generic\.)?(IEnumerable|IReadOnlyList|IList|List|IReadOnlyCollection|ICollection)\s*<\s*([A-Za-z_][\w.]*)\s*>").captures(signature) else { return Ok(false) };
        if self
            .nodes_by_name(&element[1])?
            .iter()
            .any(|node| node.language == "csharp" && is_class_like(&node.kind))
        {
            return Ok(false);
        }
        let method_site = r.clone().at(method);
        let field_site = r.clone().at(field);
        let Some(expected) = self.resolve_bound_type(text(extension_type), &method_site, 0)? else {
            return Ok(false);
        };
        let Some(actual) = self.resolve_bound_type(&element[2], &field_site, 0)? else {
            return Ok(false);
        };
        if actual.id != expected.id
            || !self.language_type_visible(&expected, &method_site)?
            || !self.language_type_visible(&actual, &field_site)?
        {
            return Ok(false);
        }
        let Some(argument_count) = argument_count else {
            return Ok(false);
        };
        let mut owners = VecDeque::from([actual]);
        let mut seen = HashSet::new();
        while let Some(owner) = owners.pop_front() {
            if !seen.insert(owner.id.clone()) {
                continue;
            }
            if seen.len() > 60 {
                return Ok(false);
            }
            let instances = self
                .nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, method.name))?;
            for instance in instances.iter().filter(|node| {
                node.language == "csharp"
                    && node.kind == "method"
                    && node.file_path == owner.file_path
            }) {
                if self.csharp_instance_accepts_arity(
                    instance,
                    owner.kind == "interface",
                    argument_count,
                    r,
                ) != Some(false)
                {
                    return Ok(false);
                }
            }
            owners.extend(self.supertype_nodes(&owner.id)?);
        }
        Ok(true)
    }

    fn csharp_instance_accepts_arity(
        &mut self,
        method: &KNode,
        interface: bool,
        count: usize,
        r: &ResolveRefIn,
    ) -> Option<bool> {
        let source = self.read_file(&method.file_path)?;
        let site = r.clone().at(method);
        let tree = self.parsed_tree(&source, &site)?;
        let line = source.get((method.start_line - 1).max(0) as usize)?;
        let point = tree_sitter::Point::new(
            (method.start_line - 1).max(0) as usize,
            super::names::js_unit_to_byte(line, method.start_column.max(0) as usize),
        );
        let mut current = tree.root_node().descendant_for_point_range(point, point);
        while let Some(node) = current {
            if node.kind() == "method_declaration" {
                let children = super::iteration::named_children(node);
                let modifiers: Vec<_> = children
                    .iter()
                    .filter(|child| child.kind() == "modifier")
                    .map(|child| &source.text()[child.start_byte()..child.end_byte()])
                    .collect();
                if modifiers.contains(&"static")
                    || modifiers.contains(&"private")
                    || (!interface
                        && !modifiers.contains(&"public")
                        && !modifiers.contains(&"internal"))
                {
                    return Some(false);
                }
                let parameters = node.child_by_field_name("parameters")?;
                let parameters: Vec<_> = super::iteration::named_children(parameters)
                    .into_iter()
                    .filter(|parameter| parameter.kind() == "parameter")
                    .collect();
                let mut required = 0;
                let mut variadic = false;
                for parameter in &parameters {
                    let mut cursor = parameter.walk();
                    let optional = parameter
                        .children(&mut cursor)
                        .any(|child| child.kind() == "=");
                    let params = super::iteration::named_children(*parameter)
                        .iter()
                        .any(|child| {
                            child.kind() == "modifier"
                                && &source.text()[child.start_byte()..child.end_byte()] == "params"
                        });
                    variadic |= params;
                    if !optional && !params {
                        required += 1;
                    }
                }
                return Some(required <= count && (variadic || count <= parameters.len()));
            }
            current = node.parent();
        }
        None
    }

    /// The br:factory tail of matchBoundReceiverCall's ESM arm — receiver's
    /// initializer ends in a call/new-factory expression whose return type
    /// carries the method.
    pub(super) fn esm_factory_tail(
        &mut self,
        binding: &KBinding,
        root: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let parsed = self.factory_initializer(binding, root, r)?;
        let (awaited, callee_name, owner_name) = (parsed.awaited, parsed.callee.clone(), parsed.owner.clone());
        let Some(callee_name) = callee_name else {
            return Ok(None);
        };
        let bindings = self.bindings(&r.file_path)?;
        let callee: Option<Arc<KNode>> = if let Some(owner_name) = owner_name {
            let owner_binding =
                innermost_binding(&bindings, &owner_name, Some(binding.line)).cloned();
            let owner_id = self.binding_target_id(owner_binding.as_ref(), |s| {
                let mut ref2 = r.clone().naming(&owner_name, "references");
                ref2.line = binding.line;
                s.resolve_via_import(&ref2)
            })?;
            let owner = self.node_by_opt_id(owner_id.as_deref())?;
            let Some(owner) = owner else { return Ok(None) };
            if !matches!(owner.kind.as_str(), "class" | "interface" | "component") {
                return Ok(None);
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
                innermost_binding(&bindings, &callee_name, Some(binding.line))
                    .cloned();
            let callee_id = self.binding_target_id(factory_binding.as_ref(), |s| {
                let mut ref2 = r.clone();
                ref2.line = binding.line;
                ref2.reference_name = callee_name.clone();
                s.resolve_via_import(&ref2)
            })?;
            self.node_by_opt_id(callee_id.as_deref())?
        };
        let Some(callee) = callee else { return Ok(None) };
        let ret_re = re!(r"\)\s*:\s*([A-Za-z0-9_$]+(?:<[A-Za-z0-9_$]+>)?)\s*$");
        let return_type = callee.return_type.clone().or_else(|| {
            callee
                .signature
                .as_deref()
                .and_then(|s| ret_re.captures(s).map(|c| c[1].to_string()))
        });
        // `!returnType` — an empty annotation/returnType fails the same way.
        let Some(return_type) = return_type.filter(|t| !t.is_empty()) else {
            // An `async` factory hands back a Promise, not the binding it
            // returns; only `await` would unwrap it.
            if awaited || callee.is_async || callee.kind != "function" {
                return Ok(None);
            }
            return self.returned_binding_member(&callee, method, r);
        };
        let promise_re = re!(r"^Promise<(.+)>$");
        let ty = if awaited {
            promise_re
                .replace(&return_type, "$1")
                .to_string()
        } else {
            return_type
        };
        self.match_bound_type_member(&ty, method, &r.clone().at(&callee))
    }

    /// An unannotated factory whose only `return` hands back one binding —
    /// `export function useI18n() { return i18n; }` over
    /// `const i18n: I18nClass = new I18nClass()`: the binding's declared or
    /// constructed type carries the method. Any other body shape declines.
    fn returned_binding_member(&mut self, callee: &KNode, method: &str, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(lines) = self.read_file(&callee.file_path) else {
            return Ok(None);
        };
        let lo = (callee.start_line - 1).max(0) as usize;
        let hi = (callee.end_line.max(callee.start_line) as usize).min(lines.len());
        if lo >= hi {
            return Ok(None);
        }
        let return_kw = re!(r"(?-u:\b)return(?-u:\b)");
        // `return x;` as a line of its own or after the `{` of a one-line body.
        let return_ident = re!(r"(?:^|[{;])\s*return\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(;|\})?\s*\}?\s*(?://.*)?$");
        // Without a `;` the statement runs on when the next line continues
        // the expression (`return plain\n  .clone()`), as JavaScript's ASI does.
        let continues = re!(r"^\s*(?:[.?(\[+\-*/%&|^<>=,:`]|(?:as|satisfies|instanceof|in)(?-u:\b))");
        let own_lines = own_return_lines(&lines[lo..hi].join("\n"));
        let mut returned: Option<(String, i64)> = None;
        for (i, line) in lines[lo..hi].iter().enumerate() {
            // A `return` inside a nested callback returns from the callback.
            if !return_kw.is_match(line) || !own_lines.contains(&i) {
                continue;
            }
            let Some(m) = return_ident.captures(line) else {
                return Ok(None);
            };
            if m.get(2).is_none() {
                let next = lines[lo + i + 1..hi].iter().find(|l| !l.trim().is_empty());
                if next.is_some_and(|l| continues.is_match(l)) {
                    return Ok(None);
                }
            }
            if returned.is_some() {
                return Ok(None);
            }
            returned = Some((m[1].to_string(), (lo + i + 1) as i64));
        }
        let Some((ident, line)) = returned else {
            return Ok(None);
        };
        let bindings = self.bindings(&callee.file_path)?;
        let Some(binding) = innermost_binding(&bindings, &ident, Some(line)).cloned() else {
            return Ok(None);
        };
        if binding.kind != "decl" && binding.kind != "local" {
            return Ok(None);
        }
        let mut site = r.clone().at(callee).naming(&format!("{ident}.{method}"), "calls");
        site.language = callee.language.clone();
        site.line = binding.line;
        site.from_node_id = binding.node_id.clone().unwrap_or_else(|| callee.id.clone());
        let Some(ty) = self.infer_local_receiver_type(&ident, &site, true)? else {
            return Ok(None);
        };
        if TS_PRIMITIVE_TYPES.contains(ty.as_str()) {
            return Ok(None);
        }
        self.match_bound_type_member(&ty, method, &site)
    }

    /// The factory call a binding's initializer ends in — `= f(…)` or
    /// `= new C(…).m(…)`, optionally awaited — parsed once per binding: every
    /// ref through the same binding reads the same declaration.
    fn factory_initializer(&mut self, binding: &KBinding, root: &str, r: &ResolveRefIn) -> Res<Rc<FactoryInit>> {
        let key = (r.file_path.clone(), binding.line, root.to_string(), binding.node_id.clone());
        if let Some(hit) = self.factory_init_memo.get(&key) {
            return Ok(hit.clone());
        }
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
                let hi = (lo + FACTORY_DECLARATION_LINES).min(ls.len());
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
        let awaited_re = re!(r"^=\s*await(?-u:\b)");
        let awaited = awaited_re.is_match(&init);
        let mut callee_name: Option<String> = None;
        let mut owner_name: Option<String> = None;
        let factory_re =
            re!(r"^=\s*(await\s+)?([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(");
        if let Some(fm) = factory_re.captures(&init) {
            let end = parens_end(&init, utf16_len(&init[..fm.get(0).unwrap().end()]));
            if ends_initializer(&init, end) {
                callee_name = fm.get(2).map(|g| g.as_str().to_string());
            }
        }
        if callee_name.is_none() {
            let ctor_re = re!(r"^=\s*(?:await\s+)?new\s+([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(");
            if let Some(cm) = ctor_re.captures(&init) {
                let ctor_end =
                    parens_end(&init, utf16_len(&init[..cm.get(0).unwrap().end()]));
                if ctor_end >= 0 {
                    let member_re = re!(r"^\s*\.\s*([A-Za-z0-9_$]+)\s*(?:<[^>]+>)?\s*\(");
                    let tail = js_slice(&init, ctor_end as usize);
                    if let Some(mm) = member_re.captures(tail) {
                        let m_end = parens_end(
                            &init,
                            ctor_end as usize
                                + utf16_len(&tail[..mm.get(0).unwrap().end()]),
                        );
                        if ends_initializer(&init, m_end) {
                            owner_name = cm.get(1).map(|g| g.as_str().to_string());
                            callee_name = mm.get(1).map(|g| g.as_str().to_string());
                        }
                    }
                }
            }
        }
        let parsed = Rc::new(FactoryInit { awaited, callee: callee_name, owner: owner_name });
        self.factory_init_memo.insert(key, parsed.clone());
        Ok(parsed)
    }

    /// inferIterationReceiver's parse precondition — kotlin/go only, true
    /// only when the receiver's declaration can come from a range or lambda.
    pub(super) fn mc_iteration_gate(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "kotlin" && r.language != "go" {
            return Ok(false);
        }
        let bindings = self.bindings(&r.file_path)?;
        let best = innermost_binding(&bindings, receiver, Some(r.line));
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
    ) -> Res<Option<KCand>> {
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
    pub(super) fn method_call_shape(&mut self, r: &ResolveRefIn) -> Res<McShape> {
        // PHP `$this->prop->method()` — exclusive declared-type path.
        if r.language == "php" {
            let re = re!(r"^(this->[A-Za-z0-9_]+)\.([A-Za-z0-9_]+)$");
            if let Some(m) = re.captures(&r.reference_name) {
                let receiver = m[1].to_string();
                let php_method = m[2].to_string();
                let Some(inferred) =
                    self.infer_local_receiver_type(&receiver, r, false)?
                else {
                    return Ok(McShape::Done(None));
                };
                // The declared type as the file names it, through its
                // namespace or `use` (`use Lib\Store as Cache;` makes `Cache`
                // `Lib\Store`): that one type, not every same-named class.
                if let Some(qn) = self.php_declared_type_qn(&inferred, &r.file_path)? {
                    return Ok(McShape::Done(self.resolve_method_on_qualified_type(
                        &qn,
                        &php_method,
                        r,
                        0.9,
                        "instance-method",
                    )?));
                }
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
        if let Some(result) = self.rust_simple_self_call(r)? {
            return Ok(McShape::Done(result));
        }
        if let Some(c) = self.match_rust_self_path(r)? {
            return Ok(McShape::Done(Some(c)));
        }

        // `this.#field.method` keeps its ES private field (#1987).
        // A Go composite literal's written type carries its type arguments
        // (`Box[int]{}.Size` arrives as `Box[int].Size`, `lib.List[int]{}.Len`
        // as `lib.List[int].Len`); an indexed receiver arrives bare, so a
        // bracketed receiver here is always an instantiation.
        if r.language == "go" {
            if let Some(m) = re!(r"^([A-Za-z0-9_.]+\[[^\]]*\])\.([A-Za-z0-9_]+)$").captures(&r.reference_name) {
                return Ok(McShape::Parsed {
                    receiver: m[1].to_string(),
                    method: m[2].to_string(),
                    inferable: true,
                    dotted: true,
                });
            }
        }
        // An element of a field — `this.listeners[type].push()`, emitted as
        // `this.listeners[].push`: the element type the field's declaration
        // writes, or nothing (matchMethodCall's elementOf arm).
        if is_js_family(&r.language) {
            if let Some(m) = re!(r"^this\.(#?[A-Za-z0-9_$]+)\[\]\.([A-Za-z0-9_$]+)$").captures(&r.reference_name) {
                return Ok(McShape::Done(self.match_ts_this_field_call(&m[1], &m[2], r, true)?));
            }
        }
        let dot_re = re!(r"^((?:this\.#)?[A-Za-z0-9_.]+)\.([A-Za-z0-9_]+:?(?:[A-Za-z0-9_]+:)*)$");
        // A JS/TS method name may carry `$`, which `\w` doesn't: an RxJS-style
        // `this.organizationService.organizations$()`, vscode's RPC
        // `this._proxy.$acceptModelChanged()`. The receiver part keeps no `$`,
        // so `this.destroy$.next()` stays out of name guessing.
        let mut dot_match = if is_js_family(&r.language) {
            re!(r"^((?:this\.#)?[A-Za-z0-9_.]+)\.([A-Za-z0-9_$]+)$").captures(&r.reference_name)
        } else {
            None
        }
        .or_else(|| dot_re.captures(&r.reference_name));
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
            return Ok(McShape::Done(None));
        };
        Ok(McShape::Parsed { receiver, method, inferable, dotted: dot_match.is_some() })
    }

    /// matchMethodCall(ref, context, requireReceiverEvidence=true) — the
    /// boundReceiver evidence slice. A member miss whose supertype walk needs
    /// edges the db may not hold yet is a miss.
    pub(super) fn match_method_call(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let (object_or_class, method_name, inferable, dotted) = match probe!(r, "mc:shape", self.method_call_shape(r)?) {
            McShape::Parsed { receiver, method, inferable, dotted } => (receiver, method, inferable, dotted),
            McShape::Done(res) => return Ok(res),
        };

        // A call on one of the browser's own objects — `window.open()`,
        // `localStorage.getItem()`, `location.reload()`, `history.back()` —
        // that the file neither binds nor imports is the browser's: no project
        // method is it, whatever the method's name or a class's resemblance to
        // the receiver (#2907). Ahead of the receiver-type reader, whose
        // backward scan has no scope and would take a `history: X` member line
        // for this name.
        if r.reference_kind == "calls" && dotted && is_js_family(&r.language)
            && self.is_browser_host_global(&object_or_class, r)? {
            return Ok(None);
        }
        if r.language == "cpp" && self.cpp_external_receiver(&object_or_class, r)? { return Ok(None); }
        // A Java call on a value a type holds in a static field,
        // `Consts.HELPER.go()`: typed by the field's declaration, or no edge
        // for a library type's field.
        if r.language == "java" && dotted {
            if let Some(hit) = self.match_java_static_field_call(&object_or_class, &method_name, r)? {
                return Ok(hit);
            }
        }
        if matches!(r.language.as_str(), "java" | "kotlin") {
            if let Some(hit) = self.enum_constant_call(&object_or_class, &method_name, r)? { return Ok(Some(hit)); }
        }
        let binding = self.receiver_binding(&object_or_class, r)?;
        if r.language == "python" {
            if let Some(b) = binding.as_ref().filter(|b| b.kind == "local" && b.line > r.line) {
                if b.scope_start >= self.enclosing_scope_start_line(&r.file_path, &r.language, r.line)? {
                    return Ok(None);
                }
            }
        }


        if inferable {
            if r.language == "go" {
                // `Box[int]`/`lib.List[int]` is the written type of the
                // composite literal the call selects on — the type's method
                // answers, or nothing does; no local inference applies.
                if let Some(base) = object_or_class.strip_suffix(']').and_then(|s| s.split('[').next()) {
                    if re!(r"^\*?[A-Za-z_]\w*(\.[A-Za-z_]\w*)?$").is_match(base) {
                        return self.go_method_on_declared_type(base, &r.file_path, &method_name, r);
                    }
                }
                if let Some(verdict) = self.go_asserted_member(&object_or_class, &method_name, r)? {
                    if verdict.is_none() { self.go_mc_terminal = true; }
                    return Ok(verdict);
                }
                // A package-level value's spec types its method calls
                // (`var level = Level(2)` → `level.Label()` on Level).
                if !object_or_class.contains('.') {
                    if let Some(verdict) = self.match_go_package_value_call(&object_or_class, &method_name, r)? {
                        return Ok(verdict);
                    }
                    // The declared type settles the call when it is readable:
                    // a literal (`[]Gomega`) or a type no project package
                    // declares methods on means no project method answers.
                    if let Some(decl) = self.go_local_declaration_at(
                        object_or_class.clone(), r.file_path.clone(), r.line, r.column)?
                    {
                        if let Some(text) = &decl.declared_type {
                            if Self::go_named_type(text).is_none()
                                || matches!(self.go_written_type(text, r)?, Some(super::go_asserted::GoAsserted::Outside))
                            {
                                self.go_mc_terminal = true;
                                return Ok(None);
                            }
                        }
                    }
                }
            }
            // A PHP `instanceof` branch narrows the receiver inside its body.
            if let Some(t) = probe!(r, "mc:guarded", self.infer_guarded_receiver(&object_or_class, r)?) {
                return self.match_bound_type_member(&t, &method_name, r);
            }
            // A lambda parameter hides the same-named field or outer local
            // the binding rows name (lambda parameters have no row). Only a
            // Kotlin `x.let { v -> … }` or a typed Java `(Foo f) ->` still
            // types it.
            if let Some(b) = binding.as_ref().filter(|b| b.kind != "import") {
                if (r.language == "java" || r.language == "kotlin")
                    && self.lambda_param_shadows(&object_or_class, r, b.line)?
                {
                    if r.language == "kotlin" {
                        if let Some(hit) = self.iteration_receiver_in_tree(&object_or_class, r)? {
                            return self.match_bound_type_member(&hit.ty, &method_name, &hit.site);
                        }
                    }
                    // `(Foo f) -> f.bar()` declares its parameter's type.
                    if r.language == "java" {
                        if let Some(ty) = self.java_lambda_param_type(&object_or_class, r)? {
                            return self.match_bound_type_member(&ty, &method_name, r);
                        }
                    }
                    return Ok(None);
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
            let mut go_type_file: Option<String> = None;
            let mut inferred = if matches!(r.language.as_str(), "java" | "kotlin" | "csharp") && self.explicit_this_receiver(&object_or_class, &method_name, r) {
                self.infer_declared_member_receiver_type(&format!("this.{object_or_class}"), r)?
            } else if r.language == "cpp" {
                self.infer_cpp_receiver_type(&object_or_class, r, 0, true)?
            } else if r.language == "python" && object_or_class.contains('.') {
                None
            } else {
                let local = if r.language == "go" && !object_or_class.contains('.') {
                    match self.go_local_receiver_written(&object_or_class, r)? {
                        Some((t, f)) => { go_type_file = Some(f); Some(t) }
                        None => None,
                    }
                } else {
                    None
                };
                probe!(r, "mc:infer-local", match local {
                    Some(_) => Ok(local),
                    None => self.infer_local_receiver_type(&object_or_class, &site, true),
                }?)
            };
            // A JVM `var name = Method()` takes the method's return type
            // (`var userRegistry = make()` is a Registry), which no text
            // pattern in local_receiver_type_patterns reads.
            if inferred.is_none() && matches!(r.language.as_str(), "java" | "csharp" | "kotlin") {
                inferred = self.jvm_var_initializer_type(&object_or_class, r)?;
            }
            if r.language == "objc" && inferred.is_none() {
                inferred = self.objc_member_receiver_type(&object_or_class, r)?;
            }
            if inferred.is_none() && r.language == "python" && !object_or_class.contains('.') {
                if let Some(hit) = self.python_global_receiver_call(&object_or_class, &method_name, r)? { return Ok(Some(hit)); }
            }
            if inferred.is_none() { if let Some(verdict)=self.python_fixture_member(&object_or_class,&method_name,r)? { return Ok(verdict); } }
if inferred.is_none() && r.language == "dart" {
    inferred = self.infer_dart_field_receiver_type(&object_or_class, r)?;
}
            if inferred.is_none() { inferred = self.infer_declared_member_receiver_type(&object_or_class, r)?; }
            if inferred.is_none() && r.language == "go" {
                if let Some(c) = self.match_go_factory_receiver(&object_or_class, &method_name, r)? {
                    return Ok(Some(c));
                }
            }
            if inferred.is_none() {
                if let Some(hit) = probe!(r, "mc:iteration", self.infer_iteration_receiver(&object_or_class, r)?) {
                    return self.match_bound_type_member(&hit.ty, &method_name, &hit.site);
                }
                if is_esm_family(&r.language) {
                    if let Some(a) =
                        probe!(r, "mc:await", self.infer_esm_awaited_call_type(&object_or_class, r)?)
                    {
                        return match a.name {
                            Some(t) if !TS_PRIMITIVE_TYPES.contains(t.as_str()) => {
                                let mut tsite = r.clone();
                                tsite.file_path = a.file_path;
                                tsite.line = a.line;
                                self.match_bound_type_member(&t, &method_name, &tsite)
                            }
                            _ => Ok(None),
                        };
                    }
                }
                // `recv->fp(...)` / `x.fp(...)` with an unrecoverable
                // receiver type: the member is still provable when exactly
                // one same-language callable `field` carries its name.
                // Ambiguous or absent → fall through to the name arms (and
                // ultimately unresolved), never a guess.
                if r.language == "c" || r.language == "cpp" {
                    if let Some(hit) = self.unique_field_candidate(&method_name, r)? {
                        return Ok(Some(hit));
                    }
                }
            }
            if let Some(t) = inferred.take() {
                let Some(mut t) = self.inferred_member_type_bound(&t, r)? else { return Ok(None) };
                if is_esm_family(&r.language) && t.ends_with(']') {
                    match self.ts_bracketed_type(&t, &method_name, r)? {
                        TsBracketed::Settled(hit) => return Ok(hit),
                        TsBracketed::Type(name) => t = name,
                    }
                }
                if is_esm_family(&r.language) {
                    let name = t.split('<').next().unwrap_or(&t).trim().to_string();
                    let simple = name.rsplit('.').next().unwrap_or(&name).to_string();
                    match self.ts_typed_local_known(&name, &simple, &method_name, r)? {
                        TsTypedLocal::NoProjectMethod => return Ok(None),
                        TsTypedLocal::Hit(hit) => return Ok(Some(hit)),
                        TsTypedLocal::Pass(_) => {}
                    }
                }
                let mut bsite = r.clone();
                if let Some(b) = &binding {
                    bsite.line = b.line;
                }
                // A Go type the written source read names the file that
                // knows its package (`model := query.Query().Session` —
                // Session lives in query/, not the call's).
                if let Some(f) = &go_type_file {
                    bsite.file_path = f.clone();
                }
                let member = self.match_bound_type_member(&t, &method_name, &bsite)?;
                if member.is_none() && r.language == "go" {
                    // A Go standard method name the declared type lacks is no
                    // other type's (`err.Error()`, `context.Done()`); any
                    // other name goes on to name matching, as one on a
                    // receiver of unread type does.
                    if super::call_shape::is_std_method("go", &method_name) {
                        self.go_mc_terminal = true;
                        return Ok(None);
                    }
                } else if member.is_none() && r.language == "kotlin" {
                    // An extension function is no member of the receiver's class, and in a
                    // package its qualified name does not say which class it extends.
                    let hit = super::kotlin_receiver::KotlinReceiverHit { ty: t.clone(), site: bsite, heuristic: false };
                    return Ok(self.kotlin_extension_member(&hit, &method_name, r)?
                        .map(|node| KCand { node, confidence: 0.9, resolved_by: "instance-method" }));
                }
                return Ok(member);
            }
        }

        // Go 2-hop field chain — exclusive branch; its miss is terminal.
        if r.language == "go" && dotted && object_or_class.contains('.') {
            let hit = self.match_go_field_chain_call(&object_or_class, &method_name, r)?;
            if hit.is_none() { self.go_mc_terminal = true; }
            return Ok(hit);
        }
        // rust field/self arms: rust is unmigrated — dead.
        // this.field arms: `this.` receivers are excluded by the claim gate —
        // dead inside boundReceiver.

        if (r.language == "java" || r.language == "kotlin") && dotted {
            if let Some(hit) = self.jvm_field_receiver(&object_or_class, &method_name, r)? {
                return Ok(Some(hit));
            }
        }

        // mc-literal — OBJECT_LITERAL_LANGUAGES is the ESM set.
        if dotted && !object_or_class.contains('.') && is_object_literal_language(&r.language) {
            match self.resolve_object_path_member(&object_or_class,&method_name,r,None)? {
                js_objects_upstream::ObjectPathMatch::Found(hit) => return Ok(Some(hit)),
                js_objects_upstream::ObjectPathMatch::Refused => return Ok(None),
                js_objects_upstream::ObjectPathMatch::NoHolder => {}
            }
        }

        // Strategy 1 — under requireReceiverEvidence the first candidate
        // passing the binding filter returns matchBoundTypeMember(receiver).
        let owners = self.cpp_translation_unit_owners(self.nodes_by_name(&object_or_class)?.iter().cloned().collect(), r)?;
        let class_candidates = prefer_call_site_file(owners, &r.file_path);
        for c in &class_candidates {
            if let Some(b) = &binding {
                if b.node_id.as_deref() != Some(c.id.as_str()) {
                    continue;
                }
            }
            return self.match_bound_type_member(&object_or_class, &method_name, r);
        }
        // `chunk, _ := s.Recv()` where `s` is a parameter of a generic type
        // from a package the project does not index: the method's result is
        // that type argument, which the written type names (`[Resp]`). The
        // free strategies then answer the call from the method's name, as
        // upstream leaves it (#2543). A value an outside package hands out
        // (`http.NewRequest`, `ctx.Err()`) stays refused.
        if r.language == "go" && !object_or_class.contains('.')
            && self
                .go_local_declaration_at(object_or_class.clone(), r.file_path.clone(), r.line, r.column)?
                .is_some_and(|d| d.declared_type.is_none())
            && self.go_generic_parameter_initializer(&object_or_class, r)?
        {
            self.go_mc_untyped_local = true;
        }
        Ok(None)
    }

    /// Whether `name`'s short-var initializer calls a method on a parameter
    /// whose written type is a generic of an outside package
    /// (`chunk, _ := s.Recv()` on `s grpc.ServerStreamingClient[Resp]`).
    fn go_generic_parameter_initializer(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(binding) = self.receiver_binding(name, r)? else { return Ok(false) };
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = lines.get((binding.line - 1).max(0) as usize) else { return Ok(false) };
        let pattern = Self::cached_regex(&format!(
            r"\b{}\s*:?[^=]*=\s*([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*\(",
            regex::escape(name)
        ))?;
        let Some(caps) = pattern.captures(line) else { return Ok(false) };
        // Only the streaming read the rule exists for — `Recv`/`RecvMsg` hand
        // back the type argument; `Header` and its like do not.
        if !matches!(&caps[2], "Recv" | "RecvMsg") {
            return Ok(false);
        }
        let root = caps[1].to_string();
        // The receiver must still be the parameter at the initializer.
        if !self.receiver_binding(&root, r)?.is_some_and(|b| b.kind == "param") {
            return Ok(false);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(false) };
        let signature = caller.signature.clone().unwrap_or_default();
        let parameter = Self::cached_regex(&format!(r"\b{}\s+([A-Za-z_][\w.\[\]]*)", regex::escape(&root)))?;
        Ok(parameter.captures(&signature).is_some_and(|m| m[1].contains('[') && m[1].contains('.')))
    }

    /// The type a JVM `var name = Method()` local holds: the return type the
    /// one project method named `Method` declares. None for any other shape.
    fn jvm_var_initializer_type(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(binding) = self.receiver_binding(name, r)? else { return Ok(None) };
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = lines.get((binding.line - 1).max(0) as usize) else { return Ok(None) };
        let pattern = Self::cached_regex(&format!(
            r"\bvar\s+{}\s*=\s*([A-Za-z_]\w*)\s*\(",
            regex::escape(name)
        ))?;
        let Some(caps) = pattern.captures(line) else { return Ok(None) };
        let callee = caps[1].to_string();
        let hits: Vec<_> = self
            .nodes_by_name(&callee)?
            .iter()
            .filter(|n| matches!(n.kind.as_str(), "method" | "function") && n.return_type.is_some())
            .cloned()
            .collect();
        let same_file: Vec<_> = hits.iter().filter(|n| n.file_path == r.file_path).cloned().collect();
        let pick = if same_file.len() == 1 { same_file.into_iter().next() } else if hits.len() == 1 { hits.into_iter().next() } else { None };
        Ok(pick.and_then(|n| n.return_type.clone()))
    }

    /// isReactLifecycleMethod (#2589): a lifecycle method (`render`,
    /// `componentDidMount`, …) of a class whose head extends React's
    /// `Component`/`PureComponent` — such calls are React's, never a
    /// receiver's untyped guess.
    fn is_react_lifecycle_method(&mut self, n: &KNode) -> Res<bool> {
        if n.kind != "method" || !REACT_LIFECYCLE_METHODS.contains(&n.name.as_str()) {
            return Ok(false);
        }
        let Some(owner_name) = n
            .qualified_name
            .rsplit_once("::")
            .map(|(owner, _)| owner.rsplit("::").next().unwrap_or(owner).to_string())
        else {
            return Ok(false);
        };
        let Some(owner) = self
            .nodes_in_file(&n.file_path)?
            .iter()
            .find(|c| c.kind == "class" && c.name == owner_name && c.start_line <= n.start_line && c.end_line >= n.end_line)
            .cloned()
        else {
            return Ok(false);
        };
        let head = self.declaration_head(&owner, 4);
        let head = head.split('{').next().unwrap_or("");
        Ok(re!(r"(?-u:\b)extends\s+(?:React\s*\.\s*)?(?:Pure)?Component(?-u:\b)").is_match(head))
    }

    /// Whether the bound-receiver miss is a local the index cannot type —
    /// upstream's strategy-3 name match runs for it (`ctx`, bound from an
    /// assertion to a type a function declares); every other miss —
    /// terminal arms included — is refused. Clears both markers either way.
    fn take_go_mc_untyped(&mut self) -> bool {
        self.go_mc_terminal = false;
        std::mem::take(&mut self.go_mc_untyped_local)
    }

    /// matchMethodCall(ref, context, requireReceiverEvidence=false) — the
    /// member-tail arm of matchReference, reached by refs the boundReceiver
    /// claim never took (`this.`/`self.` roots, non-call kinds, deep
    /// receivers). Same pattern prelude and unconditional sub-arms as the
    /// bound path; the evidence-gated arms (mc-guarded, gofactory, iteration,
    /// the btm terminal) never run here — inferred types terminal-match via
    /// rmot, and the name-similarity strategies close the arm.
    pub(super) fn match_method_call_free(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        // A route owns the handler's calls but has no receiver type of its
        // own. Keep proven members; a name-similarity fallback can invent a
        // call to another type's private implementation.
        if r.language == "swift" && r.reference_kind == "calls"
            && self.node_by_id(&r.from_node_id)?.is_some_and(|n| n.kind == "route") {
            return self.match_method_call(r);
        }
        // PHP `$this->prop.method` takes the declared-type path in both
        // modes (inside method_call_shape).
        let (object_or_class, method_name, inferable, dotted) = match self.method_call_shape(r)? {
            McShape::Parsed { receiver, method, inferable, dotted } => (receiver, method, inferable, dotted),
            McShape::Done(res) => return Ok(res),
        };
        // A Java call on a value a type holds in a static field,
        // `Consts.HELPER.go()`: typed by the field's declaration, or no edge
        // for a library type's field.
        if r.language == "java" && dotted {
            if let Some(hit) = self.match_java_static_field_call(&object_or_class, &method_name, r)? {
                return Ok(hit);
            }
        }
        if matches!(r.language.as_str(), "java" | "kotlin") {
            if let Some(hit) = self.enum_constant_call(&object_or_class, &method_name, r)? { return Ok(Some(hit)); }
        }
        if inferable {
            if r.language == "go" {
                if let Some(verdict) = self.go_asserted_member(&object_or_class, &method_name, r)? { return Ok(verdict); }
            }
            // No binding anchor under requireReceiverEvidence=false — the
            // inferrers run at the ref's own site with qualified names
            // normalized (preserveQualifiedName=false).
            let cpp_alias = if r.language == "cpp" { self.cpp_receiver_alias(&object_or_class, r)? } else { None };
            let mut go_type_file: Option<String> = None;
            let inferred = if let Some(alias) = &cpp_alias {
                match &alias.raw {
                    Some(raw) => match self.cpp_type_owner(raw, &alias.site, 0, false)? {
                        Some(owner) => Some(owner.qualified_name.clone()),
                        None => Some(raw.clone()),
                    },
                    None => None,
                }
            } else if r.language == "cpp" {
                self.infer_cpp_receiver_type(&object_or_class, r, 0, false)?
            } else if r.language == "python" && object_or_class.contains('.') {
                None
            } else if r.language == "go" && !object_or_class.contains('.') {
                match self.go_local_receiver_written(&object_or_class, r)? {
                    Some((t, f)) => { go_type_file = Some(f); Some(t) }
                    None => self.infer_local_receiver_type(&object_or_class, r, true)?,
                }
            } else {
                self.infer_local_receiver_type(&object_or_class, r, r.language == "go")?
            };
            // guarded/gofactory/iteration are evidence-gated in TS and
            // never run here; mc-await still does.
            let mut inferred = if matches!(r.language.as_str(), "java" | "kotlin" | "csharp") && self.explicit_this_receiver(&object_or_class, &method_name, r) {
                self.infer_declared_member_receiver_type(&format!("this.{object_or_class}"), r)?
            } else { inferred };
            if r.language == "objc" && inferred.is_none() {
                inferred = self.objc_member_receiver_type(&object_or_class, r)?;
            }
            if inferred.is_none() && r.language == "python" && !object_or_class.contains('.') {
                if let Some(hit) = self.python_global_receiver_call(&object_or_class, &method_name, r)? { return Ok(Some(hit)); }
            }
            if inferred.is_none() { if let Some(verdict)=self.python_fixture_member(&object_or_class,&method_name,r)? { return Ok(verdict); } }
if inferred.is_none() && r.language == "dart" {
    inferred = self.infer_dart_field_receiver_type(&object_or_class, r)?;
}
            if inferred.is_none() && cpp_alias.is_none() { inferred = self.infer_declared_member_receiver_type(&object_or_class, r)?; }
            let mut awaited_file: Option<String> = None;
            if inferred.is_none() && is_esm_family(&r.language) {
                if let Some(a) = self.infer_esm_awaited_call_type(&object_or_class, r)? {
                    match a.name {
                        Some(t) if !TS_PRIMITIVE_TYPES.contains(t.as_str()) => {
                            inferred = Some(t);
                            awaited_file = Some(a.file_path);
                        }
                        _ => return Ok(None),
                    }
                }
            }
            // Same unique-field fallback as the bound arm — `recv->fp(...)`
            // proves its field member when exactly one exists.
            if inferred.is_none() && cpp_alias.is_none() && (r.language == "c" || r.language == "cpp") {
                if let Some(hit) = self.unique_field_candidate(&method_name, r)? {
                    return Ok(Some(hit));
                }
            }
            // Ruby: the receiver's class as written (`x = Other::Sub.new`),
            // found by constant lookup from the site, settles the call on it
            // or its ancestry; the normalized `Sub` could be any `Sub`.
            if inferred.is_some() && r.language == "ruby" {
                if let Some(raw) = self.infer_local_receiver_type(&object_or_class, r, true)? {
                    if let Some(qn) = self.ruby_lexical_constant(&raw, r)? {
                        return self.resolve_method_on_qualified_type(&qn, &method_name, r, 0.9, "instance-method");
                    }
                }
            }
            if let Some(t) = inferred {
                let Some(mut t) = self.inferred_member_type_bound(&t, r)? else { return Ok(None) };
                if is_esm_family(&r.language) && awaited_file.is_none() && t.ends_with(']') {
                    match self.ts_bracketed_type(&t, &method_name, r)? {
                        TsBracketed::Settled(hit) => return Ok(hit),
                        TsBracketed::Type(name) => t = name,
                    }
                }
                if matches!(r.language.as_str(), "java" | "kotlin" | "csharp") && t.contains('.') {
                    return self.match_bound_type_member(&t, &method_name, r);
                }
                // Java/Kotlin: the file's import pins WHICH same-named class.
                let mut fqn = if r.language == "java" || r.language == "kotlin" {
                    self.imported_fqn_of(&t, r)?
                } else {
                    None
                };
                // TS/JS: the file declaring the class the file imports under
                // that name. A type imported from a package outside the
                // project has no project method, whatever project class
                // shares its name, unless a project file declares the package.
                // The class the file names decides through its own supertypes;
                // only a method found counts here, since a local's type is
                // read by an unscoped scan that can find another function's
                // `agent: IAgent`.
                if is_esm_family(&r.language) && awaited_file.is_none() {
                    let raw = self.infer_local_receiver_type(&object_or_class, r, true)?.unwrap_or_else(|| t.clone());
                    match self.ts_typed_local_known(&raw, &t, &method_name, r)? {
                        TsTypedLocal::NoProjectMethod => return Ok(None),
                        TsTypedLocal::Hit(hit) => return Ok(Some(hit)),
                        TsTypedLocal::Pass(file) => fqn = file,
                    }
                }
                // An awaited type resolves from the file that declares it,
                // and a hit on that type itself must live there. A Go type
                // the written source read names that file the same way.
                let mut site = r.clone();
                if let Some(f) = awaited_file.as_ref().or(go_type_file.as_ref()) {
                    site.file_path = f.clone();
                }
                match self.resolve_method_on_type(
                    &t,
                    &method_name,
                    &site,
                    0.9,
                    "instance-method",
                    fqn.as_deref(),
                )? {
                    Some(c) => {
                        if let Some(f) = &awaited_file {
                            if c.node.qualified_name.starts_with(&format!("{t}::")) && &c.node.file_path != f {
                                return Ok(None);
                            }
                        }
                        return Ok(Some(c));
                    }
                    None if r.language == "objc" => return Ok(None),
                    None if awaited_file.is_some() => return Ok(None),
                    None if matches!(r.language.as_str(), "java" | "kotlin" | "csharp") => {
                        return self.unique_csharp_extension_method(&t, &method_name, r);
                    }
                    // A C# or Ruby receiver whose type is a project class
                    // that carries no such method (nor do its supertypes)
                    // is not a call on whichever class does: the name
                    // strategies below would guess one. A C# extension
                    // method is the one exception.
                    None if matches!(r.language.as_str(), "csharp" | "ruby")
                        && self.resolve_bound_type(&t, r, 0)?.is_some() =>
                    {
                        return self.unique_csharp_extension_method(&t, &method_name, r);
                    }
                    None => {
                        // A known builtin/primitive receiver is external when
                        // it has no project method — TS returns null here
                        // rather than letting Strategy 3 guess an unrelated
                        // `get`/`split`/`has`.
                        if is_esm_family(&r.language)
                            && (JS_BUILT_INS.contains(t.as_str())
                                || TS_PRIMITIVE_TYPES.contains(t.as_str()))
                        {
                            return Ok(None);
                        }
                    }
                }
            }
            if let Some(alias) = &cpp_alias {
                let dot = self.cpp_receiver_operator_is(&object_or_class, ".", r);
                let template = match &alias.raw { Some(raw) => self.cpp_alias_names_template(raw, &alias.site, r)?, None => false };
                if !template && (alias.pointer || dot) { return Ok(None); }
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
        // off the enclosing class, validated by rmot, or nothing. An element
        // of a field — `this.listeners[type].push()`, emitted as
        // `this.listeners[].push` — resolves on the element's type.
        if matches!(r.language.as_str(), "typescript" | "javascript" | "tsx" | "jsx")
            && dotted
            && object_or_class.starts_with("this.")
        {
            let field = &object_or_class["this.".len()..];
            let (field, element) = field.strip_suffix("[]").map_or((field, false), |f| (f, true));
            return self.match_ts_this_field_call(field, &method_name, r, element);
        }

        // Java/Kotlin field receiver inference — non-exclusive (a miss still
        // reaches the name strategies, exactly like TS).
        if (r.language == "java" || r.language == "kotlin") && dotted {
            if let Some(hit) = self.jvm_field_receiver(&object_or_class, &method_name, r)? {
                return Ok(Some(hit));
            }
        }

        // Object-literal namespace receiver — same-file const/variable
        // holders; under requireReceiverEvidence=false there is no binding
        // filter — every holder gets its object-literal member scan.
        if dotted && !object_or_class.contains('.') && is_object_literal_language(&r.language) {
            match self.resolve_object_path_member(&object_or_class,&method_name,r,None)? {
                js_objects_upstream::ObjectPathMatch::Found(hit) => return Ok(Some(hit)),
                js_objects_upstream::ObjectPathMatch::Refused => return Ok(None),
                js_objects_upstream::ObjectPathMatch::NoHolder => {}
            }
        }


        // A built-in method called on a value the file binds — a parameter or
        // local, which shadows any class of that name — is the built-in's
        // (#1987): `list.map()`, `cache.get()`. Constructed, imported, typed
        // and object-literal receivers resolved above; a class receiver is
        // validated by the strategy below before the name-only scan.
        if r.reference_kind == "calls" && is_js_family(&r.language)
            && JS_BUILTIN_METHODS.contains(method_name.as_str()) {
            let root = object_or_class.split('.').next().unwrap_or(&object_or_class);
            let value_bound = match self.receiver_binding(root, r)? {
                Some(b) if matches!(b.kind.as_str(), "param" | "local") => {
                    self.binding_shadows_call(&b, root, r)?
                }
                _ => false,
            };
            if value_bound { return Ok(None); }
        }

        // A VB.NET receiver the file declares as a variable (`Dim logger As
        // New FileLogger()`) is a value; that its spelling matches a type
        // (`Logger`, names being case-insensitive) says nothing of its type.
        let vb_value_receiver =
            r.language == "vbnet" && self.vb_declares_variable(&r.file_path, &object_or_class.to_ascii_lowercase())?;

        // Strategy 1 — direct class-name match, call site's file first.
        if !vb_value_receiver {
            if let Some(hit) = self.class_method_scan(&object_or_class, &method_name, r, 0.85, "qualified-name")? {
                return Ok(Some(hit));
            }
        }

        // A built-in method name is not evidence of a project member (#1987).
        // The direct class-name strategy above validates a class, whatever its
        // spelling; the name-similarity strategies below — a capitalized
        // receiver, a `$`-suffixed receiver, a unique method name — are
        // guesses, so a built-in name stops here, except on `this`/`super`,
        // whose member belongs to the enclosing class.
        if r.reference_kind == "calls" && is_js_family(&r.language)
            && JS_BUILTIN_METHODS.contains(method_name.as_str()) {
            let root = object_or_class.split('.').next().unwrap_or(&object_or_class);
            if root != "this" && root != "super" {
                return Ok(None);
            }
        }

        if r.language == "csharp" {
            if let Some(alias) = self.csharp_alias_at(&object_or_class, r) {
                let name = alias.rsplit('.').next().unwrap_or(&alias);
                let owners: Vec<_> = self.nodes_by_name(name)?.iter().filter(|n| n.language == "csharp"
                    && is_class_like(&n.kind) && n.qualified_name.replace("::", ".") == alias).cloned().collect();
                return match owners.as_slice() {
                    [only] => self.resolve_method_on_qualified_type(&only.qualified_name, &method_name, r, 0.9, "instance-method"),
                    _ => Ok(None),
                };
            }
        }

        // An unbound type name from outside the project cannot name a project method.
        let type_name = re!(r"^[A-Z][A-Za-z0-9_]*$").is_match(&object_or_class)
            && !matches!(r.language.as_str(), "go" | "c" | "cpp" | "cuda" | "metal")
            && (r.language != "rust" || (object_or_class != "Self" && object_or_class.chars().any(|c| c.is_ascii_lowercase())))
            && (r.language != "pascal" || re!(r"^(?:[TEI][A-Z]\w*|Exception)$").is_match(&object_or_class));
        if type_name && !self.nodes_by_name(&object_or_class)?.iter().any(|n| {
            same_language_family(&n.language, &r.language)
                && (!matches!(r.language.as_str(), "csharp" | "java" | "rust")
                    || is_class_like(&n.kind) || matches!(n.kind.as_str(), "enum" | "namespace" | "module" | "trait" | "type_alias"))
        }) { return Ok(None); }
        if r.language == "rust" && r.reference_name.contains("::") && re!(r"^[a-z_]\w*(?:::[a-z_]\w*)*$").is_match(&object_or_class) { return Ok(None); }
        if r.language == "csharp" && matches!(object_or_class.as_str(), "string" | "object" | "int" | "long" | "short" | "byte" | "bool" | "char" | "double" | "float" | "decimal" | "uint" | "ulong" | "ushort" | "sbyte") { return Ok(None); }

        // Strategy 2 — capitalized receiver (`permissionEngine` →
        // `PermissionEngine`) against the same class scan.
        let capitalized = capitalize_first(&object_or_class);
        if capitalized != object_or_class && !vb_value_receiver {
            if let Some(hit) = self.class_method_scan(&capitalized, &method_name, r, 0.8, "instance-method")? {
                if r.language != "csharp" || hit.node.id != r.from_node_id
                    || self.same_owner_receiver_proven(&hit.node, r) {
                    return Ok(Some(hit));
                }
            }
        }

        // Strategy 3 — methods by name across the codebase, scored by
        // receiver-word overlap with the containing class name.
        if !method_name.is_empty() {
            let method_candidates = self.nodes_by_name(&method_name)?;
            // Ubiquitous-method ceiling: bail before the O(K) work.
            if method_candidates.len() as i64 > self.ambiguous_ceiling {
                return Ok(None);
            }
            // A Python receiver chain can only mean a member of what it
            // names last (`self.client.login()` is not a test case's `login`).
            let python_owner = (r.language == "python")
                .then(|| object_or_class.rsplit('.').next().unwrap_or(""))
                .filter(|last| !matches!(*last, "self" | "cls"));
            let methods: Vec<Arc<KNode>> = method_candidates
                .iter()
                .filter(|n| n.kind == "method" && n.name == method_name)
                .filter(|n| python_owner.is_none_or(|last| name_scope::python_owner_fits(n, last)))
                .cloned()
                .collect();
            let same_lang: Vec<Arc<KNode>> = methods
                .iter()
                .filter(|m| m.language == r.language)
                .cloned()
                .collect();
            let mut target = if !same_lang.is_empty() { same_lang } else { methods };
            // A receiver the file imports (`import CameraManager from
            // './ExpoCameraManager'`) is another module's value, never a
            // method declared in the calling file. Ruling the caller's file
            // out may reject a guess; it never makes the one method left a
            // likelier one, so a narrowed set takes no unique-name shortcut.
            let mut narrowed = false;
            if is_js_family(&r.language) && self.is_import_bound_receiver(&object_or_class, r)? {
                let before = target.len();
                target.retain(|m| m.file_path != r.file_path);
                narrowed = target.len() != before;
            }
            let before = target.len();
            target.retain(|m| {
                let owner = m.qualified_name.rsplit_once("::").map(|(o,_)| o.rsplit([':', '.']).next().unwrap_or("")).unwrap_or("");
                !(matches!(m.name.as_str(), "get"|"post"|"put"|"patch"|"delete"|"head"|"options"|"index"|"show"|"store"|"update"|"destroy"|"create"|"edit"|"list"|"retrieve"|"partial_update") && re!(r"(?:View|ViewSet|APIView|Controller|Endpoint|ViewMixin)$").is_match(owner))
            });
            narrowed |= target.len() != before;
            if !is_test_path(&r.file_path) { target.retain(|n| !super::resolver_upstream::test_suite_path(&n.file_path)); }
            let mut visible=Vec::new();for n in target {if self.language_type_visible(&n,r)? {visible.push(n);}}target=visible;
            // Nor is it a method of a class another function declares: a
            // guess by the receiver's name is all that remains, never the
            // only method of the name. A Kotlin object expression's member
            // out of reach leaves no likelier a guess than before it was
            // one (its overrides were local functions, never candidates):
            // okhttp's `it.isReady()` is still the SerialTask interface's.
            let mut kept=Vec::with_capacity(target.len());
            let mut scoped_out=false;
            for n in target {
                if self.is_lexically_reachable(&n,r)? {kept.push(n);}
                else if !(n.language=="kotlin" && super::names::is_anonymous_member(&n)) {scoped_out=true;}
            }
            target=kept;
            // A Vue component's own method is reached as `this.m()` inside it —
            // never as `e.preventDefault()` on an event, nor
            // `this.editor.setValue()` on something the component holds. The
            // extractors emit `this.m()` as a bare `m`, which the exact-name
            // arm filters, so every receiver that reaches this strategy is
            // something else and a component's own method is dropped; the
            // `this` test mirrors upstream's matchMethodCall.
            if is_js_family(&r.language) {
                let before = target.len();
                target.retain(|m| {
                    !php_scope::is_vue_component_method(m) || (object_or_class == "this" && m.file_path == r.file_path)
                });
                // A React class component's `render()` and lifecycle methods
                // are React's to call (#2589): an untyped `root.render(<App/>)`
                // is no error boundary's `render`, whatever holds the root.
                if object_or_class != "this" && object_or_class != "super" {
                    let mut kept = Vec::new();
                    for n in target { if !self.is_react_lifecycle_method(&n)? { kept.push(n); } }
                    target = kept;
                }
                narrowed |= target.len() != before;
            }
            if super::call_shape::is_std_method(&r.language, &method_name)
                && !matches!(object_or_class.as_str(), "self" | "Self" | "this" | "base") {
                let mut kept=Vec::new();
                for n in target {if self.dart_receiver_names_owner(super::call_shape::receiver_link(&object_or_class),&n)? {kept.push(n);}}
                target=kept;
            }
            let target = &target;
            // Nothing types a Ruby, CFML or Objective-C receiver here: the one
            // method must also belong to something the receiver is named after
            // (`web_push_request.legacy_encrypt` → WebPushRequest), or
            // rubocop's `node.loc` lands on the project's one `loc` and ObjC's
            // `image.respondsToSelector:` on a proxy's override. An ObjC
            // receiver may instead name or declare a class that inherits the
            // method. CFML's `variables.m()` is a call on the component itself.
            //
            // The PHP arm (upstream #2152) is dormant: the Phase 2b
            // bound-receiver claim (is_binding_receiver_call) takes every PHP
            // receiver call first and its refusal is terminal, so an untyped
            // `$page->save()` or `Page::save()` never reaches strategy 3 (see
            // docs/design/resolution-binding-model-plan.md). It is kept so
            // the rule is in place, receiver named for the class or its
            // ancestry, if that gate ever lets PHP calls through.
            let untyped_unnamed = match target.first() {
                Some(m) if target.len() == 1 && matches!(r.language.as_str(), "ruby" | "cfml" | "cfscript" | "objc" | "php") => {
                    !re!(r"(?i)^(?:self|self\.class|this|super|variables|weak_?self|strong_?self)$").is_match(&object_or_class)
                        && !shares_receiver_word(&object_or_class, m)
                        && !(r.language == "objc" && self.objc_receiver_reaches(&object_or_class, m)?)
                        && !(r.language == "php" && self.php_receiver_reaches(&object_or_class, m)?)
                }
                _ => false,
            };
            // A Lua call through a standard or host library table, or a
            // string method on a value, is the library's.
            let lua_library = target.len() == 1
                && matches!(r.language.as_str(), "lua" | "luau")
                && is_lua_library_call(&object_or_class, &method_name, r, &target[0]);
            if target.len() == 1 && !narrowed && !scoped_out && target[0].language == r.language && !untyped_unnamed && !lua_library
                && !self.unnamed_test_double(&target[0], &object_or_class, r)
                && (target[0].id != r.from_node_id || self.same_owner_receiver_proven(&target[0], r)) {
                return Ok(Some(KCand {
                    node: target[0].clone(),
                    confidence: 0.7,
                    resolved_by: "instance-method",
                }));
            }
            if target.len() > 1 || (target.len() == 1 && scoped_out && !narrowed) {
                let receiver_words = split_camel_case(super::call_shape::receiver_link(&object_or_class));
                // Same-file candidates first, so a score tie resolves to the
                // call site's own file (`score > bestScore` keeps first seen).
                let ordered = prefer_call_site_file(self.within_cpp_translation_unit(target.clone(), r, false)?, &r.file_path);
                let mut best: Option<Arc<KNode>> = None;
                let mut best_score = 0i64;
                let mut tied = Vec::new();
                for m in &ordered {
                    let owner = m.qualified_name.rsplit_once("::").map(|(p, _)| p.rsplit([':', '.']).next().unwrap_or("")).unwrap_or("");
                    let class_words = split_camel_case(owner);
                    let double = re!(r"(?i)\b(?:fake|mock|mocked|stub|dummy|spy)\b");
                    if double.is_match(&class_words.join(" ")) && !receiver_words.iter().any(|w| double.is_match(w)) && !self.read_file(&r.file_path).is_some_and(|s| s.text().contains(owner)) { continue; }
                    let mut score = receiver_words
                        .iter()
                        .filter(|w| {
                            class_words
                                .iter()
                                .any(|cw| cw.eq_ignore_ascii_case(w))
                        })
                        .count() as i64;
                    if receiver_words.last().zip(class_words.last()).is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)) { score += 1; }
                    if m.language == r.language {
                        score += 1;
                    }
                    if score > best_score {
                        best_score = score;
                        best = Some(m.clone());
                        tied.clear();
                        tied.push(m.clone());
                    } else if score == best_score { tied.push(m.clone()); }
                }
                if r.language == "vbnet" && best_score >= 2 && tied.len() > 1 { best = self.vb_break_tie(tied, r); }
                if let Some(bm) = best {
                    if best_score >= 2 && (bm.id != r.from_node_id || self.same_owner_receiver_proven(&bm,r)) {
                        return Ok(Some(KCand {
                            node: bm,
                            confidence: 0.65,
                            resolved_by: "instance-method",
                        }));
                    }
                }
            }
        }
        Ok(None)
    }

    /// The one C# extension method (`static T M(this Owner o)`) named
    /// `method` whose `this` parameter can hold a `receiver_type`: that type,
    /// one of its project supertypes, or a type the project does not
    /// declare (a generic `T`, `object`, a framework interface). None
    /// elsewhere.
    fn unique_csharp_extension_method(&mut self, receiver_type: &str, method: &str, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.language != "csharp" {
            return Ok(None);
        }
        let named: Vec<Arc<KNode>> = self
            .nodes_by_name(method)?
            .iter()
            .filter(|n| n.kind == "method" && n.language == "csharp")
            .cloned()
            .collect();
        let simple = |t: &str| -> String {
            let t = t.split('<').next().unwrap_or(t);
            t.rsplit('.').next().unwrap_or(t).to_string()
        };
        let receiver = simple(receiver_type);
        let mut accepts: HashSet<String> = HashSet::from([receiver.clone()]);
        let mut pending: VecDeque<Arc<KNode>> = self
            .nodes_by_name(&receiver)?
            .iter()
            .filter(|n| n.language == "csharp" && is_class_like(&n.kind))
            .cloned()
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(n) = pending.pop_front() {
            if seen.len() >= 40 || !seen.insert(n.id.clone()) {
                continue;
            }
            accepts.insert(n.name.clone());
            pending.extend(self.supertype_nodes(&n.id)?);
        }
        let mut extensions = Vec::new();
        for n in named {
            let Some(lines) = self.read_file(&n.file_path) else { continue };
            let from = (n.start_line - 1).max(0) as usize;
            let to = ((n.start_line + 2).max(0) as usize).min(lines.len());
            let head = if from < to { lines[from..to].join(" ") } else { String::new() };
            let Some(c) = re!(r"\(\s*this\s+([A-Za-z_][A-Za-z0-9_.]*)").captures(&head) else { continue };
            let this_type = simple(&c[1]);
            let declared = self
                .nodes_by_name(&this_type)?
                .iter()
                .any(|d| d.language == "csharp" && is_class_like(&d.kind));
            if accepts.contains(&this_type) || !declared {
                extensions.push(n);
            }
        }
        Ok(match extensions.as_slice() {
            [only] => Some(KCand { node: only.clone(), confidence: 0.7, resolved_by: "instance-method" }),
            _ => None,
        })
    }

    /// isImportBinding (name-matcher.ts): is the root of a member call's
    /// receiver (`CameraManager` in `CameraManager.x`) one of the file's
    /// imports? Used only to rule candidates out, never to pick one.
    fn is_import_bound_receiver(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<bool> {
        let root = receiver.split('.').next().unwrap_or("");
        Ok(!root.is_empty() && self.import_mappings(&r.file_path)?.iter().any(|i| i.local_name == root))
    }

    /// A Java call on a value a type holds in a static field,
    /// `Consts.HELPER.go()` — name-matcher.ts matchJavaStaticFieldCall.
    /// `Ok(None)` punts to the strategies below (a local or a nested type's
    /// member call); `Ok(Some(None))` is terminal — a field of a library
    /// type links nothing. The field is the owner's own or one it inherits,
    /// and the call goes to the method of the type the field's declaration
    /// gives the value, read in the field's own file.
    pub(super) fn match_java_static_field_call(
        &mut self,
        receiver: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<KCand>>> {
        let Some(m) = re!(r"^((?:[a-z_$][\w$]*\.)*(?:[A-Z][\w$]*\.)*[A-Z][\w$]*)\.([A-Za-z_$][\w$]*)$").captures(receiver) else {
            return Ok(None);
        };
        let (owner_written, field_name) = (m[1].to_string(), m[2].to_string());
        let head = owner_written.split('.').next().unwrap_or("");
        // A local or a field of that name holds a value, not a type.
        if head.starts_with(|c: char| c.is_ascii_uppercase())
            && (self.infer_local_receiver_type(head, r, false)?.is_some()
                || self.infer_java_field_receiver_type(head, r)?.is_some())
        {
            return Ok(None);
        }
        let owner = match self.resolve_bound_type(&owner_written, r, 0)? {
            Some(o) => o,
            None => {
                // jvmTypeDecls: the family types the written name pins — a
                // Kotlin object a Java file names (its INSTANCE/Companion).
                let Some(name) = java_type_name(&owner_written)
                    .map(|n| n.rsplit('.').next().unwrap_or(&n).to_string())
                else {
                    return Ok(None);
                };
                let tail = name.rsplit("::").next().unwrap_or(&name);
                let owners: Vec<Arc<KNode>> = self.nodes_by_name(tail)?.iter().filter(|n| {
                    same_language_family(&n.language, "java") && JVM_DECLARED_TYPE_KINDS.contains(&n.kind.as_str())
                        && (n.qualified_name == name || n.qualified_name.ends_with(&format!("::{name}")))
                }).cloned().collect();
                match owners.as_slice() {
                    [only] => only.clone(),
                    _ => match owners.iter().find(|d| d.file_path == r.file_path) {
                        Some(d) => d.clone(),
                        None if owners.is_empty() => {
                            // No project type of the root name: a library
                            // owner links nothing.
                            let library = !self.nodes_by_name(head)?.iter().any(|n| {
                                same_language_family(&n.language, "java") && JVM_DECLARED_TYPE_KINDS.contains(&n.kind.as_str())
                            });
                            return Ok(if library { Some(None) } else { None });
                        }
                        None => return Ok(None),
                    },
                }
            }
        };
        let found = |n: Option<Arc<KNode>>| {
            n.map(|node| KCand { node, confidence: 0.9, resolved_by: "instance-method" })
        };
        // A Kotlin object's INSTANCE or Companion, as Java writes them, is
        // the object itself.
        if owner.language == "kotlin" && (field_name == "INSTANCE" || field_name == "Companion") {
            let hit = self.java_static_field_method_of(&owner.qualified_name, method, &owner.file_path)?;
            return Ok(Some(found(hit)));
        }
        // `Outer.Inner.m()` is a static call on a nested type, not a field.
        if self.nodes_by_qualified_name(&format!("{}::{field_name}", owner.qualified_name))?
            .iter().any(|n| JVM_DECLARED_TYPE_KINDS.contains(&n.kind.as_str()))
        {
            return Ok(None);
        }
        let Some(field) = self.java_static_field(&owner, &field_name)? else {
            return Ok(None);
        };
        if field.language != "java" {
            return Ok(None);
        }
        if field.kind == "enum_member" {
            // A constant's own body overrides first (`GREEN { String hex() }`), then the enum's.
            let enum_qn = &field.qualified_name[..field.qualified_name.rfind("::").unwrap_or(0)];
            if let Some(hit) = self
                .java_static_field_method_of(&field.qualified_name, method, &field.file_path)?
                .or(self.java_static_field_method_of(enum_qn, method, &field.file_path)?)
            {
                return Ok(Some(Some(KCand { node: hit, confidence: 0.9, resolved_by: "instance-method" })));
            }
            let enum_decl = self.nodes_by_qualified_name(enum_qn)?.iter()
                .find(|n| n.file_path == field.file_path && JVM_DECLARED_TYPE_KINDS.contains(&n.kind.as_str())).cloned();
            return Ok(Some(match enum_decl {
                Some(decl) => self.java_method_on_type(&decl, method, r)?,
                None => None,
            }));
        }
        // A method of an anonymous class the field is initialized with
        // (a CREATOR's createFromParcel).
        for anon in self.nodes_in_file(&field.file_path)?.iter().filter(|n| {
            n.kind == "class"
                && n.qualified_name.starts_with(&format!("{}::<", field.qualified_name))
                && n.name.contains("$anon@")
        }) {
            if let Some(hit) = self.java_static_field_method_of(&anon.qualified_name, method, &field.file_path)? {
                return Ok(Some(Some(KCand { node: hit, confidence: 0.9, resolved_by: "instance-method" })));
            }
        }
        let site = r.clone().at(&field);
        // A `new X(…)` initializer builds X, the declared type's subtype at most.
        if let Some(built) = self.java_field_initializer_type(&field) {
            if let Some(decl) = self.resolve_bound_type(&built, &site, 0)? {
                if let Some(hit) = self.java_method_on_type(&decl, method, &site)? {
                    return Ok(Some(Some(hit)));
                }
            }
        }
        let Some(declared) = java_field_signature_type(&field).map(|n| n.replace("::", ".")) else {
            return Ok(Some(None));
        };
        Ok(Some(match self.resolve_bound_type(&declared, &site, 0)? {
            Some(d) => self.java_method_on_type(&d, method, &site)?,
            None => None,
        }))
    }

    /// The static field `name` of Java type `owner`, its own or one it
    /// inherits (an interface's constant, an enum's constant), five levels
    /// up — the resolved supertype edges walk the types it extends or
    /// implements.
    fn java_static_field(&mut self, owner: &KNode, name: &str) -> Res<Option<Arc<KNode>>> {
        let mut seen: HashSet<String> = HashSet::from([owner.id.clone()]);
        let mut frontier: Vec<Arc<KNode>> = vec![Arc::new(owner.clone())];
        for _ in 0..5 {
            let mut next = Vec::new();
            for ty in &frontier {
                if let Some(own) = self
                    .nodes_by_qualified_name(&format!("{}::{name}", ty.qualified_name))?
                    .iter()
                    .find(|n| {
                        n.file_path == ty.file_path
                            && matches!(n.kind.as_str(), "field" | "constant" | "enum_member")
                    })
                {
                    return Ok(Some(own.clone()));
                }
                for decl in self.supertype_nodes(&ty.id)? {
                    if seen.insert(decl.id.clone()) {
                        next.push(decl);
                    }
                }
            }
            if next.is_empty() {
                return Ok(None);
            }
            frontier = next;
        }
        Ok(None)
    }

    /// `type_qn`'s own `method` in `file` (name-matcher.ts's methodOf).
    fn java_static_field_method_of(&mut self, type_qn: &str, method: &str, file: &str) -> Res<Option<Arc<KNode>>> {
        Ok(self
            .nodes_by_qualified_name(&format!("{type_qn}::{method}"))?
            .iter()
            .find(|n| n.kind == "method" && n.file_path == file)
            .cloned())
    }

    /// A method on a Java type the project declares — resolveOnJavaType:
    /// the declaration's own method first, then the class-chain walk with
    /// its file pinned. A Scala or Kotlin declaration is not followed.
    fn java_method_on_type(&mut self, decl: &KNode, method: &str, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if let Some(own) = self.java_static_field_method_of(&decl.qualified_name, method, &decl.file_path)? {
            return Ok(Some(KCand { node: own, confidence: 0.9, resolved_by: "instance-method" }));
        }
        if decl.language != "java" {
            return Ok(None);
        }
        let segs: Vec<&str> = decl.qualified_name.split("::").collect();
        let has_pkg = segs.len() > 1 && segs[0].starts_with(|c: char| c.is_ascii_lowercase());
        let chain = segs[usize::from(has_pkg)..].join("::");
        let fqn = decl.qualified_name.replace("::", ".");
        self.resolve_method_on_type(&chain, method, r, 0.9, "instance-method", Some(&fqn))
    }

    /// The class a field's `new X(…)` initializer builds — written with
    /// dots or not, generics stripped (name-matcher.ts's built regex).
    fn java_field_initializer_type(&mut self, field: &KNode) -> Option<String> {
        let lines = self.read_file(&field.file_path)?;
        let start = (field.start_line - 1).max(0) as usize;
        let end = (field.end_line.max(field.start_line)) as usize;
        let decl_text = lines.get(start..end.min(lines.len()))?.join("\n");
        let m = Regex::new(&format!(
            r"\b{}\s*=\s*new\s+((?:[\w$]+\s*\.\s*)*[\w$]+)\s*(?:<[^()]*>)?\s*\(",
            regex::escape(&field.name)
        ))
        .ok()?
        .captures(&decl_text)?;
        java_type_name(&m[1].replace(char::is_whitespace, "")).map(|n| n.replace("::", "."))
    }

    /// Java/Kotlin field receiver inference — non-exclusive: `Some` settles
    /// the ref, `None` lets the name strategies run, exactly like TS.
    pub(super) fn jvm_field_receiver(&mut self, receiver: &str, method: &str, r: &ResolveRefIn) -> Res<Option<KCand>> {
        // A local or parameter of the same name hides the field, unless the
        // call names it as `this.field`.
        let bindings = self.bindings(&r.file_path)?;
        if innermost_binding(&bindings, receiver, Some(r.line)).is_some_and(|b| b.kind == "local" || b.kind == "param") {
            let call = format!("{receiver}.{method}(");
            let explicit_this = self
                .read_file(&r.file_path)
                .and_then(|ls| ls.get((r.line - 1) as usize).cloned())
                .is_some_and(|l| {
                    let mut hits = l.match_indices(&call).map(|(i, _)| &l[..i]).peekable();
                    hits.peek().is_some() && hits.all(|before| before.ends_with("this."))
                });
            if !explicit_this {
                return Ok(None);
            }
        }
        let Some(inferred) = self.infer_java_field_receiver_type(receiver, r)? else {
            return Ok(None);
        };
        let fqn = self.imported_fqn_of(&inferred, r)?;
        self.resolve_method_on_type(&inferred, method, r, 0.9, "instance-method", fqn.as_deref())
    }

    /// matchMethodCall's class scan (Strategies 1 and 2): a same-language
    /// class, struct, union, interface or Scala object named `class_name`, call site's file
    /// first, whose file holds a method `method` qualified under it.
    pub(super) fn class_method_scan(
        &mut self,
        class_name: &str,
        method: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
    ) -> Res<Option<KCand>> {
        let owners = self.cpp_translation_unit_owners(self.nodes_by_name(class_name)?.iter().cloned().collect(), r)?;
        let candidates = prefer_call_site_file(owners, &r.file_path);
        let candidates = if r.language == "vbnet" { self.vb_prefer(candidates, r) } else { candidates };
        let type_ref=r.clone().naming(class_name,"references");
        let mut visible=Vec::new();for c in candidates {if self.language_type_visible(&c,&type_ref)? {visible.push(c);}}
        let candidates=visible;
        for c in &candidates {
            let class_like = matches!(c.kind.as_str(), "class" | "struct" | "union" | "interface")
                || (c.language == "scala" && c.kind == "module");
            if !class_like || c.language != r.language {
                continue;
            }
            // C++'s `Outer::Count()` is the member C++ looks up in `Outer`: its own, else one a
            // base declares, never one of a class nested in it, like `Outer::Inner::Count` beside
            // an inherited `Base::Count`.
            if c.language == "cpp" && resolved_by == "qualified-name" {
                let Some(found) = self.cpp_method_of(c, method, r, 0, &mut HashSet::new())? else { continue };
                let own = found.qualified_name == format!("{}::{method}", c.qualified_name);
                return Ok(Some(KCand { node: found, confidence: if own { 0.85 } else { 0.8 }, resolved_by }));
            }
            // A call on a Ruby class or module constant takes the class
            // method (`def self.m`) when it and `def m` share the qualified
            // name, then the owner's Ruby ancestry (#2834).
            if c.language == "ruby" {
                if let Some((found, own)) = self.ruby_method_of(&c.qualified_name, method, r)? {
                    return Ok(Some(KCand { node: found, confidence: if own { confidence } else { 0.8 }, resolved_by }));
                }
                continue;
            }
            let in_file = self.nodes_in_file(&c.file_path)?;
            if let Some(mn) = in_file
                .iter()
                // An owner segment equal to the class name: `Loud` is not
                // `Loudspeaker`. In C and C++, only a method of exactly that
                // class: protobuf's `arena.get()` is not `Arena::UniquePtr::get`.
                .find(|n| {
                    n.kind == "method" && n.name == method && if matches!(c.language.as_str(), "c" | "cpp" | "python" | "r") {
                        n.qualified_name == format!("{}::{method}", c.qualified_name)
                    } else {
                        n.qualified_name.split([':', '.']).any(|s| s == c.name)
                    }
                })
            {
                return Ok(Some(KCand { node: mn.clone(), confidence, resolved_by }));
            }
        }
        for c in &candidates {
            if c.language == r.language && is_class_like(&c.kind) {
                if let Some(node) = self.inherited_class_method(c,method)? { return Ok(Some(KCand{node,confidence,resolved_by})); }
            }
        }
        Ok(None)
    }

    /// matchMethodCall's `luaColonMatch` — Lua/Luau method calls use a single
    /// colon (`lg:log`); recognized so receiver-type inference applies to
    /// them (#1108). `(receiver, method)`; `None` for every other language.
    pub(super) fn lua_colon_shape(&mut self, r: &ResolveRefIn) -> Res<Option<(String, String)>> {
        if r.language != "lua" && r.language != "luau" {
            return Ok(None);
        }
        let re = re!(r"^([A-Za-z0-9_.]+):([A-Za-z0-9_]+)$");
        Ok(re
            .captures(&r.reference_name)
            .map(|c| (c[1].to_string(), c[2].to_string())))
    }

    /// matchMethodCall's `rDollarMatch` — R member access is `lg$log`.
    pub(super) fn r_dollar_shape(&mut self, r: &ResolveRefIn) -> Res<Option<(String, String)>> {
        if r.language != "r" {
            return Ok(None);
        }
        let re = re!(r"^([A-Za-z0-9_.]+)\$([A-Za-z0-9_]+)$");
        Ok(re
            .captures(&r.reference_name)
            .map(|c| (c[1].to_string(), c[2].to_string())))
    }

    pub(super) fn bound_receiver_claim(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        // `^(.+)\.([\w$]+)$` is guaranteed by the gate — split at the LAST dot.
        let dot = r.reference_name.rfind('.').unwrap();
        let receiver = &r.reference_name[..dot];
        let method = &r.reference_name[dot + 1..];
        let root = receiver.split('.').next().unwrap_or(receiver);
        let bindings = self.bindings(&r.file_path)?;
        // Svelte/Astro markup and a second script sit outside the rows'
        // scopes but still see a script's top-level import.
        let binding = probe!(r, "brc:recv-binding", self.receiver_binding(root, r)?)
            .or_else(|| {
                is_sfc_scoped_script(&r.language)
                    .then(|| sfc_top_level_binding(&bindings, root).filter(|b| b.kind == "import").cloned())
                    .flatten()
            });

        if !is_esm_family(&r.language) {
            // `binding?.kind === 'import' && !phpVariable` → br:import;
            // anything else is matchMethodCall receiver inference (source).
            let php_variable =
                r.language == "php" && self.ref_line_starts_with_dollar(r);
            if binding.as_ref().is_some_and(|b| b.kind == "import") && !php_variable {
                // java/kotlin bound-type resolution runs before the import
                // descent: an owner means btm owns the ref (or refuses a
                // deeper receiver); a miss falls through to br:import.
                if (r.language == "java" || r.language == "kotlin" || r.language == "python") && self.resolve_bound_type(root, r, 0)?.is_some() {
                    return if receiver == root { self.match_bound_type_member(root, method, r) } else { self.enum_constant_call(receiver, method, r) };
                }
                let via_import = probe!(r, "brc:via-import", self.resolve_via_import_member(r)?).filter(|c| {
                    // A Go `pkg.Name` can name any exported package member —
                    // `pkg.Type(…)` conversions included; edge-kind rewriting
                    // decides calls-vs-references downstream.
                    let go_member = r.language == "go"
                        && matches!(c.node.kind.as_str(),
                            "struct" | "interface" | "type_alias" | "variable" | "constant");
                    go_member || matches!(
                        c.node.kind.as_str(),
                        "function" | "method" | "class" | "component"
                    )
                });
                return Ok(match via_import {
                    Some(c) => Some(c),
                    // A Go `pkg.Type{}.M`/`pkg.Type[args]{}.M` keeps the
                    // composite literal's written type — `pkg.Type`'s method
                    // or nothing.
                    None if r.language == "go" => {
                        match re!(r"^([A-Za-z_]\w*\.[A-Za-z_]\w*)\[[^\]]*\]$")
                            .captures(receiver)
                            .or_else(|| re!(r"^([A-Za-z_]\w*\.[A-Za-z_]\w*)$").captures(receiver))
                        {
                            Some(m) => probe!(r, "brc:go-tail", self.go_method_on_declared_type(&m[1], &r.file_path, method, r)?),
                            None => None,
                        }
                    }
                    None => None,
                });
            }
            let hit = probe!(r, "brc:match_method_call", self.match_method_call(r)?);
            if hit.is_none() && r.language == "go" && self.take_go_mc_untyped() {
                return probe!(r, "brc:mc-free", self.match_method_call_free(r));
            }
            // No binding claims the receiver's name — Kotlin's implicit `it`,
            // a parameter the index never recorded, an unknown variable — so
            // the ref is no bound call at all: matchMethodCall's name
            // strategies answer it upstream (the anon-member filter there
            // keeps an object expression's override out of the running;
            // `it.ready()` on an unseen `it` is the interface's `ready`). A
            // name the body itself declares — a named lambda's parameter,
            // `v -> v.open()` — is bound with or without a row: its refusal
            // stays terminal.
            if hit.is_none() && binding.is_none() && !self.body_binds_receiver(root, r)? {
                return probe!(r, "brc:unbound-recv", self.match_method_call_free(r));
            }
            return Ok(hit);
        }

        if binding.as_ref().is_some_and(|b| b.kind == "import") {
            // The import resolver descends one member — a deeper receiver
            // must not mistake the first member for the call.
            if receiver.contains('.') {
                // A Go `pkg.Type{}.M`/`pkg.Type[args]{}.M` keeps the
                // literal's written type — `pkg.Type`'s method or nothing.
                if r.language == "go" {
                    if let Some(m) = re!(r"^([A-Za-z_]\w*\.[A-Za-z_]\w*)\[[^\]]*\]$")
                        .captures(receiver)
                        .or_else(|| re!(r"^([A-Za-z_]\w*\.[A-Za-z_]\w*)$").captures(receiver))
                    {
                        return self.go_method_on_declared_type(&m[1], &r.file_path, method, r);
                    }
                }
                return Ok(None);
            }
            return Ok(match self.resolve_via_import_member(r)? {
                Some(c) => {
                    // A call on an imported constant or variable (`LIST.includes(x)`,
                    // `store.get(k)`) lands on that constant, as the callee is a
                    // member of it that the graph has no node for. A shadowed name
                    // never gets here: `receiver_binding` returns the parameter or
                    // local, not the import.
                    if matches!(
                        c.node.kind.as_str(),
                        "function" | "method" | "class" | "component" | "constant" | "variable"
                    ) {
                        Some(c)
                    } else {
                        None
                    }
                }
                None => None,
            });
        }
        let Some(binding) = binding else {
            return Ok(None);
        };
        if receiver.contains('.') {
            let parts: Vec<&str> = receiver.split('.').collect();
            if parts.len() != 2 {
                return Ok(None);
            }
            // br:fieldinfer — root's declared type anchored at the binding
            // site (preserve qualified names), then the field on that owner.
            let mut site = r.clone();
            site.line = binding.line;
            if let Some(nid) = &binding.node_id {
                site.from_node_id = nid.clone();
            }
            let Some(ty) = probe!(r, "brc:fieldinfer", self.infer_local_receiver_type(root, &site, true)?) else {
                return Ok(None);
            };
            let type_binding = innermost_binding(
                &bindings,
                ty.split('.').next().unwrap_or(&ty),
                Some(binding.line),
            )
            .cloned();
            let owner_id = self.binding_target_id(type_binding.as_ref(), |s| {
                // `{ ...ref, referenceName: type, 'references' }` — the
                // ORIGINAL ref, not the anchored site.
                let ref2 = r.clone().naming(&ty, "references");
                if ty.contains('.') {
                    s.resolve_via_import_member(&ref2)
                } else {
                    s.resolve_via_import(&ref2)
                }
            })?;
            let owner = self.node_by_opt_id(owner_id.as_deref())?;
            return Ok(match owner {
                Some(o)
                    if matches!(
                        o.kind.as_str(),
                        "class" | "interface" | "component" | "type_alias"
                    ) =>
                {
                    self.match_ts_field_call_bound(o.as_ref(), parts[1], method, r)?
                }
                _ => None,
            });
        }
        if let Some(c) = probe!(r, "brc:match_method_call", self.match_method_call(r)?) {
            return Ok(Some(c));
        }
        if r.language == "go" && self.take_go_mc_untyped() {
            return probe!(r, "brc:mc-free", self.match_method_call_free(r));
        }
        // A Java/Scala/C# local whose `var` or inferred declaration says
        // nothing of the value is like an unannotated JS binding: the free
        // strategies answer it, and their standard-method gate keeps a
        // library name from binding by name alone (#2915).
        if matches!(r.language.as_str(), "java" | "scala" | "csharp" | "kotlin")
            && binding.kind == "local"
            && self.infer_local_receiver_type(root, r, true)?.is_none()
        {
            return probe!(r, "brc:untyped-jvm", self.match_method_call_free(r));
        }
        if binding.kind == "param" {
            // An unannotated parameter bound the name but says nothing of
            // its value — matchMethodCall (#2589) lets the name strategies
            // answer it: `items.forEach((root) => root.render())` guesses the
            // one `render`. An annotated one (`store: any`) claims the name:
            // the claim model keeps it terminal.
            if self.js_param_unannotated(&binding, root, r)? {
                return self.match_method_call_free(r);
            }
            return Ok(None);
        }
        // An awaited binding reassigned before the call no longer holds its
        // initializer's value: the factory must not revive that type.
        if is_esm_family(&r.language) && self.infer_esm_awaited_call_type(root, r)?.is_some_and(|a| a.rebound) {
            return Ok(None);
        }
        if let Some(hit) = probe!(r, "brc:factory-tail", self.esm_factory_tail(&binding, root, method, r)?) {
            return Ok(Some(hit));
        }
        // A declaration that says nothing of the value — an unreadable
        // initializer or `new` of a non-class binding — leaves the receiver
        // untyped (#2589): the free strategies answer it.
        if self.js_binding_says_nothing(&binding, root, r)? {
            return self.match_method_call_free(r);
        }
        Ok(None)
    }

    /// Whether `name`'s first occurrence on `line` carries a `name:` type
    /// annotation — `jsParameterReading`/`jsDeclarationReading`'s `name:`
    /// anchored at the binding site (#2589). A destructured leaf (`{ root }:
    /// { root: any }`) reads its own position: followed by ` }`, it is
    /// unannotated; only a plain `name: T` pins a type.
    fn js_name_annotated(&mut self, line: &str, name: &str) -> Res<bool> {
        let escaped = regex::escape(name);
        let Some(m) = Self::cached_regex(&format!(r"(?:^|[^\w$]){escaped}(?-u:\b)"))?.find(line) else {
            return Ok(false);
        };
        Ok(re!(r"^[!?]?\s*:").is_match(&line[m.end()..]))
    }

    /// Whether a `param`/`local` row whose line scope contains the call was
    /// declared in the call's own function. Binding rows carry lines, not
    /// columns, so a one-line function and a sibling on the same line share
    /// every line; a name inside the caller's own span (its signature, or its
    /// body before the call) is the caller's, while a name before that span
    /// belongs to the sibling and does not shadow a class of the same name.
    fn binding_shadows_call(&mut self, b: &KBinding, root: &str, r: &ResolveRefIn) -> Res<bool> {
        if b.line != r.line { return Ok(true); }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(true) };
        if caller.start_line != r.line { return Ok(true); }
        let Some(line) = self.read_file(&r.file_path).and_then(|lines| lines.get((r.line - 1) as usize).cloned()) else {
            return Ok(true);
        };
        let start = caller.start_column.max(0) as usize;
        let call = r.column.max(0) as usize;
        Ok(line.get(start..call).is_some_and(|span| span_names_identifier(span, root)))
    }

    /// A `param` binding says nothing when its own position carries no
    /// annotation — `(root)`, `{ root }: { root: any }` — where `name: T`,
    /// even `name: any`, does (the claim model pins annotated receivers).
    fn js_param_unannotated(&mut self, binding: &KBinding, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = source.get((binding.line - 1).max(0) as usize) else { return Ok(false) };
        Ok(!self.js_name_annotated(line, name)?)
    }

    /// jsBindingReading's "the binding says nothing of the value" (#2589): a
    /// `name: T` annotation or a whole-value `= new C()` where `C` binds to a
    /// function/class — or to nothing — says something; every other
    /// declaration does not, and the name strategies still answer for it.
    fn js_binding_says_nothing(&mut self, binding: &KBinding, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(true) };
        let Some(line) = source.get((binding.line - 1).max(0) as usize) else { return Ok(true) };
        let escaped = regex::escape(name);
        if self.js_name_annotated(line, name)? {
            return Ok(false);
        }
        // A `name = factory(…)` binding is typed when the factory's return
        // annotation is known — upstream's jsDeclarationReading (#2589) —
        // even though the binding itself carries none.
        if self
            .untyped_local_call_type(&source, (binding.line - 1).max(0) as usize, name, r, false)?
            .is_some()
        {
            return Ok(false);
        }
        if let Some(m) = Self::cached_regex(&format!(
            r"(?:^|[^\w$]){escaped}[^=;]*=\s*new\s+([A-Za-z_$][\w$]*(?:\s*\.\s*[A-Za-z_$][\w$]*)*)\s*(?:<[^;(){{}}]*>\s*)?\(",
        ))?.captures(line) {
            let cls = m[1].split('.').next().unwrap_or("");
            return Ok(match self.receiver_binding(cls, r)? {
                None => false,
                Some(b) => match b.kind.as_str() {
                    "import" => false,
                    "decl" => !b.node_id.as_deref()
                        .and_then(|id| self.node_by_id(id).ok().flatten())
                        .is_some_and(|n| matches!(n.kind.as_str(), "function" | "class" | "component" | "method")),
                    _ => true,
                },
            });
        }
        // Every other declaration claims the receiver's name — a call result
        // the project declares (checked above) or anything else the claim
        // model pins. Only a `new C()` whose `C` is no class says nothing.
        Ok(false)
    }
}

/// parensEnd — UTF-16 unit index one past the ')' that closes the '('
/// at `from - 1`, or -1 when it never closes (JS string indexing).
/// Whether `span` contains `name` as a whole identifier. `$` and `_` are
/// identifier characters in JS.
fn span_names_identifier(span: &str, name: &str) -> bool {
    if name.is_empty() { return false; }
    let bytes = span.as_bytes();
    let ident = |i: Option<usize>| {
        i.and_then(|i| bytes.get(i))
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$'))
    };
    let mut from = 0;
    while let Some(pos) = span[from..].find(name) {
        let at = from + pos;
        if !ident(at.checked_sub(1)) && !ident(Some(at + name.len())) {
            return true;
        }
        from = at + name.len();
    }
    false
}

fn parens_end(s: &str, from: usize) -> i64 {
    let mut depth = 1i64;
    let mut units = 0usize;
    for ch in s.chars() {
        let start = units;
        units += ch.len_utf16();
        if start < from {
            continue;
        }
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return units as i64;
            }
        }
    }
    -1
}
/// ^[ \t]*(?:;|\r?\n(?![ \t]*[.(\[?])) — the lookahead is emulated:
/// `;` always ends the initializer; a newline does unless a chained
/// `.`/`(`/`[`/`?` follows its leading whitespace.
fn ends_initializer(init_s: &str, call_end: i64) -> bool {
    if call_end < 0 {
        return false;
    }
    let tail = js_slice(init_s, call_end as usize);
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
}

/// Lines of a declaration read for its factory initializer — enough to reach
/// the closing paren of a multi-line call, which the tail rule needs
/// (name-matcher.ts FACTORY_DECLARATION_LINES).
const FACTORY_DECLARATION_LINES: usize = 40;

/// REACT_LIFECYCLE_METHODS (#2589): method names React itself calls on a
/// class component — a guess must never claim them for an untyped receiver.
const REACT_LIFECYCLE_METHODS: &[&str] = &[
    "render", "componentDidMount", "componentDidUpdate", "componentWillUnmount",
    "shouldComponentUpdate", "getSnapshotBeforeUpdate", "componentDidCatch",
    "componentWillMount", "componentWillReceiveProps", "componentWillUpdate",
    "UNSAFE_componentWillMount", "UNSAFE_componentWillReceiveProps", "UNSAFE_componentWillUpdate",
];

/// A binding initializer's factory call (see factory_initializer).
pub(super) struct FactoryInit {
    awaited: bool,
    callee: Option<String>,
    owner: Option<String>,
}

/// The 0-based lines of `body` (a function's source) holding a `return` of
/// that function itself: no function body opens between the outermost
/// brace and the `return` (`=> {`, `function … {`, `name(…) {` that is not
/// `if`/`for`/`while`/`switch`/`catch`/`with`). Control-flow blocks count.
pub(super) fn own_return_offsets(body: &str) -> HashSet<usize> {
    let code = super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(body));
    let return_kw = re!(r"(?-u:\b)return(?-u:\b)");
    let returns: Vec<usize> = return_kw.find_iter(&code).map(|m| m.start()).collect();
    let mut own = HashSet::new();
    let mut stack: Vec<bool> = Vec::new();
    let mut next = 0;
    for (i, b) in code.bytes().enumerate() {
        if next < returns.len() && returns[next] == i {
            if !stack.iter().skip(1).any(|&is_fn| is_fn) {
                own.insert(i);
            }
            next += 1;
        }
        match b {
            b'{' => stack.push(opens_function_body(&code[..i])),
            b'}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    own
}

fn own_return_lines(body: &str) -> HashSet<usize> {
    let code = super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(body));
    own_return_offsets(&code).into_iter().map(|at| code[..at].bytes().filter(|b| *b == b'\n').count()).collect()
}

/// Does a `{` after `prefix` open a function body rather than a block or
/// an object literal?
fn opens_function_body(prefix: &str) -> bool {
    let p = prefix.trim_end();
    // `function name(…): T {` — the keyword after the last statement or brace.
    let tail = &p[p.rfind(['{', '}', ';']).map_or(0, |k| k + 1)..];
    if p.ends_with("=>") || re!(r"(?:^|[^A-Za-z0-9_$])function(?-u:\b)").is_match(tail) {
        return true;
    }
    let Some(head) = p.strip_suffix(')') else {
        return false;
    };
    let mut depth = 1;
    let mut open = None;
    for (k, c) in head.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(k);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(open) = open else { return false };
    let word = head[..open].trim_end();
    let word = &word[word.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$')).map_or(0, |k| k + 1)..];
    !word.is_empty() && !matches!(word, "if" | "for" | "while" | "switch" | "catch" | "with")
}

/// sharesReceiverWord (name-matcher.ts): whether a receiver is named after
/// the owner of `method`, case aside — the receiver's last segment is the
/// owner's name (`cbsecurity` → CBSecurity), or they share a word of three
/// letters or more (`web_push_request` → WebPushRequest, `executor1` →
/// Executor, `decodedImage` → UIImage). Two-letter words are class
/// prefixes (`SD`, `NS`, `UI`), not names.
pub(super) fn shares_receiver_word(receiver: &str, method: &KNode) -> bool {
    let Some(cut) = method.qualified_name.rfind("::") else { return false };
    let owner_qn = &method.qualified_name[..cut];
    let flat = |w: &str| -> String {
        let alnum: String = w.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        alnum.trim_end_matches(|c: char| c.is_ascii_digit()).to_ascii_lowercase()
    };
    let receiver_last = receiver.rsplit('.').next().unwrap_or("");
    let owner_last = re!(r"::|\.").split(owner_qn).last().unwrap_or("");
    if flat(receiver_last) == flat(owner_last) {
        return true;
    }
    let owner: HashSet<String> = split_camel_case(owner_qn).iter().map(|w| flat(w)).filter(|w| w.len() > 2).collect();
    split_camel_case(receiver).iter().any(|w| owner.contains(&flat(w)))
}

/// Lua's standard and host libraries: a call through one of these tables is
/// the library's, never the one project method that shares its name (busted's
/// `assert.truthy` went to a condition helper 987 times, `string.find` to a
/// picker's `find`, Neovim's `vim.split` to a build module's).
pub(super) const LUA_LIBRARY_TABLES: &[&str] = &[
    "string", "table", "math", "io", "os", "coroutine", "debug", "utf8", "package", "bit", "bit32", "jit", "ffi",
    "vim", "ngx", "assert", "spy", "stub", "mock", "love",
];

/// Lua string methods, reached with `s:find(…)` on any string.
const LUA_STRING_METHODS: &[&str] =
    &["find", "match", "gmatch", "gsub", "sub", "format", "upper", "lower", "len", "rep", "byte", "reverse"];

/// isLuaLibraryCall (name-matcher.ts): whether a Lua call is a library's
/// rather than `candidate` — through a library table the project doesn't
/// patch itself (kong's globalpatches do define `ngx.sleep`), or a string
/// method on a value.
fn is_lua_library_call(receiver: &str, method: &str, r: &ResolveRefIn, candidate: &KNode) -> bool {
    let root = receiver.split(['.', ':']).next().unwrap_or("");
    if LUA_LIBRARY_TABLES.contains(&root) {
        let cand_root = candidate.qualified_name.split("::").next().unwrap_or("").split('.').next().unwrap_or("");
        return cand_root != root;
    }
    LUA_STRING_METHODS.contains(&method) && r.reference_name.ends_with(&format!(":{method}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own(body: &str) -> Vec<usize> {
        let mut v: Vec<usize> = own_return_lines(body).into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn own_returns_skip_nested_function_bodies() {
        assert_eq!(own("function f() {\n  onMount(() => {\n    return i;\n  });\n}"), Vec::<usize>::new());
        assert_eq!(own("function f() {\n  if (x) {\n    return i;\n  }\n}"), vec![2]);
        assert_eq!(own("function f() {\n  const o = { m() {\n    return 1;\n  } };\n  return i;\n}"), vec![4]);
        assert_eq!(own("function f({ a }: { a: T }): R {\n  const g = function () {\n    return 1;\n  };\n  return i;\n}"), vec![4]);
        assert_eq!(own("function f() {\n  const re = /\\}/;\n  return i;\n}"), vec![2]);
    }
}

/// What the class a TS/JS typed local's file names settles for the call.
pub(super) enum TsTypedLocal {
    /// The type comes from a package outside the project: no project method.
    NoProjectMethod,
    /// The method of the class the file names, or of its supertypes.
    Hit(KCand),
    /// Nothing settled; the file declaring the imported class, when known.
    Pass(Option<String>),
}

impl KernelResolver {
    /// The class a TS/JS typed local's file names, through its own
    /// supertypes (tsKnownTypeCall), before any same-named class elsewhere.
    /// Only a method found counts: a local's type is read by an unscoped scan
    /// that can find another function's `agent: IAgent`, so one without the
    /// method mustn't rule out what follows, as a field's does.
    pub(super) fn ts_typed_local_known(&mut self, written: &str, ty: &str, method: &str, r: &ResolveRefIn) -> Res<TsTypedLocal> {
        let mut file = self.ts_imported_type_file(written, r)?;
        if file.is_none() {
            match self.ts_outside_type_declaration(written, ty, r)? {
                Some(None) => return Ok(TsTypedLocal::NoProjectMethod),
                Some(declared) => file = declared,
                // A package's type, which no manifest need list for the
                // import to leave the project: it names no project class,
                // whatever class in this file shares its last name.
                None if self.ts_imports_package(written, r)? => return Ok(TsTypedLocal::NoProjectMethod),
                None => {}
            }
        }
        if let Some(Some(hit)) = self.ts_known_type_call(ty, file.as_deref().unwrap_or(&r.file_path), method, r, 0.9)? {
            return Ok(TsTypedLocal::Hit(hit));
        }
        Ok(TsTypedLocal::Pass(file))
    }
}

/// matchNewReceiverCall (name-matcher.ts): `new Runner(args).run()`, encoded
/// by the extractor as `new Runner().run`. The receiver is an instance of the
/// class written at the call, so this shape resolves only through that class.
fn new_receiver_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^new ([\w$.]+)\(\)\.([\w$]+)$").unwrap())
}

/// javaTypeName (name-matcher.ts): a Java type as written (`Foo`,
/// `java.util.List<Foo>`, `Map.Entry<K,V>`, `Foo[]`), generics, arrays and
/// whitespace dropped, the class chain `::`-joined and a written package
/// kept dotted in front (`pkg.Outer::Inner`). Null for a primitive.
fn java_type_name(raw: &str) -> Option<String> {
    let mut t = raw.to_string();
    loop {
        let stripped = re!(r"<[^<>]*>").replace_all(&t, "").into_owned();
        if stripped == t {
            break;
        }
        t = stripped;
    }
    let t = re!(r"\[\s*\]").replace_all(&t, "").into_owned();
    let t = t.strip_suffix("...").unwrap_or(&t).to_string();
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    let segments: Vec<&str> = t.split('.').filter(|s| !s.is_empty()).collect();
    let mut first = segments.len();
    while first > 0 && re!(r"^[A-Z][\w$]*$").is_match(segments[first - 1]) {
        first -= 1;
    }
    let chain = segments[first..].join("::");
    if chain.is_empty() || NON_TYPE_RECEIVER_TOKENS.contains(chain.as_str()) {
        return None;
    }
    let pkg = segments[..first].join(".");
    Some(if pkg.is_empty() { chain } else { format!("{pkg}.{chain}") })
}

/// fieldSignatureType (name-matcher.ts), Java branch: the declared type a
/// field's signature writes (`"<Type> name"`), in java_type_name's form —
/// `pkg.Type::Inner` — or null for a primitive or missing signature.
fn java_field_signature_type(field: &KNode) -> Option<String> {
    let sig = field.signature.as_deref()?;
    let type_raw = sig[..sig.rfind(&field.name)?].trim();
    if type_raw.is_empty() {
        return None;
    }
    java_type_name(type_raw)
}

/// JVM_DECLARED_TYPE_KINDS (name-matcher.ts): the kinds a Java/Kotlin/Scala
/// type declaration has.
const JVM_DECLARED_TYPE_KINDS: [&str; 5] = ["class", "interface", "enum", "struct", "trait"];

impl KernelResolver {
    /// matchNewReceiverCall — the class a `new C().m` ref constructs, as the
    /// name its declaration carries: `ns.Runner` → `Runner`, an aliased named
    /// import `R` → `Runner`. A JS/TS built-in (`RegExp`, `Map`, `Date`…)
    /// counts only when the file imports or declares a class of that name —
    /// otherwise a project class that merely shares the name would take
    /// every `new Map().get()` in the repo.
    pub(super) fn match_new_receiver_call(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(m) = new_receiver_re().captures(&r.reference_name) else { return Ok(None) };
        let written = m.get(1).unwrap().as_str();
        let method = m.get(2).unwrap().as_str();
        let last = written.rsplit('.').next().unwrap_or(written);
        let mut cls = last.to_string();
        if !written.contains('.') {
            let imported = self
                .import_mappings(&r.file_path)?
                .iter()
                .find(|i| i.local_name == written)
                .cloned();
            if let Some(imp) = imported {
                if !imp.is_default
                    && !imp.is_namespace
                    && crate::textutil::ascii_ident_re().is_match(&imp.exported_name)
                {
                    cls = imp.exported_name;
                }
            } else if JS_BUILT_INS.contains(last)
                && !self.nodes_in_file(&r.file_path)?.iter().any(|n| n.kind == "class" && n.name == last)
            {
                return Ok(None);
            }
        }
        if r.language == "java" || r.language == "csharp" {
            return self.match_java_csharp_new_receiver_call(&cls, written, method, r);
        }
        let file = self.ts_imported_type_file(written, r)?;
        if let Some(hit) = self.resolve_method_on_type(&cls, method, r, 0.9, "instance-method", file.as_deref())? {
            return Ok(Some(hit));
        }
        // An ES5 constructor function (`function Params() {}`) keeps its
        // methods as functions assigned onto its prototype in its own file.
        let ctors: Vec<Arc<KNode>> = self
            .nodes_by_name(&cls)?
            .iter()
            .filter(|n| n.kind == "function" && same_language_family(&n.language, &r.language))
            .cloned()
            .collect();
        let [ctor] = ctors.as_slice() else { return Ok(None) };
        let lines = self.read_file(&ctor.file_path);
        let on_proto: Vec<Arc<KNode>> = self
            .nodes_in_file(&ctor.file_path)?
            .iter()
            .filter(|n| {
                n.name == method
                    && (n.kind == "function" || n.kind == "method")
                    && lines
                        .as_ref()
                        .and_then(|l| l.get((n.start_line - 1).max(0) as usize))
                        .is_some_and(|line| line.contains("prototype"))
            })
            .cloned()
            .collect();
        Ok(match on_proto.as_slice() {
            [only] => Some(KCand { node: only.clone(), confidence: 0.8, resolved_by: "instance-method" }),
            _ => None,
        })
    }

    /// matchJavaCsharpNewReceiverCall — a Java or C# `new Foo().bar()`. With
    /// one project class of the name, its method or an inherited one, as for
    /// any typed call. With several, the class the language picks, or none:
    /// Java's written package (`new a.b.Foo()`), the file's single-type
    /// import, else the caller's own package; C#'s one type the file's
    /// namespaces and usings can see (isCsharpTypeVisible). Then only that
    /// class's own method.
    fn match_java_csharp_new_receiver_call(
        &mut self,
        cls: &str,
        written: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let twins: Vec<Arc<KNode>> = self
            .nodes_by_name(cls)?
            .iter()
            .filter(|n| (n.kind == "class" || n.kind == "struct") && same_language_family(&n.language, &r.language))
            .cloned()
            .collect();
        if twins.len() <= 1 {
            return self.resolve_method_on_type(cls, method, r, 0.9, "instance-method", None);
        }
        let dir_of = |file: &str| file.rsplit('/').skip(1).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/");
        let mut chosen: Option<Arc<KNode>> = None;
        if r.language == "csharp" {
            // isCsharpTypeVisible (name-matcher.ts): `ns::Type` is visible
            // from a file whose own namespaces cover `ns` or whose usings
            // name it. A `new X().m` ref never matches a using alias, so that
            // arm does not apply.
            let own = self.csharp_namespaces_at(r).unwrap_or_default();
            let usings = self.csharp_namespace_usings_at(r);
            let mut visible = Vec::new();
            for n in &twins {
                let ns = n.qualified_name.split("::").next().unwrap_or("");
                if own.iter().any(|o| o == ns || o.starts_with(&format!("{ns}.")))
                    || usings.contains(ns)
                {
                    visible.push(n.clone());
                }
            }
            if visible.len() == 1 {
                chosen = Some(visible.remove(0));
            }
        } else {
            let fqn = if written.contains('.') {
                Some(written.to_string())
            } else {
                self.imported_fqn_of(cls, r)?
            };
            if let Some(fqn) = fqn {
                let path = fqn.replace('.', "/");
                chosen = twins
                    .iter()
                    .find(|n| n.file_path.strip_suffix(".java").unwrap_or(&n.file_path).ends_with(&format!("/{path}")))
                    .cloned();
            } else {
                let own_dir = dir_of(&r.file_path);
                let own: Vec<&Arc<KNode>> = twins.iter().filter(|n| dir_of(&n.file_path) == own_dir).collect();
                if own.len() == 1 {
                    chosen = Some(own[0].clone());
                }
            }
        }
        let Some(chosen) = chosen else { return Ok(None) };
        let want_qn = format!("{}::{}", chosen.qualified_name, method);
        Ok(self
            .nodes_in_file(&chosen.file_path)?
            .iter()
            .find(|n| n.kind == "method" && n.name == method && n.qualified_name == want_qn)
            .map(|node| KCand { node: node.clone(), confidence: 0.9, resolved_by: "instance-method" }))
    }
}
