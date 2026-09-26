//! Receiver evidence from a scoped construct (receiver-iteration.ts):
//! inferIterationReceiver — Kotlin `x.let { it.m() }` / `x.also { v -> v.m() }`
//! and Go `for _, v := range xs { v.m() }` — and inferGuardedReceiver, a PHP
//! `if ($x instanceof T) { $x->m(); }` body.
//!
//! The TS side walks the kernel's serialized tree (tree.rs), whose positions
//! are UTF-16 columns; this walks the tree-sitter tree directly and converts
//! byte columns the same way, so `descendantForPosition` picks the same node.

use super::*;
use tree_sitter::Node as TsNode;

/// A receiver type and the site its type resolves from.
pub(super) struct IterationHit {
    pub(super) ty: String,
    pub(super) site: ResolveRefIn,
}

/// (row, UTF-16 column) of a node boundary, as the TS facade reports it.
fn point16(text: &str, byte: usize, point: tree_sitter::Point) -> (usize, usize) {
    let start = byte.saturating_sub(point.column);
    (point.row, utf16_len(&text[start..byte]))
}

fn before(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

/// descendantForPosition (tree.ts): descend into the first child, named or
/// not, whose span contains the point, until none does.
fn descendant_for_position<'t>(root: TsNode<'t>, text: &str, at: (usize, usize)) -> TsNode<'t> {
    let mut node = root;
    'down: loop {
        let mut cursor = node.walk();
        for c in node.children(&mut cursor) {
            let start = point16(text, c.start_byte(), c.start_position());
            let end = point16(text, c.end_byte(), c.end_position());
            if before(start, at) && before(at, end) {
                node = c;
                continue 'down;
            }
        }
        return node;
    }
}

fn named_children<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// descendantsOfType (tree.ts): preorder, the node itself included.
fn descendants_of_type<'t>(node: TsNode<'t>, kind: &str, out: &mut Vec<TsNode<'t>>) {
    if node.kind() == kind {
        out.push(node);
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        descendants_of_type(c, kind, out);
    }
}

fn node_text<'a>(node: TsNode, text: &'a str) -> &'a str {
    &text[node.start_byte()..node.end_byte()]
}

impl KernelResolver {
    /// inferIterationReceiver — `None` when no scoped construct introduces
    /// the receiver (or its type can't be proven).
    pub(super) fn infer_iteration_receiver(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<IterationHit>> {
        // The gate is the TS parse precondition (language, and a declaration
        // shaped like a range/lambda binding).
        if !self.mc_iteration_gate(receiver, r)? {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let text = lines.text().to_string();
        if text.is_empty() {
            return Ok(None);
        }
        let Ok(tree) = crate::tree::parse_with_cached_parser(&text, &r.language) else {
            return Ok(None);
        };
        let at = ((r.line - 1).max(0) as usize, r.column.max(0) as usize);
        let mut cur = Some(descendant_for_position(tree.root_node(), &text, at));
        while let Some(node) = cur {
            cur = node.parent();
            if r.language == "kotlin" && node.kind() == "lambda_literal" {
                let names: Vec<&str> = match named_children(node).into_iter().find(|n| n.kind() == "lambda_parameters") {
                    Some(params) => {
                        let mut ids = Vec::new();
                        descendants_of_type(params, "simple_identifier", &mut ids);
                        ids.into_iter().map(|n| node_text(n, &text)).collect()
                    }
                    None => vec!["it"],
                };
                // The nearest lambda owns implicit `it`, even when its type is unknown.
                if !names.contains(&receiver) {
                    continue;
                }
                let mut call = node.parent();
                while let Some(c) = call {
                    if c.kind() == "call_expression" {
                        break;
                    }
                    call = c.parent();
                }
                let Some(navigation) = call.and_then(|c| named_children(c).into_iter().next()) else {
                    return Ok(None);
                };
                if navigation.kind() != "navigation_expression" {
                    return Ok(None);
                }
                let nav = named_children(navigation);
                let root = nav.first().copied();
                let method = nav.get(1).map(|m| node_text(*m, &text).trim_start_matches(['?', '.']));
                let Some(root) = root.filter(|n| n.kind() == "simple_identifier") else {
                    return Ok(None);
                };
                if !matches!(method, Some("let" | "also")) {
                    return Ok(None);
                }
                let (row, col) = point16(&text, navigation.start_byte(), navigation.start_position());
                let mut site = r.clone();
                site.line = row as i64 + 1;
                site.column = col as i64;
                let root_name = node_text(root, &text).to_string();
                return Ok(self
                    .infer_local_receiver_type(&root_name, &site, true)?
                    .map(|ty| IterationHit { ty, site }));
            }
            if r.language == "go" && node.kind() == "for_statement" {
                let range = named_children(node).into_iter().find(|n| n.kind() == "range_clause");
                let names = range.and_then(|rc| rc.child_by_field_name("left")).map(named_children);
                let collection = range.and_then(|rc| rc.child_by_field_name("right"));
                let (Some(names), Some(collection)) = (names, collection) else {
                    continue;
                };
                let Some(index) = names.iter().position(|n| node_text(*n, &text) == receiver) else {
                    continue;
                };
                if index != 1 {
                    return Ok(None);
                }
                return self.go_range_element(collection, &text, r);
            }
        }
        Ok(None)
    }

    /// The element type of a Go range collection: a field of a typed owner
    /// (`o.items`), a declared slice/array/map, or a factory whose signature
    /// returns a slice.
    fn go_range_element(
        &mut self,
        collection: TsNode,
        text: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<IterationHit>> {
        if collection.kind() == "selector_expression" {
            let base = collection.child_by_field_name("operand");
            let field = collection.child_by_field_name("field");
            let (Some(base), Some(field)) = (base.filter(|b| b.kind() == "identifier"), field) else {
                return Ok(None);
            };
            let Some(owner_type) = self.infer_local_receiver_type(node_text(base, text), r, true)? else {
                return Ok(None);
            };
            // A field's slice element type is resolved in its owner's file.
            let Some(owner) = self.resolve_bound_type(&owner_type, r, 0)? else {
                return Ok(None);
            };
            let Some(owner_lines) = self.read_file(&owner.file_path) else {
                return Ok(None);
            };
            let from = ((owner.start_line - 1).max(0) as usize).min(owner_lines.len());
            let to = (owner.end_line.max(0) as usize).clamp(from, owner_lines.len());
            let body = owner_lines[from..to].join("\n");
            // `\bFIELD\s+\[\d*\]\s*\*?([\w.]+)`
            static FIELD: LazyLock<Affix> = LazyLock::new(|| {
                Affix::new("", r"\s+\[[0-9]*\]\s*\*?([A-Za-z0-9_.]+)", true, false, false)
            });
            return Ok(FIELD.capture(&body, node_text(field, text)).map(|ty| {
                let mut site = r.clone();
                site.file_path = owner.file_path.clone();
                site.line = owner.start_line;
                IterationHit { ty: ty.to_string(), site }
            }));
        }
        if collection.kind() != "identifier" {
            return Ok(None);
        }
        let name = node_text(collection, text);
        let bindings = self.bindings(&r.file_path)?;
        let Some(binding) = innermost_binding(&bindings, name, Some(r.line)).cloned() else {
            return Ok(None);
        };
        let declaration = self
            .read_file(&r.file_path)
            .and_then(|ls| ls.get((binding.line - 1).max(0) as usize).cloned())
            .unwrap_or_default();
        // `\bNAME\s*(?::=|=)?\s*(?:\[\d*\]|map\[[^\]]+\])\s*\*?([\w.]+)` — the
        // first slice/array binding is the index; map keys need their own type.
        static DECLARED: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new(
                "",
                r"\s*(?::=|=)?\s*(?:\[[0-9]*\]|map\[[^\]]+\])\s*\*?([A-Za-z0-9_.]+)",
                true,
                false,
                false,
            )
        });
        if let Some(ty) = DECLARED.capture(&declaration, name) {
            return Ok(Some(IterationHit { ty: ty.to_string(), site: r.clone() }));
        }
        // `\bNAME(?:\s*,\s*\w+)*\s*:=\s*([\w.]+)\s*\(`
        static FACTORY: LazyLock<Affix> = LazyLock::new(|| {
            Affix::new("", r"(?:\s*,\s*[A-Za-z0-9_]+)*\s*:=\s*([A-Za-z0-9_.]+)\s*\(", true, false, false)
        });
        let Some(factory) = FACTORY.capture(&declaration, name).map(str::to_string) else {
            return Ok(None);
        };
        // resolveCall: matchBoundReceiverCall on the factory name at the
        // collection's declaration — undefined for a non-receiver shape.
        let mut call_site = r.clone();
        call_site.line = binding.line;
        call_site.reference_name = factory;
        if !is_binding_receiver_call(&call_site) {
            return Ok(None);
        }
        let Some(callee) = self.bound_receiver_claim(&call_site)?.map(|c| c.node) else {
            return Ok(None);
        };
        let element = callee
            .signature
            .as_deref()
            .and_then(|s| re!(r"\)\s*\(?\s*\[\]\s*\*?([A-Za-z0-9_.]+)").captures(s).map(|c| c[1].to_string()));
        Ok(element.map(|ty| {
            let mut site = r.clone();
            site.file_path = callee.file_path.clone();
            site.line = callee.start_line;
            IterationHit { ty, site }
        }))
    }
}

impl KernelResolver {
    /// inferGuardedReceiver — the `T` of the nearest enclosing
    /// `if ($receiver instanceof T)` whose body holds the call, unless the
    /// receiver is redeclared or reassigned inside it first.
    pub(super) fn infer_guarded_receiver(&mut self, receiver: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if r.language != "php" {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let text = lines.text().to_string();
        if !text.contains("instanceof") {
            return Ok(None);
        }
        let Ok(tree) = crate::tree::parse_with_cached_parser(&text, &r.language) else {
            return Ok(None);
        };
        let at = ((r.line - 1).max(0) as usize, r.column.max(0) as usize);
        let call = descendant_for_position(tree.root_node(), &text, at);
        let mut cur = Some(call);
        while let Some(node) = cur {
            cur = node.parent();
            if matches!(
                node.kind(),
                "anonymous_function"
                    | "anonymous_function_creation_expression"
                    | "arrow_function"
                    | "function_definition"
                    | "method_declaration"
            ) {
                return Ok(None);
            }
            if node.kind() != "if_statement" {
                continue;
            }
            let Some(body) = node.child_by_field_name("body") else { continue };
            if call.start_byte() < body.start_byte() || call.end_byte() > body.end_byte() {
                continue;
            }
            let Some(condition) = node.child_by_field_name("condition") else { continue };
            let Some(m) = re!(r"^\(\s*\$([A-Za-z0-9_]+)\s+instanceof\s+([A-Za-z0-9_\\]+)\s*\)$")
                .captures(node_text(condition, &text))
            else {
                continue;
            };
            if &m[1] != receiver {
                continue;
            }
            let if_line = node.start_position().row as i64 + 1;
            let shadow = self.bindings(&r.file_path)?.iter().any(|b| {
                b.name == receiver && b.scope_start > if_line && b.scope_start <= r.line && b.scope_end >= r.line
            });
            let mut assignments = Vec::new();
            descendants_of_type(body, "assignment_expression", &mut assignments);
            let var = format!("${receiver}");
            let assigned = assignments.iter().any(|a| {
                a.start_byte() < call.start_byte()
                    && a.child_by_field_name("left").is_some_and(|l| node_text(l, &text) == var)
            });
            return Ok((!shadow && !assigned).then(|| m[2].to_string()));
        }
        Ok(None)
    }
}
