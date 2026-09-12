# Kernel-only extraction — removing the WASM path

**Status:** plan, not started. Written 2026-09-11. Companion to [resolution-binding-model-plan.md](resolution-binding-model-plan.md) and [greenfield-rust-core-sketch.md](greenfield-rust-core-sketch.md). Supersedes the "coexistence is permanent" stance in [rust-kernel-migration-plan.md](rust-kernel-migration-plan.md) §4d once approved.

**Goal:** the Rust kernel (`codegraph-kernel/`) is the only parser and extractor. `web-tree-sitter`, the vendored `.wasm` grammars, the per-language routing table, the error-file deferral, the V8 `--liftoff-only` relaunch, and the Node 25 block are all removed.

**Why:** the WASM path is no longer a fallback in practice. It is the mandatory path for every Vue, Svelte, Astro and Razor file, for nine tail languages, for three read-time features, and for every file whose parse tree has an error. It costs a second grammar supply chain pinned in lockstep with Cargo.toml, a runtime relaunch that forces a dedicated Node 24 alias on the host, a per-worker grammar heap that made Bun unusable, and about 27k lines of TypeScript. The kernel already produces byte-identical graphs for 20 languages.

## 1. What the WASM path does today

Facts as of `f82e4898`.

| Consumer | Where | Why it needs WASM |
|---|---|---|
| 9 tail languages | `src/extraction/languages/{objc,arkts,pascal,vbnet,cobol,erlang,nix,terraform,solidity}.ts` and the three CFML grammars | No Rust walker |
| SFC extractors | `vue-extractor.ts`, `svelte-extractor.ts`, `astro-extractor.ts`, `razor-extractor.ts` | Slice the file, then construct `TreeSitterExtractor` directly on the block; routed before the kernel branch in `tree-sitter.ts:7321-7376` |
| CFML extractor | `cfml-extractor.ts` | Walks a live CST from `getParser('cfml')` |
| Error-file deferral | `kernel/index.ts:239,320`, `stack.rs` | Kernel throws `defer:`; WASM re-parses. Also the stack-overflow guard |
| Viewer highlighting | `extraction/syntax-tokens.ts` → `ui-server/highlight/index.ts` | Parses a live tree at read time |
| Branch guards (`WHEN` labels) | `graph/branch-guards.ts` | Parses a live tree at query time |
| Explore source ranges | `mcp/explore-source-ranges.ts` | Parses a live tree at query time |
| Parity oracle | 15 `kernel-*-parity` and grammar tests, `scripts/kernel-parity.mjs` | Two-arm compare |
| Loader fail-soft | `kernel/loader.ts` | Any load failure returns null and degrades to WASM |
| Release degradation | `scripts/build-bundle.sh:80-97` | Missing prebuild prints a warning and ships WASM-only |

The kernel exposes `extract_file`, `contract_info`, `grammar_info`, `cfnptr_scan_files`, `cfnptr_strip_c`. It has no parse-tree API. That is the single largest gap.

## 2. Design decisions

### 2.1 Native error recovery becomes canonical

WASM was declared canonical on files with ERROR nodes only so that parity held by construction (migration plan §4b). The divergence is the UTF-16 versus UTF-8 encoding the parser sees; neither recovery is better. The C/C++ deferral round showed the kernel's own numbers improving as deferral dropped, and `CODEGRAPH_KERNEL_CCPP_ERROR_EXTRACT=1` already extracts from erroring trees natively. Decision: the kernel extracts every file it parses. `EXTRACTION_VERSION` bumps once, and the golden dumps are re-baselined against the kernel.

The stack-overflow guard in `stack.rs` keeps its defer semantics but the consumer changes: a deferred file is stored with an `errors` entry and zero symbols, the same as a file that fails to parse today.

### 2.2 The kernel gains a region API, not an SFC parser

Vue, Svelte, Astro and Razor slicing stays in TypeScript for now. Each already computes `(content, language, lineOffset)` per block and today constructs a `TreeSitterExtractor` on it. They call `extract_file` on the block instead, then rebase positions exactly as they do now. Razor's synthetic `class __RazorCode__ { ... }` wrapper is unchanged. This is a call-site substitution, not a port.

Moving the slicers themselves into Rust is a later optimization and is not required to delete WASM.

### 2.3 The kernel gains a parse-tree service for the three read-time consumers

**Revised 2026-09-11 (Phase 3 implementation).** The original text proposed two derivation entry points, `parse_tokens` and `walk_guards`. Reading the consumers changed that: `branch-guards.ts` is 2,300 lines of per-language rules with eight entry points (guards, call arguments, call sites, triggers, loops, decorators, member types), all walking the tree with `type`, `childForFieldName`, `text` and `parent`. Porting those rules to Rust is weeks of work for no retrieval gain.

What shipped instead: `parse_tree(content, language)` returns the **whole CST as flat buffers in one crossing** (`codegraph-kernel/src/tree.rs`), and `src/extraction/kernel/tree.ts` wraps the rows in `NativeNode`, a facade with the web-tree-sitter node surface. `src/extraction/parse-tree.ts` is the one seam (`parseSourceTree`, kernel first, WASM fallback) and defines the structural `TreeNode` type both trees satisfy, so the three consumers run unchanged on either. Positions are UTF-16 code units so `source.slice` is exact; an all-ASCII file skips the prefix table; kind and field name tables are fetched once per language (`tree_names`); the child table is built by a counting pass; each row carries its index in its parent so sibling lookups are constant time.

The earlier rejection was of a per-node handle over napi, which would cost a crossing per property read. A serialized tree costs one crossing and is what the consumers need.

### 2.4 Tail languages: all kept (revised)

**Revised 2026-09-11 (Phase 4 implementation).** "Port" no longer means a hand-written Rust walker. Phase 3's serialized tree made a cheaper mechanism possible: `TreeSitterExtractor`, the 7,000-line generic TypeScript extractor driven by the per-language tables in `languages/`, now parses through the same kernel-first seam (`parseSourceTreeSync`) and walks the `NativeNode` facade. Any grammar compiled into the kernel is therefore extracted natively with no walker at all, with the extractor logic unchanged. Putting a tail language on the kernel is then one crate line in `Cargo.toml` plus one `grammar_for` arm, gated by the golden dump generated on WASM beforehand. The same mechanism is the fallback for a stack-guard defer on a walker language, and it changes the cost picture for the "drop" column: a dropped language costs only its grammar's compile, so that decision should be retaken with this in mind (see Phase 4 below).

| Language | Decision | Basis |
|---|---|---|
| Objective-C | Port | 181-line extractor, `tree-sitter-objc` on crates.io, feeds the Swift/ObjC bridge resolver with tests |
| Erlang | Port | 384 lines, tests for arity and behaviour synthesizers, `tree-sitter-erlang` matches the ELP lineage we vendor |
| Nix | Port | 324 lines, option synthesizer test, crate exists |
| Pascal + DFM | Port | 72 lines plus the parser-free DFM extractor, 13 changelog entries show real users |
| Solidity | Port | 282 lines, crate exists, low risk |
| ArkTS | Drop unless a user asks | Crate lineage differs from our harmony-contrib fork; one test file |
| Terraform | Drop unless a user asks | No `tree-sitter-terraform` crate; `tree-sitter-hcl` would need re-validation; no tests |
| VB.NET | Drop | No crate, patched grammar with a C scanner, no tests, no changelog |
| COBOL | Drop | 16 MB vendored fork grammar, no tests; copybook logic would need a port |
| CFML (3 grammars) | Decide with the maintainer | Own grammars for cfquery and cfscript; 508-line extractor walks a live CST; two test files |

"Drop" means the language is removed from `EXTENSION_MAP` and the README table in the same change, with a changelog entry. It does not mean silently unindexed.

### 2.5 The loader fails hard

`kernel/loader.ts` returns null today on missing binary, ABI mismatch or kind-table drift. After this plan a load failure is a fatal startup error with the platform and the expected path in the message. `build-bundle.sh` errors instead of warning when a prebuild is absent. `CODEGRAPH_KERNEL`, `CODEGRAPH_KERNEL_LANGS`, `CODEGRAPH_KERNEL_EXPECT` and `CODEGRAPH_KERNEL_CCPP_ERROR_EXTRACT` are removed. `CODEGRAPH_KERNEL_PATH` stays for source development.

### 2.6 Platform support is explicit

Release matrix today: macOS x64 and arm64, Linux glibc x64 and arm64, Windows x64 and arm64. Add `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` so Alpine containers work. Every other platform is unsupported and says so at install time. Source checkouts need a Rust toolchain; `npm test` builds the kernel in `pretest` if the prebuild for the host is missing.

### 2.7 Golden dumps replace the parity oracle

`scripts/dump-graph.mjs` already produces a natural-key sorted dump. Check in dumps for a fixed set of fixture repos at pinned commits, one per language family, and add `__tests__/golden-dumps.test.ts` that re-indexes each fixture and compares. This is the regression gate for every extractor change from here on. It replaces the 15 parity test files, which delete.

## 3. Phases

Each phase lands on `fork/consolidated` behind the golden-dump gate and leaves the tree shippable. Phases 1 through 3 can proceed in parallel; 4 and 5 depend on all of them.

### Phase 0: golden-dump gate — DONE 2026-09-11

- `__tests__/kernel-golden-dumps.test.ts` indexes each corpus fixture into a temp directory, dumps it with `scripts/dump-graph.mjs`, and compares against `__tests__/fixtures/golden/<name>.dump`. `UPDATE_GOLDEN=1` re-baselines; the resulting `.dump` diff is the review artifact.
- Corpus: `torture-multilang` (the 38-file kernel-parity torture set, all 20 routed languages), `payroll-go`, `php-import-alias-static`, and two fixtures that exist only for this gate: `golden/vue-sfc` (script-setup TS, options API, styles, vue-router, pinia, `@/` path alias) and `golden/markdown-docs` (sections, doc→code and code→doc references).
- Goldens were generated on the kernel-routed path and verified identical on the WASM-only path (`CODEGRAPH_KERNEL=0`) and under single-worker indexing, and path-independent across temp locations. A tampered golden fails with a line-level added/removed diff.
- The file name matches the `kernel-*.test.ts` glob in `release.yml`, so the gate runs against the freshly built linux-x64 kernel on every release without extra wiring. It also runs in plain `npm test`.
- `tail-render-ts` was considered and dropped: 2.2 MB of dump for six files, and TypeScript is already covered three times over. Total golden size is 1.7 MB.

Exit met: the gate is green on `fork/consolidated` with no extraction change.

### Phase 1: error recovery flip — DONE 2026-09-11

- The `defer:` throw for parse errors is removed from all 14 walkers and the `CODEGRAPH_KERNEL_CCPP_ERROR_EXTRACT` hatch is gone; only the stack-overflow guard defers. The one-slot memo stays until Phase 5 because a stack-guard defer still needs the pre-parsed source for the WASM fallback (a deviation from the original bullet).
- The one recovery path that existed only on the WASM side, the Kotlin `fun interface` misparse hook, is ported into `kotlin.rs` (`is_fun_interface_node` and the hook in `try_visit_hook`). The C++ explicit-operator scan that was already in the kernel is now live. Every other "defer-shielded" note was a phantom-error or both-arm-error case with nothing to recover.
- `EXTRACTION_VERSION` 28 → 29. The golden corpus did not change: no fixture file has a parse error, so the six goldens hold byte-for-byte.
- `scripts/kernel-parity.mjs` gained `--error-files only|skip|all`. Clean-file parity is still the walker gate (`skip`); `only` is the divergence survey.
- Measured divergence between native and WASM recovery on error files, with the survey mode, before any re-baseline:

| Repo | Files with parse errors | Differ from WASM | Net symbols in kernel view |
|---|---|---|---|
| redis (C, 794 files) | 350 | 21 | +6 functions, +6 constants, +7 variables, +96 call refs |
| fmt (C++, 74 files) | 39 | 10 | +1 function, −1 method, +1 struct, −1 type alias (positions shift, symbol sets nearly equal) |
| okio (Kotlin, 315 files) | 24 | 0 | identical, including the ported fun-interface hook |

- The 16 parity tests that pinned "erroring file → null" now assert native extraction, and the Kotlin and C++ ones assert the recovered symbol on the kernel result.
- The parse-collapse warning the WASM extractor records when an erroring tree yields no symbol (#1522) is now emitted by every kernel walker (`parse_collapse_warning` in `buffers.rs`), so `codegraph index` shows the same warning on either path; the full suite caught its absence.

Exit met: no file reaches WASM because of an ERROR node; deferral reads zero on all three survey repos.

### Phase 2: SFC extractors call the kernel — DONE 2026-09-11

- `src/extraction/block-extract.ts` is the one seam: `extractEmbeddedBlock(filePath, content, language)` tries `tryKernelExtract` and falls back to `TreeSitterExtractor` only when the kernel declines (language not routed, no binary, stack-guard defer). Vue, Svelte, Astro and Razor call it in place of constructing the WASM extractor; their position rebasing is untouched, since results are block-relative either way.
- A second SFC golden, `golden/sfc-mix` (Svelte with `lang="ts"`, `context="module"` and plain script; Astro frontmatter plus inline script; Razor `@code` with a C# sibling), was generated on the WASM block path before the change and holds byte-for-byte after it, as does `vue-sfc`.
- `__tests__/kernel-sfc-blocks.test.ts` pins both halves on six sample files: with a kernel staged, extracting an SFC never calls `getParser` (no WASM parser instantiated), and the kernel and WASM results are canonically equal.

Exit met: SFC fixtures index with zero WASM parses on a host with a kernel. The remaining WASM users in `src/extraction/` are the CFML extractor (live CST), the tail languages, and the fallback itself.

### Phase 3: parse-tree service — DONE 2026-09-11

- `codegraph-kernel/src/tree.rs` (`parse_tree`, `tree_names`), `src/extraction/kernel/tree.ts` (facade), `src/extraction/parse-tree.ts` (seam and `TreeNode` type). `syntax-tokens.ts`, `graph/branch-guards.ts` and `mcp/explore-source-ranges.ts` import the seam instead of `web-tree-sitter` and `getParser`; their walkers are untouched.
- `__tests__/kernel-parse-tree.test.ts`: for 19 torture fixtures plus three of this repo's own sources, the facade matches the WASM tree node for node (type, named-ness, UTF-16 indexes, positions, child counts, field names), syntax spans are equal, and branch guards agree at up to 60 call sites per file. Erroring C/C++ fixtures are checked for `hasError` agreement only (Phase 1). The existing highlighter, branch-guard and Steps suites pass on the kernel path.
- `scripts/bench-parse-tree.mjs` measures the read-time derivations on both arms and asserts output agreement on clean trees.

Measured (median per file, warm, single thread, this host):

| Corpus | Derivation | WASM | Kernel | Ratio |
|---|---|---|---|---|
| redis `src` (212 C files) | bare parse | 0.60 ms | 0.96 ms | 0.64x |
| | tokenize | 1.23 ms | 1.09 ms | 1.08x |
| | guards, 20 sites, re-parse per site | 11.1 ms | 17.4 ms | 0.70x |
| okio (300 Kotlin files) | bare parse | 0.32 ms | 0.51 ms | 0.57x |
| | tokenize | 0.59 ms | 0.58 ms | 1.03x |
| | guards | 5.7 ms | 9.8 ms | 0.61x |
| this repo (253 TS files) | bare parse | 0.67 ms | 0.70 ms | 0.96x |
| | tokenize | 1.42 ms | 0.90 ms | 1.58x |
| | guards | 13.6 ms | 13.6 ms | 1.07x |

Reading it honestly: the WASM parser builds a lazy tree, so a bare parse is cheaper there; the kernel serializes every node up front. Once a consumer walks the tree, the facade's buffer reads are cheaper than WASM boundary crossings, so tokenizing is at par or faster and guards are at par on TypeScript. On the largest redis file (658 KB) the kernel's parse plus serialization took 62 ms against 40 ms for the WASM parse alone and 58 ms for WASM parse plus one full walk. The guards row overstates the cost because the benchmark re-parses per site; production callers parse once per file and cache the tree. None of these paths is on the retrieval critical path: an explore call is measured in hundreds of milliseconds. The value of this phase is removing the last read-time dependency on the WASM runtime, not speed. Outputs differed on 3 of 765 files, all with erroring trees.

Exit met: no `getParser` call outside `src/extraction/` (`branch-guards.ts` and `explore-source-ranges.ts` no longer import it).

### Phase 4: tail languages — five on the kernel 2026-09-11; drops and CFML pending a decision

- The generic extractor runs on native trees (§2.4 revision). `__tests__/kernel-generic-extractor-tree.test.ts` gates it: with walker routing off and native trees on, extraction of every torture fixture equals the all-WASM arm as canonical multisets (erroring C/C++ files compared loosely). It caught one facade gap: web-tree-sitter's `childForFieldName` finds a child whose field is attached through a hidden grammar rule (Swift's `return_type`) while its `fieldNameForChild` does not; the kernel row now carries the cursor field plus an extra-field table gathered with the C-backed `child_by_field_id`, and the facade mirrors the asymmetry.
- Objective-C, Erlang, Nix, Pascal and Solidity grammars are compiled into the kernel from crates.io (`tree-sitter-objc` 3.0.2, `tree-sitter-erlang` 0.20.0, `tree-sitter-nix` 0.3.0, `tree-sitter-pascal` 0.10.2, `tree-sitter-solidity` 1.2.13). Kind and field tables against the vendored WASM grammars: Erlang, Nix and Pascal identical; Objective-C 588 vs 570 kinds (35 differ) and Solidity 531 vs 512 kinds (39 differ), the crates being newer than the 2023-era `tree-sitter-wasms` builds. The `golden/tail-langs` fixture (ten files across the five languages) was generated on WASM before the grammars were added and holds byte-for-byte on native parsing despite the drift.
- The same test proves extracting each of the five on a kernel host never instantiates a WASM parser.
- **Phase 4b (decided 2026-09-11: keep all four, move CFML).** ArkTS, Terraform (HCL), VB.NET, COBOL and the three CFML grammars are compiled into the kernel as vendored C from the exact revisions the WASM files were built from (`codegraph-kernel/grammars/PROVENANCE.md`): the npm `tree-sitter-arkts` 0.2.0 and `@tree-sitter-grammars/tree-sitter-hcl` 1.2.0 package sources, the patched govindbanura and yutaro-sakamoto forks regenerated with tree-sitter-cli 0.25.10 from the patches in `docs/grammars/`, and cfmleditor/tree-sitter-cfml at `a224ff10`. All seven kind and field tables are identical to the vendored WASM grammars, so parity holds by construction. `cfml-extractor.ts` walks the facade through `parseSourceTreeSync`. The `golden/tail-langs` fixture grew to 20 files across ten languages, generated on WASM before the grammars were added, and holds byte-for-byte on native parsing; the no-WASM proof covers all ten. The kernel prebuild grows from 41 MB to 74 MB (COBOL's parser alone is 38 MB of C), still under the 75 MB `dist/` it replaces together with 65 MB of `.wasm` files. Nothing is dropped.

Measured, TypeScript sources of this repo, median per file: bare parse 0.69 ms WASM vs 0.96 ms kernel (0.73x; the hidden-field gather costs about a quarter of the parse), tokenize 1.60 vs 1.16 ms (1.37x).

Exit met: every language in `EXTENSION_MAP` with a grammar parses natively. The only remaining WASM use in `src/extraction/` is the fallback itself, which Phase 5 removes.

### Phase 5: delete WASM

- Remove `web-tree-sitter` and `tree-sitter-wasms` from `package.json`, `src/extraction/wasm/`, `copy-assets`, `grammars.ts`, `tree-sitter.ts`'s WASM branch, `TreeSitterExtractor`, `parse-worker.ts`'s Emscripten stderr filter and OOM exit, `resetParser`.
- Remove `wasm-runtime-flags.ts` except `NODE_RUNTIME_FLAGS`, the relaunch in `bin/codegraph.ts`, `command-supervision.ts` if it has no other purpose, the liftoff lines in `npm-shim.js` and `build-bundle.sh`, and the Node 25 block in `node-version-check.ts`.
- Make the loader fatal. Make `build-bundle.sh` fatal on a missing prebuild. Add the musl targets.
- Delete the 15 parity tests, `kernel-parity.mjs`, and the wasm-flag tests. Rewrite `kernel-scaffold.test.ts` for the new semantics. Re-check the four MCP orphan tests that depended on the re-exec process shape.
- Update `server-instructions.ts` only if the language list changes. Update `AGENTS.md` build notes and the `copy-assets` rule.

Exit: `grep -r web-tree-sitter src __tests__ scripts` is empty. Full suite green on Linux, Windows and macOS. The espn-draft host no longer needs the Node 24 alias.

## 3a. Measurements so far

Every phase records before-and-after numbers here so the work can be judged, not assumed. All figures are from this WSL host (15 vCPUs exposed, Node 24) unless stated; single runs are marked.

| What | Before | After | How measured |
|---|---|---|---|
| Fresh index of redis (794 C/H files), WASM-only vs kernel, n=1 | 5.18 s, 1,301 MB peak RSS, 20,567 nodes / 76,845 edges | 4.06 s, 1,355 MB, 20,584 nodes / 76,927 edges | `codegraph init -y` under `/usr/bin/time -v`, `CODEGRAPH_KERNEL=0` vs default, after Phase 1 |
| Files reaching WASM because of a parse error (redis, fmt, okio) | 350, 39, 24 | 0, 0, 0 | `scripts/kernel-parity.mjs --error-files only` (Phase 1) |
| Symbol delta on erroring files, kernel view vs WASM (redis) | | +6 functions, +6 constants, +7 variables, +96 call refs | same |
| SFC script blocks parsed by WASM on a kernel host (vue-sfc + sfc-mix fixtures) | every block | 0 | `kernel-sfc-blocks.test.ts` spies `getParser` (Phase 2) |
| Graph output for the SFC fixtures | | byte-identical | golden dumps generated before Phase 2, unchanged after |
| Read-time parse derivations, kernel vs WASM | see the Phase 3 table | | `scripts/bench-parse-tree.mjs` |
| Whole-graph regressions caught by the golden gate | | 1 (missing parse-collapse warning, Phase 1) plus 1 stale-dist false alarm | `kernel-golden-dumps.test.ts` and the full suite |
| Test surface | 4,773 tests | 4,791 tests (Phase 2) | full engine suite, 7 workers |

### Phase 5 payoff, measured before committing to the ports (2026-09-11)

A measurement-only switch skipped the WASM grammar loads for kernel-routed languages in the parse workers (not merged); the MCP cold start was timed with and without the `--liftoff-only` relaunch; disk footprint was read from `dist/`. Fresh redis index, kernel path, n=1 per cell:

| Parse workers | Grammars loaded in workers | Wall | Peak RSS |
|---|---|---|---|
| 1 | yes | 6.55 s | 1,238 MB |
| 1 | skipped | 5.88 s | 1,250 MB |
| 4 | yes | 5.00 s | 1,237 MB |
| 4 | skipped | 4.30 s | 1,225 MB |
| 8 | yes | 4.34 s | 1,356 MB |
| 8 | skipped | 4.06 s | 1,316 MB |

| MCP server (`serve --mcp`, small indexed project, median of 7) | Time to `initialize` reply | RSS after init | Processes |
|---|---|---|---|
| Default (relaunch with `--liftoff-only`) | 120 ms | 191 MB | 3 |
| `CODEGRAPH_NO_RELAUNCH=1` | 81 ms | 139 MB | 2 |

| On disk | Size |
|---|---|
| `dist/` today | 75 MB, of which `extraction/wasm` is 65 MB (29 grammars) |
| `dist/` without WASM | 10 MB |
| Kernel prebuild (linux-x64) | 34 MB |
| `web-tree-sitter` + `tree-sitter-wasms` in `node_modules` | 54 MB |

Reading it: on Node the per-worker WASM grammar heap is about 5 MB per worker, not the 60 MB the Bun probe saw (that was a JavaScriptCore compiler-thread leak). Peak RSS at index time is dominated by the main thread's store and resolution work, so deleting WASM does not move the memory headline on Node. It does buy roughly 0.3 to 0.7 s of grammar compile per fresh index, a 40 ms and 50 MB cheaper MCP cold start with one process fewer per session, a 41% smaller install (75 MB to 44 MB with the kernel counted), two fewer runtime dependencies, and the removal of the Node 25 block and the relaunch machinery that forced a dedicated Node 24 alias on the espn-draft host. The engineering payoff (one grammar supply chain, no parity oracle to maintain, about 27k lines gone) is not in these tables and is the larger part of the case. Retrieval quality is held constant by construction (goldens) and is not a lever here.

## 4. What is removed, by the numbers

| Item | Lines or count |
|---|---|
| TypeScript language extractors (`languages/*.ts`) | ~25k, minus `c-cpp.ts` (1,793) which stays for preParse blanking |
| `TreeSitterExtractor` and WASM branch in `tree-sitter.ts` | most of 7.4k |
| Vendored `.wasm` grammars | 30 files |
| Parity and WASM tests | 15 files |
| Env vars | 6 |
| Runtime dependencies | 2 of 10 |
| Node runtime constraints | Node 25 block, `--liftoff-only` relaunch, dedicated Node alias on hosts |

What is added: about 5 Rust walkers for tail languages, two kernel entry points, two musl targets, the golden-dump test, and a `pretest` kernel build for source checkouts.

## 5. Risks

- **Error-recovery re-baseline changes graphs.** Expected and accepted. Phase 1 records per-fixture deltas. A regression in a fixture's symbol count is a walker bug to fix, not a reason to keep WASM.
- **Source-checkout DX.** Contributors need cargo. Mitigation: `pretest` builds only when the prebuild is missing, and CI publishes prebuilds on every `fork/consolidated` push so most contributors never compile.
- **Upstream divergence.** Upstream keeps coexistence. This fork's graph output stays byte-identical to upstream's kernel path for the 20 routed languages, so upstream resolution fixes still merge. Extraction changes upstream makes to WASM-only languages will not apply; that is the cost of dropping them.
- **Read-time consumers.** Phase 3 is the least-explored piece. If `walk_guards` turns out to need a general tree, fall back to a narrow node-cursor API scoped to one file, not a global tree handle.
- **Deferred files.** A stack-overflow defer now yields an empty file instead of a WASM parse. Incidence is one known file in clang.

## 6. Settings after this plan

Removed: `CODEGRAPH_KERNEL`, `CODEGRAPH_KERNEL_LANGS`, `CODEGRAPH_KERNEL_EXPECT`, `CODEGRAPH_KERNEL_CCPP_ERROR_EXTRACT`, `CODEGRAPH_WASM_RELAUNCHED`, `CODEGRAPH_ALLOW_UNSAFE_NODE`, `CODEGRAPH_NO_RELAUNCH`.
Kept: `CODEGRAPH_KERNEL_PATH` (source dev), `CODEGRAPH_KERNEL_DEBUG`, `CODEGRAPH_KERNEL_CFNPTR`, `CODEGRAPH_PARSE_WORKERS`.
