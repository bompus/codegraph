# Fork against upstream CodeGraph on Node.js and Bun, 2026-10-27

Upstream `main` at `69af19ac` against `fork/consolidated` at `0ee0116c`, each on Node.js 24.22.1 and on Bun 1.4.4, over seven corpora, the MCP server and command-line startup. It repeats the [2026-10-18 run](fork-vs-upstream-node-bun-2026-10-18.md) with the same scripts and the same pinned corpora on the current builds. Index and sync figures were re-measured **2026-10-28** on the same host at `de5a7b80` — upstream `69af19ac` unchanged — stacking the ambient-scan rework, the resolve-pipeline and work-stealing changes, and the #566–#575 resolver/explore follow-ups described in [Follow-up merges](#follow-up-merges); MCP, CLI and explore cells are unchanged by resolution-path changes and carry the earlier 2026-10-27 measurements at `bc27fdb4`. The README's "Measured results" quote this page.

## Method

The scripts are in [`scripts/benchmarks/runtime/`](../../scripts/benchmarks/runtime/): `run-index-sync.sh`, `run-mcp.sh` and `run-cli.sh`, with the four arms below. `adapt-flat-output.mjs` stages the flat output for `render-charts.mjs`, which computes every table, the heatmap and the README card from the raw rows.

- **Builds.** Upstream `main` at `69af19ac` — the commit the fork's default branch is reconciled through — and the fork at `de5a7b80` for index and sync, `bc27fdb4` for MCP, CLI and explore cells. The fork contains every upstream commit through `69af19ac` plus its own work. `bc27fdb4` carried a memo on the per-file ambient-scope scan (without it the fork's supabase index never completed — see [The supabase resolution regression](#the-supabase-resolution-regression)); `eed3d34f` added the text-tokenizer port and the resolve-batch/pool-pipeline change described there; `0ee0116c` adds the work-stealing dispatch, early batch fan-out and fire-and-forget worker recycling described in the same section. Each build was installed with `npm ci`, with the native kernel built from source, and built with `npm run build`.
- **Arms.**

  | Arm | Command |
  |---|---|
  | upstream, Node | `node` 24.22.1 running upstream's `dist/bin/codegraph.js` |
  | fork, Node | `node` 24.22.1 running the fork's `dist/bin/codegraph.js` |
  | fork, Bun | `bun` 1.4.4 running the fork's `dist/bin/codegraph.js` |
  | upstream, Bun | `bun` 1.4.4 running upstream's build, with `CODEGRAPH_ALLOW_UNSAFE_NODE=1` |

  Bun reports a Node.js compatibility version of 26 or newer, and upstream refuses Node 25 and newer, so without the override upstream exits with `Unsupported Node.js version`. The fork needs no override.
- **Host.** Bare-metal Ubuntu 26.04.1, 24 cores, 122 GiB RAM, ext4, shared with other agent sessions.
- **Environment.** `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_TELEMETRY=0`, and `CODEGRAPH_NO_DAEMON=1` for the CLI runs. Every timed command ran at normal priority under a 16 GiB memory cap and `/usr/bin/time -v`. Peak memory is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Order.** Per corpus the arms ran in the order upstream Node, fork Node, fork Bun, upstream Bun, then the reverse. Each run did a full index (`codegraph init --yes .` into an empty directory) and then two one-file syncs (each appends a comment line and a one-line symbol to the same file, then runs `codegraph sync .`). Index figures are the median of two runs and sync figures the median of four.
- **Corpora**, the same pinned commits as on 2026-10-18, shallow, no dependencies installed: gin (Go) `1bd0ecf`, Alamofire (Swift) `bda9ed5`, pretix (Python and JavaScript) `aa14505`, CPython (C and Python) `1e8ff18a`, discourse (Ruby and JavaScript) `ae5ba2c4`, supabase (React, Next.js and TypeScript) `36371de1`, n8n (Vue and TypeScript) `7cc0450a`. Each was checked at its pin, with a clean tracked tree, before the run.
- **CLI explore query.** `Engine ServeHTTP handleHTTPRequest` on gin, as before.

Both runtimes produced the same node and edge counts for the same build on every corpus.

### Host noise and the stalled arm

This run is noisier than 2026-10-18's. The host shared work with other agent sessions: 3.5 GiB of swap was in use (against 0 MiB then), and the per-row recorded 1-minute load ran 0.2–5.6 rather than 0.5–1.9. The runner waited for no `capped-*` scopes and load under 2 before each corpus (the `idle` gate in `arms.sh`), and the mirrored arm order spreads drift across arms. Following house-rules #321 there is no fixed reporting band: a cell counts as a change only when the fork/upstream gap exceeds **that comparison's own run-to-run spread** — the larger of the two arms' (max − min) / median over the repeated runs, or sd/mean for the hyperfine cells. A gap inside it is *no clear change*; a metric whose samples can't support a spread is *inconclusive*. The render script (`render-charts.mjs`) computes all of this from the raw files — no figure was typed by hand.

One MCP attempt on n8n was additionally contaminated by a second benchmark writer that had not exited; its rows are preserved in `mcp-contaminated.jsonl` and the published table comes from a clean re-run. The idle-CPU rows from the shared host are the least trustworthy measurements here and are only quoted when their spread supports it. The `eed3d34f` index/sync re-measure also overlapped a host guest-VM shutdown/reboot (12:27:05–12:27:19) inside the cpython block: every cpython metric has a pre-incident first pass, and the exposed second passes match within noise, so the rows stand as reported.

On the pre-fix build, `fork-node` indexing supabase was killed at 10:53 wall by the liveness watchdog after ~46 CPU-minutes without finishing (exit 143; the row stays in `index-sync.tsv` as evidence). Resolution was re-parsing every `.d.ts` in the project for each out-of-repo import — about 69,000 tree parses in 40 seconds measured live — because the ambient-module scan introduced upstream's `declare module` lookup (upstream `553d5d10`/`214ef50e` era, merged in fork PR #548) with no per-file memo. `d364683a` adds the memo, which shipped in `bc27fdb4`; the index and sync cells on this page were re-measured on `0ee0116c`, which lands the remaining fixes plus the work-stealing dispatch follow-up. Details: [The supabase resolution regression](#the-supabase-resolution-regression).

## Full index

Time and peak memory, median of two.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 0.96 s, 567 MiB | 1.02 s, 381 MiB | 0.91 s, 310 MiB | 0.77 s, 587 MiB |
| Alamofire | 1.42 s, 817 MiB | 1.19 s, 1.05 GiB | 1.01 s, 887 MiB | 1.38 s, 976 MiB |
| pretix | 9.69 s, 2.92 GiB | 7.48 s, 3.97 GiB | 7.15 s, 3.85 GiB | 7.20 s, 2.88 GiB |
| CPython | 53.7 s, 6.21 GiB | 25.5 s, 5.64 GiB | 25.6 s, 5.41 GiB | 52.8 s, 7.50 GiB |
| discourse | 21.7 s, 3.59 GiB | 13.2 s, 3.58 GiB | 12.4 s, 2.98 GiB | 19.8 s, 4.15 GiB |
| supabase | 19.6 s, 4.23 GiB | 15.6 s, 4.83 GiB | 14.5 s, 4.63 GiB | 16.8 s, 4.10 GiB |
| n8n | 83.0 s, 11.00 GiB | 49.4 s, 7.65 GiB | 46.6 s, 8.43 GiB | 60.5 s, 9.54 GiB |

Graph size. Node and edge counts match across runtimes for each build; database sizes are from the Node runs:

| Corpus | Upstream nodes / edges / database | Fork nodes / edges / database |
| --- | --- | --- |
| gin | 2,660 / 7,908 / 9 MiB | 3,089 / 8,404 / 10 MiB |
| Alamofire | 4,600 / 12,516 / 16 MiB | 5,614 / 13,682 / 18 MiB |
| pretix | 32,783 / 79,686 / 115 MiB | 32,799 / 73,404 / 138 MiB |
| CPython | 167,332 / 615,244 / 492 MiB | 167,923 / 603,830 / 573 MiB |
| discourse | 163,411 / 309,678 / 389 MiB | 168,110 / 308,849 / 432 MiB |
| supabase | 114,440 / 292,126 / 335 MiB | 152,274 / 327,837 / 460 MiB |
| n8n | 315,422 / 1,239,920 / 1.43 GiB | 328,519 / 1,233,163 / 1.67 GiB |

The edge counts differ because the builds resolve differently: the fork adds Markdown, route and dispatch links, and declines call links it cannot confirm. The timings compare the cost of building each build's own graph, not the same graph.

## One-file sync

Median of four (two syncs per run, two runs).

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 0.34 s | 0.27 s | 0.19 s | 0.21 s |
| Alamofire | 0.42 s | 0.27 s | 0.17 s | 0.29 s |
| pretix | 1.56 s | 0.86 s | 0.60 s | 1.11 s |
| CPython | 6.01 s | 2.65 s | 1.72 s | 4.14 s |
| discourse | 1.19 s | 1.75 s | 1.20 s | 0.92 s |
| supabase | 8.17 s | 3.05 s | 1.94 s | 6.70 s |
| n8n | 11.6 s | 3.29 s | 2.45 s | 9.17 s |


Upstream's supabase sync completes since upstream fixed [colbymchenry/codegraph#2565](https://github.com/colbymchenry/codegraph/issues/2565) on 2026-10-19. Its spread stays wide (8.17 s median of four this run, against 4.58 s last run), so that comparison still counts as a fork win only at its own wide spread. Two new losing cells appeared in the 2026-10-28 re-measure: **discourse sync** — the fork takes 1.75 s against upstream's 1.19 s on Node and 1.20 s against 0.92 s on Bun, outside spread in every sample — and **gin index** — 1.02 s against 0.96 s on Node and 0.91 s against 0.77 s on Bun, ~0.05–0.14 s of fixed fork overhead on a 119-file index. Both are real and stand as the remaining regression cells; every other index and sync cell is a fork win or a tie.

## The supabase resolution regression

Worth documenting because the README's earlier "ties on supabase" line hid a cliff:

- The fork indexed supabase in ~13.8 s at `d0f288ab` (the commit before fork PR #548). From `76937a38` (#548's merge of upstream `8beab37c..553d5d10`) through `31e3ab04`, supabase indexing never finished — killed at ~11 minutes by the watchdog.
- Perf on a symbolized kernel showed ~65% of resolution cycles inside `ts_parser_parse`, and a `[tree-miss]` counter showed ~69,000 parses in 40 s: all `imports` refs, 417 distinct `.d.ts` files re-parsed ~2,440 times each.
- Cause: `ts_ambient_lookup` (the port of upstream's ambient-module lookup) scans every `.d.ts` for each out-of-repo import and `ts_scope_scan` re-parsed each file on every call; the 32-entry FIFO tree cache cannot hold the working set. Upstream's equivalent is a tokenizer over source text (`scanAmbientModules` never parses a tree) and upstream memoizes the global-scope scan per file (`TS_GLOBAL_SCOPES`).
- `d364683a` memoizes `ts_scope_scan` per file in the resolver (PR #554). supabase indexes in ~40–47 s, with 152,274 nodes / 327,838 edges (more edges than pre-#548's 323,321 — the ambient work does resolve).
- The remaining ~2.3–2.5× gap vs upstream's ~17–19 s on this corpus is real: the fork still parses each `.d.ts` once per resolver worker where upstream scans text once per file. It is bounded work, not the quadratic.
- **Closed on 2026-10-27** at `c29349a3`: `ts_scope_scan` is now a port of upstream's text tokenizer + `scanAmbientModules`/`scanNamespaceBlocks` state machines (one text pass per file, memoized), and `ts_ambient_lookup` consults a lazily built `specifier → files` index (upstream's `scanned.bySpecifier`) instead of walking every `.d.ts` per import. supabase went from 41.5 s to 19.6 s on Node (inside spread of upstream's 18.2 s), n8n from 178 s to 68.4 s (upstream 74.9 s — a fork win), discourse from 22.2 s to 14.4 s (upstream 19.6 s). The port kept the fork's `export type { A as B }` clause support and fixed three divergences found in review: `export as namespace` now marks a file as a module (upstream semantics), the namespace `global` flag means "the namespace itself is a global name" — not "members are bare globals" (bare globals come only from literal `declare global` blocks and script top level) — and nested namespaces no longer leak into ambient member matching. supabase's graph is bit-identical to the pre-tokenizer baseline; n8n's edges moved 1,233,216 → 1,233,163 from the corrected namespace-global admission. The residual index losses are pretix (+33% Node) and Alamofire (+18% Node) — not ambient-scan work — and the Bun arm of supabase (+14%), n8n (+10%) and pretix (+64%).
- **Follow-up on the residual losses** (`eed3d34f`): profiling showed the pretix/supabase gaps were not resolver work — the fork used *less* CPU than upstream while taking longer. `CODEGRAPH_RESOLVE_PROFILE` put the time in the loop's `settle` stage, and worker-side timing showed why: the pipeline drains fully at every 5,000-ref batch boundary — workers wait for the batch's slowest chunk, supertypes persist, only then does the next batch fan out. Across ~150 boundaries per corpus the exposed wait summed to 8.1 s of supabase's 14.3 s resolution phase. Resolution semantics are batch-boundary semantics (a batch's own edges are never visible to its refs; pool workers resolve against a fixed snapshot), so `resolveAndPersistBatched`'s page grew 5,000 → 40,000 (`CODEGRAPH_RESOLVE_BATCH` override), the worker-recycle cadence moved from per-batch to per-40k-refs to keep its WAL-shallow intent, and the pool's CPU cap went 6 → 12 (the memory term still guards the kernel-scale OOM case; 16 workers bought ~0% for +30% peak RSS). Result, all corpora re-measured this run: supabase 19.6 → 15.8 s (upstream 18.2 — no clear change, inside its spread), pretix 12.0 → 9.21 s (upstream 9.04 — a tie), Alamofire 1.60 → 1.39 s (a tie), n8n 68.4 → 49.6 s (upstream 74.9 — 1.5×), CPython 33.4 → 27.8 s (upstream 55.0 — 2.0×), and every Bun arm improved except pretix (8.79 s against upstream-Bun's 7.20 — the one remaining index-time loss). The trade is peak memory: the bigger pool and pages raise pretix (+25% Node, +29% Bun) and supabase (+10% Node, +16% Bun); the other five corpora still index at 5–33% less than upstream. Edge counts are bit-identical on every corpus; golden dumps, 11/11 precision baselines and the pool-sizing tests hold.

- **Second follow-up, on top of `eed3d34f`** (`0ee0116c`): the remaining pretix-Bun loss was not kernel work either — total worker-side work held near 31 s while wall time ran ~6 s, a ~43%-utilization gap. Profiling found three pipeline flaws: all chunks of a batch were posted up front, so `busy` counted queue depth rather than cost and unlucky workers parked multi-second stragglers while peers idled; the periodic `recycleWorkers` await fully drained the overlapped pipeline (4.1 s parked on pretix); and batches never overlapped, so each batch's straggler tail serialized. `0ee0116c` fans the next batch out before the current one settles (safe under pool mode — workers resolve against a fixed snapshot, so batch-boundary ordering is only load-bearing on the single-kernel path), makes the periodic recycle a fire-and-forget FIFO message per worker (failures still surface through `pool.failed` and the existing downgrade path; `destroy()` now cancels pending recycle waiters so their timeouts cannot delay teardown), and replaces up-front posting with a bounded work-stealing queue — at most four chunks in flight per worker, refilled as results return, with the chunk size cut 500 → 250 so dense clusters spread across the pool. Re-measured on all seven corpora this run: pretix-Bun 8.79 → 7.49 s (upstream-Bun 7.20 — the last index-time loss shrank to +4%), pretix-Node 9.21 → 8.14 s (upstream-Node 9.04 — a tie became a win), CPython-Node 27.8 → 25.2 s, supabase-Node 15.8 → 15.5 s, n8n-Bun 45.7 → 45.2 s. Edge counts stay bit-identical.

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

![Heatmap of the fork's change against upstream for index time, peak memory and one-file sync on seven repositories, the MCP server on n8n and gin, and command-line startup, on Node and on Bun. Lower is better; cells inside the comparison's own run-to-run spread are marked no clear change.](fork-vs-upstream-heatmap-2026-10-27.svg?v=6)

Of 76 comparisons the fork is better in 57, shows no clear change in 6 and is worse in 13. The 13, generated from the same data as the heatmap:

- Indexing gin: 0.96 s → 1.02 s on Node (+6%), 0.77 s → 0.91 s on Bun (+18%)
- Memory indexing Alamofire: 817 MiB → 1.05 GiB on Node (+32%)
- Memory indexing pretix: 2.92 GiB → 3.97 GiB on Node (+36%), 2.88 GiB → 3.85 GiB on Bun (+34%)
- Memory indexing supabase: 4.23 GiB → 4.83 GiB on Node (+14%), 4.10 GiB → 4.63 GiB on Bun (+13%)
- Syncing one file in discourse: 1.19 s → 1.75 s on Node (+47%), 0.92 s → 1.20 s on Bun (+31%)
- Memory busy on n8n (MCP server): 3.85 GiB → 4.54 GiB on Node (+18%)
- 8 asks at once on gin (MCP server): 114 ms → 134 ms on Node (+18%), 153 ms → 161 ms on Bun (+5%)
- File watcher on gin (MCP server): 357 ms → 358 ms on Bun (0%)

No clear change (inside the comparison's own run-to-run spread): indexing pretix on Bun; memory indexing CPython on Node; memory indexing discourse on Node; syncing one file in gin on Bun; memory busy on n8n (MCP server) on Bun; CPU idle on gin (MCP server) on Bun.

## Follow-up merges

Pull requests landed on `fork/consolidated` after the tables above were generated, each measured on its stacked branch build against the same upstream numbers on the same host. The index and sync tables above were regenerated 2026-10-28 at the current head, `de5a7b80`.

| PR | Merge commit | Change | Measured result |
|---|---|---|---|
| [#558](https://github.com/bompus/codegraph/pull/558) | `a913e8c2` | Work-stealing chunk dispatch, early batch fan-out, dispatch pause during WAL backpressure, dequeue-armed recycle timer | pretix-Node 9.21 → 8.14 s (beats upstream's 9.04 s); pretix-Bun 8.79 → 7.49 s |
| [#559](https://github.com/bompus/codegraph/pull/559) | `e28058d3` | Sync retry keyed by the definition delta instead of every changed name | pretix-Bun one-file sync 1.27 → 0.68 s against upstream-Bun ~1.13 s — the sync loss became a ~40% win |
| [#560](https://github.com/bompus/codegraph/pull/560) | `c01c4a71` | Query-pool warm floor, await-ready grace, compact read-only worker connections | n8n busy RSS 4.54 → 3.10 GiB on Node (−20% vs upstream's 3.85) and → 1.24 GiB on Bun (−64%); gin 8-explore wave ~88 ms Node / ~75 ms Bun against upstream ~114/153 ms; Bun cold first explore 6.06 s → 164 ms |
| [#561](https://github.com/bompus/codegraph/pull/561) | `891e1966` | Resolver-worker SQLite cache 32 → 16 MiB; near-duplicate pair scoring reuses signing-time shingle sets | ~200 MB index RSS reclaimed, timing neutral; near-duplicate phase on pretix 458 → 284 ms, bringing pretix-Bun index within ~2% of upstream |
| [#562](https://github.com/bompus/codegraph/pull/562) | `483180b7` | Post-merge benchmark/README refresh (this section) | Documentation only |
| [#563](https://github.com/bompus/codegraph/pull/563) | `6b2e7ae1` | Kernel flat-buffer extraction restored when framework `extract()` hooks are configured — hooks run in parse workers, extras ride the transport | pretix exact graph parity (32799/73404/165700); n8n Node index RSS 8.26 → 7.41 GiB (−10%) |
| [#564](https://github.com/bompus/codegraph/pull/564) | `96afc781` | README revision bump + framework fast-path accounting | Documentation only |
| [#565](https://github.com/bompus/codegraph/pull/565) | `d9bd974c` | Exact/fuzzy name-match pools decline past 256 when the source file is generated or ≥75% of the pool is generated artifacts — never a truncated winner | pretix exact parity held; pathological ~950 ms resolve chunks bounded to ≤250 ms; overall batch wall flat (~19 k spread-out vendored-JS refs) |
| [#566](https://github.com/bompus/codegraph/pull/566) | `a403ef5e` | `js_declaration_reachable`'s candidate-side declaration walk memoized by (candidate id, ref language) — the ref-language half of the key is load-bearing because `parsed_tree` selects its parser from the reference language | pretix exact parity; wall flat — the memo check still sat behind ~6 file/tree preamble loads (reordered by #569) |
| [#567](https://github.com/bompus/codegraph/pull/567) | `330bf232` | README/benchmark doc refresh to `d9bd974c` with the #562–#565 rows | Documentation only |
| [#568](https://github.com/bompus/codegraph/pull/568) | `0ca7c001` | Explore warm path: `getSizeStats` memoized on the DB change stamp (was a guaranteed `COUNT(*)` + `GROUP BY` per explore call), `getContext`'s per-edge `getNodeById` loops batched through `getNodesByIds`, `findEdgesBetweenNodes` on the statement-shape cache | 40 focused tests green; per-call wins, no corpus-level figure — the items sit under the explore median already measured |
| [#569](https://github.com/bompus/codegraph/pull/569) | `2d385c6d` | `js_decl_scope_memo` checked before the file/tree preamble loads; `bare` regex one-entry memo per ref loop; `.d.ts` HashSet built lazily | pretix exact parity; settle 4.73 → 4.57 s (−3.5%) |
| [#570](https://github.com/bompus/codegraph/pull/570) | `e31b8947` | `bare_call_receiver` memoized on a content hash of ref-identifying fields; `is_generated_file`'s ~25 regexes memoized per unique path; both memos cleared at each batch boundary | pretix exact parity; with #571–#572, settle 4.57 → ~3.5 s (−23%; 3.2–3.7 s across two profiled index runs) |
| [#571](https://github.com/bompus/codegraph/pull/571) | `dfb8866f` | `local_declaration_scope` memo keyed by node id in its own map — drops the `format!` alloc the shared map needed for namespacing | parity 8/8; with #570/#572, settle 4.57 → ~3.5 s |
| [#572](https://github.com/bompus/codegraph/pull/572) | `645abbb8` | `apply_language_gate` filters in place (`retain`) — drops a `Vec<Arc<KNode>>` alloc per ref | parity 8/8; timing-neutral alloc shave |
| [#573](https://github.com/bompus/codegraph/pull/573) | `77c82438` | This follow-up table through #572 | Documentation only |
| [#574](https://github.com/bompus/codegraph/pull/574) | `a42f5cb8` | `callerTests` batches its per-hop caller lookups (one query per hop instead of one per candidate) | 50 focused tests green; explore-path N+1→2 queries per hop |
| [#575](https://github.com/bompus/codegraph/pull/575) | `de5a7b80` | `daemon-registry` and `mcp-daemon` suites get the 60 s process-lookup allowance `daemon-lock-holder` already had — Windows PowerShell `Get-CimInstance` can exceed the production 5 s default | 43 tests green; three prior Windows lifecycle flakes on unrelated PRs explained, none were code regressions |

What is left on the board at `de5a7b80`, after the 2026-10-28 re-measure above: **gin index** (+6% Node / +18% Bun — ~0.05–0.14 s of fixed fork overhead on a 119-file index, outside spread every sample) and **discourse sync** (+47% Node / +31% Bun — ~0.3–0.6 s, outside spread every sample) are the only remaining loss cells. pretix-Bun index — the former last residual — is now a tie (7.15 s vs 7.20 s). The pretix and supabase index-memory cells remain the disclosed trade (12 in-process worker heaps), though the fork now peaks *lower* than upstream on CPython, discourse-Bun, n8n and gin. The still-diffuse per-candidate gate cost is bounded — #569–#572 cut pretix's resolution settle 4.73 → ~3.5 s (−26%) — and #574 batched the last measured explore-path N+1. Sync wins widened sharply this run (n8n-Node 9.07 → 3.29 s, pretix-Node 1.55 → 0.86 s).
