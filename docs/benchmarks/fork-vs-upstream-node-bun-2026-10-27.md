# Fork against upstream CodeGraph on Node.js and Bun, 2026-10-27

Upstream `main` at `69af19ac` against `fork/consolidated` at `eed3d34f`, each on Node.js 24.22.1 and on Bun 1.4.4, over seven corpora, the MCP server and command-line startup. It repeats the [2026-10-18 run](fork-vs-upstream-node-bun-2026-10-18.md) with the same scripts and the same pinned corpora on the current builds. Index and sync figures were re-measured 2026-10-27 on the same host after the ambient-scan rework and the resolve-pipeline change described in [The supabase resolution regression](#the-supabase-resolution-regression); MCP, CLI and explore cells are unchanged by resolution-path changes and carry the earlier 2026-10-27 measurements at `bc27fdb4`. The README's "Measured results" quote this page.

## Method

The scripts are in [`scripts/benchmarks/runtime/`](../../scripts/benchmarks/runtime/): `run-index-sync.sh`, `run-mcp.sh` and `run-cli.sh`, with the four arms below. `adapt-flat-output.mjs` stages the flat output for `render-charts.mjs`, which computes every table, the heatmap and the README card from the raw rows.

- **Builds.** Upstream `main` at `69af19ac` — the commit the fork's default branch is reconciled through — and the fork at `eed3d34f` for index and sync, `bc27fdb4` for MCP, CLI and explore cells. The fork contains every upstream commit through `69af19ac` plus its own work. `bc27fdb4` carried a memo on the per-file ambient-scope scan (without it the fork's supabase index never completed — see [The supabase resolution regression](#the-supabase-resolution-regression)); `eed3d34f` adds the text-tokenizer port and the resolve-batch/pool-pipeline change described there. Each build was installed with `npm ci`, with the native kernel built from source, and built with `npm run build`.
- **Arms.**

  | Arm | Command |
  |---|---|
  | upstream, Node | `node` 24.22.1 running upstream's `dist/bin/codegraph.js` |
  | fork, Node | `node` 24.22.1 running the fork's `dist/bin/codegraph.js` |
  | fork, Bun | `bun` 1.4.4 running the fork's `dist/bin/codegraph.js` |
  | upstream, Bun | `bun` 1.4.4 running upstream's build, with `CODEGRAPH_ALLOW_UNSAFE_NODE=1` |

  Bun reports a Node.js compatibility version of 26 or newer, and upstream refuses Node 25 and newer, so without the override upstream exits with `Unsupported Node.js version`. The fork needs no override.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 122 GiB RAM, ext4, shared with other agent sessions.
- **Environment.** `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_TELEMETRY=0`, and `CODEGRAPH_NO_DAEMON=1` for the CLI runs. Every timed command ran at normal priority under a 16 GiB memory cap and `/usr/bin/time -v`. Peak memory is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Order.** Per corpus the arms ran in the order upstream Node, fork Node, fork Bun, upstream Bun, then the reverse. Each run did a full index (`codegraph init --yes .` into an empty directory) and then two one-file syncs (each appends a comment line and a one-line symbol to the same file, then runs `codegraph sync .`). Index figures are the median of two runs and sync figures the median of four.
- **Corpora**, the same pinned commits as on 2026-10-18, shallow, no dependencies installed: gin (Go) `1bd0ecf`, Alamofire (Swift) `bda9ed5`, pretix (Python and JavaScript) `aa14505`, CPython (C and Python) `1e8ff18a`, discourse (Ruby and JavaScript) `ae5ba2c4`, supabase (React, Next.js and TypeScript) `36371de1`, n8n (Vue and TypeScript) `7cc0450a`. Each was checked at its pin, with a clean tracked tree, before the run.
- **CLI explore query.** `Engine ServeHTTP handleHTTPRequest` on gin, as before.

Both runtimes produced the same node and edge counts for the same build on every corpus.

### Host noise and the stalled arm

This run is noisier than 2026-10-18's. The host shared work with other agent sessions: 3.5 GiB of swap was in use (against 0 MiB then), and the per-row recorded 1-minute load ran 0.2–5.6 rather than 0.5–1.9. The runner waited for no `capped-*` scopes and load under 2 before each corpus (the `idle` gate in `arms.sh`), and the mirrored arm order spreads drift across arms. Following house-rules #321 there is no fixed reporting band: a cell counts as a change only when the fork/upstream gap exceeds **that comparison's own run-to-run spread** — the larger of the two arms' (max − min) / median over the repeated runs, or sd/mean for the hyperfine cells. A gap inside it is *no clear change*; a metric whose samples can't support a spread is *inconclusive*. The render script (`render-charts.mjs`) computes all of this from the raw files — no figure was typed by hand.

One MCP attempt on n8n was additionally contaminated by a second benchmark writer that had not exited; its rows are preserved in `mcp-contaminated.jsonl` and the published table comes from a clean re-run. The idle-CPU rows from the shared host are the least trustworthy measurements here and are only quoted when their spread supports it. The `eed3d34f` index/sync re-measure also overlapped a host guest-VM shutdown/reboot (12:27:05–12:27:19) inside the cpython block: every cpython metric has a pre-incident first pass, and the exposed second passes match within noise, so the rows stand as reported.

On the pre-fix build, `fork-node` indexing supabase was killed at 10:53 wall by the liveness watchdog after ~46 CPU-minutes without finishing (exit 143; the row stays in `index-sync.tsv` as evidence). Resolution was re-parsing every `.d.ts` in the project for each out-of-repo import — about 69,000 tree parses in 40 seconds measured live — because the ambient-module scan introduced upstream's `declare module` lookup (upstream `553d5d10`/`214ef50e` era, merged in fork PR #548) with no per-file memo. `d364683a` adds the memo, which shipped in `bc27fdb4`; the index and sync cells on this page were re-measured on `eed3d34f`, which lands the remaining fixes. Details: [The supabase resolution regression](#the-supabase-resolution-regression).

## Full index

Time and peak memory, median of two.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 0.96 s, 558 MiB | 1.02 s, 399 MiB | 0.93 s, 312 MiB | 0.96 s, 618 MiB |
| Alamofire | 1.35 s, 879 MiB | 1.39 s, 865 MiB | 0.93 s, 832 MiB | 1.35 s, 974 MiB |
| pretix | 9.04 s, 3.29 GiB | 9.21 s, 4.10 GiB | 8.79 s, 3.73 GiB | 7.20 s, 2.89 GiB |
| CPython | 55.0 s, 7.15 GiB | 27.8 s, 5.98 GiB | 26.8 s, 5.31 GiB | 51.0 s, 6.92 GiB |
| discourse | 19.6 s, 4.07 GiB | 13.5 s, 3.88 GiB | 12.1 s, 2.89 GiB | 18.0 s, 4.25 GiB |
| supabase | 18.2 s, 4.86 GiB | 15.8 s, 5.36 GiB | 14.6 s, 4.67 GiB | 16.4 s, 4.02 GiB |
| n8n | 74.9 s, 12.39 GiB | 49.6 s, 8.29 GiB | 45.7 s, 8.20 GiB | 59.4 s, 9.23 GiB |

Graph size. Node and edge counts match across runtimes for each build; database sizes are from the Node runs:

| Corpus | Upstream nodes / edges / database | Fork nodes / edges / database |
| --- | --- | --- |
| gin | 2,660 / 7,908 / 8.5 MiB | 3,089 / 8,404 / 10.1 MiB |
| Alamofire | 4,600 / 12,516 / 16.2 MiB | 5,614 / 13,682 / 18.3 MiB |
| pretix | 32,781 / 79,684 / 115 MiB | 32,797 / 73,402 / 138 MiB |
| CPython | 167,332 / 615,244 / 492 MiB | 167,923 / 603,861 / 573 MiB |
| discourse | 163,411 / 309,678 / 389 MiB | 168,110 / 308,849 / 432 MiB |
| supabase | 114,440 / 292,126 / 335 MiB | 152,274 / 327,838 / 460 MiB |
| n8n | 315,422 / 1,239,920 / 1.43 GiB | 328,519 / 1,233,163 / 1.67 GiB |

The edge counts differ because the builds resolve differently: the fork adds Markdown, route and dispatch links, and declines call links it cannot confirm. The timings compare the cost of building each build's own graph, not the same graph.

## One-file sync

Median of four (two syncs per run, two runs).

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 0.47 s | 0.40 s | 0.29 s | 0.29 s |
| Alamofire | 0.48 s | 0.29 s | 0.17 s | 0.31 s |
| pretix | 1.62 s | 1.55 s | 1.27 s | 1.13 s |
| CPython | 5.35 s | 2.40 s | 1.65 s | 3.93 s |
| discourse | 4.78 s | 2.34 s | 1.78 s | 3.23 s |
| supabase | 4.58 s | 2.77 s | 1.92 s | 6.69 s |
| n8n | 11.6 s | 9.04 s | 8.09 s | 8.68 s |


Upstream's supabase sync, which failed with `FOREIGN KEY constraint failed` in every prior run, now completes ([colbymchenry/codegraph#2565](https://github.com/colbymchenry/codegraph/issues/2565) was fixed upstream on 2026-10-19). Its two passes differ widely on Node (1.05 s then 8.1 s); the 4.58 s figure is the median of the four, and against that spread the comparison counts as no clear change rather than a fork win.

## The supabase resolution regression

Worth documenting because the README's earlier "ties on supabase" line hid a cliff:

- The fork indexed supabase in ~13.8 s at `d0f288ab` (the commit before fork PR #548). From `76937a38` (#548's merge of upstream `8beab37c..553d5d10`) through `31e3ab04`, supabase indexing never finished — killed at ~11 minutes by the watchdog.
- Perf on a symbolized kernel showed ~65% of resolution cycles inside `ts_parser_parse`, and a `[tree-miss]` counter showed ~69,000 parses in 40 s: all `imports` refs, 417 distinct `.d.ts` files re-parsed ~2,440 times each.
- Cause: `ts_ambient_lookup` (the port of upstream's ambient-module lookup) scans every `.d.ts` for each out-of-repo import and `ts_scope_scan` re-parsed each file on every call; the 32-entry FIFO tree cache cannot hold the working set. Upstream's equivalent is a tokenizer over source text (`scanAmbientModules` never parses a tree) and upstream memoizes the global-scope scan per file (`TS_GLOBAL_SCOPES`).
- `d364683a` memoizes `ts_scope_scan` per file in the resolver (PR #554). supabase indexes in ~40–47 s, with 152,274 nodes / 327,838 edges (more edges than pre-#548's 323,321 — the ambient work does resolve).
- The remaining ~2.3–2.5× gap vs upstream's ~17–19 s on this corpus is real: the fork still parses each `.d.ts` once per resolver worker where upstream scans text once per file. It is bounded work, not the quadratic.
- **Closed on 2026-10-29** at `c29349a3`: `ts_scope_scan` is now a port of upstream's text tokenizer + `scanAmbientModules`/`scanNamespaceBlocks` state machines (one text pass per file, memoized), and `ts_ambient_lookup` consults a lazily built `specifier → files` index (upstream's `scanned.bySpecifier`) instead of walking every `.d.ts` per import. supabase went from 41.5 s to 19.6 s on Node (inside spread of upstream's 18.2 s), n8n from 178 s to 68.4 s (upstream 74.9 s — a fork win), discourse from 22.2 s to 14.4 s (upstream 19.6 s). The port kept the fork's `export type { A as B }` clause support and fixed three divergences found in review: `export as namespace` now marks a file as a module (upstream semantics), the namespace `global` flag means "the namespace itself is a global name" — not "members are bare globals" (bare globals come only from literal `declare global` blocks and script top level) — and nested namespaces no longer leak into ambient member matching. supabase's graph is bit-identical to the pre-tokenizer baseline; n8n's edges moved 1,233,216 → 1,233,163 from the corrected namespace-global admission. The residual index losses are pretix (+33% Node) and Alamofire (+18% Node) — not ambient-scan work — and the Bun arm of supabase (+14%), n8n (+10%) and pretix (+64%).
- **Follow-up on the residual losses** (`eed3d34f`): profiling showed the pretix/supabase gaps were not resolver work — the fork used *less* CPU than upstream while taking longer. `CODEGRAPH_RESOLVE_PROFILE` put the time in the loop's `settle` stage, and worker-side timing showed why: the pipeline drains fully at every 5,000-ref batch boundary — workers wait for the batch's slowest chunk, supertypes persist, only then does the next batch fan out. Across ~150 boundaries per corpus the exposed wait summed to 8.1 s of supabase's 14.3 s resolution phase. Resolution semantics are batch-boundary semantics (a batch's own edges are never visible to its refs; pool workers resolve against a fixed snapshot), so `resolveAndPersistBatched`'s page grew 5,000 → 40,000 (`CODEGRAPH_RESOLVE_BATCH` override), the worker-recycle cadence moved from per-batch to per-40k-refs to keep its WAL-shallow intent, and the pool's CPU cap went 6 → 12 (the memory term still guards the kernel-scale OOM case; 16 workers bought ~0% for +30% peak RSS). Result, all corpora re-measured this run: supabase 19.6 → 15.8 s (upstream 18.2 — now a win inside its spread), pretix 12.0 → 9.21 s (upstream 9.04 — a tie), Alamofire 1.60 → 1.39 s (a tie), n8n 68.4 → 49.6 s (upstream 74.9 — 1.5×), CPython 33.4 → 27.8 s (upstream 55.0 — 2.0×), and every Bun arm improved except pretix (8.79 s against upstream-Bun's 7.20 — the one remaining index-time loss). The trade is peak memory: the bigger pool and pages raise pretix (+25% Node, +29% Bun) and supabase (+10% Node, +16% Bun); the other five corpora still index at 5–33% less than upstream. Edge counts are bit-identical on every corpus; golden dumps, 11/11 precision baselines and the pool-sizing tests hold.

## MCP server

`codegraph serve --mcp` on n8n; memory and CPU summed over the server and the daemon it starts. Median of two runs per arm.

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 114 ms | 52 ms | 38 ms | 64 ms |
| Start to first explore answer | 2.95 s | 2.64 s | 2.19 s | 2.43 s |
| Warm explore, median | 242 ms | 131 ms | 107 ms | 211 ms |
| Warm explore, p90 | 285 ms | 169 ms | 154 ms | 249 ms |
| 8 concurrent explores | 2.40 s | 1.91 s | 1.63 s | 1.97 s |
| Memory while busy | 3.85 GiB | 4.54 GiB | 3.50 GiB | 3.50 GiB |
| Edited file re-indexed | 1.15 s | 766 ms | 511 ms | 943 ms |
| Memory at idle | 4.40 GiB | 3.58 GiB | 1.37 GiB | 3.40 GiB |
| CPU at idle | 0.40% | 0.33% | 0.25% | 0.49% |

And on gin, same protocol:

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 110 ms | 55 ms | 37 ms | 65 ms |
| Start to first explore answer | 380 ms | 287 ms | 179 ms | 258 ms |
| Warm explore, median | 14 ms | 13 ms | 10 ms | 12 ms |
| Warm explore, p90 | 16 ms | 15 ms | 12 ms | 14 ms |
| 8 concurrent explores | 114 ms | 134 ms | 161 ms | 153 ms |
| Memory while busy | 883 MiB | 655 MiB | 468 MiB | 812 MiB |
| Edited file re-indexed | 408 ms | 383 ms | 358 ms | 357 ms |
| Memory at idle | 934 MiB | 722 MiB | 510 MiB | 764 MiB |
| CPU at idle | 0.17% | 0.10% | 0.25% | 0.45% |

The fork-Node server's busy memory on n8n (4.54 GiB against upstream's 3.85) is the one MCP regression this run — the server keeps the larger graph resident. Its idle footprint is still lower on every arm. On gin the fork is slower only on the 8-explore wave on both runtimes (+18% Node, +5% Bun); warm single queries stay ahead.

## CLI startup

`hyperfine`, mean of 20 runs on gin, query `Engine ServeHTTP handleHTTPRequest`:

| Command | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| `codegraph --version` | 73 ms | 35 ms | 21 ms | 36 ms |
| `codegraph status` | 166 ms | 114 ms | 63 ms | 90 ms |
| `codegraph explore` | 220 ms | 117 ms | 71 ms | 181 ms |

## Every result at a glance

![Heatmap of the fork's change against upstream for index time, peak memory and one-file sync on seven repositories, the MCP server on n8n and gin, and command-line startup, on Node and on Bun. Lower is better; cells inside the comparison's own run-to-run spread are marked no clear change.](fork-vs-upstream-heatmap-2026-10-27.svg?v=3)

Of 76 comparisons the fork is better in 56, shows no clear change in 11 and is worse in 9. The 9, generated from the same data as the heatmap:

- Indexing pretix: 7.20 s → 8.79 s on Bun (+22%)
- Memory indexing pretix: 3.29 GiB → 4.10 GiB on Node (+25%), 2.89 GiB → 3.73 GiB on Bun (+29%)
- Memory indexing supabase: 4.02 GiB → 4.67 GiB on Bun (+16%)
- Syncing one file in pretix: 1.13 s → 1.27 s on Bun (+12%)
- Memory busy on n8n (MCP server): 3.85 GiB → 4.54 GiB on Node (+18%)
- 8 asks at once on gin (MCP server): 114 ms → 134 ms on Node (+18%), 153 ms → 161 ms on Bun (+5%)
- File watcher on gin (MCP server): 357 ms → 358 ms on Bun (0%)
