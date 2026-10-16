//! C++ explicit specializations (name-matcher.ts cppSpecializationPool, upstream #2609). An
//! explicit specialization is named like the template it specializes, so a call meets both among
//! that name's candidates; the template arguments the call writes choose between them. The
//! TypeScript synthesizers read the same arguments (src/resolution/cpp-specializations.ts).

use super::*;

impl KernelResolver {
    /// Of the C++ functions `candidates` a call may reach, the ones the template arguments it
    /// writes allow:
    ///  - a call that writes template arguments is to the specialization written with the same
    ///    ones (rocksdb's `Deserialize<int64_t>(src, offset)` is `template <> int64_t
    ///    Deserialize<int64_t>(…)`), else to the template itself or an ordinary overload: fmt's
    ///    `num_bits<unsigned short>()` is `num_bits<T>()`, not the `num_bits<uint128>()` beside the
    ///    call. None is left when specializations are all there is: protobuf's template
    ///    `ReadPrimitive<CType, DeclaredType>(…)` calls whichever one its arguments become.
    ///  - a call that writes none, or another reference, reaches a specialization only when there
    ///    is nothing else.
    ///
    /// The candidates as they are when none is a specialization. Each template's specializations
    /// choose among the candidates of its own qualified name only: one of `a::f` leaves a call's
    /// `b::f` in place, for the call's scope to choose between them.
    pub(super) fn cpp_specialization_pool(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Vec<Arc<KNode>> {
        if r.language != "cpp" { return candidates; }
        let written: Vec<Option<Rc<Vec<String>>>> = candidates.iter().map(|n| self.cpp_specialization_arguments(n)).collect();
        if written.iter().all(Option::is_none) { return candidates; }
        let args = self.cpp_call_template_arguments(r);
        let mut families: Vec<&str> = Vec::new();
        for n in &candidates { if !families.contains(&n.qualified_name.as_str()) { families.push(&n.qualified_name); } }
        let mut kept: HashSet<usize> = HashSet::new();
        for family in families {
            let members: Vec<usize> = (0..candidates.len()).filter(|&i| candidates[i].qualified_name == family).collect();
            let others: Vec<usize> = members.iter().copied().filter(|&i| written[i].is_none()).collect();
            if others.len() == members.len() { kept.extend(members); continue; }
            let Some(args) = &args else {
                kept.extend(if others.is_empty() { members } else { others });
                continue;
            };
            let named: Vec<usize> = members.iter().copied().filter(|&i| written[i].as_ref().is_some_and(|w| cpp_template_arguments_match(w, args))).collect();
            kept.extend(if named.is_empty() { others } else { named });
        }
        candidates.into_iter().enumerate().filter(|(i, _)| kept.contains(i)).map(|(_, n)| n).collect()
    }

    /// The template arguments the C++ function or method `n` is an explicit specialization for,
    /// as its definition writes them after its name: `int32_t` and `WireFormatLite::TYPE_INT32` for
    /// protobuf's `template <> inline bool WireFormatLite::ReadPrimitive<int32_t,
    /// WireFormatLite::TYPE_INT32>(…)`. A method a macro defines is read from its expansion. None
    /// for a template's own definition, any other function, and a specialization that leaves its
    /// arguments to be deduced.
    pub(super) fn cpp_specialization_arguments(&mut self, n: &KNode) -> Option<Rc<Vec<String>>> {
        // A constructor, whose signature is its parameter list, is never specialized by name:
        // gmock's `template <typename Func> Action(const Action<Func>& action)` writes
        // `Action<Func>` for a parameter.
        if n.language != "cpp" || (n.kind != "function" && n.kind != "method") || n.signature.as_deref().is_some_and(|s| s.starts_with('(')) { return None; }
        if let Some(hit) = self.cpp_specializations.get(&n.id) { return hit.clone(); }
        // (A method a macro defines carries its expansion as its signature.)
        let head = match n.signature.as_deref().filter(|s| !s.is_empty()) {
            Some(signature) => signature.to_string(),
            None => self.read_file(&n.file_path).map(|source| {
                let from = (n.start_line - 1).max(0) as usize;
                let text = source.iter().skip(from).take(12).cloned().collect::<Vec<_>>().join("\n");
                let column = source.get(from).map_or(0, |first| super::names::js_unit_to_byte(first, n.start_column.max(0) as usize));
                text.get(column..).unwrap_or("").to_string()
            }).unwrap_or_default(),
        };
        let args = cpp_template_arguments_after(&head, &n.name, 0).map(Rc::new);
        self.cpp_specializations.insert(n.id.clone(), args.clone());
        args
    }

    /// The template arguments the C++ call `r` writes after the name it calls (`unsigned short`
    /// for fmt's `num_bits<unsigned short>()`), or None when it writes none, leaving them to be
    /// deduced.
    fn cpp_call_template_arguments(&mut self, r: &ResolveRefIn) -> Option<Vec<String>> {
        let source = self.read_file(&r.file_path)?;
        let from = (r.line - 1).max(0) as usize;
        let text = source.iter().skip(from).take(4).cloned().collect::<Vec<_>>().join("\n");
        let column = source.get(from).map_or(0, |first| super::names::js_unit_to_byte(first, r.column.max(0) as usize));
        let name = re!(r"::|\.|->").split(&r.reference_name).last().unwrap_or("");
        cpp_template_arguments_after(&text, name, column)
    }
}

/// The template arguments `text` writes right after `name`, the first time from `from` on it is
/// written as a word of its own, as they compare (cpp_template_argument_key). None when it writes
/// none there, or `<>`. A receiver or qualifier of the same name is passed over: in
/// `foo.foo<long>()` the arguments follow the second `foo`.
fn cpp_template_arguments_after(text: &str, name: &str, from: usize) -> Option<Vec<String>> {
    if name.is_empty() { return None; }
    let bytes = text.as_bytes();
    let word = |at: Option<&u8>| at.is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
    let mut at = text.get(from..).and_then(|rest| rest.find(name)).map(|i| i + from);
    while let Some(start) = at {
        let open = start + name.len();
        let receiver = re!(r"^\s*(?:\.|->|::)").is_match(text.get(open..).unwrap_or(""));
        if !word(start.checked_sub(1).and_then(|i| bytes.get(i))) && !word(bytes.get(open)) && !receiver {
            let end = if bytes.get(open) == Some(&b'<') { cpp_template_arguments_end(bytes, open) } else { None };
            let args: Vec<String> = end.map(|end| split_cpp_top_level(&text[open + 1..end]).iter().map(|a| cpp_template_argument_key(a)).collect()).unwrap_or_default();
            return (!args.is_empty()).then_some(args);
        }
        at = text.get(start + 1..).and_then(|rest| rest.find(name)).map(|i| i + start + 1);
    }
    None
}

/// A C++ template argument as it compares: spaced only between words.
fn cpp_template_argument_key(arg: &str) -> String {
    let tight = re!(r"\s*([^\w\s])\s*").replace_all(arg, "$1");
    re!(r"\s+").replace_all(&tight, " ").into_owned()
}

/// Whether a call's template arguments `call` are the ones a specialization writes, `written`:
/// each the same, or the same name qualified further on one side (`TYPE_INT64` inside the class,
/// `WireFormatLite::TYPE_INT64` outside it).
fn cpp_template_arguments_match(written: &[String], call: &[String]) -> bool {
    written.len() == call.len() && written.iter().zip(call).all(|(w, c)| w == c || w.ends_with(&format!("::{c}")) || c.ends_with(&format!("::{w}")))
}

/// Split `a, f(b, c), d<e, f>` at its top-level commas.
fn split_cpp_top_level(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    // Where each template argument list open around the current character closes.
    let mut closes: Vec<usize> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'"' || ch == b'\'' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != ch { j += if bytes[j] == b'\\' { 2 } else { 1 }; }
            i = j + 1;
            continue;
        }
        match ch {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'<' => { if let Some(end) = cpp_template_arguments_end(bytes, i).filter(|&end| end > i) { closes.push(end); } }
            b'>' if closes.last() == Some(&i) => { closes.pop(); }
            b',' if depth == 0 && closes.is_empty() => {
                out.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let rest = text.get(start..).unwrap_or("").trim();
    if !rest.is_empty() { out.push(rest.to_string()); }
    out
}

/// Where the template argument list the `<` at `at` opens closes: the index of its `>`, or None
/// when the `<` compares or shifts.
fn cpp_template_arguments_end(bytes: &[u8], at: usize) -> Option<usize> {
    let before = at.checked_sub(1).and_then(|i| bytes.get(i));
    if !before.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') || matches!(bytes.get(at + 1), Some(b'<') | Some(b'=')) { return None; }
    let mut angles = 0usize;
    let mut others = 0usize;
    let mut i = at;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'"' || ch == b'\'' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != ch { j += if bytes[j] == b'\\' { 2 } else { 1 }; }
            i = j;
        } else if matches!(ch, b'(' | b'[' | b'{') {
            others += 1;
        } else if matches!(ch, b')' | b']' | b'}') {
            if others == 0 { return None; }
            others -= 1;
        } else if others == 0 {
            if ch == b'<' {
                angles += 1;
            } else if ch == b'>' && (i == 0 || bytes[i - 1] != b'-') {
                angles = angles.saturating_sub(1);
                if angles == 0 { return Some(i); }
            }
        }
        i += 1;
    }
    None
}
