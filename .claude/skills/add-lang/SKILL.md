---
name: add-lang
description: Add tree-sitter language support to codegraph end-to-end — wire the grammar + extractor, write tests, then benchmark extraction quality and retrieval value on 3 popular real-world repos. Use when the user runs /add-lang <language> or asks to add/support a new language (e.g. Lua, Elixir, Zig, OCaml) in codegraph.
---

# Add a language to CodeGraph

Wire a new tree-sitter language into codegraph's extraction pipeline, prove it
extracts real symbols on popular repos, and prove it beats no-codegraph for an
agent. Runs **fully autonomously** — pick repos, benchmark, update docs, then
report, then land the verified change per AGENTS.md (commit on a task branch,
integrate into `fork/consolidated` through a PR). Never publish or tag.

The argument is the language token used throughout the `Language` union, e.g.
`lua`, `elixir`, `zig`. If none was given, ask which language. Use the lowercase
single-token form everywhere (`csharp`, not `c#`).

## Prerequisites
- Run from the codegraph repo root. `node`, `git`, `gh`, and a logged-in
  `claude` CLI (the benchmark spawns real `claude -p` runs).
- The benchmark uses the local dev build — Step 8 builds + links it on PATH.

## Workflow

Copy this checklist and work through it in order:
```
- [ ] 1. Resolve language; bail early if already supported (just benchmark)
- [ ] 2. Add the grammar to the native kernel
- [ ] 3. Discover the grammar's AST node types
- [ ] 4. Wire the language (TS side; sometimes a core touch)
- [ ] 5. Build + verify-extraction loop until PASS
- [ ] 6. Add extraction tests; make them green
- [ ] 7. Auto-pick 3 popular repos by size tier; add to corpus.json
- [ ] 8. Benchmark all 3: extraction + with/without A/B
- [ ] 9. Update README + CHANGELOG
- [ ] 10. Report and land
```

### Step 1 — Resolve + short-circuit

Check whether the language is already wired: look for the token in the
`LANGUAGES` const (`src/types.ts`) and the `EXTRACTORS` map
(`src/extraction/languages/index.ts`). If it is already supported (e.g.
`typescript`, `rust`), **skip Steps 2–6** and go straight to benchmarking
(Steps 7–8) to validate/measure it — note in the report that no code changed.

### Step 2 — Add the grammar to the native kernel

The native kernel (`codegraph-kernel/`, Rust) is the only parser; there is no
wasm path. Add the grammar in one of two ways:

- **crates.io** — pin the grammar crate in `codegraph-kernel/Cargo.toml`
  (recent entries use an exact `=x.y.z` pin).
- **vendored C** — when no usable crate exists, copy the grammar's generated
  `parser.c` (+ `scanner.c`, headers) into `codegraph-kernel/grammars/<lang>/`,
  compile it in `codegraph-kernel/build.rs` like `lua`/`dart`, and add a row
  to `codegraph-kernel/grammars/PROVENANCE.md` (source, revision, ABI).

Then map the language token to the grammar in `grammar_for`
(`codegraph-kernel/src/langs.rs`). A language walked by the generic TypeScript
extractor goes in the parse-only block; only a language with a bespoke Rust
walker also joins `LANGUAGES` there. Build and stage the kernel:
```bash
npm run build:kernel
```
The grammar's ABI must be one the kernel's `tree-sitter` crate accepts; a
build or `set_language` failure means a newer grammar revision is needed.
**If you cannot obtain a working grammar, STOP and tell the user.**
See `docs/design/kernel-only-extraction-plan.md` for the parse-only path.

### Step 3 — Discover AST node types

`scripts/add-lang/dump-ast.mjs` and `check-grammar.mjs` load grammars through
`web-tree-sitter`, which this fork no longer installs, so they do not run.
Get the node types from the grammar itself: its `src/node-types.json`, or
`tree-sitter parse <sample>` from the grammar's repository (tree-sitter CLI).
Use a representative sample covering functions, classes/structs, imports and
enums. The node names and field names (`name:`, `parameters:`, `body:`,
`return_type:`) tell you what to map. Open the existing extractor closest to the
language's paradigm as a model: `rust.ts`/`scala.ts` (functional, traits),
`java.ts`/`csharp.ts` (OO), `python.ts`/`ruby.ts` (scripting), `go.ts`
(top-level methods + receivers).

### Step 4 — Wire the language (TS side)

These are exact, fragile wiring — match the existing style precisely:

1. **`src/types.ts`** — add `'<lang>',` to the `LANGUAGES` const (before
   `'unknown'`).
2. **`src/extraction/grammars.ts`** — three entries:
   - `GRAMMAR_LANGUAGES`: add `'<lang>',`
   - `EXTENSION_MAP`: each file extension → `'<lang>'` (e.g. `'.lua': 'lua',`).
     This is also the file-scan allowlist (`isSourceFile`): an extension
     missing here means `codegraph init` finds 0 files.
   - `getLanguageDisplayName`: `<lang>: '<Display Name>',`
3. **`src/extraction/languages/<lang>.ts`** — new file exporting
   `export const <lang>Extractor: LanguageExtractor = { … }`. Map the node types
   from Step 3. Required fields: `functionTypes`, `classTypes`, `methodTypes`,
   `interfaceTypes`, `structTypes`, `enumTypes`, `typeAliasTypes`,
   `importTypes`, `callTypes`, `variableTypes`, `nameField`, `bodyField`,
   `paramsField`. Add hooks as the grammar needs them (`getSignature`,
   `getVisibility`, `isExported`, `extractImport`, `visitNode`, `getReceiverType`,
   `interfaceKind`, `enumMemberTypes`, etc. — see
   `src/extraction/tree-sitter-types.ts`).
4. **`src/extraction/languages/index.ts`** — `import { <lang>Extractor } from
   './<lang>';` and add `<lang>: <lang>Extractor,` to `EXTRACTORS`.

**Sometimes a core touch in `src/extraction/tree-sitter.ts`** — variable
extraction has per-language branches in `extractVariable` (the generic fallback
only finds direct `identifier`/`variable_declarator` children). If the grammar
nests declared names (e.g. Lua's `variable_declaration → variable_list`), add a
`} else if (this.language === '<lang>')` branch there, mirroring the existing
ts/python/go ones. Import forms that aren't a distinct node (Lua/Ruby `require`
is a *call*) are handled in the extractor's `visitNode` hook instead.

### Step 5 — Build + verify loop

```bash
npm run build:kernel && npm run build
```
Index a small sample repo and check extraction:
```bash
( cd <sample-repo> && codegraph init )
node scripts/add-lang/verify-extraction.mjs <sample-repo> <lang>
```
`verify-extraction.mjs` fails (exit 1) if the language isn't detected or only
`file`/`import` nodes were produced — the classic symptom of wrong node-type
names. On FAIL or a thin WARN: re-check the node types (Step 3) against a richer
sample, fix the mappings in `<lang>.ts`, `npm run build`, re-index, re-verify. **Repeat until
PASS.**

### Step 6 — Tests

Add to `__tests__/extraction.test.ts`, modeled on the `Rust Extraction` block:
- a `detectLanguage` assertion in `describe('Language Detection')`
- a `describe('<Lang> Extraction')` block asserting functions/classes/imports
  are extracted from an inline source string.
```bash
npx vitest run __tests__/extraction.test.ts
```
Green before continuing.

### Step 7 — Auto-pick 3 repos + corpus

Pick **without asking**. Find candidates, then curate 3 that are genuinely
`<lang>`-dominant, one per size tier:
```bash
gh search repos --language=<lang> --sort=stars --limit 40 \
  --json fullName,stargazerCount,description
```
Tiers (match `corpus.json`): **Small** <~150 files · **Medium** ~150–1500 ·
**Large** >~1500. Skip repos that are tagged `<lang>` but mostly another
language. Write one cross-file architecture **question** per repo (the kind that
needs tracing across files). Add a `"<Language>"` block to
`.claude/skills/agent-eval/corpus.json` (fields: `name`, `repo`, `size`,
`files`, `question`) so `/agent-eval` can reuse them.

### Step 8 — Benchmark all 3 (extraction + A/B)

Make the dev build the codegraph on PATH **once**, then loop:
```bash
npm run build && ./scripts/local-install.sh
scripts/add-lang/bench.sh <lang> <name> <url> "<question>" headless   # ×3
```
`bench.sh` clones into `$CORPUS` (set it to a workspace-disk directory; the
`/tmp` default is shared tmpfs), wipes + indexes, runs
`verify-extraction.mjs`, then the with/without retrieval A/B via
`scripts/agent-eval/run-all.sh` (skips the paid A/B if extraction is broken).
Read each `parse-run.mjs` summary printed by `run-all.sh`: tool calls, file
`Read`s, Grep/Bash, codegraph-tool calls, duration, and **cost** — for both the
`with` and `without` arms. After the loop, restore the dev link if needed:
`./scripts/local-install.sh`.

### Step 9 — Docs + CHANGELOG

- **README.md**: add `<Lang>` to the `N+ Languages` row of the features table, and add a
  row to the **Supported Languages** table:
  `| <Lang> | \`.ext\` | Full support (classes, methods, …) |`.
- **CHANGELOG.md**: under `## [Unreleased]` at the top (create it above the
  latest version if missing), add a user-perspective bullet under
  `### New Features`, e.g.
  *"CodeGraph now indexes **<Lang>** (`.ext`) — functions, classes, imports, and
  call edges."* If `## [Unreleased]` already exists, append under it. (It's
  folded into the next versioned block at release time.)

### Step 10 — Report and land

Summarize for review:
- **Files changed**: the kernel grammar (Cargo pin or vendored C + `langs.rs`),
  the TS wiring + new extractor + tests + README + CHANGELOG + corpus.json.
- **Extraction** per repo: files / nodes / edges / `verify-extraction` result.
- **A/B** per repo: `with` vs `without` (tool calls, file Reads, cost) and a
  one-line verdict — did codegraph reduce effort, and did both arms reach a
  correct answer?
- **Gaps / follow-ups** (node types not yet mapped, resolution edges missing,
  framework routes, etc.).

Land the change per AGENTS.md's completion boundary, with the summary above as
the PR body. Do not publish or tag: this fork publishes no releases (AGENTS.md
§ Releases).

## Notes
- The A/B spawns real **paid** `claude -p` runs (Sonnet at `--effort high` by
  default, `--max-budget-usd`),
  2 arms × 3 repos. Set `CORPUS` to the same workspace-disk directory
  `/agent-eval` uses (never the `/tmp` default, which is shared tmpfs), so
  clones are reused across runs.
- An index must be served by the **same** binary that built it. Step 8 builds +
  links the dev build first, so this holds.
- If a grammar can't be obtained, or extraction can't reach PASS, **STOP and
  report** — don't ship a half-wired language.
