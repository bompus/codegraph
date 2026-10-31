# Fork against upstream on Node, 2026-10-31

A Node-only comparison at the reconciled fork head against upstream `main` at the fork's catch-up target. This supersedes the 2026-10-27/28 Node+Bun campaign and the 2026-10-30 Node re-measurement for the figures it covers; those documents keep their original dates and commits.

- Upstream: `cd04b21a` (`fix(python): a base's type argument is no supertype…`).
- Fork: `cf2a37a5` (code at `962e6579`; the last two commits are docs). Catch-up batches #596/#598/#600.
- Host: 24-core bare-metal Ubuntu, 122 GiB, Node.js 24.22.2, release kernel.
- Corpora: gin, Alamofire, pretix, CPython, discourse, supabase, n8n.
- Arms in mirrored order; full index is the median of two runs, one-file sync the median of four (two edits, two mirrored passes). Peak memory is `/usr/bin/time -v` maximum resident set.
- A difference counts only when the fork/upstream gap exceeds the larger of the two arms' `(max − min) / median` for that metric. Spreads are listed per cell; no fixed band is used.
- The host benchmark lock was held for the campaign and the admission check passed before it: CPU 1.69 busy cores of 4 allowed, 101.3 GiB available of 8 required, memory-full PSI 0 ms of 10, I/O-full PSI 0.49 ms of 100. Receipt: `~/cg-scratch/bench-2026-10-31c/admission.json`; raw runs `index-sync.tsv`, `mcp.jsonl` beside it.

Two earlier attempts in this window were discarded: the admission script compared `/proc/pressure` microseconds to millisecond limits and falsely refused, and the upstream arm's `dist/extraction/wasm/` was empty because the build skipped `copy-assets`, which lost 613 files on CPython (`Failed to get parser`). Only the run above is used; its upstream graph matches the fork's coverage (169,618 nodes on CPython against 170,311).

## Full index

| Corpus | Upstream | Fork | Fork / upstream | Verdict | Upstream RSS | Fork RSS | Nodes up/fork | Edges up/fork |
|---|---:|---:|---:|---|---:|---:|---:|---:|
| gin | 0.84 s | 0.89 s | 1.06 | **6% slower** (spreads 2.4% / 2.2%) | 450 MiB | 738 MiB | 2,660 / 3,089 | 8,050 / 8,546 |
| Alamofire | 1.24 s | 1.16 s | 0.94 | **7% faster** (1.6% / 1.7%) | 830 MiB | 1,159 MiB | 4,600 / 5,614 | 12,512 / 13,682 |
| pretix | 9.41 s | 9.13 s | 0.97 | no clear change (0.5% / 6.6%) | 3,480 MiB | 4,441 MiB | 32,783 / 32,799 | 79,319 / 73,769 |
| CPython | 46.30 s | 31.38 s | 0.68 | **1.47x faster** (0.3% / 1.7%) | 7,420 MiB | 6,431 MiB | 169,618 / 170,311 | 615,814 / 605,273 |
| discourse | 15.20 s | 15.71 s | 1.03 | **3% slower** (0.4% / 0.3%) | 4,418 MiB | 3,850 MiB | 163,411 / 168,110 | 314,691 / 310,348 |
| supabase | 15.80 s | 21.55 s | 1.36 | **36% slower** (0.5% / 0.1%), 33% more nodes | 5,631 MiB | 6,257 MiB | 114,440 / 152,274 | 292,021 / 328,137 |
| n8n | 71.12 s | 70.44 s | 0.99 | **1% faster** (0.3% / 0.0%) | 12,352 MiB | 8,659 MiB | 315,422 / 328,519 | 1,239,521 / 1,236,342 |

Peak-memory change, fork against upstream: gin +64%, Alamofire +40%, pretix +28%, CPython −13%, discourse −13%, supabase +11%, n8n −30%. The fork's larger graphs on gin, Alamofire and supabase are part of the memory and time difference; pretix and CPython are within 1% on nodes.

## One-file sync

| Corpus | Upstream | Fork | Upstream / fork | Verdict |
|---|---:|---:|---:|---|
| gin | 0.37 s | 0.30 s | 1.23 | **19% faster** (spreads 2.7% / 0.0%) |
| Alamofire | 0.41 s | 0.29 s | 1.41 | **29% faster** (2.4% / 3.4%) |
| pretix | 1.52 s | 0.92 s | 1.65 | **39% faster** (0.7% / 1.1%) |
| CPython | 5.21 s | 2.56 s | 2.03 | **51% faster** (0.8% / 1.2%) |
| discourse | 1.04 s | 0.99 s | 1.04 | **4% faster** (1.0% / 1.0%) |
| supabase | 7.41 s | 3.44 s | 2.15 | **53% faster** (0.4% / 0.3%) |
| n8n | 11.34 s | 4.22 s | 2.69 | **63% faster** (0.6% / 0.2%) |

The fork syncs faster on all seven corpora; the smallest gap, discourse, is small but clears both spreads.

## MCP server on n8n

`codegraph serve --mcp`; memory and CPU summed over the server and the daemon. Two runs per arm; medians.

| | Upstream | Fork | Change |
|---|---:|---:|---:|
| `initialize` answered | 117 ms | 58 ms | 2.0x faster |
| Start to first explore answer | 2.96 s | 2.64 s | 11% faster |
| Explore, warm median | 243 ms | 136 ms | 1.79x faster |
| Explore, p90 | 286 ms | 177 ms | 1.62x faster |
| 8 explores at once | 2.41 s | 1.87 s | 1.28x faster |
| Memory while busy (process tree) | 3.81 GiB | 3.00 GiB | 21% lower |
| Edited file re-indexed by the watcher | 689 ms | 779 ms | **13% slower** |
| Memory at idle (process tree, 70 s after last query) | 4.38 GiB | 2.18 GiB | 50% lower |
| CPU at idle, share of one core | 0.40% | 0.20% | lower |

The watcher sync is the one MCP loss. The CLI's one-file sync is faster (above) because it resolves in the foreground; the daemon's watcher also runs the deferred synthesized-edge refresh, and that pass is heavier in the fork.

### Idle memory: which number is which

Three Node figures exist for n8n and they are the same measure — the process tree's RSS at the harness's idle sample, 70 s after the last query — at different heads:

- 2026-10-28, fork `de5a7b80`: 3.58 GiB (the README row before this run).
- 2026-10-30, fork `a9dc5a61`: 19.7 GiB. This was a real regression on that build, not a different metric.
- 2026-10-31, fork `cf2a37a5`: 2.18 GiB, after #593 shares one native resolver across the importer name-match rescan.

The earlier "20 GB → 2 GB" statement is the 2026-10-30 to 2026-10-31 change on this harness. It is not comparable to a JS-heap-only figure, and the 2026-10-28 3.58 GiB row should not be read as contradicting it.

## What was not measured

- Bun. The 2026-10-28 Node+Bun campaign remains the source for Bun; this run is Node-only.
- CLI startup. The 2026-10-28 hyperfine table in the README remains the source and is dated there.
- The 40 upstream commits' individual effects. This is a whole-tree comparison, not a per-commit attribution.

## Reproducing

The raw run is `~/cg-scratch/bench-2026-10-31c/` (`index-sync.tsv`, `mcp.jsonl`, `admission.json`, logs). The scripts are `scripts/benchmarks/runtime/run-index-sync.sh`, `run-mcp.sh`, `index-sync.sh` and `mcp-bench.mjs`; the corpus list is `~/cg-scratch/index-sync-2026-10-28.list`. The upstream arm needs its own production `node_modules` and `npm run copy-assets` (WASM grammars); the fork uses the native kernel.
