//! Go package-value typing and expression-typed call arms — the tree-native
//! ports of name-matcher.ts `matchGoPackageValueCall`, `matchGoIndexedCall`
//! and `matchGoChainedCall` (`goPackageValueType`/`goValueType`/
//! `goNamedType`/`goReceiverChainType` semantics).

use super::go_asserted::GoAsserted;
use super::*;
use tree_sitter::Node as TsNode;

/// What a package-level var/const's declaration says its type is
/// (`goPackageValueType`): `Written` the spec spells or its value reads,
/// `Outside` one an outside package hands out, `Unknown` nothing readable.
enum GoPkgValue {
    Written(String),
    Outside,
    Unknown,
}

impl KernelResolver {
    /// goNamedType — `*?ident(.ident)?[type-args]?` reduced to `ident` or
    /// `pkg.ident`; anything else (a literal like `[]T`, `map[K]V`) is no
    /// named type.
    pub(super) fn go_named_type(written: &str) -> Option<String> {
        static NAMED: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^\*?\s*([A-Za-z_]\w*)\s*(?:\.\s*([A-Za-z_]\w*))?\s*(?:\[[\s\S]*\])?$").unwrap()
        });
        let cap = NAMED.captures(written.trim())?;
        Some(match cap.get(2) {
            Some(second) => format!("{}.{}", &cap[1], second.as_str()),
            None => cap[1].to_string(),
        })
    }

    /// isGoOutsidePackage — `pkg` names an import of `file` whose package is
    /// no project directory.
    pub(super) fn go_is_outside_package(&mut self, pkg: &str, file: &str) -> Res<bool> {
        Ok(self.import_mappings(file)?.iter().any(|i| i.local_name == pkg)
            && self.go_imported_package_dir(file, pkg)?.is_none())
    }

    /// goPackageValueType — the type a package-level `var`/`const` node's
    /// spec writes: an explicit `var x T`, the spec a `const` group member
    /// repeats, or the type a composite literal / conversion value gives.
    fn go_package_value_type(&mut self, n: &KNode) -> Res<GoPkgValue> {
        let Some(file) = self.read_file(&n.file_path) else { return Ok(GoPkgValue::Unknown); };
        static SPEC: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^\s*(?:(?:var|const)\s*(?:\(\s*)?)?([A-Za-z_]\w*(?:\s*,\s*[A-Za-z_]\w*)*)(.*)$").unwrap()
        });
        let start = (n.start_line - 1).max(0) as usize;
        let mut i = start as i64;
        while i >= 0 && start as i64 - i < 200 {
            let raw = file.text().lines().nth(i as usize).unwrap_or("").to_string();
            i -= 1;
            let line = strip_line_comments(&raw);
            let own = i as usize + 1 == start;
            if !own && (line.trim().is_empty() || line.trim_start().starts_with("/*") || line.trim_start().starts_with('*')) {
                continue;
            }
            let Some(cap) = SPEC.captures(&line) else { return Ok(GoPkgValue::Unknown); };
            if own && !cap[1].split(',').map(str::trim).any(|x| x == n.name) {
                return Ok(GoPkgValue::Unknown);
            }
            let rest = cap[2].trim();
            if rest.is_empty() {
                // A constant alone in its group repeats the spec above it.
                if n.kind != "constant" || line.trim_start().starts_with("var") || line.trim_start().starts_with("const") {
                    return Ok(GoPkgValue::Unknown);
                }
                continue;
            }
            let eq = rest.find('=').filter(|&p| !rest[p + 1..].starts_with('='));
            let written = match eq {
                Some(p) => rest[..p].trim(),
                None => rest,
            };
            if written.chars().any(|c| matches!(c, ':' | '"' | '\'' | '`')) {
                return Ok(GoPkgValue::Unknown);
            }
            if !written.is_empty() {
                return Ok(GoPkgValue::Written(written.to_string()));
            }
            let Some(p) = eq else { return Ok(GoPkgValue::Unknown); };
            return self.go_value_type(&rest[p + 1..], &n.file_path);
        }
        Ok(GoPkgValue::Unknown)
    }

    /// goValueType — the type a declaration value gives: a composite
    /// literal's (`&Registry{}`, `pkg.T{…}`) or a conversion's (`Role("x")`,
    /// `pkg.T(x)`). Outside for what an outside package hands out.
    fn go_value_type(&mut self, value: &str, file: &str) -> Res<GoPkgValue> {
        static VAL: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^\s*&?\s*(?:([A-Za-z_]\w*)\s*\.\s*)?([A-Za-z_]\w*)\s*(?:\[[^\]\n]*\]\s*)?([({])").unwrap()
        });
        let Some(cap) = VAL.captures(value) else { return Ok(GoPkgValue::Unknown); };
        let qualifier = cap.get(1).map(|m| m.as_str());
        let name = cap[2].to_string();
        let open = cap[3].to_string();
        if let Some(q) = qualifier {
            if self.go_is_outside_package(q, file)? {
                return Ok(GoPkgValue::Outside);
            }
        }
        let written = match qualifier {
            Some(q) => format!("{q}.{name}"),
            None => name.clone(),
        };
        if open == "{" {
            return Ok(if GO_BUILTIN_FIELD_TYPES.contains(name.as_str()) || matches!(name.as_str(), "any" | "error") {
                GoPkgValue::Outside
            } else {
                GoPkgValue::Written(written)
            });
        }
        // A call is a conversion when it names a type of its package.
        let dir = match qualifier {
            Some(q) => self.go_imported_package_dir(file, q)?,
            None => self.go_bare_type_dir(&name, file)?,
        };
        Ok(match dir {
            Some(dir) if !self.go_package_types(&name, &dir)?.is_empty() => GoPkgValue::Written(written),
            _ => GoPkgValue::Unknown,
        })
    }

    /// matchGoPackageValueCall — `receiver.method` on a package-level
    /// var/const whose declaration says its type (`var level = Level(2)`).
    /// `Some(verdict)` claims the ref; `verdict=None` is a terminal miss on
    /// an outside type. `None` leaves the other strategies to decide.
    pub(super) fn match_go_package_value_call(
        &mut self,
        receiver: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<KCand>>> {
        let dir = pos_dirname(&r.file_path);
        let decls: Vec<Arc<KNode>> = self
            .nodes_by_name(receiver)?
            .iter()
            .filter(|n| {
                n.language == "go"
                    && matches!(n.kind.as_str(), "variable" | "constant")
                    && pos_dirname(&n.file_path) == dir
            })
            .cloned()
            .collect();
        if decls.is_empty() {
            return Ok(None);
        }
        if self.import_mappings(&r.file_path)?.iter().any(|i| i.local_name == receiver) {
            return Ok(None);
        }
        if self
            .go_local_declaration_at(receiver.to_string(), r.file_path.clone(), r.line, r.column)?
            .is_some()
        {
            return Ok(None);
        }
        let pkg = self.go_package_clause(&r.file_path);
        let mut agreed: Option<String> = None;
        for d in &decls {
            if self.go_package_clause(&d.file_path) != pkg {
                continue;
            }
            let written = match self.go_package_value_type(d)? {
                GoPkgValue::Outside => return Ok(Some(None)),
                GoPkgValue::Unknown => return Ok(None),
                GoPkgValue::Written(w) => w.replace(char::is_whitespace, ""),
            };
            // Files for other build targets may declare it differently.
            if agreed.as_deref().is_some_and(|seen| seen != written) {
                return Ok(None);
            }
            agreed = Some(written);
        }
        let Some(written) = agreed else { return Ok(None); };
        let Some(named) = Self::go_named_type(&written) else { return Ok(Some(None)); };
        let mut site = r.clone();
        site.file_path = decls[0].file_path.clone();
        if matches!(self.go_written_type(&written, &site)?, Some(GoAsserted::Outside)) {
            return Ok(Some(None));
        }
        let dot = named.find('.');
        let name = match dot {
            Some(p) => named[p + 1..].to_string(),
            None => named.clone(),
        };
        if dot.is_none() && GO_BUILTIN_FIELD_TYPES.contains(name.as_str()) {
            return Ok(Some(None));
        }
        let pkg_dir = match dot {
            Some(p) => self.go_imported_package_dir(&site.file_path, &named[..p])?,
            None => self.go_bare_type_dir(&name, &site.file_path)?,
        };
        if pkg_dir.is_none() && dot.is_none() {
            return Ok(Some(None));
        }
        let Some(pkg_dir) = pkg_dir else { return Ok(None); };
        if self.go_package_types(&name, &pkg_dir)?.is_empty() {
            return Ok(None);
        }
        // Upstream resolves the package-value arm at the typed-receiver
        // confidence, 0.9.
        Ok(Some(self.go_method_in_package(&name, method, &pkg_dir, r)?.map(|mut c| {
            c.confidence = 0.9;
            c
        })))
    }

    /// The text after the name in a Go type declaration's node span —
    /// `[]Item` of `type Items []Item`, `B` of `type A = B`.
    fn go_type_decl_rhs(&mut self, node: &KNode) -> Option<String> {
        let lines = self.read_file(&node.file_path)?;
        let decl = lines.iter()
            .skip((node.start_line - 1).max(0) as usize)
            .take((node.end_line - node.start_line + 1).max(0) as usize)
            .enumerate()
            .map(|(i, l)| {
                let s = if i == 0 { &l[node.start_column.max(0) as usize..] } else { l.as_str() };
                strip_line_comments(s).to_string()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let decl = re!(r"/\*.*?\*/").replace_all(&decl, "");
        let m = re!(r"^\s*[A-Za-z_]\w*\s*(?:\[[^\]]*\])?\s*(?:=\s*)?(\S.*)$").captures(&decl)?;
        Some(m[1].trim().trim_end_matches(';').trim().to_string()).filter(|s| !s.is_empty())
    }

    /// goElementOf (name-matcher.ts): the element one `x[i]` on a value of
    /// type `written` gives — `T` of `[]T`/`[N]T`, `V` of `map[K]V`, and of
    /// a named type what its declaration's element gives. Returns the
    /// element's written text and the file it is read in.
    fn go_element_of(&mut self, written: &str, file: &str, depth: u8) -> Res<Option<(String, String)>> {
        if depth > 4 {
            return Ok(None);
        }
        let text = written.trim().trim_start_matches('*').trim_start().to_string();
        if let Some(rest) = text.strip_prefix("map[") {
            return Ok(rest.find(']').map(|p| (rest[p + 1..].trim().to_string(), file.to_string())));
        }
        if text.starts_with('[') {
            return Ok(text.find(']').map(|p| (text[p + 1..].trim().to_string(), file.to_string())));
        }
        let Some(named) = Self::go_named_type(&text) else { return Ok(None) };
        let (name, dir) = match named.find('.') {
            Some(p) => match self.go_imported_package_dir(file, &named[..p])? {
                Some(d) => (named[p + 1..].to_string(), d),
                None => return Ok(None),
            },
            None => {
                if GO_BUILTIN_FIELD_TYPES.contains(named.as_str()) {
                    return Ok(None);
                }
                match self.go_bare_type_dir(&named, file)? {
                    Some(d) => (named.clone(), d),
                    None => return Ok(None),
                }
            }
        };
        for decl in self.go_package_types(&name, &dir)? {
            if let Some(rhs) = self.go_type_decl_rhs(&decl) {
                if rhs != text
                    && (rhs.starts_with('[')
                        || rhs.starts_with("map[")
                        || rhs.starts_with('*')
                        || Self::go_named_type(&rhs).is_some())
                {
                    return self.go_element_of(&rhs, &decl.file_path, depth + 1);
                }
            }
        }
        Ok(None)
    }

    /// goFieldTypeOf (name-matcher.ts): the type `field` of the Go struct
    /// `type_name` in `dir` is declared with — the written text, read in the
    /// struct's own file.
    fn go_field_type(&mut self, type_name: &str, field: &str, dir: &str) -> Res<Option<(String, String)>> {
        static FIELD_OF: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new("", r"\s+(\*?(?:map\[[^\]]+\])?\[?\]?\s*[A-Za-z_][A-Za-z0-9_.]*(?:\[[^\]]*\])?)", true, false, false)
        });
        for s in self.go_package_types(type_name, dir)?.iter().filter(|n| n.kind == "struct") {
            let Some(source) = self.read_file(&s.file_path) else { continue };
            let start = (s.start_line - 1).max(0) as usize;
            let end = (s.end_line as usize).min(source.len());
            for raw in source.get(start..end).unwrap_or_default() {
                let line = strip_line_comments(raw);
                if let Some(t) = FIELD_OF.capture(&line, field) {
                    return Ok(Some((t.trim().to_string(), s.file_path.clone())));
                }
            }
        }
        Ok(None)
    }

    /// The first result of a Go call's callee — `*Session` of
    /// `func New() (*Session, error)`, from its `return_type` or the tail of
    /// its signature.
    fn go_call_result(&self, callee: &KNode) -> Option<String> {
        let raw = callee.return_type.clone().or_else(|| {
            let s = callee.signature.as_deref()?;
            let close = s.match_indices(')').map(|(i, _)| i).next_back()?;
            Some(s[close + 1..].trim().trim_end_matches('{').trim().to_string())
        })?;
        let raw = raw.trim();
        if raw.is_empty() || raw == "error" {
            return None;
        }
        // `(T, error)` and `[]T`/`T` alike reduce to the first result.
        let inner = raw.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(raw);
        let first = inner.split(',').next()?.trim();
        Some(first.to_string()).filter(|s| !s.is_empty() && s != "error")
    }

    /// The type an expression's source text spells (`goReceiverChainType`,
    /// tree-native): `(written, file)`. `None` is unreadable — the other
    /// strategies decide.
    fn go_expr_written(&mut self, expr: TsNode, file_text: &str, r: &ResolveRefIn) -> Res<Option<(String, String)>> {
        let file = r.file_path.clone();
        match expr.kind() {
            "parenthesized_expression" => match expr.named_child(0) {
                Some(inner) => self.go_expr_written(inner, file_text, r),
                None => Ok(None),
            },
            "identifier" => {
                let name = file_text[expr.byte_range()].to_string();
                if let Some(decl) =
                    self.go_local_declaration_at(name.clone(), file.clone(), r.line, r.column)?
                {
                    if let Some(t) = decl.declared_type {
                        return Ok(Some((t, file)));
                    }
                    if let Some(w) = self.go_asserted_local_type(&name, r) {
                        return Ok(Some((w, file)));
                    }
                    if let Some(t) = self.infer_local_receiver_type(&name, r, true)? {
                        return Ok(Some((t, file)));
                    }
                    return Ok(None);
                }
                // A parameter or `:=` local the declaration reader missed —
                // the bound-receiver inference still types it.
                if let Some(t) = self.infer_local_receiver_type(&name, r, true)? {
                    return Ok(Some((t, file)));
                }
                for n in self.nodes_by_name(&name)?.iter().filter(|n| {
                    n.language == "go"
                        && matches!(n.kind.as_str(), "variable" | "constant")
                        && pos_dirname(&n.file_path) == pos_dirname(&file)
                }) {
                    if let GoPkgValue::Written(w) = self.go_package_value_type(n)? {
                        return Ok(Some((w, n.file_path.clone())));
                    }
                }
                Ok(None)
            }
            "index_expression" | "slice_expression" => {
                let Some(operand) = expr.child_by_field_name("operand") else { return Ok(None); };
                let Some((text, at)) = self.go_expr_written(operand, file_text, r)? else {
                    return Ok(None);
                };
                self.go_element_of(&text, &at, 0)
            }
            "selector_expression" => {
                let (Some(op), Some(field)) =
                    (expr.child_by_field_name("operand"), expr.child_by_field_name("field"))
                else {
                    return Ok(None);
                };
                if op.kind() != "identifier" {
                    return Ok(None);
                }
                let base = file_text[op.byte_range()].to_string();
                let field = file_text[field.byte_range()].to_string();
                // `pkg.Name` — a qualified type, not a field read.
                if self.go_imported_package_dir(&file, &base)?.is_some() {
                    return Ok(Some((format!("{base}.{field}"), file)));
                }
                let Some(base_ty) = self.infer_local_receiver_type(&base, r, true)? else {
                    return Ok(None);
                };
                let Some((tn, dir)) = self.go_type_package(&base_ty, &file)? else {
                    return Ok(None);
                };
                self.go_field_type(&tn, &field, &dir)
            }
            "type_assertion_expression" => Ok(expr
                .child_by_field_name("type")
                .map(|t| (file_text[t.byte_range()].trim().to_string(), file))),
            "composite_literal" => Ok(expr
                .child_by_field_name("type")
                .map(|t| (file_text[t.byte_range()].trim().to_string(), file))),
            "unary_expression" => {
                let op = expr.child(0).map(|n| &file_text[n.byte_range()]).unwrap_or("");
                let Some(inner) = expr.named_child(0) else { return Ok(None); };
                let Some((t, at)) = self.go_expr_written(inner, file_text, r)? else { return Ok(None); };
                Ok(match op {
                    "*" => t.strip_prefix('*').map(|s| (s.trim().to_string(), at)),
                    "&" => Some((format!("*{t}"), at)),
                    _ => None,
                })
            }
            "call_expression" => {
                let Some(function) = expr.child_by_field_name("function") else { return Ok(None); };
                if function.kind() == "identifier" {
                    let callee = &file_text[function.byte_range()];
                    if matches!(callee, "new" | "make") {
                        return Ok(expr
                            .child_by_field_name("arguments")
                            .and_then(|a| a.named_child(0))
                            .map(|t| (file_text[t.byte_range()].trim().to_string(), file)));
                    }
                    // A conversion names a type of its package.
                    let dir = self.go_bare_type_dir(callee, &file)?;
                    if dir.is_some_and(|d| !self.go_package_types(callee, &d).unwrap_or_default().is_empty()) {
                        return Ok(Some((callee.to_string(), file)));
                    }
                }
                if function.kind() == "selector_expression" {
                    if let (Some(operand), Some(field)) =
                        (function.child_by_field_name("operand"), function.child_by_field_name("field"))
                    {
                        if operand.kind() == "identifier" {
                            let q = &file_text[operand.byte_range()];
                            let callee = &file_text[field.byte_range()];
                            if self.go_is_outside_package(q, &file)? {
                                return Ok(None);
                            }
                            if let Some(d) = self.go_imported_package_dir(&file, q)? {
                                if !self.go_package_types(callee, &d)?.is_empty() {
                                    return Ok(Some((format!("{q}.{callee}"), file)));
                                }
                                // `pkg.F(…)` — F's first result.
                                for f in self.nodes_by_name(callee)?.iter().filter(|n| {
                                    n.language == "go" && n.kind == "function" && pos_dirname(&n.file_path) == d
                                }) {
                                    if let Some(t) = self.go_call_result(f) {
                                        return Ok(Some((t, f.file_path.clone())));
                                    }
                                }
                                return Ok(None);
                            }
                            // `m.F(…)` — F's first result, on m's type.
                            let Some(base_ty) = self.infer_local_receiver_type(q, r, true)? else {
                                return Ok(None);
                            };
                            let Some((tn, dir)) = self.go_type_package(&base_ty, &file)? else {
                                return Ok(None);
                            };
                            for f in self.nodes_by_name(callee)?.iter().filter(|n| {
                                n.language == "go"
                                    && n.kind == "method"
                                    && pos_dirname(&n.file_path) == dir
                                    && n.qualified_name.contains(&format!("::{tn}"))
                            }) {
                                if let Some(t) = self.go_call_result(f) {
                                    return Ok(Some((t, f.file_path.clone())));
                                }
                            }
                            return Ok(None);
                        }
                    }
                }
                // `f(…)` on a project function — its first result.
                if function.kind() == "identifier" {
                    let callee = &file_text[function.byte_range()];
                    let own = pos_dirname(&file);
                    for f in self.nodes_by_name(callee)?.iter().filter(|n| {
                        n.language == "go" && n.kind == "function" && pos_dirname(&n.file_path) == own
                    }) {
                        if let Some(t) = self.go_call_result(f) {
                            return Ok(Some((t, f.file_path.clone())));
                        }
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// The settled type of `expr` where `r` is read: a project type with its
    /// package, `Outside`, or `None` (unreadable — name matching decides).
    fn go_expr_type(&mut self, expr: TsNode, file_text: &str, r: &ResolveRefIn) -> Res<Option<GoAsserted>> {
        let Some((written, at)) = self.go_expr_written(expr, file_text, r)? else { return Ok(None); };
        let mut site = r.clone();
        site.file_path = at;
        self.go_written_type(&written, &site)
    }

    /// matchGoIndexedCall + matchGoChainedCall — a bare `calls` name whose
    /// enclosing call selects the method on an index, call, assertion,
    /// composite or selector receiver: `gs[0].Expect`, `f().M`, `v.(T).M`,
    /// `x.f.M`. The receiver's type owns the ref: a project type names the
    /// method (or nothing — terminal), an outside type refuses it, and an
    /// unreadable one leaves name matching to decide.
    pub(super) fn match_go_expr_call(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "go" || r.reference_kind != "calls" || r.reference_name.contains('.') {
            return Ok(None);
        }
        let Some(file) = self.read_file(&r.file_path) else { return Ok(None); };
        let Some(tree) = self.parsed_tree(&file, r) else { return Ok(None); };
        let mut node = super::iteration::descendant_for_position(
            tree.root_node(),
            file.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        );
        let mut selector = None;
        loop {
            if node.kind() == "call_expression" {
                if let Some(function) = node.child_by_field_name("function") {
                    if function.kind() == "selector_expression" {
                        if let Some(field) = function.child_by_field_name("field") {
                            if &file.text()[field.byte_range()] == r.reference_name {
                                selector = Some(function);
                                break;
                            }
                        }
                    }
                }
            }
            let Some(parent) = node.parent() else { break };
            if matches!(parent.kind(), "argument_list" | "block" | "source_file") {
                break;
            }
            node = parent;
        }
        let Some(selector) = selector else { return Ok(None); };
        let Some(operand) = selector.child_by_field_name("operand") else { return Ok(None); };
        let mut operand = operand;
        while operand.kind() == "parenthesized_expression" {
            let Some(inner) = operand.named_child(0) else { return Ok(None); };
            operand = inner;
        }
        match operand.kind() {
            // `x.M` arrives as a dotted ref, never a bare name.
            "identifier" => Ok(None),
            _ => Ok(match self.go_expr_type(operand, file.text(), r)? {
                Some(GoAsserted::Project { name, dir }) => {
                    Some(self.go_method_in_package(&name, &r.reference_name, &dir, r)?)
                }
                Some(GoAsserted::Outside) => Some(None),
                None => None,
            }),
        }
    }
}
