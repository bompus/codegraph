# Fork vs upstream on Node and Bun (2026-10-03)

The measurements behind the README's "Measured results" and "What it costs"
sections. Each build runs on Node.js 24 and on Bun, so one table covers both
the fork-versus-upstream comparison and the runtime comparison. This
supersedes [`fork-vs-upstream-node-bun-2026-09-28.md`](fork-vs-upstream-node-bun-2026-09-28.md),
which measured upstream at `290e03f7` and the fork at `48903f51` with the same
method and corpora. [Since 2026-09-28](#since-2026-09-28) compares the two runs.

## Method

The scripts are in [`scripts/benchmarks/runtime/`](../../scripts/benchmarks/runtime/): `run-index-sync.sh`, `run-mcp.sh` and `run-cli.sh`, with the four arms below.

- **Builds.** Upstream `main` at `6560052a` (release 1.6.2), the last upstream
  commit merged into the fork, and fork `fork/consolidated` at `34cc55e2`.
  Each worktree ran `npm ci`, built its own native kernel from source, and ran
  `npm run build`.
- **Arms.**

  | Arm | Command |
  |---|---|
  | upstream, Node | `node` 24.21.0 running upstream's `dist/bin/codegraph.js` |
  | fork, Node | `node` 24.21.0 running the fork's `dist/bin/codegraph.js` |
  | fork, Bun | `bun` 1.4.2 running the fork's `dist/bin/codegraph.js` |
  | upstream, Bun | `bun` 1.4.2 running upstream's build, with `CODEGRAPH_ALLOW_UNSAFE_NODE=1` |

  Bun reports Node.js compatibility version 26.3.0, and upstream refuses
  Node 25 and newer, so without the override upstream exits with
  `Unsupported Node.js version: 26.3.0`. The fork needs no override.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 47 GiB RAM, ext4. Before each corpus the
  runner waited until no other session held a capped job and the 1-minute
  load average was under 2. Unlike the 2026-09-28 run, swap was not cycled
  first: 2.1 GiB of swap was in use, with about 35 GiB of memory available.
- **Environment.** `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_TELEMETRY=0`, and
  `CODEGRAPH_NO_DAEMON=1` for the CLI runs. Every CLI run used
  `nice -n 10 ~/.local/bin/run-capped 16G` and `/usr/bin/time -v`. Peak memory
  is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Order.** Per corpus the arms ran in the order upstream Node, fork Node,
  fork Bun, upstream Bun, then the reverse. Each run did a full index
  (`codegraph init --yes .` into an empty directory) and then two one-file
  syncs (a one-line edit to the same file, then `codegraph sync .`). Index
  figures are the median of two runs; sync figures are the median of four.
- **Corpora**, pinned to the same commits as on 2026-09-28, shallow, no
  dependencies installed:
  - gin (Go) `1bd0ecf`
  - Alamofire (Swift) `bda9ed5`
  - pretix (Python and JavaScript) `aa14505`
  - CPython (C and Python) `1e8ff18`
  - discourse (Ruby and JavaScript) `ae5ba2c4`
  - supabase (React, Next.js and TypeScript) `36371de1`
  - n8n (Vue and TypeScript) `7cc0450a`

All 168 CLI runs exited 0. Both runtimes of a build produced the same node and
edge counts on every corpus.

## Full index

Time and peak memory, median of two.

| Corpus | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| gin | 1.03 s, 549 MiB | 1.17 s, 479 MiB | 1.11 s, 383 MiB | 0.98 s, 318 MiB |
| Alamofire | 1.62 s, 838 MiB | 2.05 s, 664 MiB | 2.08 s, 486 MiB | 1.56 s, 720 MiB |
| pretix | 21.1 s, 3.27 GiB | 10.8 s, 3.70 GiB | 9.41 s, 3.23 GiB | 17.2 s, 2.83 GiB |
| CPython | 94.7 s, 8.34 GiB | 45.5 s, 6.41 GiB | 45.1 s, 5.73 GiB | 95.1 s, 9.94 GiB |
| discourse | 23.7 s, 3.86 GiB | 25.1 s, 2.88 GiB | 22.1 s, 2.44 GiB | 24.4 s, 3.75 GiB |
| supabase | 26.7 s, 4.26 GiB | 23.1 s, 3.95 GiB | 23.4 s, 3.65 GiB | 26.8 s, 4.10 GiB |
| n8n | 168.9 s, 9.45 GiB | 78.4 s, 6.33 GiB | 79.1 s, 5.76 GiB | 142.5 s, 7.55 GiB |

Graph size. Node and edge counts are identical across runtimes; database
sizes are from the Node runs and vary by at most 36 KiB between runs:

| Corpus | Upstream nodes / edges / database | Fork nodes / edges / database |
|---|---|---|
| gin | 2,582 / 7,802 / 8.3 MiB | 3,089 / 8,402 / 10.0 MiB |
| Alamofire | 4,600 / 12,507 / 17.1 MiB | 5,614 / 13,673 / 19.2 MiB |
| pretix | 32,322 / 81,229 / 122 MiB | 32,338 / 77,554 / 146 MiB |
| CPython | 167,253 / 620,214 / 494 MiB | 167,884 / 599,991 / 575 MiB |
| discourse | 162,890 / 299,213 / 384 MiB | 167,589 / 298,815 / 428 MiB |
| supabase | 113,457 / 284,976 / 338 MiB | 151,291 / 321,029 / 462 MiB |
| n8n | 310,034 / 1,179,729 / 1.43 GiB | 323,131 / 1,212,790 / 1.67 GiB |

The edge counts differ because the builds resolve differently: the fork adds
Markdown, route and dispatch links, and declines call links it cannot confirm.
The timings compare the cost of building each build's own graph, not the same
graph.

## One-file sync

Median of four syncs; the raw times are in the run log.

| Corpus | File | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|---|
| gin | `gin.go` | 0.47 s | 0.37 s | 0.32 s | 0.34 s |
| Alamofire | `Source/Core/Session.swift` | 0.69 s | 0.51 s | 0.46 s | 0.56 s |
| pretix | `src/pretix/base/models/orders.py` | 3.01 s | 1.90 s | 1.67 s | 2.49 s |
| CPython | `Lib/os.py` | 8.30 s | 4.67 s | 4.97 s | 7.88 s |
| discourse | `app/models/user.rb` | 6.03 s | 2.79 s | 2.64 s | 5.21 s |
| supabase | `apps/studio/data/projects/clone-mutation.ts` | 6.30 s | 2.94 s | 2.77 s | 5.48 s |
| n8n | `packages/cli/src/active-executions.ts` | 13.2 s | 4.38 s | 4.06 s | 10.8 s |

## MCP server

The method matches the [2026-09-28 run](fork-vs-upstream-node-bun-2026-09-28.md#mcp-server):
`mcp-bench.mjs` speaks JSON-RPC to `codegraph serve --mcp --path <repo>` over
stdio, with each build's index prebuilt by its own Node arm, two runs per arm
in mirrored order. The query files and the client script are unchanged since
then. Each run times 12 warm explores and records the 7th fastest as its
median and the 11th as its p90; the tables show the median of the two runs.
Memory and CPU are summed over the stdio server, the detached daemon it
starts, and their children. Idle CPU is the percentage of one core.

**n8n** (edited `packages/workflow/src/workflow.ts`):

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| `initialize` answered | 115 ms | 80 ms | 69 ms | 80 ms |
| Start to first explore answer | 4.23 s | 2.89 s | 2.66 s | 3.64 s |
| Warm explore, median | 707 ms | 227 ms | 223 ms | 682 ms |
| Warm explore, p90 | 835 ms | 286 ms | 285 ms | 827 ms |
| 8 concurrent explores | 3.62 s | 2.25 s | 2.51 s | 3.93 s |
| Memory while busy | 4.29 GiB | 3.84 GiB | 3.14 GiB | 3.82 GiB |
| Edited file re-indexed | 1.40 s | 0.87 s | 0.70 s | 1.22 s |
| Memory at idle | 4.77 GiB | 2.07 GiB | 1.22 GiB | 3.75 GiB |
| CPU at idle | 0.47% | 0.32% | 0.67% | 0.80% |

**gin** (edited `gin.go`):

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| Start to first explore answer | 597 ms | 365 ms | 290 ms | 395 ms |
| Warm explore, median | 28 ms | 26 ms | 25 ms | 26 ms |
| 8 concurrent explores | 258 ms | 200 ms | 275 ms | 320 ms |
| Memory while busy | 1,093 MiB | 737 MiB | 469 MiB | 1,025 MiB |
| Edited file re-indexed | 424 ms | 385 ms | 375 ms | 399 ms |
| Memory at idle | 1,118 MiB | 784 MiB | 494 MiB | 920 MiB |
| CPU at idle | 0.24% | 0.16% | 0.63% | 0.48% |

Warm explores on n8n are faster than on 2026-09-28: about six times for the
fork on Node (1.43 s to 227 ms) and 2.6 times for upstream on Node (1.84 s to
707 ms).
Since then the fork has read fewer rows on explore's hot paths
([#373](https://github.com/bompus/codegraph/pull/373)) and skipped
did-you-mean lookups whose note is discarded
([#375](https://github.com/bompus/codegraph/pull/375)), and both builds carry
upstream's changes since `290e03f7`. This run did not measure each change on
its own.

Bun's concurrent explores still trail the fork on Node on n8n (2.51 s against
2.25 s). Contention between worker threads reading through Bun's SQLite is
the suspected cause, not yet confirmed
([oven-sh/bun#44187](https://github.com/oven-sh/bun/issues/44187)). Bun's idle
CPU is two to four times Node's, still under 1% of one core.

Two gin figures carry noise. Upstream on Node ran first, and its two starts
took 757 and 437 ms; a cold page cache may explain the first, but cache state
was not measured. The fork's first Bun run
overlapped a browser-viewer build that a test run started on the host, and
its 8 concurrent explores took 354 ms against 195 ms in the second run.

## CLI startup

`hyperfine -N --warmup 3 --runs 20` on gin with a prebuilt index and
`CODEGRAPH_NO_DAEMON=1`, mean ± standard deviation:

| Command | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| `codegraph --version` | 63 ± 2 ms | 31 ± 2 ms | 23 ± 1 ms | 43 ± 2 ms |
| `codegraph status .` | 173 ± 8 ms | 117 ± 3 ms | 86 ± 3 ms | 121 ± 6 ms |
| `codegraph explore '<query>'` | 224 ± 16 ms | 121 ± 5 ms | 99 ± 2 ms | 233 ± 7 ms |

## Since 2026-09-28

Full index time on Node, 2026-09-28 against today:

| Corpus | Upstream `290e03f7` | Upstream `6560052a` | Fork `48903f51` | Fork `34cc55e2` |
|---|---|---|---|---|
| gin | 0.89 s | 1.03 s | 0.81 s | 1.17 s |
| Alamofire | 1.45 s | 1.62 s | 1.34 s | 2.05 s |
| pretix | 9.24 s | 21.1 s | 9.15 s | 10.8 s |
| CPython | 35.6 s | 94.7 s | 26.9 s | 45.5 s |
| discourse | 23.2 s | 23.7 s | 22.9 s | 25.1 s |
| supabase | 21.5 s | 26.7 s | 21.2 s | 23.1 s |
| n8n | 112.3 s | 168.9 s | 67.0 s | 78.4 s |

Indexing pretix on Node 24 with both upstream builds on the same host, right
after this run, in mirrored order:

| Build | Full index, two runs | Edges |
|---|---|---|
| Upstream `290e03f7` | 8.81 s, 9.13 s | 104,109 |
| Upstream `6560052a` | 19.88 s, 19.86 s | 81,229 |

Upstream `290e03f7` indexes pretix as fast as it did on 2026-09-28 (9.24 s),
so the host did not slow pretix down. Upstream's slower pretix index comes
from code merged between `290e03f7` and `6560052a`: 194 commits, many of them
changes to Python and scope-aware resolution. The commit has not been
bisected, and upstream's CPython slowdown (35.6 s to 94.7 s) was not checked
this way. The fork carries those commits, and its Python corpora slowed too
(pretix 9.15 s to 10.8 s, CPython 26.9 s to 45.5 s); whether the same commits
cause that was not tested.

## Test suite

At `34cc55e2` the fork's full suite passed on Node.js 24.21.0. It was not re-run
on Node.js 26 or Bun for this measurement; the last passes on Node.js 26.10.0
and Bun 1.4.2 were at `48903f51`, in the
[2026-09-28 run](fork-vs-upstream-node-bun-2026-09-28.md#test-suite).
