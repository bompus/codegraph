//! Resolve Dart annotations and member chains from the expression written at the reference site.
use super::dart_libraries::{tokens, DartToken};
use super::*;

#[derive(Clone)]
struct Value {
    ty: String,
    static_: bool,
    owner: Option<String>,
}
fn identifier(t: &DartToken) -> bool {
    !t.string && re!(r"^[A-Za-z_$][\w$]*$").is_match(&t.text)
}
fn type_kind(n: &KNode) -> bool {
    matches!(
        n.kind.as_str(),
        "class" | "enum" | "interface" | "struct" | "trait"
    )
}
fn simple_type(ty: &str) -> String {
    ty.trim()
        .split(['<', '?', ' ', '['])
        .next()
        .unwrap_or("")
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_string()
}
fn pair_back(t: &[DartToken], close: usize, open: &str, shut: &str) -> Option<usize> {
    let mut depth = 0;
    for i in (0..=close).rev() {
        if t[i].string {
            continue;
        }
        if t[i].text == shut {
            depth += 1;
        }
        if t[i].text == open {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}
fn args_before(t: &[DartToken], end: usize) -> Option<(usize, Vec<String>)> {
    if t[end].text != ">" {
        return Some((end, vec![]));
    }
    let open = pair_back(t, end, "<", ">")?;
    let mut args = Vec::new();
    let mut start = open + 1;
    let mut depth = 0;
    for i in open + 1..=end {
        if t[i].text == "<" {
            depth += 1;
        }
        if t[i].text == ">" {
            depth -= 1;
        }
        if i == end || depth == 0 && t[i].text == "," {
            args.push(
                t[start..i]
                    .iter()
                    .map(|n| n.text.as_str())
                    .collect::<String>(),
            );
            start = i + 1;
        }
    }
    Some((open.checked_sub(1)?, args))
}
impl KernelResolver {
    fn dart_pick(
        &mut self,
        nodes: Vec<Arc<KNode>>,
        r: &ResolveRefIn,
        prefix: &str,
    ) -> Res<Option<Arc<KNode>>> {
        let mut visible = Vec::new();
        for n in nodes {
            if !self.dart_unnamed_extension(&n)
                && self.dart_library_decl(&n)?
                && self.dart_visible(&r.file_path, &n.file_path, &n.name, prefix)
            {
                visible.push(n);
            }
        }
        if prefix.is_empty() {
            let own: Vec<_> = visible
                .iter()
                .filter(|n| self.dart_same_library(&r.file_path, &n.file_path))
                .cloned()
                .collect();
            if !own.is_empty() {
                visible = own;
            }
        }
        if visible.len() == 1 {
            Ok(visible.pop())
        } else {
            Ok(None)
        }
    }
    fn dart_owners(&mut self, ty: &str, r: &ResolveRefIn, prefix: &str) -> Res<Vec<Arc<KNode>>> {
        let mut out = Vec::new();
        for n in self
            .nodes_by_name(&simple_type(ty))?
            .iter()
            .filter(|n| n.language == "dart" && type_kind(n))
            .cloned()
            .collect::<Vec<_>>()
        {
            if !self.dart_unnamed_extension(&n)
                && self.dart_visible(&r.file_path, &n.file_path, &n.name, prefix)
            {
                out.push(n);
            }
        }
        if prefix.is_empty() {
            let own: Vec<_> = out
                .iter()
                .filter(|n| self.dart_same_library(&r.file_path, &n.file_path))
                .cloned()
                .collect();
            if !own.is_empty() {
                out = own;
            }
        }
        Ok(out)
    }
    fn dart_constructor(&mut self, n: &KNode) -> bool {
        let Some((owner, _)) = n.qualified_name.rsplit_once("::") else {
            return false;
        };
        if n.return_type
            .as_deref()
            .is_some_and(|t| simple_type(t) == owner.rsplit("::").next().unwrap_or(owner))
        {
            let Some(source) = self.read_file(&n.file_path) else {
                return false;
            };
            let text = source
                .iter()
                .skip((n.start_line - 1).max(0) as usize)
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            return Self::cached_regex(&format!(
                r"\b{}\s*\.",
                regex::escape(owner.rsplit("::").next().unwrap_or(owner))
            ))
            .is_ok_and(|re| re.is_match(&text));
        }
        false
    }
    fn dart_static_member(
        &mut self,
        v: &Value,
        name: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        let owners = if let Some(owner) = &v.owner {
            self.nodes_by_name(&simple_type(&v.ty))?
                .iter()
                .filter(|n| n.language == "dart" && type_kind(n) && &n.file_path == owner)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.dart_owners(&v.ty, r, "")?
        };
        let mut out = Vec::new();
        for n in self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| n.language == "dart")
            .cloned()
            .collect::<Vec<_>>()
        {
            if owners.iter().any(|o| {
                o.file_path == n.file_path
                    && n.qualified_name == format!("{}::{name}", o.qualified_name)
            }) && (n.is_static
                || matches!(n.kind.as_str(), "constant" | "enum_member")
                || self.dart_constructor(&n))
            {
                out.push(n);
            }
        }
        Ok(out
            .into_iter()
            .max_by_key(|n| super::names::compute_path_proximity(&r.file_path, &n.file_path)))
    }
    fn dart_return_type(&mut self, n: &KNode, args: &[String]) -> Option<String> {
        let raw = n.return_type.as_deref()?;
        let ty = simple_type(raw);
        let source = self.read_file(&n.file_path)?;
        let text = source
            .iter()
            .skip((n.start_line - 1).max(0) as usize)
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let pattern =
            Self::cached_regex(&format!(r"\b{}\s*<([^<>]*)>\s*\(", regex::escape(&n.name))).ok()?;
        if let Some(m) = pattern.captures(&text) {
            let params: Vec<_> = m[1]
                .split(',')
                .map(|p| p.split_whitespace().next().unwrap_or(""))
                .collect();
            if let Some(i) = params.iter().position(|p| *p == ty) {
                return args.get(i).map(|t| simple_type(t));
            }
        }
        if let Some((owner, _)) = n.qualified_name.rsplit_once("::") {
            let owners = self.nodes_in_file(&n.file_path).ok()?;
            for parent in owners
                .iter()
                .filter(|p| p.qualified_name == owner && type_kind(p))
            {
                let head = source
                    .iter()
                    .skip((parent.start_line - 1).max(0) as usize)
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                if Self::cached_regex(&format!(r"\b{}\s*<([^<>]*)>", regex::escape(&parent.name)))
                    .ok()?
                    .captures(&head)
                    .is_some_and(|m| {
                        m[1].split(',')
                            .any(|p| p.split_whitespace().next() == Some(&ty))
                    })
                {
                    return None;
                }
            }
        }
        Some(ty)
    }
    fn dart_member_value(
        &mut self,
        n: &KNode,
        call: bool,
        args: &[String],
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Value>> {
        if n.kind == "enum_member" {
            return Ok((!call).then(|| Value {
                ty: n
                    .qualified_name
                    .rsplit_once("::")
                    .map_or("", |p| p.0)
                    .into(),
                static_: false,
                owner: None,
            }));
        }
        let getter = self.is_dart_getter(n);
        let ty = if matches!(n.kind.as_str(), "constant" | "variable") {
            self.dart_constant_type(n, r, depth)?
        } else {
            self.dart_return_type(n, if getter { &[] } else { args })
        };
        let Some(ty) = ty else {
            return Ok(None);
        };
        if (getter || matches!(n.kind.as_str(), "constant" | "variable")) && call {
            return match self.dart_member_of(&ty, "call", r, false)? {
                Some((member, _)) => Ok(self.dart_return_type(&member, args).map(|ty| Value {
                    ty,
                    static_: false,
                    owner: None,
                })),
                None => Ok(None),
            };
        }
        let type_site = if n.return_type.as_deref().is_some_and(|raw| simple_type(raw) != ty) {
            // Substituted generic arguments are written at the call site.
            r.clone()
        } else {
            ResolveRefIn { file_path: n.file_path.clone(), line: n.start_line, ..r.clone() }
        };
        let owners = self.dart_owners(&ty, &type_site, "")?;
        Ok((getter || matches!(n.kind.as_str(), "constant" | "variable") || call).then_some(Value {
            ty,
            static_: false,
            owner: (owners.len() == 1).then(|| owners[0].file_path.clone()),
        }))
    }
    fn dart_constant_type(
        &mut self,
        n: &KNode,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<String>> {
        if depth >= 8 {
            return Ok(None);
        }
        let Some(source) = self.read_file(&n.file_path) else {
            return Ok(None);
        };
        let text = source
            .iter()
            .skip((n.start_line - 1).max(0) as usize)
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let re = Self::cached_regex(&format!(
            r"\b(?:const|final|var)\s+(?:([A-Za-z_$][\w$.]*)\s*(?:<[^;=]*>)?\s*\??\s+)?{}\s*=",
            regex::escape(&n.name)
        ))?;
        let Some(m) = re.captures(&text) else {
            return Ok(None);
        };
        if let Some(ty) = m.get(1) {
            return Ok(Some(simple_type(ty.as_str())));
        }
        let t = tokens(&text[m.get(0).unwrap().end()..]);
        let end = dart_expr_end(&t);
        if end == 0 {
            return Ok(None);
        }
        let site = ResolveRefIn {
            file_path: n.file_path.clone(),
            line: n.start_line,
            column: 0,
            ..r.clone()
        };
        Ok(self
            .dart_expression(&t, end - 1, &site, depth + 1)?
            .map(|v| v.ty))
    }
    fn dart_field_type_for(
        &mut self,
        ty: &str,
        name: &str,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<String>> {
        if depth >= 8 {
            return Ok(None);
        }
        let lineage = self.dart_lineage(&simple_type(ty))?;
        let mut owners = Vec::new();
        for (owner, rank) in lineage.iter() {
            for n in self
                .nodes_by_name(owner)?
                .iter()
                .filter(|n| n.language == "dart" && type_kind(n))
            {
                owners.push((n.clone(), *rank));
            }
        }
        owners.sort_by_key(|(_, rank)| *rank);
        for (owner, _) in owners {
            if let Some(ty) = self.declared_member_type(&owner, name)? {
                return Ok(Some(simple_type(&ty)));
            }
            let Some(lines) = self.declared_member_lines.get(&owner.id) else {
                continue;
            };
            let text = lines
                .iter()
                .map(|(line, _)| line.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let pattern = Self::cached_regex(&format!(
                r"\b(?:final|var|const)\s+{}\s*=",
                regex::escape(name)
            ))?;
            if let Some(m) = pattern.find(&text) {
                let t = tokens(&text[m.end()..]);
                let end = dart_expr_end(&t);
                if end > 0 {
                    let site = ResolveRefIn {
                        file_path: owner.file_path.clone(),
                        line: owner.start_line,
                        column: 0,
                        ..r.clone()
                    };
                    return Ok(self
                        .dart_expression(&t, end - 1, &site, depth + 1)?
                        .map(|v| v.ty));
                }
            }
        }
        Ok(None)
    }
    fn dart_expression(
        &mut self,
        t: &[DartToken],
        mut end: usize,
        r: &ResolveRefIn,
        depth: usize,
    ) -> Res<Option<Value>> {
        if depth >= 12 || t.is_empty() {
            return Ok(None);
        }
        if t[end].text == "!" {
            let Some(e) = end.checked_sub(1) else {
                return Ok(None);
            };
            end = e;
        }
        let call = t[end].text == ")";
        if call {
            let Some(open) = pair_back(t, end, "(", ")") else {
                return Ok(None);
            };
            if open == 0 || !identifier(&t[open - 1]) && t[open - 1].text != ">" {
                if open + 1 < end {
                    return self.dart_expression(t, end - 1, r, depth + 1);
                }
                return Ok(None);
            }
            end = open - 1;
        }
        if t[end].string {
            return Ok(Some(Value {
                ty: "String".into(),
                static_: false,
                owner: None,
            }));
        }
        if t[end].text == "]" {
            return Ok(None);
        }
        let Some((name_at, args)) = args_before(t, end) else {
            return Ok(None);
        };
        if !identifier(&t[name_at]) {
            return Ok(None);
        }
        let name = &t[name_at].text;
        if name_at > 0 && t[name_at - 1].text == "." {
            let Some(mut before) = name_at.checked_sub(2) else {
                return Ok(None);
            };
            if t[before].text == "?" {
                let Some(i) = before.checked_sub(1) else {
                    return Ok(None);
                };
                before = i;
            }
            if identifier(&t[before])
                && self.dart_prefixes(&r.file_path).contains(&t[before].text)
                && self.dart_local_binding(&t[before].text, r).is_none()
            {
                let nodes = self
                    .nodes_by_name(name)?
                    .iter()
                    .filter(|n| n.language == "dart")
                    .cloned()
                    .collect();
                let Some(n) = self.dart_pick(nodes, r, &t[before].text)? else {
                    return Ok(None);
                };
                if type_kind(&n) {
                    return Ok(Some(Value {
                        ty: name.clone(),
                        static_: !call,
                        owner: Some(n.file_path.clone()),
                    }));
                }
                return self.dart_member_value(&n, call, &args, r, depth + 1);
            }
            let value = self.dart_expression(t, before, r, depth + 1)?;
            let member = match value.as_ref() {
                Some(v) if v.static_ => self.dart_static_member(v, name, r)?,
                Some(v) => self.dart_member_of_in_library(&v.ty, name, r, false, v.owner.as_deref())?.map(|p| p.0),
                None => None,
            };
            if let Some(n) = member {
                return self.dart_member_value(&n, call, &args, r, depth + 1);
            }
            if call
                && args.len() == 1
                && matches!(
                    name.as_str(),
                    "read"
                        | "watch"
                        | "get"
                        | "find"
                        | "of"
                        | "maybeOf"
                        | "call"
                        | "dependOnInheritedWidgetOfExactType"
                        | "getInheritedWidgetOfExactType"
                        | "findAncestorStateOfType"
                        | "findAncestorWidgetOfExactType"
                        | "findRootAncestorStateOfType"
                )
            {
                return Ok(Some(Value {
                    ty: simple_type(&args[0]),
                    static_: false,
                    owner: None,
                }));
            }
            if call && name == "toString" {
                return Ok(Some(Value {
                    ty: "String".into(),
                    static_: false,
                    owner: None,
                }));
            }
            let Some(v) = value else { return Ok(None) };
            if call && v.static_ && !self.dart_owners(&v.ty, r, "")?.is_empty() {
                return Ok(Some(Value {
                    static_: false,
                    ..v
                }));
            }
            if !call {
                return Ok(self
                    .dart_field_type_for(&v.ty, name, r, depth + 1)?
                    .map(|ty| Value {
                        ty: simple_type(&ty),
                        static_: false,
                        owner: None,
                    }));
            }
            return Ok(None);
        }
        if matches!(name.as_str(), "this" | "super") {
            let own = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .filter(|n| {
                    n.language == "dart"
                        && type_kind(n)
                        && n.start_line <= r.line
                        && n.end_line >= r.line
                })
                .max_by_key(|n| n.start_line)
                .cloned();
            let Some(n) = own else {
                return Ok(None);
            };
            let ty = if name == "super" {
                self.dart_head_of(&n).supers.first().cloned()
            } else {
                Some(n.name.clone())
            };
            return Ok(ty.map(|ty| Value {
                ty,
                static_: false,
                owner: None,
            }));
        }
        if name_at > 0 && t[name_at - 1].text == "as" {
            return Ok(Some(Value {
                ty: name.clone(),
                static_: false,
                owner: None,
            }));
        }
        let locally_bound = self.dart_local_binding(name, r).is_some();
        let owners = if locally_bound { Vec::new() } else { self.dart_owners(name, r, "")? };
        if !locally_bound && (!owners.is_empty() || name.starts_with(|c: char| c.is_ascii_uppercase())) {
            return Ok(Some(Value {
                ty: name.clone(),
                static_: !call,
                owner: (owners.len() == 1).then(|| owners[0].file_path.clone()),
            }));
        }
        if call && !locally_bound {
            let own = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .filter(|n| {
                    n.language == "dart"
                        && type_kind(n)
                        && n.start_line <= r.line
                        && n.end_line >= r.line
                })
                .max_by_key(|n| n.start_line)
                .cloned();
            if let Some(own) = own {
                if let Some((member, _)) = self.dart_member_of(&own.name, name, r, false)? {
                    return self.dart_member_value(&member, true, &args, r, depth + 1);
                }
            }
            let nodes = self
                .nodes_by_name(name)?
                .iter()
                .filter(|n| n.language == "dart" && n.kind == "function")
                .cloned()
                .collect();
            if let Some(n) = self.dart_pick(nodes, r, "")? {
                return self.dart_member_value(&n, true, &args, r, depth + 1);
            }
        }
        let mut ty = self.infer_local_receiver_type(name, r, false)?;
        if ty.is_none() {
            ty = self.infer_dart_field_receiver_type(name, r)?;
        }
        if ty.is_none() {
            let nodes = self
                .nodes_by_name(name)?
                .iter()
                .filter(|n| {
                    n.language == "dart" && matches!(n.kind.as_str(), "constant" | "variable")
                })
                .cloned()
                .collect();
            if let Some(n) = self.dart_pick(nodes, r, "")? {
                ty = self.dart_constant_type(&n, r, depth + 1)?;
            }
        }
        if let Some(ty) = ty {
            if call {
                return match self.dart_member_of(&simple_type(&ty), "call", r, false)? {
                    Some((n, _)) => self.dart_member_value(&n, true, &args, r, depth + 1),
                    None => Ok(None),
                };
            }
            return Ok(Some(Value {
                ty: simple_type(&ty),
                static_: false,
                owner: None,
            }));
        }
        // Generic dependency lookups can start from a value whose type is outside the index.
        if call && args.len() == 1 && matches!(name.as_str(), "get" | "find" | "read" | "watch") {
            return Ok(Some(Value {
                ty: simple_type(&args[0]),
                static_: false,
                owner: None,
            }));
        }
        Ok(None)
    }
    pub(super) fn resolve_dart_written(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.language != "dart" || !matches!(r.reference_kind.as_str(), "calls" | "decorates") {
            return Ok(None);
        }
        let Some(source) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let t = source.dart_tokens();
        let start = source
            .iter()
            .take((r.line - 1).max(0) as usize)
            .map(|l| l.len() + 1)
            .sum::<usize>();
        let line_end = start
            + source
                .get((r.line - 1).max(0) as usize)
                .map_or(0, |l| l.len());
        if r.reference_kind == "decorates" {
            return self.dart_annotation(t, start, line_end, r).map(Some);
        }
        let name = r
            .reference_name
            .rsplit('.')
            .next()
            .unwrap_or(&r.reference_name);
        let expected = start
            + source.get((r.line - 1).max(0) as usize).map_or(0, |l| {
                super::names::js_unit_to_byte(l, r.column.max(0) as usize)
            });
        let Some(at) = t
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.string && n.text == name && start <= n.start && n.start <= line_end)
            .min_by_key(|(_, n)| n.end.abs_diff(expected).min(n.start.abs_diff(expected)))
            .map(|(i, _)| i)
        else {
            return Ok(None);
        };
        if at == 0 || t[at - 1].text != "." {
            return Ok(None);
        }
        let Some(mut before) = at.checked_sub(2) else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        if t[before].text == "?" {
            let Some(i) = before.checked_sub(1) else {
                return Ok(Some(ResolveOutcome::unresolved()));
            };
            before = i;
        }
        let Some((line, column)) = token_position(&source, t[before].start) else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        let site = ResolveRefIn {
            reference_name: t[before].text.clone(),
            line,
            column,
            ..r.clone()
        };
        if let Some((prefix, tail)) = r.reference_name.split_once('.') {
            if !tail.contains('.')
                && self.dart_prefixes(&r.file_path).contains(prefix)
                && t[before].text != prefix
            {
                return Ok(Some(ResolveOutcome::unresolved()));
            }
        }
        if identifier(&t[before])
            && self.dart_prefixes(&r.file_path).contains(&t[before].text)
            && self.dart_local_binding(&t[before].text, &site).is_none()
        {
            let nodes = self
                .nodes_by_name(name)?
                .iter()
                .filter(|n| n.language == "dart")
                .cloned()
                .collect();
            let n = self.dart_pick(nodes, r, &t[before].text)?;
            return match n {
                Some(node) => self
                    .finish_pre_framework(
                        r,
                        KCand {
                            node,
                            confidence: 0.9,
                            resolved_by: "import",
                        },
                    )
                    .map(Some),
                None => Ok(Some(ResolveOutcome::unresolved())),
            };
        }
        let Some(v) = self.dart_expression(t, before, r, 0)? else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        let n = if v.static_ {
            self.dart_static_member(&v, name, r)?
        } else {
            self.dart_member_of_in_library(&v.ty, name, r, false, v.owner.as_deref())?.map(|p| p.0)
        };
        match n {
            Some(node) if node.kind != "enum_member" => self
                .finish_pre_framework(
                    r,
                    KCand {
                        node,
                        confidence: if !v.static_ && identifier(&t[before]) {
                            0.9
                        } else {
                            0.85
                        },
                        resolved_by: if v.static_ {
                            "qualified-name"
                        } else {
                            "instance-method"
                        },
                    },
                )
                .map(Some),
            _ => Ok(Some(ResolveOutcome::unresolved())),
        }
    }
    fn dart_annotation(
        &mut self,
        t: &[DartToken],
        start: usize,
        end: usize,
        r: &ResolveRefIn,
    ) -> Res<ResolveOutcome> {
        for (at, _) in t
            .iter()
            .enumerate()
            .filter(|(_, n)| n.text == "@" && start <= n.start && n.start <= end)
        {
            let mut names = Vec::new();
            let mut i = at + 1;
            let mut type_arg = None;
            while i < t.len() && names.len() < 8 && identifier(&t[i]) {
                names.push(t[i].text.clone());
                i += 1;
                if t.get(i).is_some_and(|n| n.text == "<") {
                    let mut nesting = 0;
                    for (j, token) in t.iter().enumerate().skip(i) {
                        if token.text == "<" {
                            nesting += 1;
                        }
                        if token.text == ">" {
                            nesting -= 1;
                            if nesting == 0 {
                                i = j + 1;
                                break;
                            }
                        }
                    }
                    type_arg.get_or_insert(names.len() - 1);
                }
                if t.get(i).is_some_and(|n| n.text == ".") {
                    i += 1;
                } else {
                    break;
                }
            }
            let call = t.get(i).is_some_and(|n| !n.string && n.text == "(");
            if let Some(i) = type_arg {
                names.truncate(i + 1);
            }
            if names.last() != Some(&r.reference_name) {
                continue;
            }
            let prefix = names
                .first()
                .filter(|name| self.dart_prefixes(&r.file_path).contains(*name))
                .cloned();
            let parts = &names[usize::from(prefix.is_some())..];
            let prefix = prefix.unwrap_or_default();
            let nodes = self
                .nodes_by_name(&r.reference_name)?
                .iter()
                .filter(|n| n.language == "dart")
                .cloned()
                .collect::<Vec<_>>();
            let node = if parts.len() == 1 {
                if !call && prefix.is_empty() {
                    let own = self
                        .nodes_in_file(&r.file_path)?
                        .iter()
                        .filter(|n| {
                            type_kind(n)
                                && n.id != r.from_node_id
                                && n.start_line <= r.line
                                && n.end_line >= r.line
                        })
                        .max_by_key(|n| n.start_line)
                        .cloned();
                    if let Some(own) = own {
                        if let Some(node) = nodes.iter().find(|n| {
                            n.file_path == own.file_path
                                && n.qualified_name
                                    == format!("{}::{}", own.qualified_name, r.reference_name)
                                && matches!(n.kind.as_str(), "constant" | "enum_member")
                        }) {
                            return self.finish_pre_framework(
                                r,
                                KCand {
                                    node: node.clone(),
                                    confidence: 0.9,
                                    resolved_by: "exact-match",
                                },
                            );
                        }
                    }
                }
                self.dart_pick(
                    nodes
                        .into_iter()
                        .filter(|n| {
                            if call {
                                n.kind == "class"
                            } else {
                                n.kind == "constant"
                            }
                        })
                        .collect(),
                    r,
                    &prefix,
                )?
            } else if parts.len() == 2 {
                let owners = self.dart_owners(&parts[0], r, &prefix)?;
                let mut pool = Vec::new();
                for n in nodes {
                    if owners.iter().any(|o| {
                        o.file_path == n.file_path
                            && n.qualified_name
                                == format!("{}::{}", o.qualified_name, r.reference_name)
                    }) && (if call {
                        self.dart_constructor(&n)
                    } else {
                        matches!(n.kind.as_str(), "constant" | "enum_member")
                    }) {
                        pool.push(n);
                    }
                }
                if pool.len() == 1 {
                    pool.pop()
                } else {
                    None
                }
            } else {
                None
            };
            return match node {
                Some(node) => self.finish_pre_framework(
                    r,
                    KCand {
                        node,
                        confidence: 0.9,
                        resolved_by: if parts.len() == 1 {
                            "exact-match"
                        } else {
                            "qualified-name"
                        },
                    },
                ),
                None => Ok(ResolveOutcome::unresolved()),
            };
        }
        Ok(ResolveOutcome::unresolved())
    }
}
fn dart_expr_end(t: &[DartToken]) -> usize {
    let mut depth = 0;
    for (i, n) in t.iter().enumerate() {
        if n.string {
            continue;
        }
        if matches!(n.text.as_str(), "(" | "[" | "{" | "<") {
            depth += 1;
        }
        if matches!(n.text.as_str(), ")" | "]" | "}" | ">") {
            if depth == 0 {
                return i;
            }
            depth -= 1;
        }
        if depth == 0 && matches!(n.text.as_str(), ";" | ",") {
            return i;
        }
    }
    t.len()
}

// Token offsets refer to the normalized whole file; reference columns use UTF-16.
fn token_position(source: &SourceFile, byte: usize) -> Option<(i64, i64)> {
    let mut start = 0;
    for (row, line) in source.iter().enumerate() {
        if byte <= start + line.len() {
            let prefix = line.get(..byte - start)?;
            return Some((row as i64 + 1, names::utf16_len(prefix) as i64));
        }
        start += line.len() + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_position_uses_its_line_and_utf16_column() {
        let source = SourceFile::new(vec!["// header".into(), "/* 🦊 */ kit".into(), "  .helper();".into()]);
        let byte = source.text().find("kit").unwrap();
        assert_eq!(token_position(&source, byte), Some((2, 9)));
        assert_eq!(token_position(&source, source.text().find("helper").unwrap()), Some((3, 3)));
        assert_eq!(token_position(&source, source.text().len() + 1), None);
    }
}
