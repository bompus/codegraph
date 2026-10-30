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
        // A composite literal's own type is its first token: `[]T{…}`,
        // `map[K]V{…}`, `&pkg.T{…}`.
        let lead = value.trim_start_matches('&').trim_start();
        if lead.starts_with('[') || lead.starts_with("map[") {
            let token: String = lead.chars().take_while(|c| !matches!(c, '{' | '(' | ' ' | '\t')).collect();
            return Ok(if token.is_empty() { GoPkgValue::Unknown } else { GoPkgValue::Written(token) });
        }
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
        // The name's own `[…]` is generics (`Pair[K,V]` — never empty); a
        // `[]`/`[N]` prefix is the declared type's own (`Items []Item`).
        let m = re!(r"^\s*[A-Za-z_]\w*(?:\[[A-Za-z_~][^\]]*\])?\s*(?:=\s*)?(\S[\s\S]*)$").captures(&decl)?;
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
                None => {
                    // `pkg.T` whose `pkg` is an import of no project package:
                    // an outside element — go_written_type vetoes it.
                    return Ok(if self.import_mappings(file)?.iter().any(|i| i.local_name == named[..p]) {
                        Some((named.clone(), file.to_string()))
                    } else {
                        None
                    });
                }
            },
            None => {
                // A builtin element (`error`) is no project type's either.
                if GO_BUILTIN_FIELD_TYPES.contains(named.as_str()) {
                    return Ok(Some((named.clone(), file.to_string())));
                }
                match self.go_bare_type_dir(&named, file)? {
                    Some(d) => (named.clone(), d),
                    None => return Ok(Some((named.clone(), file.to_string()))),
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
            Affix::new("", r"\s+(\S+)", true, false, false)
        });
        for s in self.go_package_types(type_name, dir)?.iter().filter(|n| n.kind == "struct") {
            let Some(source) = self.read_file(&s.file_path) else { continue };
            let start = (s.start_line - 1).max(0) as usize;
            let end = (s.end_line as usize).min(source.len());
            for raw in source.get(start..end).unwrap_or_default() {
                let line = strip_line_comments(raw);
                if let Some(t) = FIELD_OF.capture(&line, field) {
                    let mut ty = t.trim().to_string();
                    // A func type's results follow its parameter list, past
                    // spaces (`get func() A`), which the first token cuts off.
                    if ty.starts_with("func") {
                        let toks: Vec<&str> = line.split_whitespace().collect();
                        if let Some(i) = toks.iter().position(|w| *w == field) {
                            let joined = toks[i + 1..].join(" ");
                            // `get func() *A; other *B` — the next field is
                            // no part of this type, nor is a struct tag.
                            let joined = joined.split(';').next().unwrap_or(&joined);
                            let joined = joined.split('`').next().unwrap_or(joined);
                            ty = joined.trim_end_matches(['}', ';']).trim().to_string();
                        }
                    }
                    return Ok(Some((ty, s.file_path.clone())));
                }
                // An embedded field spells only its type — `KV` of `struct
                // { KV; name string }` answers `KV` for the field `KV`.
                let toks = line.split_whitespace().collect::<Vec<_>>();
                if let [only, ..] = toks.as_slice() {
                    if only.trim_start_matches('*').rsplit('.').next() == Some(field) {
                        return Ok(Some((only.to_string(), s.file_path.clone())));
                    }
                }
            }
        }
        Ok(None)
    }


    /// The `index`th result of a Go call's callee — `*Session` of `func
    /// New() (*Session, error)` at 0, `*Txn` of `func Begin() (int, *Txn)`
    /// at 1. A result that names the callee's own type parameter reduces to
    /// its constraint (`func Pick[V Validator](vs []V) V` → `Validator`);
    /// `any`/an interface literal leaves no named type.
    pub(super) fn go_call_result_at(&mut self, callee: &KNode, index: usize) -> Option<String> {
        // `() (int, *Txn)` — the results follow the params' closing `)`.
        let sig_tail = |c: &KNode| -> Option<String> {
            let s = c.signature.as_deref()?;
            let mut depth = 0usize;
            for (i, ch) in s.char_indices() {
                match ch {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return Some(s[i + 1..].trim().trim_end_matches('{').trim().to_string());
                        }
                    }
                    _ => {}
                }
            }
            None
        };
        let raw = if index == 0 {
            callee.return_type.clone().or_else(|| sig_tail(callee))?
        } else {
            // `return_type` keeps only the first result.
            sig_tail(callee).or(callee.return_type.clone())?
        };
        let raw = raw.trim();
        if raw.is_empty() || raw == "error" {
            return None;
        }
        // `(T, error)` and `[]T`/`T` alike reduce to the named result.
        let inner = raw.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(raw);
        let mut depth = 0usize;
        let mut starts = vec![0usize];
        let mut commas: Vec<usize> = Vec::new();
        for (i, c) in inner.char_indices() {
            match c {
                '[' | '(' | '{' => depth += 1,
                ']' | ')' | '}' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => { commas.push(i); starts.push(i + 1); }
                _ => {}
            }
        }
        if index >= starts.len() {
            return None;
        }
        let end = commas.get(index).copied().unwrap_or(inner.len());
        let nth = inner[starts[index]..end].trim();
        let nth = nth.rsplit(' ').next().unwrap_or(nth).trim();
        if nth.is_empty() || nth == "error" {
            return None;
        }
        // The result a type parameter spells is its constraint: `V` of
        // `F[V Validator]` carries Validator's methods, `T` of `F[T any]`
        // none. The stored signature drops `[…]`, so the declaration line
        // is read for it.
        if let Some(source) = self.read_file(&callee.file_path) {
            if let Some(line) = source.get((callee.start_line - 1).max(0) as usize) {
                if let Some(tps) = re!(r"func\s+(?:\([^)]*\)\s*)?[A-Za-z_]\w*\s*\[([^\]]+)\]")
                    .captures(line).map(|m| m[1].to_string())
                {
                    // `T1, T2 C` names share C; `K comparable, V any` pairs.
                    let mut pending: Vec<String> = Vec::new();
                    for chunk in tps.split(',') {
                        let toks: Vec<&str> = chunk.split_whitespace().collect();
                        match toks.as_slice() {
                            [name] => pending.push(name.to_string()),
                            [rest @ .., constraint] => {
                                for n in rest.iter().map(|s| s.trim_end_matches(',')).chain(pending.iter().map(|s| s.as_str())) {
                                    if n == nth {
                                        return Some(constraint.to_string());
                                    }
                                }
                                pending.clear();
                            }
                            [] => {}
                        }
                    }
                }
            }
        }
        Some(nth.to_string())
    }

    /// The initializer `names = values` gives the name `name_node`
    /// declares — its place among the names picks the value among the
    /// expression list (`in, out := &in.Items, &out.Items`).
    fn decl_rhs_node<'a>(name_node: TsNode<'a>, name: &str, text: &'a str) -> Option<(TsNode<'a>, usize, bool)> {
        use super::go_asserted::{identifiers, text_of};
        let mut owner = name_node.parent()?;
        if owner.kind() == "expression_list" {
            owner = owner.parent()?;
        }
        let (names, value, range) = match owner.kind() {
            "short_var_declaration" => {
                (identifiers(owner.child_by_field_name("left")?), owner.child_by_field_name("right")?, false)
            }
            // `for k, v := range coll` — the name's position means key/index
            // vs element, not a multi-result index.
            "range_clause" => {
                (identifiers(owner.child_by_field_name("left")?), owner.child_by_field_name("right")?, true)
            }
            "var_spec" | "const_spec" => {
                let mut c = owner.walk();
                (
                    owner.children_by_field_name("name", &mut c).collect::<Vec<_>>(),
                    owner.child_by_field_name("value")?,
                    false,
                )
            }
            _ => return None,
        };
        let k = names.iter().position(|n| text_of(*n, text) == name)?;
        if value.kind() == "expression_list" {
            if value.named_child_count() > k {
                return Some((value.named_child(k)?, 0, range));
            }
            // `a, b := f()` — the call's kth result types b.
            return Some((value.named_child(value.named_child_count() - 1)?, k, range));
        }
        Some((value, k, range))
    }

    /// The type `for k, v := range coll` binds `k`/`v` at — name position
    /// 0 takes the index (`int` for a slice, array or `string`) or key
    /// (`K` of `map[K]V`); position 1 takes the element/value. A `chan`
    /// range binds only its received value.
    fn go_range_pick(written: &str, pos: usize) -> String {
        let w = written.trim().trim_start_matches('*').trim();
        if let Some(rest) = w.strip_prefix("map[") {
            if let Some(end) = rest.find(']') {
                return (if pos == 0 { &rest[..end] } else { rest[end + 1..].trim() }).to_string();
            }
        }
        for p in ["chan ", "<-chan ", "chan<- "] {
            if let Some(rest) = w.strip_prefix(p) {
                return rest.trim().to_string();
            }
        }
        if pos == 0 {
            "int".to_string()
        } else if w.starts_with('[') {
            w.find(']').map(|e| w[e + 1..].trim().to_string()).unwrap_or_else(|| w.to_string())
        } else if w == "string" {
            "rune".to_string()
        } else {
            w.to_string()
        }
    }

    /// The type parameter `name`'s constraint in the function enclosing the
    /// ref — `model.Validator` of `F[T model.Validator]`, the interface
    /// literal of `F[T interface{ … }]`. `None` when `name` is no type
    /// parameter of an enclosing declaration.
    fn go_type_param_constraint(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        use super::go_asserted::text_of;
        let Some(file) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&file, r) else { return Ok(None) };
        let mut scope = super::iteration::descendant_for_position(
            tree.root_node(),
            file.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        );
        loop {
            if matches!(scope.kind(), "function_declaration" | "method_declaration" | "func_literal") {
                let Some(tps) = scope.child_by_field_name("type_parameters") else { return Ok(None) };
                let mut c = tps.walk();
                for decl in tps.named_children(&mut c) {
                    if decl.kind() != "type_parameter_declaration" {
                        continue;
                    }
                    let mut nc = decl.walk();
                    let names: Vec<_> = decl.children_by_field_name("name", &mut nc).collect();
                    if names.iter().any(|n| text_of(*n, file.text()) == name) {
                        return Ok(decl.child_by_field_name("type").map(|t| text_of(t, file.text()).to_string()));
                    }
                }
                return Ok(None);
            }
            let Some(parent) = scope.parent() else { return Ok(None) };
            scope = parent;
        }
    }

    /// The written type a Go local `name` carries at `anchor`'s site —
    /// declared type, asserted type, its `:=`/`var` initializer's type, or
    /// a package-level value's. `None` when nothing types it.
    fn go_named_local_written(
        &mut self,
        name: &str,
        anchor: TsNode,
        file_text: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<(String, String)>> {
        let file = r.file_path.clone();
        // The same walk `go_local_declaration_at` memoizes — the typed path
        // asks for this name at this site more than once per ref.
        let key = (file.clone(), name.to_string(), anchor.start_byte());
        let found = match self.go_local_decl_memo.get(&key) {
            Some(hit) => hit.clone(),
            None => {
                let hit = super::go_asserted::local_decl_at(file_text, name, anchor)
                    .map(|(t, decl)| (t.start_byte(), t.end_byte(), decl));
                self.go_local_decl_memo.insert(key, hit.clone());
                hit
            }
        };
        if let Some((ts, te, decl)) = found {
            if let Some(t) = decl.declared_type {
                return Ok(Some((t, file)));
            }
            if let Some(w) = self.go_asserted_local_type(name, r) {
                return Ok(Some((w, file)));
            }
            // An untyped `:=`/`var` — read the initializer itself:
            // `&m.F`, `f()`, `x[i]`, `[]T(v)` all carry a type.
            let target_tree = self.read_file(&file).and_then(|f| self.parsed_tree(&f, r));
            let target = target_tree
                .as_ref()
                .and_then(|tree| tree.root_node().descendant_for_byte_range(ts, te));
            if let Some((rhs, pos, range)) = target.and_then(|t| Self::decl_rhs_node(t, name, file_text)) {
                let w = self.go_expr_written_at(rhs, if range { 0 } else { pos }, file_text, r)?
                    .map(|(t, f)| if range { (Self::go_range_pick(&t, pos), f) } else { (t, f) });
                if let Some(t) = w {
                    return Ok(Some(t));
                }
            }
            if let Some(t) = self.infer_local_receiver_type(name, r, true)? {
                return Ok(Some((t, file)));
            }
            return Ok(None);
        }
        // A parameter or `:=` local the declaration reader missed —
        // the bound-receiver inference still types it.
        if let Some(t) = self.infer_local_receiver_type(name, r, true)? {
            return Ok(Some((t, file)));
        }
        for n in self.nodes_by_name(name)?.iter().filter(|n| {
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

    /// A Go receiver named `name` at `r`'s call site, typed through the
    /// same scoped local machinery `x[i].M` uses — the line-regex
    /// infer_local_receiver_type can't see `x := pkg.F().Field` or a
    /// shadowed name's outer binding.
    pub(super) fn go_local_receiver_written(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<(String, String)>> {
        let Some(file) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&file, r) else { return Ok(None) };
        let anchor = super::iteration::descendant_for_position(
            tree.root_node(),
            file.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        );
        let w = self.go_named_local_written(name, anchor, file.text(), r)?;
        Ok(w)
    }

    /// The type an expression's source text spells (`goReceiverChainType`,
    /// tree-native): `(written, file)`. `None` is unreadable — the other
    /// strategies decide.
    fn go_expr_written(&mut self, expr: TsNode, file_text: &str, r: &ResolveRefIn) -> Res<Option<(String, String)>> {
        self.go_expr_written_at(expr, 0, file_text, r)
    }

    /// `go_expr_written` whose top-level call takes its `result_idx`th
    /// result — `_, txn := store.Begin()` types `txn` by the second.
    fn go_expr_written_at(&mut self, expr: TsNode, result_idx: usize, file_text: &str, r: &ResolveRefIn) -> Res<Option<(String, String)>> {
        let file = r.file_path.clone();
        match expr.kind() {
            "parenthesized_expression" => match expr.named_child(0) {
                Some(inner) => self.go_expr_written(inner, file_text, r),
                None => Ok(None),
            },
            "identifier" => {
                let name = file_text[expr.byte_range()].to_string();
                self.go_named_local_written(&name, expr, file_text, r)
            }
            "index_expression" => {
                let Some(operand) = expr.child_by_field_name("operand") else { return Ok(None); };
                let Some((text, at)) = self.go_expr_written(operand, file_text, r)? else {
                    return Ok(None);
                };
                self.go_element_of(&text, &at, 0)
            }
            "slice_expression" => {
                // `x[a:b]` keeps the slice's own type — `[]T` stays `[]T`, a
                // named `Items` stays `Items`; `[N]T` cuts to `[]T`.
                let Some(operand) = expr.child_by_field_name("operand") else { return Ok(None); };
                let Some((text, at)) = self.go_expr_written(operand, file_text, r)? else {
                    return Ok(None);
                };
                let trimmed = text.trim_start_matches('*').trim_start();
                Ok(Some(if trimmed.starts_with('[') && trimmed[1..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    let p = trimmed.find(']').unwrap_or(1);
                    (format!("[]{}", trimmed[p + 1..].trim()), at)
                } else {
                    (trimmed.to_string(), at)
                }))
            }
            "selector_expression" => {
                let (Some(op), Some(field)) =
                    (expr.child_by_field_name("operand"), expr.child_by_field_name("field"))
                else {
                    return Ok(None);
                };
                let field = file_text[field.byte_range()].to_string();
                if op.kind() == "identifier" {
                    let base = file_text[op.byte_range()].to_string();
                    // `pkg.Name` — a qualified type, not a field read.
                    if self.go_imported_package_dir(&file, &base)?.is_some() {
                        return Ok(Some((format!("{base}.{field}"), file)));
                    }
                    let Some((base_ty, base_at)) = self.go_expr_written(op, file_text, r)? else {
                        return Ok(None);
                    };
                    // A literal type's package can't be named — the text
                    // itself settles the read (Outside), never a name cue.
                    let Some((tn, dir)) = self.go_type_package(&base_ty, &base_at)? else {
                        return Ok(Some((base_ty, base_at)));
                    };
                    return self.go_field_type(&tn, &field, &dir);
                }
                // `f().F`, `x[0].F`, `(a).F` — the field the operand's type
                // declares.
                let Some((w, at)) = self.go_expr_written(op, file_text, r)? else {
                    return Ok(None);
                };
                let Some((tn, dir)) = self.go_type_package(&w, &at)? else {
                    return Ok(Some((w, at)));
                };
                self.go_field_type(&tn, &field, &dir)
            }
            "type_assertion_expression" => Ok(expr
                .child_by_field_name("type")
                .map(|t| (file_text[t.byte_range()].trim().to_string(), file))),
            "type_conversion_expression" => {
                let Some(t) = expr.child_by_field_name("type") else { return Ok(None) };
                let text = file_text[t.byte_range()].trim().to_string();
                // `F[T](x)` parses as a conversion; when its type names a
                // function rather than a declared type it is a generic call
                // — the callee's result types it, not the "converted" name.
                if let Some(named) = Self::go_named_type(&text) {
                    let (q, name) = named.split_once('.')
                        .map(|(a, b)| (Some(a.to_string()), b.to_string()))
                        .unwrap_or((None, named));
                    let dir = match &q {
                        Some(q) => self.go_imported_package_dir(&file, q)?,
                        None => Some(pos_dirname(&file).to_string()),
                    };
                    if let Some(d) = dir {
                        if self.go_package_types(&name, &d)?.is_empty() {
                            for f in self.nodes_by_name(&name)?.iter().filter(|n| {
                                n.language == "go" && n.kind == "function" && pos_dirname(&n.file_path) == d
                            }) {
                                if let Some(t) = self.go_call_result_at(f, result_idx) {
                                    return Ok(Some((t, f.file_path.clone())));
                                }
                            }
                        }
                    }
                }
                Ok(Some((text, file)))
            }
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
                // `F[T](x)` — the callee sits under the instantiation as a
                // qualified_type/type_identifier, not the expression
                // grammar's selector/identifier shapes.
                let function = if function.kind() == "type_instantiation_expression" {
                    function.child_by_field_name("type").unwrap_or(function)
                } else {
                    function
                };
                if function.kind() == "qualified_type" {
                    let q = function.child_by_field_name("package").map(|n| file_text[n.byte_range()].to_string());
                    let callee = function.child_by_field_name("name").map(|n| file_text[n.byte_range()].to_string());
                    if let (Some(q), Some(callee)) = (q, callee) {
                        if self.go_is_outside_package(&q, &file)? {
                            return Ok(None);
                        }
                        if let Some(d) = self.go_imported_package_dir(&file, &q)? {
                            if !self.go_package_types(&callee, &d)?.is_empty() {
                                return Ok(Some((format!("{q}.{callee}"), file)));
                            }
                            for f in self.nodes_by_name(&callee)?.iter().filter(|n| {
                                n.language == "go" && n.kind == "function" && pos_dirname(&n.file_path) == d
                            }) {
                                if let Some(t) = self.go_call_result_at(f, result_idx) {
                                    return Ok(Some((t, f.file_path.clone())));
                                }
                            }
                        }
                        return Ok(None);
                    }
                }
                if function.kind() == "type_identifier" {
                    let callee = &file_text[function.byte_range()];
                    let own = pos_dirname(&file);
                    for f in self.nodes_by_name(callee)?.iter().filter(|n| {
                        n.language == "go" && n.kind == "function" && pos_dirname(&n.file_path) == own
                    }) {
                        if let Some(t) = self.go_call_result_at(f, result_idx) {
                            return Ok(Some((t, f.file_path.clone())));
                        }
                    }
                    return Ok(None);
                }
                // A conversion spells its result type where a call spells a
                // name: `[]T(x)`, `Pair[A,B](x)`, `map[K]V(x)`, `(*T)(x)`.
                if matches!(
                    function.kind(),
                    "slice_type" | "map_type" | "pointer_type" | "generic_type" | "array_type"
                ) {
                    return Ok(Some((file_text[function.byte_range()].trim().to_string(), file)));
                }
                if function.kind() == "parenthesized_expression" {
                    return Ok(function.named_child(0).map(|t| (file_text[t.byte_range()].trim().to_string(), file)));
                }
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
                            // A local or parameter of the package's name is the
                            // value, not the package: `shadowPkg(store *Holder)`
                            // makes `store.New()` the method, whose result types
                            // the call (#2543).
                            if !self.is_shadowed_import(q, r)? {
                                if let Some(d) = self.go_imported_package_dir(&file, q)? {
                                    if !self.go_package_types(callee, &d)?.is_empty() {
                                        return Ok(Some((format!("{q}.{callee}"), file)));
                                    }
                                    // `pkg.F(…)` — F's result.
                                    for f in self.nodes_by_name(callee)?.iter().filter(|n| {
                                        n.language == "go" && n.kind == "function" && pos_dirname(&n.file_path) == d
                                    }) {
                                        if let Some(t) = self.go_call_result_at(f, result_idx) {
                                            return Ok(Some((t, f.file_path.clone())));
                                        }
                                    }
                                    return Ok(None);
                                }
                            }
                            // `m.F(…)` — F's first result, on m's type.
                            let Some((base_ty, base_at)) = self.go_expr_written(operand, file_text, r)? else {
                                return Ok(None);
                            };
                            let Some((tn, dir)) = self.go_type_package(&base_ty, &base_at)? else {
                                // A qualified type of a package the project does not
                                // index names nothing the call can be read from; a
                                // literal type's text is the answer (Outside).
                                if base_ty.contains('.') { return Ok(None); }
                                return Ok(Some((base_ty, base_at)));
                            };
                            let want = format!("{tn}::{callee}");
                            let suffix = format!("::{want}");
                            for f in self.nodes_by_name(callee)?.iter().filter(|n| {
                                n.language == "go"
                                    && n.kind == "method"
                                    && pos_dirname(&n.file_path) == dir
                                    && (n.qualified_name == want
                                        || n.qualified_name.ends_with(&suffix))
                            }) {
                                if let Some(t) = self.go_call_result_at(f, result_idx) {
                                    return Ok(Some((t, f.file_path.clone())));
                                }
                            }
                            // `h.get()` — a field of func type called as a
                            // callee: the call's type is the func type's
                            // `result_idx`th result, not the func value.
                            if let Some((ft, at)) = self.go_field_type(&tn, callee, &dir)? {
                                if let Some(result) = go_func_type_result(&ft, result_idx) {
                                    return Ok(Some((result, at)));
                                }
                                return Ok(Some((ft, at)));
                            }
                            return Ok(None);
                        }
                        // `f().G(…)`, `x[0].G(…)` — the method's first
                        // result on the operand's type.
                        let callee = file_text[field.byte_range()].to_string();
                        let Some((w, at)) = self.go_expr_written(operand, file_text, r)? else {
                            return Ok(None);
                        };
                        let Some((tn, dir)) = self.go_type_package(&w, &at)? else {
                            if w.contains('.') { return Ok(None); }
                            return Ok(Some((w, at)));
                        };
                        let want = format!("{tn}::{callee}");
                        let suffix = format!("::{want}");
                        for f in self.nodes_by_name(&callee)?.iter().filter(|n| {
                            n.language == "go"
                                && n.kind == "method"
                                && pos_dirname(&n.file_path) == dir
                                && (n.qualified_name == want
                                    || n.qualified_name.ends_with(&suffix))
                        }) {
                            if let Some(t) = self.go_call_result_at(f, result_idx) {
                                return Ok(Some((t, f.file_path.clone())));
                            }
                        }
                        return Ok(None);
                    }
                }
                // `f(…)` on a project function — its result.
                if function.kind() == "identifier" {
                    let callee = &file_text[function.byte_range()];
                    let own = pos_dirname(&file);
                    for f in self.nodes_by_name(callee)?.iter().filter(|n| {
                        n.language == "go" && n.kind == "function" && pos_dirname(&n.file_path) == own
                    }) {
                        if let Some(t) = self.go_call_result_at(f, result_idx) {
                            return Ok(Some((t, f.file_path.clone())));
                        }
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Whether a `.go` file of package `dir` that the index read nothing
    /// from declares `type::method` — or itself can't be read (its methods
    /// are then unknowable): kubernetes' oversized generated.pb.go. The
    /// method the graph is missing is not the one an embedded type has.
    pub(super) fn go_unread_file_declares(&mut self, type_name: &str, method: &str, dir: &str) -> Res<bool> {
        let files = self.sorted_files().unwrap_or_default();
        let mut unread: Vec<String> = files
            .iter()
            .filter(|f| {
                f.ends_with(".go") && !f.ends_with("_test.go") && pos_dirname(f) == dir && self.known_file(f)
            })
            .filter(|f| self.nodes_in_file(f).map(|l| l.is_empty()).unwrap_or(true))
            .cloned()
            .collect();
        // A path the index never recorded still sits on disk.
        if let Ok(rd) = std::fs::read_dir(pos_resolve(&self.root_abs, dir)) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.ends_with(".go") || name.ends_with("_test.go") {
                    continue;
                }
                let f = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
                if !unread.contains(&f) && self.nodes_in_file(&f).map(|l| l.is_empty()).unwrap_or(true) {
                    unread.push(f);
                }
            }
        }
        for f in unread {
            let ms = self.go_unread_file_methods(&f)?;
            match ms {
                None => return Ok(true),
                Some(ms) if ms.contains(&format!("{type_name}::{method}")) => return Ok(true),
                _ => {}
            }
        }
        Ok(false)
    }

    /// `Type::Method` names a file the index couldn't read declares, from
    /// its text (`goUnreadFileMethods`). `None` when the file can't be read.
    fn go_unread_file_methods(&mut self, f: &str) -> Res<Option<std::collections::HashSet<String>>> {
        if let Some(hit) = self.go_unread_methods_memo.get(f) {
            return Ok(hit.clone());
        }
        // GO_UNREAD_SCAN_BYTES — the sweep reads far past the indexer's 1
        // MB cap, which is exactly why these files are unread.
        const GO_UNREAD_SCAN_BYTES: u64 = 32 * 1024 * 1024;
        let path = pos_resolve(&self.root_abs, f);
        let methods = (|| {
            let meta = std::fs::metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > GO_UNREAD_SCAN_BYTES {
                return None;
            }
            let raw = std::fs::read(&path).ok()?;
            let text = String::from_utf8(raw).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            let mut set = std::collections::HashSet::new();
            let re = re!(r"(?m)^func\s*\(\s*(?:[A-Za-z_]\w*\s+)?\*?\s*([A-Za-z_]\w*)\s*(?:\[[^\]\n]*\])?\s*\)\s*([A-Za-z_]\w*)\s*[\[(]");
            for m in re.captures_iter(&text) {
                set.insert(format!("{}::{}", &m[1], &m[2]));
            }
            Some(set)
        })();
        self.go_unread_methods_memo.insert(f.to_string(), methods.clone());
        Ok(methods)
    }

    /// The settled type of `expr` where `r` is read: a project type with its
    /// package, `Outside`, or `None` (unreadable — name matching decides).
    fn go_expr_type(&mut self, expr: TsNode, file_text: &str, r: &ResolveRefIn) -> Res<Option<GoAsserted>> {
        let Some((written, at)) = self.go_expr_written(expr, file_text, r)? else { return Ok(None); };
        // A type parameter is its constraint, never the package's namesake
        // type: `F[T interface{…}]` → literal (nothing to read), `F[T
        // model.Validator]` → the constraint's type.
        if let Some(named) = Self::go_named_type(&written) {
            if !named.contains('.') {
                if let Some(constraint) = self.go_type_param_constraint(&named, r)? {
                    return match Self::go_named_type(&constraint) {
                        Some(_) => self.go_written_type(&constraint, r),
                        None => Ok(Some(GoAsserted::Outside)),
                    };
                }
            }
        }
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
                            if file.text()[field.byte_range()] == r.reference_name {
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
            _ => {
                let ty = self.go_expr_type(operand, file.text(), r)?;
                Ok(match ty {
                    Some(GoAsserted::Project { name, dir }) => {
                        Some(self.go_method_in_package(&name, &r.reference_name, &dir, r)?)
                    }
                    Some(GoAsserted::Outside) => Some(None),
                    None => None,
                })
            }
        }
    }
}

/// The `index`th result a written Go func type declares: `func() A` is `A`,
/// `func(int) (A, error)` at 0 is `A` and at 1 `error` (none). A func value's
/// call has the type its declaration spells, which is what a func-typed
/// field's call gives (`h.get().Run()` on `get func() A`).
fn go_func_type_result(ft: &str, index: usize) -> Option<String> {
    let tail = ft.trim().strip_prefix("func")?;
    let mut depth = 0usize;
    let mut params_end = None;
    for (i, c) in tail.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 { params_end = Some(i + 1); break; }
            }
            _ => {}
        }
    }
    let raw = tail[params_end?..].trim().trim_end_matches('{').trim();
    let inner = raw.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(raw);
    let mut depth = 0usize;
    let mut starts = vec![0usize];
    let mut commas: Vec<usize> = Vec::new();
    for (i, c) in inner.char_indices() {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => { commas.push(i); starts.push(i + 1); }
            _ => {}
        }
    }
    if index >= starts.len() { return None; }
    let end = commas.get(index).copied().unwrap_or(inner.len());
    let nth = inner[starts[index]..end].trim();
    let nth = nth.rsplit(' ').next().unwrap_or(nth).trim();
    if nth.is_empty() || nth == "error" { None } else { Some(nth.to_string()) }
}
