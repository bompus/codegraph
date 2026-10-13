# Fork vs upstream on Node and Bun (2026-10-13)

The measurements behind the README's "Measured results" and "What it costs" sections. Each build runs on Node.js 24 and on Bun, so one table covers both the fork-versus-upstream comparison and the runtime comparison. This supersedes [`fork-vs-upstream-node-bun-2026-10-12.md`](fork-vs-upstream-node-bun-2026-10-12.md), which measured upstream `6560052a` against fork `ce6120be` with the same arms, corpora and measured steps. [Since 2026-10-12](#since-2026-10-12) compares the two runs. The mid-size resolver-pool sweep, the idle CPU profile and the test-suite results stay in the 2026-10-12 doc; they were not re-run.

The main change since the last run is upstream's: its full index is 1.1 to 2.4 times faster than at `6560052` (gin 1.1, pretix 2.3, CPython 2.4), so the fork's lead on indexing has narrowed and, on three corpora, reversed.

## Method

The scripts are in [`scripts/benchmarks/runtime/`](../../scripts/benchmarks/runtime/): `run-index-sync.sh`, `run-mcp.sh` and `run-cli.sh`, with the four arms below. `render-charts.mjs` turns the raw output into the README chart, the heatmap and the tables on this page.

- **Builds.** Upstream `main` at `34dd3d2f` and fork `fork/consolidated` at `f045e5f7`. The fork contains upstream through `df3ce972`; upstream's seven later commits are C++ fixes, so the arms differ in them. Of these corpora only CPython has C code. Each worktree ran `npm ci`, built its own native kernel from source, and ran `npm run build`.
- **Arms.**

  | Arm | Command |
  |---|---|
  | upstream, Node | `node` 24.21.0 running upstream's `dist/bin/codegraph.js` |
  | fork, Node | `node` 24.21.0 running the fork's `dist/bin/codegraph.js` |
  | fork, Bun | `bun` 1.4.3 running the fork's `dist/bin/codegraph.js` |
  | upstream, Bun | `bun` 1.4.3 running upstream's build, with `CODEGRAPH_ALLOW_UNSAFE_NODE=1` |

  Bun reports a Node.js compatibility version of 26 or newer, and upstream refuses Node 25 and newer, so without the override upstream exits with `Unsupported Node.js version`. The fork needs no override.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 62 GiB RAM, ext4, shared with other agent sessions. The benchmark held a lock that other sessions on the host respect before starting heavy jobs.
- **Environment.** `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_TELEMETRY=0`, and `CODEGRAPH_NO_DAEMON=1` for the CLI runs. Every timed command ran at normal priority under a 16 GiB memory cap and `/usr/bin/time -v`. Peak memory is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Order.** Per corpus the arms ran in the order upstream Node, fork Node, fork Bun, upstream Bun, then the reverse. Each run did a full index (`codegraph init --yes .` into an empty directory) and then two one-file syncs (each appends a comment line and a one-line symbol to the same file, then runs `codegraph sync .`). Index figures are the median of two runs; sync figures are the median of four; MCP figures are the median of two.
- **Corpora**, pinned to the same commits as on 2026-10-12, shallow, no dependencies installed: gin (Go) `1bd0ecf`, Alamofire (Swift) `bda9ed5`, pretix (Python and JavaScript) `aa14505`, CPython (C and Python) `1e8ff18`, discourse (Ruby and JavaScript) `ae5ba2c4`, supabase (React, Next.js and TypeScript) `36371de1`, n8n (Vue and TypeScript) `7cc0450a`.
- **CLI explore query.** `Engine ServeHTTP handleHTTPRequest` on gin. The 2026-10-12 run's query was not recorded, so the CLI `explore` row is comparable across arms within this run but not with the previous run's row.

Both runtimes of the fork produced the same node and edge counts on every corpus. Upstream on Node and on Bun differed by one node and one edge on pretix (32,782 nodes and 78,005 edges on Node, 32,781 and 78,004 on Bun); every other corpus matched.

### Quiet-host rule

A step (index and sync, MCP, CLI) started only when swap in use was at most 64 MiB, the 1-minute load average was under 2 and at least 8 GiB of memory was available; the runner waited, and cycled swap when it was in use, until that held. Within a step each runner also waited before every corpus until no memory-capped job (a `capped-*` systemd user scope) ran and the load was under 2. The 1-minute load recorded with each timed index and sync run ranged from 1.70 to 8.31 (median 3.64). It includes the benchmark's own CPU from the runs just before, so it does not separate that from other sessions' work.

- **No run was rejected or repeated inside the sample.** The previous run also sampled Windows CPU from the Windows side and rejected suspect attempts; this run did not, so a burst of load from outside the WSL guest during a run would not have been caught. The two runs per arm are the only spread this page offers; no wider variation criterion was set before the run, so a difference within the 3% reporting band is labelled no clear change.
- **A first launch was discarded.** It started with 564 MiB of swap in use, which broke the rule above because the script only logged the value then. It was stopped after about three minutes, its output was set aside, and the runner was changed to enforce the rule. A leftover index process from that launch appended 32 rows (Alamofire return code 1, pretix up to a return code 143 when it was stopped) to the new output file before it stopped; those rows are excluded, and the remaining 168 index and sync rows are the 7 corpora × 4 arms × 3 steps × 2 runs, all with return code 0.
- **Limit.** Other work on the host during a run was observed only by the load at the start of each run, not continuously.

## Full index

Time and peak memory, median of two.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 1.04 s, 567 MiB | 1.06 s, 389 MiB | 0.95 s, 301 MiB | 0.83 s, 337 MiB |
| Alamofire | 1.45 s, 806 MiB | 1.69 s, 648 MiB | 1.67 s, 466 MiB | 1.37 s, 705 MiB |
| pretix | 8.29 s, 2.97 GiB | 9.66 s, 3.37 GiB | 9.46 s, 2.55 GiB | 7.83 s, 2.40 GiB |
| CPython | 39.8 s, 6.41 GiB | 37.6 s, 4.31 GiB | 37.8 s, 3.81 GiB | 43.9 s, 5.84 GiB |
| discourse | 22.2 s, 3.87 GiB | 22.7 s, 2.78 GiB | 21.5 s, 2.43 GiB | 23.2 s, 3.51 GiB |
| supabase | 18.1 s, 4.69 GiB | 20.5 s, 3.71 GiB | 21.1 s, 3.41 GiB | 18.2 s, 3.55 GiB |
| n8n | 85.9 s, 9.80 GiB | 72.5 s, 6.69 GiB | 72.1 s, 5.99 GiB | 81.6 s, 6.97 GiB |

Graph size. Node and edge counts match across runtimes except for upstream on pretix (above); database sizes are from the Node runs:

| Corpus | Upstream nodes / edges / database | Fork nodes / edges / database |
| --- | --- | --- |
| gin | 2,582 / 7,705 / 8.3 MiB | 3,089 / 8,329 / 10.1 MiB |
| Alamofire | 4,600 / 12,507 / 17.1 MiB | 5,614 / 13,673 / 19.2 MiB |
| pretix | 32,782 / 78,005 / 123 MiB | 32,797 / 74,462 / 148 MiB |
| CPython | 167,295 / 615,403 / 492 MiB | 167,929 / 600,467 / 574 MiB |
| discourse | 163,251 / 299,172 / 386 MiB | 167,950 / 299,203 / 430 MiB |
| supabase | 114,265 / 284,165 / 341 MiB | 152,099 / 320,649 / 466 MiB |
| n8n | 315,196 / 1,191,518 / 1.45 GiB | 328,293 / 1,220,237 / 1.68 GiB |

The edge counts differ because the builds resolve differently: the fork adds Markdown, route and dispatch links, and declines call links it cannot confirm. The timings compare the cost of building each build's own graph, not the same graph.

## One-file sync

Median of four.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 0.49 s | 0.42 s | 0.35 s | 0.38 s |
| Alamofire | 0.65 s | 0.53 s | 0.45 s | 0.54 s |
| pretix | 2.58 s | 2.33 s | 2.13 s | 2.15 s |
| CPython | 6.83 s | 7.34 s | 7.80 s | 6.16 s |
| discourse | 5.88 s | 3.03 s | 2.74 s | 5.02 s |
| supabase | 5.86 s | 3.05 s | 2.65 s | 5.25 s |
| n8n | 13.2 s | 4.44 s | 4.31 s | 10.8 s |

## MCP server

`codegraph serve --mcp`; memory and CPU are summed over the server and the daemon it starts. Median of two runs.

### n8n

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 115 ms | 56 ms | 62 ms | 90 ms |
| Start to first explore answer | 3.82 s | 3.01 s | 2.78 s | 3.77 s |
| Warm explore, median | 681 ms | 204 ms | 221 ms | 681 ms |
| Warm explore, p90 | 793 ms | 276 ms | 311 ms | 810 ms |
| 8 concurrent explores | 3.64 s | 2.35 s | 2.40 s | 3.49 s |
| Memory while busy | 4.31 GiB | 3.78 GiB | 3.02 GiB | 3.68 GiB |
| Edited file re-indexed | 1.43 s | 887 ms | 707 ms | 1.19 s |
| Memory at idle | 4.80 GiB | 2.06 GiB | 1.19 GiB | 3.62 GiB |
| CPU at idle | 0.47% | 0.33% | 0.49% | 0.83% |

### gin

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 108 ms | 58 ms | 51 ms | 81 ms |
| Start to first explore answer | 430 ms | 343 ms | 273 ms | 355 ms |
| Warm explore, median | 26 ms | 20 ms | 25 ms | 23 ms |
| Warm explore, p90 | 34 ms | 23 ms | 28 ms | 28 ms |
| 8 concurrent explores | 256 ms | 163 ms | 239 ms | 280 ms |
| Memory while busy | 1.04 GiB | 694 MiB | 426 MiB | 992 MiB |
| Edited file re-indexed | 437 ms | 386 ms | 386 ms | 397 ms |
| Memory at idle | 1.08 GiB | 750 MiB | 461 MiB | 932 MiB |
| CPU at idle | 0.23% | 0.17% | 0.43% | 0.50% |

## CLI startup

`hyperfine`, mean ± standard deviation of 20 runs, on gin.

| Command | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| --version | 73 ± 4 ms | 38 ± 2 ms | 29 ± 1 ms | 49 ± 3 ms |
| status | 203 ± 10 ms | 145 ± 6 ms | 102 ± 8 ms | 140 ± 7 ms |
| explore | 259 ± 17 ms | 142 ± 6 ms | 116 ± 6 ms | 250 ± 9 ms |

## Every result at a glance

![Heatmap of the fork's change against upstream for index time, peak memory and one-file sync on seven repositories, the MCP server on n8n and gin, and command-line startup, on Node and on Bun. Lower is better; cells within 3% are marked no clear change.](fork-vs-upstream-heatmap-2026-10-13.svg?v=1)

## Since 2026-10-12

Node arms, full-index time and one-file sync time. The upstream build moved from `6560052a` to `34dd3d2f`, and the fork from `ce6120be` to `f045e5f7`; this page does not attribute either change to a particular commit. A change within 3% is no clear change.

| Corpus | Upstream, Node: 10-12 → 10-13 | Change | Fork, Node: 10-12 → 10-13 | Change |
|---|---|---|---|---|
| gin | 1.13 s → 1.04 s | −8% | 1.15 s → 1.06 s | −8% |
| Alamofire | 1.73 s → 1.45 s | −16% | 1.81 s → 1.69 s | −7% |
| pretix | 18.8 s → 8.29 s | −56% | 11 s → 9.66 s | −12% |
| CPython | 95.3 s → 39.8 s | −58% | 41.6 s → 37.6 s | −10% |
| discourse | 26.6 s → 22.2 s | −17% | 22.7 s → 22.7 s | no clear change |
| supabase | 26 s → 18.1 s | −30% | 20.9 s → 20.5 s | no clear change |
| n8n | 163.6 s → 85.9 s | −47% | 71.8 s → 72.5 s | no clear change |

| Corpus | Upstream, Node: 10-12 → 10-13 | Change | Fork, Node: 10-12 → 10-13 | Change |
|---|---|---|---|---|
| gin | 0.51 s → 0.49 s | −4% | 0.48 s → 0.42 s | −12% |
| Alamofire | 0.73 s → 0.65 s | −11% | 0.55 s → 0.53 s | −4% |
| pretix | 3.15 s → 2.58 s | −18% | 2.38 s → 2.33 s | no clear change |
| CPython | 8.54 s → 6.83 s | −20% | 7.87 s → 7.34 s | −7% |
| discourse | 5.83 s → 5.88 s | no clear change | 3.01 s → 3.03 s | no clear change |
| supabase | 6.12 s → 5.86 s | −4% | 3.17 s → 3.05 s | −4% |
| n8n | 12.8 s → 13.2 s | +3% | 4.39 s → 4.44 s | no clear change |

Upstream's Python indexing is back near its `290e03f` times (pretix 9.2 s, CPython 35.6 s), which the 2026-10-03 doc recorded before the regression it measured at `6560052`.

## Not measured here

The mid-size resolver-pool sweep (13,900 to 111,000 pending references), the idle CPU profile of Bun and the test suite on Node and Bun are in the [2026-10-12 doc](fork-vs-upstream-node-bun-2026-10-12.md). Results of later fork-only changes (the codegraph.json lookup in #491 is the latest) are in their own pull requests and are included in `f045e5f7`.
