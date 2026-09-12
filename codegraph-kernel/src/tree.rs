//! Whole-CST serialization — the parse-tree service for read-time consumers
//! (Phase 3 of docs/design/kernel-only-extraction-plan.md).
//!
//! Three TS modules parse a file at query time and walk the tree with the
//! web-tree-sitter node API: viewer highlighting (`syntax-tokens.ts`), branch
//! guards (`graph/branch-guards.ts`, 2,300 lines of per-language rules) and
//! explore's requested-source ranges. Porting those rules to Rust is not
//! worth it; giving them a tree that came from the native parser is. So this
//! module parses once and returns the ENTIRE tree as flat buffers in one
//! boundary crossing — never a per-node handle — and `src/extraction/kernel/
//! tree.ts` wraps the rows in a facade with the same `type` / `childForFieldName`
//! / `text` / `parent` surface the consumers already use.
//!
//! Layout (little-endian; `TREE_ROW_SIZE` bytes per node, preorder):
//!
//! ```text
//!    0  u16  kind id            (index into the kind-name table)
//!    2  u8   flags              (bit0 named, bit1 has_error, bit2 missing)
//!    3  u8   reserved
//!    4  u16  field id           (index into the field-name table; 0 = none)
//!    6  u16  reserved
//!    8  u32  parent index       (NONE for the root)
//!   12  u32  start byte
//!   16  u32  end byte
//!   20  u32  start row
//!   24  u32  start column       (UTF-16 units — what JS sees)
//!   28  u32  end row
//!   32  u32  end column         (UTF-16 units)
//!   36  u32  start index        (UTF-16 units from file start — JS string index)
//!   40  u32  end index          (UTF-16 units)
//!   44  u32  children offset    (into the child-index table)
//!   48  u32  child count
//!   52  u32  named child count
//!   56  u32  index in parent    (position among the parent's children; 0 for root)
//! ```
//!
//! Positions are reported in UTF-16 code units so `source.slice(startIndex,
//! endIndex)` on the JS side is exact, matching what web-tree-sitter reported.
//! Kind and field names are per grammar, not per parse: `tree_names(language)`
//! returns them once (NUL-joined, kinds then fields) and the TS side caches
//! the table per language, so a parse ships only what changed.
//!
//! Fast paths that keep this cheaper than the wasm parser's lazy tree: an
//! all-ASCII file skips the UTF-16 prefix table (byte offsets are code-unit
//! offsets), and the child table is built by a counting pass over the parent
//! array rather than one Vec per node.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use tree_sitter::{Node, Parser};

use crate::langs::grammar_for;
use crate::stack;

pub const TREE_ROW_SIZE: usize = 60;

thread_local! {
    static PARSERS: std::cell::RefCell<std::collections::HashMap<String, Parser>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}
const NONE: u32 = u32::MAX;
const FLAG_NAMED: u8 = 1;
const FLAG_HAS_ERROR: u8 = 2;
const FLAG_MISSING: u8 = 4;

/// Whole-tree buffers for one parse. `src/extraction/kernel/tree.ts` mirrors.
#[napi(object)]
pub struct TreeBuffers {
    /// u32 LE: [abi, node_count, root_has_error]
    pub meta: Buffer,
    pub nodes: Buffer,
    /// u32 LE child indexes, sliced per node by (children offset, child count)
    pub children: Buffer,
}

/// Kind and field names for one grammar (NUL-joined; kinds then fields), with
/// the two counts. Fetched once per language by the TS facade.
#[napi(object)]
pub struct TreeNames {
    pub kind_count: u32,
    pub field_count: u32,
    pub names: Buffer,
}

pub const TREE_ABI_VERSION: u32 = 1;

/// Prefix table: UTF-16 units before each byte offset (len + 1 entries).
fn utf16_prefix(src: &str) -> Vec<u32> {
    let bytes = src.as_bytes();
    let mut out = vec![0u32; bytes.len() + 1];
    let mut units = 0u32;
    let mut i = 0;
    for ch in src.chars() {
        let len = ch.len_utf8();
        for k in 0..len {
            out[i + k] = units;
        }
        units += ch.len_utf16() as u32;
        i += len;
    }
    out[bytes.len()] = units;
    out
}

struct Row {
    kind: u16,
    flags: u8,
    field: u16,
    parent: u32,
    start_byte: u32,
    end_byte: u32,
    start_row: u32,
    start_col: u32,
    end_row: u32,
    end_col: u32,
    start_idx: u32,
    end_idx: u32,
}

fn parse_tree_inner(content: &str, language: &str) -> Result<TreeBuffers> {
    // One parser per language per thread: read-time callers parse many small
    // files in a row, and Parser::new + set_language per call was measurable
    // against a sub-millisecond parse.
    let tree = PARSERS.with(|cell| -> Result<tree_sitter::Tree> {
        let mut map = cell.borrow_mut();
        let parser = match map.entry(language.to_string()) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let grammar = grammar_for(language)
                    .ok_or_else(|| Error::from_reason(format!("no native grammar for {language}")))?;
                let mut parser = Parser::new();
                parser
                    .set_language(&grammar)
                    .map_err(|e| Error::from_reason(format!("set_language({language}) failed: {e}")))?;
                v.insert(parser)
            }
        };
        parser
            .parse(content, None)
            .ok_or_else(|| Error::from_reason("parser returned null tree".to_string()))
    })?;

    // ASCII: byte offsets ARE UTF-16 offsets and tree-sitter's byte columns
    // are code-unit columns. Only a non-ASCII file pays for the prefix table.
    let ascii = content.is_ascii();
    let prefix: Vec<u32> = if ascii { Vec::new() } else { utf16_prefix(content) };
    let line_starts: Vec<usize> = if ascii { Vec::new() } else { crate::textutil::line_starts(content) };
    let idx16 = |byte: usize| -> u32 { if ascii { byte as u32 } else { prefix[byte] } };
    let col16 = |row: usize, col_bytes: usize, byte: usize| -> u32 {
        if ascii {
            return col_bytes as u32;
        }
        let ls = line_starts.get(row).copied().unwrap_or(0);
        if byte <= ls { 0 } else { prefix[byte] - prefix[ls] }
    };

    // Preorder walk with a cursor so field ids are available.
    let mut rows: Vec<Row> = Vec::with_capacity(content.len() / 4 + 16);
    let mut cursor = tree.walk();
    let mut parent_stack: Vec<u32> = Vec::new();
    'walk: loop {
        let node: Node = cursor.node();
        let parent = parent_stack.last().copied().unwrap_or(NONE);
        let mut flags = 0u8;
        if node.is_named() {
            flags |= FLAG_NAMED;
        }
        if node.has_error() {
            flags |= FLAG_HAS_ERROR;
        }
        if node.is_missing() {
            flags |= FLAG_MISSING;
        }
        let sp = node.start_position();
        let ep = node.end_position();
        let idx = rows.len() as u32;
        rows.push(Row {
            kind: node.kind_id(),
            flags,
            field: cursor.field_id().map(|f| f.get()).unwrap_or(0),
            parent,
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            start_row: sp.row as u32,
            start_col: col16(sp.row, sp.column, node.start_byte()),
            end_row: ep.row as u32,
            end_col: col16(ep.row, ep.column, node.end_byte()),
            start_idx: idx16(node.start_byte()),
            end_idx: idx16(node.end_byte()),
        });
        if cursor.goto_first_child() {
            parent_stack.push(idx);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
            parent_stack.pop();
        }
    }
    Ok(finish(rows, tree.root_node().has_error()))
}

fn finish(rows: Vec<Row>, root_has_error: bool) -> TreeBuffers {
    let n = rows.len();
    // Counting pass: children per parent, then prefix sums give each node's
    // slice of the child table; a second pass fills it in preorder (which is
    // source order among siblings).
    let mut counts = vec![0u32; n];
    let mut named = vec![0u32; n];
    for r in &rows {
        if r.parent != NONE {
            counts[r.parent as usize] += 1;
            if r.flags & FLAG_NAMED != 0 {
                named[r.parent as usize] += 1;
            }
        }
    }
    let mut offsets = vec![0u32; n];
    let mut acc = 0u32;
    for i in 0..n {
        offsets[i] = acc;
        acc += counts[i];
    }
    let mut fill = vec![0u32; n];
    let mut child_table = vec![0u32; acc as usize];
    let mut in_parent = vec![0u32; n];
    for (i, r) in rows.iter().enumerate() {
        if r.parent != NONE {
            let p = r.parent as usize;
            let slot = offsets[p] + fill[p];
            child_table[slot as usize] = i as u32;
            in_parent[i] = fill[p];
            fill[p] += 1;
        }
    }

    let mut nodes: Vec<u8> = Vec::with_capacity(n * TREE_ROW_SIZE);
    for (i, r) in rows.iter().enumerate() {
        nodes.extend_from_slice(&r.kind.to_le_bytes());
        nodes.push(r.flags);
        nodes.push(0);
        nodes.extend_from_slice(&r.field.to_le_bytes());
        nodes.extend_from_slice(&0u16.to_le_bytes());
        for v in [
            r.parent,
            r.start_byte,
            r.end_byte,
            r.start_row,
            r.start_col,
            r.end_row,
            r.end_col,
            r.start_idx,
            r.end_idx,
            offsets[i],
            counts[i],
            named[i],
            in_parent[i],
        ] {
            nodes.extend_from_slice(&v.to_le_bytes());
        }
    }
    debug_assert_eq!(nodes.len(), n * TREE_ROW_SIZE);

    let mut children: Vec<u8> = Vec::with_capacity(child_table.len() * 4);
    for c in &child_table {
        children.extend_from_slice(&c.to_le_bytes());
    }

    let mut meta: Vec<u8> = Vec::with_capacity(12);
    for v in [TREE_ABI_VERSION, n as u32, root_has_error as u32] {
        meta.extend_from_slice(&v.to_le_bytes());
    }
    TreeBuffers { meta: meta.into(), nodes: nodes.into(), children: children.into() }
}

/// Kind and field names for `language`'s grammar. Field ids are 1-based in
/// tree-sitter, so slot 0 of the field table is the empty "no field" name.
#[napi]
pub fn tree_names(language: String) -> Option<TreeNames> {
    let grammar = grammar_for(&language)?;
    let kind_count = grammar.node_kind_count() as u32;
    let field_count = grammar.field_count() as u32;
    let mut names = String::new();
    for id in 0..kind_count as u16 {
        names.push_str(grammar.node_kind_for_id(id).unwrap_or(""));
        names.push('\0');
    }
    names.push('\0');
    for id in 1..=field_count as u16 {
        names.push_str(grammar.field_name_for_id(id).unwrap_or(""));
        names.push('\0');
    }
    Some(TreeNames { kind_count, field_count: field_count + 1, names: names.into_bytes().into() })
}

/// Parse `content` with the native grammar for `language` and return the
/// whole tree. Errors (no grammar, deep nesting) surface as napi errors; the
/// TS facade falls back to the wasm parser on any error.
#[napi]
pub fn parse_tree(content: String, language: String) -> Result<TreeBuffers> {
    stack::run_guarded(|| parse_tree_inner(&content, &language).map_err(|e| e.reason.clone()))
        .map_err(Error::from_reason)
}
