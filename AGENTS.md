# AGENTS.md

Canonical project guidance for coding agents working in this repository (Codex/Astra, Claude Code via `@AGENTS.md`, Cursor, etc.).

**Instruction budget:** Keep this root file below 32,768 UTF-8 bytes; `npm run check:agent-docs` enforces the limit. Put conditional procedures, evidence, and worked examples in linked documents. Codex user configuration may set `project_doc_max_bytes = 65536` as a safety margin, but the larger limit does not replace the repository guard.

**Completion boundary:** This repository's default integration branch is `fork/consolidated`. A worktree or other feature branch is intermediate. For authorized repository work, finish by committing the verified change, integrating it through any required checks or PR into `fork/consolidated`, pushing, and confirming that its remote head contains the commit. Stop earlier only when the user requests it or integration is blocked; report the exact blocker. Do not run `npm publish` without explicit authorization. Fork GitHub release policy is defined under Releases below.

**Branch roles:** `origin/main` is an exact mirror of `upstream/main`; never commit or merge fork work into it. `.github/workflows/sync-upstream-main.yml` maintains that mirror. Merge upstream updates into `fork/consolidated`, which is this fork's canonical/default development branch. Base focused upstream contributions on `upstream/main` so they do not include the consolidated branch's experimental history.

## Project Overview

CodeGraph is a local-first code intelligence library + CLI + MCP server. It parses any supported codebase with tree-sitter, stores symbols/edges/files in SQLite (FTS5), and exposes a knowledge graph to AI agents (Claude Code, Cursor, Codex CLI, opencode) over MCP. Per-project data lives in `.codegraph/`. Extraction is deterministic — derived from AST, not LLM-summarized.

Distributed as `@colbymchenry/codegraph` on npm; same binary serves as installer, indexer, and MCP server.

## Build, Test, Run

```bash
npm run build           # tsc + copy schema.sql + build the viewer into dist/; chmods dist/bin/codegraph.js
npm run build:kernel    # the native Rust kernel (the only parser) → codegraph-kernel/prebuilds/<platform>/; needs cargo
npm run build:lib       # the viewer's components as @colbymchenry/codegraph-ui (ui/dist) — NOT part of `build`
npm run dev             # tsc --watch
npm run clean           # rm -rf dist

npm test                # vitest run (all)
npm run test:watch
npm run test:eval       # only __tests__/evaluation/
npm run eval            # build then run __tests__/evaluation/runner.ts via tsx

npm run cli             # build then run the local dist binary

# Single test file / pattern
npx vitest run __tests__/installer-targets.test.ts
npx vitest run __tests__/extraction.test.ts -t "TypeScript"
```

`copy-assets` (called from `build`) copies `src/db/schema.sql` into `dist/`. **Any new SQL asset must be copied or it won't ship.**

The native kernel (`codegraph-kernel/`, Rust) is the **only parser**: every grammar is compiled into it (crates.io pins plus the vendored C under `codegraph-kernel/grammars/`, provenance in `grammars/PROVENANCE.md`). There is no wasm fallback. A source checkout needs a prebuild at `codegraph-kernel/prebuilds/<platform>-<arch>/codegraph-kernel.node`; `npm test` builds it through `scripts/ensure-kernel.mjs` when missing (needs a Rust toolchain), release bundles ship it, and a platform without a prebuild is unsupported (the CLI says so at startup). Languages with a bespoke walker are extracted in Rust; the rest are parsed by the kernel and walked by the generic TypeScript extractor over the serialized tree (`src/extraction/parse-tree.ts`). The whole-graph golden dumps (`__tests__/kernel-golden-dumps.test.ts`) are the regression gate for any extraction change; see `docs/design/kernel-only-extraction-plan.md`.

One other build step writes into `dist/` and is subject to the same rule: `build:ui` builds the
browser viewer into `dist/viewer/` (never `dist/ui/` — that's the terminal ui).
`scripts/check-ui-build.mjs` asserts `dist/viewer/` after every build and inside every release
archive.

`npm run build:lib` is separate and does NOT run as part of `npm run build`: it compiles the same
`ui/src` tree a second way, with `svelte-package`, into `ui/dist` — the `@colbymchenry/codegraph-ui`
component library the Pro app imports (task CG-61). `scripts/check-ui-package.mjs` then prunes the
standalone app's shell out of it, resolves the extensionless import specifiers `svelte-package`
leaves behind, and asserts the seam: nothing outside `lib/adapter.js` may reach the network. The
package is **prepared, not published** — `ui/package.json` carries `"private": true` deliberately,
and `scripts/pack-npm.sh` only packs a tarball when `CODEGRAPH_PACK_UI=1`.

Tests run as **two vitest projects** (`vitest.workspace.mts`): `engine` (node) and `ui` (jsdom, the
Svelte plugin, `resolve.conditions: ['browser']`) for the single `__tests__/ui-package.test.ts`.
`npm test` still runs both. The split is not cosmetic — `browser` is a package-resolution
condition, and applied globally it hands the engine's suites the browser builds of
`web-tree-sitter` and friends. The root config (`vitest.config.mts`, `.mts` because the plugin is
ESM-only and the repo is CJS) is the shared base; note that a workspace project **concatenates**
the base's `include` with its own, which is why the `ui` project does not `extends` it.

Node engines: `>=20.0.0`. There is a hard exit below Node 20 (see `src/bin/node-version-check.ts`). The Node 25 block went with the wasm path; Node 25+ is untested but no longer refused.

## Architecture

### Layered pipeline

```
files → ExtractionOrchestrator (tree-sitter) → DB (nodes/edges/files)
              ↓
       ReferenceResolver (imports, name-matching, framework patterns)
              ↓
       GraphQueryManager / GraphTraverser (callers, callees, impact)
              ↓
       ContextBuilder (markdown/JSON for AI consumption)
```

The public API surface is `src/index.ts` — the `CodeGraph` class wires all the layers and re-exports types. Library users only touch this file; the MCP server and CLI also drive it.

### Module layout

- `src/index.ts` is the public library API and wires the system together.
- `src/db/` owns the `node:sqlite` database, schema, and prepared queries. Source development requires Node 22.5 or newer; published bundles carry their own supported runtime.
- `src/extraction/` parses supported languages; `src/resolution/` connects imports, names, frameworks, callbacks, and cross-tier flows.
- `src/graph/` owns shared graph derivations. If more than one surface renders a derivation, put it here rather than in an individual handler.
- `src/context/` and `src/search/` format and retrieve context; `src/sync/` owns watching and git-hook helpers.
- `src/mcp/` defines the MCP server and its agent-facing instructions; `src/installer/` defines host integrations.
- `src/bin/codegraph.ts` is the CLI. `src/ui/` is the terminal UI; `src/ui-server/` and `ui/` implement the browser viewer and component package.

### NodeKind / EdgeKind

Defined in `src/types.ts`. Both extractors and resolvers must use these exact strings.

- **NodeKind**: `file`, `module`, `class`, `struct`, `interface`, `trait`, `protocol`, `function`, `method`, `property`, `field`, `variable`, `constant`, `enum`, `enum_member`, `type_alias`, `namespace`, `parameter`, `import`, `export`, `route`, `component`, `union`.
- **EdgeKind**: `contains`, `calls`, `imports`, `exports`, `extends`, `implements`, `references`, `type_of`, `returns`, `instantiates`, `overrides`, `decorates`.

### Multi-agent installer

`src/installer/` is the entry point for `codegraph install` (and the bare `codegraph`/`npx @colbymchenry/codegraph` invocation). Architecture:

- `targets/registry.ts` is the supported-agent inventory.
- `targets/types.ts` defines the `AgentTarget` interface. Adding an agent is **one new file in `targets/` + one entry in `registry.ts`**. Each target owns its config-file location, MCP-server JSON/TOML/JSONC writing, and any instructions-file integration described below.
- `targets/toml.ts` is a hand-rolled TOML serializer scoped to `[mcp_servers.codegraph]` (used by Codex). Sibling tables and `[[array_of_tables]]` are preserved verbatim. No new dependency.
- opencode reads `opencode.jsonc` by default; the installer prefers existing `.jsonc`, falls back to `.json`, and creates `.jsonc` for greenfield installs. Edits are surgical via `jsonc-parser` so user comments and formatting survive install/re-install/uninstall round-trips. The MCP entry is OpenCode 2's native `mcp.servers.codegraph` with `disabled: false` and `codemode: false` (so `codegraph_explore` stays on the native tool list); a pre-#1698 `mcp.codegraph` + `enabled` entry is migrated on re-install and removed by uninstall.
- `instructions-template.ts` holds a deliberately short marker-fenced pointer for subagents and non-MCP harnesses that cannot receive MCP `initialize` instructions. Claude, Codex, opencode, and Gemini upsert it into their instructions files; uninstall removes it. Cursor and Kiro do not write an instructions block and only strip legacy blocks. Keep detailed tool behavior in `server-instructions.ts` so the short pointer does not recreate the pre-#529 duplicated playbook.
- All installer changes need matching coverage in `__tests__/installer-targets.test.ts`, including install idempotency, sibling preservation, uninstall reverses install, byte-equal re-runs returning `unchanged`, and partial-state recovery.

### Cursor MCP working-directory quirk

Cursor launches MCP subprocesses with the wrong cwd and doesn't pass `rootUri` in `initialize`. The installer injects `--path` into Cursor's MCP args — absolute path for local installs, `${workspaceFolder}` for global installs. If you touch Cursor wiring, preserve this.

### MCP server instructions

`src/mcp/server-instructions.ts` is sent to the main agent in the MCP `initialize` response and is the **single source of truth for detailed tool behavior**. `src/installer/instructions-template.ts` contains only the short command-and-surface pointer for subagents and non-MCP harnesses. Change detailed guidance in the server instructions; update both files only when a command or available surface changes.

## Retrieval performance & dynamic-dispatch coverage (do not regress)

CodeGraph's core value is letting an agent answer **structural/flow** questions ("how does X reach Y", trace, impact, callers) with a few **fast** codegraph calls and **zero Read/Grep**. The optimization target is **wall-clock latency + tool-call count** — *don't optimize for token cost*. (Cost is **lower**, not "flat" as earlier framing claimed: a current-build with-vs-without A/B across the 7 README repos, median of 4, saved on average **35% cost · 57% tokens · 46% time · 71% tool calls** — reproducing the published README. The mechanism is **far fewer turns over a much smaller accumulated context** — NOT cache-ability: the without-arm's huge token volume is *mostly* cheap cache-reads, which is why token-count savings (57%) look bigger than cost savings (35%). Measure tokens by **summing per-turn assistant usage**, not `result.usage` (last-turn only in current Claude Code). See `docs/benchmarks/call-sequence-analysis.md`.) The mechanism that drives everything here: **an agent falls back to Read/Grep the instant a codegraph answer is insufficient.** So every change is judged by one question — is codegraph's answer sufficient enough to *stop* the agent from reading?

**Target behavior:** a flow question resolves in **1 codegraph call on small repos, scaling to 3–5 on large**, with **Read/Grep = 0**. When reviewing a PR or trying something new, do not regress this.

### Adapt the tool to the agent — don't try to change the agent

The lever that decides whether a retrieval change lands. **Test before building anything here: does this make a tool the agent _already calls_ do more with the input it _already gives_? If it instead needs the agent to behave differently — pick a different tool, query differently, learn from examples — it hits the low-salience wall and won't land.**

CodeGraph's only channels to influence the agent are low-salience: the MCP `initialize` instructions (`server-instructions.ts`) and the tool descriptions. Changing them does **not** reliably move the agent's tool _choice_ or query style — validated: trace-first steering ported into the server-instructions + tool descriptions (3 wording variants) never reproduced what a CLI `--append-system-prompt` achieved, and **regressed** wall-clock vs baseline. New tools fare worse (rarely chosen — the agent under-picks even `trace`); "better examples" is the same steering. The agent's tool-choice does improve on its own as host models get better at tool use — but that is not ours to force.

What works is meeting the agent where it already is:
- **explore-flow** — `codegraph_explore` is the PRIMARY tool the agent reliably calls; its query is a precise bag of symbol names (incl. qualified `Class.method`) spanning the flow the agent is after; explore finds the call path _among those named symbols_ (riding synthesized edges) and leads its output with it. (`buildFlowFromNamedSymbols`: segment/co-naming disambiguation; ≤1 unnamed bridge so it never wanders a god-function's fan-out. Overload-aware: a PascalCase type token in the query biases an overloaded name to that type's own def — `DataRequest task` → DataRequest's `task`, not the abstract base; named-symbol files sort first.)
- **Sufficiency** — make `codegraph_explore` complete enough that the agent stops. It returns full bodies plus caller/callee context, and for an ambiguous name returns every overload's body in one call (validated on Alamofire/gin).
- **Errors teach abandonment** — one or two `isError: true` responses early in a session and the agent stops calling codegraph entirely (maintainer-observed, repeatedly). `isError` is reserved for genuine "stop trying" cases: security refusals (`PathRefusalError`) and real malfunctions (which carry a retry-once note). Every expected/recoverable condition — project not indexed, symbol not found, file not in the index — returns a **SUCCESS-shaped response carrying the guidance** (`NotIndexedError` → `textResult`, see `ToolHandler.execute`'s catch). The same principle is why the tool surface is **always exposed, even at an un-indexed root** (the old empty-`tools/list` gate was removed in #964 — it broke monorepos where only sub-projects carry a `.codegraph/`, and hid the tools from a session that started before `codegraph init`): safety comes from the response SHAPE (success-shaped guidance, never `isError`), not from hiding tools. An un-indexed root's `initialize` sends a per-project variant (`SERVER_INSTRUCTIONS_NO_ROOT_INDEX` — "pass `projectPath` to a project that has a `.codegraph/`"), not an "inactive" note; indexing is still deliberately the user's call, never the agent's.

What fails is the inverse — folding a precise answer into a **fuzzy-input** tool: the now-removed `codegraph_context` took a description, not symbols, so it couldn't disambiguate a flow's endpoints and surfaced the _wrong feature_ (which is why it was cut). Precise output needs precise input — explore takes a symbol bag for exactly this reason. (`codegraph_trace` was likewise removed: explore-flow does its job and the agent under-picked it.)

The remaining lever under this axis is **coverage**: every flow made to connect statically (a new dynamic-dispatch synthesizer, or extracting symbols static parsing skipped — e.g. object-literal store actions in `create((set,get)=>({...}))`) is then surfaced automatically by explore-flow, no agent change needed. Reactive/reconciler runtimes (Halo's `ReactiveExtensionClient`, MediatR, Vue Proxy) are the frontier — flows there have no static edges, so nothing surfaces (correctly — silent beats wrong). Full investigation + A/B record: `docs/benchmarks/call-sequence-analysis.md`.

### Explore budgets

`getExploreBudget` and `getExploreOutputBudget` in `src/mcp/tools.ts` are the source of truth for the size tiers; `__tests__/explore-output-budget.test.ts` pins them. Keep both budgets monotonic with indexed file count: a larger tier must never receive fewer calls or a smaller `maxCharsPerFile` than a smaller tier. A regression here truncates large files and forces agents back to Read.

- Explore output must **never tell the agent to "use Read"** — steer to another `codegraph_explore` and "treat returned source as already Read."

### Dynamic-dispatch coverage — the flow must EXIST in the graph end-to-end

Static tree-sitter extraction misses computed/indirect calls, so flows break at dynamic dispatch and the agent reads to reconstruct them. Synthesizers/resolvers bridge these so `codegraph_explore` connects them end-to-end (`src/resolution/callback-synthesizer.ts`, `src/resolution/frameworks/`). Channels today: callback/observer, EventEmitter, **React re-render** (`setState`→`render`), **JSX child** (`render`→child component), **React Native native→JS events** (`sendEvent(withName:)` / JVM `emit` → the `addListener` handler, named or inline, `rn-event-channel`), django ORM descriptor. The JS→native direction is a *resolver* (`frameworks/react-native.ts`: `RCT_EXPORT_METHOD`, `RCT_EXTERN_MODULE` Swift shims, TurboModules), which trusts receiver evidence — an alias bound to `NativeModules.X` — over the import resolver. All synthesized edges are `provenance:'heuristic'` with `metadata.synthesizedBy` + `registeredAt` (the wiring site), surfaced inline in `codegraph_explore`'s Flow section.

**Principle: partial coverage is WORSE than none.** Bridging one boundary but not the next reveals a hop the agent then drills + reads to finish. Measured on excalidraw: react-render alone *raised* reads to 5–7; only completing the flow (adding the jsx-child hop) dropped it to 0–1. **Always close the flow end-to-end and re-measure** — never ship a half-bridged flow.


### Validation methodology & worked examples

**Required** for every new language/framework: validate on small/medium/large real repos with >=3 flow prompts; deterministic `scripts/agent-eval/probe-explore.mjs` probes, then agent A/B (`scripts/agent-eval/run-all.sh` / `ab-new-vs-baseline.sh`). Pass bar: ~0 Read/Grep within the explore-call budget, faster than without-codegraph, no control-repo regression.

Full methodology (feedback metrics, CLI contamination guard, Sonnet/`--effort high` model policy, daemon pre-warm), the Excalidraw worked example, and coverage matrix live in:
- `docs/AGENTS.md` (nested; also loaded when cwd is under `docs/`)
- `docs/design/dynamic-dispatch-coverage-playbook.md`
- `docs/design/callback-edge-synthesis.md`
- `docs/benchmarks/call-sequence-analysis.md` / `docs/benchmarks/agent-eval-feedback-metrics.md`


Tests live in `__tests__/` and mirror the module they cover. Notable ones beyond the obvious:

- `installer-targets.test.ts` — parameterized contract suite across the registered agent targets (see installer notes above).
- `evaluation/` — `runner.ts` + `test-cases.ts` exercise codegraph against synthetic projects and score the results; run via `npm run eval` (builds first). Not part of `npm test`.
- `sqlite-backend.test.ts` / `node-sqlite-backend.test.ts` — pin that `node:sqlite` is the sole backend: `getBackend()` reports `node-sqlite` and the DB comes up in WAL.
- `pr19-improvements.test.ts`, `frameworks-integration.test.ts` — regression coverage for specific past PRs/incidents; don't rename these, the names anchor to git history.
- `kernel-golden-dumps.test.ts` — whole-graph golden dumps for a fixed fixture corpus (`__tests__/fixtures/golden/`); any extraction, resolution or synthesis change re-baselines with `UPDATE_GOLDEN=1` and the `.dump` diff is the review artifact. See `docs/design/kernel-only-extraction-plan.md` Phase 0.
- `bindings-*.test.ts` and `kernel-generic-extractor-tree.test.ts` — the kernel emits a `bindings` table per file (what each name is bound to: declaration, import, parameter, local, with scope and export form); the resolver reads it instead of scanning source. A resolution change is gated by `npm run eval:precision -- <corpus>` on the pinned real repositories in `__tests__/evaluation/edge-cases.ts` (known-wrong edges must stay absent, controls present) plus an edge-level before/after review. See `docs/design/resolution-binding-model-plan.md`.

Tests create temp dirs with `fs.mkdtempSync` and clean up in `afterEach`. They write real files and exercise real SQLite — there is no DB mocking.

### Windows-gated tests

Behavior that differs by platform (path resolution, drive letters, `SENSITIVE_PATHS`, `%APPDATA%` config dirs, CRLF) must be gated, not assumed. Use `it.runIf(process.platform === 'win32')(...)` for Windows-only assertions and `it.runIf(process.platform !== 'win32')(...)` for POSIX-only ones — e.g. `/etc` is sensitive on POSIX but resolves to `C:\etc` (non-existent) on Windows, so an ungated `/etc` assertion fails on Windows. Validate the Windows side for real (see below); don't merge a Windows-gated test you haven't seen run.

## Cross-platform validation

The development host and default test target are Ubuntu under WSL. Run CodeGraph build and test commands through `fnm exec --using codegraph` so they use the supported Node 24 runtime, and have a Rust toolchain on PATH for the kernel. Platform-sensitive changes (file watching, sockets or named pipes, paths and symlinks, process lifecycle, and inotify limits) still need validation on every affected operating system.

### Linux and containers

Run Linux validation directly in WSL. Use Docker only when a clean container or PID-1 behavior is part of the test; Docker is optional isolation, not the default Linux route.

- For a clean image, start from the supported Node line, exclude host `node_modules`, `dist`, `.git`, and `.codegraph`, then install and build inside the image because native dependencies are platform-specific.
- Use `docker run --rm --init` for process-lifecycle tests. Without a zombie-reaping PID 1, an exited process can remain visible and make exit-detection assertions fail.
- Inspect Linux inotify use through `/proc/<pid>/fdinfo/*`; count `^inotify ` lines on the descriptor whose `readlink` is `anon_inode:inotify`.

### Windows

For Windows-specific behavior, use a Windows-local checkout and the host's PowerShell toolchain. Do not build against a checkout or `node_modules` tree shared across the WSL boundary.

- Install the supported Node version, Git, and the matching VC++ redistributable for native packages.
- Fetch a contributor branch into the Windows-local checkout and install dependencies there.
- Confirm a suspected platform failure against `origin/fork/consolidated` before attributing it to the current change. Keep Windows-only assertions behind `it.runIf(process.platform === 'win32')`.

## Releases

**Fork policy:** `bompus/codegraph` does not publish GitHub releases. Do not prepare or create fork GitHub releases, create release tags, or dispatch `.github/workflows/release.yml` on this fork. Missing release credentials or upstream publishing targets are not fork setup tasks. Authorized fork work completes on remote `fork/consolidated`; validation must not depend on publishing a release.

The release procedures below describe upstream `colbymchenry/codegraph` only. Upstream publishes to npm and [GitHub Releases](https://github.com/colbymchenry/codegraph/releases). `CHANGELOG.md` remains the source of truth for change notes.

### Writing changelog entries

**Default: write entries under `## [Unreleased]`** — that's the section reserved for work landing between releases. **Don't pre-create a `## [X.Y.Z]` block** for the next release: the Release workflow's first step is `scripts/prepare-release.mjs`, which automatically promotes everything under `[Unreleased]` into a new `## [X.Y.Z] - <YYYY-MM-DD>` block at release time (or merges into a pre-existing `[X.Y.Z]` block if one exists — but you don't need one). Pre-staging is what caused the v0.9.5 sparse-release-notes incident: a sparse `[0.9.5]` block hand-added before the rest of the work landed got picked by the extractor over the much-larger `[Unreleased]` section above it. Don't do that.

Formatting rules for any entry (anywhere — `[Unreleased]` or otherwise):

1. **Write friendly, user-facing notes — not engineer-facing ones.** Group under `### New Features` and `### Fixes` (sentence-case). Surface `### Breaking Changes` and `### Security` as their own sections **only when the release has them**; fold improvement-flavored changes into New Features. Omit empty sections. (This replaces the old Keep-a-Changelog `Added/Changed/Fixed/Removed/Deprecated` grouping: the GitHub Release page extracts each version block **verbatim** via `scripts/extract-release-notes.mjs`, and the old dense, implementation-focused entries rendered as an unreadable wall of text — so the whole CHANGELOG was rewritten to this format and every published release re-noted to match.)
2. **One plain-language sentence per bullet:** what changed and why it matters to a user. Lead with the capability, or with the symptom that's now fixed.
3. **Strip the internals.** No internal file paths (`src/...`), no internal symbol / function / class names, no benchmark numbers / percentages / node-or-edge counts. **Keep:** language & framework names (Go, Spring, NestJS, …), things a user types or sets (`codegraph install`, `codegraph_explore`, the `CODEGRAPH_*` env vars), agent / IDE names (Claude Code, Cursor, opencode, Kiro, …), and a brief `Thanks @user` when a contributor is credited.
4. Issue / PR references in entries are by number (`(#403)` etc.); the GitHub renderer auto-links them in the published release notes.
5. **Don't add a `[X.Y.Z]: https://...` link reference yourself** — `prepare-release.mjs` appends it automatically when it promotes the version (idempotent: a re-run is a no-op if it already exists).
6. **Every release opens with a `### Highlights` block — the only part most people read.** At most ~8 one-line bullets, in plain language for someone who doesn't read code, ordered by what a typical user notices first (new agent/IDE support and setup changes, then answer quality, then reliability), plus a one-sentence upgrade note when a re-index is needed. Write or refresh it in `[Unreleased]` when a release is being prepared — not per PR — and keep the detailed `### New Features` / `### Fixes` entries below it. When `### Fixes` grows past ~15 entries, group them under `####` sub-headings (`Better answers from codegraph_explore`, `Finding your project, live updates, and the CLI`, `Indexing reliability and disk usage`, `Language and framework accuracy`) so a skimmer can find their area.

Multi-word headings like `### New Features` are safe on the normal release path: `prepare-release.mjs` **Case A** moves the whole `[Unreleased]` body verbatim into `[X.Y.Z]`. (Only its rarely-used **Case B** *merge* splits sub-sections with a single-word `^### (\w+)$` regex that wouldn't match them — and Case B fires only if a `[X.Y.Z]` block was pre-created, which rule above already forbids.)

### Upstream release flow (reference only)

Upstream runs releases through `.github/workflows/release.yml`; do not publish the root package manually. Agents do not bump versions unless explicitly asked. A requested release normally needs only the target version in `package.json`; the workflow synchronizes the lock file, promotes `[Unreleased]`, builds the platform bundles, creates the GitHub Release, and publishes through npm trusted publishing. Read the workflow before changing or describing this process.

## House rules

- Any change to `src/installer/` (especially `targets/`) needs corresponding test coverage and a CHANGELOG entry — installer regressions break every new install silently.
- When changing MCP tool behavior or usage, edit `src/mcp/server-instructions.ts`, the single source of truth for detailed guidance. If a command or available surface changes, also update the short subagent/non-MCP pointer in `src/installer/instructions-template.ts`. Claude, Codex, opencode, and Gemini install that pointer; Cursor and Kiro do not. (The repo's own checked-in `.cursor/rules/codegraph.mdc` is dogfooding config — update it too if you use Cursor on this repo, but it ships nowhere.)
- **Before adding or extending a router, a web framework, or a language's `WHEN` rules, read `docs/design/framework-coverage.md`.** It is the standing answer to "what is supported and what is left" across the three axes (route nodes → Entry points, `navigates` edges → Screens, branch-guard rules → the `WHEN` labels), with what each remaining item needs, the traps that have already cost debugging time, and the queries to re-verify it. Update it in the same change that moves a row.
- CodeGraph provides **code context**, not product requirements. For new features, ask the user about UX, edge cases, and acceptance criteria — the graph won't tell you.
- **When the user references issues, PR comments, or external reports, anchor them to a date and version before drawing conclusions.** Check the comment's `createdAt` against:
  - The **last released version** — `grep -m1 '^## \[' CHANGELOG.md` shows the top-of-file version (older releases follow). A comment dated before the latest `## [X.Y.Z] - YYYY-MM-DD` is reacting to *released* state — work that's only on `fork/consolidated` or on an unmerged branch doesn't apply.
  - The **last default-branch commit** — `git log --first-parent origin/fork/consolidated -1 --format='%ai %h %s'`. A comment after the last release but before a fix on `fork/consolidated` may already be addressed there but unreleased.
  - The **current branch's tip** — your own unmerged work obviously can't be what the comment is reacting to.
  Always disambiguate "released," "merged-but-unreleased," and "in-progress" before agreeing that a user-reported problem is unfixed (or that a fix is incomplete). A user saying "your fix only covers X" about a recent PR is usually pointing at the *released* shortcomings — your in-flight branch may already address them but they have no way to know that.
- **Version-tag every image referenced in `README.md`.** GitHub caches README images (`raw.githubusercontent.com` with a 5-minute TTL; third-party hosts sit behind the long-lived camo proxy), so updating an asset in place can keep showing the stale version. Give each README image URL a `?v=N` query tag and **bump `N` in the same commit whenever the asset bytes change** — e.g. `assets/waitlist.svg?v=2`. The changed URL sidesteps every cache so the new image shows immediately instead of waiting on a TTL to expire.
