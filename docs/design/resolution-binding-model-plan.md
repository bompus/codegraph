# Resolution binding model — one source of truth for exports and bindings

**Status:** Phases 0 to 2 done (2026-09-12; the Phase 2 exit criterion is met, no source regex remains for TS/JS in the resolver); Phase 3 done (2026-09-12): Python, Go, Java, Kotlin, PHP and C/C++ emit rows and every per-language import regex is deleted; Phase 4 not started. Written 2026-09-11. Companion to [kernel-only-extraction-plan.md](kernel-only-extraction-plan.md) (which should land first, so there is one extractor to emit the new facts) and [greenfield-rust-core-sketch.md](greenfield-rust-core-sketch.md). Closes upstream issue #1721 and ends the fix cycle behind #1566, #1790, #1794 and #1844.

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

#### Python — DONE 2026-09-12 (import regex retired)

- The Python walker (`codegraph-kernel/src/python.rs`) emits: `decl` rows for every module-level definition, exported as itself with form `public` (a leading underscore is `storage = private`, the convention, not a boundary); `local` rows for a definition nested in a function or class and for an assignment inside a function body (nodeless); `param` rows for every parameter form (plain, default, typed, `*args`, `**kw`); `import` rows for `import a`, `import a.b as c`, `import x, y`, `from m import a, b as c` and relative `from .m` / `from ..m` (the module as written is `target_spec`; a whole-module import has `target_name = *`). `from m import *` emits no row, as the regex emitted no mapping.
- `import a.b` keeps the resolver's long-standing reading of the local name (the last segment, `b`) rather than what Python binds (`a`), so the import mappings are byte-equal to the regex's; correcting it belongs with the Python receiver rule.
- `extractPythonImports` is deleted; `getImportMappings` reads the rows. Python nodes at module level are now `isExported` (from the rows), where the walker had left the flag off.
- Gate: a Python corpus was added to the precision runner (pallets/flask at `d73fa1cd`, one control: `from .helpers import get_debug_flag` in `app.py` resolves). Baseline 5,268 edges (import 90, exact-match 1,341, fuzzy 1); after: 5,292 (import 371, exact-match 1,110, fuzzy 1). Edge-level review (same corpus, previous walker vs this one): 267 references moved from a name guess to `import` now that the definitions are exported; 0 lost outright; 24 gained outright, all resolved by import (18 calls through an imported signal variable, `request_started.send(...)` → the `signals.py` variable, the same root-binding the JS path produces for an imported constant; 6 class references). Goldens: `torture.py` rows and export flags only, no edge or ref changes.
- An import inside a function body (`from . import db` in flask's `create_app`) is a row scoped to that function; the first cut missed it and lost one edge the file-wide regex had found.
- `bindings_file` handles Python too (`python::bindings_only`, an iterative AST-only pass with the same rules), so the generic extractor's path (`CODEGRAPH_KERNEL=0`, a stack-guard defer) has rows and the parity gate covers `torture.py`. Found by the extraction suite, where a test had left `CODEGRAPH_KERNEL=0` set for every later test in the file; the leak is fixed too.
- One resolver rule came with the export flag: a chained call resolved by import onto a plain function or method is declined (`send_welcome.delay(x)` after `from .tasks import send_welcome` is Celery's dispatch on the task object, not a call to the function). Binding it hid the queue step in the Steps view; the celery-dispatch synthesizer keeps the edge it always made.
- `__tests__/bindings-python.test.ts` pins every row form and the mappings built from them.

#### Go — DONE 2026-09-12 (import regex retired)

- The Go walker (`codegraph-kernel/src/go.rs`) emits: `decl` rows for every package-level name, exported by case (`public` when capitalized, else `storage = package`); `local` rows for names nested in a type or function (node-backed) and, nodeless, for `x := …`, `var x` and `for i, v := range` inside a function body; `param` rows for parameters and receivers (functions, methods, closures the walk names, interface method specs); `import` rows per import spec with the alias as spelled (`str "strconv"`, a dot or blank import) or the path's last segment — the resolver's long-standing reading, since a package's declared name can differ from its path. Go set `isExported` by case for functions and types but not for every package-level kind; the rows now set it by case for all of them (constants, variables, methods), which the goldens show as flag-only node changes.
- `bindings_file` handles Go (`go::bindings_only`, iterative AST-only pass), so the generic extractor's path has rows; the parity gate covers `torture.go`.
- `extractGoImports` is deleted; `getImportMappings` reads the rows.
- Gate: a Go corpus was added (gin-gonic/gin at `dcaa4296`, no cases yet). 8,544 edges before and after, byte-identical histogram (import 58, exact-match 2,895). The edge dump differs in 11 edges, all one shape: `jsonBinding{}.BindBody(…)` in the binding tests, a call whose receiver is a composite literal, name-matched among ten same-named `BindBody` methods; the tie moved from the interface's method to `bsonBinding`'s because method export flags are now consistent by case. Both picks are wrong (the receiver names `jsonBinding`); the Go receiver rule in Phase 2b/§2.3 is where that gets fixed. Deterministic: two runs of the new build agree. Goldens (`payroll-go`, `torture.go`): rows and export flags only, no edge or ref changes.
- `__tests__/bindings-go.test.ts` pins every row form and the mappings built from them.

#### Java and Kotlin — DONE 2026-09-12 (JVM import regex retired)

- Both walkers (`java.rs`, `kotlin.rs`) emit: `decl` rows for file-level declarations with the modifier as the export — `public` (Kotlin's default) exports, `protected` exports with `storage = protected`, `private` and Kotlin `internal` do not export (`storage = private` / `internal`), and Java's package-private default is `storage = package`; node-backed `local` rows for members, scoped to their class; `param` rows for method and function parameters; nodeless `local` rows for method-body variables (Java `local_variable_declaration`, Kotlin `val`/`var` inside a function body); `import` rows for every non-wildcard import with the FQN as `target_spec` and the alias (Kotlin `as`) or last segment as the local name. The package declaration's `namespace` node is scaffolding, not a scope or a binding.
- `bindings_file` handles both (`java::bindings_only`, `kotlin::bindings_only`); the parity gate covers `Torture.java`, `torture.kt` and `TortureScript.kts`. Aligning the AST-only pass with the walk fixed the walk's scoping rules into words: an anonymous class body is a scope in Java; in Kotlin a companion object, an object literal and an enum entry body are not scopes, a local `object` inside a function is not a node, a class-level `val` is a field, and a top-level `val x = object { fun run() }` scopes `run` to the property. A Kotlin property node created without a visibility now takes it from its `property_declaration`.
- `extractJavaImports` is deleted. It required a trailing `;`, so Kotlin files had never had import mappings; they do now.
- Gate, Java (spring-petclinic at `818c4136`): 1,457 edges before and after, byte-identical dump — the rows reproduce the regex's mappings exactly.
- Gate, Kotlin (JetBrains/Exposed at `2155404`): 80,302 → 80,573 edges; import 3,595 → 8,032, exact-match 39,041 → 36,827, qualified-name 2,080 → 909, framework 3,310 → 2,603, fuzzy 7 → 31. Edge-level review: 34,730 references unchanged, 2,893 relabelled to `import` with the same target, 2,678 with a different target, 0 lost, 261 gained. Of the target changes that moved to `import`, 480 crossed from a same-named class in the wrong module (jdbc vs r2dbc, whose source trees mirror each other) to the one the file's own import names, 0 went the other way, 15 landed in a third module. The 1,750 exact-match → exact-match target changes are ties among same-named candidates re-broken now that Kotlin's file-level export flags are set (65 flips in the golden); sampled, they move from doc-snippet methods to the library's extension functions (`batchInsert`, `union`). The 24 new fuzzy edges are unique-callable guesses the previous unresolved refs now reach; the fuzzy rule is unchanged.
- `__tests__/bindings-jvm.test.ts` pins every row form for both languages.

#### PHP — DONE 2026-09-12 (import regex retired)

- The PHP walker (`php.rs`) emits: `decl` rows for file-level declarations, exported as themselves (PHP has no file-level visibility); node-backed `local` rows for members; `param` rows (names as written, with `$`); nodeless `local` rows for `$x = …` in a function body; `import` rows for every `use` clause — single, aliased (`use A\B as C`), grouped (`use A\{B, C as D}`), `use function` and `use const` — with the imported name as `target_spec` and the alias or last segment as the local name. A trait `use` inside a class body is not an import. The walk makes no nodes inside `new class { … }`, and the AST-only pass mirrors that.
- The regex had matched trait `use` statements as imports and missed grouped, `use function` and `use const` forms; the rows do the reverse.
- `bindings_file` handles PHP; the parity gate covers `torture.php` and `TortureHtml.php`. `extractPHPImports` is deleted. File-level PHP declarations are now `isExported` (15 flips in the golden).
- Gate (slimphp/Slim at `3675bf6b`): 5,016 edges before and after, byte-identical dump (2,830 references, none changed); Slim's `use` statements are all the single form the regex already read. Goldens (`php-import-alias-static`, `torture-multilang`): rows and export flags only, no edge or ref changes.
- `__tests__/bindings-php.test.ts` pins every row form.

#### C and C++ — DONE 2026-09-12 (include regex and static-function source check retired; Phase 3 complete)

- The C/C++ walker (`ccpp/mod.rs`) emits: `decl` rows for file-level definitions, exported as themselves, with `storage = static` when the definition carries the storage class (the resolver keeps its header exemption: a `static inline` in a header exists in every includer); node-backed `local` rows for members; `param` rows (the identifier under any pointer, reference or array wrapping; a function-pointer parameter names nothing, as the walk's own declarator reader); nodeless `local` rows for a function body's declarations; `import` rows for every `#include`, the header's basename without its extension as the local name and the path as written. A `namespace` block is a name prefix, not a scope.
- `bindings_file` handles C and C++; the parity gate covers `torture.c`, `torture.cpp` and `torture.hpp`. Aligning it pinned more of the walk: a definition the parser mangled (no `declarator` field, a keyword name, an ERROR in its parameter list) is no node and its children stay at the enclosing scope; `class MACRO Name : Base { … }` parses as a function definition and is recovered as the class; a specifier without a body is skipped with its subtree; C file-scope variables take init / pointer / array declarators only, C++ only a bare identifier; class-body declarations are fields, not locals. The AST-only path now also applies the walker's offset-preserving pre-parse (C/C++ macro blanking), which the first cut had missed.
- `extractCppImports` is deleted, and `isStaticCFunction` reads `storage` from the row instead of the definition's first two source lines. `extractImportMappings` now returns nothing for every language and remains only as the context hook's implementation.
- Gate, C (jqlang/jq at `9d241e27`): 7,712 → 7,713 edges (import 115 → 117); 4,597 references unchanged, 2 relabelled, 0 target changes, 0 lost, 1 gained.
- Gate, C++ (nlohmann/json at `aa391dc0`): 24,640 → 24,647 edges; import 644 → 686 (+42, mostly class references resolved through an include row the regex had not produced); exact-match 7,188 → 7,153. Edge review: 11,015 references unchanged, 319 with a different target, 2 lost, 43 gained. The 319 are ties among same-named C++ entities re-broken now that file-level definitions are `isExported` and class members are not: a bare `begin(…)` in a test moves from another test's `alt_string_iter::begin` member to a free `begin` function; `array` in an arithmetic helper moves from the `value_t::array` enum member to the `nlohmann::array` function. The 2 lost are `iteration_proxy<iterator>(*this)` constructor calls that had name-matched the constructor. Goldens: rows and export flags only (42 C and 27 C++ file-level nodes now `isExported`), no edge or ref changes.
- `__tests__/bindings-ccpp.test.ts` pins every row form.

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
| Per-language import regex extractors — all done (JS/TS in Phase 2, the rest in Phase 3) | `import-resolver.ts:898-1174` |
| Receiver inference regex table, as languages migrate | `name-matcher.ts:1950-2082` |
| `strip-comments.ts` once no resolver reads source | 574 lines |

Estimated: 4k to 6k lines of `src/resolution/` deleted, replaced by one table, one context accessor, and walker emission code in the kernel.

## 5. Risks

- **Re-index required.** The table is empty until re-index; readers must union with `nodes.isExported` until then, matching the existing DDL-only migration rule.
- **Emission completeness.** A binding the walker misses becomes an unresolved reference, not a wrong edge. That is the failure direction the maintainer asked for in #1566 and every PR since. Track "unresolved with reason" counts per corpus so misses are visible.
- **Framework resolvers.** They keep their own evidence and run first. Some read source today (`extract` hooks); those are out of scope here but should migrate to bindings when they touch a language that has them.
- **Upstream.** The table and the phase-1 walker change are contributable upstream as the resolution of #1721. Phases 2 onward diverge from upstream's regex predicates and will need care when merging their resolution fixes; most of those fixes become no-ops once the predicate they patch is gone.
