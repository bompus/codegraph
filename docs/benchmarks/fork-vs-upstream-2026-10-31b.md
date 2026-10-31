# Fork against upstream on Node, 2026-10-31, after the prerequisite-pool fix

A Node-only comparison at the fork head that fixes the index regression the first 2026-10-31 run exposed. It supersedes that run for every figure it covers; [fork-vs-upstream-2026-10-31.md](fork-vs-upstream-2026-10-31.md) keeps the pre-fix data and the regression record.

- Upstream: `cd04b21a` (`fix(python): a base's type argument is no supertype…`).
- Fork: `c63e3d7c` (`perf(resolve): pool the prerequisite phase again`), on top of `0d4c48dc`.
- Host: 24-core bare-metal Ubuntu, 122 GiB, Node.js 24.22.2, release kernel.
- Corpora: gin, Alamofire, pretix, CPython, discourse, supabase, n8n.
- Arms in mirrored order; full index is the median of two runs, one-file sync the median of four (two edits, two mirrored passes). Peak memory is `/usr/bin/time -v` maximum resident set.
- A difference counts only when the fork/upstream gap exceeds the larger of the two arms' `(max − min) / median` for that metric. Spreads are listed per cell; no fixed band is used.
- The host benchmark lock was held for the campaign and the admission check passed before it: CPU 0.8 busy cores of 4 allowed, 98.9 GiB available of 8 required, memory-full PSI 0 ms of 10, I/O-full PSI 9.4 ms of 100. Receipt: `~/cg-scratch/bench-2026-10-31d/admission.json`; raw runs `index-sync.tsv`, `mcp.jsonl` beside it.

## The regression this run fixes

The first 2026-10-31 run at `cf2a37a5` was 36% slower than upstream on supabase and level on n8n, but the fork's own index had slowed at matched graph sizes against the 2026-10-28 build. A bisect on the same host and corpus state (fresh index per run) names the commit:

| Commit | What it is | supabase | n8n | n8n index CPU |
|---|---|---:|---:|---:|
| `216093df` | parent of #590 | 17.27 / 17.46 s | 54.46 / 57.22 s | 630–636% |
| `abc462ea` | #590 (pool sizing) | 17.98 / 18.29 s | 55.89 / 58.48 s | 626–627% |
| `05077514` | #591 (early spawn, one snapshot) | 21.84 / 22.51 s | 70.34 / 70.70 s | 458–462% |

`#590` alone is inside the pre-#590 spread; its two rows above ran at 1-minute load 9–11, and both corpora sit above the 25k-refs-per-worker cap, so the sizing change does not move their worker count. `#591` is the regression: it moved the prerequisite (`imports`/`extends`/`implements`) batches onto the main-thread kernel so the workers could open one post-prerequisite snapshot. That saved a snapshot copy and serialized the whole prerequisite phase on one core — the index CPU drop from ~630% to ~460% is that phase losing its workers.

The fix (`c63e3d7c`) opens the workers on the pre-prerequisite snapshot so the prerequisite refs resolve across the pool, then takes a second snapshot once their supertype edges persist and recycles the workers onto it. It keeps #591's early spawn, single worker-script lookup, `loaded` first message and synthesis-boundary open, and refreshes before synthesis when a sweep had no calls batch. An interleaved A/B on the same host and corpora:

| Corpus | `0d4c48dc` (before) | `c63e3d7c` (after) |
|---|---:|---:|
| supabase | 21.60 / 21.88 s | 17.11 / 17.27 s |
| n8n | 70.72 / 73.17 s | 53.58 / 53.91 s |

The cost of the second snapshot generation is peak memory: n8n's index peak is 8.8–8.9 GiB here against 8.2–8.5 GiB before, still 27% below upstream's 11.9 GiB.

## Full index

| Corpus | Upstream | Fork | Fork / upstream | Verdict | Upstream RSS | Fork RSS | Nodes up/fork | Edges up/fork |
|---|---:|---:|---:|---|---:|---:|---:|---:|
| gin | 0.81 s | 0.88 s | 1.08 | **8% slower** (spreads 1.2% / 0.0%) | 496 MiB | 744 MiB | 2,660 / 3,089 | 8,050 / 8,546 |
| Alamofire | 1.23 s | 1.17 s | 0.96 | **4% faster** (1.6% / 0.9%) | 850 MiB | 1,168 MiB | 4,600 / 5,614 | 12,512 / 13,682 |
| pretix | 9.13 s | 8.08 s | 0.89 | **11% faster** (0.2% / 9.0%) | 3,547 MiB | 4,367 MiB | 32,783 / 32,799 | 79,319 / 73,769 |
| CPython | 46.34 s | 27.06 s | 0.58 | **1.71x faster** (0.1% / 0.2%) | 7,388 MiB | 6,168 MiB | 169,618 / 170,311 | 615,814 / 605,273 |
| discourse | 15.13 s | 14.38 s | 0.95 | **5% faster** (0.5% / 0.1%) | 4,440 MiB | 3,906 MiB | 163,411 / 168,110 | 314,691 / 310,348 |
| supabase | 15.70 s | 17.37 s | 1.11 | **11% slower** (2.7% / 2.4%), 33% more nodes | 5,830 MiB | 5,877 MiB | 114,440 / 152,274 | 292,021 / 328,137 |
| n8n | 71.36 s | 53.95 s | 0.76 | **1.32x faster** (0.5% / 0.2%) | 12,219 MiB | 8,867 MiB | 315,422 / 328,519 | 1,239,521 / 1,236,342 |

The fork indexes faster on five of seven corpora, by 4% (Alamofire) to 1.71× (CPython). pretix's 11% is close to its 9.0% fork spread but clears it. The two losses are gin (8%, 0.07 s absolute on a 119-file repo) and supabase (11% on a graph with 33% more nodes and 12% more edges; per node the fork is faster). Peak-memory change, fork against upstream: gin +50%, Alamofire +37%, pretix +23%, CPython −17%, discourse −12%, supabase +1%, n8n −27%.

## One-file sync

| Corpus | Upstream | Fork | Upstream / fork | Verdict |
|---|---:|---:|---:|---|
| gin | 0.37 s | 0.30 s | 1.23 | **19% faster** (spreads 0.0% / 3.3%) |
| Alamofire | 0.39 s | 0.28 s | 1.39 | **28% faster** (0.0% / 3.6%) |
| pretix | 1.50 s | 0.93 s | 1.62 | **38% faster** (0.7% / 1.1%) |
| CPython | 5.21 s | 2.54 s | 2.05 | **51% faster** (1.2% / 0.8%) |
| discourse | 1.06 s | 0.98 s | 1.08 | **8% faster** (3.8% / 3.1%) |
| supabase | 7.49 s | 3.45 s | 2.17 | **54% faster** (5.3% / 0.9%) |
| n8n | 11.37 s | 4.23 s | 2.69 | **63% faster** (0.7% / 2.1%) |

The fork syncs faster on all seven corpora, unchanged by the pool fix; the smallest gap, discourse, clears its widest spread.

## MCP server on n8n

`codegraph serve --mcp`; memory and CPU summed over the server and the daemon. Two runs per arm; medians.

| | Upstream | Fork | Change |
|---|---:|---:|---:|
| `initialize` answered | 115 ms | 64 ms | 1.79x faster |
| Start to first explore answer | 2.91 s | 2.70 s | 7% faster |
| Explore, warm median | 240 ms | 138 ms | 1.75x faster |
| Explore, p90 | 282 ms | 178 ms | 1.58x faster |
| 8 explores at once | 2.38 s | 1.88 s | 1.26x faster |
| Memory while busy (process tree) | 3.81 GiB | 3.02 GiB | 21% lower |
| Edited file re-indexed by the watcher | 702 ms | 766 ms | **9% slower** |
| Memory at idle (process tree, 70 s after last query) | 4.46 GiB | 2.19 GiB | 51% lower |
| CPU at idle, share of one core | 0.40% | 0.20% | lower |

The watcher sync is the one MCP loss. The CLI's one-file sync is faster (above) because it resolves in the foreground; the daemon's watcher also runs the deferred synthesized-edge refresh, and that pass is heavier in the fork.

### Idle memory: which number is which

The Node figures for n8n are the same measure — the process tree's RSS at the harness's idle sample, 70 s after the last query — at different heads:

- 2026-10-27, fork `bc27fdb4`: 3.58 GiB (the old README row).
- 2026-10-28, fork `5784e289`: 4.21 GiB (the clean re-measure).
- 2026-10-30, fork `a9dc5a61`: 19.7 GiB. This was a real regression on that build, not a different metric.
- 2026-10-31, fork `cf2a37a5`: 2.18 GiB, after #593 shares one native resolver across the importer name-match rescan.
- 2026-10-31, fork `c63e3d7c`: 2.19 GiB, this run.

The earlier "20 GB → 2 GB" statement is the 2026-10-30 to 2026-10-31 change on this harness. It is not comparable to a JS-heap-only figure, and the 3.58 GiB row should not be read as contradicting it.

## What was not measured

- Bun. The 2026-10-28 Node+Bun campaign remains the source for Bun; this run is Node-only.
- CLI startup. The 2026-10-27 hyperfine table at fork `bc27fdb4` against upstream `69af19ac` remains the source and is dated in the README.
- The 40+ upstream commits' individual effects. This is a whole-tree comparison, not a per-commit attribution.

## Reproducing

The raw run is `~/cg-scratch/bench-2026-10-31d/` (`index-sync.tsv`, `mcp.jsonl`, `admission.json`, logs), and the bisect/A-B receipts are in `~/cg-scratch/bisect-index-20261031/`. The scripts are `scripts/benchmarks/runtime/run-index-sync.sh`, `run-mcp.sh`, `index-sync.sh`, `mcp-bench.mjs` and the card generator `render-node-cards.py`; the corpus list is `~/cg-scratch/index-sync-2026-10-28.list`. The upstream arm needs its own production `node_modules` and `npm run copy-assets` (WASM grammars); the fork uses the native kernel.
