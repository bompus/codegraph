---
name: codegraph-build-validation
description: Use when building or testing CodeGraph, changing extraction, resolution or synthesis, re-baselining kernel golden dumps, writing platform-gated (Windows/POSIX) tests, or validating platform-specific behavior.
---

## Build, Test, Run

```bash
npm run build           # tsc + copy schema.sql + build the viewer into dist/; chmods dist/bin/codegraph.js
npm run build:kernel    # the native Rust kernel (the only parser) → codegraph-kernel/prebuilds/<platform>/; needs cargo
npm run build:lib       # the viewer's components as @colbymchenry/codegraph-ui (ui/dist) — NOT part of `build`
npm run dev             # tsc --watch
npm run clean           # rm -rf dist

npm test                # vitest run (all)
npm run test:bun        # vitest under Bun (must be `bun --bun x vitest run` — plain `bun x` honors vitest's node shebang and silently runs Node)
npm run test:watch
npm run test:eval       # only __tests__/evaluation/
npm run eval            # build then run __tests__/evaluation/runner.ts via tsx

npm run cli             # build then run the local dist binary

# Single test file / pattern
npx vitest run __tests__/installer-targets.test.ts
npx vitest run __tests__/extraction.test.ts -t "TypeScript"
```

Under Bun the suite runs green via `__tests__/bun-homedir.setup.ts` (an `os.homedir` shim for oven-sh/bun#29244) plus three test-file adaptations linked to their upstream issues (#42891, #42893, #25498).

`copy-assets` (called from `build`) copies `src/db/schema.sql` into `dist/`. **Any new SQL asset must be copied or it won't ship.**

The native kernel (`codegraph-kernel/`, Rust) is the **only parser**: every grammar is compiled into it (crates.io pins plus the vendored C under `codegraph-kernel/grammars/`, provenance in `grammars/PROVENANCE.md`). There is no wasm fallback. A source checkout needs a prebuild at `codegraph-kernel/prebuilds/<platform>-<arch>/codegraph-kernel.node`; `npm test` builds it through `scripts/ensure-kernel.mjs` when it is missing or was built from other kernel sources (needs a Rust toolchain), release bundles ship it, and a platform without a prebuild is unsupported (the CLI says so at startup). Languages with a bespoke walker are extracted in Rust; the rest are parsed by the kernel and walked by the generic TypeScript extractor over the serialized tree (`src/extraction/parse-tree.ts`). The whole-graph golden dumps (`__tests__/kernel-golden-dumps.test.ts`) are the regression gate for any extraction change; see `docs/design/kernel-only-extraction-plan.md`.

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
their dependencies. The root config (`vitest.config.mts`, `.mts` because the plugin is
ESM-only and the repo is CJS) is the shared base; note that a workspace project **concatenates**
the base's `include` with its own, which is why the `ui` project does not `extends` it.

Engines: Node `>=22.13.0` or Bun `>=1.4.0` for source development (`package.json`); published bundles carry their own supported runtime. The CLI exits at startup on an older Node or Bun (`src/bin/node-version-check.ts`). The full suite also passes on Node 26 and Bun; the tested versions are listed in the README's "Runtimes" section.

Tests live in `__tests__/` and mirror the module they cover. Notable ones beyond the obvious:

- `installer-targets.test.ts` — parameterized contract suite across the registered agent targets (see the `codegraph-installer` skill).
- `evaluation/` — `runner.ts` + `test-cases.ts` exercise codegraph against synthetic projects and score the results; run via `npm run eval` (builds first). Not part of `npm test`.
- `evaluation/search-baseline-runner.ts` — known-answer search checks on pinned real repositories (`search-cases.ts`): exact names, prefixes, typos, spaced words, `path:`/`name:`/`kind:`/`lang:` filters, scoped matches the unfiltered query ranks below the limit, and Unicode case folding. Run `EVAL_REPOS=~/cg-scratch/<dir> npm run eval:search -- <corpus>` after a search or ranking change; it prints per-category pass counts, MRR and latency and writes `results/search-<corpus>-<commit>-<codegraph>.json`. `knownMiss` cases record measured limits and report FIXED when one starts passing.
- `sqlite-backend.test.ts` / `node-sqlite-backend.test.ts` — pin that `node:sqlite` is the sole backend: `getBackend()` reports `node-sqlite` and the DB comes up in WAL.
- `pr19-improvements.test.ts`, `frameworks-integration.test.ts` — regression coverage for specific past PRs/incidents; don't rename these, the names anchor to git history.
- `kernel-golden-dumps.test.ts` — whole-graph golden dumps for a fixed fixture corpus (`__tests__/fixtures/golden/`); any extraction, resolution or synthesis change re-baselines with `UPDATE_GOLDEN=1` and the `.dump` diff is the review artifact. See `docs/design/kernel-only-extraction-plan.md` Phase 0.
- `bindings-*.test.ts` and `kernel-generic-extractor-tree.test.ts` — the kernel emits a `bindings` table per file (what each name is bound to: declaration, import, parameter, local, with scope and export form); the resolver reads it instead of scanning source. A resolution change is gated by `npm run eval:precision -- <corpus>` on the pinned real repositories in `__tests__/evaluation/edge-cases.ts` (known-wrong edges must stay absent, controls present) plus an edge-level before/after review. See `docs/design/resolution-binding-model-plan.md`.

Tests create temp dirs with `fs.mkdtempSync` and clean up in `afterEach`. They write real files and exercise real SQLite — there is no DB mocking.

Working copies of the real corpora (e.g. `~/codegraph-corpora/linux`, multi-GB SQLite DBs) must live on the workspace disk, never on `/tmp` — that is a shared tmpfs and a handful of `VACUUM INTO` snapshots will fill it and OOM the host. Put snapshots under `~/cg-scratch/` or the corpus tree and delete them when done.

Before pushing a branch meant for upstream, run `node <fork checkout>/scripts/pr-guard.mjs` from the branch: it fails when the branch adds a `bun.lock`, `yarn.lock` or `pnpm-lock.yaml` (upstream tracks only `package-lock.json`). To check what a change does to the graph, index a corpus with each build and compare them with `node scripts/index-metrics.mjs <before.db> <after.db>`: counts plus the call edges lost or gained, keyed by qualified name so shifted lines do not count. `CHANGELOG.md` merges with git's `union` driver (`.gitattributes`), so an upstream merge keeps both sides' bullets; check for a doubled bullet after the merge.

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
- One failure is expected without symlink privileges (Developer Mode off): `security.test.ts > Session marker symlink resistance > does not follow a pre-planted symlink`.
- Tests that spawn `serve --mcp` must wait for the child to exit before removing its temp dir, and close every `CodeGraph` or `DatabaseConnection` in `afterEach` or `finally`. An open handle makes the removal fail with `EPERM`; remove temp dirs with `maxRetries`.
- Windows checkouts may use CRLF, so tests split source lines on `/\r?\n/`.
