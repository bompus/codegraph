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
