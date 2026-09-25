//! Bound receiver types and member lookup on a resolved owner type.

use super::*;

impl KernelResolver {
    /// resolveBoundType — the declared type's owner node: Java type-parameter
    /// bounds first, then its lexical binding, then (non-ESM) the visible
    /// unique candidate. Punt propagates from viaImport's source arms.
    pub(super) fn resolve_bound_type(
        &mut self,
        ty: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Res<Option<Arc<KNode>>> {
        if depth > 4 {
            return Ok(None);
        }
        if r.language == "java" {
            let in_file = self.nodes_in_file(&r.file_path)?;
            let mut scopes: Vec<&Arc<KNode>> = in_file
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "class" | "interface" | "method")
                        && n.start_line <= r.line
                        && n.end_line >= r.line
                        && (n.start_line != r.line || n.start_column <= r.column)
                        && (n.end_line != r.line || n.end_column >= r.column)
                })
                .collect();
            scopes.sort_by(|a, b| {
                (a.end_line - a.start_line)
                    .cmp(&(b.end_line - b.start_line))
                    .then(b.start_column.cmp(&a.start_column))
            });
            let bound_re =
                re!(r"^[A-Za-z0-9_]+\s+extends\s+([A-Za-z0-9_.]+)$");
            for scope in scopes {
                let decl = scope.type_parameters.as_ref().and_then(|tps| {
                    tps.iter().find(|p| {
                        // split(/\s+/)[0] — leading whitespace yields ''.
                        p.split(|c: char| c.is_whitespace()).next() == Some(ty)
                    })
                });
                let Some(declaration) = decl else { continue };
                // An unbounded or self declaration shadows any outer bound.
                return match bound_re.captures(declaration) {
                    Some(m) if &m[1] != ty => {
                        let mut site = r.clone();
                        site.line = scope.start_line;
                        site.column = scope.start_column;
                        self.resolve_bound_type(&m[1], &site, depth + 1)
                    }
                    _ => Ok(None),
                };
            }
        }
        let bindings = self.bindings(&r.file_path)?;
        let binding = innermost_binding(
            &bindings,
            ty.split('.').next().unwrap_or(ty),
            Some(r.line),
        );
        let mut owner_id: Option<String> = None;
        if let Some(b) = binding {
            if b.kind == "import" {
                let mut ref2 = r.clone();
                ref2.reference_name = ty.to_string();
                ref2.reference_kind = "references".to_string();
                let hit = if ty.contains('.') {
                    self.resolve_via_import_member(&ref2)?
                } else {
                    self.resolve_via_import(&ref2)?
                };
                owner_id = hit.map(|c| c.node.id.clone());
                if owner_id.is_none() {
                    let mut ref3 = r.clone();
                    ref3.reference_name =
                        b.target_spec.clone().unwrap_or_else(|| ty.to_string());
                    ref3.reference_kind = "imports".to_string();
                    if let Some(c) = self.resolve_jvm_import(&ref3)? {
                        owner_id = Some(c.node.id.clone());
                    }
                }
            } else {
                owner_id = b.node_id.clone();
            }
        }
        let mut owner: Option<Arc<KNode>> = self.node_by_opt_id(owner_id.as_deref())?;
        if binding.is_some_and(|b| b.kind == "import") && r.language == "php" {
            if let Some(spec) = binding.and_then(|b| b.target_spec.clone()) {
                let stripped = spec.strip_prefix('\\').unwrap_or(&spec);
                let qualified = match stripped.rfind('\\') {
                    Some(pos) if pos + 1 < stripped.len() => {
                        format!("{}::{}", &stripped[..pos], &stripped[pos + 1..])
                    }
                    _ => stripped.to_string(),
                };
                let owners: Vec<Arc<KNode>> = self
                    .nodes_by_qualified_name(&qualified)?
                    .iter()
                    .filter(|n| {
                        n.language == "php"
                            && matches!(n.kind.as_str(), "class" | "interface" | "trait")
                    })
                    .cloned()
                    .collect();
                owner = if owners.len() == 1 {
                    Some(owners[0].clone())
                } else {
                    None
                };
            }
        }
        if binding.is_none() && !is_esm_family(&r.language) {
            let raw = if ty.contains("::") {
                self.nodes_by_qualified_name(ty)?
            } else {
                self.nodes_by_name(ty)?
            };
            let mut candidates: Vec<Arc<KNode>> = Vec::new();
            for n in raw.iter() {
                if !matches!(
                    n.kind.as_str(),
                    "class" | "struct" | "interface" | "component" | "type_alias" | "union"
                ) || n.language != r.language {
                    continue;
                }
                if !self.is_visible_across_files(n, r)? {
                    continue;
                }
                candidates.push(n.clone());
            }
            let local: Vec<Arc<KNode>> = candidates
                .iter()
                .filter(|n| n.file_path == r.file_path)
                .cloned()
                .collect();
            let namespace = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .find(|n| n.kind == "namespace")
                .map(|n| n.qualified_name.clone());
            let mut packages: Vec<String> = Vec::new();
            if r.language == "java" || r.language == "kotlin" {
                packages = self
                    .import_mappings(&r.file_path)?
                    .iter()
                    .filter(|i| i.is_namespace && i.source.ends_with(".*"))
                    .map(|i| i.source[..i.source.len() - 2].to_string())
                    .collect();
                if let Some(ns) = &namespace {
                    packages.insert(0, ns.clone());
                }
            }
            let package_candidates: Vec<Arc<KNode>> = candidates
                .into_iter()
                .filter(|n| {
                    if r.language == "python" {
                        return false;
                    }
                    if r.language == "go" {
                        return pos_dirname(&n.file_path) == pos_dirname(&r.file_path);
                    }
                    if r.language == "php" {
                        return n.qualified_name
                            == match &namespace {
                                Some(ns) => format!("{}::{}", ns, ty),
                                None => ty.to_string(),
                            };
                    }
                    if r.language == "java" || r.language == "kotlin" {
                        if namespace.is_none() && n.qualified_name == ty {
                            return true;
                        }
                        return packages
                            .iter()
                            .any(|pkg| n.qualified_name == format!("{}::{}", pkg, ty));
                    }
                    true
                })
                .collect();
            let visible = if !local.is_empty() {
                local
            } else {
                package_candidates
            };
            if visible.len() == 1 {
                owner = Some(visible[0].clone());
            }
        }
        match owner {
            Some(o)
                if matches!(
                    o.kind.as_str(),
                    "class" | "struct" | "interface" | "component" | "type_alias" | "union"
                ) =>
            {
                Ok(Some(o))
            }
            _ => Ok(None),
        }
    }

    /// matchBoundTypeMember — owner's own `QName::method` member. A miss is a
    /// PUNT, not a refusal: TS next walks live supertype edges the snapshot
    /// can't see, so the TS spine must re-derive the miss.
    pub(super) fn match_bound_type_member(
        &mut self,
        ty: &str,
        method: &str,
        site: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let owner = match self.resolve_bound_type(ty, site, 0)? {
            Some(o) => o,
            None => return Ok(None),
        };
        let members: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, method))?
            .iter()
            .filter(|n| {
                // C/C++ function-pointer members are `field` nodes —
                // `rtc->read(...)` proves `ds1685_priv::read` the same way a
                // method is proven on its owner.
                (n.kind == "method"
                    || (n.kind == "field"
                        && (site.language == "c" || site.language == "cpp")))
                    && same_language_family(&n.language, &site.language)
                    && (n.file_path == owner.file_path
                        || (site.language == "go"
                            && pos_dirname(&n.file_path) == pos_dirname(&owner.file_path))
                        || site.language == "cpp")
            })
            .cloned()
            .collect();
        let member = if members.len() == 1 {
            members.into_iter().next()
        } else {
            members
                .into_iter()
                .find(|n| n.file_path == owner.file_path)
        };
        match member {
            Some(m) => Ok(Some(KCand {
                resolved_by: if m.kind == "field" {
                    "field-call"
                } else {
                    "instance-method"
                },
                node: m,
                confidence: 0.9,
            })),
            None => Err(Halt::Punt("btm-supers")),
        }
    }

    /// resolveMethodOnType — `typeName::methodName` qualified-name suffix
    /// match with preferred-FQN and call-site disambiguation. Zero direct
    /// matches is a PUNT: TS falls into the live-edge supertype walk.
    pub(super) fn resolve_method_on_type(
        &mut self,
        type_name: &str,
        method: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
        preferred_fqn: Option<&str>,
    ) -> Res<Option<KCand>> {
        let want = format!("{}::{}", type_name, method);
        let matches: Vec<Arc<KNode>> = self
            .nodes_by_name(method)?
            .iter()
            .filter(|m| {
                m.kind == "method"
                    && same_language_family(&m.language, &r.language)
                    && (m.qualified_name == want
                        || m.qualified_name.ends_with(&format!("::{}", want)))
            })
            .cloned()
            .collect();
        if matches.is_empty() {
            return Err(Halt::Punt("rmot-supers"));
        }
        if matches.len() > 1 {
            if let Some(fqn) = preferred_fqn {
                let ext = if r.language == "kotlin" { ".kt" } else { ".java" };
                let fqn_path = format!("{}{}", fqn.replace('.', "/"), ext);
                if let Some(chosen) = matches.iter().find(|m| {
                    let fp = m.file_path.replace('\\', "/");
                    fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{}", fqn_path))
                }) {
                    return Ok(Some(KCand {
                        node: chosen.clone(),
                        confidence,
                        resolved_by,
                    }));
                }
            }
        }
        let ordered = prefer_call_site_file(matches, &r.file_path);
        Ok(Some(KCand {
            node: ordered[0].clone(),
            confidence,
            resolved_by,
        }))
    }

    /// inferJavaFieldReceiverType — the declared type of a `field` node in
    /// the class enclosing the call (`signature` = "<Type> <name>").
    pub(super) fn infer_java_field_receiver_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let in_file = self.nodes_in_file(&r.file_path)?;
        if in_file.is_empty() {
            return Ok(None);
        }
        let mut enclosing: Option<&KNode> = None;
        for n in in_file.iter() {
            if n.kind != "class" && n.kind != "interface" {
                continue;
            }
            if n.language != r.language {
                continue;
            }
            if n.start_line <= r.line
                && n.end_line >= r.line
                && enclosing.is_none_or(|e| n.start_line >= e.start_line)
            {
                enclosing = Some(n);
            }
        }
        let Some(enclosing) = enclosing else { return Ok(None) };
        let Some(field) = in_file.iter().find(|n| {
            n.kind == "field"
                && n.name == receiver
                && n.language == r.language
                && n.start_line >= enclosing.start_line
                && n.end_line <= enclosing.end_line
        }) else {
            return Ok(None);
        };
        let Some(sig) = field.signature.as_deref() else {
            return Ok(None);
        };
        // slice(0, lastIndexOf(name)) — a -1 index drops the last UTF-16 unit.
        let before = match sig.rfind(&field.name) {
            Some(i) => &sig[..i],
            None => js_prefix(sig, utf16_len(sig).saturating_sub(1)),
        };
        let type_raw = before.trim();
        if type_raw.is_empty() {
            return Ok(None);
        }
        let no_generics = re!(r"<[^>]*>").replace_all(type_raw, "");
        let no_array = re!(r"\[\s*\]")
            .replace_all(&no_generics, "")
            .to_string();
        let no_varargs = re!(r"\.\.\.$").replace(&no_array, "");
        let Some(last) = no_varargs
            .split(|c: char| c == '.' || c.is_whitespace())
            .rfind(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        if !last.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return Ok(None);
        }
        Ok(Some(last.to_string()))
    }

    /// matchGoFactoryReceiver — a Go receiver bound to a declared/param value
    /// or to the first result of a same-line `:=` factory call.
    pub(super) fn match_go_factory_receiver(
        &mut self,
        receiver: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let mut site = r.clone();
        let bindings = self.bindings(&r.file_path)?;
        let mut binding = innermost_binding(&bindings, receiver, Some(r.line)).cloned();
        if binding.is_none() {
            let mut values: Vec<Arc<KNode>> = Vec::new();
            for n in self.nodes_by_name(receiver)?.iter() {
                if n.language != "go"
                    || !matches!(n.kind.as_str(), "variable" | "constant")
                    || pos_dirname(&n.file_path) != pos_dirname(&r.file_path)
                {
                    continue;
                }
                let decls = self.bindings(&n.file_path)?;
                if decls
                    .iter()
                    .any(|row| row.node_id.as_deref() == Some(n.id.as_str()) && row.kind == "decl")
                {
                    values.push(n.clone());
                }
            }
            if values.len() != 1 {
                return Ok(None);
            }
            let value = values[0].clone();
            site.file_path = value.file_path.clone();
            site.line = value.start_line;
            let site_bindings = self.bindings(&site.file_path)?;
            binding =
                innermost_binding(&site_bindings, receiver, Some(site.line)).cloned();
        }
        let Some(binding) = binding else { return Ok(None) };
        let declaration = self
            .read_file(&site.file_path)
            .and_then(|ls| ls.get((binding.line - 1) as usize).cloned())
            .unwrap_or_default();
        // `\bRECV\s+\*?TYPE(?:\s*[,)]|\s*$)` / `\bRECV\s+\*?TYPE\s*(?:=|$)`
        static PARAM_TYPE: LazyLock<Affix> =
            LazyLock::new(|| Affix::new("", r"\s+\*?([A-Za-z0-9_.]+)(?:\s*[,)]|\s*$)", true, false, false));
        static VAR_TYPE: LazyLock<Affix> =
            LazyLock::new(|| Affix::new("", r"\s+\*?([A-Za-z0-9_.]+)\s*(?:=|$)", true, false, false));
        let value = self.node_by_opt_id(binding.node_id.as_deref())?;
        let ty = if binding.kind == "param" {
            PARAM_TYPE.capture(&declaration, receiver).map(str::to_string)
        } else {
            let sig_ty = match value.as_ref().and_then(|v| v.signature.as_deref()) {
                Some(sig) => re!(r"^=\s*&?([A-Za-z0-9_.]+)\s*\{")
                    .captures(sig)
                    .and_then(|c| c.get(1).map(|g| g.as_str().to_string())),
                None => None,
            };
            match sig_ty {
                Some(t) => Some(t),
                None => VAR_TYPE.capture(&declaration, receiver).map(str::to_string),
            }
        };
        if let Some(ty) = ty {
            let mut bsite = site.clone();
            bsite.line = binding.line;
            return self.match_bound_type_member(&ty, method, &bsite);
        }
        if binding.kind == "param" {
            return Ok(None);
        }
        let assign_re = re!(r"(?-u:\b)([A-Za-z0-9_]+(?:\s*,\s*[A-Za-z0-9_]+)*)\s*:=\s*([A-Za-z0-9_.]+)\s*\(");
        let site_bindings = self.bindings(&site.file_path)?;
        for caps in assign_re.captures_iter(&declaration) {
            let names: Vec<String> = caps[1]
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
            if names.first().map(|n| n != receiver).unwrap_or(true) {
                continue;
            }
            let name = caps[2].to_string();
            let mut factory_site = site.clone();
            factory_site.line = binding.line;
            factory_site.reference_name = name.clone();
            let factory_binding = innermost_binding(
                &site_bindings,
                name.split('.').next().unwrap_or(&name),
                Some(binding.line),
            )
            .cloned();
            let mut callee: Option<Arc<KNode>> = None;
            match &factory_binding {
                Some(b) if b.kind == "import" => {
                    let via = if name.contains('.') {
                        self.resolve_via_import_member(&factory_site)?
                    } else {
                        self.resolve_via_import(&factory_site)?
                    };
                    if let Some(c) = via {
                        callee = self.node_by_id(&c.node.id)?;
                    }
                }
                Some(b) => {
                    if let Some(id) = &b.node_id {
                        callee = self.node_by_id(id)?;
                    }
                }
                None => {
                    if !name.contains('.') {
                        let cands: Vec<Arc<KNode>> = self
                            .nodes_by_name(&name)?
                            .iter()
                            .filter(|n| {
                                n.language == "go"
                                    && n.kind == "function"
                                    && pos_dirname(&n.file_path)
                                        == pos_dirname(&site.file_path)
                            })
                            .cloned()
                            .collect();
                        if cands.len() == 1 {
                            callee = Some(cands[0].clone());
                        }
                    }
                }
            }
            let ret_shape = re!(r"^\*?[A-Za-z0-9_.]+$");
            let valid = callee.as_ref().is_some_and(|c| {
                c.kind == "function"
                    && c.return_type
                        .as_deref()
                        .is_some_and(|t| ret_shape.is_match(t))
            });
            if !valid {
                return Ok(None);
            }
            let callee = callee.unwrap();
            let ret = callee.return_type.clone().unwrap();
            let stripped = ret.strip_prefix('*').unwrap_or(&ret);
            let mut tsite = r.clone();
            tsite.file_path = callee.file_path.clone();
            tsite.line = callee.start_line;
            return self.match_bound_type_member(stripped, method, &tsite);
        }
        Ok(None)
    }
}
