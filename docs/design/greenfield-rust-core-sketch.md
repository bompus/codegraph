# Greenfield Rust core — design sketch and tradeoff

**Status:** sketch, not approved. Written 2026-09-11 so the "rewrite from scratch in Rust" option is judged on paper against the two incremental plans it competes with: [kernel-only extraction](kernel-only-extraction-plan.md) and the [resolution binding model](resolution-binding-model-plan.md).

**Recommendation:** do not start a greenfield core. Do the two incremental plans first. If, after both land, the TypeScript side is a thin driver over a Rust crate, spin that crate into its own repository then. The reasons are in §5.

## 1. What a replacement must reproduce

This is the surface a from-scratch core inherits on day one. Every row is a user-visible contract or a persisted artifact; none is optional.

### 1.1 Persisted graph (`.codegraph/codegraph.db`)

- Tables `nodes`, `edges`, `files`, `unresolved_refs`, `name_segment_vocab`, `literals`, `project_metadata`, `schema_versions`, plus the FTS5 external-content table `nodes_fts` with its three sync triggers. Schema in `src/db/schema.sql`, ten ordered migrations in `src/db/migrations.ts`.
- Two version axes: schema version (migratable, DDL-only, never backfills content columns) and `EXTRACTION_VERSION` (content shape, drives the re-index advisory in `codegraph status` and `codegraph upgrade`). Currently 28.
- Node identity is a deterministic text id; edge identity is `(source, target, kind, line, col)` with `INSERT OR IGNORE` dedupe. Provenance column separates extracted from `heuristic` (synthesized) edges. Every explore answer and every golden dump depends on these being stable.
- Second database `.codegraph/sessions.db` for transcript search (FTS5 porter, BM25).

### 1.2 Vocabulary

`NodeKind` (23 values) and `EdgeKind` (12 values) in `src/types.ts`. Extractors, resolvers, synthesizers, explore ranking, the viewer, and the kernel ABI contract check (`src/extraction/kernel/loader.ts`) all key on the exact strings.

### 1.3 Extraction

29 grammars, 20 of them in the Rust kernel already. Nine hand-written slicing extractors for embedded-language files (Vue, Svelte, Astro, Razor, Liquid, Markdown, CFML, MyBatis, DFM). Literal capture, generated-file detection, C/C++ blanking pre-passes, docstring and signature capture, per-file error handling. Inventory and line counts in the kernel-only plan.

### 1.4 Resolution

About 30k lines: import resolver, name matcher, alias binding, path aliases, workspace packages, Go modules, 33 framework resolvers under `src/resolution/frameworks/`, and 36 synthesis passes (callback, EventEmitter, React re-render, JSX child, React Native events, router synthesizers for Next, Expo, React Router, TanStack, Vue Router, SvelteKit, C function pointers, GoFrame, Swift/ObjC bridge, tiering). Each was validated against real repos by agent A/B. None is derivable from a spec.

### 1.5 Graph derivations (`src/graph/`)

Traversal, symbol lookup (the one "what did the user mean" resolver), named-symbol flow (the one path finder), type hierarchy, dead code, branch guards (query-time AST walk for `WHEN` labels), dynamic boundary report.

### 1.6 Retrieval and MCP

- Tools: `codegraph_explore`, `codegraph_sessions` served by default; `search`, `callers`, `callees`, `impact`, `node`, `status`, `files` behind `CODEGRAPH_MCP_TOOLS`. Input schemas in `src/mcp/tools.ts`.
- Explore semantics: budgets tiered by indexed file count (`getExploreBudget`, `getExploreOutputBudget`), allocation across named and discovered files, gap markers, verbatim line-numbered source in Read's shape, flow section, blast radius, staleness and degraded banners, session dedup, did-you-mean for exact names, literal seeding.
- Server instructions in three variants (indexed root, unindexed root, unindexed worktree). The response-shape rule: expected conditions are success-shaped, `isError` only for refusals and malfunctions.
- Process model: detached daemon per project root on a socket or named pipe, stdio proxy with PPID watchdog, query worker pool with per-worker WAL read connections and circuit breaker, single writer lock, liveness and startup watchdogs, refresh launcher, daemon registry under `~/.codegraph/daemons/`.

### 1.7 Sync

`fs.watch` based watcher with platform-specific strategies (one recursive watch on macOS and Windows, one inotify watch per directory on Linux with a cap), scope built to match `git ls-files --exclude-standard`, WSL2 `/mnt` force-off, debounced sync, lock-contention retry and degraded state, opt-in git hooks, worktree index-mismatch detection.

### 1.8 CLI, installer, viewer, sessions, telemetry, upgrade

- 27 CLI commands including the hidden `prompt-hook` and `serve`.
- Installer for 11 agent hosts with per-host config formats (JSON, JSONC, TOML), marker-fenced instruction blocks, the Cursor `--path` quirk, idempotency and uninstall contracts.
- Viewer: 16 HTTP endpoints plus SSE, own highlighter reading the same grammar as the engine, trail store.
- Sessions: readers for Claude, Codex, Cursor, OpenCode, AGY transcript formats.
- Telemetry, update check, self-upgrade including the Windows locked-binary helper.

### 1.9 Settings

About 75 `CODEGRAPH_*` environment variables plus `codegraph.json` (`extensions`, `include`, `includeIgnored`, `exclude`, `deprioritize`, `sessions`). Grouped:

| Group | Count | Fate under a Rust core |
|---|---|---|
| WASM and V8 workarounds (`KERNEL*`, `WASM_RELAUNCHED`, `ALLOW_UNSAFE_NODE`, `NO_RELAUNCH`) | 7 | Gone with WASM |
| Worker and pool sizing | 8 | Auto-detected from cgroups and load; keep one override |
| Daemon and watchdog timeouts | 12 | Keep as a typed config struct, most never set |
| WAL valve and fast-init | 5 | Keep until the store changes |
| Explore debug and ranking toggles | 7 | Debug flags only, not user settings |
| Paths and host identity | 9 | Keep |
| Install and hook | 8 | Keep |

The knob count is a symptom of one-off incident fixes, not of the language. A rewrite that keeps the same incident history would grow the same knobs.

## 2. What the Rust core would look like

A workspace of crates behind one C ABI and one napi binding:

```
codegraph-core/
  crates/
    graph-model     NodeKind, EdgeKind, ids, provenance, extraction version
    store           SQLite (rusqlite, bundled, FTS5), schema, migrations, WAL valve, writer lock
    extract         tree-sitter walkers for 29 grammars + region slicers for SFC/Markdown/CFML
    resolve         import resolver, binding model, name matcher, framework resolvers
    synth           synthesis passes over the stored graph
    derive          traversal, symbol lookup, flow, hierarchy, dead code, branch guards
    retrieve        explore ranking, budgets, allocation, formatting
    sync            notify-based watcher, scope from gitignore, debounce, git hooks
    daemon          socket server, query pool, watchdogs, registry
    mcp             JSON-RPC 2.0 over stdio and socket, tool schemas, instructions
    cli             clap front end
  bindings/napi     thin surface for the viewer server and installer, which stay in TypeScript
```

Design choices that would differ from today:

- **One extractor per language, no fallback.** Error recovery is whatever the native parser produces. Golden dumps pin it.
- **Bindings as data, not regex.** Extraction emits a per-file binding table (name, kind, target, exported, re-exported-from). Resolution consumes it and never rescans raw source. This is the same model the resolution plan proposes in place.
- **Typed config.** One `Config` struct loaded from `codegraph.json` and a short env allowlist. No ad-hoc `process.env` reads inside modules.
- **Store as the parallelism boundary.** Parse and resolve on rayon; a single writer thread owns the connection; workers send batches over a channel. This is what the current TypeScript pipeline approximates with worker threads and a store worker.
- **Single binary.** CLI, daemon, MCP server, and indexer in one static executable. No Node runtime in the bundle, no `--liftoff-only`, no version gate.

## 3. What it would buy

- Fresh-index wall time: the kernel spike measured a 4.4x to 14x parse walk over WASM, and the migration plan's Linux-kernel run puts resolution at 73% of wall. A native resolve with a proper binding table is the remaining large win. Realistic target on the Linux kernel: under 8 minutes on 8 cores against 14.8 today.
- Memory: no per-worker grammar copies, no V8 heap per worker. The Bun probe in the espn-draft research showed how much of today's footprint is runtime, not data.
- Distribution: one binary per platform. The npm thin installer stays as a shim.
- Bugs of the class "handle left open on Windows teardown" and "orphan daemon per test run" become type-system and RAII concerns instead of discipline.

## 4. What it would cost

Measured against the repository as of 2026-09-11.

| Item | Size | Note |
|---|---|---|
| Engine TypeScript to replace | 117k lines | Of which extraction 27k, resolution 30k, MCP 15k |
| Tests encoding fixed incidents | 101k lines | Many anchor to PR numbers; the coverage is the spec |
| Rust already written | 26k lines | Extraction only, reusable as-is |
| Upstream velocity | ~90 first-parent commits/month | 154 of the last 400 are fixes |
| Fork divergence | 166 commits ahead, 13 upstream PRs open | Flow of accuracy fixes in both directions |

- **The heuristics are the product.** Every framework resolver and synthesizer exists because an agent A/B showed a flow breaking without it. A rewrite re-validates all of them or regresses answers silently, and the repository rule is that a half-bridged flow is worse than none.
- **The upstream relationship ends.** A separate codebase cannot merge upstream fixes or contribute back. Five of the fork's fixes in the last week landed upstream as maintainer re-lands; that channel closes.
- **Time to parity is long.** With the kernel as a head start, extraction is done. Resolution, synthesis, retrieval, daemon, sync, and MCP are each multi-week efforts with a byte-parity bar, done serially because each consumes the previous one's output.
- **Retrieval quality does not move.** The espn-draft audit log shows the organic loss is in ranking and trimming inside explore, at a median 228 ms latency. That code is pure logic and gains nothing from Rust.

## 5. Decision

Do not start a greenfield core now. The two incremental plans capture nearly all of the performance and simplification benefit while keeping the tests, the heuristics, and the upstream channel:

1. [Kernel-only extraction](kernel-only-extraction-plan.md) removes the WASM runtime, the second grammar supply chain, the V8 workarounds, and about 27k lines of TypeScript, and makes the kernel the single producer.
2. [Resolution binding model](resolution-binding-model-plan.md) replaces the three-place export logic with a binding table emitted by extraction, ends the over-matching fix cycle, and is the natural first resolution module to port into the kernel.

Re-open this sketch when all of the following hold:

- Both plans have landed and the TypeScript engine is under roughly 40k lines.
- Resolution and synthesis run in the kernel with golden-dump parity.
- A concrete need exists that the Node shell blocks: a single static binary for a host with no Node, or a memory ceiling the daemon cannot meet.

At that point the crate is already the core, and "greenfield" becomes a repository split, not a rewrite.
