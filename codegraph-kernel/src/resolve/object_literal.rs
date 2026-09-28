//! Object-literal members (#1932): which top-level property of
//! `const api = { … }` a member name selects, and the binding a shorthand
//! (`{ getUser }`) or identifier-valued (`{ run: fetchUser }`) property
//! names. A nested object, a member body, a comment or a string never
//! donates a member; the last own property wins, and a spread or computed
//! key after it hides it. Offsets are UTF-16 units, the unit node columns
//! use; comment and string blanking preserves them.

use super::awaited::{blank_string_contents, has_parameter_binding, strip_ts_comments};
use super::*;

/// What an object literal says about one member name.
pub(super) enum LiteralProperty {
    /// The literal's source couldn't be read or its `{` found.
    Unreadable,
    /// No own property with that name survives.
    Absent,
    /// The winning property: its span within the literal's extent and the
    /// binding it names, if its value is a bare identifier.
    Found { start: usize, end: usize, binding: Option<String> },
}

impl KernelResolver {
    /// objectLiteralProperty (name-matcher.ts).
    pub(super) fn object_literal_property(&mut self, container: &KNode, member: &str) -> LiteralProperty {
        let Some(lines) = self.read_file(&container.file_path) else {
            return LiteralProperty::Unreadable;
        };
        let from = ((container.start_line - 1).max(0) as usize).min(lines.len());
        let to = (container.end_line.max(0) as usize).clamp(from, lines.len());
        if from == to {
            return LiteralProperty::Unreadable;
        }
        let mut extent_lines: Vec<&str> = lines[from..to].iter().map(String::as_str).collect();
        let last = extent_lines.len() - 1;
        extent_lines[last] = js_prefix(extent_lines[last], container.end_column.max(0) as usize);
        extent_lines[0] = js_slice(extent_lines[0], container.start_column.max(0) as usize);
        let extent = strip_ts_comments(&extent_lines.join("\n"));
        let code = blank_string_contents(&extent);
        // Start at this declarator, never a sibling on the same line.
        let Some(open) = re!(r"^[^=]*=\s*(?:(?:Object\.(?:freeze|seal)\s*)?\(\s*)*\{").find(&code) else {
            return LiteralProperty::Unreadable;
        };
        let code16: Vec<u16> = code.encode_utf16().collect();
        let extent16: Vec<u16> = extent.encode_utf16().collect();

        let mut parts: Vec<(usize, usize)> = Vec::new();
        let mut depth = 0i32;
        let mut start = utf16_len(open.as_str());
        for (i, &unit) in code16.iter().enumerate().skip(start) {
            match unit {
                0x7B | 0x28 | 0x5B => depth += 1, // { ( [
                0x29 | 0x5D => depth -= 1,        // ) ]
                0x7D if depth == 0 => {
                    parts.push((start, i));
                    break;
                }
                0x7D => depth -= 1,
                0x2C if depth == 0 => {
                    parts.push((start, i));
                    start = i + 1;
                }
                _ => {}
            }
        }

        let key_re = re!(r#"^(?:(?:async|get|set)\s+)?\*?\s*(?:([A-Za-z_$][A-Za-z0-9_$]*)|['"]([^'"\\]*)['"])"#);
        let mut selected = None;
        for (s, e) in parts {
            let raw = String::from_utf16_lossy(&extent16[s..e.min(extent16.len())]);
            let text = raw.trim();
            if text.starts_with("...") || text.starts_with('[') {
                selected = None;
                continue;
            }
            let Some(key) = key_re.captures(text) else { continue };
            let rest = &text[key.get(0).unwrap().end()..];
            // The key ends at `:`, `(`, `<`, `,`, `=` or the property's end.
            let after = rest.trim_start();
            if !(after.is_empty() || after.starts_with([':', '(', '<', ',', '='])) {
                continue;
            }
            let name = key.get(1).or_else(|| key.get(2)).map_or("", |m| m.as_str());
            if name != member {
                continue;
            }
            let value = rest.trim();
            let binding = if value.is_empty() {
                Some(member.to_string())
            } else {
                re!(r"^:\s*([A-Za-z_$][A-Za-z0-9_$]*)$").captures(value).map(|c| c[1].to_string())
            };
            selected = Some(LiteralProperty::Found { start: s, end: e, binding });
        }
        selected.unwrap_or(LiteralProperty::Absent)
    }

    /// Whether `node` starts inside the property span `[start, end)` of
    /// `container`'s extent.
    pub(super) fn literal_property_contains(&mut self, container: &KNode, node: &KNode, start: usize, end: usize) -> bool {
        let Some(lines) = self.read_file(&container.file_path) else {
            return false;
        };
        let mut offset = node.start_column - container.start_column;
        for line in container.start_line..node.start_line {
            offset += lines.get((line - 1).max(0) as usize).map_or(0, |l| utf16_len(l) as i64) + 1;
        }
        offset >= start as i64 && offset < end as i64
    }

    /// resolveObjectLiteralBinding (name-matcher.ts): `api.getUser()` where
    /// `api` is `{ getUser }` or `{ getUser: fetchUser }` and the function is
    /// declared outside the literal. Follows the named binding — the nearest
    /// declaration in scope where the literal is written, else the literal
    /// file's import — unless a parameter of an enclosing function shadows it.
    pub(super) fn resolve_object_literal_binding(
        &mut self,
        container: &KNode,
        member: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        let LiteralProperty::Found { binding: Some(binding), .. } = self.object_literal_property(container, member) else {
            return Ok(None);
        };
        let in_file = self.nodes_in_file(&container.file_path)?;
        let encloses = |outer: &KNode| {
            (outer.start_line, outer.start_column) <= (container.start_line, container.start_column)
                && (outer.end_line, outer.end_column) >= (container.end_line, container.end_column)
        };
        let param_shadowed = in_file.iter().any(|n| {
            (n.kind == "function" || n.kind == "method")
                && encloses(n)
                && n.signature.as_deref().is_some_and(|s| has_parameter_binding(&format!("{s} {{"), &binding))
        });
        if param_shadowed {
            return Ok(None);
        }

        let Some(lines) = self.read_file(&container.file_path) else {
            return Ok(None);
        };
        let code: Vec<u16> = blank_string_contents(&strip_ts_comments(&lines.join("\n"))).encode_utf16().collect();
        let mut line_starts = vec![0usize];
        line_starts.extend(code.iter().enumerate().filter(|(_, &u)| u == 0x0A).map(|(i, _)| i + 1));
        // The open-brace positions enclosing `node`'s start.
        let scope_at = |node: &KNode| {
            let line = line_starts.get((node.start_line - 1).max(0) as usize).copied().unwrap_or(code.len());
            let end = (line + node.start_column.max(0) as usize).min(code.len());
            let mut scope: Vec<usize> = Vec::new();
            for (i, &u) in code[..end].iter().enumerate() {
                if u == 0x7B {
                    scope.push(i);
                } else if u == 0x7D {
                    scope.pop();
                }
            }
            scope
        };
        let scope = scope_at(container);

        let calls = r.reference_kind == "calls";
        let accepts = |n: &KNode| {
            let callable = matches!(n.kind.as_str(), "function" | "method" | "class");
            callable || (!calls && matches!(n.kind.as_str(), "constant" | "variable" | "component"))
        };
        let cand = |node| KCand { node, confidence: 0.85, resolved_by: "instance-method" };

        // The nearest visible declaration wins before callability is checked:
        // a nearer value shadows an outer function even if it can't be called.
        let mut locals: Vec<(Arc<KNode>, usize)> = in_file
            .iter()
            .filter(|n| {
                n.name == binding
                    && n.id != container.id
                    && matches!(n.kind.as_str(), "function" | "class" | "constant" | "variable" | "component")
            })
            .filter_map(|n| {
                let s = scope_at(n);
                s.iter().enumerate().all(|(i, p)| scope.get(i) == Some(p)).then(|| (n.clone(), s.len()))
            })
            .collect();
        locals.sort_by_key(|l| std::cmp::Reverse(l.1));
        if let Some((local, _)) = locals.into_iter().next() {
            return Ok(accepts(&local).then(|| cand(local)));
        }

        let mut at = r.clone().at(container).naming(&binding, &r.reference_kind);
        at.language = container.language.clone();
        at.from_node_id = container.id.clone();
        at.column = container.start_column;
        Ok(self.resolve_via_import(&at)?.filter(|hit| accepts(&hit.node)).map(|hit| cand(hit.node)))
    }
}
