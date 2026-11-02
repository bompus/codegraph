//! A Python name bound by the expression around a reference: a lambda
//! parameter or a comprehension variable. Binding rows are scoped to whole
//! lines, so these single-expression scopes are read from the syntax tree.

use super::iteration::{descendant_for_position, named_children};
use super::*;

/// Every identifier `node` names as a binding target, tuples and stars included.
fn binds(node: tree_sitter::Node, text: &str, name: &str) -> bool {
    match node.kind() {
        "identifier" => &text[node.start_byte()..node.end_byte()] == name,
        "attribute" | "subscript" => false,
        _ => named_children(node).into_iter().any(|c| binds(c, text, name)),
    }
}

impl KernelResolver {
    /// Whether the lambda or comprehension the reference sits in binds `name`
    /// there: `lambda other: other.f()`, `[other.f() for other in items]`.
    /// A comprehension's first iterable is evaluated outside its scope.
    pub(super) fn python_expression_binds(&mut self, name: &str, r: &ResolveRefIn) -> bool {
        let Some(file) = self.read_file(&r.file_path) else { return false };
        let text = file.text();
        let Some(tree) = self.parsed_tree(&file, r) else { return false };
        let leaf = descendant_for_position(
            tree.root_node(),
            text,
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        );
        let at = leaf.start_byte();
        let inside = |n: tree_sitter::Node| n.start_byte() <= at && at < n.end_byte();
        let mut current = Some(leaf);
        while let Some(node) = current {
            current = node.parent();
            match node.kind() {
                "lambda" => {
                    let in_body = node.child_by_field_name("body").is_some_and(inside);
                    let named = node.child_by_field_name("parameters").is_some_and(|params| {
                        named_children(params).into_iter().any(|param| {
                            let target = param
                                .child_by_field_name("name")
                                .or_else(|| {
                                    matches!(param.kind(), "list_splat_pattern" | "dictionary_splat_pattern")
                                        .then(|| param.named_child(0))
                                        .flatten()
                                })
                                .unwrap_or(param);
                            binds(target, text, name)
                        })
                    });
                    if in_body && named {
                        return true;
                    }
                }
                "list_comprehension" | "set_comprehension" | "dictionary_comprehension" | "generator_expression" => {
                    let clauses: Vec<_> = named_children(node)
                        .into_iter()
                        .filter(|c| c.kind() == "for_in_clause")
                        .collect();
                    let in_first_iterable = clauses
                        .first()
                        .and_then(|c| c.child_by_field_name("right"))
                        .is_some_and(inside);
                    if !in_first_iterable
                        && clauses
                            .iter()
                            .filter_map(|c| c.child_by_field_name("left"))
                            .any(|target| binds(target, text, name))
                    {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
}
