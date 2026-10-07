//! Dart lexical binding scopes. Parameters and locals never call unrelated indexed declarations.
use super::dart_libraries::{tokens, DartToken};
use super::*;

#[derive(Clone)]
struct Scope {
    name: String,
    start: usize,
    end: usize,
    function: bool,
}
#[derive(Default)]
pub(super) struct DartLocals {
    files: HashMap<String, Vec<Scope>>,
}
fn word(t: &DartToken) -> bool {
    !t.string && re!(r"^[A-Za-z_$][\w$]*$").is_match(&t.text)
}
fn reserved(name: &str) -> bool {
    matches!(
        name,
        "assert"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "default"
            | "do"
            | "else"
            | "enum"
            | "extends"
            | "false"
            | "final"
            | "finally"
            | "for"
            | "if"
            | "in"
            | "is"
            | "new"
            | "null"
            | "rethrow"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "var"
            | "void"
            | "while"
            | "with"
    )
}
fn keyword(name: &str) -> bool {
    reserved(name)
        || matches!(
            name,
            "abstract"
                | "as"
                | "async"
                | "await"
                | "base"
                | "covariant"
                | "deferred"
                | "export"
                | "extension"
                | "external"
                | "factory"
                | "get"
                | "hide"
                | "implements"
                | "import"
                | "interface"
                | "late"
                | "library"
                | "mixin"
                | "of"
                | "on"
                | "operator"
                | "part"
                | "required"
                | "sealed"
                | "set"
                | "show"
                | "static"
                | "sync"
                | "typedef"
                | "when"
                | "yield"
        )
}
fn bind(scopes: &mut Vec<Scope>, name: &str, start: usize, end: usize, function: bool) {
    if name != "_" && !reserved(name) {
        scopes.push(Scope {
            name: name.into(),
            start,
            end,
            function,
        });
    }
}
fn statement_end(
    t: &[DartToken],
    pairs: &[Option<usize>],
    mut i: usize,
    expression: bool,
    eof: usize,
) -> usize {
    while i < t.len() {
        let text = t[i].text.as_str();
        if !t[i].string && (matches!(text, ";" | ")" | "]" | "}") || expression && text == ",") {
            return t[i].start;
        }
        if !t[i].string && matches!(text, "(" | "[" | "{") {
            if let Some(end) = pairs[i] {
                i = end + 1;
                continue;
            } else {
                return eof;
            }
        }
        i += 1;
    }
    eof
}
fn pattern_names(t: &[DartToken]) -> Vec<String> {
    t.iter()
        .enumerate()
        .filter(|(i, n)| {
            word(n)
                && !reserved(&n.text)
                && n.text != "_"
                && !t
                    .get(i + 1)
                    .is_some_and(|next| matches!(next.text.as_str(), ":" | "(") || word(next))
                && !i.checked_sub(1).is_some_and(|p| t[p].text == ".")
        })
        .map(|(_, n)| n.text.clone())
        .collect()
}
fn parameters(t: &[DartToken]) -> Vec<String> {
    let mut names = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    for i in 0..=t.len() {
        if i < t.len() && !t[i].string {
            match t[i].text.as_str() {
                "{" | "[" if depth == 0 && start == i => {
                    let end = t.len().saturating_sub(1);
                    names.extend(parameters(&t[i + 1..end.max(i + 1)]));
                    break;
                }
                "(" | "[" | "{" | "<" => depth += 1,
                ")" | "]" | "}" | ">" => depth -= 1,
                _ => {}
            }
        }
        if i == t.len() || depth == 0 && t[i].text == "," {
            let item = &t[start..i];
            let mut end = item
                .iter()
                .position(|n| !n.string && n.text == "=")
                .unwrap_or(item.len());
            if end > 0 && item[end - 1].text == ")" {
                let mut nesting = 0;
                for j in (0..end).rev() {
                    if item[j].text == ")" {
                        nesting += 1;
                    }
                    if item[j].text == "(" {
                        nesting -= 1;
                        if nesting == 0 {
                            end = j;
                            break;
                        }
                    }
                }
            }
            if let Some(name) = item[..end].last().filter(|n| word(n)) {
                names.push(name.text.clone());
            }
            start = i + 1;
        }
    }
    names
}
fn scopes(source: &str) -> Vec<Scope> {
    let t = tokens(source);
    let mut pairs = vec![None; t.len()];
    let mut parents = vec![None; t.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (i, token) in t.iter().enumerate() {
        parents[i] = stack.last().copied();
        if token.string {
            continue;
        }
        if matches!(token.text.as_str(), "(" | "[" | "{") {
            stack.push(i);
        } else if matches!(token.text.as_str(), ")" | "]" | "}") {
            let want = match token.text.as_str() {
                ")" => "(",
                "]" => "[",
                _ => "{",
            };
            if let Some(j) = stack.iter().rposition(|p| t[*p].text == want) {
                let open = stack[j];
                stack.truncate(j);
                pairs[i] = Some(open);
                pairs[open] = Some(i);
                parents[i] = parents[open];
            }
        }
    }
    let type_body = |i: usize| {
        let start = (0..i)
            .rfind(|j| matches!(t[*j].text.as_str(), ";" | "{" | "}"))
            .map_or(0, |j| j + 1);
        t[start..i]
            .iter()
            .any(|n| matches!(n.text.as_str(), "class" | "mixin" | "enum" | "extension"))
    };
    let block_end = |i: usize| pairs[i].map_or(source.len(), |j| t[j].start);
    let mut out = Vec::new();
    let mut parameter_lists = HashSet::new();
    for (close, _) in t
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.string && n.text == ")")
    {
        let Some(open) = pairs[close] else {
            continue;
        };
        let head = open.checked_sub(1).and_then(|i| t.get(i));
        let mut body = close + 1;
        if t.get(body)
            .is_some_and(|n| matches!(n.text.as_str(), "async" | "sync"))
        {
            body += 1;
            if t.get(body).is_some_and(|n| n.text == "*") {
                body += 1;
            }
        }
        let block = t
            .get(body)
            .filter(|n| n.text == "{")
            .and_then(|_| pairs[body]);
        if head.is_some_and(|h| h.text == "for") {
            let list = &t[open + 1..close];
            let end = block.map_or_else(
                || statement_end(&t, &pairs, body, false, source.len()),
                |j| t[j].start,
            );
            let before_in = list.iter().position(|n| n.text == "in").map(|i| &list[..i]);
            let names = if let Some(list) = before_in {
                if list
                    .first()
                    .is_some_and(|n| matches!(n.text.as_str(), "var" | "final" | "const"))
                {
                    if list
                        .iter()
                        .any(|n| matches!(n.text.as_str(), "(" | "[" | "{"))
                    {
                        pattern_names(list)
                    } else {
                        list.last()
                            .filter(|n| word(n))
                            .map(|n| vec![n.text.clone()])
                            .unwrap_or_default()
                    }
                } else if list.len() >= 2 {
                    list.last()
                        .filter(|n| word(n))
                        .map(|n| vec![n.text.clone()])
                        .unwrap_or_default()
                } else {
                    vec![]
                }
            } else {
                let init = &list[..list
                    .iter()
                    .position(|n| n.text == ";")
                    .unwrap_or(list.len())];
                if init
                    .first()
                    .is_some_and(|n| matches!(n.text.as_str(), "var" | "final" | "const" | "late"))
                    || init.len() > 2 && word(&init[0]) && word(&init[1])
                {
                    init.windows(2)
                        .filter(|p| word(&p[0]) && p[1].text == "=")
                        .map(|p| p[0].text.clone())
                        .collect()
                } else {
                    vec![]
                }
            };
            for name in names {
                bind(&mut out, &name, t[open].start, end, false);
            }
            parameter_lists.insert(open);
            continue;
        }
        if head.is_some_and(|h| {
            matches!(
                h.text.as_str(),
                "if" | "while" | "switch" | "super" | "this" | "assert"
            )
        }) {
            continue;
        }
        if head.is_some_and(word)
            && open >= 2
            && matches!(t[open - 2].text.as_str(), "=" | ":" | ",")
        {
            continue;
        }
        if head.is_some_and(word)
            && open >= 3
            && t[open - 2].text == "."
            && matches!(t[open - 3].text.as_str(), "super" | "this")
        {
            continue;
        }
        let extent = if let Some(end) = block {
            Some((t[body].start, t[end].start))
        } else if t.get(body).is_some_and(|n| n.text == "=")
            && t.get(body + 1).is_some_and(|n| n.text == ">")
        {
            Some((
                t[body].start,
                statement_end(&t, &pairs, body + 2, true, source.len()),
            ))
        } else if t.get(body).is_some_and(|n| n.text == ":")
            && head.is_some_and(|h| h.text.starts_with(|c: char| c.is_ascii_uppercase()))
        {
            Some((
                t[body].start,
                statement_end(&t, &pairs, body + 1, false, source.len()),
            ))
        } else {
            None
        };
        let Some((start, end)) = extent else {
            continue;
        };
        parameter_lists.insert(open);
        for name in parameters(&t[open + 1..close]) {
            bind(&mut out, &name, start, end, false);
        }
        if let Some(h) = head.filter(|h| word(h) && !keyword(&h.text)) {
            if let Some(around) = parents[open].filter(|p| t[*p].text == "{" && !type_body(*p)) {
                bind(&mut out, &h.text, h.start, block_end(around), true);
            }
        }
    }
    for (i, n) in t.iter().enumerate().filter(|(_, n)| word(n)) {
        if reserved(&n.text) || i == 0 || t[i - 1].text == "." {
            continue;
        }
        let mut parent = parents[i];
        let prior = &t[i - 1];
        let next = t.get(i + 1);
        let normal = next.is_some_and(|n| matches!(n.text.as_str(), "=" | ";"))
            && (matches!(prior.text.as_str(), "final" | "var" | "const")
                || word(prior) && !keyword(&prior.text)
                || matches!(prior.text.as_str(), "?" | ">")
                || prior.text == ")"
                    && pairs[i - 1].is_some_and(|p| p > 0 && t[p - 1].text == "Function"));
        let pattern = matches!(prior.text.as_str(), "final" | "var")
            && !(next.is_some_and(|n| n.text == "?") && t.get(i + 2).is_some_and(word))
            && next.is_some_and(|n| {
                matches!(
                    n.text.as_str(),
                    ")" | "}" | "]" | "," | ":" | "?" | "=" | "when" | "&" | "|"
                )
            });
        if !normal && !pattern {
            continue;
        }
        if pattern {
            while parent.is_some_and(|p| t[p].text != "{") {
                let p = parent.unwrap();
                if parameter_lists.contains(&p) {
                    parent = None;
                    break;
                }
                parent = parents[p];
            }
        }
        if let Some(p) = parent.filter(|p| t[*p].text == "{" && !type_body(*p)) {
            bind(&mut out, &n.text, n.start, block_end(p), false);
        }
    }
    for (i, n) in t
        .iter()
        .enumerate()
        .filter(|(_, n)| matches!(n.text.as_str(), "final" | "var"))
    {
        let mut open = i + 1;
        if t.get(open).is_some_and(word) {
            open += 1;
        }
        if !t
            .get(open)
            .is_some_and(|n| matches!(n.text.as_str(), "(" | "[" | "{"))
        {
            continue;
        }
        let Some(end) = pairs[open] else {
            continue;
        };
        if !t.get(end + 1).is_some_and(|n| n.text == "=") {
            continue;
        }
        if let Some(p) = parents[i].filter(|p| t[*p].text == "{" && !type_body(*p)) {
            for name in pattern_names(&t[open + 1..end]) {
                bind(&mut out, &name, n.start, block_end(p), false);
            }
        }
    }
    out
}
impl KernelResolver {
    pub(super) fn dart_local_binding(&mut self, name: &str, r: &ResolveRefIn) -> Option<bool> {
        let source = self.read_file(&r.file_path)?;
        if !self.dart_locals.files.contains_key(&r.file_path) {
            self.dart_locals
                .files
                .insert(r.file_path.clone(), scopes(source.text()));
        }
        let line = source.get((r.line - 1).max(0) as usize)?;
        let column = super::lang_scope::name_start_at_column(line, name, r.column.max(0) as usize)
            .or_else(|| {
                line.match_indices(name)
                    .min_by_key(|(at, _)| at.abs_diff(r.column.max(0) as usize))
                    .map(|(at, _)| at)
            })?;
        let before = line[..column].trim_end();
        if before.ends_with('.')
            || before.is_empty()
                && r.line > 1
                && source
                    .get((r.line - 2) as usize)
                    .is_some_and(|l| l.split("//").next().unwrap_or("").trim_end().ends_with('.'))
        {
            return None;
        }
        let at = source
            .iter()
            .take((r.line - 1).max(0) as usize)
            .map(|l| l.len() + 1)
            .sum::<usize>()
            + column;
        self.dart_locals
            .files
            .get(&r.file_path)?
            .iter()
            .filter(|s| s.name == name && s.start <= at && at <= s.end)
            .max_by_key(|s| s.start)
            .map(|s| s.function)
    }
}
