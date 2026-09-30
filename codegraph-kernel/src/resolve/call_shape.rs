//! Call-site forms and language scope for names whose extractor drops the receiver.

use super::*;
use super::method_call::shares_receiver_word;

const SWIFT_STD_LABELS: &[&str] = &[
    "contentsOf", "where", "by", "at", "keepingCapacity", "separator",
    "forKey", "of", "into", "in", "options", "maxSplits",
    "omittingEmptySubsequences", "with", "after", "before", "upTo", "through",
    "offsetBy", "default",
];

const SWIFT_STD_METHODS: &[&str] = &[
    "append", "insert", "remove", "removeAll", "removeFirst", "removeLast",
    "removeValue", "contains", "map", "compactMap", "flatMap", "filter",
    "reduce", "forEach", "sorted", "sort", "first", "last",
    "min", "max", "firstIndex", "lastIndex", "index", "enumerated",
    "reversed", "joined", "split", "prefix", "suffix", "dropFirst",
    "dropLast", "allSatisfy", "randomElement", "shuffled", "popLast", "replaceSubrange",
    "replacingOccurrences", "components", "trimmingCharacters", "hasPrefix", "hasSuffix", "lowercased",
    "uppercased", "appending", "updateValue", "merge", "merging", "union",
    "intersection", "subtracting", "formUnion", "isEqual", "addSubview", "removeFromSuperview",
    "setNeedsDisplay", "setNeedsLayout", "layoutIfNeeded", "addGestureRecognizer", "addTarget", "addObserver",
    "removeObserver", "eraseToAnyPublisher",
];

const RUST_STD_METHODS: &[&str] = &[
    "unwrap", "unwrap_or", "unwrap_or_else", "unwrap_or_default", "unwrap_err", "unwrap_unchecked",
    "expect", "expect_err", "ok", "err", "map", "map_err",
    "map_or", "map_or_else", "and_then", "or_else", "ok_or", "ok_or_else",
    "is_some", "is_none", "is_ok", "is_err", "is_some_and", "as_ref",
    "as_mut", "as_deref", "clone", "cloned", "copied", "iter",
    "iter_mut", "into_iter", "collect", "enumerate", "zip", "rev",
    "chain", "skip", "step_by", "peekable", "flat_map", "filter_map",
    "flatten", "any", "all", "len", "is_empty", "push",
    "push_str", "pop", "extend", "drain", "clear", "retain",
    "truncate", "reserve", "with_capacity", "capacity", "sort", "sort_by",
    "sort_by_key", "dedup", "split_off", "contains_key", "to_string", "to_owned",
    "to_vec", "as_str", "as_bytes", "as_slice", "as_ptr", "into",
    "try_into", "borrow", "borrow_mut", "deref", "deref_mut", "chars",
    "bytes", "lines", "starts_with", "ends_with", "trim", "to_lowercase",
    "to_uppercase", "windows", "chunks", "then", "then_some", "eq",
    "cmp", "partial_cmp", "read_to_end", "read_to_string", "fetch_add", "fetch_sub",
];

const GO_STD_METHODS: &[&str] = &[
    "String", "Error", "Unwrap", "Is", "As", "Header",
    "WriteHeader", "WriteString", "Lock", "Unlock", "RLock", "RUnlock",
    "Err", "Deadline", "Int", "Bool", "Float64", "Int64",
    "Uint64", "Bytes", "Len", "Cap", "Seconds", "Unix",
    "Before", "After", "Equal", "IsNil", "IsValid", "Elem",
    "NumField", "Interface", "Kind",
];

const KOTLIN_STD_METHODS: &[&str] = &[
    "apply", "also", "let", "run", "takeIf", "takeUnless",
    "toString", "equals", "hashCode", "map", "mapNotNull", "mapIndexed",
    "filter", "filterNot", "filterIsInstance", "forEach", "forEachIndexed", "first",
    "firstOrNull", "last", "lastOrNull", "single", "singleOrNull", "isEmpty",
    "isNotEmpty", "isNullOrEmpty", "isNullOrBlank", "isBlank", "isNotBlank", "orEmpty",
    "joinToString", "toList", "toMutableList", "toSet", "toMutableSet", "toMap",
    "toTypedArray", "any", "all", "none", "count", "sumOf",
    "maxOf", "minOf", "maxOrNull", "minOrNull", "sortedBy", "sortedByDescending",
    "sorted", "sortedWith", "reversed", "drop", "dropLast", "take",
    "takeLast", "zip", "flatMap", "flatten", "distinct", "groupBy",
    "associate", "associateBy", "associateWith", "partition", "contains", "containsKey",
    "getOrElse", "getOrNull", "getOrPut", "getOrDefault", "trim", "trimEnd",
    "trimStart", "split", "substring", "startsWith", "endsWith", "replace",
    "lowercase", "uppercase", "toInt", "toLong", "toDouble", "toFloat",
    "toIntOrNull", "toLongOrNull", "encodeToByteArray", "decodeToString", "copyOf", "copyOfRange",
    "indexOf", "lastIndexOf", "withIndex", "asSequence", "asList", "ifEmpty",
    "ifBlank", "padStart", "padEnd", "repeat", "lines", "toCharArray",
    "coerceAtLeast", "coerceAtMost", "coerceIn",
];

const CSHARP_STD_METHODS: &[&str] = &[
    "ToString", "Equals", "GetHashCode", "GetType", "CompareTo", "Add",
    "AddRange", "Remove", "RemoveAt", "RemoveAll", "Contains", "ContainsKey",
    "ContainsValue", "Clear", "Insert", "IndexOf", "CopyTo", "ToArray",
    "ToList", "ToDictionary", "GetEnumerator", "MoveNext", "Reset", "TryGetValue",
    "GetValueOrDefault", "TryAdd", "Write", "WriteLine", "WriteAsync", "WriteLineAsync",
    "Read", "ReadAsync", "ReadLine", "ReadToEnd", "Flush", "FlushAsync",
    "Close", "Dispose", "DisposeAsync", "Parse", "TryParse", "Format",
    "Join", "Split", "Replace", "Substring", "Trim", "TrimStart",
    "TrimEnd", "StartsWith", "EndsWith", "ToUpper", "ToLower", "ToUpperInvariant",
    "ToLowerInvariant", "Select", "Where", "First", "FirstOrDefault", "Single",
    "SingleOrDefault", "Last", "LastOrDefault", "Any", "All", "Count",
    "Sum", "Max", "Min", "OrderBy", "OrderByDescending", "GroupBy",
    "Skip", "Take", "Distinct", "Concat", "Cast", "OfType",
    "Aggregate", "Invoke", "DynamicInvoke", "GetMethod", "GetProperty", "GetField",
    "GetConstructor", "GetCustomAttributes", "GetGenericArguments", "MakeGenericType", "IsAssignableFrom", "ConfigureAwait",
    "Wait", "ContinueWith", "Append", "AppendLine", "Peek", "Push",
    "Pop", "Enqueue", "Dequeue", "HasFlag", "Find", "FindAll",
    "ForEach", "Sort", "Reverse", "Clone", "Seek", "SetLength",
];

#[derive(Clone, PartialEq)]
enum Shape { Bare, Path, SelfCall, SuperCall, Chain }
#[derive(PartialEq)]
pub(super) enum KotlinChainEvidence { Bound, Heuristic }
struct CallSite { shape: Shape, receiver: String, label: String, subscript: bool }

/// Drop balanced call, subscript and literal arguments while walking back to the receiver.
pub(super) fn receiver_name(text: &str) -> String {
    let text = text.trim_end().trim_end_matches('.').trim_end();
    let chars: Vec<char> = text.chars().collect();
    let mut i = chars.len();
    let mut out = String::new();
    while i > 0 {
        let ch = chars[i - 1];
        if matches!(ch, ')' | ']' | '}') {
            let open = match ch { ')' => '(', ']' => '[', _ => '{' };
            let end = i;
            let mut depth = 1;
            i -= 1;
            while i > 0 && depth > 0 {
                i -= 1;
                if chars[i] == ch { depth += 1; }
                else if chars[i] == open { depth -= 1; }
            }
            if depth != 0 { return String::new(); }
            if ch == ')' && out.is_empty() {
                let literal: String = chars[i..end].iter().collect();
                if let Some(m) = re!(r"^\(\s*&?\s*([A-Za-z_][\w.]*)\s*\{").captures(&literal) {
                    return m[1].to_string();
                }
            }
        } else if ch.is_ascii_alphanumeric() || "_$.:!".contains(ch) {
            if ch != '!' { out.insert(0, ch); }
            i -= 1;
        } else { break; }
    }
    out.trim_matches(['.', ':']).to_string()
}

pub(super) fn receiver_link(receiver: &str) -> &str {
    let mut links = receiver.rsplit('.');
    let last = links.next().unwrap_or("");
    if re!(r"^[A-Z][A-Z0-9_]+$").is_match(last) { links.next().unwrap_or(last) } else { last }
}

pub(super) fn is_std_method(language: &str, name: &str) -> bool {
    match language {
        "rust" => RUST_STD_METHODS, "go" => GO_STD_METHODS,
        "kotlin" => KOTLIN_STD_METHODS, "csharp" => CSHARP_STD_METHODS,
        _ => &[],
    }.contains(&name)
}

fn owner(n: &KNode) -> Option<&str> {
    n.qualified_name.rsplit_once("::").map(|(path, _)| path.rsplit([':', '.']).next().unwrap_or(""))
}
fn type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "enum" | "interface" | "trait" | "protocol" | "module" | "namespace")
}
fn swift_member(n: &KNode) -> bool {
    matches!(n.kind.as_str(), "method" | "property" | "field" | "enum_member")
        || (owner(n).is_some() && matches!(n.kind.as_str(), "constant" | "variable"))
}

/// Remove nested generic and constructor parameters from a declaration head.
pub(super) fn flat_head(text: &str, scala: bool) -> String {
    let mut depth = 0usize;
    let mut flat = String::new();
    for ch in text.chars() {
        if ch == '(' || ch == '<' || (scala && ch == '[') { depth += 1; }
        else if ch == ')' || ch == '>' || (scala && ch == ']') { depth = depth.saturating_sub(1); }
        else if depth == 0 {
            if ch == '{' || (scala && ch == '=') { break; }
            flat.push(ch);
        }
    }
    flat
}

impl KernelResolver {
    fn call_site(&mut self, r: &ResolveRefIn) -> Option<CallSite> {
        let lines = self.read_file(&r.file_path)?;
        let line = lines.get((r.line - 1).max(0) as usize)?;
        let name = &r.reference_name;
        let at = super::lang_scope::name_start_at_column(line, name, r.column.max(0) as usize)
            .filter(|&at| at == 0 || !line[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '$' || c == '_'))
            .or_else(|| {
                Self::cached_regex(&format!(r"(?:^|[^\w$])({})\s*(?:[(<{{\[]|!|::<)", regex::escape(name)))
                    .ok()?.captures_iter(line).filter_map(|m| m.get(1))
                    .find(|m| r.language != "kotlin" || m.start() >= js_unit_to_byte(line, r.column.max(0) as usize)).map(|m| m.start())
            })?;
        let before = &line[..at];
        let after = &line[at + name.len()..];
        let shape = if before.trim_end().ends_with("::") { Shape::Path }
        else if !before.trim_end().ends_with('.') { Shape::Bare }
        else if let Some(m) = re!(r"(?:^|[^\w$.)\]])(self|Self|super)\s*[?!]?\s*\.\s*$").captures(before) {
            if &m[1] == "super" { Shape::SuperCall } else { Shape::SelfCall }
        } else { Shape::Chain };
        let before = re!(r"[?!]\s*\.").replace_all(before, ".");
        Some(CallSite {
            shape, receiver: receiver_name(&before),
            label: re!(r"^\s*\(\s*([A-Za-z_]\w*)\s*:[^:]").captures(after).map(|m| m[1].to_string()).unwrap_or_default(),
            subscript: after.trim_start().starts_with('['),
        })
    }

    /// A declared callee result admits a chain; unknown receivers remain heuristic.
    pub(super) fn kotlin_chain_evidence(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<Option<KotlinChainEvidence>> {
        if r.language != "kotlin" || r.reference_kind != "calls" || r.reference_name != n.name || !is_std_method("kotlin", &n.name) { return Ok(None); }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return Ok(None) };
        let code = super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(line));
        let pat = Self::cached_regex(&format!(r"(?:^|[^\w])({})\s*\(", regex::escape(&n.name)))?;
        let column = js_unit_to_byte(&code, r.column.max(0) as usize);
        let Some(at) = pat.captures_iter(&code).filter_map(|m| m.get(1)).find(|m| m.start() >= column).map(|m| m.start()) else { return Ok(None) };
        let before = code[..at].trim_end().trim_end_matches('.').trim_end();
        let receiver = self.kotlin_chain_receiver_call(r)
            .map(|call| super::awaited::blank_string_contents(&super::awaited::strip_ts_comments(&call)));
        let before = receiver.as_deref().map(str::trim_end).unwrap_or(before);
        if !before.ends_with(')') { return Ok(None); }
        let mut depth = 0usize;
        let mut open = None;
        for (i, ch) in before.char_indices().rev() {
            if ch == ')' { depth += 1; }
            else if ch == '(' { depth -= 1; if depth == 0 { open = Some(i); break; } }
        }
        let Some(open) = open else { return Ok(None) };
        let Some(call) = re!(r"(?:([A-Za-z_]\w*)\s*\.)?([A-Za-z_]\w*)(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\s*$").captures(&before[..open]) else { return Ok(None) };
        if call.get(1).is_none() && before[..open][..call.get(0).unwrap().start()].trim_end().ends_with('.') { return Ok(None); }
        let name = &call[2];
        let receiver_type = match call.get(1) {
            Some(receiver) => self.infer_local_receiver_type(receiver.as_str(), r, true)?,
            None => None,
        };
        let mut factory = match &receiver_type {
            Some(ty) => self.match_bound_type_member(ty, name, r)?.map(|c| c.node),
            None => None,
        };
        if factory.is_none() {
            let callee_ref = r.clone().naming(name, "calls");
            let imported = self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == name);
            if imported {
                factory = self.resolve_via_import(&callee_ref)?.map(|c| c.node);
                if factory.is_none() {
                    // Kotlin extensions are methods qualified by the receiver,
                    // while their imports name the package and function.
                    let mappings = self.import_mappings(&r.file_path)?;
                    let mut extensions = Vec::new();
                    for candidate in self.nodes_by_name(name)?.iter().filter(|c| c.language == "kotlin" && c.kind == "method") {
                        let pkg = self.kotlin_file_scope(&candidate.file_path).pkg.clone();
                        let imported_name = format!("{pkg}.{}", candidate.name);
                        if !mappings.iter().any(|m| m.local_name == name && m.source == imported_name) { continue; }
                        if self.nodes_in_file(&candidate.file_path)?.iter().any(|c| {
                            type_kind(&c.kind) && c.kind != "namespace" && c.kind != "module"
                                && c.start_line <= candidate.start_line && c.end_line >= candidate.end_line
                        }) { continue; }
                        extensions.push(candidate.clone());
                    }
                    if extensions.len() == 1 { factory = extensions.pop(); }
                }
            }
            else if call.get(1).is_none() {
                let bindings = self.bindings(&r.file_path)?;
                factory = match innermost_binding(&bindings, name, Some(r.line)) {
                    Some(binding) => self.node_by_opt_id(binding.node_id.as_deref())?,
                    None => None,
                };
            }
            if let (Some(ty), Some(f)) = (&receiver_type, &factory) {
                let mut declaration = r.clone(); declaration.file_path = f.file_path.clone(); declaration.line = f.start_line;
                let extension_owner = owner(f).unwrap_or("");
                let expected = self.resolve_bound_type(extension_owner, &declaration, 0)?;
                let actual = self.resolve_bound_type(ty, r, 0)?;
                if !expected.zip(actual).is_some_and(|(a, b)| a.id == b.id) { return Ok(None); }
            }
        }
        let Some(factory) = factory else { return Ok(None) };
        let Some(raw) = &factory.return_type else { return Ok(None) };
        let ty = raw.split('<').next().unwrap_or(raw).trim().trim_end_matches('?');
        if factory.type_parameters.as_ref().is_some_and(|ps| ps.iter().any(|p| p.split_whitespace().next() == Some(ty))) { return Ok(None); }
        let mut declaration = r.clone(); declaration.file_path = factory.file_path.clone(); declaration.line = factory.start_line; declaration.from_node_id = factory.id.clone();
        if call.get(1).is_some() && receiver_type.is_none() {
            // A declared conflicting member can shadow the imported extension.
            // Unannotated competitors keep the result uncertain; the shared
            // target gate caps this hypothesis below the trust line.
            for candidate in self.nodes_by_name(&factory.name)?.iter().filter(|c| {
                matches!(c.language.as_str(), "kotlin" | "java") && matches!(c.kind.as_str(), "method" | "function")
            }) {
                let Some(raw) = &candidate.return_type else { continue };
                let ty = raw.split('<').next().unwrap_or(raw).trim().trim_end_matches('?');
                let mut site = declaration.clone(); site.file_path = candidate.file_path.clone(); site.line = candidate.start_line; site.from_node_id = candidate.id.clone();
                if !self.match_bound_type_member(ty, &n.name, &site)?.is_some_and(|member| member.node.id == n.id) { return Ok(None); }
            }
        }
        Ok(self.match_bound_type_member(ty, &n.name, &declaration)?
            .filter(|c| c.node.id == n.id)
            .map(|_| if receiver_type.is_some() { KotlinChainEvidence::Bound } else { KotlinChainEvidence::Heuristic }))
    }

    /// Scope predicates only eliminate candidates, never select one.
    pub(super) fn call_shape_target(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some(site) = self.call_site(r) else { return Ok(true) };
        match r.language.as_str() {
            "rust" | "go" => Ok(match site.shape {
                Shape::Path => true,
                Shape::Bare => n.kind != "method" || (r.language == "rust" && n.file_path == r.file_path
                    && self.nodes_in_file(&r.file_path)?.iter().any(|f| f.id != n.id
                        && matches!(f.kind.as_str(), "function" | "method")
                        && f.start_line < n.start_line && f.end_line >= n.end_line
                        && f.start_line <= r.line && f.end_line >= r.line)),
                _ if n.kind != "method" => n.kind != "function",
                _ => !is_std_method(&n.language, &n.name) || matches!(site.receiver.as_str(), "self" | "Self")
                    || (!site.receiver.is_empty() && shares_receiver_word(&site.receiver, n)),
            }),
            "swift" => self.swift_call_target(n, r, &site),
            "scala" => self.scala_call_target(n, r, &site),
            "kotlin" if site.shape == Shape::Chain && is_std_method("kotlin", &r.reference_name) => {
                Ok(!matches!(n.kind.as_str(), "method" | "function") || site.receiver == "this"
                    || (!site.receiver.is_empty() && shares_receiver_word(&site.receiver, n))
                    || self.kotlin_chain_evidence(n, r)?.is_some())
            }
            _ => Ok(true),
        }
    }

    fn swift_head(&mut self, n: &KNode) -> (Vec<String>, bool) {
        let Some(lines) = self.read_file(&n.file_path) else { return (vec![], false) };
        let from = (n.start_line - 1).max(0) as usize;
        let to = (n.end_line.min(n.start_line + 20).max(0) as usize).min(lines.len());
        if from >= to { return (vec![], false); }
        let source = super::awaited::strip_ts_comments(&lines[from..to].join("\n"));
        let flat = flat_head(&source, false);
        let Some(head) = re!(r"(?-u:\b)(class|struct|enum|protocol|extension|actor)\s+[\w.]+\s*([\s\S]*)$").captures(&flat) else {
            return (vec![], false);
        };
        let supers = head[2].trim_start().strip_prefix(':').unwrap_or("").split("where").next().unwrap_or("")
            .split(',').filter_map(|part| re!(r"([A-Za-z_]\w*)\s*$").captures(part).map(|m| m[1].to_string())).collect();
        (supers, &head[1] == "extension")
    }

    fn swift_decl(&mut self, name: &str) -> Res<(Vec<String>, bool)> {
        let mut supers: Vec<String> = match name {
            "RandomAccessCollection" => vec!["BidirectionalCollection"],
            "BidirectionalCollection" | "MutableCollection" | "RangeReplaceableCollection" => vec!["Collection"],
            "Collection" | "LazySequenceProtocol" => vec!["Sequence"],
            "LazyCollectionProtocol" => vec!["Collection", "LazySequenceProtocol"],
            "StringProtocol" => vec!["BidirectionalCollection"],
            "Array" | "ArraySlice" | "ContiguousArray" => vec!["RandomAccessCollection", "MutableCollection", "RangeReplaceableCollection"],
            "String" | "Substring" => vec!["StringProtocol", "RangeReplaceableCollection"],
            "Dictionary" => vec!["Collection"], "Set" => vec!["Collection", "SetAlgebra"],
            "Range" | "ClosedRange" => vec!["RandomAccessCollection"], _ => vec![],
        }.into_iter().map(str::to_string).collect();
        if let Some((_, sup)) = super::member_scope::OBJC_SYSTEM_SUPERS.iter().find(|(ty, _)| *ty == name) { supers.push((*sup).to_string()); }
        let mut project = false;
        for decl in self.nodes_by_name(name)?.iter().filter(|n| n.language == "swift" && type_kind(&n.kind)) {
            let (parents, extension) = self.swift_head(decl);
            supers.extend(parents);
            project |= !extension;
        }
        Ok((supers, project))
    }

    fn swift_hierarchy(&mut self, r: &ResolveRefIn) -> Res<HashMap<String, usize>> {
        let mut queue: VecDeque<(String, usize)> = self.nodes_in_file(&r.file_path)?.iter()
            .filter(|n| n.language == "swift" && type_kind(&n.kind) && n.start_line <= r.line && n.end_line >= r.line)
            .map(|n| (n.name.rsplit('.').next().unwrap_or("").to_string(), 0)).collect();
        let mut seen = HashMap::new();
        while seen.len() < 40 {
            let Some((name, depth)) = queue.pop_front() else { break };
            if seen.contains_key(&name) { continue; }
            seen.insert(name.clone(), depth);
            queue.extend(self.swift_decl(&name)?.0.into_iter().map(|s| (s, depth + 1)));
        }
        Ok(seen)
    }

    fn swift_call_target(&mut self, n: &KNode, r: &ResolveRefIn, site: &CallSite) -> Res<bool> {
        if matches!(n.kind.as_str(), "constant" | "variable") && n.file_path != r.file_path && !site.subscript { return Ok(false); }
        let dirs: Vec<&str> = n.file_path.split('/').collect();
        if let Some(index) = dirs[..dirs.len().saturating_sub(1)].iter().position(|d| d.ends_with("Tests") || d.ends_with(".playground")) {
            if !r.file_path.starts_with(&format!("{}/", dirs[..=index].join("/"))) { return Ok(false); }
        }
        let Some(owner) = owner(n) else {
            return Ok(n.kind != "function" || site.shape == Shape::Bare
                || (site.shape == Shape::Chain && site.receiver.starts_with(|c: char| c.is_ascii_uppercase())));
        };
        if !swift_member(n) { return Ok(true); }
        if site.shape == Shape::Chain {
            if site.receiver.is_empty() || !SWIFT_STD_METHODS.contains(&n.name.as_str()) { return Ok(true); }
            if shares_receiver_word(site.receiver.rsplit('.').next().unwrap_or(""), n) || !self.swift_decl(owner)?.1 { return Ok(true); }
            if site.label.is_empty() || SWIFT_STD_LABELS.contains(&site.label.as_str()) { return Ok(false); }
            let Some(lines) = self.read_file(&n.file_path) else { return Ok(false) };
            let from = (n.start_line - 1).max(0) as usize;
            let to = (n.start_line + 3).max(0) as usize;
            let head = lines[from.min(lines.len())..to.min(lines.len())].join(" ");
            return Ok(Self::cached_regex(&format!(r"(?-u:\b)func\s+{}\s*(?:<[^>]*>)?\s*\(\s*{}\b", regex::escape(&n.name), regex::escape(&site.label)))?.is_match(&head));
        }
        Ok(self.swift_hierarchy(r)?.get(owner).is_some_and(|&d| site.shape != Shape::SuperCall || d > 0))
    }

    pub(super) fn swift_scope_bound(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let Some(site) = self.call_site(r) else { return Ok(false) };
        if site.shape == Shape::Chain { return Ok(false); }
        Ok(owner(n).is_some_and(|o| !o.is_empty()) && self.swift_call_target(n, r, &site)?)
    }

    pub(super) fn nearest_swift_members(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        if r.language != "swift" || r.reference_kind != "calls" { return Ok(candidates); }
        let Some(site) = self.call_site(r) else { return Ok(candidates) };
        if site.shape == Shape::Chain { return Ok(candidates); }
        let hierarchy = self.swift_hierarchy(r)?;
        let depth = |n: &KNode| if swift_member(n) { owner(n).and_then(|o| hierarchy.get(o)).copied() } else { None };
        let Some(nearest) = candidates.iter().filter_map(|n| depth(n)).min() else { return Ok(candidates) };
        Ok(candidates.into_iter().filter(|n| depth(n).is_none_or(|d| d == nearest)).collect())
    }

    fn scala_call_target(&mut self, n: &KNode, r: &ResolveRefIn, site: &CallSite) -> Res<bool> {
        let member = matches!(n.kind.as_str(), "method" | "field" | "property" | "variable" | "constant");
        if site.shape == Shape::Chain {
            if n.file_path == r.file_path { return Ok(true); }
            if member { return Ok(!site.receiver.is_empty() && shares_receiver_word(&site.receiver, n)); }
            if n.kind != "function" { return Ok(true); }
            return Ok(self.read_file(&n.file_path).is_some_and(|lines| {
                let from = (n.start_line - 4).max(0) as usize;
                let to = (n.start_line.max(0) as usize).min(lines.len());
                lines[from.min(to)..to].iter().any(|l| re!(r"^\s*extension\b").is_match(l))
            }));
        }
        let nodes = self.nodes_in_file(&r.file_path)?;
        let lines = self.read_file(&r.file_path);
        if let (Some(f), Some(lines)) = (nodes.iter().filter(|f| matches!(f.kind.as_str(), "method" | "function")
            && f.start_line <= r.line && f.end_line >= r.line).min_by_key(|f| f.end_line - f.start_line), &lines) {
            let from = (f.start_line - 1).max(0) as usize;
            let to = (r.line.max(0) as usize).min(lines.len());
            let text = lines[from.min(to)..to].join("\n");
            let name = regex::escape(&r.reference_name);
            let pattern = format!(r"(?:[(,\[]\s*(?:implicit\s+|using\s+)?{name}\s*:)|(?:\b(?:val|var|def|lazy\s+val)\s+{name}\b)|(?:^|[^\w$.]){name}\s*(?:=>|<-)|\(\s*{name}\s*(?:,[^)]*)?\)\s*=>");
            if Self::cached_regex(&pattern)?.is_match(&text) {
                return Ok(n.file_path == r.file_path && n.start_line >= f.start_line && n.end_line <= f.end_line);
            }
        }
        if !member || n.file_path == r.file_path { return Ok(true); }
        let Some(owner) = owner(n) else { return Ok(true) };
        if let Some(lines) = &lines {
            for m in re!(r"(?m)^\s*import\s+([\w.]+?)\.(?:([_*])|\{([^}]*)\}|([\w$]+))\s*$").captures_iter(lines.text()) {
                let imported = m[1].rsplit('.').next().unwrap_or("");
                let wildcard = m.get(2).is_some() || m.get(3).is_some_and(|x| x.as_str().split(',').any(|v| matches!(v.trim(), "_" | "*")));
                if wildcard && (imported == owner || imported.starts_with(|c: char| c.is_ascii_lowercase())) { return Ok(true); }
                if imported == owner && m.get(3).or_else(|| m.get(4)).is_some_and(|x| x.as_str().split(',').any(|v| v.trim().split("=>").next().unwrap_or("").trim() == r.reference_name)) { return Ok(true); }
            }
        }
        let mut queue: VecDeque<String> = nodes.iter().filter(|t| type_kind(&t.kind) && t.start_line <= r.line && t.end_line >= r.line).map(|t| t.name.clone()).collect();
        if queue.is_empty() { return Ok(true); }
        // Open anonymous class bodies introduce their bases as implicit receivers.
        if let Some(lines) = &lines {
            let mut depth = 0usize;
            let to = (r.line.max(0) as usize).min(lines.len());
            for text in lines[to.saturating_sub(400)..to].iter().rev() {
                for (at, ch) in text.char_indices().rev() {
                    if ch == '}' { depth += 1; }
                    else if ch == '{' {
                        if depth > 0 { depth -= 1; continue; }
                        if let Some(head) = re!(r"\bnew\s+([\w.]+(?:\s*\[[^\]]*\])?(?:\s*\([^)]*\))?(?:\s+with\s+[\w.]+(?:\s*\[[^\]]*\])?)*)\s*$").captures(&text[..at]) {
                            let simple = re!(r"\[[^\]]*\]|\([^)]*\)").replace_all(&head[1], "");
                            queue.extend(simple.split("with").map(|s| s.trim().rsplit('.').next().unwrap_or("").to_string()));
                        }
                    }
                }
            }
        }
        let mut seen = HashSet::new();
        while seen.len() < 60 {
            let Some(name) = queue.pop_front() else { break };
            if !seen.insert(name.clone()) { continue; }
            if name == owner { return Ok(true); }
            for decl in self.nodes_by_name(&name)?.iter().filter(|d| d.language == "scala" && type_kind(&d.kind)) {
                let Some(lines) = self.read_file(&decl.file_path) else { continue };
                let from = (decl.start_line - 1).max(0) as usize;
                let to = ((decl.start_line + 12).max(0) as usize).min(lines.len());
                let flat = flat_head(&lines[from.min(to)..to].join(" "), true);
                let Some((_, clause)) = flat.split_once("extends") else { continue };
                queue.extend(re!(r"[A-Za-z_][\w.]*").find_iter(clause).map(|m| m.as_str().rsplit('.').next().unwrap_or("").to_string()).filter(|s| !matches!(s.as_str(), "with" | "derives") && s != &name));
            }
        }
        Ok(false)
    }
}
