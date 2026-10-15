# Fork vs upstream on Node and Bun (2026-10-14)

The measurements behind the README's "Measured results" and "What it costs" sections. Each build runs on Node.js 24 and on Bun, so one table covers both the fork-versus-upstream comparison and the runtime comparison. This supersedes [`fork-vs-upstream-node-bun-2026-10-13.md`](fork-vs-upstream-node-bun-2026-10-13.md), which measured upstream `34dd3d2f` against fork `f045e5f7` with the same arms, corpora and measured steps. [Since 2026-10-13](#since-2026-10-13) compares the two runs. The mid-size resolver-pool sweep, the idle CPU profile and the test-suite results stay in the [2026-10-12 doc](fork-vs-upstream-node-bun-2026-10-12.md); they were not re-run.

Two things changed since the last run. The fork's one-file sync in CPython, its one weak spot on 2026-10-13, is now faster than upstream's (26% on Node, 21% on Bun). And upstream's one-file sync now fails on supabase; see [below](#upstream-sync-fails-on-supabase).

## Method

The scripts are in [`scripts/benchmarks/runtime/`](../../scripts/benchmarks/runtime/): `run-index-sync.sh`, `run-mcp.sh` and `run-cli.sh`, with the four arms below. `render-charts.mjs` turns the raw output into the README chart, the heatmap and most tables on this page.

- **Builds.** Upstream `main` at `3f714bcc` and fork `fork/consolidated` at `ba972275`. The fork contains upstream through `d0bb9be5`; upstream's two later commits (`8206c8c2`, a JSX tag imported from the project renders what its module exports, and `3f714bcc`, a `.jsx` component links to the file-scope values it reads) are in the upstream arms only. Each worktree ran `npm ci`, built its own native kernel from source, and ran `npm run build`.
- **Arms.**

  | Arm | Command |
  |---|---|
  | upstream, Node | `node` 24.22.0 running upstream's `dist/bin/codegraph.js` |
  | fork, Node | `node` 24.22.0 running the fork's `dist/bin/codegraph.js` |
  | fork, Bun | `bun` 1.4.3 running the fork's `dist/bin/codegraph.js` |
  | upstream, Bun | `bun` 1.4.3 running upstream's build, with `CODEGRAPH_ALLOW_UNSAFE_NODE=1` |

  Bun reports a Node.js compatibility version of 26 or newer, and upstream refuses Node 25 and newer, so without the override upstream exits with `Unsupported Node.js version`. The fork needs no override. The previous run used Node 24.21.0.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 62 GiB RAM, ext4, shared with other agent sessions. The benchmark held a lock that other sessions on the host respect before starting heavy jobs.
- **Environment.** `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_TELEMETRY=0`, and `CODEGRAPH_NO_DAEMON=1` for the CLI runs. Every timed command ran at normal priority under a 16 GiB memory cap and `/usr/bin/time -v`. Peak memory is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Order.** Per corpus the arms ran in the order upstream Node, fork Node, fork Bun, upstream Bun, then the reverse. Each run did a full index (`codegraph init --yes .` into an empty directory) and then two one-file syncs (each appends a comment line and a one-line symbol to the same file, then runs `codegraph sync .`). Index figures are the median of two runs; sync figures are the median of four; MCP figures are the median of two.
- **Corpora**, the same pinned commits as on 2026-10-13, shallow, no dependencies installed: gin (Go) `1bd0ecf`, Alamofire (Swift) `bda9ed5`, pretix (Python and JavaScript) `aa14505`, CPython (C and Python) `1e8ff18`, discourse (Ruby and JavaScript) `ae5ba2c4`, supabase (React, Next.js and TypeScript) `36371de1`, n8n (Vue and TypeScript) `7cc0450a`.
- **CLI explore query.** `Engine ServeHTTP handleHTTPRequest` on gin, as on 2026-10-13.

Both runtimes produced the same node and edge counts for the same build on every corpus.

### Quiet-host rule

Before each step (index and sync, MCP, CLI) the driver script for this run, which is kept with the run record and not in this repository, required swap in use of at most 64 MiB, a 1-minute load average under 2 and at least 8 GiB of memory available. Each step passed at its first attempt (load 1.82, 1.48 and 0.54; 0 MiB of swap; 57 GiB available). The published runners add one check of their own: before every corpus they wait until no memory-capped job (a `capped-*` systemd user scope) runs and the load is under 2. The 1-minute load recorded with each timed index and sync run ranged from 1.56 to 5.35 (median 2.80); it includes the benchmark's own CPU from the runs just before, so it does not separate that from other sessions' work.

- **No run was rejected or repeated.** The 168 index and sync rows are the 7 corpora × 4 arms × 3 steps × 2 runs. All have return code 0 except upstream's supabase syncs below.
- **Limits.** The two runs per arm are the only spread this page offers; no wider variation criterion was set before the run, so a difference within the 3% reporting band is labelled no clear change. The host was somewhat slower than on 2026-10-13 for every arm (CPython on upstream took 47 s to index against 40 s), so compare ratios between arms within this run rather than seconds between runs. There is no output-identity gate: the two builds resolve differently, so they build different graphs. Other work on the host during a run was observed only by the load at the start of each run.

## Full index

Time and peak memory, median of two.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 1.23 s, 572 MiB | 1.21 s, 402 MiB | 1.45 s, 303 MiB | 1.46 s, 333 MiB |
| Alamofire | 1.70 s, 809 MiB | 1.90 s, 668 MiB | 1.81 s, 470 MiB | 1.63 s, 711 MiB |
| pretix | 9.85 s, 2.95 GiB | 11.3 s, 3.35 GiB | 10.7 s, 2.63 GiB | 9.26 s, 2.36 GiB |
| CPython | 47.1 s, 6.43 GiB | 41.1 s, 4.15 GiB | 41.1 s, 3.73 GiB | 51.9 s, 6.26 GiB |
| discourse | 26.0 s, 4.02 GiB | 25.8 s, 2.84 GiB | 24.3 s, 2.40 GiB | 27.7 s, 3.40 GiB |
| supabase | 20.5 s, 4.57 GiB | 20.8 s, 3.81 GiB | 21.3 s, 3.56 GiB | 20.3 s, 3.54 GiB |
| n8n | 88.3 s, 9.81 GiB | 71.2 s, 6.53 GiB | 73.5 s, 6.21 GiB | 80.9 s, 7.20 GiB |

Graph size. Node and edge counts match across runtimes for each build; database sizes are from the Node runs:

| Corpus | Upstream nodes / edges / database | Fork nodes / edges / database |
| --- | --- | --- |
| gin | 2,582 / 7,705 / 8.3 MiB | 3,089 / 8,329 / 10.1 MiB |
| Alamofire | 4,600 / 12,507 / 17.1 MiB | 5,614 / 13,673 / 19.2 MiB |
| pretix | 32,781 / 78,004 / 123 MiB | 32,797 / 74,462 / 148 MiB |
| CPython | 167,295 / 615,403 / 492 MiB | 167,929 / 600,467 / 574 MiB |
| discourse | 163,251 / 299,172 / 386 MiB | 167,950 / 299,203 / 430 MiB |
| supabase | 114,265 / 285,012 / 341 MiB | 152,099 / 320,727 / 466 MiB |
| n8n | 315,196 / 1,191,520 / 1.45 GiB | 328,293 / 1,220,253 / 1.68 GiB |

The edge counts differ because the builds resolve differently: the fork adds Markdown, route and dispatch links, and declines call links it cannot confirm. The timings compare the cost of building each build's own graph, not the same graph.

## One-file sync

Median of four.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 0.55 s | 0.47 s | 0.40 s | 0.43 s |
| Alamofire | 0.73 s | 0.58 s | 0.49 s | 0.61 s |
| pretix | 2.83 s | 2.46 s | 2.12 s | 2.31 s |
| CPython | 7.53 s | 5.54 s | 5.40 s | 6.79 s |
| discourse | 6.75 s | 3.31 s | 3.02 s | 5.51 s |
| supabase | sync failed | 3.10 s | 2.83 s | sync failed |
| n8n | 14.0 s | 4.72 s | 4.10 s | 11.4 s |

### Upstream sync fails on supabase

Upstream's `codegraph sync` exited with return code 1 and `Failed to sync: FOREIGN KEY constraint failed` in all eight supabase syncs (two per run, two runs, Node and Bun). The same edit synced with return code 0 on 2026-10-13 at upstream `34dd3d2f`. The fork syncs it. A full index of the same project succeeds. This is reported upstream as [colbymchenry/codegraph#2565](https://github.com/colbymchenry/codegraph/issues/2565); it reproduces on a small workspace project and starts at upstream `d0bb9be5`. The supabase one-file sync is therefore left out of every comparison and tally on this page, which is 2 of the 76 comparisons of 2026-10-13.

### Peak memory

Peak memory of the same one-file syncs, median of four. The README chart and its tallies do not include this row; it is listed here because the fork is worse on CPython, Alamofire and gin on Node. Return code 0 only.

| Corpus | Upstream, Node | Fork, Node | Fork vs upstream, Node | Upstream, Bun | Fork, Bun | Fork vs upstream, Bun |
|---|---|---|---|---|---|---|
| gin | 205 MiB | 229 MiB | +12% | 183 MiB | 172 MiB | −6% |
| Alamofire | 282 MiB | 329 MiB | +17% | 277 MiB | 204 MiB | −27% |
| pretix | 689 MiB | 700 MiB | no clear change | 650 MiB | 623 MiB | −4% |
| CPython | 889 MiB | 1.22 GiB | +40% | 809 MiB | 1.26 GiB | +59% |
| discourse | 840 MiB | 832 MiB | no clear change | 880 MiB | 906 MiB | no clear change |
| supabase | sync failed | 831 MiB | n/a | sync failed | 810 MiB | n/a |
| n8n | 1.14 GiB | 951 MiB | −18% | 1.23 GiB | 1.06 GiB | −14% |

## MCP server

`codegraph serve --mcp`; memory and CPU are summed over the server and the daemon it starts. Median of two runs.

### n8n

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 125 ms | 65 ms | 53 ms | 82 ms |
| Start to first explore answer | 3.97 s | 3.14 s | 2.84 s | 3.61 s |
| Warm explore, median | 700 ms | 203 ms | 223 ms | 717 ms |
| Warm explore, p90 | 791 ms | 266 ms | 312 ms | 825 ms |
| 8 concurrent explores | 3.68 s | 2.29 s | 2.36 s | 3.64 s |
| Memory while busy | 4.40 GiB | 3.88 GiB | 3.05 GiB | 3.73 GiB |
| Edited file re-indexed | 1.39 s | 873 ms | 682 ms | 1.20 s |
| Memory at idle | 4.91 GiB | 2.12 GiB | 1.14 GiB | 3.20 GiB |
| CPU at idle | 0.47% | 0.28% | 0.47% | 0.82% |

### gin

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 117 ms | 60 ms | 47 ms | 75 ms |
| Start to first explore answer | 444 ms | 347 ms | 248 ms | 334 ms |
| Warm explore, median | 26 ms | 18 ms | 21 ms | 23 ms |
| Warm explore, p90 | 37 ms | 20 ms | 24 ms | 29 ms |
| 8 concurrent explores | 255 ms | 154 ms | 223 ms | 272 ms |
| Memory while busy | 1.10 GiB | 686 MiB | 445 MiB | 994 MiB |
| Edited file re-indexed | 436 ms | 386 ms | 375 ms | 386 ms |
| Memory at idle | 1.14 GiB | 743 MiB | 446 MiB | 943 MiB |
| CPU at idle | 0.26% | 0.14% | 0.42% | 0.49% |

## CLI startup

`hyperfine`, mean ± standard deviation of 20 runs, on gin.

| Command | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| --version | 70 ± 4 ms | 35 ± 1 ms | 26 ± 1 ms | 44 ± 2 ms |
| status | 186 ± 4 ms | 134 ± 4 ms | 97 ± 6 ms | 125 ± 5 ms |
| explore | 238 ± 13 ms | 127 ± 4 ms | 102 ± 4 ms | 235 ± 5 ms |

## Every result at a glance

![Heatmap of the fork's change against upstream for index time, peak memory and one-file sync on seven repositories, the MCP server on n8n and gin, and command-line startup, on Node and on Bun. Lower is better; cells within 3% are marked no clear change; upstream's failed supabase sync is marked n/a.](fork-vs-upstream-heatmap-2026-10-14.svg?v=1)

## Since 2026-10-13

Node arms, full-index time and one-file sync time. The upstream build moved from `34dd3d2f` to `3f714bcc`, and the fork from `f045e5f7` to `ba972275`; this page does not attribute either change to a particular commit. The host was slower in this run (above), which shifts both builds up together. A change within 3% is no clear change.

| Corpus | Upstream, Node: 10-13 → 10-14 | Change | Fork, Node: 10-13 → 10-14 | Change |
|---|---|---|---|---|
| gin | 1.04 s → 1.23 s | +18% | 1.06 s → 1.21 s | +14% |
| Alamofire | 1.45 s → 1.70 s | +17% | 1.69 s → 1.90 s | +12% |
| pretix | 8.29 s → 9.85 s | +19% | 9.66 s → 11.3 s | +17% |
| CPython | 39.8 s → 47.1 s | +18% | 37.6 s → 41.1 s | +9% |
| discourse | 22.2 s → 26.0 s | +17% | 22.7 s → 25.8 s | +14% |
| supabase | 18.1 s → 20.5 s | +13% | 20.5 s → 20.8 s | no clear change |
| n8n | 85.9 s → 88.3 s | no clear change | 72.5 s → 71.2 s | no clear change |

| Corpus | Upstream, Node: 10-13 → 10-14 | Change | Fork, Node: 10-13 → 10-14 | Change |
|---|---|---|---|---|
| gin | 0.49 s → 0.55 s | +12% | 0.42 s → 0.47 s | +12% |
| Alamofire | 0.65 s → 0.73 s | +12% | 0.53 s → 0.58 s | +9% |
| pretix | 2.58 s → 2.83 s | +10% | 2.33 s → 2.46 s | +6% |
| CPython | 6.83 s → 7.53 s | +10% | 7.34 s → 5.54 s | −25% |
| discourse | 5.88 s → 6.75 s | +15% | 3.03 s → 3.31 s | +9% |
| supabase | 5.86 s → sync failed | n/a | 3.05 s → 3.10 s | no clear change |
| n8n | 13.2 s → 14.0 s | +6% | 4.44 s → 4.72 s | +6% |

## Not measured here

The mid-size resolver-pool sweep (13,900 to 111,000 pending references), the idle CPU profile of Bun and the test suite on Node and Bun are in the [2026-10-12 doc](fork-vs-upstream-node-bun-2026-10-12.md). Results of later fork-only changes are in their own pull requests; the fork build here includes the prepared-statement cache (#502) and not the NestJS route scan fix (#503) or the upstream merge through `eec9288a` (#504).
