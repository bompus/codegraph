# Fork vs upstream, and Node 24 vs Node 26 vs Bun (2026-09-26)

Paired measurements behind the README's "Measured results", "What it costs"
and "Runtimes" sections. This replaces an earlier run the same day on five
corpora (fork build `0202084e`). This run adds supabase (React and Next.js) and
n8n (Vue and TypeScript) and re-measures every corpus on one newer fork build.

## Method

- **Builds.** Upstream `main` at `ba3c21e5` (2026-09-16, newer than its last
  release, 1.6.0) with its own native kernel built from source. Fork
  `fork/consolidated` at `76cdf8d4` (2026-09-26). Both built with `npm run build`
  in their own worktrees.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 47 GiB RAM, ext4. No swap in use. Each batch
  started with a 1-minute load average under 2 and Windows under 5% CPU.
- **Commands.** Every run was capped with
  `nice -n 10 ~/.local/bin/run-capped 16G` and timed with `/usr/bin/time -v`,
  with `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_NO_DAEMON=1` and
  `CODEGRAPH_TELEMETRY=0`. The index directory started empty for every run.
  - Index: `codegraph init --yes .`
  - One-file sync: an untimed `init`, then a one-line edit to one source file,
    then a timed `codegraph sync .`.
- **Pairing.** Arms alternate upstream, fork, fork, upstream (runtimes:
  24, 26, Bun, Bun, 26, 24). Tables give the median of two runs. Peak memory is
  MaxRSS. Database size is `codegraph.db` after a WAL checkpoint.
- **Corpora**, each at a pinned commit:
  - gin (Go) `1bd0ecf`
  - Alamofire (Swift) `bda9ed5`
  - pretix (Python and JavaScript) `aa14505`
  - CPython (C and Python) `1e8ff18`
  - discourse (Ruby and JavaScript) `ae5ba2c4`
  - supabase (React, Next.js and TypeScript) `36371de1`
  - n8n (Vue and TypeScript) `7cc0450a`

  Shallow clones, no dependencies installed.
- **Discarded runs.** Runs that a live draft room on the host refused or
  contended were thrown away and re-run. The first n8n runs came from an
  incomplete clone (about 7.5% of files missing) and were discarded; every n8n
  number below comes from a verified complete checkout. n8n was measured while a
  draft room was active, and its pairs vary by about 10% (upstream 99.8 and
  111.7 s, fork 103.8 and 92.8 s).

## Full index: fork vs upstream (Node 24)

| Corpus | Files (up / fork) | Time (up → fork) | Peak memory | Database | Edges |
|---|---|---|---|---|---|
| gin | 110 / 119 | 0.87 → 0.79 s (−10%) | 492 → 293 MiB (−41%) | 7.8 → 9.5 MiB (+22%) | 8,139 → 8,722 (+7%) |
| Alamofire | 117 / 129 | 1.41 → 1.31 s (−7%) | 617 → 480 MiB (−22%) | 16.9 → 19.3 MiB (+14%) | 13,866 → 15,194 (+10%) |
| pretix | 1,467 / 1,473 | 12.3 → 10.4 s (−16%) | 2.65 → 2.26 GiB (−15%) | 117 → 141 MiB (+21%) | 107,118 → 93,230 (−13%) |
| CPython | 3,667 / 3,710 | 33.4 → 28.0 s (−16%) | 4.20 → 3.58 GiB (−15%) | 451 → 542 MiB (+20%) | 648,378 → 610,100 (−6%) |
| discourse | 20,034 / 20,278 | 24.1 → 25.3 s (+5%) | 3.46 → 2.98 GiB (−14%) | 370 → 424 MiB (+15%) | 324,764 → 314,193 (−3%) |
| supabase | 8,740 / 10,718 | 80.5 → 24.1 s (−70%) | 5.02 → 4.07 GiB (−19%) | 327 → 459 MiB (+40%) | 287,934 → 313,771 (+9%) |
| n8n | 23,734 / 24,435 | 105.7 → 98.3 s (−7%) | 8.17 → 4.92 GiB (−40%) | 1,460 → 1,739 MiB (+19%) | 1,399,819 → 1,211,201 (−13%) |

The fork indexes more files because it reads Markdown (supabase: 1,978 `.md`
files; its code files are identical in both arms). Its edge count is higher
where it adds links (Markdown sections, bindings) and lower where it declines
uncertain ones.

The pretix fork runs varied (11.6 and 9.1 s).

### Why supabase is 3.3× faster

Parsing is not the difference. Both builds' parse loops are dominated by
writing results to SQLite:

| Build | Parse loop | Store step |
|---|---|---|
| Upstream | 62.0 s | 60.7 s |
| Fork | 4.9 s | 3.7 s |

The fork's first-index store path skips per-row foreign-key probes and
secondary-index maintenance ([metrics ledger §5.12](../design/metrics-ledger.md)).
Resolution takes about the same time in both (12.4 s and 12.9 s).

Upstream also fails to parse TypeScript 5's `export type * from '…'`. Four
supabase files are indexed with no symbols as a result; the fork parses them.

### What the n8n edge drop is

Nearly all of it is `calls` edges: 681.5k → 492.7k. Among the 182k call edges
upstream holds and the fork does not, most are name-only matches the fork
declines on purpose:

| Call | Upstream links it to | Edges |
|---|---|---|
| `it(…)` | a TypeORM test helper's `XFailFunction::it` | 76,409 |
| `expect(…)` | a Playwright fixture constant | 29,331 |
| `Object.assign` | TypeORM `ObjectUtils::assign` | 2,164 |
| `path.join` | TypeORM `SelectQueryBuilder::join` | 1,160 |
| Vue `computed` / `ref` | an ESLint rule property / a test mock | ~4,000 |

It also loses real links. Member calls on an imported, exported instance
(`export const Container = new ContainerClass()`) are declined when upstream
gets them right. On n8n that is `Container.get` (5,276 edges) and
`i18n.baseText` (1,298). This is the TypeScript member-call recall gap the
README lists as the fork's largest.

## One-file sync: fork vs upstream (Node 24)

Re-measured later the same day on an idle host (no other session's capped jobs
running, 1-minute load 1.5–4 from the benchmark's own workers), with the fork at
`perf/incremental-sync` (the sync skip cache, [`docs/design/incremental-synthesis.md`](../design/incremental-synthesis.md)).
Each arm indexes once, then times two one-file syncs, each adding a line to the
same file; arms alternate upstream, fork, fork, upstream. The table gives the
median of the four syncs per arm, capped at 13G.

| Corpus | Edited file | Upstream | Fork |
|---|---|---|---|
| gin | `gin.go` | 0.38 s, 172 MiB | 0.39 s, 212 MiB |
| Alamofire | `Source/Core/Session.swift` | 0.51 s, 210 MiB | 0.51 s, 223 MiB |
| pretix | `src/pretix/base/models/orders.py` | 1.11 s, 441 MiB | 2.27 s, 578 MiB |
| CPython | `Lib/os.py` | 1.44 s, 509 MiB | 4.37 s, 866 MiB |
| discourse | `app/models/user.rb` | 5.57 s, 936 MiB | 4.68 s, 970 MiB |
| supabase | `apps/studio/data/projects/clone-mutation.ts` | 1.56 s, 332 MiB | 4.63 s, 929 MiB |
| n8n | `packages/cli/src/active-executions.ts` | 12.1 s, 1,414 MiB | 9.4 s, 1,344 MiB |

A second round of sync work (`perf/sync-detect`, 2026-09-27) was measured the
same way against the fork at `013f814a`, which already had the table above's
skip cache. A third round (below) supersedes its right-hand figures in the
README; the README's upstream column is the table above.

| Corpus | Fork `013f814a` | Fork `perf/sync-detect` |
|---|---|---|
| gin | 0.39 s, 216 MiB | 0.39 s, 217 MiB |
| Alamofire | 0.53 s, 225 MiB | 0.54 s, 226 MiB |
| pretix | 2.34 s, 579 MiB | 1.85 s, 580 MiB |
| CPython | 4.41 s, 903 MiB | 4.62 s, 902 MiB |
| discourse | 5.04 s, 979 MiB | 4.04 s, 945 MiB |
| supabase | 4.72 s, 930 MiB | 3.56 s, 971 MiB |
| n8n | 9.54 s, 1,386 MiB | 6.95 s, 1,261 MiB |

Other sessions raised the 1-minute load to 4–7 during the discourse and n8n
arms. The alternating order spreads that over both arms, and one outlier on
each (a 7.09 s base sync on discourse, an 8.35 s branch sync on n8n) falls
outside the median. The CPython difference is within run-to-run spread: none
of the changes reach Python-only work.

A third round (`perf/sync-node-reads`, 2026-09-27) filters the synthesis passes'
node scans in SQL. Measured the same way against the fork at `41e7f7df`, which
had the second round. The README's fork column uses the right-hand figures.

| Corpus | Fork `41e7f7df` | Fork `perf/sync-node-reads` |
|---|---|---|
| gin | 0.39 s, 217 MiB | 0.37 s, 211 MiB |
| Alamofire | 0.48 s, 227 MiB | 0.48 s, 228 MiB |
| pretix | 1.77 s, 578 MiB | 1.69 s, 586 MiB |
| CPython | 4.50 s, 881 MiB | 3.65 s, 849 MiB |
| discourse | 4.02 s, 968 MiB | 3.56 s, 932 MiB |
| supabase | 3.88 s, 973 MiB | 3.48 s, 977 MiB |
| n8n | 6.66 s, 1,230 MiB | 6.24 s, 1,236 MiB |

A live draft room paused the run after CPython; the last three corpora ran
once it ended, at a 1-minute load of 2–4 from other sessions. Supabase's base
syncs spread from 3.55 s to 4.37 s, so its difference is mostly noise. The
base column also sits below the second round's right-hand figures (pretix 1.85
s, n8n 6.95 s) on the same code, which is the run-to-run spread between days.

The first measurement, taken during the full-index batch with fork `76cdf8d4`,
read higher for the fork (pretix 2.77 s, CPython 5.91 s, supabase 6.68 s, n8n
14.2 s). Part of that gap is the skip cache and part is host load at the time;
the paired before/after on the idle host is in the design doc.

Upstream's sync does not rebuild links inferred from dynamic dispatch (events,
callbacks, function pointers), so they go stale until a full index
(colbymchenry/codegraph#1988). The fork rebuilds them and refreshes
near-duplicates on every sync. `CODEGRAPH_SYNC_RESYNTHESIS=0` turns that off.

## Runtimes (fork, full index)

| Corpus | Node 24.21.0 | Node 26.10.0 | Bun 1.4.2 |
|---|---|---|---|
| pretix | 8.63 s, 2.25 GiB | 9.45 s, 1.98 GiB | 7.58 s, 1.33 GiB |
| CPython | 28.7 s, 3.48 GiB | 28.7 s, 3.08 GiB | 27.9 s, 2.66 GiB |
| discourse | 24.1 s, 3.05 GiB | 24.6 s, 2.69 GiB | 24.5 s, 2.38 GiB |
| supabase | 25.5 s, 4.06 GiB | 24.5 s, 3.44 GiB | 24.3 s, 3.31 GiB |
| n8n | 91.7 s, 4.93 GiB | 92.7 s, 4.34 GiB | 89.6 s, 3.93 GiB |

Node and edge counts are identical across the three runtimes for every corpus.

Full suite on `fork/consolidated` at `d6172b74`:
- Node 26.10.0: 334 files, 5,557 passed, 12 skipped. Same as Node 24.
- Bun 1.4.2 (`bun --bun x vitest run`): 334 files, 5,556 passed, 13 skipped.
  Bun skips one more test, a value-reference case gated on oven-sh/bun#42891.

An earlier profile of Bun on CPython with an older build found `node:sqlite`
`StatementSync.get()` about 1.5× slower under Bun than under Node when it
returns table columns. That is filed as oven-sh/bun#44084.
