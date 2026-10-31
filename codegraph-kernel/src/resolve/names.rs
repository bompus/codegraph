//! Name machinery (name-matcher.ts): visibility, exact and fuzzy matching, best-match scoring.

use super::*;
use super::js_objects_upstream::CollapsedCall;
use super::name_scope::{can_name_in_type_position, is_bare_rust_name};

/// The most a PHP `$this`/`self`/`parent` call settled on a repository trait
/// no repository class uses may claim: a guess, not a proven link.
const PHP_UNSEEN_ANCESTOR_CONFIDENCE: f64 = 0.5;

/// Max same-name candidate pool a generated-file ref will evaluate: real
/// same-name pools sit in the tens; minified bundles push thousands of
/// artifacts into one pool and turn each ref's retain/scope chain
/// quadratic (pretix's fabric.min.js: ~950ms per 250-ref chunk vs a ~90ms
/// floor). Over the cap the name-match declines — unresolved, never a
/// truncated winner.
const GENERATED_NAME_POOL_CAP: usize = 256;

/// A module a bundler loads as data, not code: a stylesheet, an image, a GraphQL document, JSON, a font.
const ASSET_MODULE: &str = r"(?i)\.(?:css|scss|sass|less|styl|stylus|pcss|graphql|gql|png|jpe?g|gif|svg|webp|avif|ico|bmp|tiff?|json5?|ya?ml|toml|md|mdx|txt|html?|xml|csv|woff2?|ttf|otf|eot|mp3|mp4|webm|ogg|wav|flac|pdf|wasm)$";

/// Whether `nodes` are one C++ function's overloads (and declarations): functions all of one
/// qualified name, which a call's arguments choose among (cpp_function_overload).
fn cpp_overload_set(nodes: &[Arc<KNode>]) -> bool {
    nodes.first().is_some_and(|first| nodes.iter().all(|n| n.kind == "function" && n.language == "cpp" && n.qualified_name == first.qualified_name))
}

fn objc_file_local_elsewhere(n: &KNode, r: &ResolveRefIn) -> bool {
    n.language == "objc" && n.file_path != r.file_path
        && matches!(n.kind.as_str(), "struct" | "union" | "enum" | "type_alias" | "variable" | "constant")
        && c_source_ext_re().is_match(&n.file_path)
}

fn prefer_adjacent_implementations(candidates: &mut Vec<Arc<KNode>>, r: &ResolveRefIn) {
    if r.reference_kind != "calls" || !is_js_family(&r.language) { return; }
    // Nothing to demote without a .d.ts candidate — skip the HashSet build
    // and its two String allocations per candidate.
    if !candidates.iter().any(|n| [".d.ts", ".d.mts", ".d.cts"].iter().any(|s| n.file_path.ends_with(s))) {
        return;
    }
    let implementations: HashSet<_> = candidates.iter()
        .filter(|n| !re!(r"\.d\.[cm]?ts$").is_match(&n.file_path))
        .filter_map(|n| n.file_path.rsplit_once('.').map(|(stem, _)| (stem.to_string(), n.qualified_name.clone())))
        .collect();
    candidates.retain(|n| {
        let stem = [".d.ts", ".d.mts", ".d.cts"].iter().find_map(|suffix| n.file_path.strip_suffix(suffix));
        stem.is_none_or(|stem| !implementations.contains(&(stem.to_string(), n.qualified_name.clone())))
    });
}

/// Byte offsets of `name` in `text` where it stands as a whole word: not
/// preceded or followed by a name character (`$` included, as JavaScript
/// allows it in identifiers). The regex crate has no look-around.
pub(super) fn name_offsets<'t>(text: &'t str, name: &'t str) -> impl Iterator<Item = usize> + 't {
    text.match_indices(name).map(|(at, _)| at).filter(move |at| {
        let before = text[..*at].chars().next_back().is_none_or(|c| !is_name_char(c));
        let after = at + name.len();
        let after = text[after..].chars().next().is_none_or(|c| !is_name_char(c));
        before && after
    })
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

impl KernelResolver {
    // -----------------------------------------------------------------------
    // Name machinery (name-matcher.ts)
    // -----------------------------------------------------------------------

    /// Objective-C imports C header globals, even when that header has no Objective-C syntax.
    pub(super) fn objc_header_global_ref(&mut self, n: &KNode, r: &ResolveRefIn) -> bool {
        r.language == "objc" && r.reference_kind == "references"
            && matches!(n.language.as_str(), "c" | "cpp")
            && matches!(n.kind.as_str(), "variable" | "constant") && n.is_exported
            && re!(r"(?i)\.(?:h|hh|hpp|hxx)$").is_match(&n.file_path)
            && self.namespace_visible_files(&r.file_path, "objc").is_ok_and(|files| files.contains(&n.file_path))
    }

    /// applyLanguageGate (name-matcher.ts).
    pub(super) fn apply_language_gate(&mut self, mut candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Vec<Arc<KNode>> {
        // retain in place — one Vec alloc per ref instead of two.
        if r.reference_kind == "references" || r.reference_kind == "function_ref" {
            candidates.retain(|c| same_language_family(&c.language, &r.language) || self.objc_header_global_ref(c, r));
            return candidates;
        }
        if r.reference_kind == "imports" {
            candidates.retain(|c| {
                !crosses_known_family(&c.language, &r.language)
                    && !crosses_code_boundary(&c.language, &r.language)
            });
            return candidates;
        }
        candidates.retain(|c| !crosses_code_boundary(&c.language, &r.language) || self.has_bridge_evidence(c, r));
        candidates
    }

    /// hasBridgeEvidence (name-matcher.ts): a call may cross a language
    /// boundary only through a framework export or the C ABI, scoped to the
    /// named free function (#1986).
    pub(super) fn has_bridge_evidence(&mut self, c: &KNode, r: &ResolveRefIn) -> bool {
        if r.reference_kind != "calls" {
            return false;
        }
        // Expo's extractor creates explicit JS exports resolved by name.
        if code_interop_group(&r.language) == Some("web")
            && c.id.starts_with("expo-module:")
            && c.is_exported
            && (c.language == "swift" || c.language == "kotlin")
        {
            return true;
        }
        if c.kind != "function" {
            return false;
        }
        let name = regex::escape(&c.name);
        let decl = |lines: &[String], from: i64| -> String {
            let lo = (from.max(1) - 1) as usize;
            let hi = (c.end_line.max(0) as usize).min(lines.len());
            lines.get(lo..hi).map(|ls| ls.iter().map(|l| strip_line_comments(l)).collect::<Vec<_>>().join("\n")).unwrap_or_default()
        };
        if code_interop_group(&r.language) == Some("native") {
            let Some(src) = self.read_file(&c.file_path) else { return false };
            if c.language == "go" {
                // `//export NAME` on the line above `func NAME(` in a cgo file.
                let at = (c.start_line - 1) as usize;
                return imports_cgo(&src)
                    && at >= 1
                    && src.get(at - 1).is_some_and(|l| l.trim_end() == format!("//export {}", c.name))
                    && src.get(at).is_some_and(|l| {
                        Self::cached_regex(&format!(r"^func {name}\s*\(")).is_ok_and(|re| re.is_match(l))
                    });
            }
            if c.language == "rust" {
                return Self::cached_regex(&format!(r#"(?-u:\b)pub\s+(?:unsafe\s+)?extern\s+"C"\s+fn\s+{name}(?-u:\b)"#))
                    .is_ok_and(|re| re.is_match(&decl(&src, c.start_line)));
            }
        }
        if c.language == "c" || c.language == "cpp" {
            let Some(src) = self.read_file(&r.file_path) else { return false };
            if r.language == "go" {
                return imports_cgo(&src) && r.reference_name == format!("C.{}", c.name);
            }
            if r.language == "rust" {
                return Self::cached_regex(&format!(r#"extern\s+"C"\s*\{{[^}}]*(?-u:\b)fn\s+{name}\s*\("#))
                    .is_ok_and(|re| re.is_match(&stripped_text(&src)));
            }
        }
        false
    }

    /// isLexicallyReachable (name-matcher.ts): a function — or a type
    /// (`case class B()` in a test method), or a method of such a type —
    /// declared inside a function is reachable only from inside it.
    pub(super) fn is_lexically_reachable(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        // Same reference_name for every candidate in a ref's loop — one-entry
        // memo instead of the thread-local re! per candidate.
        let bare = match &self.bare_name_memo {
            Some((name, b)) if name == &r.reference_name => *b,
            _ => {
                let b = re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name);
                self.bare_name_memo = Some((r.reference_name.clone(), b));
                b
            }
        };
        if candidate.kind == "function" && bare && !self.object_member_reachable_by_name(candidate,r)? { return Ok(false); }
        if bare && js_objects_upstream::path_holder(candidate) && js_objects_upstream::literal_owner(candidate) {
            let last = js_objects_upstream::last_segment(candidate);
            if !last.starts_with("window.") && !last.starts_with("globalThis.") { return Ok(false); }
        }
        if self.js_declaration_reachable(candidate, r) == Some(false) { return Ok(false); }
        if matches!(candidate.kind.as_str(), "variable" | "constant") || (candidate.kind == "field" && candidate.language == "scala") {
            if !self.scala_block_reachable(candidate, r) { return Ok(false); }
            return Ok(self.local_declaration_scope(candidate)?.is_none_or(|(start,end)| r.file_path == candidate.file_path && r.line >= start && r.line <= end));
        }
        if candidate.kind != "function" && candidate.kind != "method" && !is_local_type_kind(&candidate.kind) {
            return Ok(true);
        }
        if no_nested_functions(&candidate.language) {
            return Ok(true);
        }
        Ok(match self.lexical_scope_of(candidate)? {
            None => true,
            Some((start, end)) => r.file_path == candidate.file_path && r.line >= start && r.line <= end,
        })
    }

    /// lexicalScopeOf (name-matcher.ts): the innermost function or method
    /// BODY that scopes a declaration, walking out through its qualified name
    /// (`test_x::Request::User::has_perm` → `test_x`). A method directly on a
    /// class is reachable through its instances, and an object-literal method
    /// a function returns through the object — neither is scoped by the
    /// function it sits in.
    pub(super) fn lexical_scope_of(&mut self, candidate: &KNode) -> Res<Option<(i64, i64)>> {
        if let Some(hit) = self.lexical_scope_memo.get(&candidate.id) {
            return Ok(*hit);
        }
        let own = candidate.qualified_name.as_str();
        let parent_qn = own.rfind("::").map_or("", |cut| &own[..cut]);
        let mut qn = own;
        let mut scope = None;
        while let Some(cut) = qn.rfind("::") {
            qn = &qn[..cut];
            let container = self
                .nodes_by_qualified_name(qn)?
                .iter()
                .find(|p| {
                    p.file_path == candidate.file_path
                        && (p.kind == "function" || p.kind == "method")
                        && p.start_line <= candidate.start_line
                        && p.end_line >= candidate.end_line
                })
                .map(|p| (p.start_line, p.end_line));
            let Some(container) = container else { continue };
            if candidate.kind != "method" || qn != parent_qn {
                scope = Some(container);
            }
            break;
        }
        self.lexical_scope_memo.insert(candidate.id.clone(), scope);
        Ok(scope)
    }

    /// isSealedModule (name-matcher.ts): an ESM file with import statements
    /// and no export of any form offers nothing cross-file.
    pub(super) fn is_sealed_module(&mut self, file_path: &str) -> Res<bool> {
        if let Some(&hit) = self.sealed_memo.get(file_path) {
            return Ok(hit);
        }
        let rows = self.bindings(file_path)?;
        let nodes = self.nodes_in_file(file_path)?;
        let sealed = !rows.is_empty()
            && nodes.iter().any(|n| n.kind == "import")
            && !rows.iter().any(|r| r.exported_as.is_some())
            && !nodes.iter().any(|n| n.is_exported);
        self.sealed_memo.insert(file_path.to_string(), sealed);
        Ok(sealed)
    }

    /// isStaticCFunction (name-matcher.ts): binding row storage === 'static'.
    pub(super) fn is_static_c_function(&mut self, candidate: &KNode) -> Res<bool> {
        if let Some(&hit) = self.c_static_memo.get(&candidate.id) {
            return Ok(hit);
        }
        let rows = self.bindings(&candidate.file_path)?;
        let is_static = rows
            .iter()
            .find(|r| r.node_id.as_deref() == Some(candidate.id.as_str()))
            .is_some_and(|r| r.storage.as_deref() == Some("static"));
        self.c_static_memo.insert(candidate.id.clone(), is_static);
        Ok(is_static)
    }

    /// Whether `includer` has a quoted `#include` that names `included`
    /// relative to its own directory. The two files then share a translation
    /// unit, so a `static` function in one is visible in the other: CPython's
    /// `Modules/clinic/*.c.h` wrappers call `static` `*_impl` functions in the
    /// `.c` file that includes them.
    fn c_includes(&self, includer: &str, included: &str) -> Res<bool> {
        let dir = pos_dirname(includer);
        Ok(self.nodes_in_file(includer)?.iter().any(|n| {
            n.kind == "import"
                && n.signature.as_deref().is_some_and(|s| s.contains('"'))
                && pos_normalize(&format!("{}/{}", dir, n.name)).trim_start_matches('/') == included
        }))
    }

    /// Whether `candidate`'s file includes `fragment` and no other file that
    /// includes it defines a C function of the same name. A fragment shared by
    /// several files names a different function in each: Linux
    /// `fs/minix/itree_common.c` is included by `itree_v1.c` and `itree_v2.c`,
    /// which each define a `static` `block_to_cpu`.
    fn c_sole_includer(&self, candidate: &KNode, fragment: &str) -> Res<bool> {
        if !self.c_includes(&candidate.file_path, fragment)? {
            return Ok(false);
        }
        for n in self.nodes_by_name(&candidate.name)?.iter() {
            if n.file_path != candidate.file_path
                && n.kind == "function"
                && (n.language == "c" || n.language == "cpp")
                && self.c_includes(&n.file_path, fragment)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn cpp_test_file_in_translation_unit(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if !matches!(candidate.language.as_str(), "c" | "cpp") || !matches!(r.language.as_str(), "c" | "cpp") { return Ok(false); }
        let visible = self.namespace_visible_files(&r.file_path, &r.language)?;
        if visible.contains(&candidate.file_path) { return Ok(true); }
        // A test-named header can declare methods implemented in its same-stem
        // source. Including an ordinary header cannot expose its test suite.
        let stem = re!(r"(?i)\.(?:c|cc|cpp|cxx|c\+\+)$").replace(&candidate.file_path, "").to_string();
        if stem == candidate.file_path { return Ok(false); }
        Ok(["h", "hh", "hpp", "hxx"].iter().any(|ext| {
            let header = format!("{stem}.{ext}");
            super::resolver_upstream::test_suite_path(&header) && visible.contains(&header)
        }))
    }

    /// isRustTraitImplMethod (name-matcher.ts) — scan upward for the nearest
    /// `impl` header; `impl Trait for` wins, a top-level item ends the scan.
    pub(super) fn is_rust_trait_impl_method(&mut self, candidate: &KNode) -> Res<bool> {
        if candidate.kind != "method" {
            return Ok(false);
        }
        if let Some(&hit) = self.rust_trait_memo.get(&candidate.id) {
            return Ok(hit);
        }
        let site = ResolveRefIn {
            row_id: None, from_node_id: candidate.id.clone(), reference_name: candidate.name.clone(),
            reference_kind: "calls".to_string(), line: candidate.start_line,
            column: candidate.start_column, candidates: None, file_path: candidate.file_path.clone(),
            language: "rust".to_string(), failure_reason: None, generated: None,
        };
        let is_trait = self.rust_self_context(&site).is_some_and(|cx| cx.trait_name.is_some());
        self.rust_trait_memo.insert(candidate.id.clone(), is_trait);
        Ok(is_trait)
    }

    /// Mark that a Rust typed-receiver path resolved `r` (RUST_TYPED_REFS).
    pub(super) fn mark_rust_typed(&mut self, r: &ResolveRefIn) {
        if let Some(id) = r.row_id {
            self.rust_typed_refs.insert(id);
        }
    }

    /// rustTraitOf (name-matcher.ts): the `trait` node `Owner::method`'s
    /// owner names when it encloses the method's lines, else None.
    pub(super) fn rust_trait_of(&mut self, candidate: &KNode) -> Res<Option<Arc<KNode>>> {
        if let Some(hit) = self.rust_trait_of_memo.get(&candidate.id) {
            return Ok(match hit {
                Some(id) => self.node_by_id(id)?,
                None => None,
            });
        }
        let owner = candidate
            .qualified_name
            .rsplit_once("::")
            .map(|(q, _)| q.rsplit("::").next().unwrap_or(""))
            .unwrap_or("");
        let found = if owner.is_empty() {
            None
        } else {
            self.nodes_in_file(&candidate.file_path)?.iter().find(|n| {
                n.kind == "trait" && n.name == owner
                    && n.start_line <= candidate.start_line
                    && n.end_line >= candidate.start_line
            }).cloned()
        };
        self.rust_trait_of_memo
            .insert(candidate.id.clone(), found.as_ref().map(|t| t.id.clone()));
        Ok(found)
    }

    /// isCrossFileReachable (name-matcher.ts).
    pub(super) fn is_cross_file_reachable(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "markdown"
            && candidate.language == "markdown"
            && !markdown_path_re().is_match(&r.reference_name.replace('\\', "/"))
        {
            return Ok(false);
        }
        if r.reference_kind == "calls"
            && is_esm_family(&candidate.language)
            && (candidate.kind == "constant" || candidate.kind == "variable")
            && is_json_require_signature(candidate.signature.as_deref().unwrap_or(""))
        {
            return Ok(false);
        }
        if candidate.file_path == r.file_path {
            return Ok(true);
        }
        if !is_esm_family(&candidate.language) {
            return Ok(true);
        }
        if self.js_typed_destructured_member(r)?.is_some_and(|member| member.id == candidate.id) {
            return Ok(true);
        }
        Ok(!self.is_sealed_module(&candidate.file_path)? && !self.is_unexported_module_binding(candidate)?)
    }

    /// Whether the reference's own import names the file `target` is in: a
    /// default or named binding whose specifier resolves to that file. A
    /// function value reached this way is import-justified, so a module's
    /// unexported-binding rule does not bar it.
    pub(super) fn ref_imports_target_file(&mut self, target: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let name = r.reference_name.trim();
        if name.is_empty() { return Ok(false); }
        let mappings = self.import_mappings(&r.file_path)?;
        for mapping in mappings.iter() {
            if mapping.local_name != name { continue; }
            let Some(hit) = self.resolve_via_import(&r.clone().naming(&mapping.local_name, "references"))? else { continue; };
            if hit.node.file_path == target.file_path { return Ok(true); }
        }
        Ok(false)
    }

    /// isVisibleAcrossFiles (name-matcher.ts).
    pub(super) fn is_visible_across_files(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        self.is_visible_across_files_for(candidate, r, true)
    }

    /// `is_visible_across_files` for a pick a strategy made from the code's own
    /// structure — a receiver's type, a function value — rather than from the
    /// name. Upstream guards only the name strategies with this rule
    /// (index.ts:1867), so a module's unexported binding is no bar to a typed
    /// pick: `items$.getState().inc()` reaches the store action `inc`, and an
    /// imported component reaches its default export. Every language's own
    /// privacy still applies.
    pub(super) fn is_visible_across_files_for(&mut self, candidate: &KNode, r: &ResolveRefIn, name_pick: bool) -> Res<bool> {
        if candidate.language == "go" {
            if self.go_external_qualified(r)? { return Ok(false); }
            if let Some(imp) = self.go_ref_qualifier(r)? {
                if let Some(dir) = self.go_package_dir(&imp.source, &r.file_path) {
                    if candidate.kind == "method" || candidate.qualified_name.contains("::")
                        || pos_dirname(&candidate.file_path) != dir { return Ok(false); }
                }
            }
        }

        if !self.language_type_visible(candidate,r)? { return Ok(false); }
        if candidate.file_path == r.file_path {
            return Ok(true);
        }
        if self.is_minified_script(&candidate.file_path)
            || (super::resolver_upstream::test_suite_path(&candidate.file_path) && !is_test_path(&r.file_path)
                && !self.cpp_test_file_in_translation_unit(candidate, r)?)
            || self.sfc_private(candidate) || !self.php_class_visible(candidate, r)?
            || !self.language_type_visible(candidate, r)? { return Ok(false); }
        let lang = candidate.language.as_str();
        if lang == "c" || lang == "cpp" {
            let static_seen = candidate.kind != "function"
                || !c_source_ext_re().is_match(&candidate.file_path)
                || !self.is_static_c_function(candidate)?
                || (matches!(r.language.as_str(), "c" | "cpp")
                    && (self.c_sole_includer(candidate, &r.file_path)?
                        // Or through an include written before the call, by any path its
                        // include writes and through the headers it includes: hiredis's
                        // async.c includes `deps/hiredis/dict.c`. Not one under `#if 0`.
                        || self.cpp_included_before(&r.file_path, r.line, &candidate.file_path, &r.language)?));
            // So is what a source file's unnamed namespace declares, and a type only source files
            // declare, with its members (cpp_translation_unit).
            return Ok(static_seen && !self.cpp_translation_unit_local(candidate, r)?);
        }
        // Objective-C source globals and C types stay local; headers expose their declarations.
        if objc_file_local_elsewhere(candidate, r)
        {
            return Ok(false);
        }
        if lang == "go" {
            return self.go_name_visible(candidate, r);
        }
        // A Dart `_name` is its library's, which spans the library file and
        // its parts: riverpod_generator's `class Counter extends _$Counter`
        // names the class it wrote into `main.g.dart` (upstream f7820b58).
        if lang == "dart" {
            return Ok(candidate.visibility.as_deref() != Some("private")
                || (r.language == "dart"
                    && self.dart_same_library(&r.file_path, &candidate.file_path)));
        }
        if lang == "rust" {
            if candidate.visibility.as_deref() != Some("private") {
                return Ok(true);
            }
            if self.is_rust_trait_impl_method(candidate)? {
                return Ok(true);
            }
            // A method a trait declares carries no `pub` of its own: it is
            // the trait's, reached through a value whose type the call's
            // receiver declares (rust_written_call marks the ref typed). A
            // guess by its name alone stays out.
            if candidate.kind == "method"
                && r.row_id.is_some_and(|id| self.rust_typed_refs.contains(&id))
                && self
                    .rust_trait_of(candidate)?
                    .is_none_or(|t| t.visibility.as_deref() != Some("private"))
            {
                return Ok(true);
            }
            let owner = rust_module_dir(&candidate.file_path);
            return Ok(self.rust_file_under(&r.file_path, &owner, &candidate.file_path));
        }
        if private_is_file_local(lang) {
            return Ok(candidate.visibility.as_deref() != Some("private"));
        }
        // An R test file runs in an environment of its own (testthat): its
        // top-level `c <- ggplot(…)` is not what the package's `c(…)` calls
        // mean. `helper-*.R` / `setup-*.R` are sourced for every test, so
        // theirs are shared.
        if lang == "r"
            && matches!(candidate.kind.as_str(), "variable" | "constant")
            && re!(r"(?:^|/)tests?/").is_match(&candidate.file_path)
            && !re!(r"(?:^|/)(?:helper|setup)[^/]*\.[rR]$").is_match(&candidate.file_path)
        {
            return Ok(false);
        }
        // A Lua `local` belongs to its chunk: a spec helper's `local it = it`
        // is not busted's `it(…)` in every other spec file.
        if (lang == "lua" || lang == "luau") && self.is_lua_local(candidate) {
            return Ok(self.lua_alias_target(r)?.flatten().is_some_and(|n| n.id == candidate.id));
        }
        if !name_pick && is_esm_family(&candidate.language) {
            return Ok(true);
        }
        self.is_cross_file_reachable(candidate, r)
    }

    /// isBareJsCall (name-matcher.ts): a receiver-less JS/TS `calls` ref.
    pub(super) fn is_bare_js_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if !is_js_family(&r.language) {
            return Ok(false);
        }
        self.is_receiver_less_call(r)
    }

    /// isBareGoCall (name-matcher.ts): a receiver-less Go `calls` ref. A Go
    /// method is reachable only through a value or a method expression, so
    /// a bare call never means one (#1857). Read from the source line
    /// because `pkg.Factory().Method()` reaches the resolver as a bare
    /// `Method` ref too.
    pub(super) fn is_bare_go_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "go" {
            return Ok(false);
        }
        if self.is_receiver_less_call(r)? { return Ok(true); }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false); };
        let Some(line) = lines.get((r.line - 1).max(0) as usize) else { return Ok(false); };
        let at = super::names::js_unit_to_byte(line, r.column.max(0) as usize).min(line.len());
        let conversion = Self::cached_regex(&format!(r"^\(\s*\*?\s*{}\s*\)\s*\(", regex::escape(&r.reference_name)))?;
        Ok(conversion.is_match(&line[at..]))
    }

    fn is_bare_r_call(&mut self, r: &ResolveRefIn) -> bool {
        if r.language != "r" || r.reference_kind != "calls" { return false; }
        let Some(lines) = self.read_file(&r.file_path) else { return false };
        let Some(line) = lines.get((r.line-1).max(0) as usize) else { return false };
        let Ok(pattern) = Self::cached_regex(&format!(r"(?:^|[^\w.])({})\s*\(",regex::escape(&r.reference_name))) else { return false };
        pattern.captures(line).is_some_and(|m| !re!(r"(?:\$|@|::)\s*$").is_match(&line[..m.get(1).unwrap().start()]))
    }

    /// The name in a Go file's `package` clause (the first line that starts
    /// with `package`, outside block comments).
    pub(super) fn go_package_clause(&mut self, file: &str) -> Option<String> {
        let lines = self.read_file(file)?;
        let mut in_block = false;
        for line in lines.iter() {
            let mut code = line.as_str();
            if in_block {
                let Some(end) = code.find("*/") else { continue };
                code = &code[end + 2..];
                in_block = false;
            }
            let code = code.trim_start();
            if code.starts_with("/*") && !code.contains("*/") {
                in_block = true;
                continue;
            }
            if let Some(c) = re!(r"^package\s+([A-Za-z_][A-Za-z0-9_]*)").captures(code) {
                return Some(c[1].to_string());
            }
        }
        None
    }

    /// isGoNameVisible (name-matcher.ts): by the name's first letter, not the
    /// extractor's flag (unset for every Go method, exported or not); an
    /// unexported name is its own package's. A package is its directory AND
    /// package clause: `a_test` files beside `a` files are a package of their
    /// own whose bare top-level names reach `a` only through an import —
    /// qualified, or bare after a dot import and exported (#2581).
    pub(super) fn go_name_visible(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        let dir = pos_dirname(&candidate.file_path);
        if dir != pos_dirname(&r.file_path) {
            return Ok(candidate.name.chars().next().is_some_and(|c| c.is_ascii_uppercase()));
        }
        if candidate.file_path == r.file_path
            || matches!(candidate.kind.as_str(), "method" | "property" | "field" | "enum_member")
            || r.reference_name.contains('.')
        {
            return Ok(true);
        }
        let own = self.go_package_clause(&r.file_path);
        let theirs = self.go_package_clause(&candidate.file_path);
        if own.is_none() || theirs.is_none() || own == theirs {
            return Ok(true);
        }
        Ok(candidate.name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && self.go_dot_imports(&r.file_path)?.dirs.iter().any(|d| *d == dir))
    }

    /// isBarePhpCall (name-matcher.ts): a PHP `calls` ref written without a
    /// receiver — `redirect($url)`, `view('books.show')` — rather than
    /// `$this->redirect()` / `Foo::view()`. PHP has no implicit `$this`, so
    /// such a call is only ever a function. PHP refs record the column of
    /// the call expression, so a bare call is one whose text at that column
    /// is the name itself (after an optional `\` for a qualified function).
    pub(super) fn is_bare_php_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "php" || r.reference_kind != "calls" {
            return Ok(false);
        }
        if r.reference_name.is_empty() || !r.reference_name.bytes().all(is_word_byte) {
            return Ok(false);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(false) };
        let col = r.column as usize;
        let at = if js_slice(line, col).starts_with('\\') { col + 1 } else { col };
        let is_call = js_slice(line, at)
            .strip_prefix(r.reference_name.as_str())
            .is_some_and(|rest| bare_call_opener_re().is_match(rest));
        if !is_call {
            return Ok(false);
        }
        // `$obj->name(` / `Foo::name(` / `$obj?->name(`, should a column ever land on
        // the name — but not `'size' => filesize($zip)` or `$x ? a : name()`.
        let before = js_prefix(line, at).trim_end();
        Ok(!(before.ends_with("->") || before.ends_with("::")))
    }

    pub(super) fn is_receiver_less_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.reference_kind != "calls" {
            return Ok(false);
        }
        // `!name.includes('.')` — dead on the bare path, live for non-bare
        // callers (a `Foo::bar` calls ref carries no '.').
        if r.reference_name.contains('.') {
            return Ok(false);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(false) };
        let at = js_slice(line, r.column as usize);
        // `new RegExp('^' + nameEsc + '\\s*[(<]')` — the name is a literal
        // prefix, then the call opener.
        let is_call = at
            .strip_prefix(r.reference_name.as_str())
            .is_some_and(|rest| bare_call_opener_re().is_match(rest));
        if !is_call {
            return Ok(false);
        }
        let before = js_prefix(line, r.column as usize);
        // A JS/TS receiver ends in a dot; a preceding ')' closes a braceless
        // statement header. Go's parenthesized call forms keep their reading.
        if is_js_family(&r.language) && before.trim_end().ends_with(')') {
            return Ok(true);
        }
        Ok(!js_call_prefix_re().is_match(before) || js_call_keyword_re().is_match(before))
    }

    /// cppBareCallForm (name-matcher.ts) — only the ADL range names.
    pub(super) fn cpp_bare_call_form(&mut self, r: &ResolveRefIn) -> Res<Option<&'static str>> {
        if r.reference_kind != "calls" {
            return Ok(None);
        }
        if r.language != "c" && r.language != "cpp" {
            return Ok(None);
        }
        if !is_cpp_adl_name(&r.reference_name) {
            return Ok(None);
        }
        // `name.includes('.') || includes('::')` — dead for bare names.
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(line) = lines.get((r.line - 1) as usize) else { return Ok(None) };
        let from = (r.column as usize).min(utf16_len(line));
        let hay = js_slice(line, from);
        // `(^|[^A-Za-z0-9_])NAME\s*\(` — the literal name, not preceded by a
        // word byte, then the call opener.
        let name = r.reference_name.as_str();
        let Some(at) = occurrences(hay, name, 0).find(|&at| {
            (at == 0 || !is_word_byte(hay.as_bytes()[at - 1]))
                && cpp_call_opener_re().is_match(&hay[at + name.len()..])
        }) else {
            return Ok(None);
        };
        // m.index + m[1].length in TS: the name's UTF-16 offset in `hay`.
        let name_at = from + utf16_len(&hay[..at]);
        let before: String = js_prefix(line, name_at)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if before.ends_with("::") {
            return Ok(Some("qualified"));
        }
        if cpp_this_arrow_re().is_match(&before) || cpp_this_dot_re().is_match(&before) {
            return Ok(Some("this-member"));
        }
        if before.ends_with("->") || before.ends_with('.') {
            return Ok(Some("explicit-member"));
        }
        let after_name = js_slice(line, name_at + utf16_len(&r.reference_name));
        let Some(open) = after_name.find('(') else { return Ok(Some("implicit-this")) };
        Ok(Some(
            if after_name_paren_re().is_match(&after_name[open + 1..]) {
                "implicit-this"
            } else {
                "free-args"
            },
        ))
    }

    /// isImplicitThisFieldCall (name-matcher.ts): the one case a bare `calls`
    /// name can be a `field` — C++ `this->fp(...)` arrives as a bare ref (the
    /// extractor drops `this`), so allow it only when the field's owner is
    /// the call site's enclosing type.
    pub(super) fn is_implicit_this_field_call(&mut self, r: &ResolveRefIn, field: &KNode) -> Res<bool> {
        if r.language != "cpp" {
            return Ok(false);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(false);
        };
        let Some(prefix) = enclosing_type_prefix(Some(caller.as_ref())) else {
            return Ok(false);
        };
        Ok(enclosing_type_prefix(Some(field)).as_deref() == Some(prefix.as_str()))
    }

    /// Returns `Ok(None)` when the call-site form vetoes every candidate
    /// (the TS `null` — reference stays unresolved).
    pub(super) fn apply_cpp_call_site_form(
        &mut self,
        r: &ResolveRefIn,
        candidates: Vec<Arc<KNode>>,
    ) -> Res<Option<Vec<Arc<KNode>>>> {
        let Some(form) = self.cpp_bare_call_form(r)? else {
            return Ok(Some(candidates));
        };
        match form {
            "qualified" | "explicit-member" => Ok(None),
            "this-member" => {
                let origin = self.node_by_id(&r.from_node_id)?;
                let Some(prefix) = enclosing_type_prefix(origin.as_deref()) else {
                    return Ok(None);
                };
                Ok(Some(
                    candidates
                        .into_iter()
                        .filter(|n| callable_on_type(n, &prefix))
                        .collect(),
                ))
            }
            "free-args" => {
                let local: Vec<Arc<KNode>> = candidates
                    .into_iter()
                    .filter(|n| n.kind == "function" && n.file_path == r.file_path)
                    .collect();
                Ok(Some(if local.len() == 1 { local } else { Vec::new() }))
            }
            _ => Ok(Some(candidates)),
        }
    }

    /// findBestMatch (name-matcher.ts) — strict `>` first-max scoring.
    pub(super) fn find_best_match(&mut self, r: &ResolveRefIn, candidates: &[Arc<KNode>]) -> Option<Arc<KNode>> {
        let mut best_score = -1f64;
        let mut best: Option<Arc<KNode>> = None;
        let mut tied: Vec<Arc<KNode>> = Vec::new();
        let mut ref_dirs: Vec<String> =
            r.file_path.split('/').map(|s| s.to_string()).collect();
        ref_dirs.pop();
        let has_same_language = candidates.iter().any(|c| c.language == r.language);
        for candidate in candidates {
            if has_same_language && candidate.language != r.language {
                continue;
            }
            let mut score = 0f64;
            if candidate.file_path == r.file_path {
                score += 100.0;
            }
            if r.language == "vbnet" && self.vb_same_project(&candidate.file_path, &r.file_path) { score += 80.0; }
            score += path_proximity_from_dirs(&ref_dirs, &candidate.file_path) as f64;
            if candidate.language == r.language {
                score += 50.0;
            } else {
                score -= 80.0;
            }
            if r.reference_kind == "calls"
                && (candidate.kind == "function" || candidate.kind == "method")
            {
                score += 25.0;
            }
            if r.reference_kind == "instantiates"
                && matches!(candidate.kind.as_str(), "class" | "struct" | "union" | "interface")
            {
                score += 25.0;
            }
            if r.reference_kind == "decorates" {
                if candidate.kind == "function" || candidate.kind == "method" {
                    score += 25.0;
                } else if candidate.kind == "class" || candidate.kind == "interface" {
                    score += 15.0;
                }
            }
            if candidate.is_exported {
                score += 10.0;
            }
            if candidate.file_path == r.file_path && candidate.start_line > 0 {
                let distance = (candidate.start_line - r.line).abs() as f64;
                score += (20.0 - distance / 10.0).max(0.0);
            }
            if score > best_score {
                best_score = score;
                best = Some(candidate.clone());
                tied.clear();
                tied.push(candidate.clone());
            } else if score == best_score {
                tied.push(candidate.clone());
            }
        }
        // Proximity is capped, so on deep trees two copies of the same symbol
        // in sibling modules can tie; the caller's own copy, if uniquely
        // closest, wins (upstream 81559dcc).
        if tied.len() > 1 {
            if let Some(own) = closest_copy(r, &tied) {
                return Some(own);
            }
        }
        best
    }

    /// innerScopeShadows (name-matcher.ts): of the same-name nodes, those the
    /// ref's own file declares strictly inside a function or method — a
    /// nested declaration shadows an out-of-repo import binding; a top-level
    /// one does not (#2491).
    fn inner_scope_shadows(&mut self, nodes: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let local: Vec<Arc<KNode>> = nodes.into_iter().filter(|n| n.file_path == r.file_path).collect();
        if local.is_empty() {
            return Ok(local);
        }
        let scopes: Vec<Arc<KNode>> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| n.kind == "function" || n.kind == "method")
            .cloned()
            .collect();
        Ok(local
            .into_iter()
            .filter(|n| {
                scopes.iter().any(|s| {
                    s.id != n.id
                        && s.start_line <= n.start_line
                        && s.end_line >= n.end_line
                        && (s.start_line < n.start_line || s.end_line > n.end_line)
                })
            })
            .collect())
    }

    /// matchByExactName (name-matcher.ts).
    pub(super) fn match_by_exact_name(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let found = self.match_by_exact_name_uncapped(r)?;
        self.cap_php_unseen_ancestor(found, r)
    }

    fn match_by_exact_name_uncapped(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if self.is_unknown_receiver_built_in_call(r) {
            return Ok(None);
        }
        let bare_js = self.is_bare_js_call(r)?;
        let bare_go = self.is_bare_go_call(r)?;
        let bare_r = self.is_bare_r_call(r);
        let bare_php = self.is_bare_php_call(r)?;
        // A type, a value or an import the file binds from a package outside
        // the repository names nothing in it, whatever kind of reference it
        // is (upstream's early null in matchByExactName, #2491).
        if !bare_js
            && is_js_family(&r.language)
            && r.reference_kind != "calls"
            && re!(r"^[A-Za-z_$][\w$]*$").is_match(&r.reference_name)
            && self.js_out_of_repo_binding(r)?
        {
            return Ok(None);
        }
        let mut out_of_repo_call = false;
        if bare_js {
            if self.is_param_shadowed(r)? {
                return Ok(None);
            }
            if let Some(c) = self.match_js_store_binding_call(r)? {
                return Ok(Some(c));
            }
            // `import { useQuery } …`, or an in-scope `await import()`: the
            // call means the package's, and no other file's same-named
            // symbol — only a declaration nested in this file's own inner
            // scope still shadows it.
            out_of_repo_call = self.js_out_of_repo_binding(r)?;
        }
        let named = self.nodes_by_name(&r.reference_name)?;
        let named = if out_of_repo_call {
            Arc::new(self.inner_scope_shadows(named.to_vec(), r)?)
        } else {
            named
        };
        // Big same-name pools make a bare match pathological: a name like
        // `t` gets thousands of candidates whose retain/scope checks
        // dominate a whole batch (pretix's fabric.min.js: ~950ms per
        // 250-ref chunk vs a ~90ms floor). Two bounds, both declining the
        // name-match entirely — `unresolved`, never a truncated winner —
        // with the framework merge still applying downstream:
        //   * the ref's own file is generated-flagged (minified/codegen
        //     sources degrade by precedent);
        //   * the pool is dominated by generated nodes — an artifact pool
        //     cannot hold a real target anyway, so handwritten refs in
        //     vendored sources (d3.v6.js, fabric.js, pdf.worker.js —
        //     unminified, unflagged) get the same bound, while real big
        //     pools (Migration, __init__) proceed untouched.
        if named.len() > GENERATED_NAME_POOL_CAP
            && (r.generated == Some(true)
                || named.iter().filter(|n| self.is_gen_path(&n.file_path)).count() * 4
                    >= named.len() * 3)
        {
            return Ok(None);
        }
        // Macro constants are not callees (#1838). Where the name is a
        // function-like macro, the call can only reach the function another
        // build configuration compiles, never a same-named type
        // (`PREFIX(scanRef)(…)` onto an unrelated `struct PREFIX`). A type
        // declared in the calling file, which defines no such macro, is still
        // the one `Widget()` builds.
        let macro_named = r.reference_kind == "calls"
            && (r.language == "c" || r.language == "cpp")
            && named.iter().any(|n| cpp::is_define(n))
            && !(named.iter().any(|n| n.file_path == r.file_path && !matches!(n.kind.as_str(), "function" | "method" | "constant"))
                && !named.iter().any(|n| n.file_path == r.file_path && cpp::is_define(n)));
        let all_named: Vec<Arc<KNode>> = named
            .iter()
            .filter(|n| !objc_file_local_elsewhere(n, r))
            .filter(|n| !macro_named || matches!(n.kind.as_str(), "function" | "method"))
            .cloned()
            .collect();
        let mut candidates = self.apply_language_gate(all_named, r);
        candidates.retain(|n| n.kind != "import");
        // A .NET type position names a type, never a same-named member.
        if self.is_dotnet_type_ref(r) {
            candidates.retain(|n| can_name_in_type_position(n));
        }
        // A Scala type position (`Arbitrary[B]`) never names a method: an
        // `implicit def A: Order[A]` shares its name with half of cats' type
        // parameters. Scala's value references only read a file's own vals.
        if r.language == "scala"
            && r.reference_kind == "references"
            && r.reference_name.starts_with(|c: char| c.is_ascii_uppercase())
        {
            candidates.retain(|n| !matches!(n.kind.as_str(), "method" | "function"));
        }
        // A bare Rust name reaches only what is in scope.
        if is_bare_rust_name(r) {
            candidates.retain(|n| self.is_rust_name_in_scope(n, r));
        }
        candidates = self.retain_python_java_scope(candidates, r)?;
        let (kept, scope_shrank) = self.retain_lang_scope_tracked(candidates, r)?;
        candidates = kept;
        candidates = self.retain_php_self_scope(candidates, r)?;
        candidates = self.retain_php_declared_type(candidates, r)?;
        candidates = self.retain_ruby_lexical_constant(candidates, r)?;
        // A Vue component's own method is `this.m()` inside that component —
        // not `this.$refs['input'].click()` on an element another component renders.
        if r.reference_kind == "calls" && is_js_family(&r.language) {
            let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(candidates.len());
            for n in candidates.into_iter() {
                if !php_scope::is_vue_component_method(&n) || self.is_this_call_in_own_file(&n, r)? {
                    kept.push(n);
                }
            }
            candidates = kept;
        }
        // Nested locals reachable only from inside their container (#1230).
        let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(candidates.len());
        for n in candidates.into_iter() {
            if self.dart_top_level_visible(&n, r)? && self.is_lexically_reachable(&n, r)? && self.language_type_visible(&n, r)? && self.php_class_visible(&n,r)?
                // And a Go name that isn't exported, only its own package's: prometheus
                // tsdb's `[]*memSeries` is head.go's, not `tsdb/agent`'s, which scored the
                // same by directory proximity and was then vetoed as invisible.
                && (n.language != "go" || self.go_name_visible(&n, r)?)
                // A Dart `_name` another library declares: the reference's
                // own library's declaration wins instead of the namesake
                // picked and then declined (upstream f7820b58 / #2466).
                && !(r.language == "dart" && n.language == "dart" && n.visibility.as_deref() == Some("private") && n.file_path != r.file_path && !self.dart_same_library(&r.file_path, &n.file_path))
                && !(matches!(r.language.as_str(),"lua"|"luau") && r.reference_kind=="calls" && !r.reference_name.contains(['.',':']) && n.kind=="method")
                && !(r.language=="python" && r.reference_kind=="calls" && n.file_path!=r.file_path && self.is_receiver_less_call(r)? && self.python_locally_bound(&r.reference_name,r)? && !self.python_fixture_reachable(&n,&r.file_path)) {
                kept.push(n);
            }
        }
        let mut candidates = kept;
        prefer_adjacent_implementations(&mut candidates, r);
        candidates.retain(|n| !is_inheritance_ref(&r.reference_kind) || is_supertype_target(n));
        candidates.retain(|n| r.reference_kind != "imports" || is_importable_kind(&n.kind));
        {
            let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(candidates.len());
            for n in candidates.into_iter() {
                if r.reference_kind != "imports"
                    || n.file_path == r.file_path
                    || !is_esm_family(&n.language)
                    || !self.is_sealed_module(&n.file_path)?
                {
                    kept.push(n);
                }
            }
            candidates = kept;
        }
        // A receiver-less JS/TS or Go call cannot reach a method (#1714, #1857).
        candidates.retain(|n| !((bare_js || bare_go || bare_r) && n.kind == "method"));
        // `self.get_ip()` names the method (or nested class) the calling
        // class declares or inherits along single bases, never a module
        // function or another class's method that shares the name (upstream
        // settles that by node order alone).
        if r.language == "python"
            && candidates.len() > 1
            && candidates.iter().any(|n| n.kind == "method" || n.kind == "class")
        {
            if let Some(own) = self.python_self_method(r)? {
                if candidates.iter().any(|n| n.id == own.id) {
                    candidates.retain(|n| n.id == own.id);
                }
            }
        }
        // A bare Go call names its own package's function: another
        // package's `New` needs its qualifier (a dot import cannot bring in a
        // name the package itself declares). The package is the directory
        // and its `package` clause: an external test package (`pkg_test`)
        // in the same directory is another package.
        if bare_go && candidates.len() > 1 {
            let dir = pos_dirname(&r.file_path).to_string();
            let own = self.go_package_clause(&r.file_path);
            let mut same: Vec<Arc<KNode>> = Vec::new();
            for n in &candidates {
                if n.language == "go" && pos_dirname(&n.file_path) == dir && self.go_package_clause(&n.file_path) == own {
                    same.push(n.clone());
                }
            }
            if !same.is_empty() {
                candidates = same;
            }
        }
        // A bare PHP call is a function call: nothing else is callable without a receiver.
        candidates.retain(|n| !(bare_php && n.kind != "function"));
        // A C/C++ `field` is reachable only through a receiver — `s.f`,
        // `p->f`, `T::f` — which the extractor encodes as a dotted or
        // `::`-qualified ref, so a bare name can never mean one. C++ keeps
        // an implicit-this exception (`this->` is dropped by the extractor)
        // when the field's owner is the ref site's enclosing type. Without
        // this, Linux-scale corpora resolved ~39k bare libc/kernel calls
        // (`bind`, `close`, `ioctl`) onto whatever fn-pointer member shared
        // the name. Other languages' `field` nodes are exempt — Solidity
        // `emit Event(...)` legitimately bare-calls an event field.
        // (name-matcher.ts: matchByExactName / isImplicitThisFieldCall)
        {
            let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(candidates.len());
            for n in candidates.into_iter() {
                if n.kind == "field"
                    && (n.language == "c" || n.language == "cpp")
                    && !self.is_implicit_this_field_call(r, &n)?
                {
                    continue;
                }
                kept.push(n);
            }
            candidates = kept;
        }
        if bare_js {
            let locally_bound =
                self.is_locally_bound_js_name(&r.reference_name, &r.file_path, Some(r.line))?;
            if locally_bound {
                candidates.retain(|n| n.file_path == r.file_path);
            }
        }

        // Whatever the call looks like, a name a parameter or local shadows at
        // the ref line is never another file's symbol.
        if candidates.iter().any(|n| n.file_path != r.file_path) && self.is_shadowed_import_name(r)? {
            candidates.retain(|n| n.file_path == r.file_path);
        }

        // A name bound to a bare external import can land only on a same-file
        // definition (#1709).
        if candidates.iter().any(|n| n.file_path != r.file_path)
            && self.is_bound_to_bare_import(r)?
        {
            candidates.retain(|n| n.file_path == r.file_path);
        }

        if r.language == "dart" {
            let mut own = Vec::new();
            for n in &candidates {
                if self.dart_library_decl(n)? && self.dart_same_library(&r.file_path, &n.file_path) { own.push(n.clone()); }
            }
            if !own.is_empty() {
                let ids: HashSet<_> = own.iter().map(|n| n.id.clone()).collect();
                let mut preferred = Vec::new();
                for n in candidates { if !self.dart_library_decl(&n)? || ids.contains(&n.id) { preferred.push(n); } }
                candidates = preferred;
            }
        }
        // A bare Dart call means the nearest member of the hierarchy around it.
        let candidates = self.nearest_dart_members(candidates, r)?;
        let candidates = self.nearest_swift_members(candidates, r)?;
        let candidates = self.cpp_specialization_pool(candidates, r);

        // C/C++ call-site form.
        let Some(cpp_form) = self.apply_cpp_call_site_form(r, candidates)? else {
            return Ok(None);
        };
        // (Above the ceiling below nothing is picked, so no translation unit is read.)
        let candidates = if cpp_form.len() as i64 <= self.ambiguous_ceiling {
            self.within_cpp_translation_unit(cpp_form, r, true)?
        } else {
            cpp_form
        };
        if candidates.is_empty() {
            return Ok(None);
        }
        if candidates.len() == 1 {
            if !self.is_cross_file_reachable(&candidates[0], r)? {
                return Ok(None);
            }
            if bare_js && !is_bare_call_target_kind(&candidates[0].kind) {
                return Ok(None);
            }
            let cross = candidates[0].language != r.language;
            // A scope rule narrowed the set: the one left is what remains,
            // not what the call binds, unless its scope says so.
            let leftover = scope_shrank && !self.is_lang_survivor_bound(&candidates[0], r)?;
            // An import from a package only manifest text places here names
            // no directory: the one same-named symbol is a guess, not the
            // import's target.
            let unmapped = candidates[0].file_path != r.file_path && self.is_bound_to_unmapped_package(r)?;
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: if cross { 0.5 } else if leftover || unmapped { 0.7 } else { 0.9 },
                resolved_by: "exact-match",
            }));
        }
        // A member of the caller's own class (or what it inherits / is
        // nested in) is what the name means there, however far away it is
        // declared. Not for a call written on `super` in JS (upstream
        // 25776014 / #2497).
        let member = if self.is_js_super_call_site(r)? {
            None
        } else {
            self.match_enclosing_scope_member(r, &candidates)?
        };
        // The scope names the member, not the overload: when the class
        // declares the name more than once, the one the arguments fit, else
        // the nearest, is chosen below — among those overloads only.
        let mut pool: Vec<Arc<KNode>> = candidates.to_vec();
        if let Some(member) = member {
            let overloads: Vec<Arc<KNode>> = candidates
                .iter()
                .filter(|c| c.qualified_name == member.qualified_name && c.file_path == member.file_path)
                .cloned()
                .collect();
            if overloads.len() <= 1 {
                return Ok(Some(KCand {
                    node: member,
                    confidence: 0.9,
                    resolved_by: "exact-match",
                }));
            }
            pool = overloads;
        }
        if pool.len() as i64 > self.ambiguous_ceiling {
            return Ok(None);
        }
        let Some(best) = self.find_best_match(r, &pool) else {
            return Ok(None);
        };
        let best = self.cpp_function_overload(best, &pool, r)?;
        if bare_js && !is_bare_call_target_kind(&best.kind) {
            return Ok(None);
        }
        if self.is_cross_file_reachable(&best, r)? {
            let proximity = compute_path_proximity(&r.file_path, &best.file_path);
            // Every candidate is a copy of one symbol and this is the caller's
            // own copy: the name itself is not ambiguous, only which copy —
            // so rank it above a proximity guess, below a unique name or the
            // file's own definition (upstream 81559dcc).
            let weighed: Vec<Arc<KNode>> = if candidates.iter().any(|c| c.language == r.language) {
                candidates.iter().filter(|c| c.language == r.language).cloned().collect()
            } else {
                candidates.to_vec()
            };
            let confidence = if self.is_decisive_same_file_match(&best, r, &candidates)? {
                0.9
            } else if best.id != r.from_node_id && closest_copy(r, &weighed).is_some_and(|c| c.id == best.id) {
                0.8
            } else if proximity >= 30 {
                0.7
            } else {
                0.4
            };
            return Ok(Some(KCand {
                node: best,
                confidence,
                resolved_by: "exact-match",
            }));
        }
        Ok(None)
    }

    /// matchEnclosingScopeMember (name-matcher.ts, upstream 25776014): a call
    /// that means a member of the class it is written in — a receiver-less
    /// `render()` in Java / Scala, or `this.render()` / `self.render()` in
    /// TypeScript / JavaScript / Python — binds by the language's scoping, not
    /// by which same-named method sits nearest: the caller's own class first,
    /// then what it inherits, then (bare only) enclosing classes.
    fn match_enclosing_scope_member(&mut self, r: &ResolveRefIn, candidates: &[Arc<KNode>]) -> Res<Option<Arc<KNode>>> {
        const IMPLICIT: [&str; 2] = ["java", "scala"];
        const EXPLICIT: [&str; 5] = ["python", "typescript", "tsx", "javascript", "jsx"];
        if r.reference_kind != "calls" || r.reference_name.contains(['.', ':']) {
            return Ok(None);
        }
        let implicit = IMPLICIT.contains(&r.language.as_str());
        if !implicit && !EXPLICIT.contains(&r.language.as_str()) {
            return Ok(None);
        }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else {
            return Ok(None);
        };
        if caller.qualified_name.is_empty() {
            return Ok(None);
        }
        let Some(receiver) = self.call_receiver_kind(r)? else {
            return Ok(None);
        };
        if receiver == ReceiverKind::Bare && !implicit {
            return Ok(None);
        }

        let name = r.reference_name.as_str();
        // Candidates keyed by declaring type, qualified and simple.
        let mut by_owner: HashMap<String, Vec<Arc<KNode>>> = HashMap::new();
        let mut by_owner_name: HashMap<String, Vec<Arc<KNode>>> = HashMap::new();
        for c in candidates {
            let Some(sep) = c.qualified_name.rfind("::") else { continue };
            if sep == 0 || &c.qualified_name[sep + 2..] != name {
                continue;
            }
            let owner = c.qualified_name[..sep].to_string();
            let simple = owner.rsplit("::").next().unwrap_or(&owner).to_string();
            by_owner.entry(owner).or_default().push(c.clone());
            by_owner.entry(simple.clone()).or_default();
            by_owner_name.entry(simple).or_default().push(c.clone());
        }
        let callable_first = |found: &Vec<Arc<KNode>>| -> Vec<Arc<KNode>> {
            let mut v = found.clone();
            v.sort_by_key(|c| i32::from(!(c.kind == "method" || c.kind == "function")));
            v
        };
        // `Owner::Owner` is a constructor: a bare `Owner(...)` builds a new
        // instance, it does not call an inherited or enclosing member.
        let members = |owner: &str| -> Vec<Arc<KNode>> {
            if owner == name || owner.ends_with(&format!("::{name}")) {
                Vec::new()
            } else {
                callable_first(by_owner.get(owner).unwrap_or(&Vec::new()))
            }
        };

        let segments: Vec<&str> = caller.qualified_name.split("::").collect();
        let start = if MEMBER_OWNER_KINDS.contains(&caller.kind.as_str()) { segments.len() } else { segments.len() - 1 };
        for i in (1..=start).rev() {
            let scope = segments[..i].join("::");
            let type_decls = self.types_named(&scope, &r.language)?;
            if type_decls.is_empty() {
                // `this.`/`super.` name a class member; only a bare call sees locals.
                if receiver != ReceiverKind::Bare {
                    continue;
                }
                if let Some(local_fn) = members(&scope).into_iter().find(|c| {
                    c.file_path == r.file_path && (c.kind == "method" || c.kind == "function")
                }) {
                    return Ok(Some(local_fn));
                }
                continue;
            }
            if receiver != ReceiverKind::Super {
                // pick(scope, true): the caller's file first; another file
                // only when a single type carries the qualified name.
                let found = members(&scope);
                if !found.is_empty() {
                    if let Some(local) = found.iter().find(|c| c.file_path == r.file_path) {
                        return Ok(Some(local.clone()));
                    }
                    if type_decls.len() == 1 {
                        return Ok(Some(found[0].clone()));
                    }
                }
            }
            // The type's own supertypes first; by name when they reach nothing.
            let decl = self.decl_of(&type_decls, r);
            let simple = segments[i - 1];
            let supers = self.direct_supertypes(&type_decls, simple, r)?;
            let via_super = match &decl {
                Some(d) => self.inherited_through(d, &by_owner, name, receiver, r)?,
                None => None,
            };
            let via_super = match via_super {
                Some(v) => Some(v),
                None => self.inherited_by_name(&supers, simple, &by_owner_name, name, receiver, r)?,
            };
            if let Some(v) = via_super {
                return Ok(Some(v));
            }
            // `this`/`self`/`super` is the innermost instance; outer classes
            // are reachable only by a bare call.
            if receiver != ReceiverKind::Bare {
                return Ok(None);
            }
            // A supertype outside the index may declare the name: stop rather
            // than guess past it.
            if supers.is_empty() && self.declares_supertype(&type_decls, simple, r)? {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// The receiver a call site's line names: `render()`, `this.render()`,
    /// `self.render()`, `super.render()` all extract as `render` (upstream
    /// callReceiverKind). None when the receiver is something else.
    fn call_receiver_kind(&mut self, r: &ResolveRefIn) -> Res<Option<ReceiverKind>> {
        let Some(src) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let Some(line) = src.line((r.line - 1) as usize) else {
            return Ok(None);
        };
        let mut at: Option<usize> = None;
        for start in name_offsets(line, &r.reference_name) {
            at = Some(start);
            if start >= r.column as usize {
                break;
            }
        }
        let Some(at) = at else {
            return Ok(None);
        };
        let before = &line[..at];
        if re!(r"(?:^|[^\w$.])super(?:\s*\([^()]*\)|<[^<>]*>)?\s*\.\s*$").is_match(before) {
            return Ok(Some(ReceiverKind::Super));
        }
        if re!(r"(?:^|[^\w$.])this\s*\??\.\s*$").is_match(before) {
            return Ok(Some(ReceiverKind::Slf));
        }
        // A JS `self` the file doesn't bind is the global object, not the instance.
        if re!(r"(?:^|[^\w$.])self\s*\??\.\s*$").is_match(before) {
            if is_js_family(&r.language) && self.is_browser_host_global("self", r)? {
                return Ok(None);
            }
            return Ok(Some(ReceiverKind::Slf));
        }
        if r.language == "python" && re!(r"(?:^|[^\w$.])cls\s*\.\s*$").is_match(before) {
            return Ok(Some(ReceiverKind::Slf));
        }
        if re!(r"(?:\.|->|::)\s*$").is_match(before) {
            return Ok(None);
        }
        Ok(Some(ReceiverKind::Bare))
    }

    /// typesNamed: owner-kind nodes carrying the qualified name, same language
    /// family as the ref.
    fn types_named(&mut self, qn: &str, lang: &str) -> Res<Vec<Arc<KNode>>> {
        Ok(self
            .nodes_by_qualified_name(qn)?
            .iter()
            .filter(|n| MEMBER_OWNER_KINDS.contains(&n.kind.as_str()) && same_language_family(&n.language, lang))
            .cloned()
            .collect())
    }

    /// declOf: the declaration of `scope` the call sits in, else the only one.
    fn decl_of(&mut self, decls: &[Arc<KNode>], r: &ResolveRefIn) -> Option<Arc<KNode>> {
        let enclosing: Vec<&Arc<KNode>> = decls
            .iter()
            .filter(|n| n.file_path == r.file_path && n.start_line <= r.line && n.end_line >= r.line)
            .collect();
        if let Some(last) = enclosing.last() {
            return Some((*last).clone());
        }
        if decls.len() == 1 {
            return Some(decls[0].clone());
        }
        None
    }

    /// A type declaration's header: its text up to the body's `{`, every
    /// declaration of that name joined (upstream typeHeader/headerOf).
    fn type_header(&mut self, decls: &[Arc<KNode>], r: &ResolveRefIn) -> Res<Option<String>> {
        let enclosing: Vec<&Arc<KNode>> = decls
            .iter()
            .filter(|n| n.file_path == r.file_path && n.start_line <= r.line && n.end_line >= r.line)
            .collect();
        let chosen: Vec<Arc<KNode>> = if let Some(last) = enclosing.last() {
            vec![(*last).clone()]
        } else {
            decls.to_vec()
        };
        if decls.is_empty() {
            return Ok(None);
        }
        let mut header = String::new();
        for decl in chosen {
            let Some(src) = self.read_file(&decl.file_path) else {
                return Ok(None);
            };
            let start = (decl.start_line - 1) as usize;
            let stop = (decl.start_line as usize + 30).min(src.lines.len());
            for l in start..stop {
                let Some(text) = src.line(l) else { break };
                match text.find('{') {
                    Some(b) => {
                        header.push(' ');
                        header.push_str(&text[..b]);
                        break;
                    }
                    None => {
                        header.push(' ');
                        header.push_str(text);
                    }
                }
            }
        }
        Ok(Some(header))
    }

    /// directSupertypes: of the simple name's resolved supertypes, the ones
    /// the declaration's header names, in header order.
    fn direct_supertypes(&mut self, decls: &[Arc<KNode>], simple: &str, r: &ResolveRefIn) -> Res<Vec<String>> {
        let all = self.supertype_names(simple, r, false, false)?;
        if all.is_empty() {
            return Ok(all);
        }
        let Some(header) = self.type_header(decls, r)? else {
            return Ok(all);
        };
        let mut keyed: Vec<(i64, String)> = all
            .into_iter()
            .map(|t| {
                let pos = name_offsets(&header, &t).next().map(|p| p as i64).unwrap_or(-1);
                (pos, t)
            })
            .filter(|(p, _)| *p >= 0)
            .collect();
        keyed.sort_by_key(|(p, _)| *p);
        Ok(keyed.into_iter().map(|(_, t)| t).collect())
    }

    /// Python's C3 linearization over the header's base list, with
    /// `*bases` expanded. Falls back to appending the remaining heads when a
    /// hierarchy is inconsistent, so a cycle cannot borrow a member.
    fn python_c3(&mut self, class: &Arc<KNode>, r: &ResolveRefIn, depth: u32) -> Res<Vec<Arc<KNode>>> {
        if depth > 8 {
            return Ok(vec![class.clone()]);
        }
        let bases = self.python_bases(class, r)?;
        let mut seqs: Vec<Vec<Arc<KNode>>> = Vec::with_capacity(bases.len() + 1);
        for base in &bases {
            seqs.push(self.python_c3(base, r, depth + 1)?);
        }
        seqs.push(bases);
        let mut out = vec![class.clone()];
        loop {
            seqs.retain(|q| !q.is_empty());
            if seqs.is_empty() {
                return Ok(out);
            }
            let head = seqs
                .iter()
                .map(|q| q[0].clone())
                .find(|h| !seqs.iter().any(|q| q[1..].iter().any(|n| n.id == h.id)));
            let Some(head) = head else {
                for q in &seqs {
                    if !out.iter().any(|c| c.id == q[0].id) {
                        out.push(q[0].clone());
                    }
                }
                return Ok(out);
            };
            if !out.iter().any(|c| c.id == head.id) {
                out.push(head.clone());
            }
            for q in seqs.iter_mut() {
                if !q.is_empty() && q[0].id == head.id {
                    q.remove(0);
                }
            }
        }
    }

    /// inheritedThrough: members reached through the declaration's own
    /// resolved extends/implements edges, breadth-first, in header order;
    /// an interface/abstract declaration answers only when nothing concrete
    /// does (upstream 25776014).
    fn inherited_through(
        &mut self,
        ty: &Arc<KNode>,
        by_owner: &HashMap<String, Vec<Arc<KNode>>>,
        name: &str,
        receiver: ReceiverKind,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        // Python reads the member along its C3 order, and each class's *own*
        // member decides: `Adapter(Left, Right)` with `Left(Root)` and
        // `Right(Root)` reaches Right's, though Root is written first in Left's
        // subtree. `python_bases` also expands a header's `*bases`.
        if r.language == "python" {
            for class in self.python_c3(ty, r, 0)?.iter().skip(1) {
                if class.name == name {
                    continue;
                }
                if self.python_class_assigns(class, name)? {
                    return Ok(Some(class.clone()));
                }
                let mut own: Vec<_> = self
                    .nodes_by_qualified_name(&format!("{}::{}", class.qualified_name, name))?
                    .iter()
                    .filter(|n| n.file_path == class.file_path)
                    .cloned()
                    .collect();
                own.retain(|c| {
                    !(receiver == ReceiverKind::Super
                        && (c.is_abstract || class.kind == "interface" || class.kind == "protocol"))
                });
                if let Some(first) = own.first() {
                    return Ok(Some(first.clone()));
                }
            }
            return Ok(None);
        }
        let mut seen: HashSet<String> = HashSet::from([ty.id.clone()]);
        let mut frontier = vec![ty.clone()];
        let mut declared: Option<Arc<KNode>> = None;
        for _depth in 0..4 {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for t in &frontier {
                // Supertypes in the order the header names them.
                let mut supers = self.supertype_nodes(&t.id)?;
                if supers.len() >= 2 {
                    let header = self.header_of(t)?;
                    let at = |s: &Arc<KNode>| -> i64 {
                        name_offsets(&header, &s.name).next().map(|p| p as i64).unwrap_or(i64::MAX)
                    };
                    supers.sort_by_key(|s| at(s));
                }
                for s in supers {
                    if seen.contains(&s.id) {
                        continue;
                    }
                    seen.insert(s.id.clone());
                    next.push(s.clone());
                    if s.name == name {
                        continue;
                    }
                    let abstract_only = s.kind == "interface" || s.kind == "protocol";
                    let mut found: Vec<Arc<KNode>> = by_owner
                        .get(&s.qualified_name)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|c| {
                            c.file_path == s.file_path
                                && !(receiver == ReceiverKind::Super && (c.is_abstract || abstract_only))
                        })
                        .collect();
                    found.sort_by_key(|c| i32::from(!(c.kind == "method" || c.kind == "function")));
                    if found.is_empty() {
                        continue;
                    }
                    if !abstract_only && !found[0].is_abstract {
                        return Ok(Some(found[0].clone()));
                    }
                    if declared.is_none() {
                        declared = Some(found[0].clone());
                    }
                }
            }
            frontier = next;
        }
        let _ = r;
        Ok(declared)
    }

    /// inherited (by name): breadth-first over resolved extends/implements
    /// names from the caller's own type's supertypes.
    fn inherited_by_name(
        &mut self,
        first_supers: &[String],
        type_name: &str,
        by_owner_name: &HashMap<String, Vec<Arc<KNode>>>,
        name: &str,
        receiver: ReceiverKind,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        let mut frontier = vec![type_name.to_string()];
        let mut seen: HashSet<String> = HashSet::from([type_name.to_string()]);
        for depth in 0..4 {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for t in &frontier {
                let supers: Vec<String> = if depth == 0 {
                    first_supers.to_vec()
                } else {
                    self.supertype_names(t, r, false, false)?
                };
                for s in supers {
                    if seen.contains(&s) {
                        continue;
                    }
                    seen.insert(s.clone());
                    next.push(s.clone());
                    if s == name {
                        continue;
                    }
                    let Some(cands) = by_owner_name.get(&s) else {
                        continue;
                    };
                    // Supertypes come back as bare names: a same-named type
                    // of another language is not it.
                    let own_language = self.nodes_by_name(&s)?.iter().any(|n| {
                        MEMBER_OWNER_KINDS.contains(&n.kind.as_str()) && n.language == r.language
                    });
                    let mut kept: Vec<Arc<KNode>> = Vec::new();
                    for c in cands {
                        if own_language && c.language != r.language {
                            continue;
                        }
                        if receiver == ReceiverKind::Super
                            && (c.is_abstract || self.member_in_interface(c)?)
                        {
                            continue;
                        }
                        kept.push(c.clone());
                    }
                    let mut found = kept;
                    found.sort_by_key(|c| i32::from(!(c.kind == "method" || c.kind == "function")));
                    // Overloads share a file; copies across files are a guess
                    // unless one is the caller's.
                    if !found.is_empty() && found.iter().all(|c| c.file_path == found[0].file_path) {
                        return Ok(Some(found[0].clone()));
                    }
                    if !found.is_empty() {
                        return Ok(found.iter().find(|c| c.file_path == r.file_path).cloned());
                    }
                }
            }
            frontier = next;
        }
        Ok(None)
    }

    /// isInterfaceMember: the member's owner resolves to an interface/protocol.
    fn member_in_interface(&mut self, member: &Arc<KNode>) -> Res<bool> {
        let Some(sep) = member.qualified_name.rfind("::") else {
            return Ok(false);
        };
        Ok(self
            .nodes_by_qualified_name(&member.qualified_name[..sep])?
            .iter()
            .any(|n| n.kind == "interface" || n.kind == "protocol"))
    }

    /// A declaration's header — its text up to the body's `{` (upstream
    /// headerOf).
    pub(super) fn header_of(&mut self, decl: &Arc<KNode>) -> Res<String> {
        let mut header = String::new();
        let Some(src) = self.read_file(&decl.file_path) else {
            return Ok(header);
        };
        let start = (decl.start_line - 1) as usize;
        let stop = (decl.start_line as usize + 30).min(src.lines.len());
        for l in start..stop {
            let Some(text) = src.line(l) else { break };
            match text.find('{') {
                Some(b) => {
                    header.push(' ');
                    header.push_str(&text[..b]);
                    break;
                }
                None => {
                    header.push(' ');
                    header.push_str(text);
                }
            }
        }
        Ok(header)
    }

    /// declaresSupertype: the declaration's header names an
    /// extends/implements/with or a C++ `:` base list.
    fn declares_supertype(&mut self, decls: &[Arc<KNode>], simple: &str, r: &ResolveRefIn) -> Res<bool> {
        if simple.starts_with('<') {
            return Ok(true); // an anonymous class always has one
        }
        let Some(header) = self.type_header(decls, r)? else {
            return Ok(true);
        };
        Ok(re!(r"\b(?:extends|implements|with)\b|[\w>)\]]\s*:(?:[^:]|$)").is_match(&header))
    }

    /// Whether `findBestMatch` picked `best` because it is THE definition in
    /// the call site's own file, so the edge deserves a same-file confidence
    /// rather than the directory-proximity one. Proximity measures shared
    /// folders, so a same-file match in a shallow path scored 0.4 like a guess
    /// across unrelated modules. Decisive only when:
    /// - `best` is the ONLY same-file candidate — several are separated by
    ///   line distance, which is not evidence;
    /// - a member of a type, module or namespace belongs to one the caller is
    ///   inside — a bare call does not reach a sibling class's method just
    ///   because it is written in that file. A method's owner is always such a
    ///   scope; another kind's is judged when the file declares it as anything
    ///   but a function; and
    /// - `best` is not the caller itself.
    ///
    /// (name-matcher.ts isDecisiveSameFileMatch, upstream cde4cd74.)
    fn is_decisive_same_file_match(
        &self,
        best: &Arc<KNode>,
        r: &ResolveRefIn,
        candidates: &[Arc<KNode>],
    ) -> Res<bool> {
        if best.file_path != r.file_path || best.id == r.from_node_id {
            return Ok(false);
        }
        if candidates.iter().any(|c| c.id != best.id && c.file_path == r.file_path) {
            return Ok(false);
        }
        let sep = best.qualified_name.rfind("::").unwrap_or(0);
        if sep == 0 {
            return Ok(best.kind != "method");
        }
        let owner = &best.qualified_name[..sep];
        if best.kind != "method" {
            let declared = self.nodes_in_file(&best.file_path)?;
            if declared
                .iter()
                .filter(|n| n.qualified_name == owner)
                .all(|n| n.kind == "function" || n.kind == "method")
            {
                return Ok(true);
            }
        }
        let Some(from) = self.node_by_id(&r.from_node_id)? else {
            return Ok(false);
        };
        Ok(from.qualified_name == owner || from.qualified_name.starts_with(&format!("{owner}::")))
    }

    /// The candidates a PHP `$this->m()` / `self::m()` / `parent::m()` call
    /// allows: methods of the class hierarchy around the call site
    /// (matchByExactName / matchFuzzy, name-matcher.ts).
    ///
    /// A method the hierarchy binds wins over a repository trait admitted only
    /// because an ancestor lies outside the repository; the latter is kept
    /// only when nothing is bound, and `cap_php_unseen_ancestor` then holds
    /// its confidence below the trusted range.
    fn retain_php_self_scope(&mut self, candidates: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let Some(via) = self.php_self_receiver(r)? else { return Ok(candidates) };
        let mut bound: Vec<Arc<KNode>> = Vec::new();
        let mut unseen: Vec<Arc<KNode>> = Vec::new();
        for n in candidates.into_iter() {
            if n.kind != "method" {
                continue;
            }
            match self.php_method_scope(&n, r, via)? {
                php_scope::PhpScope::Bound => bound.push(n),
                php_scope::PhpScope::UnseenAncestor => unseen.push(n),
                php_scope::PhpScope::Out => {}
            }
        }
        Ok(if bound.is_empty() { unseen } else { bound })
    }

    /// A `$this`/`self`/`parent` call settled on a repository trait that no
    /// class in the repository hierarchy uses stays an untrusted guess.
    fn cap_php_unseen_ancestor(&mut self, cand: Option<KCand>, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(mut c) = cand else { return Ok(None) };
        if let Some(via) = self.php_self_receiver(r)? {
            if self.php_method_scope(&c.node, r, via)? == php_scope::PhpScope::UnseenAncestor {
                c.confidence = c.confidence.min(PHP_UNSEEN_ANCESTOR_CONFIDENCE);
            }
        }
        Ok(Some(c))
    }

    /// matchFuzzy (name-matcher.ts).
    pub(super) fn match_fuzzy(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if self.is_unknown_receiver_built_in_call(r) {
            return Ok(None);
        }
        if self.is_bound_to_bare_import(r)? {
            return Ok(None);
        }
        if let Some(form) = self.cpp_bare_call_form(r)? {
            if matches!(form, "qualified" | "explicit-member" | "this-member" | "free-args") {
                return Ok(None);
            }
        }
        let candidates = self.nodes_by_lower_name(&r.reference_name)?;
        // Same bound as match_by_exact_name_uncapped: the lowercase pool is
        // at least as wide, so decline before the retain chain.
        if candidates.len() > GENERATED_NAME_POOL_CAP
            && (r.generated == Some(true)
                || candidates.iter().filter(|n| self.is_gen_path(&n.file_path)).count() * 4
                    >= candidates.len() * 3)
        {
            return Ok(None);
        }
        let type_ref = self.is_dotnet_type_ref(r);
        let rust_bare = is_bare_rust_name(r);
        // Names are case-sensitive in every language but a handful: Rust's
        // `Bytes` is not the method `bytes`, Python's builtin `dir(…)` not a
        // class `Dir`, an imported `type RsbuildConfig` not a local
        // `rsbuildConfig`, a Java `Node` not a `node()`. Only PHP,
        // Pascal/Delphi, CFML, COBOL and VB.NET resolve a name without regard
        // to case, which is what this fallback's lowercase index is for.
        let case_exact = !is_case_insensitive_language(&r.language);
        let mut callable: Vec<Arc<KNode>> = candidates
            .iter()
            .filter(|n| matches!(n.kind.as_str(), "function" | "method" | "class"))
            .filter(|n| !objc_file_local_elsewhere(n, r))
            .filter(|n| !type_ref || can_name_in_type_position(n))
            // `new …MockData()` makes an instance of a type; a method is never what it names.
            .filter(|n| !(r.reference_kind == "instantiates" && n.kind == "method"))
            .filter(|n| !case_exact || n.name == r.reference_name)
            .cloned()
            .collect();
        if rust_bare {
            callable.retain(|n| self.is_rust_name_in_scope(n, r));
        }
        let callable = self.retain_python_java_scope(callable, r)?;
        let callable = self.retain_lang_scope(callable, r)?;
        let callable = self.retain_php_self_scope(callable, r)?;
        let mut callable = self.apply_language_gate(callable, r);
        callable.retain(|n| self.js_declaration_reachable(n, r) != Some(false));
        // A Vue component's own method is `this.m()` inside that component, as
        // in the exact-name arm: a `this.showToast()` from a plugin or mixin
        // is not another component's `showToast`.
        if r.reference_kind == "calls" && is_js_family(&r.language) {
            let mut kept: Vec<Arc<KNode>> = Vec::with_capacity(callable.len());
            for n in callable.into_iter() {
                if !php_scope::is_vue_component_method(&n) || self.is_this_call_in_own_file(&n, r)? {
                    kept.push(n);
                }
            }
            callable = kept;
        }
        prefer_adjacent_implementations(&mut callable, r);
        let gated = callable;
        let same_language: Vec<Arc<KNode>> = gated
            .iter()
            .filter(|n| n.language == r.language)
            .cloned()
            .collect();
        let final_candidates = if same_language.is_empty() {
            gated
        } else {
            same_language
        };
        let final_candidates = self.cpp_specialization_pool(final_candidates, r);
        if final_candidates.len() == 1 {
            let only = final_candidates[0].clone();
            let reachable = self.is_lexically_reachable(&only, r)?
                && self.is_visible_across_files(&only, r)?
                && self.is_cross_file_reachable(&only, r)?;
            let bare_decline = self.is_bare_js_call(r)?
                && (only.kind == "method"
                    || (only.file_path != r.file_path
                        && self.is_locally_bound_js_name(
                            &r.reference_name,
                            &r.file_path,
                            Some(r.line),
                        )?));
            let bare_decline = bare_decline || (only.kind == "method" && (self.is_bare_go_call(r)? || self.is_bare_r_call(r)));
            // Fuzzy is case-insensitive, so it may find the class `View` for `view(…)`.
            let bare_decline = bare_decline || (only.kind != "function" && self.is_bare_php_call(r)?);
            let shadowed = if only.file_path != r.file_path {
                self.is_shadowed_import_name(r)?
            } else {
                self.is_bare_js_call(r)? && self.is_param_shadowed(r)?
            };
            // Fuzzy settles a name one definition carries. A class that shares
            // its exact name with an interface or type alias in another file
            // is not that: a `Locator` type imported from vitest's public API
            // is the interface it declares, not the tester's abstract class.
            // One in the class's own file is a declaration merge, the same
            // symbol.
            let type_twin = only.kind == "class"
                && self.nodes_by_name(&only.name)?.iter().any(|n| {
                    matches!(n.kind.as_str(), "interface" | "type_alias")
                        && n.file_path != only.file_path
                        && same_language_family(&n.language, &only.language)
                });
            let reachable =
                reachable && !bare_decline && !shadowed && !type_twin && self.is_lexically_reachable(&only, r)?;
            if reachable {
                let cross = only.language != r.language;
                return Ok(Some(KCand {
                    node: only,
                    confidence: if cross { 0.3 } else { 0.5 },
                    resolved_by: "fuzzy",
                }));
            }
        }
        Ok(None)
    }

    /// matchReference restricted to the bare-name slice: every strategy
    /// before exact-name keys on a separator a bare name cannot carry, so
    /// the pipeline is exact → fuzzy.
    /// matchReference's leading Erlang arms: `Some(result)` when one owns the
    /// ref (its result, a miss included, is the name match), `None` otherwise.
    /// A `-behaviour(m)` ref, or any ref from an `.app`/`.app.src` resource,
    /// can only name a module. A call carrying its arity (`f/1`) resolves only
    /// to a definition of exactly that arity: the call site's own file, then a
    /// unique one, then the best-ranked — never a sibling arity.
    pub(super) fn match_erlang_reference(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "erlang" {
            return Ok(None);
        }
        if r.reference_kind == "implements" || re!(r"(?i)\.app(?:\.src)?$").is_match(&r.file_path) {
            let modules: Vec<Arc<KNode>> = self
                .nodes_by_name(&r.reference_name)?
                .iter()
                .filter(|n| n.language == "erlang" && n.kind == "namespace")
                .cloned()
                .collect();
            let chosen = prefer_call_site_file(modules, &r.file_path).into_iter().next();
            return Ok(Some(chosen.map(|node| KCand { node, confidence: 0.9, resolved_by: "exact-match" })));
        }
        if r.reference_name.contains("::") || (r.reference_kind != "calls" && r.reference_kind != "references") {
            return Ok(None);
        }
        let Some(m) = re!(r"^(.+)/([0-9]{1,3})$").captures(&r.reference_name) else {
            return Ok(None);
        };
        let arity_tail = format!("/{}", &m[2]);
        let mut candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(&m[1])?
            .iter()
            .filter(|n| n.language == "erlang" && n.kind == "function" && n.qualified_name.ends_with(&arity_tail))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(Some(None));
        }
        if let Some(same) = candidates.iter().find(|n| n.file_path == r.file_path) {
            return Ok(Some(Some(KCand { node: same.clone(), confidence: 0.95, resolved_by: "exact-match" })));
        }
        if let Some(source) = self.read_file(&r.file_path) {
            for imported in re!(r"(?m)^[ \t]*-import\s*\(\s*'?([A-Za-z_][\w@]*)'?\s*,\s*\[([^\]]*)\]\s*\)\s*\.").captures_iter(source.text()) {
                let pattern = Self::cached_regex(&format!(r"(?:^|[\s,])'?{}'?\s*/\s*{}\b",regex::escape(&m[1]),&m[2]))?;
                if pattern.is_match(&imported[2]) {
                    let selected = candidates.iter().find(|n| n.qualified_name.starts_with(&format!("{}::",&imported[1]))).cloned();
                    return Ok(Some(selected.map(|node| KCand{node,confidence:0.9,resolved_by:"exact-match"})));
                }
            }
        }
        if !r.file_path.ends_with(".hrl") {
            candidates.retain(|n| n.file_path.ends_with(".hrl"));
            if candidates.is_empty() { return Ok(Some(None)); }
        }
        if candidates.len() == 1 {
            return Ok(Some(Some(KCand { node: candidates[0].clone(), confidence: 0.8, resolved_by: "exact-match" })));
        }
        Ok(Some(self.find_best_match(r, &candidates).map(|best| {
            let proximity = compute_path_proximity(&r.file_path, &best.file_path);
            KCand { node: best, confidence: if proximity >= 30 { 0.7 } else { 0.4 }, resolved_by: "exact-match" }
        })))
    }

    /// matchReference's ArkTS arm: `.width` / `.fancy` attribute calls name
    /// only `@Extend`/`@Styles`/`@AnimatableExtend`/`@Builder` helpers — never
    /// the name-match fallthrough — and only a single such helper.
    pub(super) fn match_arkts_attribute(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let base = &r.reference_name[1..];
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(base)?
            .iter()
            .filter(|n| {
                n.language == "arkts"
                    && n.kind == "function"
                    && n.decorators.as_ref().is_some_and(|ds| {
                        ds.iter().any(|d| matches!(d.as_str(), "Extend" | "Styles" | "AnimatableExtend" | "Builder"))
                    })
            })
            .cloned()
            .collect();
        // preferCallSiteFile reorders without filtering: more than one
        // helper is ambiguous and drops the ref.
        if candidates.len() != 1 {
            return Ok(None);
        }
        Ok(Some(KCand { node: candidates[0].clone(), confidence: 0.85, resolved_by: "exact-match" }))
    }

    /// An assertion owns its method chain, including terminal misses.
    pub(super) fn go_assertion_call(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "go" || r.reference_kind != "calls" || r.reference_name.contains('.') { return Ok(None); }
        let Some(file) = self.read_file(&r.file_path) else { return Ok(None); };
        let Some(tree) = self.parsed_tree(&file, r) else { return Ok(None); };
        let mut node = super::iteration::descendant_for_position(tree.root_node(), file.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        let mut call = None;
        loop {
            if node.kind() == "call_expression" {
                if let Some(function) = node.child_by_field_name("function") {
                    if let Some(field) = function.child_by_field_name("field") {
                        if function.kind() == "selector_expression" && file.text()[field.byte_range()] == r.reference_name {
                            call = Some(function); break;
                        }
                    }
                }
            }
            let Some(parent) = node.parent() else { break; };
            if matches!(parent.kind(), "argument_list" | "block" | "source_file") { break; }
            node = parent;
        }
        let Some(mut selector) = call else { return Ok(None); };
        let mut methods = Vec::new();
        let raw = loop {
            let (Some(member), Some(mut operand)) = (selector.child_by_field_name("field"), selector.child_by_field_name("operand")) else { return Ok(None); };
            methods.push(file.text()[member.byte_range()].to_string());
            while operand.kind() == "parenthesized_expression" {
                let Some(inner) = operand.named_child(0) else { return Ok(None); }; operand = inner;
            }
            if operand.kind() == "type_assertion_expression" {
                let Some(ty) = operand.child_by_field_name("type") else { return Ok(Some(None)); };
                break file.text()[ty.byte_range()].to_string();
            }
            if operand.kind() != "call_expression" { return Ok(None); }
            let Some(function) = operand.child_by_field_name("function") else { return Ok(None); };
            if function.kind() != "selector_expression" { return Ok(None); }
            selector = function;
        };
        let Some((ty, own_dir)) = self.go_type_package(&raw, &r.file_path)? else { return Ok(Some(None)); };
        let mut dirs = vec![own_dir];
        if !raw.contains('.') {
            for import in self.import_mappings(&r.file_path)?.iter().filter(|i| i.local_name == ".") {
                if let Some(dir) = self.go_package_dir(&import.source, &r.file_path) { dirs.push(dir); }
            }
        }
        let declarations = self.nodes_by_name(&ty)?;
        let dirs: HashSet<_> = dirs.into_iter().filter(|dir| declarations.iter().any(|n|
            n.language == "go" && matches!(n.kind.as_str(), "struct" | "interface" | "type_alias")
                && pos_dirname(&n.file_path) == dir)).collect();
        if dirs.len() != 1 { return Ok(Some(None)); }
        let dir = dirs.into_iter().next().unwrap();
        methods.reverse();
        let mut result = self.go_method_in_package(&ty, &methods[0], &dir, r)?;
        for method in methods.iter().skip(1) {
            let Some(previous) = result else { return Ok(Some(None)); };
            let Some(return_type) = previous.node.return_type.as_deref() else { return Ok(Some(None)); };
            result = self.go_method_on_declared_type(return_type, &previous.node.file_path, method, r)?;
        }
        Ok(Some(result))
    }

    fn go_collapsed_chain_call(&mut self, r: &ResolveRefIn) -> Option<Vec<String>> {
        if r.language != "go" || r.reference_kind != "calls" || r.reference_name.contains('.') { return None; }
        let file = self.read_file(&r.file_path)?;
        let tree = self.parsed_tree(&file, r)?;
        let mut node = super::iteration::descendant_for_position(tree.root_node(), file.text(),
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        loop {
            if node.kind() == "call_expression" {
                if let Some(function) = node.child_by_field_name("function") {
                    if function.kind() == "selector_expression" {
                        if let (Some(field), Some(receiver)) = (function.child_by_field_name("field"), function.child_by_field_name("operand")) {
                            if file.text()[field.byte_range()] == r.reference_name
                                && receiver.kind() == "call_expression" {
                                let mut inner = receiver;
                                let mut chain = Vec::new();
                                loop {
                                    let Some(callee) = inner.child_by_field_name("function") else { return Some(Vec::new()); };
                                    if callee.kind() != "selector_expression" { return Some(Vec::new()); }
                                    let (Some(member), Some(operand)) = (callee.child_by_field_name("field"), callee.child_by_field_name("operand")) else { return Some(Vec::new()); };
                                    chain.push(file.text()[member.byte_range()].to_string());
                                    if operand.kind() == "identifier" {
                                        chain.push(file.text()[operand.byte_range()].to_string());
                                        chain.reverse();
                                        return Some(chain);
                                    }
                                    if operand.kind() != "call_expression" { return Some(Vec::new()); }
                                    inner = operand;
                                }
                            }
                        }
                    }
                }
            }
            let parent = node.parent()?;
            if matches!(parent.kind(), "argument_list" | "block" | "source_file") { return None; }
            node = parent;
        }
    }

    pub(super) fn match_reference_bare(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        // A collapsed Go expression call has no proven receiver. Name-only
        // fallback must not borrow a method from an unrelated type.
        if let Some(chain) = self.go_collapsed_chain_call(r) {
            // Every fluent step must have a declared result in its proven
            // receiver's package. Unproven steps cannot borrow a name match.
            if let [receiver, method, rest @ ..] = chain.as_slice() {
                let mut inner = match self.infer_local_receiver_type(receiver, r, true)? {
                    Some(raw) => self.go_method_on_declared_type(&raw, &r.file_path, method, r)?,
                    None => self.match_go_factory_receiver(receiver, method, r)?,
                };
                for method in rest.iter().chain(std::iter::once(&r.reference_name)) {
                    let Some(proven) = inner else { return Ok(None); };
                    let Some(result) = proven.node.return_type.as_deref() else { return Ok(None); };
                    inner = self.go_method_on_declared_type(result, &proven.node.file_path, method, r)?;
                }
                return Ok(inner);
            }
            return Ok(None);
        }
        if let Some(node) = self.js_typed_destructured_member(r)? {
            return Ok(Some(KCand { node, confidence: 0.9, resolved_by: "exact-match" }));
        }
        if self.java_outside_import(r)? { return Ok(None); }
        if let Some(alias) = self.lua_alias_target(r)? {
            return Ok(alias.map(|node| { let resolved_by = if node.file_path == r.file_path { "exact-match" } else { "import" }; KCand { node, confidence: 0.9, resolved_by } }));
        }
        if matches!(r.language.as_str(), "lua" | "luau") && r.reference_kind == "calls"
            && super::lua_alias::is_global(&r.reference_name)
            && !self.nodes_in_file(&r.file_path)?.iter().any(|n| n.name == r.reference_name && n.kind == "function") { return Ok(None); }
        if let Some(alias) = self.csharp_alias_type_target(r)? { return Ok(Some(alias)); }
        match self.match_collapsed_object_call(r)? {
            CollapsedCall::Hit(c) => return Ok(Some(c)),
            CollapsedCall::Declined => return Ok(None),
            CollapsedCall::NotOurs => {}
        }
        if let Some(c) = self.match_by_exact_name(r)? {
            return Ok(Some(c));
        }
        self.match_fuzzy(r)
    }

    /// resolveObjectLiteralMember (name-matcher.ts): an imported object
    /// literal used as a namespace — find the member by containment.
    pub(super) fn resolve_object_literal_member(
        &mut self,
        container: &KNode,
        member: &str,
        r: &ResolveRefIn,
        confidence: f64,
        resolved_by: &'static str,
    ) -> Res<Option<KCand>> {
        if container.kind != "constant" && container.kind != "variable" {
            return Ok(None);
        }
        if !is_object_literal_language(&container.language) {
            return Ok(None);
        }
        if !same_language_family(&container.language, &r.language) {
            return Ok(None);
        }
        let in_file = self.nodes_in_file(&container.file_path)?;
        let callable =
            |n: &KNode| n.kind == "function" || n.kind == "method";
        let accepts = |n: &KNode| {
            if r.reference_kind == "calls" {
                callable(n)
            } else {
                callable(n)
                    || n.kind == "property"
                    || n.kind == "variable"
                    || n.kind == "constant"
            }
        };
        // rangeWithin / sameRange (name-matcher.ts).
        let range_within = |inner: &KNode, outer: &KNode| {
            !(inner.start_line < outer.start_line
                || inner.end_line > outer.end_line
                || (inner.start_line == outer.start_line
                    && inner.start_column < outer.start_column)
                || (inner.end_line == outer.end_line && inner.end_column > outer.end_column))
        };
        let same_range = |a: &KNode, b: &KNode| {
            a.start_line == b.start_line
                && a.start_column == b.start_column
                && a.end_line == b.end_line
                && a.end_column == b.end_column
        };
        let inside: Vec<Arc<KNode>> = in_file
            .iter()
            .filter(|n| n.id != container.id && range_within(n, container))
            .cloned()
            .collect();
        let mut candidates: Vec<Arc<KNode>> = inside
            .iter()
            .filter(|n| n.name == member && accepts(n))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        // Only the literal's own winning property counts; one that names a
        // binding resolves through resolve_object_literal_binding (#1932).
        match self.object_literal_property(container, member) {
            LiteralProperty::Absent | LiteralProperty::Found { binding: Some(_), .. } => return Ok(None),
            LiteralProperty::Found { start, end, binding: None } => {
                candidates.retain(|c| self.literal_property_contains(container, c, start, end));
                if candidates.is_empty() {
                    return Ok(None);
                }
            }
            LiteralProperty::Unreadable => {}
        }
        // Drop members nested inside another callable's body in the literal.
        let bodies: Vec<&Arc<KNode>> = inside.iter().filter(|n| callable(n)).collect();
        candidates.retain(|c| {
            !bodies
                .iter()
                .any(|b| b.id != c.id && !same_range(b, c) && range_within(c, b))
        });
        if candidates.is_empty() {
            return Ok(None);
        }
        candidates.sort_by(|a, b| {
            let ca = if callable(a) { 0 } else { 1 };
            let cb = if callable(b) { 0 } else { 1 };
            ca.cmp(&cb)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.start_column.cmp(&b.start_column))
        });
        Ok(Some(KCand {
            node: candidates[0].clone(),
            confidence,
            resolved_by,
        }))
    }

    /// matchByQualifiedName (name-matcher.ts) — exact `qualifiedName` lookup,
    /// then the last-segment suffix match. Erlang's arity arms are dead (not
    /// a migrated language).
    pub(super) fn match_by_qualified_name(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        if !r.reference_name.contains("::") && !r.reference_name.contains('.') {
            return Ok(None);
        }
        let cpp_visible = if matches!(r.language.as_str(), "c" | "cpp") {
            Some(self.namespace_visible_files(&r.file_path, &r.language)?)
        } else { None };
        // A `calls` ref never resolves to a yaml/properties config key (#1180).
        let keep_for_ref = |nodes: &[Arc<KNode>]| -> Vec<Arc<KNode>> {
            nodes
                .iter()
                .filter(|n| {
                    cpp_visible.as_ref().is_none_or(|files| !re!(r"(?i)\.(?:h|hh|hpp|hxx)$").is_match(&n.file_path) || files.contains(&n.file_path))
                        && !(r.reference_kind == "imports" && n.kind == "import")
                        && (r.reference_kind != "calls"
                        || !(n.kind == "constant"
                            && (n.language == "yaml" || n.language == "properties")))
                })
                .cloned()
                .collect()
        };

        if matches!(r.language.as_str(), "c" | "cpp") && r.reference_name.starts_with("::") {
            let spelling = r.reference_name.trim_start_matches("::");
            let mut exact = Vec::new();
            for node in keep_for_ref(&self.nodes_by_qualified_name(spelling)?) {
                if !matches!(node.language.as_str(), "c" | "cpp") { continue; }
                if self.namespace_frames(&node.file_path)?.iter().any(|(start, end, _)|
                    *start <= node.start_line && node.start_line <= *end) { continue; }
                exact.push(node);
            }
            let exact = self.cpp_specialization_pool(exact, r);
            let exact = prefer_call_site_file(exact, &r.file_path);
            let Some(first) = exact.first().filter(|node| exact.len() == 1 || node.file_path == r.file_path || cpp_overload_set(&exact)).cloned() else { return Ok(None) };
            let node = self.cpp_function_overload(first, &exact, r)?;
            return Ok(Some(KCand { node, confidence: 0.95, resolved_by: "qualified-name" }));
        }

        // Relative C++ qualification searches the caller's enclosing scopes
        // before an identically written name in another namespace.
        if matches!(r.language.as_str(), "c" | "cpp") && r.reference_name.contains("::") {
            let visible = self.namespace_visible_files(&r.file_path, &r.language)?;
            let mut scope = self.node_by_id(&r.from_node_id)?.map(|n| n.qualified_name.clone()).unwrap_or_default();
            while let Some((parent, _)) = scope.rsplit_once("::") {
                scope = parent.to_string();
                let qualified = format!("{scope}::{}", r.reference_name);
                let named = keep_for_ref(&self.nodes_by_qualified_name(&qualified)?);
                let candidates: Vec<_> = named.into_iter().filter(|n| matches!(n.language.as_str(), "c" | "cpp") && visible.contains(&n.file_path)).collect();
                if candidates.is_empty() { continue; }
                let candidates = self.cpp_specialization_pool(candidates, r);
                let candidates = prefer_call_site_file(candidates, &r.file_path);
                let Some(first) = candidates.first().filter(|n| candidates.len() == 1 || n.file_path == r.file_path || cpp_overload_set(&candidates)).cloned() else { return Ok(None) };
                let node = self.cpp_function_overload(first, &candidates, r)?;
                return Ok(Some(KCand { node, confidence: 0.95, resolved_by: "qualified-name" }));
            }
        }

        let candidates = if r.language == "csharp" && r.reference_kind == "instantiates" {
            // Relative qualification searches enclosing namespaces before the global namespace.
            let absolute = r.reference_name.strip_prefix("global::");
            let spelling = absolute.unwrap_or(&r.reference_name);
            let leaf = spelling.rsplit([':', '.']).next().unwrap_or("");
            let nodes = self.nodes_by_name(leaf)?;
            let mut namespace = if absolute.is_some() { String::new() }
                else { self.csharp_namespaces_at(r).unwrap_or_default().join(".") };
            let mut found = Vec::new();
            loop {
                let qualified = if namespace.is_empty() { spelling.to_string() }
                    else { format!("{namespace}.{spelling}") };
                found.extend(nodes.iter().filter(|n| n.language == "csharp"
                    && matches!(n.kind.as_str(), "class" | "struct" | "record")
                    && n.qualified_name.replace("::", ".") == qualified).cloned());
                if !found.is_empty() || namespace.is_empty() { break; }
                namespace = namespace.rsplit_once('.').map_or("", |(parent, _)| parent).to_string();
            }
            found
        } else { keep_for_ref(&self.nodes_by_qualified_name(&r.reference_name)?) };
        let candidates = self.cpp_specialization_pool(candidates, r);
        if candidates.len() == 1 {
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: 0.95,
                resolved_by: "qualified-name",
            }));
        }
        if candidates.len() > 1 {
            let ordered = prefer_call_site_file(candidates, &r.file_path);
            if ordered[0].file_path == r.file_path {
                return Ok(Some(KCand {
                    node: self.cpp_function_overload(ordered[0].clone(), &ordered, r)?,
                    confidence: 0.95,
                    resolved_by: "qualified-name",
                }));
            }
        }

        // Erlang (#1610): every function's qualified name carries its arity
        // (`mod::f/2`). A ref WITH an arity that missed the exact lookup names
        // an undefined arity — never the partial match (its last segment would
        // be the digits) and never a sibling arity. An arity-less ref resolves
        // only when the module defines exactly one arity of the function.
        if r.language == "erlang" && r.reference_name.contains("::") {
            if re!(r"/[0-9]{1,3}$").is_match(&r.reference_name) {
                return Ok(None);
            }
            let base = &r.reference_name[r.reference_name.rfind("::").unwrap() + 2..];
            let prefix = format!("{}/", r.reference_name);
            let arity: Vec<Arc<KNode>> = keep_for_ref(&self.nodes_by_name(base)?)
                .into_iter()
                .filter(|n| {
                    n.qualified_name
                        .strip_prefix(&prefix)
                        .is_some_and(|tail| re!(r"^[0-9]{1,3}$").is_match(tail))
                })
                .collect();
            return Ok((arity.len() == 1).then(|| KCand {
                node: arity[0].clone(),
                confidence: 0.85,
                resolved_by: "qualified-name",
            }));
        }

        // Partial match — the last `:`/`.` segment, then the suffix filter.
        let last_name = r
            .reference_name
            .rsplit([':', '.'])
            .next()
            .unwrap_or("");
        if !last_name.is_empty() {
            let partial = keep_for_ref(&self.nodes_by_name(last_name)?)
                .into_iter()
                .filter(|n| if matches!(r.language.as_str(), "c" | "cpp") {
                    n.qualified_name == r.reference_name || n.qualified_name.ends_with(&format!("::{}", r.reference_name))
                } else { n.qualified_name.ends_with(&r.reference_name) })
                .collect();
            let partial = self.cpp_specialization_pool(partial, r);
            let chosen = prefer_call_site_file(partial, &r.file_path);
            if let Some(first) = chosen.first().cloned() {
                return Ok(Some(KCand {
                    node: self.cpp_function_overload(first, &chosen, r)?,
                    confidence: 0.85,
                    resolved_by: "qualified-name",
                }));
            }
        }
        Ok(None)
    }


    /// matchFunctionRef, bare arm (name-matcher.ts): name-exact
    /// function/method nodes in the ref's language family — plus Python
    /// classes — excluding the origin node; JS/TS/ArkTS/C++/Python/PHP match
    /// functions only (a bare identifier there is never a method value).
    /// Same-file wins by earliest line; cross-file is unique-or-drop.
    pub(super) fn match_function_ref_bare(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let bare_fn_only = matches!(
            r.language.as_str(),
            "typescript" | "tsx" | "javascript" | "jsx" | "arkts" | "cpp" | "python" | "php"
        );
        let bare_class_ok = r.language == "python";
        let candidates: Vec<Arc<KNode>> = self
            .nodes_by_name(&r.reference_name)?
            .iter()
            .filter(|n| {
                (n.kind == "function"
                    || (!bare_fn_only && n.kind == "method")
                    || (bare_class_ok && n.kind == "class"))
                    && same_language_family(&n.language, &r.language)
                    && n.id != r.from_node_id
            })
            .cloned()
            .collect();
        let named_count = candidates.len();
        let mut reachable = Vec::new();
        for candidate in candidates { if self.is_lexically_reachable(&candidate,r)? { reachable.push(candidate); } }
        let candidates = reachable;
        if r.language == "python" && self.python_locally_bound(&r.reference_name,r)?
            && !candidates.iter().any(|n| self.python_fixture_reachable(n,&r.file_path)) { return Ok(None); }
        let mut local = Vec::new();
        for candidate in candidates { if !self.outside_js_local(&candidate,r)? { local.push(candidate); } }
        let mut candidates = local;
        if candidates.is_empty() {
            return Ok(None);
        }
        // Swift implicit-self: a bare identifier names a method only of the
        // enclosing type; same-named methods elsewhere are parameter
        // collisions. Free functions are unaffected; top-level code has no
        // implicit self, so method targets drop entirely there.
        if r.language == "swift" && candidates.iter().any(|n| n.kind == "method") {
            let class_prefix = match self.node_by_id(&r.from_node_id)? {
                Some(from) => match from.qualified_name.rfind("::") {
                    Some(sep) if sep > 0 => Some(from.qualified_name[..sep].to_string()),
                    _ => None,
                },
                None => None,
            };
            candidates.retain(|n| {
                if n.kind != "method" {
                    return true;
                }
                let Some(cp) = &class_prefix else { return false };
                match n.qualified_name.rfind("::") {
                    Some(msep) if msep > 0 => {
                        let mp = &n.qualified_name[..msep];
                        mp == cp.as_str()
                            || mp.ends_with(&format!("::{cp}"))
                            || cp.ends_with(&format!("::{mp}"))
                    }
                    _ => false,
                }
            });
            if candidates.is_empty() {
                return Ok(None);
            }
        }
        // Same-file definition wins; same-name overloads in one file are the
        // same conceptual symbol — first by position for determinism
        // (min_by_key keeps the first minimum, matching TS's `<=` reduce).
        let same_file: Vec<Arc<KNode>> = candidates
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        if !same_file.is_empty() {
            // Swift: several same-named METHODS in one file are an overload
            // family — a bare identifier is a same-named parameter, not a
            // method value. A single method still resolves.
            if r.language == "swift"
                && same_file.len() > 1
                && same_file.iter().all(|n| n.kind == "method")
            {
                return Ok(None);
            }
            let target = same_file.iter().min_by_key(|n| n.start_line).unwrap().clone();
            return Ok(Some(KCand {
                node: target,
                confidence: if same_file.len() == 1 { 0.95 } else { 0.9 },
                resolved_by: "function-ref",
            }));
        }
        // Cross-file: only an unambiguous match resolves.
        // Never a name the file imports from a package outside the repository,
        // which no project function is: outline's `styled(Link)`, with `Link`
        // from react-router-dom, is not an email template's `Link`.
        if candidates.len() == 1 && (named_count == 1 || (r.language == "python" && self.import_mappings(&r.file_path)?.iter().any(|m| m.local_name == r.reference_name)))
            && !(is_js_family(&r.language) && self.is_bound_to_bare_import(r)?)
            // Nor a name a project import binds to something else.
            && !(is_js_family(&r.language) && self.import_binds_other(&candidates[0], r)?)
        {
            return Ok(Some(KCand {
                node: candidates[0].clone(),
                confidence: 0.8,
                resolved_by: "function-ref",
            }));
        }
        Ok(None)
    }

    /// importBindsOther (name-matcher.ts): does a project import bind `r`'s
    /// name to something other than `candidate`, a function of that name in
    /// another file? A namespace import is the module object, and an asset
    /// import is what the bundler makes of the file: topcoder's `import TCO21
    /// from './themes/TCO21.scss'` is no `TCO21()` route. An import the import
    /// resolver binds to a constant, class or component names that: outline's
    /// `isCloudHosted` constant is not the server's `isCloudHosted()`. Two
    /// bindings still lead to the namesake: a wrapper declared beside it
    /// (`export default styled(Disclosure)`), and a lazy loader (signoz's
    /// `Loadable(() => import('pages/Home'))`), which loads the page it is named
    /// after. An import the resolver can't follow at all says nothing.
    fn import_binds_other(&mut self, candidate: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if !re!(r"^[A-Za-z_$][A-Za-z0-9_$]*$").is_match(&r.reference_name) {
            return Ok(false);
        }
        let Some(binding) = self.import_mappings(&r.file_path)?.iter().find(|m| m.local_name == r.reference_name).cloned() else {
            return Ok(false);
        };
        let source = binding.source.split(['?', '#']).next().unwrap_or("");
        if binding.is_namespace || re!(ASSET_MODULE).is_match(source) {
            return Ok(true);
        }
        let Some(bound) = self.resolve_via_import(r)? else { return Ok(false) };
        let node = bound.node;
        if node.kind == "function" || node.kind == "method" || node.file_path == candidate.file_path {
            return Ok(false);
        }
        let Some(file) = self.read_file(&node.file_path) else { return Ok(true) };
        let from = (node.start_line.max(1) - 1) as usize;
        let to = node.end_line.min(node.start_line + 15).max(0) as usize;
        let head = (from..to).filter_map(|i| file.get(i).map(|l| l.as_str())).collect::<Vec<_>>().join("\n");
        Ok(!re!(r"(?-u:\b)import\s*\(").is_match(&head))
    }

    /// matchFunctionRef's `::` member-pointer arm (name-matcher.ts): an
    /// explicit `Cls::member` shape (`&Widget::on_click` emitted as
    /// `Widget::on_click`) resolves the member ON THAT SCOPE — exempt from
    /// bareFnOnly, origin excluded, qualified-name equality or `::`-suffix.
    /// Same-file pool wins by earliest line @0.9; cross-file unique-or-drop.
    pub(super) fn match_function_ref_scoped(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(sep) = r.reference_name.rfind("::") else {
            return Ok(None);
        };
        let member = &r.reference_name[sep + 2..];
        let suffix = format!("::{}", r.reference_name);
        let scoped: Vec<Arc<KNode>> = self
            .nodes_by_name(member)?
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "function" | "method")
                    && same_language_family(&n.language, &r.language)
                    && n.id != r.from_node_id
                    && (n.qualified_name == r.reference_name
                        || n.qualified_name.ends_with(&suffix))
            })
            .cloned()
            .collect();
        if scoped.is_empty() {
            return Ok(None);
        }
        let same_file: Vec<Arc<KNode>> = scoped
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        if same_file.is_empty() && scoped.len() > 1 {
            return Ok(None);
        }
        let pool = if same_file.is_empty() { scoped } else { same_file };
        // `<=` reduce keeps the first minimum — min_by_key does the same.
        let target = pool.iter().min_by_key(|n| n.start_line).unwrap().clone();
        Ok(Some(KCand {
            node: target,
            confidence: 0.9,
            resolved_by: "function-ref",
        }))
    }

}

/// JS `string.slice(i)` over UTF-16 code units — `column` values are
/// extraction offsets consumed as JS string indices, so replicate that
/// indexing (round a mid-surrogate boundary up to the next char).
pub(super) fn js_slice(s: &str, start: usize) -> &str {
    &s[js_unit_to_byte(s, start)..]
}

/// The spelling of `r`'s name at its column when the source qualified it
/// (`Other::Sub` / `Other\Sub` / `\Sub` / `::Sub` for a ref named `Sub`):
/// the extractors keep only the last segment. None when the name is bare
/// there, or the site can't be read.
pub(super) fn qualified_spelling(lines: &[String], r: &ResolveRefIn) -> Option<String> {
    let line = lines.get((r.line - 1).max(0) as usize)?;
    let text = js_slice(line, r.column.max(0) as usize);
    let m = re!(r"^(?:\\|::)?(?:[A-Za-z_][A-Za-z0-9_]*(?:\\|::))*[A-Za-z_][A-Za-z0-9_]*").find(text)?;
    let spelled = m.as_str();
    let last = spelled.rsplit(['\\', ':']).next().unwrap_or("");
    (last == r.reference_name && spelled.len() > last.len()).then(|| spelled.to_string())
}

/// JS `string.slice(0, i)` over UTF-16 code units.
pub(super) fn js_prefix(s: &str, end: usize) -> &str {
    &s[..js_unit_to_byte(s, end)]
}

/// UTF-16 unit offset → byte offset (clamped to the next char boundary).
pub(super) fn js_unit_to_byte(s: &str, units: usize) -> usize {
    // ASCII up to the offset: one byte per unit. A column far into a minified
    // one-line file would otherwise walk every char before it, per lookup.
    let reach = units.min(s.len());
    if s.as_bytes()[..reach].is_ascii() {
        return reach;
    }
    // Non-ASCII before the offset: count UTF-16 units a block at a time (a
    // char adds one unit at its lead byte, two for a 4-byte char) and walk
    // chars only inside the block that holds the offset. Every lookup on a
    // long minified line with one non-ASCII char otherwise walks all of them.
    const BLOCK: usize = 64;
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    let mut seen = 0usize;
    while pos + BLOCK <= bytes.len() {
        // u8 sums (at most 64) vectorize; a usize sum of the same does not.
        let block = &bytes[pos..pos + BLOCK];
        let leads: u8 = block.iter().map(|&b| u8::from(b as i8 >= -0x40)).sum();
        let wide: u8 = block.iter().map(|&b| u8::from(b >= 0xF0)).sum();
        let block_units = usize::from(leads) + usize::from(wide);
        if seen + block_units >= units {
            break;
        }
        seen += block_units;
        pos += BLOCK;
    }
    // The block may start inside a char whose lead byte was counted above.
    while pos < bytes.len() && bytes[pos] & 0xC0 == 0x80 {
        pos += 1;
    }
    for (byte_idx, ch) in s[pos..].char_indices() {
        if seen >= units {
            return pos + byte_idx;
        }
        seen += ch.len_utf16();
    }
    s.len()
}

/// UTF-16 length — the `.length` JS sees.
pub(super) fn utf16_len(s: &str) -> usize {
    // One unit per char (every non-continuation byte), two for a 4-byte one.
    s.bytes().map(|b| usize::from(b & 0xC0 != 0x80) + usize::from(b >= 0xF0)).sum()
}

/// `utf16_len(s) > limit`, settled from the byte length when it can be: a
/// char is 1–3 bytes per UTF-16 unit.
pub(super) fn utf16_len_exceeds(s: &str, limit: usize) -> bool {
    s.len() > limit && (s.len() > 3 * limit || utf16_len(s) > limit)
}

/// rustModuleDir (name-matcher.ts).
pub(super) fn rust_module_dir(file_path: &str) -> String {
    let base = pos_basename(file_path);
    let dir = pos_dirname(file_path);
    if base == "mod.rs" || base == "lib.rs" || base == "main.rs" {
        return dir.to_string();
    }
    pos_join(dir, base.strip_suffix(".rs").unwrap_or(base))
}

/// The `= require("….json")` signature guard (isCrossFileReachable).
/// Hand-rolled because the JS pattern uses a backreference.
pub(super) fn is_json_require_signature(sig: &str) -> bool {
    // ^=\s*require\s*\(\s*(['"])[^'"]+\.json\1\s*\)\s*;?\s*$
    let s = sig.trim();
    let Some(s) = s.strip_prefix('=') else { return false };
    let s = s.trim_start();
    let Some(s) = s.strip_prefix("require") else { return false };
    let s = s.trim_start();
    let Some(s) = s.strip_prefix('(') else { return false };
    let s = s.trim_start();
    let Some(q) = s.chars().next() else { return false };
    if q != '\'' && q != '"' {
        return false;
    }
    let Some(end) = s[1..].find(q) else { return false };
    let content = &s[1..1 + end];
    // `[^'"]+\.json` — at least one non-quote char, then literal `.json`.
    if content.len() <= ".json".len()
        || !content.ends_with(".json")
        || content.contains(['\'', '"'])
    {
        return false;
    }
    let s = s[1 + end + 1..].trim_start();
    let Some(s) = s.strip_prefix(')') else { return false };
    let s = s.trim_start();
    let s = s.strip_prefix(';').unwrap_or(s);
    s.trim().is_empty()
}

/// enclosingTypePrefix + callableOnType + applyCppCallSiteForm
/// (name-matcher.ts).
pub(super) fn enclosing_type_prefix(node: Option<&KNode>) -> Option<String> {
    let node = node?;
    let sep = node.qualified_name.rfind("::")?;
    if sep == 0 {
        return None;
    }
    Some(node.qualified_name[..sep].to_string())
}

pub(super) fn callable_on_type(candidate: &KNode, type_prefix: &str) -> bool {
    if candidate.kind != "method" && candidate.kind != "function" {
        return false;
    }
    let qn = &candidate.qualified_name;
    if !qn.contains("::") {
        return false;
    }
    let want = format!("{}::{}", type_prefix, candidate.name);
    *qn == want || qn.ends_with(&format!("::{}", want)) || want.ends_with(&format!("::{}", qn))
}

/// pathProximityFromDirs + computePathProximity (name-matcher.ts).
pub(super) fn path_proximity_from_dirs(dir1: &[String], file_path2: &str) -> i64 {
    (shared_dir_prefix(dir1, file_path2) as i64 * 15).min(80)
}

pub(super) fn compute_path_proximity(file_path1: &str, file_path2: &str) -> i64 {
    let mut dir1: Vec<String> = file_path1.split('/').map(|s| s.to_string()).collect();
    dir1.pop();
    path_proximity_from_dirs(&dir1, file_path2)
}

/// MEMBER_OWNER_KINDS (name-matcher.ts): node kinds a member scope can be.
const MEMBER_OWNER_KINDS: [&str; 8] =
    ["class", "struct", "interface", "trait", "protocol", "enum", "union", "module"];

/// callReceiverKind's answer: how the call site names its receiver.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiverKind {
    Bare,
    Slf,
    Super,
}

/// sharedDirDepth (name-matcher.ts): leading directory segments two file
/// paths share, uncapped.
fn shared_dir_depth(file_path1: &str, file_path2: &str) -> usize {
    let mut a: Vec<&str> = file_path1.split('/').collect();
    let mut b: Vec<&str> = file_path2.split('/').collect();
    a.pop();
    b.pop();
    let mut shared = 0;
    while shared < a.len() && shared < b.len() && a[shared] == b[shared] {
        shared += 1;
    }
    shared
}

/// closestCopy (name-matcher.ts, upstream 81559dcc): when every candidate is
/// a copy of the same symbol — same qualified name, kind and file basename,
/// as when a module is duplicated under another source root — return the copy
/// whose file shares the longest directory prefix with the reference. None
/// when the candidates are not all copies or no copy is uniquely closest.
fn closest_copy(r: &ResolveRefIn, candidates: &[Arc<KNode>]) -> Option<Arc<KNode>> {
    if candidates.len() < 2 {
        return None;
    }
    let first = &candidates[0];
    let base = first.file_path.rsplit('/').next().unwrap_or("");
    let mut best: Option<Arc<KNode>> = None;
    let mut best_depth = 0usize;
    let mut have = false;
    let mut unique = false;
    for c in candidates {
        if c.qualified_name != first.qualified_name
            || c.kind != first.kind
            || c.file_path.rsplit('/').next().unwrap_or("") != base
        {
            return None;
        }
        let depth = shared_dir_depth(&r.file_path, &c.file_path);
        if !have || depth > best_depth {
            best_depth = depth;
            best = Some(c.clone());
            have = true;
            unique = true;
        } else if depth == best_depth {
            unique = false;
        }
    }
    if unique {
        best
    } else {
        None
    }
}

/// The file's lines with `//` and one-line `/* */` comments removed, rejoined.
fn stripped_text(src: &SourceFile) -> String {
    src.iter().map(|l| strip_line_comments(l)).collect::<Vec<_>>().join("\n")
}

/// A Go file that imports the cgo pseudo-package `"C"`.
fn imports_cgo(src: &SourceFile) -> bool {
    !src.lines_containing("\"C\"").is_empty()
        && re!(r#"(?-u:\b)import\s+(?:\(\s*)?"C""#).is_match(&stripped_text(src))
}

#[cfg(test)]
mod js_unit_tests {
    use super::js_unit_to_byte;

    /// The char-by-char walk the ASCII fast path replaced.
    fn walk(s: &str, units: usize) -> usize {
        let mut seen = 0usize;
        for (byte_idx, ch) in s.char_indices() {
            if seen >= units {
                return byte_idx;
            }
            seen += ch.len_utf16();
        }
        s.len()
    }

    #[test]
    fn matches_the_char_walk_at_every_offset() {
        for s in ["", "abc", "a\u{e9}b", "x😀y", "😀😀", "plain ascii line", "é", "ab\u{2028}cd"] {
            for units in 0..s.len() + 4 {
                assert_eq!(js_unit_to_byte(s, units), walk(s, units), "{s:?} at {units}");
            }
        }
    }

    /// Lines past the 64-byte block, with multi-byte chars on and around the
    /// block edges, so the block counter and the char walk hand over mid-char.
    #[test]
    fn matches_the_char_walk_across_block_edges() {
        for shift in 0..9 {
            for unit in ["é", "€", "😀", "a"] {
                let s = format!("{}{}", "x".repeat(60 + shift), unit.repeat(80) + "tail😀é");
                for units in 0..utf16_total(&s) + 4 {
                    assert_eq!(js_unit_to_byte(&s, units), walk(&s, units), "shift {shift} unit {unit:?} at {units}");
                }
            }
        }
    }

    fn utf16_total(s: &str) -> usize {
        s.chars().map(char::len_utf16).sum()
    }
}
