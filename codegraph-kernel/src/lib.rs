//! codegraph-kernel — native extraction kernel (napi-rs).
//!
//! Replaces ONLY the parse+extract walk inside the parse workers, behind the
//! existing `ExtractionResult` contract. Input `(filePath, content, language)`
//! per file; output flat typed buffers — one boundary crossing per file.
//! Everything downstream (resolution, synthesis, frameworks, MCP) is
//! untouched and consumes the decoded result exactly as before.
//!
//! Calls are synchronous by design: the existing `ParseWorkerPool` workers
//! already parallelize per-file, so each worker thread drives its own kernel
//! call (do NOT rebuild the pool on the Rust side — see the migration plan §3).
//!
//! Per-language extraction lives in a dedicated walker module (tsjs/ for
//! typescript/tsx/javascript/jsx) that mirrors the TS extractor for behavioral
//! parity — verified by scripts/kernel-parity.mjs and the §5 gate.

#![deny(clippy::all)]

/// First statement of every recursive walker function (see stack.rs, #1581):
/// once the stack pointer is inside the red zone, stop descending — the
/// latched flag makes `stack::run_guarded` discard the walk and return a
/// `defer:` error (the TS side then serves the file with the generic
/// extractor). `Default::default()` covers every walker return type in use
/// (`()`, `bool`, `Option<_>`, `String`); a hook returning `false` just sends
/// its caller down the generic child walk, whose own guard returns at once.
macro_rules! stack_guard {
    () => {
        if $crate::stack::exhausted() {
            return ::core::default::Default::default();
        }
    };
}

/// The position helpers every walker shares: source text of a node, its
/// 1-based line, its UTF-16 start/end columns (via the walker's `cols`), and
/// the row of the innermost scope (0 = the file node). Expects `src: &'t str`,
/// `cols: textutil::Cols` and `stack: Vec<Scope>` with a `row: u32`.
macro_rules! walker_pos_impl {
    () => {
        fn text(&self, node: Node) -> &'t str {
            &self.src[node.byte_range()]
        }
        fn line_of(&self, node: Node) -> u32 {
            node.start_position().row as u32 + 1
        }
        fn col_of(&self, node: Node) -> u32 {
            self.cols.col(self.src, node.start_position().row, node.start_byte())
        }
        fn end_col_of(&self, node: Node) -> u32 {
            self.cols.col(self.src, node.end_position().row, node.end_byte())
        }
        fn top_row(&self) -> u32 {
            self.stack.last().map(|s| s.row).unwrap_or(0)
        }
    };
}

/// isInsideClassLikeNode — the innermost scope only (the file never
/// counts), over the walker's class-like kinds (C/C++ and Rust add `union`).
macro_rules! inside_class_like_impl {
    ($($kind:literal)|+) => {
        fn inside_class_like(&self) -> bool {
            self.stack.last().map(|s| matches!(s.kind, $($kind)|+)).unwrap_or(false)
        }
    };
}

/// Emits the function-reference candidates held during the walk, now that
/// the file's defined and imported names are known: a candidate survives
/// when it is `this.`-rooted, `::`-qualified, or names a function this file
/// defines or imports; one ref per (owner node id, name). Expects
/// `fn_ref_cands: Vec<walker::Cand>`, `defined_fn_names`, `imported_names`,
/// `node_ids`, `file_path`, `cols`, `src`, `arena`, `tables`. An `ungated`
/// candidate skips the defined/imported check.
macro_rules! flush_fn_ref_candidates_impl {
    () => {
        fn flush_fn_ref_candidates(&mut self) {
            let cands = std::mem::take(&mut self.fn_ref_cands);
            if cands.is_empty() || $crate::textutil::is_generated_file(self.file_path) {
                return;
            }
            // The TS side keys on `${fromNodeId}|${name}`; ids can collide
            // across rows, so the key is the id string, not the row.
            let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
            for c in cands {
                if !c.name.starts_with("this.")
                    && !c.name.contains("::")
                    && !c.ungated
                    && !self.defined_fn_names.contains(&c.name)
                    && !self.imported_names.contains(&c.name)
                {
                    continue;
                }
                if !seen.insert((self.node_ids[c.from as usize].clone(), c.name.clone())) {
                    continue;
                }
                let column = self.cols.col(self.src, c.row, c.column_byte);
                let name_ref = self.arena.put(&c.name);
                self.tables.push_ref(&$crate::buffers::RefRow {
                    from_idx: c.from,
                    kind: $crate::buffers::FUNCTION_REF_CODE,
                    line: c.line,
                    column,
                    reference_name: name_ref,
                    candidates: $crate::buffers::NONE_STR,
                    from_id_str: $crate::buffers::NONE_STR,
                });
            }
        }
    };
}

/// One binding row from the walker's arena and tables; an `exported` row is
/// `EXPORT_PUBLIC` under its own name.
macro_rules! push_binding_row_impl {
    () => {
        #[allow(clippy::too_many_arguments)]
        fn push_binding_row(
            &mut self,
            kind: u8,
            name: &str,
            node_idx: u32,
            scope: (u32, u32),
            line: u32,
            target: Option<(&str, &str)>,
            exported: bool,
            storage: Option<&str>,
        ) {
            use $crate::buffers::{BindingRow, EXPORT_NONE, EXPORT_PUBLIC, NONE_STR};
            let name_ref = self.arena.put(name);
            let (target_spec, target_name) = match target {
                Some((spec, imported)) => (self.arena.put(spec), self.arena.put(imported)),
                None => (NONE_STR, NONE_STR),
            };
            let storage_ref = self.arena.put_opt(storage);
            self.tables.push_binding(&BindingRow {
                kind,
                export_form: if exported { EXPORT_PUBLIC } else { EXPORT_NONE },
                node_idx,
                scope_start: scope.0,
                scope_end: scope.1,
                name: name_ref,
                target_spec,
                target_name,
                exported_as: if exported { name_ref } else { NONE_STR },
                storage: storage_ref,
                line,
            });
        }
    };
}

/// extractName for the walkers whose grammars carry a `name` field (or a first identifier child) — identical in seven languages.
macro_rules! extract_name_impl {
    () => {
        /// extractName (tree-sitter.ts:90) — the C#-reachable paths: the `name`
        /// field (always present on named declarations), else the shared
        /// identifier scan, else `<anonymous>`.
        fn extract_name(&self, node: Node) -> String {
            if let Some(name_node) = node.child_by_field_name("name") {
                return self.text(name_node).to_string();
            }
            for c in $crate::walker::named_kids(node) {
                if matches!(c.kind(), "identifier" | "type_identifier" | "simple_identifier" | "constant") {
                    return self.text(c).to_string();
                }
            }
            "<anonymous>".to_string()
        }
    };
}

/// extractDecoratorsFor + considerDecorator for the annotation-modifier grammars (Java, Kotlin, Swift).
macro_rules! decorators_impl {
    () => {
        /// extractDecoratorsFor — Java annotations live inside `modifiers`.
        fn extract_decorators_for(&mut self, decl: Node<'t>, decorated_row: u32) {
            for i in 0..decl.named_child_count() {
                let Some(child) = decl.named_child(i) else { continue };
                self.consider_decorator(child, decorated_row);
                if child.kind() == "modifiers" {
                    for m in $crate::walker::named_kids(child) {
                        self.consider_decorator(m, decorated_row);
                    }
                }
            }
            // Preceding-sibling scan (TS-style class decorators) — Java annotations
            // are inside modifiers, so this is inert here; kept for parity of shape.
            let Some(parent) = decl.parent() else { return };
            let decl_start = decl.start_byte();
            let mut decl_idx: isize = -1;
            for i in 0..parent.named_child_count() {
                if let Some(sib) = parent.named_child(i) {
                    if sib.start_byte() == decl_start {
                        decl_idx = i as isize;
                        break;
                    }
                }
            }
            if decl_idx > 0 {
                let mut j = decl_idx - 1;
                while j >= 0 {
                    let Some(sib) = parent.named_child(j as usize) else {
                        j -= 1;
                        continue;
                    };
                    if !matches!(sib.kind(), "decorator" | "annotation" | "marker_annotation") {
                        break;
                    }
                    self.consider_decorator(sib, decorated_row);
                    j -= 1;
                }
            }
        }
        fn consider_decorator(&mut self, n: Node<'t>, decorated_row: u32) {
            if !matches!(n.kind(), "decorator" | "annotation" | "marker_annotation" | "attribute") {
                return;
            }
            let mut target: Option<Node> = None;
            for i in 0..n.named_child_count() {
                let Some(child) = n.named_child(i) else { continue };
                if child.kind() == "call_expression" {
                    target = child.child_by_field_name("function").or_else(|| child.named_child(0));
                    if target.is_some() {
                        break;
                    }
                }
                if matches!(
                    child.kind(),
                    "identifier" | "member_expression" | "scoped_identifier" | "navigation_expression"
                        | "user_type" | "type_identifier"
                ) {
                    target = Some(child);
                    break;
                }
            }
            let Some(target) = target else { return };
            let name = strip_generic_and_qualifier(self.text(target));
            if name.is_empty() {
                return;
            }
            self.push_ref_at(decorated_row, &name, $crate::buffers::EDGE_DECORATES, n);
        }
    };
}

/// extractTypeRefsFromSubtree for the grammars whose type nodes are `type_identifier`s (Go, Java, Rust, Swift).
macro_rules! type_refs_from_subtree_impl {
    () => {
        fn extract_type_refs_from_subtree(&mut self, node: Node<'t>, from_row: u32) {
            stack_guard!();
            if node.kind() == "type_identifier" {
                let type_name = self.text(node).to_string();
                if !type_name.is_empty() && !is_builtin_type(&type_name) {
                    self.push_ref_at(from_row, &type_name, $crate::buffers::EDGE_REFERENCES, node);
                }
                return;
            }
            for c in $crate::walker::named_kids(node) {
                self.extract_type_refs_from_subtree(c, from_row);
            }
        }
    };
}

/// A reference row, plus the import feed: an `imports` ref's simple or qualified-import name joins `imported_names` (the fn-ref flush gate). `push_ref_at` positions it at a node.
macro_rules! push_ref_impl {
    () => {
        fn push_ref(&mut self, from_row: u32, name: &str, kind_code: u8, line: u32, column: u32) {
            let name_ref = self.arena.put(name);
            self.tables.push_ref(&RefRow {
                from_idx: from_row,
                kind: kind_code,
                line,
                column,
                reference_name: name_ref,
                candidates: NONE_STR,
                from_id_str: NONE_STR,
            });
            if kind_code == $crate::buffers::EDGE_IMPORTS {
                if util::simple_name().is_match(name) {
                    self.imported_names.insert(name.to_string());
                } else if let Some(c) = util::qualified_import().captures(name) {
                    self.imported_names.insert(c[1].to_string());
                }
            }
        }
        fn push_ref_at(&mut self, from_row: u32, name: &str, kind_code: u8, node: Node) {
            self.push_ref(from_row, name, kind_code, self.line_of(node), self.col_of(node));
        }
    };
}

/// The enclosing node's lines, or None at file level. The listed scope kinds
/// count as file level (Java/Kotlin/PHP add the package `namespace` node,
/// which is qualified-name scaffolding, not a scope).
macro_rules! enclosing_scope_impl {
    ($($kind:literal)|+) => {
        fn enclosing_scope(&self) -> Option<(u32, u32)> {
            let top = self.stack.last()?;
            if matches!(top.kind, $($kind)|+) {
                return None;
            }
            Some(self.tables.node_lines(top.row))
        }
    };
}

/// The markdown path-reference pair, for a walker with the usual shape
/// (`text`, `line_of`, `col_of`, `arena`, `tables`, `file_path`). Every routed
/// language needs the same two methods, and the wasm arm they must match is
/// one implementation, so this is one implementation too — see markdown.rs for
/// what it mirrors and why the two ref flags are set only here.
macro_rules! markdown_refs_impl {
    () => {
        /// extractMarkdownPathReferencesFromStringNode.
        fn markdown_refs_from_string(&mut self, node: Node<'t>, owner_row: u32) {
            if !$crate::markdown::is_markdown_path_string_kind(node.kind()) {
                return;
            }
            let found = $crate::markdown::markdown_path_refs(self.text(node), self.file_path);
            if found.is_empty() {
                return;
            }
            let kind_code = $crate::buffers::EDGE_REFERENCES;
            let line = self.line_of(node);
            let column = self.col_of(node);
            for (name, offset) in found {
                let column = column + offset as u32;
                // Declaration scans and the general walk can reach the same string.
                if !self.md_ref_keys.insert(format!("{owner_row}|{name}|{line}|{column}")) {
                    continue;
                }
                let name_ref = self.arena.put(&name);
                // addReference denormalizes filePath and language onto the ref
                // where the ordinary ref path does not, so these two flags are
                // set here and nowhere else.
                self.tables.push_ref_flagged(
                    &$crate::buffers::RefRow {
                        from_idx: owner_row,
                        kind: kind_code,
                        line,
                        column,
                        reference_name: name_ref,
                        candidates: $crate::buffers::NONE_STR,
                        from_id_str: $crate::buffers::NONE_STR,
                    },
                    $crate::buffers::REF_FLAG_FILE_PATH | $crate::buffers::REF_FLAG_LANGUAGE,
                );
            }
        }

        /// extractMarkdownPathReferencesFromSubtree — for constructs whose walk
        /// stops before their value, where the owner is the declared symbol
        /// rather than the enclosing scope.
        ///
        /// Only some walkers stop that way (tsjs, python, java, csharp, ruby,
        /// lua); in the rest a declaration's children are walked normally and
        /// the string method above already covers them, so the pair is one
        /// macro and this half goes unused there. The parity fixtures decide
        /// which is which — every language's torture file carries the same
        /// eight markdown shapes.
        #[allow(dead_code)]
        fn markdown_refs_from_subtree(&mut self, node: Node<'t>, owner_row: u32) {
            stack_guard!();
            self.markdown_refs_from_string(node, owner_row);
            for c in $crate::walker::named_kids(node) {
                self.markdown_refs_from_subtree(c, owner_row);
            }
        }
    };
}

mod buffers;
mod ccpp;
mod cfnptr;
mod csharp;
mod dart;
mod docstring;
mod ids;
mod go;
mod java;
mod kotlin;
mod langs;
mod lua;
mod markdown;
mod php;
mod resolve;
mod rlang;
mod ruby;
mod rustlang;
mod scala;
mod stack;
mod swift;
mod textutil;
mod tree;
mod walker;
mod python;
mod tsjs;

use napi::bindgen_prelude::*;
use napi_derive::napi;

/// The six flat tables for one file. See buffers.rs for the byte layout;
/// `src/extraction/kernel/layout.ts` is the TS mirror.
#[napi(object)]
pub struct ExtractBuffers {
    pub meta: Buffer,
    pub nodes: Buffer,
    pub edges: Buffer,
    pub refs: Buffer,
    /// v3: per-file bindings (docs/design/resolution-binding-model-plan.md).
    pub bindings: Buffer,
    pub arena: Buffer,
}

/// Wire-contract description — the TS loader verifies this against
/// src/types.ts before routing anything to the kernel, so an out-of-date
/// `.node` degrades to the wasm path instead of mis-decoding.
#[napi(object)]
pub struct ContractInfo {
    pub abi_version: u32,
    pub kernel_version: String,
    pub node_kinds: Vec<String>,
    pub edge_kinds: Vec<String>,
    /// Languages this binary can extract (routing is still TS-side policy).
    pub languages: Vec<String>,
}

/// Grammar identity for the grammar-source-parity gate: the wasm grammar and
/// the native grammar must expose identical node-kind/field tables, or
/// kernel-vs-fallback routing would be non-deterministic.
#[napi(object)]
pub struct GrammarInfo {
    pub abi_version: u32,
    pub node_kind_count: u32,
    pub field_count: u32,
    pub node_kinds: Vec<String>,
    pub field_names: Vec<String>,
}

#[napi]
pub fn contract_info() -> ContractInfo {
    ContractInfo {
        abi_version: buffers::KERNEL_ABI_VERSION as u32,
        kernel_version: env!("CARGO_PKG_VERSION").to_string(),
        node_kinds: buffers::NODE_KINDS.iter().map(|s| s.to_string()).collect(),
        edge_kinds: buffers::EDGE_KINDS.iter().map(|s| s.to_string()).collect(),
        languages: langs::LANGUAGES.iter().map(|s| s.to_string()).collect(),
    }
}

#[napi]
pub fn grammar_info(language: String) -> Option<GrammarInfo> {
    let lang = langs::grammar_for(&language)?;
    let node_kind_count = lang.node_kind_count();
    let field_count = lang.field_count();
    let node_kinds = (0..node_kind_count)
        .map(|i| lang.node_kind_for_id(i as u16).unwrap_or("").to_string())
        .collect();
    // Field ids are 1-based in tree-sitter.
    let field_names = (1..=field_count)
        .map(|i| lang.field_name_for_id(i as u16).unwrap_or("").to_string())
        .collect();
    Some(GrammarInfo {
        abi_version: lang.abi_version() as u32,
        node_kind_count: node_kind_count as u32,
        field_count: field_count as u32,
        node_kinds,
        field_names,
    })
}

/// The per-language walk, under the stack guard (stack.rs, #1581): a file
/// nested deeply enough to overflow this thread's stack comes back as a
/// `defer:` error — the TS side's routine "serve this one file another way"
/// signal — instead of a SIGSEGV that kills the entire indexer process. A
/// walker panic is caught here too and returned as an error, which the TS
/// side answers the same way (generic extractor for that file); the crate
/// must therefore never build with `panic = "abort"`.
fn walk_file(file_path: &str, content: &str, language: &str) -> std::result::Result<buffers::EmitOut, String> {
    stack::run_guarded(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch_walker(file_path, content, language)))
            .unwrap_or_else(|panic| {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Err(format!("kernel walker panicked on {file_path}: {msg}"))
            })
    })
}

fn dispatch_walker(file_path: &str, content: &str, language: &str) -> std::result::Result<buffers::EmitOut, String> {
    match language {
        "java" => java::extract(file_path, content),
        "python" => python::extract(file_path, content),
        "go" => go::extract(file_path, content),
        "c" | "cpp" => ccpp::extract(file_path, content, language),
        "rust" => rustlang::extract(file_path, content),
        "csharp" => csharp::extract(file_path, content),
        "ruby" => ruby::extract(file_path, content),
        "php" => php::extract(file_path, content),
        "swift" => swift::extract(file_path, content),
        "kotlin" => kotlin::extract(file_path, content),
        "r" => rlang::extract(file_path, content),
        "lua" | "luau" => lua::extract(file_path, content, language),
        "scala" => scala::extract(file_path, content),
        "dart" => dart::extract(file_path, content),
        // ArkTS rides the TypeScript walker over its own grammar (bindings only).
        "typescript" | "tsx" | "javascript" | "jsx" | "arkts" => tsjs::extract(file_path, content, language),
        _ => Err(format!("no native walker for {language}")),
    }
}

fn to_buffers(out: buffers::EmitOut) -> ExtractBuffers {
    ExtractBuffers {
        meta: out.meta.into(),
        nodes: out.nodes.into(),
        edges: out.edges.into(),
        refs: out.refs.into(),
        bindings: out.bindings.into(),
        arena: out.arena.into(),
    }
}

/// Binding rows only, for a file the generic extractor extracts (a
/// stack-guard defer, ArkTS, the kernel kill switch —
/// resolution-binding-model-plan.md §2.4). The same walk as `extract_file`
/// produces them, so the two paths can never disagree on a row; nodes, edges
/// and refs are dropped and the rows come back nodeless for the TS side to
/// attach by name and line. A file too deep for the walk yields `defer:` and
/// no rows, like its nodes.
#[napi]
pub fn bindings_file(file_path: String, content: String, language: String) -> Result<ExtractBuffers> {
    let out = walk_file(&file_path, &content, &language).map_err(Error::from_reason)?;
    Ok(to_buffers(out.bindings_only()))
}

#[napi]
pub fn extract_file(file_path: String, content: String, language: String) -> Result<ExtractBuffers> {
    let out = walk_file(&file_path, &content, &language).map_err(Error::from_reason)?;
    Ok(to_buffers(out))
}
