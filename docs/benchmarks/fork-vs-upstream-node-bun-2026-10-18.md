# Fork against upstream CodeGraph on Node.js and Bun, 2026-10-18

Upstream `main` at `986de95c` against `fork/consolidated` at `633fc82c`, each on Node.js 24.22.0 and on Bun 1.4.3, over seven corpora, the MCP server and command-line startup. It repeats the [2026-10-14 run](fork-vs-upstream-node-bun-2026-10-14.md) with the same scripts and corpora on the current builds. The README's "Measured results" quote this page.

## Method

The scripts are in [`scripts/benchmarks/runtime/`](../../scripts/benchmarks/runtime/): `run-index-sync.sh`, `run-mcp.sh` and `run-cli.sh`, with the four arms below. `render-charts.mjs` turns the raw output into the README chart, the heatmap and most tables on this page.

- **Builds.** Upstream `main` at `986de95c` and fork `fork/consolidated` at `633fc82c`. The fork contains every upstream commit before `986de95c` and its own changes, among them the mid-size resolver pool (#506), the Kotlin, regex-cache and WAL-checkpoint changes (#515 to #519), imported-constant member links (#523) and the markup arrow-parameter guard (#526). It lacks one upstream commit, `986de95c` (an import through `@/` or `~/` in a Vue project links the module's file), so the arms differ in it. Each build was installed with `npm ci`, with the native kernel built from source, and built with `npm run build`.
- **Arms.**

  | Arm | Command |
  |---|---|
  | upstream, Node | `node` 24.22.0 running upstream's `dist/bin/codegraph.js` |
  | fork, Node | `node` 24.22.0 running the fork's `dist/bin/codegraph.js` |
  | fork, Bun | `bun` 1.4.3 running the fork's `dist/bin/codegraph.js` |
  | upstream, Bun | `bun` 1.4.3 running upstream's build, with `CODEGRAPH_ALLOW_UNSAFE_NODE=1` |

  Bun reports a Node.js compatibility version of 26 or newer, and upstream refuses Node 25 and newer, so without the override upstream exits with `Unsupported Node.js version`. The fork needs no override.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 62 GiB RAM, ext4, shared with other agent sessions. The benchmark held a lock that other sessions on the host respect before starting heavy jobs, and took it only after the host had been idle for 10 minutes.
- **Environment.** `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_TELEMETRY=0`, and `CODEGRAPH_NO_DAEMON=1` for the CLI runs. Every timed command ran at normal priority under a 16 GiB memory cap and `/usr/bin/time -v`. Peak memory is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Order.** Per corpus the arms ran in the order upstream Node, fork Node, fork Bun, upstream Bun, then the reverse. Each run did a full index (`codegraph init --yes .` into an empty directory) and then two one-file syncs (each appends a comment line and a one-line symbol to the same file, then runs `codegraph sync .`). Index figures are the median of two runs and sync figures the median of four.
- **Corpora**, the same pinned commits as on 2026-10-14, shallow, no dependencies installed: gin (Go) `1bd0ecf`, Alamofire (Swift) `bda9ed5`, pretix (Python and JavaScript) `aa14505`, CPython (C and Python) `1e8ff18`, discourse (Ruby and JavaScript) `ae5ba2c4`, supabase (React, Next.js and TypeScript) `36371de1`, n8n (Vue and TypeScript) `7cc0450a`. Each was checked at its pin, with a clean tracked tree, before the run.
- **CLI explore query.** `Engine ServeHTTP handleHTTPRequest` on gin, as before.

Both runtimes produced the same node and edge counts for the same build on every corpus.

### Quiet-host rule

Before each step (index and sync, MCP, CLI) the driver required swap in use of at most 64 MiB, a 1-minute load average under 2 and at least 8 GiB of memory available. Each step passed at its first attempt (load 1.87, 1.24 and 0.48; 0 MiB of swap; 57 to 58 GiB available).

- **No run was rejected or repeated.** The 168 index and sync rows are the 7 corpora × 4 arms × 3 steps × 2 runs. All have return code 0 except upstream's supabase syncs below. The index and sync step took 31 minutes, the MCP step 42 minutes and the CLI step 33 seconds.
- **Limits.** The two runs per arm are the only spread this page offers; no wider variation criterion was set before the run, so a difference within the 3% reporting band is labelled no clear change, and the percentages are for the medians of two runs. The protocol was written before the first timed run. The host was faster than on 2026-10-14 for most arms (CPython on upstream took 40 s to index against 47 s), so compare ratios between arms, not seconds across runs.

## Full index

Time and peak memory, median of two.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 1.06 s, 577 MiB | 1.02 s, 395 MiB | 1.24 s, 293 MiB | 1.22 s, 322 MiB |
| Alamofire | 1.44 s, 830 MiB | 1.39 s, 953 MiB | 1.19 s, 643 MiB | 1.35 s, 706 MiB |
| pretix | 8.36 s, 2.94 GiB | 9.40 s, 3.25 GiB | 9.27 s, 2.50 GiB | 7.75 s, 2.36 GiB |
| CPython | 40.2 s, 6.25 GiB | 33.3 s, 4.10 GiB | 34.0 s, 3.61 GiB | 42.4 s, 6.45 GiB |
| discourse | 20.7 s, 3.86 GiB | 20.8 s, 2.95 GiB | 18.6 s, 2.23 GiB | 20.3 s, 3.71 GiB |
| supabase | 18.6 s, 4.66 GiB | 18.7 s, 3.82 GiB | 18.9 s, 3.34 GiB | 18.4 s, 3.76 GiB |
| n8n | 84.5 s, 9.97 GiB | 66.3 s, 6.30 GiB | 66.0 s, 6.07 GiB | 74.7 s, 7.49 GiB |

Graph size. Node and edge counts match across runtimes for each build; database sizes are from the Node runs:

| Corpus | Upstream nodes / edges / database | Fork nodes / edges / database |
| --- | --- | --- |
| gin | 2,582 / 7,705 / 8.3 MiB | 3,089 / 8,329 / 10.1 MiB |
| Alamofire | 4,600 / 12,507 / 17.1 MiB | 5,614 / 13,673 / 19.2 MiB |
| pretix | 32,781 / 78,004 / 123 MiB | 32,797 / 74,467 / 148 MiB |
| CPython | 167,295 / 615,485 / 492 MiB | 167,929 / 600,516 / 574 MiB |
| discourse | 163,251 / 299,172 / 386 MiB | 167,950 / 299,725 / 430 MiB |
| supabase | 114,265 / 284,963 / 341 MiB | 152,099 / 323,321 / 467 MiB |
| n8n | 315,196 / 1,191,210 / 1.45 GiB | 328,293 / 1,223,018 / 1.68 GiB |

The edge counts differ because the builds resolve differently: the fork adds Markdown, route and dispatch links, and declines call links it cannot confirm. The timings compare the cost of building each build's own graph, not the same graph.

## One-file sync

Median of four (two syncs per run, two runs).

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| gin | 0.47 s | 0.41 s | 0.34 s | 0.36 s |
| Alamofire | 0.68 s | 0.55 s | 0.44 s | 0.52 s |
| pretix | 2.55 s | 2.19 s | 2.00 s | 2.12 s |
| CPython | 6.47 s | 4.69 s | 4.62 s | 5.84 s |
| discourse | 5.88 s | 2.74 s | 2.54 s | 4.70 s |
| supabase | sync failed | 3.17 s | 2.73 s | sync failed |
| n8n | 13.1 s | 4.37 s | 3.79 s | 10.7 s |

### Upstream sync fails on supabase

Upstream's `codegraph sync` exited with return code 1 and `Failed to sync: FOREIGN KEY constraint failed` in all eight supabase syncs (two per run, two runs, Node and Bun), as on 2026-10-14 ([colbymchenry/codegraph#2565](https://github.com/colbymchenry/codegraph/issues/2565)). The fork syncs it. The supabase one-file sync is therefore left out of every comparison and tally on this page, so this run reports 74 comparisons.

### Peak memory

Peak memory of the same one-file syncs, median of four. The README chart and its tallies do not include this row; it is listed here because the fork is higher on several corpora. Return code 0 only.

| Corpus | Upstream, Node | Fork, Node | Fork vs upstream, Node | Upstream, Bun | Fork, Bun | Fork vs upstream, Bun |
|---|---|---|---|---|---|---|
| gin | 207 MiB | 228 MiB | +10% | 183 MiB | 169 MiB | −8% |
| Alamofire | 288 MiB | 329 MiB | +14% | 278 MiB | 207 MiB | −26% |
| pretix | 701 MiB | 713 MiB | no clear change | 645 MiB | 646 MiB | no clear change |
| CPython | 909 MiB | 1.23 GiB | +39% | 857 MiB | 1.26 GiB | +51% |
| discourse | 797 MiB | 832 MiB | +4% | 813 MiB | 876 MiB | +8% |
| supabase | sync failed | 851 MiB | n/a | sync failed | 831 MiB | n/a |
| n8n | 1.10 GiB | 963 MiB | −15% | 1.14 GiB | 1.08 GiB | −5% |

## MCP server

`codegraph serve --mcp`; memory and CPU are summed over the server and the daemon it starts. Median of two runs.

### n8n

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 122 ms | 71 ms | 51 ms | 85 ms |
| Start to first explore answer | 3.60 s | 2.78 s | 2.48 s | 3.25 s |
| Warm explore, median | 600 ms | 177 ms | 187 ms | 639 ms |
| Warm explore, p90 | 707 ms | 244 ms | 262 ms | 755 ms |
| 8 concurrent explores | 3.45 s | 2.30 s | 2.29 s | 3.35 s |
| Memory while busy | 4.30 GiB | 3.83 GiB | 3.01 GiB | 3.78 GiB |
| Edited file re-indexed | 1.32 s | 835 ms | 669 ms | 1.17 s |
| Memory at idle | 4.88 GiB | 2.15 GiB | 1.23 GiB | 3.24 GiB |
| CPU at idle | 0.44% | 0.35% | 0.48% | 0.74% |

### gin

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| `initialize` answered | 109 ms | 58 ms | 49 ms | 79 ms |
| Start to first explore answer | 423 ms | 349 ms | 249 ms | 350 ms |
| Warm explore, median | 25 ms | 17 ms | 19 ms | 21 ms |
| Warm explore, p90 | 32 ms | 20 ms | 22 ms | 26 ms |
| 8 concurrent explores | 246 ms | 149 ms | 210 ms | 256 ms |
| Memory while busy | 1.08 GiB | 683 MiB | 455 MiB | 1010 MiB |
| Edited file re-indexed | 437 ms | 387 ms | 374 ms | 400 ms |
| Memory at idle | 1.12 GiB | 730 MiB | 481 MiB | 954 MiB |
| CPU at idle | 0.21% | 0.14% | 0.40% | 0.48% |

## CLI startup

`hyperfine`, mean ± standard deviation of 20 runs, on gin.

| Command | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
| --- | --- | --- | --- | --- |
| --version | 64 ± 1 ms | 33 ± 2 ms | 24 ± 1 ms | 41 ± 1 ms |
| status | 176 ± 5 ms | 130 ± 3 ms | 87 ± 1 ms | 116 ± 2 ms |
| explore | 231 ± 15 ms | 121 ± 5 ms | 91 ± 1 ms | 225 ± 5 ms |

## Every result at a glance

![Heatmap of the fork's change against upstream for index time, peak memory and one-file sync on seven repositories, the MCP server on n8n and gin, and command-line startup, on Node and on Bun. Lower is better; cells within 3% are marked no clear change; upstream's failed supabase sync is marked n/a.](fork-vs-upstream-heatmap-2026-10-18.svg?v=1)

## Since 2026-10-14

Node arms, full-index time and one-file sync time. The upstream build moved from `3f714bcc` to `986de95c`, and the fork from `ba972275` to `633fc82c`; this page does not attribute either change to a particular commit. The host was faster in this run, which shifts both builds down together. A change within 3% is no clear change.

| Corpus | Upstream, Node: 10-14 → 10-18 | Change | Fork, Node: 10-14 → 10-18 | Change |
|---|---|---|---|---|
| gin | 1.23 s → 1.06 s | −13% | 1.21 s → 1.02 s | −15% |
| Alamofire | 1.70 s → 1.44 s | −15% | 1.90 s → 1.39 s | −27% |
| pretix | 9.85 s → 8.36 s | −15% | 11.3 s → 9.40 s | −17% |
| CPython | 47.1 s → 40.2 s | −15% | 41.1 s → 33.3 s | −19% |
| discourse | 26.0 s → 20.7 s | −20% | 25.8 s → 20.8 s | −19% |
| supabase | 20.5 s → 18.6 s | −9% | 20.8 s → 18.7 s | −10% |
| n8n | 88.3 s → 84.5 s | −4% | 71.2 s → 66.3 s | −7% |

| Corpus | Upstream, Node: 10-14 → 10-18 | Change | Fork, Node: 10-14 → 10-18 | Change |
|---|---|---|---|---|
| gin | 0.55 s → 0.47 s | −15% | 0.47 s → 0.41 s | −14% |
| Alamofire | 0.73 s → 0.68 s | −8% | 0.58 s → 0.55 s | −6% |
| pretix | 2.83 s → 2.55 s | −10% | 2.46 s → 2.19 s | −11% |
| CPython | 7.53 s → 6.47 s | −14% | 5.54 s → 4.69 s | −15% |
| discourse | 6.75 s → 5.88 s | −13% | 3.31 s → 2.74 s | −17% |
| supabase | sync failed → sync failed | n/a | 3.10 s → 3.17 s | no clear change |
| n8n | 14.0 s → 13.1 s | −7% | 4.72 s → 4.37 s | −8% |

## Not measured here

The mid-size resolver-pool sweep is in the [2026-10-15 doc](midsize-sweep-2026-10-15.md) and the [2026-10-12 doc](fork-vs-upstream-node-bun-2026-10-12.md), the idle CPU profile of Bun and the test suite on Node and Bun in the 2026-10-12 doc. This run covers one Node and one Bun version on one host; Windows and macOS are not measured.
