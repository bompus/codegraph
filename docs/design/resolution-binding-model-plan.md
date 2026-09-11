# Resolution binding model — one source of truth for exports and bindings

**Status:** plan, not started. Written 2026-09-11. Companion to [kernel-only-extraction-plan.md](kernel-only-extraction-plan.md) (which should land first, so there is one extractor to emit the new facts) and [greenfield-rust-core-sketch.md](greenfield-rust-core-sketch.md). Closes upstream issue #1721 and ends the fix cycle behind #1566, #1790, #1794 and #1844.

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

### Phase 0: precision in the eval runner

- Add a precision score to `__tests__/evaluation/scoring.ts`: for a fixed corpus (vite, vitest, svelte, rollup, the ones the PR bodies used), a checked-in list of known-wrong edges that must stay absent and known-right edges that must stay present. This is the gate every later phase runs against.

Exit: the LOST/GAINED tables from #1713, #1718, #1746 and #1844 are encoded as tests.

### Phase 1: emit bindings for TS/JS

- Add the `bindings` table and migration (DDL only, empty until re-index).
- Kernel `tsjs` walker emits rows for declarations, imports, re-exports, aliases, parameters, and block-scoped locals, with `export_form` covering ESM, later-export, default, `module.exports.X`, `module.exports = {}`, and `exports["x"]`.
- Set `nodes.isExported` from the table. Delete `isExportedLater` in both engines.
- Bump `EXTRACTION_VERSION`.

Exit: `store-exported-later.test.ts` and `commonjs-exports.test.ts` pass against the table, not the regexes.

### Phase 2: resolution reads bindings for TS/JS

- Add `bindingsFor` to the context. Replace `isBoundToBareImport`, `isBareJsCall`, `isLocallyBoundJsName`, `isSealedModule`, `DEFAULT_EXPORT_BINDING_RE`, `extractLocalExportAliases`, `extractJSImports` and `extractReExports` with lookups.
- Replace `isTsJsNestedCall` and the host-global list with the receiver rule in §2.3.
- Run the Phase 0 precision corpus and `resolution.test.ts`.

Exit: no `RegExp` construction over file source remains in `name-matcher.ts` or `import-resolver.ts` for TS/JS. The 8 tests from #1794 and the store tests from #1762 pass with no carve-outs.

### Phase 3: other languages

Per language, in order of resolver regex weight: Python, Go, Java/Kotlin, C/C++ (`storage='static'`), Rust (visibility), PHP, Ruby, C#, Swift. Each phase deletes that language's import-extractor regex and its rows in the receiver inference table.

Exit per language: its `extractXImports` function and inference table entries are deleted.

### Phase 4: move the binding lookup into the kernel

With source-reading predicates gone, the resolve step is a join over `bindings`, `nodes` and `unresolved_refs`. Port it into the kernel as a batch entry point that takes a chunk of refs and returns resolved edges, mirroring today's `resolver-worker` chunk contract. The TypeScript `ReferenceResolver` becomes the orchestrator over the kernel and the framework resolvers. This is the P1 item in the migration plan, executed after the model is stable rather than before.

Exit: `settle` and `read` stages run natively; Linux-kernel resolution under the §7a target.

## 4. What is removed

| Item | Location |
|---|---|
| `isExportedLater` (both engines) | `tree-sitter.ts:2451`, `extractors.rs:359` |
| `isSealedModule` and its three regexes | `name-matcher.ts:621-692` |
| `isBoundToBareImport`, `isBareJsCall`, `isLocallyBoundJsName` and their memos | `name-matcher.ts:551-970` |
| `isTsJsNestedCall` early-out and the host-global chain gate | `index.ts:1030`, `name-matcher.ts:3423`, `js-builtins.ts` |
| `DEFAULT_EXPORT_BINDING_RE`, `extractLocalExportAliases` | `import-resolver.ts:96`, `alias-binding.ts:105` |
| Per-language import regex extractors | `import-resolver.ts:898-1174` |
| Receiver inference regex table, as languages migrate | `name-matcher.ts:1950-2082` |
| `strip-comments.ts` once no resolver reads source | 574 lines |

Estimated: 4k to 6k lines of `src/resolution/` deleted, replaced by one table, one context accessor, and walker emission code in the kernel.

## 5. Risks

- **Re-index required.** The table is empty until re-index; readers must union with `nodes.isExported` until then, matching the existing DDL-only migration rule.
- **Emission completeness.** A binding the walker misses becomes an unresolved reference, not a wrong edge. That is the failure direction the maintainer asked for in #1566 and every PR since. Track "unresolved with reason" counts per corpus so misses are visible.
- **Framework resolvers.** They keep their own evidence and run first. Some read source today (`extract` hooks); those are out of scope here but should migrate to bindings when they touch a language that has them.
- **Upstream.** The table and the phase-1 walker change are contributable upstream as the resolution of #1721. Phases 2 onward diverge from upstream's regex predicates and will need care when merging their resolution fixes; most of those fixes become no-ops once the predicate they patch is gone.
