# Resolution binding model — one source of truth for exports and bindings

**Status:** Phases 0 to 2 done (2026-09-12; the Phase 2 exit criterion is met, no source regex remains for TS/JS in the resolver); Phases 3 and 4 not started. Written 2026-09-11. Companion to [kernel-only-extraction-plan.md](kernel-only-extraction-plan.md) (which should land first, so there is one extractor to emit the new facts) and [greenfield-rust-core-sketch.md](greenfield-rust-core-sketch.md). Closes upstream issue #1721 and ends the fix cycle behind #1566, #1790, #1794 and #1844.

**Goal:** extraction emits a per-file binding table. Resolution consumes it and never rescans raw source to answer "is X exported", "what does N bind to in F", or "is this receiver a known thing". Every resolver predicate that reads source today is replaced by a lookup.

## 1. The problem, with the evidence

### 1.1 Five regex families answer one question

"Is X exported" is decided in five places, none shared:

| Site | Mechanism | Blind spot |
|---|---|---|
| `languages/typescript.ts:110`, `javascript.ts:68` | AST `export_statement` ancestor walk | `const x; export { x }`, all CommonJS |
| `tree-sitter.ts:2451` mirrored in `codegraph-kernel/src/tsjs/extractors.rs:359` | Regex over file source (`isExportedLater`) | Scoped to Zustand-style stores only; the two hand-mirrored regexes named in #1721 |
| `name-matcher.ts:672` (`isSealedModule`) | Three regexes over comment-stripped source plus a raw-source CommonJS check | Per-file "exports nothing", added by #1720/#1746 |
| `import-resolver.ts:96` (`DEFAULT_EXPORT_BINDING_RE`) | Regex for `export default NAME` | Patches `isExported` inside `FileExportIndex` |
| `alias-binding.ts:105` (`extractLocalExportAliases`) | Regex for `export { X as Y }` | Not shared with any of the above |

The only persisted fact is `nodes.isExported`. `NodeKind` declares `export` and `EdgeKind` declares `exports`, but no extractor emits either. The export side of the schema exists on paper.

### 1.2 "What does N bind to in F" is answered by per-reference regexes at resolution time

`isBoundToBareImport` (`name-matcher.ts:551`), `isBareJsCall` (`:895`) and `isLocallyBoundJsName` (`:932`) each build `RegExp`s over the whole file source per name, memoized per resolution context. `isStaticCFunction` reads C source lines because the extractor records no storage class. The receiver-type inference table (`:1950-2082`) is 40 languages of regex over source.

### 1.3 The fix cycle

Each PR in the chain added a new source-reading predicate, narrowed by a corpus measurement, at a different point in the pipeline:

- #1790 fixed #1566 by dropping every nested identifier-rooted call in both engines. Over-fix. #1794 reported 8 red tests.
- #1844 restores the receiver text and adds `isTsJsNestedCall` as an early-out so nested calls keep only framework evidence.
- #1710 tried the general "drop every unnameable receiver" rule, lost 64 correct edges, and shipped a host-global list instead (#1766). #1790 reversed that judgment; #1844 restores it.
- #1709 filtered the fuzzy candidate set by lexical reachability and manufactured uniqueness (+59 wrong on vite). #1718 moved the check onto the single survivor. The rule "reachability may reject a unique guess, never manufacture one" now lives in a comment at `name-matcher.ts:3220`.
- #1720/#1746 learned that sealed-module status must filter candidates for `imports` ranking but only reject the winner for calls, because removing from a crowd promotes a runner-up.
- #1759 made Zustand store bindings look like local shadows; #1762/#1763 carved them back out.
- #1767 showed that 3,310 edges classified as `exact-match` or `fuzzy` on one repo were really failed import resolution (`.js` specifier to `.ts` source).

The recurring shape: resolution guesses at binding facts from source text, each guess is a separate predicate, and the predicates interact through candidate-set ordering. Every fix is a new predicate.

### 1.4 What it costs

The migration plan's §7a numbers put resolution at 73% of Linux-kernel wall. `settle` (where these predicates run) is 3.6 s at 8 cores after the pool absorbs it, but the predicates also force `readFileCached` on every candidate file and hold source in the worker lane. The larger cost is correctness: the eval runner scores recall only (`__tests__/evaluation/scoring.ts`), so precision regressions are found by ad-hoc corpus runs in PR bodies.

## 2. Design

### 2.1 A binding table emitted by extraction

Extraction emits, per file, a list of bindings. Persisted in a new table:

```
bindings(
  file_path      TEXT NOT NULL,
  name           TEXT NOT NULL,      -- local name as written in this file
  kind           TEXT NOT NULL,      -- 'decl' | 'import' | 'reexport' | 'alias' | 'param' | 'local'
  node_id        TEXT,               -- the declaring node when kind='decl' or 'alias'
  target_spec    TEXT,               -- import/re-export specifier as written
  target_name    TEXT,               -- imported/re-exported name (or '*' / 'default')
  exported_as    TEXT,               -- NULL if not exported; the export name otherwise
  export_form    TEXT,               -- 'esm' | 'esm-later' | 'esm-default' | 'cjs' | 'cjs-object' | 'public' | ...
  scope_start    INTEGER, scope_end INTEGER,   -- line range the binding is visible in
  storage        TEXT                -- language-specific visibility: 'static', 'private', 'pub(crate)', ...
)
```

Rules:

- One row per (file, name, scope). A re-declared name in a nested scope is a separate row with its own scope range.
- `exported_as` is the single answer to "is X exported". The five regex families collapse into the extractor emitting the right `export_form`.
- Per-file "exports nothing" is `NOT EXISTS (SELECT 1 FROM bindings WHERE file_path=? AND exported_as IS NOT NULL)`. `isSealedModule` deletes.
- `kind='import'` with `target_spec` not resolvable to a project file is a bare import. `isBoundToBareImport` becomes a lookup.
- `storage` carries C `static`, Rust visibility, Java/Kotlin/C# access modifiers. `isStaticCFunction` and the `PRIVATE_IS_FILE_LOCAL` table become one predicate over one column.
- `nodes.isExported` stays as a denormalized convenience and is set from the table at store time. `EdgeKind` `exports` starts being emitted, from `bindings` rows with `exported_as`, so `context/index.ts` stops being the sole consumer of a kind nothing produces.

### 2.2 Resolution reads bindings, not source

`ResolutionContext` gains `bindingsFor(file)` returning the file's rows, indexed by name and by scope. The strategy ladder in `resolveOneInner` (`index.ts:900`) becomes:

1. Framework resolvers (unchanged, still first).
2. **Binding lookup**: for reference `N` at line `L` in file `F`, find the innermost `bindings` row for `N` whose scope contains `L`.
   - `decl` or `alias` with `node_id`: resolved. Confidence 1.0.
   - `import` with a project-resolvable `target_spec`: follow to the target file's `exported_as = target_name` row. Resolved by import. This is `resolveViaImport` with the regex import extractors removed.
   - `import` with a bare specifier: unresolved, `status='failed'`, reason `external`. Never falls through to name matching. This is #1713 as a rule instead of a predicate.
   - `local` or `param`: unresolved, reason `local`. Never falls through. This subsumes `isLocallyBoundJsName` and the #1762 carve-outs, because a Zustand store accessor is a `decl` row, not a `local` row.
3. **Cross-file name match** only when no binding row exists for `N` in `F`. Candidates are `bindings` rows across the project with `name = N` and `exported_as IS NOT NULL` (or `storage` visible across files), joined to nodes. `isVisibleAcrossFiles`, `isCrossFileReachable`, `isLexicallyReachable` become a single visibility predicate over the row.
4. Fuzzy match keeps the #1718 rule, expressed as: fuzzy may reject the unique candidate on visibility, never filter the set.

Receiver-typed calls (`a.b()`, `this.x()`) keep `matchMethodCall` and the chain matchers, but receiver identity comes from the binding row for `a` (a `decl` with a `node_id` whose node has a type, or an `import` whose target is a class) before the regex inference table is consulted. The inference table shrinks as languages emit typed bindings.

### 2.3 Where the receiver-guessing rule lands

The #1566 / #1790 / #1844 question, "should `values.get()` bind to the sole project `get`", has a fixed answer under this model: only if `values` has a binding row whose target is a project node with a `get` member. An unbound receiver yields unresolved with reason `unknown-receiver`. There is no builtin list and no host-global list; `js-builtins.ts` becomes documentation of why `window` and `document` never have binding rows.

### 2.4 One extractor

The binding table is emitted by the kernel walkers only. This is why the kernel-only plan lands first. Emitting it twice, in TypeScript and Rust, would recreate the mirrored-regex trap that #1721 describes.

## 3. Phases

### Phase 0: precision in the eval runner — DONE 2026-09-12

- `__tests__/evaluation/scoring.ts` gains `scoreEdgeCase`: an `EdgeCase` names a `kind`, a target endpoint (file suffix + symbol name) and optionally a source endpoint, and expects the edge `absent` (known-wrong) or `present` (control). The recall scorers cannot see a false edge; this can. `__tests__/evaluation-edge-scoring.test.ts` pins the scorer on a synthetic project with the #1713 bare-import and #1746 sealed-module shapes, and proves it flags a violated case and a missing endpoint.
- `__tests__/evaluation/edge-cases.ts` encodes the edges the PR bodies named, on the exact commits they measured (full SHAs, since `git fetch` of an arbitrary commit needs one): vite `8492422b` (#1713 self-import and `getEnv`, #1718 the two fuzzy nested calls, #1746 the sealed `defineConfig` → `test-stacktrace.js::vite`, plus a relative-import control), vitest `7c818153` (`evaluatedModules`), svelte `5895c637` (`bundle`). #1844's regression is already pinned by `ts-chained-receiver.test.ts`; rollup's PR table listed counts only, no endpoints, so it has no cases yet.
- `npm run eval:precision -- <corpus>` (`precision-runner.ts`) fetches the pinned commit, indexes it, scores the cases, prints the resolved-edge histogram by resolver (the LOST/GAINED methodology the PRs used) and writes `results/precision-<corpus>-<commit>-<codegraph>.json`. The reports are committed as the baseline.

Baseline at codegraph `1498c2ff` (kernel-only branch, after Phase 5):

| Corpus | Index | Edges | fuzzy | Cases |
|---|---|---|---|---|
| vite | 3.3 s | 28,893 | 16 | 5 absent held, 1 control held |
| vitest | 5.8 s | 75,035 | 28 | 1 absent held |
| svelte | 7.9 s | 70,432 | 159 | 1 absent held |

Reading it: the resolution PRs' removals hold on the current engine. The `fuzzy` counts are the number to watch through Phases 1 to 3; the plan's claim is that they fall toward zero as bindings replace guesses, and any `absent` case flipping to found is a regression the recall scorers would never report.

Exit met: the LOST/GAINED tables that named endpoints are encoded and green; the runner reports the histogram every later phase compares against.

### Phase 1: emit bindings for TS/JS — DONE 2026-09-12 (first cut)

- `bindings` table and DDL-only migration v11 (`src/db/schema.sql`, `migrations.ts`); `EXTRACTION_VERSION` 29 → 30. Rows appear on re-index.
- Wire contract ABI 2 → 3: a sixth buffer (`BindingRow`, 64 bytes, `buffers.rs` / `layout.ts`), decoded into `ExtractionResult.bindings` and persisted by both store paths (`attachBindings` in `store-writer.ts`; `queries.insertBindings`; per-file delete and full-index clear alongside literals). `scripts/dump-graph.mjs` dumps the table, so the goldens pin it.
- The `tsjs` walker emits: `decl` rows for module-scope declarations with `export_form` `esm` / `esm-later` / `esm-default` / `cjs`; `local` rows for declarations nested in a function or class body, scoped to the enclosing node's lines; `import` rows (default, named, aliased, namespace) with the specifier; `reexport` rows with the source and exported name. Later exports are collected from the AST before the walk (`collect_later_exports`), and the same scan replaces the regex in the generic extractor (`collectLaterExports`), so both `isExportedLater` regexes are gone.
- `nodes.isExported` is now set from the table for any node a `decl` row exports: the vue-sfc golden's `router` constant (declared, then `export default router`) flipped to exported, which the old flag could not see.
- `__tests__/bindings-tsjs.test.ts` pins every emitted form at extraction and the stored export flag.

Not in this cut, deferred to Phase 2 where they are consumed: `param` rows, block-scoped `let`/`const` inside blocks (only function/class-body declarations are `local` today), `alias` rows, and the CommonJS object forms `module.exports = { x }` / `exports["x"]` (the walker's CommonJS detection still covers `exports.NAME = fn` only). The generic TypeScript extractor does not emit bindings, so a TS/JS file that reaches it (stack-guard defer) has none; the walker ≡ generic gate compares nodes, edges and refs only.

Precision gate after this cut (vite, same commit): every absent case still held, the control held, 28,893 edges unchanged in total, and 12 edges moved from `exact-match` to `import` (7,250 → 7,238 and 5,410 → 5,422): symbols exported by a later statement are now visible to the import resolver's export index, so the reference binds through the import instead of a name guess. `fuzzy` stayed at 16. Report: `__tests__/evaluation/results/precision-vite-8492422-72a5703b.json`.

Exit met for the cut: the later-export regexes are deleted in both engines and the store-gate and export flag read the AST scan and the table.

### Phase 2: resolution reads bindings for TS/JS — DONE 2026-09-12 (first cut)

Rows the walker now emits, on top of Phase 1 (`codegraph-kernel/src/tsjs/bindings.rs`, a pre-walk that runs before the node walk):

- `param` rows for every function, method, arrow and `catch` parameter, including destructured, defaulted and rest names, scoped to the function's lines.
- `local` rows, without a node, for a function-body `const`/`let`/`var` whose value is not a function (the walk makes those nodes). Two shapes are deliberately not rows, matching the carve-outs the regex made: a destructured member (`const { fetchUser } = useStore.getState()`) and a store selector (`const picked = useStore((s) => s.picked)`) — the graph's symbol is what a call through them means.
- `import` rows for `require('x')` and `await import('x')`, bare or destructured (`{ a: b }`), at any scope. A module-level `const m = require(..)` is node-backed, so a later `module.exports = { m }` can still export it.
- CommonJS exports anywhere in the tree, not only at the top level: `module.exports = { a, b: c }` (`cjs-object`), `module.exports = NAME` (as `default`), `exports.x = NAME` / `exports['x'] = NAME` / `module.exports.x = NAME` (`cjs`). An imported name a later clause exports (`import X from './x'; export { X }`) carries the export on its `import` row.
- `reexport` rows for `export * from`, `export * as ns from` (name `*`) and `export { default as X } from`.
- A nodeless `default` row for `export default <expression>` (a config file's `export default defineConfig(..)`), so such a file is not read as exporting nothing. `export default function NAME` is `default` / `esm-default` (the declaration must be the statement's own); a declaration inside `declare global` is exported as itself with form `public`.

Resolution (`ResolutionContext.getBindings(file)`, LRU-cached with the other per-file caches; `innermostBinding(rows, name, line)` picks the narrowest row whose scope holds the line):

- `isBoundToBareImport` reads the innermost `import` row; `isLocallyBoundJsName` reads the innermost `decl`/`local`/`param` row **at the reference's line** — a parameter now shadows only inside its own function, where the regex shadowed the whole file; `isSealedModule` is "has an ESM `import` node, no row with `exported_as`, no exported node"; `defaultExportBinding` and the local export aliases come from rows with `exported_as`; `getImportMappings` and `getReExports` are built from `import` / `reexport` rows.
- In the first cut every one of these kept its regex path as the fallback for a file with no rows (SFC blocks, the generic extractor). The second cut, below, gave those paths rows and deleted the regexes.
- Not touched in this cut: `isBareJsCall` (a question about the call site's shape, not a binding), `isTsJsNestedCall` and the host-global list (the §2.3 receiver rule), `alias` rows (`alias-binding.ts` reads node signatures, not source).

Precision gate (three corpora, all absent cases and the control held; baselines are the Phase 0 reports):

| corpus | edges before → after | exact-match | import | other |
|---|---|---|---|---|
| vite | 28,893 → 28,826 (−67) | 7,238 → 6,958 | 5,422 → 5,635 | fuzzy 16 → 16 |
| vitest | 75,035 → 74,987 (−48) | 13,647 → 12,398 | 31,966 → 33,190 | framework 86 → 64, fuzzy 28 → 26 |
| svelte | 70,432 → 70,603 (+171) | 16,731 → 16,648 | 8,468 → 8,722 | fuzzy 159 → 159 |

Edge-level review on vite (same extraction, rows on vs off): 289 edges lost, 221 gained; 201 of the lost references re-resolved through `import` (dynamic `await import('./x')` and `require` destructurings now follow the import instead of a name guess). Of the 76 references that lost their edge outright: bare and dynamic-bare imports (`const { createServer } = await import('node:http')` no longer lands on vite's `createServer`; `import type { OutputChunk } from 'rolldown'` no longer lands on a same-named local interface), parameters (`.then(({ lazyLoad }) => lazyLoad())`), function-body locals (`render = (await import('./dist/app.js')).render`), and edges that were never right (`type` read as a default import of `environment.ts` and resolved to `DevEnvironment`; ). One class in that list was misjudged at the time: 41 bare `log()` calls onto `playground/hmr-ssr/event.d.ts`'s `let log` inside `declare global`, a name that block really does give every file. The second cut restored them (see below).

Golden diff: binding rows only (57 `param`, 7 `local`, an `import` for a `require`, a wildcard `reexport`, a nodeless `default`), plus one edge: `a()` at `torture.tsx:243` now resolves cross-language onto a Dart `enum_member` named `a` by exact match. The regex had masked it (a parameter `a` elsewhere in the file counted as a file-wide shadow); the real defect was that a bare JS call could land on any kind. Fixed in the follow-up below.

Tests: `__tests__/bindings-tsjs.test.ts` (every new row form) and `__tests__/bindings-resolution.test.ts` (six end-to-end shapes the regexes could not answer). `resolution.test.ts`, the #1794 and #1762 suites pass unchanged.

**Second cut, rows for every TS/JS path and the regexes deleted (same day).**

- `codegraph-kernel` gains `bindings_file(file, source, language)`: binding rows from the AST alone, without the node walk, for the files the walker does not extract — a stack-guard defer and ArkTS, which the generic extractor extracts. `tsjs::bindings_only` runs the same pre-walks the walker runs (`collect_later_exports`, `collect_scoped_bindings`) plus an AST-only declaration pass and import/re-export pass (`bindings.rs`). Every pre-walk is iterative, so a file too deep for the recursive walker still gets its rows. §2.4 holds: one emitter, in the kernel; the TypeScript side only attaches node ids to `decl` rows by name and line (`attachBindingNodeIds`).
- The walker's own rows now take their scope from the AST (`enclosing_scope`), not the node stack: a function declared inside an IIFE or a callback is `local` to it. `declare global { }` and `namespace X { }` bodies are not scopes. `exports.x = function () {}` exports through the same `cjs_fn_exports` list on both paths.
- Vue, Svelte and Astro rebase their script blocks' rows to file positions and carry them; Razor's C# block has none.
- The parity gate (`kernel-generic-extractor-tree.test.ts`) compares the AST-only rows with the walker's on the JS-family fixtures: every AST-only row is a walker row, and a walker-only row is node-backed (a class member, an object-literal action whose export flag lives on the node).
- Deleted from `src/resolution/` (434 lines net): `extractJSImports`, the JS body of `extractReExports` and `stripJsComments`, `DEFAULT_EXPORT_BINDING_RE`, `extractLocalExportAliases`, the three sealed-module regexes and the local-binding regexes with their memo. `isBoundToBareImport` keeps `getImportMappings` as its no-rows fallback because that is a context hook, not a regex (a test context supplies mappings by hand). `EXTRACTION_VERSION` 30 → 31 so existing indexes re-index into rows for every file.
- Gate: all cases held. vite 28,688 → 28,731 (+43: the 41 `declare global` `log()` edges above came back, plus file-path imports), vitest 74,887 → 74,888, svelte 70,589 → 70,589 (the SFC rows reproduce the whole-file regex's mappings exactly).

**Follow-up, kind eligibility for bare calls (same day).** A receiver-less JS/TS call may only resolve to a function, class, component, constant or variable (`BARE_CALL_TARGET_KINDS`, `name-matcher.ts`). The rule rejects the candidate exact-match would commit to; it does not filter the candidate set. The filtering form was tried first and manufactured 192 edges on vitest (dropping the property candidates from a crowd left a lone wrong survivor: every dynamic `import(...)` landed on a function named `import`, `trace()` on a Vue constant); the rejection form lost 102 property-target edges and gained 2 fuzzy ones. Corpus totals after: vite 28,826 → 28,688, vitest 74,987 → 74,887, svelte 70,603 → 70,589, all cases held. Goldens: the `a()` → Dart `enum_member` edge and an `expensive()` → Swift `field` edge are gone.

**Windows validation (2026-09-12).** `fork/consolidated` at `91902770` (PRs #29, #30, #31) on the Windows-local checkout (`C:\Users\bompus\src\codegraph-win`, Node 26.8.1, cargo 1.98.1, MSVC): `npm ci`, `bash scripts/build-kernel.sh` (53 s, 76 MB `win32-x64` prebuild, no source change needed), `npm run build`; the 14 binding-model and kernel suites pass (426 tests, golden dumps byte-identical), and the full suite passes: 268 files, 4,736 tests, 7 files skipped by the POSIX-only gates.

### Phase 3: other languages

Per language, in order of resolver regex weight: Python, Go, Java/Kotlin, C/C++ (`storage='static'`), Rust (visibility), PHP, Ruby, C#, Swift. Each phase deletes that language's import-extractor regex and its rows in the receiver inference table.

Exit per language: its `extractXImports` function and inference table entries are deleted.

### Phase 4: move the binding lookup into the kernel

With source-reading predicates gone, the resolve step is a join over `bindings`, `nodes` and `unresolved_refs`. Port it into the kernel as a batch entry point that takes a chunk of refs and returns resolved edges, mirroring today's `resolver-worker` chunk contract. The TypeScript `ReferenceResolver` becomes the orchestrator over the kernel and the framework resolvers. This is the P1 item in the migration plan, executed after the model is stable rather than before.

Exit: `settle` and `read` stages run natively; Linux-kernel resolution under the §7a target.

## 4. What is removed

| Item | Location |
|---|---|
| `isExportedLater` (both engines) — done, Phase 1 | `tree-sitter.ts:2451`, `extractors.rs:359` |
| `isSealedModule`'s three regexes — done, Phase 2 (the predicate stays, over rows) | `name-matcher.ts:621-692` |
| `isBoundToBareImport`, `isBareJsCall`, `isLocallyBoundJsName` and their memos — `isLocallyBoundJsName`'s regexes and memo done, Phase 2; the other two stay (bare-import classification and call-site shape) | `name-matcher.ts:551-970` |
| `isTsJsNestedCall` early-out and the host-global chain gate | `index.ts:1030`, `name-matcher.ts:3423`, `js-builtins.ts` |
| `DEFAULT_EXPORT_BINDING_RE`, `extractLocalExportAliases` — done, Phase 2 | `import-resolver.ts:96`, `alias-binding.ts:105` |
| Per-language import regex extractors — JS/TS done, Phase 2; Python, Go, JVM, PHP, C/C++ wait on Phase 3 | `import-resolver.ts:898-1174` |
| Receiver inference regex table, as languages migrate | `name-matcher.ts:1950-2082` |
| `strip-comments.ts` once no resolver reads source | 574 lines |

Estimated: 4k to 6k lines of `src/resolution/` deleted, replaced by one table, one context accessor, and walker emission code in the kernel.

## 5. Risks

- **Re-index required.** The table is empty until re-index; readers must union with `nodes.isExported` until then, matching the existing DDL-only migration rule.
- **Emission completeness.** A binding the walker misses becomes an unresolved reference, not a wrong edge. That is the failure direction the maintainer asked for in #1566 and every PR since. Track "unresolved with reason" counts per corpus so misses are visible.
- **Framework resolvers.** They keep their own evidence and run first. Some read source today (`extract` hooks); those are out of scope here but should migrate to bindings when they touch a language that has them.
- **Upstream.** The table and the phase-1 walker change are contributable upstream as the resolution of #1721. Phases 2 onward diverge from upstream's regex predicates and will need care when merging their resolution fixes; most of those fixes become no-ops once the predicate they patch is gone.
