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

Add a `parse_tokens(file, content, language)` entry point that returns the flat token stream `syntax-tokens.ts` derives today, and a `walk_guards(file, content, language, ranges)` entry point that returns the guard conditions `branch-guards.ts` computes. `explore-source-ranges.ts` needs only node boundaries and folds into the tokens call. All three return flat buffers through the existing layout mechanism.

Alternative considered: expose a generic tree handle over napi. Rejected. Per-node boundary crossings are exactly the cost the kernel exists to avoid, and the three consumers need three fixed derivations, not a tree.

### 2.4 Tail languages: port five, drop four, decide CFML separately

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

### Phase 0: golden-dump gate

- Pick fixtures: one small repo per language family plus the espn-draft-shaped Vue fixture and a Markdown-heavy fixture.
- Check in dumps generated on the current kernel-routed path.
- Add the test. Wire it into CI on the linux-x64 prebuild job.

Exit: the gate is green on `fork/consolidated` with no extraction change.

### Phase 1: error recovery flip

- Remove the `defer:` throw for parse errors in every walker; keep it for the stack guard.
- Delete `takeDeferredPreParse` and the one-slot memo.
- Bump `EXTRACTION_VERSION`. Re-baseline the golden dumps and record the node and edge deltas per fixture in this document.

Exit: no file reaches WASM because of an ERROR node. Deferral rate in `scripts/kernel-parity.mjs --max-deferral` reads zero for every routed language.

### Phase 2: SFC extractors call the kernel

- Replace `new TreeSitterExtractor(...)` in the four SFC extractors with `extract_file` on the block, then the existing position rebase.
- Add SFC fixtures to the golden set.

Exit: the espn-draft fixture indexes with zero WASM parses (instrument `getParser` to throw under a test flag).

### Phase 3: parse-tree service

- Add `parse_tokens` and `walk_guards` to the kernel with layouts in `layout.ts`.
- Port `syntax-tokens.ts`, `branch-guards.ts` and `explore-source-ranges.ts` to consume them.
- Extend the viewer highlighting parity test (`cg57-highlighting-parity`) to run against the kernel tokens.

Exit: no `getParser` call outside `src/extraction/`.

### Phase 4: tail languages

- Port Objective-C, Erlang, Nix, Pascal, Solidity in that order, each with its own checklist file following the existing `*-kernel-port-checklist.md` pattern and a golden fixture.
- Remove ArkTS, Terraform, VB.NET and COBOL from `EXTENSION_MAP`, the README table, and `grammars.ts`, with a changelog entry under Breaking Changes.
- CFML: separate decision recorded here before Phase 5 starts.

Exit: every language in `EXTENSION_MAP` has a kernel walker.

### Phase 5: delete WASM

- Remove `web-tree-sitter` and `tree-sitter-wasms` from `package.json`, `src/extraction/wasm/`, `copy-assets`, `grammars.ts`, `tree-sitter.ts`'s WASM branch, `TreeSitterExtractor`, `parse-worker.ts`'s Emscripten stderr filter and OOM exit, `resetParser`.
- Remove `wasm-runtime-flags.ts` except `NODE_RUNTIME_FLAGS`, the relaunch in `bin/codegraph.ts`, `command-supervision.ts` if it has no other purpose, the liftoff lines in `npm-shim.js` and `build-bundle.sh`, and the Node 25 block in `node-version-check.ts`.
- Make the loader fatal. Make `build-bundle.sh` fatal on a missing prebuild. Add the musl targets.
- Delete the 15 parity tests, `kernel-parity.mjs`, and the wasm-flag tests. Rewrite `kernel-scaffold.test.ts` for the new semantics. Re-check the four MCP orphan tests that depended on the re-exec process shape.
- Update `server-instructions.ts` only if the language list changes. Update `AGENTS.md` build notes and the `copy-assets` rule.

Exit: `grep -r web-tree-sitter src __tests__ scripts` is empty. Full suite green on Linux, Windows and macOS. The espn-draft host no longer needs the Node 24 alias.

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
