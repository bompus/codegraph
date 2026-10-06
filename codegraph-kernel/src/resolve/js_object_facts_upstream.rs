//! File facts for f40db4b9 object-path resolution.
use super::awaited::{blank_string_contents, strip_ts_comments};
use super::*;
use tree_sitter::{Point, Tree};

type DestructureScopes = HashMap<String, HashMap<String, Vec<Option<(Point, Point)>>>>;

type BindingRanges = RefCell<HashMap<String, Rc<Vec<(i64, i64)>>>>;

pub(super) struct JsObjectFacts {
    pub source: Rc<SourceFile>,
    code: String,
    byte_units: Vec<usize>,
    line_starts: Vec<usize>,
    byte_line_starts: Vec<usize>,
    events: Vec<usize>,
    open: Vec<Option<usize>>,
    close: HashMap<usize, usize>,
    length: usize,
    bindings: BindingRanges,
    function_bindings: RefCell<HashMap<(i64,i64,String),bool>>,
    destructured: OnceCell<DestructureScopes>,
    classic: OnceCell<bool>,
}

impl JsObjectFacts {
    pub fn new(source: Rc<SourceFile>) -> Self {
        let code = blank_string_contents(&strip_ts_comments(source.text()));
        let mut byte_units = vec![0; code.len() + 1];
        let mut units = 0;
        for (at, ch) in code.char_indices() {
            for slot in &mut byte_units[at..at + ch.len_utf8()] {
                *slot = units;
            }
            units += ch.len_utf16();
            byte_units[at + ch.len_utf8()] = units;
        }
        let mut line_starts = vec![0];
        let mut byte_line_starts = vec![0];
        byte_line_starts.extend(code.match_indices('\n').map(|(at,_)| at + 1));
        let mut events = Vec::new();
        let mut open = Vec::new();
        let mut close = HashMap::new();
        let mut stack = Vec::new();
        for (at, ch) in code.encode_utf16().enumerate() {
            match ch {
                10 => line_starts.push(at + 1),
                123 => {
                    stack.push(at);
                    events.push(at);
                    open.push(stack.last().copied());
                }
                125 => {
                    if let Some(start) = stack.pop() { close.insert(start, at); }
                    events.push(at);
                    open.push(stack.last().copied());
                }
                _ => {}
            }
        }
        Self {
            source, code, byte_units, line_starts, byte_line_starts, events, open, close,
            length: units, bindings: RefCell::new(HashMap::new()),
            function_bindings: RefCell::new(HashMap::new()),
            destructured: OnceCell::new(), classic: OnceCell::new(),
        }
    }

    pub fn offset(&self, line: i64, column: i64) -> usize {
        self.line_starts.get((line - 1).max(0) as usize).copied()
            .unwrap_or(self.length).saturating_add(column.max(0) as usize)
    }

    pub fn block_at(&self, offset: usize) -> Option<(usize, usize)> {
        let count = self.events.partition_point(|at| *at < offset);
        let start = count.checked_sub(1).and_then(|i| self.open[i])?;
        Some((start, self.close.get(&start).copied().unwrap_or(self.length)))
    }

    fn block_of_byte(&self, at: usize) -> (i64, i64) {
        self.block_at(self.byte_units[at]).map(|(a,b)| (a as i64,b as i64))
            .unwrap_or((-1,self.length as i64))
    }

    fn body_after(&self, start: usize, end: usize) -> (i64, i64) {
        let opener = if self.code.as_bytes().get(end.saturating_sub(1)) == Some(&b'{') {
            Some(end - 1)
        } else {
            let tail = &self.code[end..];
            let skipped = tail.len() - tail.trim_start().len();
            (self.code.as_bytes().get(end + skipped) == Some(&b'{')).then_some(end + skipped)
        };
        if let Some(at) = opener {
            let unit = self.byte_units[at];
            return (unit as i64,self.close.get(&unit).copied().unwrap_or(self.length) as i64);
        }
        (self.byte_units[start] as i64,self.block_of_byte(start).1)
    }

    pub fn binding_scopes(&self, name: &str) -> Rc<Vec<(i64,i64)>> {
        if let Some(hit) = self.bindings.borrow().get(name) { return hit.clone(); }
        let mut scopes = Vec::new();
        if self.code.contains(name) {
            let n = regex::escape(name);
            // Rust regex has no lookaround. These patterns consume the left
            // boundary; the right boundary is captured, not a name suffix.
            let decl = format!(r"(?:^|[^\w$])(?P<binding>(?:const|let|var)\s+(?:{n}(?:[^\w$]|$)|[{{\[][^;=]*?[}}\]]))");
            let function = format!(r"(?:^|[^\w$])(?P<binding>(?:function|class)\s+{n}(?:[^\w$]|$))");
            for pattern in [decl,function] {
                if let Ok(re) = KernelResolver::cached_regex(&pattern) {
                    for caps in re.captures_iter(&self.code) {
                        let binding = caps.name("binding").unwrap();
                        if KernelResolver::cached_regex(&format!(r"(?:^|[^\w$]){n}(?:[^\w$]|$)"))
                            .is_ok_and(|re| re.is_match(binding.as_str())) {
                            scopes.push(self.block_of_byte(binding.start()));
                        }
                    }
                }
            }
            let parameter = parameter_pattern(&n);
            if let Ok(re) = KernelResolver::cached_regex(&parameter) {
                for m in re.find_iter(&self.code) { scopes.push(self.body_after(m.start(),m.end())); }
            }
            let arrow = format!(r"(?:^|[^\w$.]){n}\s*=>");
            if let Ok(re) = KernelResolver::cached_regex(&arrow) {
                for m in re.find_iter(&self.code) { scopes.push(self.body_after(m.start(),m.end())); }
            }
        }
        let scopes = Rc::new(scopes);
        self.bindings.borrow_mut().insert(name.to_string(),scopes.clone());
        scopes
    }

    pub fn binds_at(&self, name: &str, r: &ResolveRefIn) -> bool {
        let at = self.offset(r.line,r.column) as i64;
        self.binding_scopes(name).iter().any(|(a,b)| at > *a && at < *b)
    }

    // Upstream's holderScopeAt checks the written caller, not every visible
    // binding. Keep its function-local test separate from binds_at.
    pub fn function_binds(&self, name: &str, caller: &KNode, r: &ResolveRefIn) -> bool {
        let key = (caller.start_line,r.line,name.to_string());
        if let Some(hit) = self.function_bindings.borrow().get(&key) { return *hit; }
        let lo = self.byte_line_starts.get((caller.start_line - 1).max(0) as usize)
            .copied().unwrap_or(self.code.len());
        let hi = self.byte_line_starts.get(r.line.max(0) as usize)
            .copied().unwrap_or(self.code.len());
        let text = self.code.get(lo..hi).unwrap_or("");
        let n = regex::escape(name);
        let decl = format!(r"(?:^|[^\w$])(?:const|let|var)\s+{n}(?:[^\w$]|$)");
        let declared = KernelResolver::cached_regex(&decl).is_ok_and(|re| {
            re.find_iter(text).any(|m| {
                // The consumed right boundary may be whitespace. Inspect
                // from the end of the name, then reject destructuring tails.
                let raw = m.as_str();
                let Some(at) = raw.rfind(name) else { return false };
                !raw[at + name.len()..].trim_start().starts_with([',',']','}'])
                    && !text[m.end()..].trim_start().starts_with([',',']','}'])
            })
        });
        let parameter = parameter_pattern(&n).replace("(?::[^=;{]*)?","(?::[^=;{}()\n]*)?");
        let bound = declared || KernelResolver::cached_regex(&parameter).is_ok_and(|re| {
            re.find_iter(text).any(|m| {
                !re!(r"(?:^|[^\w$])(?:if|while|for|switch|with)\s*$")
                    .is_match(&text[..m.start()])
            })
        });
        self.function_bindings.borrow_mut().insert(key,bound);
        bound
    }

    pub fn destructures(&self, path: &str, member: &str, r: &ResolveRefIn, tree: &Tree) -> bool {
        self.destructured.get_or_init(|| {
            let mut paths = DestructureScopes::new();
            let re = re!(r"\{([^{}]*)\}\s*=\s*((?:(?:window|globalThis)\s*\.\s*)?[A-Za-z_$][\w$]*(?:\s*\.\s*[A-Za-z_$][\w$]*)*)");
            for caps in re.captures_iter(&self.code) {
                let at = caps.get(0).unwrap().start();
                let row = self.byte_line_starts.partition_point(|start| *start <= at).saturating_sub(1);
                let units = self.byte_units[at].saturating_sub(self.line_starts[row]);
                let Some(line) = self.source.get(row) else { continue };
                let point = Point::new(row, super::names::js_unit_to_byte(line, units));
                let mut ancestor = tree.root_node().descendant_for_point_range(point, point);
                let mut declaration = None;
                while let Some(node) = ancestor {
                    if node.kind() == "variable_declarator" {
                        if node.child_by_field_name("name").is_some_and(|pattern|
                            pattern.kind() == "object_pattern" && pattern.start_position() <= point && point < pattern.end_position()) {
                            declaration = Some(node);
                        }
                        break;
                    }
                    ancestor = node.parent();
                }
                let Some(declaration) = declaration else { continue };
                let scope = super::js_scope_upstream::binding_scope(declaration);
                let rhs = caps.get(2).unwrap();
                // Equivalent to upstream's two negative lookaheads, without
                // permitting regex backtracking to shorten the path.
                let tail = &self.code[rhs.end()..];
                if tail.chars().next().is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
                    || tail.trim_start().starts_with(['.','(','[']) { continue; }
                let mut path: String = rhs.as_str().chars().filter(|ch| !ch.is_whitespace()).collect();
                for prefix in ["window.","globalThis."] {
                    if let Some(rest) = path.strip_prefix(prefix) { path = rest.to_string(); break; }
                }
                for item in caps[1].split(',') {
                    let item = item.trim();
                    let mut pair = item.split(':');
                    let key = pair.next().unwrap_or("").split('=').next().unwrap_or("").trim();
                    let local = pair.next().map(|s| s.split('=').next().unwrap_or("").trim()).unwrap_or(key);
                    // An explicit alias is permitted only when key == local.
                    if key == local && re!(r"^[A-Za-z_$][\w$]*$").is_match(key) {
                        paths.entry(path.clone()).or_default().entry(key.to_string()).or_default().push(scope);
                    }
                }
            }
            paths
        }).get(path).and_then(|keys| keys.get(member)).is_some_and(|scopes| {
            let Some(line) = self.source.get((r.line - 1).max(0) as usize) else { return false };
            let point = Point::new((r.line - 1).max(0) as usize,
                super::names::js_unit_to_byte(line, r.column.max(0) as usize));
            scopes.iter().any(|scope| scope.is_none_or(|(start, end)| point >= start && point < end))
        })
    }

    pub fn classic(&self, path: &str) -> bool {
        *self.classic.get_or_init(|| {
            !re!(r"(?i)\.[mc]js$").is_match(path)
                && !re!(r"\bmodule\.exports\b|\bexports\s*[.\[]").is_match(self.source.text())
                && !re!(r#"(?m)^[ \t]*import[\s{*'"]"#).is_match(&self.code)
                && !re!(r"(?m)^[ \t]*export[\s{*]|^[ \t]*declare\s+global\b").is_match(&self.code)
                && !re!(r"(?:^|[^\w$.])require\s*\(").is_match(&self.code)
        })
    }
}

fn parameter_pattern(n: &str) -> String {
    // Port of localBindingPatterns.param. Explicit non-name boundary after
    // the selected parameter replaces JS's \b without lookaround.
    format!(r"\(\s*(?:(?:\.\.\.)?[\w$]+(?:\s*(?:\?\s*)?:[^,()]+|\s*=[^,()]+|\s*),\s*)*{n}(?:\s*\??\s*:[^,()]*)?(?:\s*=[^,()]*)?(?:\s*,\s*[^()]*)?\)\s*(?::[^=;{{]*)?(?:=>|\{{)")
}

impl KernelResolver {
    pub(super) fn js_object_facts(&mut self, path: &str) -> Option<Rc<JsObjectFacts>> {
        let source = self.read_file(path)?;
        let cached = self.js_objects.files.get(path)
            .filter(|facts| Rc::ptr_eq(&facts.source,&source)).cloned();
        let facts = cached.unwrap_or_else(|| Rc::new(JsObjectFacts::new(source)));
        // put refreshes LRU insertion order and retains only 32 files.
        self.js_objects.files.put(path.to_string(),facts.clone());
        Some(facts)
    }
}
