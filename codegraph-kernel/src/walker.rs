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
/// defined and imported names are known (flush_fn_ref_candidates). PHP and
/// C/C++ carry extra per-candidate flags and keep their own type.
pub(crate) struct Cand {
    pub from: u32,
    pub name: String,
    pub line: u32,
    pub column_byte: usize,
    pub row: usize,
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
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i) {
                    stack.push(c);
                }
            }
        }
    }
}
