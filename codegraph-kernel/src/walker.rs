//! State types every language walker shares.

use tree_sitter::Node;

/// One scope-stack entry: the row of the node that opened it (0 = the file
/// node), its kind, and its name for qualified-name building.
pub(crate) struct Scope {
    pub row: u32,
    pub kind: &'static str,
    pub name: String,
}

/// A function or method body scanned for value references once the walk
/// ends (flush_value_refs): the owner row, the body node, the owner's name.
pub(crate) struct ValueScope<'t> {
    pub row: u32,
    pub node: Node<'t>,
    pub name: String,
}

/// A function-reference candidate held until the walk ends, when the file's
/// defined and imported names are known (flush_fn_ref_candidates). C/C++
/// carries extra per-candidate modes and keeps its own type.
pub(crate) struct Cand {
    pub from: u32,
    pub name: String,
    pub line: u32,
    pub column_byte: usize,
    pub row: usize,
    /// Exempt from the defined/imported gate (PHP's HOF-position string
    /// callables).
    pub ungated: bool,
}

impl Cand {
    /// A gated candidate at `node`'s start, or None for an empty or
    /// stoplisted name (never a function reference).
    pub fn at(from: u32, name: impl Into<String>, node: Node) -> Option<Cand> {
        let name = name.into();
        if name.is_empty() || crate::textutil::is_stoplisted(&name) {
            return None;
        }
        let p = node.start_position();
        Some(Cand { from, name, line: p.row as u32 + 1, column_byte: node.start_byte(), row: p.row, ungated: false })
    }
}

/// `name` qualified by the enclosing non-file scopes, `::`-joined
/// (createNode's default qualifiedName).
pub(crate) fn scope_qualified_name(stack: &[Scope], name: &str) -> String {
    let parts: Vec<&str> = stack.iter().filter(|s| s.kind != "file").map(|s| s.name.as_str()).collect();
    let mut qn = parts.join("::");
    if !qn.is_empty() {
        qn.push_str("::");
    }
    qn.push_str(name);
    qn
}

/// `node`'s children in order — `child(0..child_count())`. `child(i)` scans
/// from the first child, so a wide node is walked with one cursor instead;
/// a narrow one (most of them) keeps the lookups and skips the cursor's
/// allocation.
pub(crate) fn kids<'t>(node: Node<'t>) -> Kids<'t> {
    Kids { node, index: 0, count: node.child_count(), cursor: None }
}

/// Child count from which `kids` switches to a cursor.
const CURSOR_MIN_CHILDREN: usize = 16;

pub(crate) struct Kids<'t> {
    node: Node<'t>,
    index: usize,
    count: usize,
    cursor: Option<tree_sitter::TreeCursor<'t>>,
}

impl<'t> Iterator for Kids<'t> {
    type Item = Node<'t>;

    fn next(&mut self) -> Option<Node<'t>> {
        if self.index >= self.count {
            return None;
        }
        let i = self.index;
        self.index += 1;
        if self.count < CURSOR_MIN_CHILDREN {
            return self.node.child(i);
        }
        let node = self.node;
        let cursor = self.cursor.get_or_insert_with(|| node.walk());
        let moved = if i == 0 { cursor.goto_first_child() } else { cursor.goto_next_sibling() };
        moved.then(|| cursor.node())
    }
}

/// `node`'s named children in order — `named_child(0..named_child_count())`.
pub(crate) fn named_kids<'t>(node: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    kids(node).filter(|c| c.is_named())
}

/// The node cap on every value-reference DFS (shadow prune and emission),
/// matching the TS side's MAX_VALUE_REF_NODES.
pub(crate) const MAX_VALUE_REF_NODES: usize = 20_000;

/// The emission half of flushValueRefs: for each captured scope, a
/// `references` edge (metadata `{"valueRef":true}`) to every file-scope
/// value target an identifier in its body names — once per target per
/// scope, never to itself or to a same-named target. `targets` is the
/// shadow-pruned name → row map; `node_ids` compares by id string, as the TS
/// side does (ids collide).
pub(crate) fn emit_value_refs(
    src: &str,
    node_ids: &[String],
    arena: &mut crate::buffers::Arena,
    tables: &mut crate::buffers::Tables,
    scopes: &[ValueScope],
    targets: &std::collections::HashMap<String, u32>,
) {
    use crate::buffers::{EdgeRow, NONE, NONE_STR};
    let refs_kind = crate::buffers::EDGE_REFERENCES;
    // One arena string for every value-ref edge of the file (unchanged when none).
    let mut value_ref_meta = None;
    for scope in scopes {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut stack: Vec<Node> = vec![scope.node];
        let mut visited = 0usize;
        while let Some(n) = stack.pop() {
            if visited >= MAX_VALUE_REF_NODES {
                break;
            }
            visited += 1;
            if matches!(n.kind(), "identifier" | "constant" | "name" | "simple_identifier") {
                let ref_name = &src[n.byte_range()];
                if let Some(&target_row) = targets.get(ref_name) {
                    let target_id = node_ids[target_row as usize].as_str();
                    if target_id != node_ids[scope.row as usize] && ref_name != scope.name && seen.insert(target_id) {
                        let meta = *value_ref_meta.get_or_insert_with(|| arena.put(r#"{"valueRef":true}"#));
                        tables.push_edge(&EdgeRow {
                            source_idx: scope.row,
                            target_idx: target_row,
                            kind: refs_kind,
                            provenance: 0,
                            line: NONE,
                            column: NONE,
                            metadata_json: meta,
                            source_id_str: NONE_STR,
                            target_id_str: NONE_STR,
                        });
                    }
                }
            }
            for c in named_kids(n) {
                stack.push(c);
            }
        }
    }
}
