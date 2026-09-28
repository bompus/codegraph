# Fork vs upstream on Node and Bun (2026-09-28)

The measurements behind the README's "Measured results" and "What it costs"
sections. Each build runs on Node.js 24 and on Bun, so a single table covers
both the fork-versus-upstream comparison and the runtime comparison. This
supersedes the tables in
[`fork-vs-upstream-2026-09-28.md`](fork-vs-upstream-2026-09-28.md) (fork only
on Node 24, upstream at `e63fe2ec`) and the runtime table in
[`fork-vs-upstream-2026-09-26.md`](fork-vs-upstream-2026-09-26.md) (fork only).
The index-time profile in the first of those still applies.

## Method

The scripts are in [`scripts/benchmarks/runtime/`](../../scripts/benchmarks/runtime/): `run-index-sync.sh`, `run-mcp.sh` and `run-cli.sh`, with the four arms below.

- **Builds.** Upstream `main` at `290e03f7`, the last upstream commit merged
  into the fork, and fork `fork/consolidated` at `48903f51`. Each worktree ran
  `npm ci`, built its own native kernel from source, and ran `npm run build`.
- **Arms.**

  | Arm | Command |
  |---|---|
  | upstream, Node | `node` 24.21.0 running upstream's `dist/bin/codegraph.js` |
  | fork, Node | `node` 24.21.0 running the fork's `dist/bin/codegraph.js` |
  | fork, Bun | `bun` 1.4.2 running the fork's `dist/bin/codegraph.js` |
  | upstream, Bun | `bun` 1.4.2 running upstream's build, with `CODEGRAPH_ALLOW_UNSAFE_NODE=1` |

  Bun 1.4.2 was the latest Bun release. Bun reports Node.js compatibility
  version 26.3.0, and upstream refuses Node 25 and newer, so without the
  override upstream exits with `Unsupported Node.js version: 26.3.0`. The
  fork needs no override.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 47 GiB RAM, ext4. Swap was cycled before
  the run and stayed at 0 B. Before each corpus the runner waited until no
  other session held a capped job and the 1-minute load average was under 2.
  Windows-side CPU was about 9%.
- **Environment.** `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_TELEMETRY=0`, and
  `CODEGRAPH_NO_DAEMON=1` for the CLI runs. Every CLI run used
  `nice -n 10 ~/.local/bin/run-capped 16G` and `/usr/bin/time -v`. Peak memory
  is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Order.** Per corpus the arms ran in the order upstream Node, fork Node,
  fork Bun, upstream Bun, then the reverse. Each run did a full index
  (`codegraph init --yes .` into an empty directory) and then two one-file
  syncs (a one-line edit to the same file, then `codegraph sync .`). Index
  figures are the median of two runs; sync figures are the median of four.
- **Corpora**, pinned, shallow, no dependencies installed:
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
| gin | 0.89 s, 519 MiB | 0.81 s, 296 MiB | 0.80 s, 199 MiB | 0.73 s, 413 MiB |
| Alamofire | 1.45 s, 647 MiB | 1.34 s, 493 MiB | 1.29 s, 294 MiB | 1.28 s, 600 MiB |
| pretix | 9.24 s, 2.75 GiB | 9.15 s, 1.71 GiB | 8.92 s, 1.18 GiB | 9.07 s, 2.30 GiB |
| CPython | 35.6 s, 4.79 GiB | 26.9 s, 3.43 GiB | 29.7 s, 2.62 GiB | 39.4 s, 4.71 GiB |
| discourse | 23.2 s, 3.47 GiB | 22.9 s, 2.61 GiB | 20.5 s, 2.44 GiB | 23.6 s, 3.43 GiB |
| supabase | 21.5 s, 4.54 GiB | 21.2 s, 3.38 GiB | 20.9 s, 2.83 GiB | 21.9 s, 4.01 GiB |
| n8n | 112.3 s, 8.08 GiB | 67.0 s, 5.04 GiB | 69.6 s, 4.72 GiB | 107.1 s, 7.59 GiB |

Graph size, identical across runtimes:

| Corpus | Upstream nodes / edges / database | Fork nodes / edges / database |
|---|---|---|
| gin | 2,582 / 8,156 / 8.4 MiB | 3,089 / 8,721 / 10.1 MiB |
| Alamofire | 4,601 / 13,866 / 17.3 MiB | 5,615 / 15,226 / 19.5 MiB |
| pretix | 32,322 / 104,109 / 126 MiB | 32,338 / 87,943 / 148 MiB |
| CPython | 167,256 / 654,429 / 495 MiB | 168,663 / 624,245 / 578 MiB |
| discourse | 163,060 / 316,077 / 386 MiB | 167,759 / 319,090 / 431 MiB |
| supabase | 113,460 / 286,974 / 341 MiB | 151,302 / 333,335 / 463 MiB |
| n8n | 310,029 / 1,385,401 / 1.44 GiB | 323,128 / 1,262,297 / 1.68 GiB |

The edge counts differ because the builds resolve differently: the fork adds
Markdown, route and dispatch links, and declines call links it cannot confirm.
The timings compare the cost of building each build's own graph, not the same
graph.

## One-file sync

Median of four syncs; the raw times are in the run log.

| Corpus | File | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|---|
| gin | `gin.go` | 0.46 s | 0.38 s | 0.32 s | 0.36 s |
| Alamofire | `Source/Core/Session.swift` | 0.69 s | 0.46 s | 0.41 s | 0.53 s |
| pretix | `src/pretix/base/models/orders.py` | 3.08 s | 1.61 s | 1.49 s | 2.69 s |
| CPython | `Lib/os.py` | 7.38 s | 3.78 s | 3.99 s | 6.84 s |
| discourse | `app/models/user.rb` | 6.64 s | 2.88 s | 2.56 s | 5.33 s |
| supabase | `apps/studio/data/projects/clone-mutation.ts` | 6.50 s | 2.67 s | 2.53 s | 6.15 s |
| n8n | `packages/cli/src/active-executions.ts` | 14.2 s | 4.46 s | 4.92 s | 12.6 s |

Sync peak memory was 148–297 MiB on gin and Alamofire and 0.91–1.19 GiB on n8n
for every arm.

## MCP server

A client script spoke JSON-RPC to `codegraph serve --mcp --path <repo>` over
stdio, with each build's index prebuilt by its own Node arm. The arms ran in
the same mirrored order, two runs each; the table shows medians. Each run:

1. Sent `initialize`, then one `codegraph_explore`, and timed both from
   process start.
2. Ran 12 explore queries once to warm up, then again timed (median and 90th
   percentile).
3. Sent 8 explores at once (the "wave").
4. Appended a function to one file and timed until the watcher had re-indexed
   it, then reverted the file.
5. Waited 70 s, past the fork's 60 s idle retirement of extra query workers,
   then read resident memory and sampled CPU time over the next 60 s.

Memory and CPU are summed over the whole process tree: the stdio server, the
detached daemon it starts, and their children. Idle CPU is the percentage of
one core.

**n8n** (24,435 indexed files; edited `packages/workflow/src/workflow.ts`):

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| `initialize` answered | 104 ms | 81 ms | 69 ms | 79 ms |
| Start to first explore answer | 4.05 s | 3.27 s | 2.89 s | 3.64 s |
| Warm explore, median | 1.84 s | 1.43 s | 1.48 s | 1.77 s |
| Warm explore, p90 | 1.97 s | 1.51 s | 1.54 s | 1.95 s |
| 8 concurrent explores | 3.78 s | 2.13 s | 2.51 s | 3.71 s |
| Memory while busy | 5.02 GiB | 3.96 GiB | 3.16 GiB | 3.87 GiB |
| Edited file re-indexed | 1.47 s | 0.86 s | 0.67 s | 1.21 s |
| Memory at idle | 5.27 GiB | 2.40 GiB | 1.22 GiB | 3.51 GiB |
| CPU at idle | 0.48% | 0.29% | 0.59% | 0.98% |

**gin** (119 indexed files; edited `gin.go`):

| Measure | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| Start to first explore answer | 461 ms | 379 ms | 300 ms | 371 ms |
| Warm explore, median | 32 ms | 33 ms | 33 ms | 31 ms |
| 8 concurrent explores | 271 ms | 211 ms | 236 ms | 332 ms |
| Memory while busy | 1,122 MiB | 723 MiB | 473 MiB | 1,108 MiB |
| Edited file re-indexed | 438 ms | 387 ms | 387 ms | 399 ms |
| Memory at idle | 1,170 MiB | 763 MiB | 480 MiB | 1,006 MiB |
| CPU at idle | 0.26% | 0.13% | 0.58% | 0.51% |

Bun's concurrent explores trail the fork on Node (2.51 s against 2.13 s on
n8n): reads from several worker threads contend inside Bun's SQLite
([oven-sh/bun#44084](https://github.com/oven-sh/bun/issues/44084),
[#44157](https://github.com/oven-sh/bun/issues/44157)). Bun's idle CPU is
about twice Node's on both corpora, still under 1% of one core.

## CLI startup

`hyperfine -N --warmup 3 --runs 20` on gin with a prebuilt index and
`CODEGRAPH_NO_DAEMON=1`, mean ± standard deviation:

| Command | Upstream, Node | Fork, Node | Fork, Bun | Upstream, Bun |
|---|---|---|---|---|
| `codegraph --version` | 67 ± 1 ms | 33 ± 1 ms | 27 ± 1 ms | 48 ± 1 ms |
| `codegraph status .` | 179 ± 9 ms | 121 ± 4 ms | 98 ± 10 ms | 125 ± 9 ms |
| `codegraph explore '<query>'` | 226 ± 10 ms | 131 ± 4 ms | 109 ± 4 ms | 240 ± 7 ms |

## Test suite

The fork's full suite at `48903f51` passes on Node 24.21.0 (the CI runtime),
Node 26.10.0 and Bun 1.4.2 (`npm run test:bun`). Under Bun one test is
skipped for [oven-sh/bun#42891](https://github.com/oven-sh/bun/issues/42891).
