use super::*;

impl KernelResolver {
    pub(in crate::resolve) fn vb_binding_on(
        &self,
        code: &str,
        name: &str,
        file: &str,
        line: i64,
    ) -> Res<Option<VbBinding>> {
        if !re!(r"(?i)\b(?:As|Dim|Static|Const|For|Each|Function|Sub|Catch|Using|From|Let|Aggregate)\b|\w[$%&!#@]").is_match(code){return Ok(None);}
        let name = regex::escape(name);
        let pattern = Self::cached_regex(&format!(
            r"(?i)(?:^|[^\w.])({name})(\s*\([\s\d,]*\))?\s*\??\s+As\s+(New\s+)?"
        ))?;
        for m in pattern.captures_iter(code) {
            let start = m.get(1).unwrap().start();
            if re!(r"(?i)\b(?:Function|Sub|Property|Event|Operator|Declare|Delegate|Class|Structure|Module|Interface|Enum|Namespace)\s+$").is_match(&code[..start]){continue;}
            let value = vb_type(&code[m.get(0).unwrap().end()..], file, line).map(|mut t| {
                if m.get(3).is_some() {
                    t.array = false;
                } else {
                    t.array |= m.get(2).is_some();
                }
                t
            });
            return Ok(Some(
                value.map(VbBinding::Type).unwrap_or(VbBinding::Unknown),
            ));
        }
        let listed = Self::cached_regex(&format!(
            r"(?i)\b(?:Dim|Static)\s+(?:[A-Za-z_]\w*\s*,\s*)*{name}\s*,[\w\s,]*?\bAs\s+"
        ))?;
        if let Some(m) = listed.find(code) {
            return Ok(Some(
                vb_type(&code[m.end()..], file, line)
                    .map(VbBinding::Type)
                    .unwrap_or(VbBinding::Unknown),
            ));
        }
        let character = Self::cached_regex(&format!(
            r"(?i)(?:\b(?:Dim|Static|Const|ByVal|ByRef|Optional|ParamArray|Each|For|Using|Private|Public|Friend|Protected|Shared|ReadOnly|WithEvents)\s+|[,(]\s*){name}([$%&!#@])(\s*\([\s\d,]*\))?"
        ))?;
        if let Some(m) = character.captures(code) {
            let kind = match &m[1] {
                "$" => "String",
                "%" => "Integer",
                "&" => "Long",
                "!" => "Single",
                "#" => "Double",
                _ => "Decimal",
            };
            let mut t = vb_type(kind, file, line).unwrap();
            t.array = m.get(2).is_some();
            return Ok(Some(VbBinding::Type(t)));
        }
        let inferred =
            Self::cached_regex(&format!(r"(?i)\b(?:Dim|Static|Const|Using)\s+{name}\s*="))?;
        if let Some(m) = inferred.find(code) {
            if !code[m.end()..].starts_with('=') {
                return Ok(Some(self.vb_value_binding(&code[m.end()..], file, line)));
            }
        }
        let each = Self::cached_regex(&format!(r"(?i)\bFor\s+Each\s+{name}\s+In\s+(.+)$"))?;
        if let Some(m) = each.captures(code) {
            let expr = m[1].trim();
            if let Some(m) = re!(r"(?i)\.\s*(?:OfType|Cast)\s*\(\s*Of\s+").find(expr) {
                if re!(r"^[^()]*\)\s*(?:\(\s*\))?\s*$").is_match(&expr[m.end()..]) {
                    if let Some(t) = vb_type(&expr[m.end()..], file, line) {
                        return Ok(Some(VbBinding::Type(t)));
                    }
                }
            }
            return Ok(Some(if re!(r"^[A-Za-z_]\w*$").is_match(expr) {
                VbBinding::Each(expr.into())
            } else {
                VbBinding::Unknown
            }));
        }
        let loose = Self::cached_regex(&format!(
            r"(?i)\bFor\s+(?:Each\s+)?{name}\b|\b(?:From|Aggregate)\s+{name}\s+In\b|\bLet\s+{name}\s*=|\bCatch\s+{name}\b|\b(?:Function|Sub)\s*\((?:[^()]*,)?\s*(?:ByVal\s+|ByRef\s+)?{name}\s*[,)]"
        ))?;
        Ok(loose.is_match(code).then_some(VbBinding::Unknown))
    }
    pub(in crate::resolve) fn vb_value_binding(&self, expr: &str, file: &str, line: i64) -> VbBinding {
        let expr = expr.split(" :").next().unwrap_or(expr).trim();
        if let Some(m) = re!(r"(?i)^New\s+").find(expr) {
            return vb_type(&expr[m.end()..], file, line)
                .map(|mut t| {
                    t.array = false;
                    VbBinding::Type(t)
                })
                .unwrap_or(VbBinding::Unknown);
        }
        if let Some(m) = re!(r"(?i)^(?:DirectCast|TryCast|CType)\s*\(").find(expr) {
            let open = m.end() - 1;
            if let Some(close) = vb_close(expr, open) {
                let args = vb_args(&expr[open + 1..close]);
                if args.len() == 2 {
                    return vb_type(&args[1], file, line)
                        .map(VbBinding::Type)
                        .unwrap_or(VbBinding::Unknown);
                }
            }
            return VbBinding::Unknown;
        }
        if expr.starts_with("\"\"") || expr.starts_with("$\"\"") {
            return VbBinding::Type(vb_type("String", file, line).unwrap());
        }
        if let Some(m) = re!(r"(?i)^(C[A-Za-z]+)\s*\(").captures(expr) {
            let ty = match m[1].to_ascii_lowercase().as_str() {
                "cbool" => "Boolean",
                "cbyte" => "Byte",
                "cchar" => "Char",
                "cdate" => "Date",
                "cdbl" => "Double",
                "cdec" => "Decimal",
                "cint" => "Integer",
                "clng" => "Long",
                "cobj" => "Object",
                "csbyte" => "SByte",
                "cshort" => "Short",
                "csng" => "Single",
                "cstr" => "String",
                "cuint" => "UInteger",
                "culng" => "ULong",
                "cushort" => "UShort",
                _ => "",
            };
            if !ty.is_empty() {
                return VbBinding::Type(vb_type(ty, file, line).unwrap());
            }
        }
        if let Some(m) = re!(r"^(?:([A-Za-z_]\w*)\s*\.\s*)?([A-Za-z_]\w*)\s*").captures(expr) {
            let rest = &expr[m.get(0).unwrap().end()..];
            let rest = if rest.starts_with('(') {
                vb_close(rest, 0)
                    .map(|close| rest[close + 1..].trim())
                    .unwrap_or("unclosed")
            } else {
                rest
            };
            if rest.is_empty() {
                return VbBinding::Call(m.get(1).map(|m| m.as_str().into()), m[2].into());
            }
        }
        VbBinding::Unknown
    }
    pub(in crate::resolve) fn vb_scope_start(&self, r: &ResolveRefIn) -> Res<i64> {
        if let Some(from) = self.node_by_id(&r.from_node_id)? {
            if from.file_path == r.file_path
                && from.start_line <= r.line
                && from.end_line >= r.line
                && matches!(
                    from.kind.as_str(),
                    "method" | "function" | "property" | "field"
                )
            {
                return Ok(from.start_line);
            }
        }
        Ok(self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "method" | "function" | "property")
                    && n.start_line <= r.line
                    && n.end_line >= r.line
            })
            .map(|n| n.start_line)
            .max()
            .unwrap_or(r.line))
    }
    pub(in crate::resolve) fn vb_declared_type(&mut self, n: &KNode, returns: bool) -> Res<Option<VbType>> {
        let code = self
            .vb_lines(&n.file_path)
            .iter()
            .skip((n.start_line - 1).max(0) as usize)
            .take(if returns { 7 } else { 3 })
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let pattern = Self::cached_regex(&format!(
            r"(?i)\b(Function|Property|Sub)\s+{}\b",
            regex::escape(&n.name)
        ))?;
        if let Some(m) = pattern.captures(&code) {
            if m[1].eq_ignore_ascii_case("Sub") {
                return Ok(None);
            }
            let mut at = m.get(0).unwrap().end();
            for _ in 0..2 {
                if let Some(open) = re!(r"^\s*\(").find(&code[at..]) {
                    let Some(close) = vb_close(&code, at + open.end() - 1) else {
                        return Ok(None);
                    };
                    at = close + 1;
                } else {
                    break;
                }
            }
            if let Some(m) = re!(r"(?i)^\s*As\s+(New\s+)?").captures(&code[at..]) {
                let mut ty = vb_type(
                    &code[at + m.get(0).unwrap().end()..],
                    &n.file_path,
                    n.start_line,
                );
                if m.get(1).is_some() {
                    if let Some(t) = &mut ty {
                        t.array = false;
                    }
                }
                return Ok(ty);
            }
            return Ok(None);
        }
        if let Some(VbBinding::Type(t)) =
            self.vb_binding_on(&code, &n.name, &n.file_path, n.start_line)?
        {
            return Ok(Some(t));
        }
        let init = Self::cached_regex(&format!(r"(?i)(?:^|[^\w.]){}\s*=", regex::escape(&n.name)))?;
        if let Some(m) = init.find(&code) {
            if let VbBinding::Type(t) =
                self.vb_value_binding(&code[m.end()..], &n.file_path, n.start_line)
            {
                return Ok(Some(t));
            }
        }
        Ok(None)
    }
    pub(in crate::resolve) fn vb_resolve_param(
        &mut self,
        t: Option<VbType>,
        owner: Option<&KNode>,
    ) -> Res<Option<VbType>> {
        let Some(t) = t else { return Ok(None) };
        if t.array || vb_builtin(&t) || !t.qualifier.is_empty() {
            return Ok(Some(t));
        }
        let mut declarations = Vec::new();
        if let Some(owner) = owner.filter(|o| o.kind == "method") {
            declarations.push(Arc::new(owner.clone()));
        }
        declarations.extend(self.vb_around(&t.file, t.line)?);
        for decl in declarations {
            for (p, constraint) in self.vb_params(&decl)? {
                if p == t.name {
                    return Ok(constraint);
                }
            }
        }
        Ok(Some(t))
    }
    pub(in crate::resolve) fn vb_member_type(&mut self, m: &VbMember) -> Res<Option<VbType>> {
        let ty = self.vb_declared_type(&m.node, m.node.kind == "method")?;
        let substituted = ty.as_ref().map(|t| vb_substitute(t, &m.args));
        // A supplied generic argument is already sited at its use, and must not be replaced by the declaration's parameter constraint.
        if ty
            .as_ref()
            .is_some_and(|t| t.qualifier.is_empty() && !t.array && m.args.contains_key(&t.name))
        {
            return Ok(substituted);
        }
        self.vb_resolve_param(substituted, Some(&m.node))
    }
    pub(in crate::resolve) fn vb_module_members(&mut self, name: &str, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let mut out = Vec::new();
        for n in self.nodes_by_lower_name(name)?.iter() {
            if n.language != "vbnet" {
                continue;
            }
            let Some(qn) = vb_parent_qn(n) else { continue };
            for owner in self.nodes_by_qualified_name(qn)?.iter() {
                if self.vb_module(owner) {
                    out.push(n.clone());
                    break;
                }
            }
        }
        Ok(self.vb_prefer(out, r))
    }
    pub(in crate::resolve) fn vb_receiver_type(
        &mut self,
        name: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Option<VbType>>> {
        if depth > 2 {
            return Ok(Some(None));
        }
        let start = self.vb_scope_start(r)?;
        let lines = self.vb_lines(&r.file_path);
        let mut local = None;
        for i in (start.max(1) as usize - 1..(r.line.max(0) as usize).min(lines.len())).rev() {
            if let Some(b) = self.vb_binding_on(&lines[i], name, &r.file_path, i as i64 + 1)? {
                local = Some((b, i as i64 + 1));
                break;
            }
        }
        if let Some((b, line)) = local {
            let mut site = r.clone();
            site.line = line;
            let ty = match b {
                VbBinding::Type(t) => {
                    let owner = self.node_by_id(&r.from_node_id)?;
                    self.vb_resolve_param(Some(t), owner.as_deref())?
                }
                VbBinding::Call(receiver, member) => {
                    self.vb_call_type(receiver.as_deref(), &member, &site, depth)?
                }
                VbBinding::Each(collection) => {
                    let collection = self
                        .vb_receiver_type(&collection, &site, depth + 1)?
                        .flatten();
                    collection.and_then(|mut t|{
                    if t.array{t.array=false;Some(t)}else if re!(r"(?i)^(List|IList|IEnumerable|ICollection|IReadOnlyList|IReadOnlyCollection|HashSet|SortedSet|Queue|Stack|LinkedList|ObservableCollection|Collection|ReadOnlyCollection|BindingList|ConcurrentBag|ConcurrentQueue|ConcurrentStack|BlockingCollection)$").is_match(&t.name)&&t.args.len()==1&&t.args[0].name!="?"{Some(t.args.remove(0))}else{None}})
                }
                VbBinding::Unknown => None,
            };
            return Ok(Some(ty));
        }
        for owner in self.vb_around(&r.file_path, r.line)? {
            for ancestor in self.vb_ancestry(&owner)? {
                if let Some(node) = self
                    .vb_members(&ancestor.node, name)?
                    .into_iter()
                    .find(|n| vb_value_kind(&n.kind) || n.kind == "method")
                {
                    return self
                        .vb_member_type(&VbMember {
                            node,
                            args: ancestor.args,
                        })
                        .map(Some);
                }
            }
        }
        if let Some(node) = self
            .vb_module_members(name, r)?
            .into_iter()
            .find(|n| vb_value_kind(&n.kind) || n.kind == "method")
        {
            return self
                .vb_member_type(&VbMember {
                    node,
                    args: HashMap::new(),
                })
                .map(Some);
        }
        Ok(None)
    }
    pub(in crate::resolve) fn vb_owners(&mut self, t: &VbType) -> Res<Option<Vec<Arc<KNode>>>> {
        if t.array || vb_builtin(t) {
            return Ok(Some(vec![]));
        }
        let (owners, ambiguous) = self.vb_types_at(t, false)?;
        Ok((!ambiguous).then_some(owners))
    }
    pub(in crate::resolve) fn vb_call_type(
        &mut self,
        receiver: Option<&str>,
        member: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<VbType>> {
        let own = receiver.is_none()
            || receiver.is_some_and(|v| re!(r"(?i)^(Me|MyClass|MyBase)$").is_match(v));
        let found = if own {
            let owners = self.vb_around(&r.file_path, r.line)?;
            let owners = &owners[..owners.len().min(1)];
            let mut found = self.vb_member_on(owners, member, r, true, None, false)?;
            if found.is_none() && receiver.is_none() {
                found = self
                    .vb_module_members(member, r)?
                    .into_iter()
                    .find(|n| n.kind == "method" || vb_value_kind(&n.kind))
                    .map(|node| VbMember {
                        node,
                        args: HashMap::new(),
                    });
            }
            found
        } else {
            let receiver = receiver.unwrap();
            let bound = self.vb_receiver_type(receiver, r, depth + 1)?;
            let ty = match bound {
                None => Some(vb_simple(receiver, r)),
                Some(t) => t,
            };
            if let Some(t) = ty {
                if let Some(owners) = self.vb_owners(&t)? {
                    self.vb_member_on(&owners, member, r, true, Some(&t), false)?
                } else {
                    None
                }
            } else {
                None
            }
        };
        match found {
            Some(m) => self.vb_member_type(&m),
            None => Ok(None),
        }
    }
    pub(in crate::resolve) fn vb_extension_param(&mut self, n: &KNode) -> Res<Option<Option<VbType>>> {
        if n.language != "vbnet" || n.kind != "method" {
            return Ok(None);
        }
        let code = self
            .vb_lines(&n.file_path)
            .iter()
            .skip((n.start_line - 2).max(0) as usize)
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let decl = Self::cached_regex(&format!(
            r"(?i)\b(?:Function|Sub)\s+{}\b",
            regex::escape(&n.name)
        ))?;
        let Some(head) = decl.find(&code) else {
            return Ok(None);
        };
        let attr=re!(r"(?i)((?:<[^<>]*>\s*)+)(?:(?:Public|Friend|Private|Protected|Shared|Overloads|Async|Iterator)\s+)*$").captures(&code[..head.start()]);
        if !attr.is_some_and(|m| re!(r"(?i)\bExtension(?:Attribute)?\b").is_match(&m[1])) {
            return Ok(None);
        }
        let mut at = head.end();
        if re!(r"(?i)^\s*\(\s*Of\b").is_match(&code[at..]) {
            let open = at + code[at..].find('(').unwrap();
            let Some(close) = vb_close(&code, open) else {
                return Ok(None);
            };
            at = close + 1;
        }
        let Some(open) = code[at..].find('(').map(|i| i + at) else {
            return Ok(None);
        };
        let Some(close) = vb_close(&code, open) else {
            return Ok(None);
        };
        let params = vb_args(&code[open + 1..close]);
        let Some(first) = params.first() else {
            return Ok(None);
        };
        let first = re!(r"<[^<>]*>").replace_all(first, "");
        let Some(as_clause) = re!(r"(?i)\bAs\s+").find(&first) else {
            return Ok(None);
        };
        let Some(t) = vb_type(&first[as_clause.end()..], &n.file_path, n.start_line) else {
            return Ok(None);
        };
        let generic = self.vb_params(n)?.iter().any(|(p, _)| p == &t.name) && !t.array;
        Ok(Some((!generic).then_some(t)))
    }
}

impl KernelResolver {
    pub(in crate::resolve) fn vb_extension_for(
        &mut self,
        t: &VbType,
        owners: &[Arc<KNode>],
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let instance = if owners.is_empty() {
            vb_bcl_methods(t)
        } else {
            None
        }; // static helper described below
        if instance.is_some_and(|methods| methods.contains(&method.to_ascii_lowercase().as_str())) {
            return Ok(None);
        }
        let mut extensions = Vec::new();
        for n in self.nodes_by_lower_name(method)?.iter() {
            if let Some(param) = self.vb_extension_param(n)? {
                extensions.push((n.clone(), param));
            }
        }
        let mut names = HashSet::from([t.name.to_ascii_lowercase()]);
        let mut open = owners.is_empty() && !vb_builtin(t) && !t.array;
        for owner in owners {
            for ancestor in self.vb_ancestry(owner)? {
                names.insert(ancestor.node.name.to_ascii_lowercase());
                for (sup, implemented) in self.vb_supers(&ancestor.node)? {
                    names.insert(vb_key(&sup));
                    let (found, _) = self.vb_types_at(&sup, false)?;
                    if found.is_empty() {
                        open = true;
                    } else if implemented {
                        for n in found {
                            for parent in self.vb_ancestry(&n)? {
                                names.insert(parent.node.name.to_ascii_lowercase());
                            }
                        }
                    }
                }
            }
        }
        let exact: Vec<_> = extensions
            .iter()
            .filter(|(_, p)| {
                p.as_ref().is_some_and(|p| {
                    vb_key(p) == vb_key(t) || (!p.array && !t.array && names.contains(&vb_key(p)))
                })
            })
            .map(|(n, _)| n.clone())
            .collect();
        if let Some(node) = self.vb_prefer(exact, r).into_iter().next() {
            return Ok(Some(KCand {
                node,
                confidence: 0.85,
                resolved_by: "instance-method",
            }));
        }
        let loose: Vec<_> = extensions
            .into_iter()
            .filter(|(_, p)| p.as_ref().is_none_or(|p| vb_may_extend(t, open, p)))
            .collect();
        if loose.len() == 1
            && (instance.is_some() || !super::call_shape::is_std_method("vbnet", method))
        {
            return Ok(Some(KCand {
                node: loose[0].0.clone(),
                confidence: 0.7,
                resolved_by: "instance-method",
            }));
        }
        Ok(None)
    }
}
pub(in crate::resolve) fn vb_may_extend(t: &VbType, open: bool, param: &VbType) -> bool {
    if vb_key(param) == "object" {
        return true;
    }
    if param.array != t.array {
        return t.array
            && re!(
                r"(?i)^(IEnumerable|IList|ICollection|IReadOnlyList|IReadOnlyCollection|Array)$"
            )
            .is_match(&param.name);
    }
    if vb_builtin(param) {
        return false;
    }
    if vb_builtin(t) {
        return param.interface_spelling;
    }
    open
}

pub(in crate::resolve) fn vb_bcl_methods(t: &VbType) -> Option<&'static [&'static str]> {
    match if t.array { "[]".to_string() } else { vb_key(t) }.as_str() {
        "string" => Some(&[
            "tostring",
            "equals",
            "gethashcode",
            "gettype",
            "clone",
            "compareto",
            "contains",
            "copyto",
            "endswith",
            "getenumerator",
            "gettypecode",
            "indexof",
            "indexofany",
            "insert",
            "isnormalized",
            "lastindexof",
            "lastindexofany",
            "normalize",
            "padleft",
            "padright",
            "remove",
            "replace",
            "split",
            "startswith",
            "substring",
            "tochararray",
            "tolower",
            "tolowerinvariant",
            "toupper",
            "toupperinvariant",
            "trim",
            "trimend",
            "trimstart",
        ]),
        "stringbuilder" => Some(&[
            "tostring",
            "equals",
            "gethashcode",
            "gettype",
            "append",
            "appendformat",
            "appendjoin",
            "appendline",
            "clear",
            "copyto",
            "ensurecapacity",
            "getchunks",
            "insert",
            "remove",
            "replace",
        ]),
        "list" => Some(&[
            "tostring",
            "equals",
            "gethashcode",
            "gettype",
            "add",
            "addrange",
            "asreadonly",
            "binarysearch",
            "clear",
            "contains",
            "convertall",
            "copyto",
            "exists",
            "find",
            "findall",
            "findindex",
            "findlast",
            "findlastindex",
            "foreach",
            "getenumerator",
            "getrange",
            "indexof",
            "insert",
            "insertrange",
            "lastindexof",
            "remove",
            "removeall",
            "removeat",
            "removerange",
            "reverse",
            "sort",
            "toarray",
            "trimexcess",
            "trueforall",
        ]),
        "dictionary" => Some(&[
            "tostring",
            "equals",
            "gethashcode",
            "gettype",
            "add",
            "clear",
            "containskey",
            "containsvalue",
            "ensurecapacity",
            "getenumerator",
            "remove",
            "trimexcess",
            "tryadd",
            "trygetvalue",
        ]),
        "hashset" => Some(&[
            "tostring",
            "equals",
            "gethashcode",
            "gettype",
            "add",
            "clear",
            "contains",
            "copyto",
            "exceptwith",
            "getenumerator",
            "intersectwith",
            "ispropersubsetof",
            "ispropersupersetof",
            "issubsetof",
            "issupersetof",
            "overlaps",
            "remove",
            "removewhere",
            "setequals",
            "symmetricexceptwith",
            "trimexcess",
            "trygetvalue",
            "unionwith",
        ]),
        "[]" => Some(&[
            "tostring",
            "equals",
            "gethashcode",
            "gettype",
            "clone",
            "copyto",
            "getenumerator",
            "getlength",
            "getlonglength",
            "getlowerbound",
            "getupperbound",
            "getvalue",
            "initialize",
            "setvalue",
        ]),
        _ => None,
    }
}
