# Fork vs upstream (2026-09-28)

Paired measurements behind the README's "Measured results" and "What it costs"
sections. This replaces the fork-vs-upstream tables of
[`fork-vs-upstream-2026-09-26.md`](fork-vs-upstream-2026-09-26.md), which
compared against an older upstream (`ba3c21e5`). That document still holds the
runtime comparison (Node 24, Node 26, Bun), which was not re-run.

## Method

- **Builds.** Upstream `main` at `e63fe2ec` (2026-09-27, the last upstream
  commit merged into the fork) with its own native kernel built from source.
  Fork `fork/consolidated` at `eb2a24f6` (2026-09-28). Both built with
  `npm run build` in their own worktrees.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 47 GiB RAM, ext4, Node 24.21.0. No swap in
  use. Before each corpus's index step and again before its sync step, the
  runner waited until no other session held a capped job and the 1-minute load
  average was under 2.
- **Commands.** Every run was capped with `nice -n 10 ~/.local/bin/run-capped`
  (16G for index, 13G for sync) and timed with `/usr/bin/time -v`, with
  `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_NO_DAEMON=1` and
  `CODEGRAPH_TELEMETRY=0`. The index directory started empty for every run.
  - Index: `codegraph init --yes .`
  - One-file sync: an untimed `init`, then two timed rounds of a one-line edit
    to the same file followed by `codegraph sync .`.
- **Pairing.** Arms alternate upstream, fork, fork, upstream. Index figures are
  the median of two runs; sync figures are the median of four syncs. Peak
  memory is MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Corpora**, each at the same pinned commit as the 2026-09-26 run:
  - gin (Go) `1bd0ecf`
  - Alamofire (Swift) `bda9ed5`
  - pretix (Python and JavaScript) `aa14505`
  - CPython (C and Python) `1e8ff18`
  - discourse (Ruby and JavaScript) `ae5ba2c4`
  - supabase (React, Next.js and TypeScript) `36371de1`
  - n8n (Vue and TypeScript) `7cc0450a`

  Shallow clones, no dependencies installed.
- **Re-run corpora.** Another session's benchmark overlapped the first pass.
  Most arms repeated within 5%. Gin's sub-second upstream arm spread 9% on
  index and 13% on one sync. The CPython index (11% spread upstream, 20% fork),
  the supabase index (21% upstream) and one CPython fork sync (30%) spread
  further. CPython and supabase were measured again on a quiet host: the
  runner also required a load under 1.5 and no other process above 50% of a
  core over a 5-second sample. The tables use the re-run for those two
  corpora; their first-pass runs are superseded, not averaged in. The re-run's
  repeats agree within 5%.
- **Failures.** None. All 108 timed runs exited 0: 36 index runs (28 in the
  first pass, 8 in the re-run) and 72 syncs (56 and 16). The untimed `init`
  before each sync pair is not included in any figure.

## Full index

| Corpus | Files (up / fork) | Time (up → fork) | Peak memory | Database | Edges |
|---|---|---|---|---|---|
| gin | 110 / 119 | 0.89 → 0.86 s (−3%) | 514 → 308 MiB (−40%) | 8.4 → 10.1 MiB (+20%) | 8,156 → 8,721 (+7%) |
| Alamofire | 117 / 129 | 1.33 → 1.27 s (−5%) | 639 → 479 MiB (−25%) | 17.3 → 19.5 MiB (+13%) | 13,866 → 15,226 (+10%) |
| pretix | 1,467 / 1,473 | 9.64 → 8.91 s (−8%) | 2.77 → 1.73 GiB (−37%) | 126 → 148 MiB (+17%) | 104,109 → 87,943 (−16%) |
| CPython | 3,667 / 3,710 | 33.0 → 26.7 s (−19%) | 4.66 → 3.48 GiB (−25%) | 495 → 578 MiB (+17%) | 654,429 → 624,245 (−5%) |
| discourse | 20,034 / 20,278 | 21.8 → 22.9 s (+5%) | 3.49 → 2.53 GiB (−27%) | 386 → 431 MiB (+11%) | 316,077 → 319,090 (+1%) |
| supabase | 8,740 / 10,718 | 19.9 → 21.6 s (+8%) | 4.50 → 3.49 GiB (−22%) | 341 → 463 MiB (+36%) | 286,974 → 333,335 (+16%) |
| n8n | 23,734 / 24,435 | 104.7 → 71.7 s (−32%) | 8.18 → 5.05 GiB (−38%) | 1.44 → 1.68 GiB (+17%) | 1,385,401 → 1,262,297 (−9%) |

Per-run times, upstream then fork:

| Corpus | Upstream | Fork |
|---|---|---|
| gin | 0.93, 0.85 s | 0.86, 0.86 s |
| Alamofire | 1.35, 1.32 s | 1.26, 1.28 s |
| pretix | 9.57, 9.71 s | 8.95, 8.88 s |
| CPython | 32.4, 33.6 s | 27.2, 26.2 s |
| discourse | 21.5, 22.2 s | 23.4, 22.4 s |
| supabase | 20.3, 19.5 s | 21.0, 22.1 s |
| n8n | 100.7, 108.7 s | 72.3, 71.0 s |

The fork indexes more files because it reads Markdown (supabase: 1,978 `.md`
files). Its edge count is higher where it adds links (Markdown sections,
bindings) and lower where it declines uncertain ones.

Upstream's supabase index took 80.5 s at `ba3c21e5` and 19.9 s at `e63fe2ec`.
At `ba3c21e5` nearly all of it went to writing parse results to SQLite; which
upstream change removed that cost was not traced. Supabase is no longer the
fork's largest index-time lead. The first pass, which overlapped another
session's benchmark, gave supabase 21.6 s upstream and 20.8 s fork; the quiet
re-run above shows the fork 8% slower.

## One-file sync

| Corpus | Edited file | Upstream | Fork |
|---|---|---|---|
| gin | `gin.go` | 0.46 s, 199 MiB | 0.38 s, 212 MiB |
| Alamofire | `Source/Core/Session.swift` | 0.67 s, 250 MiB | 0.47 s, 228 MiB |
| pretix | `src/pretix/base/models/orders.py` | 3.08 s, 712 MiB | 1.75 s, 578 MiB |
| CPython | `Lib/os.py` | 7.59 s, 1,059 MiB | 3.88 s, 955 MiB |
| discourse | `app/models/user.rb` | 6.24 s, 915 MiB | 2.76 s, 813 MiB |
| supabase | `apps/studio/data/projects/clone-mutation.ts` | 6.60 s, 731 MiB | 3.01 s, 802 MiB |
| n8n | `packages/cli/src/active-executions.ts` | 13.8 s, 1,211 MiB | 4.29 s, 942 MiB |

At `ba3c21e5` upstream's syncs were faster than the fork's on pretix, CPython
and supabase (1.11, 1.44 and 1.56 s), because that upstream left inferred links
stale after a sync ([colbymchenry/codegraph#1988](https://github.com/colbymchenry/codegraph/issues/1988)).
Upstream's refresh ([#2033](https://github.com/colbymchenry/codegraph/pull/2033))
now reruns every inference pass after a sync. The fork reruns only the passes
the changed files can affect ([`docs/design/incremental-synthesis.md`](../design/incremental-synthesis.md)),
and is now faster on all seven corpora.

## Decision

The README's "Measured results" and "What it costs" sections now use these
figures. Its "slower one-file syncs" cost is removed, and the slower full index
on discourse (+5%) and supabase (+8%) is listed instead. More runs would not
change the sync result: every corpus's gap exceeds its run spread, the
narrowest being gin (21% gap, 13% spread). Supabase's index slowdown held on
the quiet re-run. Discourse's +5% is close to its 4% run spread, but the
2026-09-26 run measured the same +5%.

The profile below traces most of both slowdowns to work upstream doesn't do:
Markdown indexing on supabase and near-duplicate detection on discourse. That
work is intended, so no fix is planned for it. The two follow-ups below, the
near-duplicate pass and the maintenance step, didn't produce a change worth
making either.

## Where the index time goes

Both builds print per-phase times with `CODEGRAPH_SYNTH_TIMINGS=1`. Each corpus
ran U F F U on a quiet host (same builds, same flags as above; upstream got its
own `node_modules`, because it still needs `web-tree-sitter`). All 8 runs
exited 0. Times are the two runs per arm, in seconds.

| Phase | discourse U | discourse F | supabase U | supabase F |
|---|---|---|---|---|
| Wall time | 21.7, 22.5 | 25.1, 22.8 | 19.9, 19.7 | 21.2, 20.2 |
| Files indexed | 20,033 | 20,277 | 8,738 | 10,716 |
| Nodes | 163,060 | 167,759 | 113,460 | 151,302 |
| parse-loop | 7.6, 7.7 | 7.9, 8.0 | 4.1, 4.2 | 5.0, 4.9 |
| parse-index-rebuild + fts-rebuild | 1.5, 1.5 | 1.6, 1.6 | 1.3, 1.2 | 1.8, 1.8 |
| resolution | 10.3, 10.4 | 8.7, 8.2 | 12.4, 12.0 | 9.4, 9.3 |
| callback-synthesis | 1.9, 2.0 | 1.0, 1.0 | 2.0, 2.0 | 2.7, 2.2 |
| near-duplicates (fork only) | — | 1.2, 1.2 | — | 0.9, 0.9 |
| maintenance | 0.3, 0.9 | 1.5, 2.4 | 0.5, 0.4 | 1.6, 1.6 |

- **supabase:** the fork indexes 1,978 Markdown files that upstream skips.
  Every other language's file count matches exactly. The extra files and nodes
  account for the slower parse, index rebuild and FTS rebuild, and for the
  larger database.
- **discourse:** 244 more files (the repo has 258 Markdown files) and 3% more
  nodes. The visible fork-only cost is the near-duplicate pass.
- **resolution** is 19% faster on discourse and 23% faster on supabase.
  Callback synthesis is twice as fast on discourse but 0.5 s slower on
  supabase, probably because the extra Markdown nodes give the passes more to
  scan (not measured).
- **maintenance** runs the same code in both builds (`PRAGMA optimize` plus a
  passive checkpoint of a 0.5–0.6 GB WAL, on a worker thread), yet the fork
  was slower in all four runs. A separate supabase pair split the step into
  WAL-valve drain and pragmas and measured 0.30 s upstream and 0.50 s fork,
  much less than the profile's gap. See the follow-ups below.
- Phase times add up to more than the wall time because some phases overlap.

## Follow-ups

**Near-duplicate pass.** Profiled cold on the supabase index (signatures and
cached scores cleared, 5 runs per build): 0.81–0.92 s. MinHash signing and
shingling take about 400 ms of that, and scoring candidate pairs, which
re-reads their bodies, about 200 ms. Two changes were tried and dropped, each
with identical output:
- Swapping the signing loop (one hash at a time over a flat array) was slower,
  267 ms per run against 207 ms.
- Deciding exclusion once per file instead of per body saved at most 30 ms,
  inside run-to-run noise.

The pass is about 4% of a full index, so it stays as is.

**Maintenance step.** `PRAGMA optimize`'s analysis isn't the cause: a full
`ANALYZE` (with `analysis_limit=1000`) of each finished supabase database
took 3–60 ms. That leaves the final passive WAL checkpoint, which copies the
finished pages into the database file, and the fork's supabase database is 36%
larger. In the split probe, upstream's WAL valve was still folding pages when
maintenance began (113 ms drain) while the fork's had finished, so the two
builds split the same work differently between resolution and maintenance.
Confirming that would take instrumented runs of the valve, which isn't worth
it for about 1 s on a 20 s index. No change is planned.
