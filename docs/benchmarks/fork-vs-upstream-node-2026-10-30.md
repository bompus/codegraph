# Fork against upstream on Node, 2026-10-30

A Node-only re-measurement after the resolver-pool threshold change (#590) and the one-snapshot pool boot (#591). It compares upstream `main` at `60b0d7a9` with the fork at `a9dc5a61` on the same 24-core bare-metal Linux host (Node 24.22.1, release kernel, `nice -n 10`).

Seven corpora, arms in mirrored order. Full index is the median of two runs; one-file sync is the median of four (two edits, two mirrored passes). Peak memory is `/usr/bin/time -v` maximum resident set. The host carried other work during the run (1-minute load 1.6-5.7), so every cell's spread is reported and a difference counts only when it clears the larger of the two arms' `(max - min) / median`.

## Full index

| Corpus | Upstream | Fork | Fork/upstream | Upstream RSS | Fork RSS |
|---|---:|---:|---:|---:|---:|
| gin (Go, 119 files) | 0.81 s | 0.90 s | +11% | 502 MiB | 786 MiB |
| Alamofire (Swift, 129 files) | 1.19 s | 1.18 s | no clear change | 861 MiB | 1.22 GiB |
| pretix (Python + JS, 1,472 files) | 8.72 s | 8.89 s | +2% | 3.33 GiB | 4.39 GiB |
| CPython (C + Python, 3,708 files) | 52.11 s | 29.96 s | **-42% (1.74x faster)** | 7.42 GiB | 6.30 GiB |
| discourse (Ruby + JS, 20,277 files) | 14.89 s | 15.56 s | +5% | 4.33 GiB | 3.84 GiB |
| supabase (React + Next.js + TS, 10,716 files) | 15.67 s | 21.50 s | +37% | 5.75 GiB | 6.10 GiB |
| n8n (Vue + TS, 24,434 files) | 71.12 s | 70.52 s | no clear change | 12.06 GiB | 8.23 GiB |

Per-arm spreads: index 0.1-6.5%, sync 0.0-3.8%. The one cell above its spread by less than a point (discourse, 0.1% vs 1.2%) is marked +5% rather than a tie.

The fork's graph is larger on four corpora because its extraction and resolution keep more nodes and edges. Supabase is the extreme: 152,274 nodes and 328,137 edges against upstream's 114,440 and 292,021 (+33% nodes, +12% edges). gin is +16% nodes, Alamofire +22%, discourse +3%, n8n +4%; CPython and pretix are within 1%. The index gaps on supabase and, in part, gin are not equivalent-work comparisons.

## One-file sync

| Corpus | Upstream | Fork | Fork vs upstream |
|---|---:|---:|---:|
| gin | 0.36 s | 0.29 s | **-19%** |
| Alamofire | 0.39 s | 0.29 s | **-26%** |
| pretix | 1.50 s | 0.93 s | **-38%** |
| CPython | 5.22 s | 2.52 s | **-52%** |
| discourse | 1.04 s | 0.98 s | **-6%** |
| supabase | 7.47 s | 3.43 s | **-54%** |
| n8n | 11.64 s | 4.55 s | **-61%** |

The fork is faster on all seven, and discourse—the one loss in the 2026-10-27 campaign—now wins by 6%.

## Regression against the 2026-10-30 morning campaign

The morning campaign at `bench-2026-10-30` measured the same corpus state and the same graph sizes, but at fork `afcdb6b6` (before #590/#591). Its upstream n8n index was 71.2 s and the fork's was 53.7-54.2 s (CPU 630%, peak 8.5 GiB); the current fork is 70.5-71.1 s (CPU 459-465%, peak 8.3 GiB). Upstream is unchanged, so the fork lost about 17 s (31%) on n8n while using fewer cores. The same direction appears on supabase (fork 15.6 s then, 21.5 s now; upstream 19.6 s then, 15.7 s now) and discourse (13.2 s then, 15.6 s now; upstream 21.7 s then, 14.9 s now).

This is not attributed: the candidate deltas are #590's smooth worker sizing, #591's main-thread prerequisite phase and single snapshot, or an interaction with the host load. Before the fork's index results are used to decide whether to keep maintaining it, bisect n8n between `afcdb6b6`, #590's merge `abc462ea`, and #591's head.

## MCP

The MCP arm (`codegraph serve --mcp`, eight concurrent explores, watcher sync, idle RSS/CPU after the 60 s retire window) was run after this document's index/sync section; its numbers are recorded in `mcp.jsonl` next to the raw TSV.
