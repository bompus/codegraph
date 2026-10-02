//! Receiver type inference and call chains (name-matcher.ts inferLocalReceiverType and friends).

use super::*;

impl KernelResolver {
    // -----------------------------------------------------------------------
    // Stage-2 member-access arms — matchMethodCall(requireReceiverEvidence)
    // and its source-backed inference helpers (name-matcher.ts). Every helper
    // returns `Some` for a proven edge and `None` for a miss.
    // -----------------------------------------------------------------------

    /// enclosingScopeStartLine — 1-based start line of the tightest
    /// function/method node enclosing `line` in `file_path`.
    pub(super) fn enclosing_scope_start_line(
        &mut self,
        file_path: &str,
        language: &str,
        line: i64,
    ) -> Res<i64> {
        let mut start = 1i64;
        for n in self.nodes_in_file(file_path)?.iter() {
            if n.kind != "function" && n.kind != "method" {
                continue;
            }
            if n.language != language {
                continue;
            }
            if n.start_line <= line && n.end_line >= line && n.start_line >= start {
                start = n.start_line;
            }
        }
        Ok(start)
    }

    /// normalizeInferredTypeName — strip generics + `&`/`*`, take the last
    /// `.`/`:`-separated segment, reject non-type tokens.
    pub(super) fn normalize_inferred_type_name(&mut self, raw: &str) -> Res<Option<String>> {
        let generics = re!("<[^>]*>");
        let cleaned = generics.replace_all(raw, "");
        let cleaned: String = cleaned
            .chars()
            .filter(|c| *c != '&' && *c != '*')
            .collect::<String>()
            .trim()
            .to_string();
        let Some(seg) = cleaned
            .split(['.', ':'])
            .rfind(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        if NON_TYPE_RECEIVER_TOKENS.contains(seg) {
            return Ok(None);
        }
        Ok(Some(seg.to_string()))
    }

    /// First per-pattern match for one source line — mirrors `matchLine` in
    /// inferLocalReceiverType: per pattern only the first match position is
    /// considered (non-global `.match`), then the captured type must survive
    /// normalizeInferredTypeName. `guard == 1` reproduces the TS annotation
    /// pattern's negative lookahead `(?![\w.$]|\s*(?:<[^>]*>)?\s*[\[|&])`; a
    /// shrunk capture can't satisfy it (the released char is itself `[\w.$]`),
    /// so checking the greedy capture's tail at each start position is exact.
    pub(super) fn infer_match_line(
        &mut self,
        line: &str,
        receiver: &str,
        pats: &'static [ReceiverPattern],
        preserve: bool,
    ) -> Res<Option<String>> {
        if utf16_len_exceeds(line, 10_000) {
            return Ok(None);
        }
        self.infer_match_text(line, receiver, pats, preserve)
    }

    /// infer_match_line without the long-line cap: resolveImportedInstanceMember
    /// applies the patterns to a value's whole declaration, joined, uncapped.
    pub(super) fn infer_match_text(
        &mut self,
        line: &str,
        receiver: &str,
        pats: &'static [ReceiverPattern],
        preserve: bool,
    ) -> Res<Option<String>> {
        for pat in pats {
            let mut from = 0;
            while let Some(m) = pat.affix.find_from(line, receiver, from) {
                let Some((gs, ge)) = m.group else { break };
                let m1 = &line[gs..ge];
                if m1.is_empty() {
                    break;
                }
                // The next match starts after this one, as captures_iter's does.
                from = m.end;
                if pat.guard == 1 {
                    let rest = &line[m.end..];
                    if rest.chars().next().is_some_and(|c| {
                        c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$'
                    }) {
                        continue;
                    }
                    if guard1_tail_re().is_match(rest) && !nullable_union_tail_re().is_match(rest) {
                        continue;
                    }
                } else if pat.guard == 4 {
                    // An assignment statement, not a trailing keyword argument.
                    if line[m.end..].split('\n').next().is_some_and(|rest| rest.trim_end().ends_with(',')) { continue; }
                } else if pat.guard == 3 {
                    // `(?!\s*\()` — a declarator followed by `(` is a function.
                    if cpp_call_opener_re().is_match(&line[m.end..]) {
                        continue;
                    }
                } else if pat.guard == 2 {
                    // `(?![\w.]|\s*[({"'\[])` — the greedy capture already
                    // consumed every `[\w.]`, and a shrunk capture would be
                    // followed by one, so the call-form tail is the whole gate.
                    let rest = &line[m.end..];
                    if rest
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                    {
                        continue;
                    }
                    if guard2_tail_re().is_match(rest) {
                        continue;
                    }
                }
                match self.normalize_inferred_type_name(m1)? {
                    Some(t) => {
                        return Ok(Some(if preserve { m1.to_string() } else { t }));
                    }
                    None => break,
                }
            }
        }
        Ok(None)
    }

    /// inferLocalReceiverType (name-matcher.ts) — backward declaration scan
    /// bounded by the enclosing scope. The TS incremental-scan memo is a pure
    /// optimization: it returns the highest matching line in [start..call],
    /// identical to the plain backward scan reproduced here.
    pub(super) fn infer_local_receiver_type(
        &mut self,
        receiver: &str,
        site: &ResolveRefIn,
        preserve: bool,
    ) -> Res<Option<String>> {
        if site.language == "cfml" || site.language == "cfscript" {
            return self.infer_cfml_receiver_type(receiver, site, preserve);
        }
        let mut scan_receiver = receiver.to_string();
        let mut component_scoped = false;
        let mut php_property = false;
        if site.language == "php" {
            let re = re!("^this->(.+)$");
            if let Some(m) = re.captures(&scan_receiver) {
                scan_receiver = m[1].to_string();
                component_scoped = true;
                php_property = true;
            }
        }
        let pats: &'static [ReceiverPattern] = if php_property {
            &PHP_PROPERTY_TYPE_PATTERNS
        } else {
            local_receiver_type_patterns(&site.language)
        };
        if pats.is_empty() {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&site.file_path) else {
            return Ok(None);
        };
        if lines.is_empty() {
            return Ok(None);
        }
        if site.language == "swift" && self.node_by_id(&site.from_node_id)?.is_some_and(|n| n.kind == "route") {
            return self.infer_swift_lexical_receiver_type(&scan_receiver, site, preserve, pats, &lines);
        }
        let call_idx = (site.line - 1).clamp(0, lines.len() as i64 - 1) as usize;
        let start_idx = if component_scoped {
            0usize
        } else {
            let scope = probe!(site, "il:scope", self.enclosing_scope_start_line(
                &site.file_path,
                &site.language,
                site.line,
            )?);
            call_idx.min((scope - 1).max(0) as usize)
        };
        let js = is_js_family(&site.language);
        let shadow_scan = js || site.language == "go";
        // Every pattern needs the receiver literal in the line, so only the
        // lines containing it are scanned (highest first, as before).
        let scanned = probe!(site, "il:scan", {
            let candidates = lines.lines_containing(&scan_receiver);
            let upto = candidates.partition_point(|&l| l as usize <= call_idx);
            let mut hit = None;
            for &i in candidates[..upto].iter().rev() {
                if (i as usize) < start_idx {
                    break;
                }
                let line = &lines[i as usize];
                if let Some(t) = self.infer_match_line(line, &scan_receiver, pats, preserve)? {
                    hit = Some(t);
                    break;
                }
                // `const x = …` (Go: `x := …`, `var x`) shadows every
                // declaration above it while its block is open. In JS it types
                // `x` when its initializer is a call with a known return
                // annotation; otherwise `x` stays untyped here rather than
                // taking an outer same-named declaration's type.
                if shadow_scan && declares(line, &scan_receiver, js) && block_reaches(&lines, i as usize, call_idx) {
                    if js {
                        hit = self.untyped_local_call_type(&lines, i as usize, &scan_receiver, site, preserve)?;
                    }
                    break;
                }
            }
            hit
        });
        if scanned.is_some() {
            return Ok(scanned);
        }
        if component_scoped {
            for line in lines.iter().skip(call_idx + 1) {
                if let Some(t) = self.infer_match_line(line, &scan_receiver, pats, preserve)? {
                    return Ok(Some(t));
                }
            }
        }
        if php_property {
            return self.infer_php_assigned_property_type(&scan_receiver, &lines, call_idx);
        }
        Ok(None)
    }

    /// Swift route-call declarations belong to parsed lexical scopes, not every later
    /// line in the registering function. Reuse the existing type patterns on
    /// the nearest visible declaration; an untyped binding shadows outer ones.
    fn infer_swift_lexical_receiver_type(
        &mut self,
        receiver: &str,
        site: &ResolveRefIn,
        preserve: bool,
        pats: &'static [ReceiverPattern],
        file: &Rc<SourceFile>,
    ) -> Res<Option<String>> {
        use super::iteration::{descendant_for_position, named_children};
        let Some(tree) = self.parsed_tree(file, site) else { return Ok(None) };
        let text = file.text();
        let row = (site.line - 1).max(0) as usize;
        let column = site.column.max(0) as usize;
        let call = descendant_for_position(tree.root_node(), text, (row, column + 1));
        let contains_receiver = |pattern: tree_sitter::Node<'_>| {
            let mut pending = vec![pattern];
            while let Some(name) = pending.pop() {
                if name.kind() == "simple_identifier" && &text[name.start_byte()..name.end_byte()] == receiver { return true; }
                pending.extend(named_children(name));
            }
            false
        };
        let mut scope = call.parent();
        while let Some(node) = scope {
            if matches!(node.kind(), "for_statement" | "catch_block" | "switch_entry") {
                let children = named_children(node);
                let in_body = children.iter().any(|n| n.kind() == "statements"
                    && n.start_byte() <= call.start_byte() && call.end_byte() <= n.end_byte());
                if in_body && node.kind() == "catch_block" && receiver == "error"
                    && !children.iter().any(|n| n.kind() == "pattern") { return Ok(None); }
                if in_body && children.into_iter().filter(|n| matches!(n.kind(), "pattern" | "switch_pattern"))
                    .any(contains_receiver) { return Ok(None); }
            }

            if matches!(node.kind(), "statements" | "source_file" | "class_body") {
                for declaration in named_children(node).into_iter().rev() {
                    if declaration.end_byte() > call.start_byte() { continue; }
                    if declaration.kind() == "guard_statement" {
                        let children = named_children(declaration);
                        if children.windows(2).any(|pair| pair[0].kind() == "value_binding_pattern"
                            && pair[1].kind() == "simple_identifier"
                            && &text[pair[1].start_byte()..pair[1].end_byte()] == receiver) { return Ok(None); }
                    }
                    if declaration.kind() != "property_declaration" { continue; }
                    let patterns: Vec<_> = named_children(declaration).into_iter()
                        .filter(|n| n.kind() == "pattern").collect();
                    for (index, pattern) in patterns.iter().enumerate() {
                        if contains_receiver(*pattern) {
                            let end = patterns.get(index + 1).map_or(declaration.end_byte(), |n| n.start_byte());
                            return self.infer_match_text(&text[pattern.start_byte()..end], receiver, pats, preserve);
                        }
                    }
                }
            }
            if matches!(node.kind(), "if_statement" | "while_statement") {
                let children = named_children(node);
                if let Some(body) = children.iter().find(|n| n.kind() == "statements") {
                    if body.start_byte() <= call.start_byte() && call.end_byte() <= body.end_byte() {
                        let header = &text[node.start_byte()..body.start_byte()];
                        let binds_receiver = children.windows(2).any(|pair|
                            pair[0].kind() == "value_binding_pattern" && pair[1].kind() == "simple_identifier"
                            && &text[pair[1].start_byte()..pair[1].end_byte()] == receiver);
                        if binds_receiver { return self.infer_match_text(header, receiver, pats, preserve); }
                    }
                }
            }
            if matches!(node.kind(), "lambda_literal" | "function_declaration") {
                if node.kind() == "lambda_literal" {
                    let children = named_children(node);
                    let in_body = children.iter().any(|n| n.kind() == "statements"
                        && n.start_byte() <= call.start_byte() && call.end_byte() <= n.end_byte());
                    if in_body && children.into_iter().filter(|n| n.kind() == "capture_list")
                        .flat_map(named_children).filter(|n| n.kind() == "capture_list_item")
                        .any(|item| named_children(item).into_iter().find(|n| n.kind() == "simple_identifier")
                            .is_some_and(|name| &text[name.start_byte()..name.end_byte()] == receiver)) { return Ok(None); }
                }
                let mut parameters = if node.kind() == "lambda_literal" {
                    named_children(node).into_iter().find(|n| n.kind() == "lambda_function_type")
                        .map(named_children).unwrap_or_default()
                } else { named_children(node) };
                while let Some(parameter) = parameters.pop() {
                    if parameter.kind() == "lambda_function_type_parameters" {
                        parameters.extend(named_children(parameter));
                    } else if matches!(parameter.kind(), "parameter" | "lambda_parameter") {
                        let name = named_children(parameter).into_iter().rfind(|n| n.kind() == "simple_identifier");
                        if name.is_some_and(|name| &text[name.start_byte()..name.end_byte()] == receiver) {
                            return self.infer_match_text(&text[parameter.start_byte()..parameter.end_byte()], receiver, pats, preserve);
                        }
                    }
                }
            }
            scope = node.parent();
        }
        Ok(None)
    }

    /// `const|let|var NAME = f(…)` or `A.f(…)` with no annotation, the call
    /// ending the initializer: the return annotation every `f` (same-file ones
    /// first) or `A::f` agrees on, unwrapping `T | undefined` and `Pick<T, …>`
    /// wrappers. The type must be declared, or imported under its own name,
    /// in the factory's module, and not collide with a different same-named
    /// type in the caller. `await` forms belong to infer_esm_awaited_call_type.
    fn untyped_local_call_type(
        &mut self,
        lines: &[String],
        at: usize,
        name: &str,
        site: &ResolveRefIn,
        preserve: bool,
    ) -> Res<Option<String>> {
        let decl = re!(r"(?-u:\b)(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*([A-Za-z_$][\w$]*)(?:\.([A-Za-z_$][\w$]*))?\s*(?:<[^>]*>)?\(");
        let Some(m) = decl.captures_iter(&lines[at]).find(|m| &m[1] == name && &m[2] != "await") else {
            return Ok(None);
        };
        if !call_ends_initializer(&lines[at..lines.len().min(at + 80)], m.get(0).unwrap().end()) {
            return Ok(None);
        }
        // A parameter or local named like the factory hides the declaration.
        let bindings = self.bindings(&site.file_path)?;
        if innermost_binding(&bindings, &m[2], Some((at + 1) as i64))
            .is_some_and(|b| b.kind == "param" || b.kind == "local")
        {
            return Ok(None);
        }
        let candidates: Vec<Arc<KNode>> = match m.get(3) {
            Some(method) => {
                let want = format!("{}::{}", &m[2], method.as_str());
                self.nodes_by_name(method.as_str())?
                    .iter()
                    .filter(|n| {
                        n.kind == "method"
                            && is_esm_family(&n.language)
                            && (n.qualified_name == want || n.qualified_name.ends_with(&format!("::{want}")))
                    })
                    .cloned()
                    .collect()
            }
            None => {
                let fns: Vec<Arc<KNode>> = self
                    .nodes_by_name(&m[2])?
                    .iter()
                    .filter(|n| n.kind == "function" && is_esm_family(&n.language))
                    .cloned()
                    .collect();
                let local: Vec<Arc<KNode>> = fns.iter().filter(|n| n.file_path == site.file_path).cloned().collect();
                if local.is_empty() { fns } else { local }
            }
        };
        // Copies of one declaration (vendored trees) count as one when they
        // agree on the annotation.
        let mut anns = candidates
            .iter()
            .map(|n| n.signature.as_deref().and_then(signature_return_annotation));
        let Some(Some(ret)) = anns.next() else {
            return Ok(None);
        };
        if !anns.all(|a| a == Some(ret)) {
            return Ok(None);
        }
        let mut parts = ret.split('|').map(str::trim);
        let head = parts.next().unwrap_or("");
        if parts.any(|p| p != "undefined" && p != "null") {
            return Ok(None);
        }
        if !re!(r"^[A-Z][\w$]*(?:<.*>)?$").is_match(head) {
            return Ok(None);
        }
        // `Pick<T, …>`-style wrappers keep T's members; pattern captures never
        // include type arguments, so neither does the result.
        let mut bare = head.split('<').next().unwrap_or(head).trim_end();
        if matches!(bare, "Pick" | "Omit" | "Partial" | "Required" | "Readonly" | "NonNullable") {
            let args = &head[bare.len()..];
            bare = args.trim_start_matches('<').split([',', '<', '>']).next().unwrap_or("").trim();
            if !re!(r"^[A-Z][\w$]*$").is_match(bare) {
                return Ok(None);
            }
        }
        if bare == "Promise" {
            return Ok(None);
        }
        let mut factory_files: Vec<&str> = candidates.iter().map(|n| n.file_path.as_str()).collect();
        factory_files.sort_unstable();
        factory_files.dedup();
        for file in &factory_files {
            if !self.names_type_as_itself(file, bare)? {
                return Ok(None);
            }
        }
        if !factory_files.contains(&site.file_path.as_str()) && self.declares_type(&site.file_path, bare)? {
            let own = self.import_mappings(&site.file_path)?.iter().any(|m| m.local_name == bare);
            if !own {
                return Ok(None);
            }
        }
        Ok(self
            .normalize_inferred_type_name(bare)?
            .map(|t| if preserve { bare.to_string() } else { t }))
    }

    /// `file` declares a class/interface/type alias named `name`.
    fn declares_type(&mut self, file: &str, name: &str) -> Res<bool> {
        Ok(self
            .nodes_in_file(file)?
            .iter()
            .any(|n| n.name == name && matches!(n.kind.as_str(), "class" | "interface" | "type_alias")))
    }

    /// `name` in `file` means the type of that name: declared there, or
    /// imported without a rename.
    fn names_type_as_itself(&mut self, file: &str, name: &str) -> Res<bool> {
        if self.declares_type(file, name)? {
            return Ok(true);
        }
        Ok(self
            .import_mappings(file)?
            .iter()
            .any(|m| m.local_name == name && m.exported_name == name && !m.is_namespace))
    }

    /// inferLocalReceiverType for CFML. A `variables.`/`this.` receiver is
    /// component-scoped: the prefix is stripped and the scan widens to the
    /// whole file (backward from the call, then forward); `local.`/`arguments.`
    /// only strip. The declaration patterns — `new X()`, both `createObject`
    /// forms, a typed parameter, and `cfargument`/`property` attributes in
    /// either order — are built per receiver: the attribute forms match the
    /// name case-insensitively, which no Affix split can.
    fn infer_cfml_receiver_type(&mut self, receiver: &str, site: &ResolveRefIn, preserve: bool) -> Res<Option<String>> {
        let mut scan_receiver = receiver.to_string();
        let mut component_scoped = false;
        if let Some(m) = re!(r"(?i)^(variables|this|local|arguments)\.(.+)$").captures(receiver) {
            let scope = m[1].to_lowercase();
            component_scoped = scope == "variables" || scope == "this";
            scan_receiver = m[2].to_string();
        }
        let r = regex::escape(&scan_receiver);
        let b = r"(?-u:\b)";
        let w = "A-Za-z0-9_";
        let patterns = [
            format!(r#"{b}{r}{b}\s*=\s*new\s+([A-Za-z_][{w}.]*)"#),
            format!(r#"{b}{r}{b}\s*=\s*[Cc]reate[Oo]bject\s*\(\s*["']component["']\s*,\s*["']([{w}.]+)["']"#),
            format!(r#"{b}{r}{b}\s*=\s*[Cc]reate[Oo]bject\s*\(\s*["']([{w}.]+)["']\s*\)"#),
            format!(r#"{b}([A-Z][{w}.]*)\s+{r}{b}\s*[=;,)]"#),
            format!(r#"(?i){b}cfargument[^>\n]*{b}name\s*=\s*["']{r}["'][^>\n]*{b}type\s*=\s*["']([{w}.]+)["']"#),
            format!(r#"(?i){b}cfargument[^>\n]*{b}type\s*=\s*["']([{w}.]+)["'][^>\n]*{b}name\s*=\s*["']{r}["']"#),
            format!(r#"(?i){b}(?:cf)?property{b}[^;\n]*{b}name\s*=\s*["']{r}["'][^;\n]*{b}(?:type|inject)\s*=\s*["']([{w}.]+)["']"#),
            format!(r#"(?i){b}(?:cf)?property{b}[^;\n]*{b}(?:type|inject)\s*=\s*["']([{w}.]+)["'][^;\n]*{b}name\s*=\s*["']{r}["']"#),
        ];
        let mut regexes = Vec::with_capacity(patterns.len());
        for p in &patterns {
            regexes.push(Self::cached_regex(p)?);
        }
        let Some(lines) = self.read_file(&site.file_path) else { return Ok(None) };
        if lines.is_empty() {
            return Ok(None);
        }
        let call_idx = (site.line - 1).clamp(0, lines.len() as i64 - 1) as usize;
        let start_idx = if component_scoped {
            0
        } else {
            let scope = self.enclosing_scope_start_line(&site.file_path, &site.language, site.line)?;
            call_idx.min((scope - 1).max(0) as usize)
        };
        let match_line = |this: &mut Self, i: usize| -> Res<Option<String>> {
            let line = &lines[i];
            if line.is_empty() || utf16_len_exceeds(line, 10_000) {
                return Ok(None);
            }
            for (k, re) in regexes.iter().enumerate() {
                let Some(caps) = re.captures(line) else { continue };
                // A component-scoped receiver is never the function-local
                // `var x = …` / `local.x = …` (or an argument) of its name.
                if component_scoped
                    && k < 3
                    && re!(r"(?i)(?:(?-u:\b)var\s+|(?-u:\b)(?:local|arguments)\.)$")
                        .is_match(&line[..caps.get(0).unwrap().start()])
                {
                    continue;
                }
                if let Some(m1) = caps.get(1).map(|g| g.as_str()).filter(|s| !s.is_empty()) {
                    if let Some(t) = this.normalize_inferred_type_name(m1)? {
                        return Ok(Some(if preserve { m1.to_string() } else { t }));
                    }
                }
            }
            Ok(None)
        };
        for i in (start_idx..=call_idx).rev() {
            if let Some(t) = match_line(self, i)? {
                return Ok(Some(t));
            }
        }
        if component_scoped {
            for i in call_idx + 1..lines.len() {
                if let Some(t) = match_line(self, i)? {
                    return Ok(Some(t));
                }
            }
        }
        Ok(None)
    }

    /// inferPhpAssignedPropertyType — `$this->prop = $var` second-chance
    /// typing through the assigned variable's own declaration.
    pub(super) fn infer_php_assigned_property_type(
        &mut self,
        prop: &str,
        lines: &[String],
        call_idx: usize,
    ) -> Res<Option<String>> {
        // `\$this->PROP\b\s*=\s*\$([A-Za-z0-9_]+)\b`
        static ASSIGN: LazyLock<Affix> =
            LazyLock::new(|| Affix::new(r"\$this->", r"\s*=\s*\$([A-Za-z0-9_]+)(?-u:\b)", false, true, false));
        let func_re = re!(r"(?-u:\b)function(?-u:\b)");
        let mut assign_idx: Option<usize> = None;
        let mut var_name: Option<String> = None;
        for i in (0..=call_idx).rev() {
            let line = &lines[i];
            if line.is_empty() || utf16_len_exceeds(line, 10_000) {
                continue;
            }
            if let Some(var) = ASSIGN.capture(line, prop) {
                assign_idx = Some(i);
                var_name = Some(var.to_string());
                break;
            }
        }
        if var_name.is_none() {
            for (i, line) in lines.iter().enumerate().skip(call_idx + 1) {
                if line.is_empty() || utf16_len_exceeds(line, 10_000) {
                    continue;
                }
                if let Some(var) = ASSIGN.capture(line, prop) {
                    assign_idx = Some(i);
                    var_name = Some(var.to_string());
                    break;
                }
            }
        }
        let (Some(ai), Some(vn)) = (assign_idx, var_name) else {
            return Ok(None);
        };
        let pats = local_receiver_type_patterns("php");
        for i in (0..=ai).rev() {
            let line = &lines[i];
            if !line.is_empty() && !utf16_len_exceeds(line, 10_000) {
                if let Some(t) = self.infer_match_line(line, &vn, pats, false)? {
                    return Ok(Some(t));
                }
            }
            if !line.is_empty() && func_re.is_match(line) {
                break;
            }
        }
        Ok(None)
    }

    /// normalizeCppTypeName — strip cv-qualifiers/keywords, refs, generics;
    /// take the last `::` segment (or the qualified name when preserving).
    pub(super) fn normalize_cpp_type_name(&mut self, raw: &str, preserve: bool) -> Res<Option<String>> {
        let kw = re!(r"(?-u:\b)(?:const|volatile|mutable|typename|class|struct)(?-u:\b)")
            .replace_all(raw, " ");
        let no_ref = re!(r"[&*]+").replace_all(&kw, " ");
        let no_gen = re!(r"<[^>]*>").replace_all(&no_ref, " ");
        let normalized = no_gen.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            return Ok(None);
        }
        let parts: Vec<&str> = normalized.split("::").filter(|s| !s.is_empty()).collect();
        let Some(last) = parts.last() else { return Ok(None) };
        if CPP_NON_TYPE_TOKENS.contains(last) {
            return Ok(None);
        }
        Ok(Some(if preserve {
            parts.join("::")
        } else {
            last.to_string()
        }))
    }

    /// buildDeclaratorRegex — `Type receiver` requiring a declarator
    /// terminator. The JS lookahead `(?=[;=,)\[{(]|$)` is post-checked on the
    /// remainder: the greedy `\s*` tail can't shrink into a passing position.
    pub(super) fn cpp_declarator_match(&mut self, line: &str, escaped_receiver: &str) -> Res<Option<String>> {
        let re = shared_regex(&format!(
            r"([A-Za-z_][A-Za-z0-9_:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*(?-u:\b){}(?-u:\b)\s*",
            escaped_receiver
        ))?;
        for caps in re.captures_iter(line) {
            let Some(m0) = caps.get(0) else { continue };
            let rest = &line[m0.end()..];
            let ok = match rest.chars().next() {
                None => true,
                Some(c) => matches!(c, ';' | '=' | ',' | ')' | '[' | '{' | '('),
            };
            if ok {
                if let Some(m1) = caps.get(1) {
                    return Ok(Some(m1.as_str().to_string()));
                }
            }
        }
        Ok(None)
    }

    /// inferCppReceiverType — backward declarator scan, `auto` deduction via
    /// the initializer, then same-named header fallback (.h/.hpp/.hxx).
    pub(super) fn infer_cpp_receiver_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
        depth: u32,
        preserve: bool,
    ) -> Res<Option<String>> {
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        if lines.is_empty() {
            return Ok(None);
        }
        let call_idx = (r.line - 1).clamp(0, lines.len() as i64 - 1) as usize;
        let escaped = regex::escape(receiver);
        for i in (0..=call_idx).rev() {
            let line = &lines[i];
            if line.is_empty() || !has_word(line, receiver) {
                continue;
            }
            if let Some(decl) = self.cpp_declarator_match(line, &escaped)? {
                match self.normalize_cpp_type_name(&decl, preserve)? {
                    Some(t) if t == "auto" || t.ends_with("::auto") => {
                        if let Some(init) =
                            self.infer_cpp_auto_initializer_type(line, receiver, r, depth)?
                        {
                            return Ok(Some(init));
                        }
                        // An undeduced `auto` local shadows earlier decls.
                        return Ok(None);
                    }
                    Some(t) => return Ok(Some(t)),
                    None => {}
                }
            }
        }
        let ext_re = re!(r"(?i)\.(?:c|cc|cpp|cxx)$");
        let mut header_candidates: Vec<String> = Vec::new();
        for ext in [".h", ".hpp", ".hxx"] {
            let candidate = ext_re.replace(&r.file_path, ext).to_string();
            if !header_candidates.contains(&candidate) && candidate != r.file_path {
                header_candidates.push(candidate);
            }
        }
        for header in header_candidates {
            if !self.file_exists(&header) {
                continue;
            }
            let Some(header_lines) = self.read_file(&header) else {
                continue;
            };
            for line in header_lines.iter() {
                if !has_word(line, receiver) {
                    continue;
                }
                let Some(decl) = self.cpp_declarator_match(line, &escaped)? else {
                    continue;
                };
                if let Some(t) = self.normalize_cpp_type_name(&decl, preserve)? {
                    if t != "auto" {
                        return Ok(Some(t));
                    }
                }
            }
        }
        Ok(None)
    }

    /// inferCppAutoInitializerType — `auto x = <init>;` deduction.
    pub(super) fn infer_cpp_auto_initializer_type(
        &mut self,
        line: &str,
        receiver: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Res<Option<String>> {
        // `\bRECV\b\s*=\s*([^;]+)`
        static INIT: LazyLock<Affix> = LazyLock::new(|| Affix::new("", r"\s*=\s*([^;]+)", true, true, false).lead(b"="));
        let Some(init) = INIT.capture(line, receiver).map(|s| s.trim().to_string()) else {
            return Ok(None);
        };
        let neu = re!(r"^new\s+([A-Za-z_][A-Za-z0-9_:]*)");
        if let Some(n) = neu.captures(&init) {
            return Ok(Some(cpp_last_segment(&n[1])));
        }
        let call = re!(r"^([A-Za-z_][A-Za-z0-9_:]*(?:\s*<[^>;]*>)?)\s*\(");
        if let Some(c) = call.captures(&init) {
            let collapsed: String = c[1].split_whitespace().collect();
            return self.resolve_cpp_call_result_type(&collapsed, r, depth + 1);
        }
        Ok(None)
    }

    /// resolveCppCallResultType — make_unique/make_shared, single-level
    /// member call, callee returnType, direct construction.
    pub(super) fn resolve_cpp_call_result_type(
        &mut self,
        inner: &str,
        r: &ResolveRefIn,
        depth: u32,
    ) -> Res<Option<String>> {
        if depth > 3 {
            return Ok(None);
        }
        let expr = inner.trim();
        let make = re!(r"(?:^|::)(?:make_unique|make_shared)\s*<\s*([A-Za-z_][A-Za-z0-9_]*)");
        if let Some(m) = make.captures(expr) {
            return Ok(Some(m[1].to_string()));
        }
        if let Some(dot) = expr.rfind('.') {
            if dot > 0 {
                let recv = &expr[..dot];
                let method = &expr[dot + 1..];
                if recv.contains('.') || recv.contains('(') || recv.contains("::") {
                    return Ok(None);
                }
                let Some(recv_type) =
                    self.infer_cpp_receiver_type(recv, r, depth + 1, false)?
                else {
                    return Ok(None);
                };
                return self.lookup_callee_return_type(&format!("{}::{}", recv_type, method), r);
            }
        }
        if let Some(ret) = self.lookup_callee_return_type(expr, r)? {
            return Ok(Some(ret));
        }
        if self.cpp_class_exists(expr, r)? {
            return Ok(Some(cpp_last_segment(expr)));
        }
        Ok(None)
    }

    /// lookupCalleeReturnType — the indexed `return_type` of `Cls::method` or
    /// a free function, language-filtered.
    pub(super) fn lookup_callee_return_type(
        &mut self,
        callee: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let (method, cls) = if callee.contains("::") {
            let parts: Vec<&str> = callee.split("::").filter(|s| !s.is_empty()).collect();
            let (m, owner) = parts.split_last().map_or((callee, &[][..]), |(m, owner)| (*m, owner));
            let joined = owner.join("::");
            // `if (cls)` — '' is falsy, so `::x` falls to the function path.
            (m.to_string(), if joined.is_empty() { None } else { Some(joined) })
        } else {
            (callee.to_string(), None)
        };
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(&method)?
            .iter()
            .filter(|n| {
                (n.kind == "method" || n.kind == "function")
                    && n.language == r.language
                    && n.return_type.as_deref().is_some_and(|t| !t.is_empty())
            })
            .cloned()
            .collect();
        if let Some(cls) = cls {
            let want = format!("{}::{}", cls, method);
            let hit = candidates.iter().find(|n| {
                n.qualified_name == want
                    || n.qualified_name.ends_with(&format!("::{}", want))
                    || want.ends_with(&format!("::{}", n.qualified_name))
            });
            return Ok(hit.and_then(|n| n.return_type.clone()));
        }
        // A bare Go call names a function of the caller's own package.
        let dir = pos_dirname(&r.file_path);
        Ok(candidates
            .iter()
            .find(|n| n.kind == "function" && (r.language != "go" || pos_dirname(&n.file_path) == dir))
            .and_then(|n| n.return_type.clone()))
    }

    /// A Go package-level function value in the caller's package whose
    /// literal spells its result type, `var f = func(…) *T { … }` (also
    /// wrapped in `sync.OnceValue(…)`): `T` as written, and the variable,
    /// whose file's imports place a `pkg.T`.
    fn go_func_value_result_type(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<(String, Arc<KNode>)>> {
        let dir = pos_dirname(&r.file_path);
        let values: Vec<Arc<KNode>> = self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| n.language == "go" && n.kind == "variable" && pos_dirname(&n.file_path) == dir)
            .cloned()
            .collect();
        let [value] = values.as_slice() else { return Ok(None) };
        let literal = re!(
            r"^=\s*(?:sync\.OnceValue\s*\(\s*)?func\s*\((?:[^()]|\([^()]*\))*\)\s*\*?([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)?)\s*\{"
        );
        Ok(value
            .signature
            .as_deref()
            .and_then(|sig| literal.captures(sig))
            .map(|c| (c[1].to_string(), value.clone())))
    }

    /// cppClassExists — an aggregate type with this last `::` segment exists.
    pub(super) fn cpp_class_exists(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let last = cpp_last_segment(name);
        Ok(self.nodes_by_name(&last)?.iter().any(|n| {
            matches!(n.kind.as_str(), "class" | "struct" | "union") && n.language == r.language
        }))
    }

    /// importedFqnOf — the import mapping whose localName is the type.
    pub(super) fn imported_fqn_of(&mut self, type_name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        Ok(self
            .import_mappings(&r.file_path)?
            .iter()
            .find(|i| i.local_name == type_name)
            .map(|i| i.source.clone()))
    }

    /// matchReference's chain arms in TS dispatch order — at most one runs
    /// per language: cppChain (c/cpp), scopedChain (php/rust), dottedChain
    /// (the dot-notation list). A miss continues to the member tail.
    pub(super) fn match_call_chain(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        match r.language.as_str() {
            "c" | "cpp" => self.match_cpp_call_chain(r),
            "rust" => {
                let Some(m)=call_chain_re().captures(&r.reference_name) else {return Ok(None)};
                if let Some(hit)=self.rust_chain_member(&super::call_shape::receiver_name(&m[1]),&m[2],r)? {return Ok(Some(hit));}
                // Unresolved imports can still name a constructor's unique matching member.
                let Some((path,_))=m[1].split_once("::") else {return Ok(None)};
                let factory_owner=m[1].split('(').next().unwrap_or("").rsplit_once("::").map(|(owner,_)|owner.rsplit("::").next().unwrap_or(path)).unwrap_or(path);
                let owners:Vec<_>=self.nodes_by_name(factory_owner)?.iter().filter(|n|n.language=="rust" && matches!(n.kind.as_str(),"struct"|"enum"|"class")).cloned().collect();
                if owners.is_empty() {return Ok(None);}
                if owners.len()>1 {
                    let members=self.nodes_by_name(&m[2])?;
                    if members.iter().filter(|n|n.language=="rust" && n.kind=="method" && super::call_shape::owner(n)==Some(factory_owner)).count()!=1 {return Ok(None);}
                }
                Ok(self.match_scoped_call_chain(r)?.filter(|hit|super::call_shape::owner(&hit.node)==Some(factory_owner)).map(|mut hit|{hit.confidence=hit.confidence.min(0.7);hit}))
            },
            "php" => self.match_scoped_call_chain(r),
            "java" | "kotlin" | "csharp" | "swift" | "go" | "scala" | "dart" | "objc"
            | "pascal" => self.match_dotted_call_chain(r),
            _ => Ok(None),
        }
    }

    /// matchCppCallChain — `<inner>().<method>` where the inner call's
    /// return type is the receiver's type (#645); resolveMethodOnType
    /// validates, so a wrong inference yields no edge.
    pub(super) fn match_cpp_call_chain(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(m) = call_chain_re().captures(&r.reference_name) else {
            return Ok(None);
        };
        let inner = m.get(1).unwrap().as_str();
        let method = m.get(2).unwrap().as_str();
        let Some(cls) = self.resolve_cpp_call_result_type(inner, r, 0)? else {
            return Ok(None);
        };
        self.resolve_method_on_type(&cls, method, r, 0.85, "instance-method", None)
    }

    /// matchScopedCallChain — `Cls::factory().method` static-factory chains
    /// (PHP `Cls::for($x)->m()`, Rust `Foo::new().bar()`); a `self` return
    /// marker resolves to the factory's own class (#608).
    pub(super) fn match_scoped_call_chain(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(m) = call_chain_re().captures(&r.reference_name) else {
            return Ok(None);
        };
        let inner = m.get(1).unwrap().as_str();
        let method = m.get(2).unwrap().as_str();
        if !inner.contains("::") {
            return Ok(None);
        }
        let factory_class = &inner[..inner.rfind("::").unwrap()];
        let Some(ret) = self.lookup_callee_return_type(inner, r)? else {
            return Ok(None);
        };
        let resolved = if ret == "self" { factory_class } else { ret.as_str() };
        self.resolve_method_on_type(resolved, method, r, 0.85, "instance-method", None)
    }

    /// matchDottedCallChain — `Foo.getInstance().bar` factory/fluent chains,
    /// Go's bare `New().Method` (or a same-package `var f = func() *T {…}`),
    /// Go's imported `pkg.New().Method`, and the objc/pascal convention arms
    /// (#645/#608). Go's `f().m` with an
    /// untyped `f` is a miss, never `m` by its bare name.
    pub(super) fn match_dotted_call_chain(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(m) = call_chain_re().captures(&r.reference_name) else {
            return Ok(None);
        };
        let inner = m.get(1).unwrap().as_str();
        let method = m.get(2).unwrap().as_str();
        // TS `lastIndexOf('.') <= 0` — no dot, or a leading one.
        let last_dot = inner.rfind('.');
        if last_dot.is_none() || last_dot == Some(0) {
            if r.language == "go" {
                let Some(ret) = self.lookup_callee_return_type(inner, r)? else {
                    // A function value whose literal gives no result type
                    // says nothing about what `m` is called on.
                    return match self.go_func_value_result_type(inner, r)? {
                        Some((ty, value)) => self.match_bound_type_member(&ty, method, &r.clone().at(&value)),
                        None => Ok(None),
                    };
                };
                let fqn = self.imported_fqn_of(&ret, r)?;
                return self.resolve_method_on_type(
                    &ret,
                    method,
                    r,
                    0.85,
                    "instance-method",
                    fqn.as_deref(),
                );
            }
            if !CONSTRUCTS_VIA_BARE_CALL.contains(r.language.as_str())
                || !inner.as_bytes()[0].is_ascii_uppercase()
            {
                return Ok(None);
            }
            let fqn = self.imported_fqn_of(inner, r)?;
            return self.resolve_method_on_type(
                inner,
                method,
                r,
                0.85,
                "instance-method",
                fqn.as_deref(),
            );
        }
        let last_dot = last_dot.unwrap();
        if r.language == "go" {
            return self.match_go_package_call_chain(&inner[..last_dot], &inner[last_dot + 1..], method, r);
        }
        let factory_class = inner[..last_dot].split('.').next_back().unwrap();
        let factory_method = &inner[last_dot + 1..];
        if factory_class.is_empty() || factory_method.is_empty() {
            return Ok(None);
        }
        let want = format!("{}::{}", factory_class, factory_method);
        let Some(ret) = self.lookup_callee_return_type(&want, r)? else {
            // objc `[X alloc]` / pascal `TFoo.Create` conventions — the
            // receiver's type is the class itself. Both unmigrated today
            // (unreachable); ported verbatim for fidelity.
            let first = factory_class.as_bytes()[0];
            if (r.language == "objc" && first.is_ascii_uppercase())
                || (r.language == "pascal" && matches!(first, b'T' | b'I'))
            {
                let fqn = self.imported_fqn_of(factory_class, r)?;
                return self.resolve_method_on_type(
                    factory_class,
                    method,
                    r,
                    0.8,
                    "instance-method",
                    fqn.as_deref(),
                );
            }
            return Ok(None);
        };
        let fqn = self.imported_fqn_of(&ret, r)?;
        self.resolve_method_on_type(&ret, method, r, 0.85, "instance-method", fqn.as_deref())
    }

    /// Go `pkg.New().Method`: `New`'s result type among the functions of
    /// the package the file imports as `pkg`, and `Method` on that type in
    /// the same package, never on a same-named type in another package. An
    /// import outside the module gives no edge.
    fn match_go_package_call_chain(
        &mut self,
        pkg: &str,
        func: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if pkg.contains('.') {
            return Ok(None);
        }
        let Some(pkg_dir) = self.go_imported_package_dir(&r.file_path, pkg)? else {
            return Ok(None);
        };
        let ret = self.nodes_by_name(func)?.iter().find_map(|n| {
            (n.kind == "function" && n.language == "go" && pos_dirname(&n.file_path) == pkg_dir)
                .then(|| n.return_type.clone())
                .flatten()
                .filter(|t| !t.is_empty())
        });
        let Some(ret) = ret else { return Ok(None) };
        self.go_method_in_package(&ret, method, &pkg_dir, r)
    }

    /// resolveJvmImport (import-resolver.ts) — `imports`-kind java/kotlin FQN
    /// to a qualified-name node, KMP `expect` preferred on ties.
    pub(super) fn resolve_jvm_import(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if r.reference_kind != "imports" {
            return Ok(None);
        }
        if r.language != "java" && r.language != "kotlin" {
            return Ok(None);
        }
        let fqn = &r.reference_name;
        let Some(dot) = fqn.rfind('.') else { return Ok(None) };
        if dot == 0 {
            return Ok(None);
        }
        let (pkg, sym) = (&fqn[..dot], &fqn[dot + 1..]);
        if sym == "*" {
            return Ok(None);
        }
        let candidates = self.nodes_by_qualified_name(&format!("{}::{}", pkg, sym))?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let best = if candidates.len() == 1 {
            candidates[0].clone()
        } else {
            pick_closest_jvm_candidate(&candidates, &r.file_path)
        };
        Ok(Some(KCand {
            node: best,
            confidence: 0.95,
            resolved_by: "import",
        }))
    }

}

/// No non-blank line between `decl` and `call` is indented less than `decl`,
/// so the block holding the declaration is still open at the call.
/// `line` declares `name`: JS `const|let|var name =`, Go `name := …` (among
/// other names) or `var name`.
fn declares(line: &str, name: &str, js: bool) -> bool {
    if js {
        return re!(r"(?-u:\b)(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=")
            .captures_iter(line)
            .any(|m| &m[1] == name);
    }
    if let Some(m) = re!(r"^\s*(?:if\s+|for\s+|switch\s+)?([\w\s,]+?)\s*:=").captures(line) {
        if m[1].split(',').any(|n| n.trim() == name) {
            return true;
        }
    }
    re!(r"^\s*var\s+([\w\s,]+?)(?:\s+[\[\]*\w.]+)?\s*(?:=|$)")
        .captures(line)
        .is_some_and(|m| m[1].split(',').any(|n| n.trim() == name))
}

fn block_reaches(lines: &[String], decl: usize, call: usize) -> bool {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let base = indent(&lines[decl]);
    lines[decl + 1..=call].iter().all(|l| l.trim().is_empty() || indent(l) >= base)
}

/// The call whose `(` ends at byte `open_end` of `lines[0]` closes and is
/// followed by `;`, the end of the text, or a line break that does not
/// continue the expression (`.x`, `(…)`, `[…]`, `?.`).
fn call_ends_initializer(lines: &[String], open_end: usize) -> bool {
    let text = lines.join("\n");
    let bytes = text.as_bytes();
    let mut depth = 1;
    let mut i = open_end;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    if depth > 0 {
        return false;
    }
    let after = text[i..].trim_start_matches([' ', '\t']);
    if after.is_empty() || after.starts_with(';') {
        return true;
    }
    after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .is_some_and(|next| !next.trim_start().starts_with(['.', '(', '[', '?']))
}

/// The `: T` after a signature's parameter list, trimmed.
fn signature_return_annotation(signature: &str) -> Option<&str> {
    let after = &signature[signature.rfind(')')? + 1..];
    let ret = after.trim_start().strip_prefix(':')?.trim();
    (!ret.is_empty()).then_some(ret)
}

pub(super) fn cpp_last_segment(name: &str) -> String {
    let parts: Vec<&str> = name.split("::").filter(|s| !s.is_empty()).collect();
    parts.last().map(|s| s.to_string()).unwrap_or_else(|| name.to_string())
}

/// pickClosestJvmCandidate — shared-directory-prefix proximity, Kotlin
/// Multiplatform `expect` preferred on a tie.
pub(super) fn pick_closest_jvm_candidate(candidates: &[Arc<KNode>], from_path: &str) -> Arc<KNode> {
    let from_dirs: Vec<&str> = from_path.split('/').collect();
    let from_dirs = &from_dirs[..from_dirs.len().saturating_sub(1)];
    let shared = |p: &str| shared_dir_prefix(from_dirs, p);
    let is_expect = |n: &KNode| {
        n.decorators
            .as_ref()
            .is_some_and(|ds| ds.iter().any(|d| d == "expect"))
    };
    let mut best = &candidates[0];
    let mut best_prox = shared(&best.file_path);
    for c in &candidates[1..] {
        let prox = shared(&c.file_path);
        if prox > best_prox || (prox == best_prox && is_expect(c) && !is_expect(best)) {
            best = c;
            best_prox = prox;
        }
    }
    best.clone()
}
