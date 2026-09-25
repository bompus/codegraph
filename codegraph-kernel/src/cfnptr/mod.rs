//! Native port of the cFnPtr synthesizer's EXTRACTION SWEEP (task #5 step 2,
//! plan §7a.9): raw file text in → collected per-file facts out. The linking
//! stages, gates, and every registration/dispatch decision stay TS-side; this
//! module only reproduces, bug-for-bug, what the JS sweep computes per file in
//! `src/resolution/c-fnptr-synthesizer.ts`:
//!
//!   • `stripCommentsForRegex(text, 'c')` — the C-style comment/string state
//!     machine (comments blanked to spaces, string interiors skipped, backtick
//!     treated as a multi-line string delimiter — quirks and all);
//!   • the typedef scans (fn-pointer + fn-type forms);
//!   • struct-node field declarations (structural parse; classification stays
//!     TS-side where the complete typedef sets live);
//!   • the survival-filter scans (inline structs, initializers, bare arrays,
//!     alias-shaped object macros, field-assign pairs, dispatch fields, array
//!     dispatch names);
//!   • the raw-text `#include "..."` capture (path resolution stays TS-side —
//!     it needs the filesystem).
//!
//! Parity discipline: the JS side runs these as JavaScript REGEXES, so every
//! scanner here is a hand-rolled byte machine replicating THAT engine's
//! semantics, not idiomatic Rust regex:
//!   • JS `\w`/`\b` are ASCII (non-ASCII chars are non-word) — byte checks
//!     against `[A-Za-z0-9_]` reproduce them exactly, because UTF-8
//!     continuation bytes are non-ASCII and therefore non-word on both sides.
//!   • JS `\s` is the UNICODE whitespace class (NBSP, U+2000-200A, U+FEFF, …)
//!     — `jsws_len` decodes exactly that set from UTF-8.
//!   • Backtracking is reproduced where it is observable (INIT/ARRAY modifier
//!     and `struct` keyword ambiguity, DISPATCH's greedy segment loop,
//!     optional groups) and elided only where analysis shows no input can
//!     distinguish greedy from backtracked (documented per scanner).
//!   • `lastIndex` advancement (resume after each match, +1 on failure) is
//!     reproduced so overlapping-match selection is identical.
//!
//! The stripper blanks per UTF-16 code unit (see `strip_c`), so its output
//! equals the JS stripper's output EXACTLY as a string — every scanner here
//! runs over the identical character stream the JS regexes see, and the strip
//! differential oracle test pins that equality directly. The record-level
//! differential suite (JS sweep vs this sweep over fixtures and whole repos)
//! then pins the scanners themselves.

/// One struct node's extent, as the TS side reads it from the graph.
pub struct StructExtent {
    pub id: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// A structurally-parsed struct field — mirror of the TS `RawFieldDecl`
/// (`name: null` is represented as an empty string; the TS side treats them
/// identically everywhere).
pub struct RawField {
    pub name: String,
    pub index: u32,
    pub ptr: bool,
    pub ty: String,
}

pub struct StructFields {
    pub id: String,
    /// False when the body never parsed (no `{`, unbalanced braces, or a
    /// falsy start line) — the TS side then records nothing for this node,
    /// exactly like the JS sweep.
    pub parsed: bool,
    pub fields: Vec<RawField>,
}

/// Everything the sweep collects for one file.
pub struct FileFacts {
    pub fn_ptr_typedefs: Vec<String>,
    pub fn_type_typedefs: Vec<String>,
    pub structs: Vec<StructFields>,
    pub inline_ptr: bool,
    pub inline_types: Vec<String>,
    pub inline_tags: Vec<String>,
    pub init_tokens: Vec<String>,
    /// `*`-prefixed when the declaration carried the pointer star.
    pub array_elems: Vec<String>,
    pub alias_names: Vec<String>,
    /// `lfield\0rfield`, distinct.
    pub d_pairs: Vec<String>,
    /// Distinct LHS field names of `x->f = fn;` / `(*x)->f = fn;` matches
    /// (bare-function-assignment registration filter; FN_ASSIGN_RE ∪
    /// DEREF_FN_ASSIGN_RE second captures).
    pub assign_fields: Vec<String>,
    pub dispatch_fields: Vec<String>,
    pub array_dispatch_names: Vec<String>,
    /// Raw `#include "…"` captures, in source order, NOT deduplicated —
    /// extension filtering and path resolution happen TS-side.
    pub includes: Vec<String>,
}

mod jsre;
mod strip;
mod scan;
mod env;
mod link;
mod napi;
use self::jsre::*;
use self::scan::*;
use self::env::*;
use self::link::*;
pub use self::strip::strip_c;
pub use self::scan::scan_file;
pub use self::env::file_env;
pub use self::link::{cfnptr_link, LinkArrEntry, LinkField, LinkFile, LinkFn, LinkTables};

/// Fan `f` over `items` on scoped threads (≤16, ≤ items), output 1:1 with
/// input order. Per-item work is independent, so the batch scales with
/// cores instead of riding one worker thread. A chunk thread that panics pads
/// its slots with `pad()` so alignment survives; per-item panics are the
/// caller's to catch when it wants finer padding.
pub(super) fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync, pad: impl Fn() -> R) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16)
        .min(items.len().max(1));
    if threads <= 1 {
        return items.iter().map(&f).collect();
    }
    let chunk_len = items.len().div_ceil(threads);
    let chunks: Vec<&[T]> = items.chunks(chunk_len).collect();
    let mut parts: Vec<Vec<R>> = Vec::with_capacity(chunks.len());
    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| s.spawn(|| chunk.iter().map(&f).collect::<Vec<_>>()))
            .collect();
        for (i, h) in handles.into_iter().enumerate() {
            match h.join() {
                Ok(v) => parts.push(v),
                Err(_) => parts.push(chunks[i].iter().map(|_| pad()).collect()),
            }
        }
    });
    parts.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(src: &str) -> FileFacts {
        scan_file(src, &[])
    }

    #[test]
    fn strip_blanks_comments_keeps_strings() {
        let s = strip_c(b"a /* x\ny */ b // c\nd \"in//str\" e");
        assert_eq!(&s, b"a     \n     b     \nd \"in//str\" e".as_slice());
    }

    #[test]
    fn typedef_forms() {
        let f = facts("typedef void (*hook_fn)(int);\ntypedef void redisCommandProc(int c);\n");
        assert_eq!(f.fn_ptr_typedefs, vec!["hook_fn"]);
        assert_eq!(f.fn_type_typedefs, vec!["redisCommandProc"]);
    }

    #[test]
    fn init_modifier_backtrack() {
        // `static x = {` must match with type token `static` (the JS engine
        // backtracks the modifier loop) — harmless downstream, but collected.
        let f = facts("; static x = {1};\n; static struct cmd t[] = { {0} };");
        assert!(f.init_tokens.contains(&"static".to_string()));
        assert!(f.init_tokens.contains(&"cmd".to_string()));
    }

    #[test]
    fn dispatch_backtracks_segments() {
        let f = facts("int go(struct c *x){ x->cmd->proc(1); tbl[i](2); (*ops[k])(3); }");
        assert!(f.dispatch_fields.contains(&"proc".to_string()));
        assert!(f.array_dispatch_names.contains(&"tbl".to_string()));
        assert!(f.array_dispatch_names.contains(&"ops".to_string()));
    }

    #[test]
    fn field_assign_pairs() {
        let f = facts("void g(void){ a->f = b->g; h.x = k.y; m == n; }");
        assert!(f.d_pairs.contains(&"f\0g".to_string()));
        assert!(f.d_pairs.contains(&"x\0y".to_string()));
        assert_eq!(f.d_pairs.len(), 2);
    }

    #[test]
    fn alias_shapes() {
        let f = facts("#define A redisCommand\n#define B struct foo\n#define C 0x12\n#define D(x) x\n");
        assert!(f.alias_names.contains(&"A".to_string()));
        assert!(f.alias_names.contains(&"B".to_string()));
        assert!(!f.alias_names.contains(&"C".to_string()));
        assert!(!f.alias_names.contains(&"D".to_string()));
    }

    #[test]
    fn includes_from_raw() {
        let f = facts("#include \"commands.def\"\n// #include \"in-comment.h\"\n");
        // Raw-text scan: the commented include IS captured (parity with the
        // JS INCLUDE_RE over raw text).
        assert_eq!(f.includes, vec!["commands.def", "in-comment.h"]);
    }
}
