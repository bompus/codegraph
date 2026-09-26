# Fork vs upstream, and Node 24 vs Node 26 vs Bun (2026-09-26)

Paired measurements behind the README's "Measured results", "What it costs"
and "Runtimes" sections.

## Method

- **Builds.** Upstream `main` at `ba3c21e5` (2026-09-16, newer than its last
  release, 1.6.0) with its own native kernel built from source. Fork
  `fork/consolidated` at `0202084e` (2026-09-26). Both built with `npm run build`.
- **Host.** WSL2 Ubuntu, 16 vCPUs, 47 GiB RAM, ext4. No swap in use, and
  Windows under ~13% CPU at the start of each batch.
- **Commands.** Every run was capped with
  `nice -n 10 ~/.local/bin/run-capped 16G` and timed with `/usr/bin/time -v`,
  with `CODEGRAPH_DIR=.cg-bench`, `CODEGRAPH_NO_DAEMON=1` and
  `CODEGRAPH_TELEMETRY=0`. The index directory started empty for every run.
  - Index: `codegraph init --yes .`
  - One-file sync: an untimed `init`, then a one-line edit to one source file,
    then a timed `codegraph sync .`.
- **Pairing.** Arms alternate upstream, fork, fork, upstream (runtimes:
  24, 26, Bun, Bun, 26, 24). Tables give the median of two runs. Peak memory is
  MaxRSS, in MiB (GiB where marked). Database size is `codegraph.db` after a WAL
  checkpoint.
- **Corpora.** gin (Go), Alamofire (Swift), pretix (Python and JavaScript),
  CPython (C and Python), discourse (Ruby and JavaScript), each at a pinned commit
  (gin `1bd0ecf`, Alamofire `bda9ed5`, pretix `aa14505`, CPython `1e8ff18`,
  discourse `ae5ba2c4`).

## Full index: fork vs upstream (Node 24)

| Corpus | Files (up / fork) | Time (up → fork) | Peak memory | Database | Edges |
|---|---|---|---|---|---|
| gin | 110 / 119 | 0.90 → 0.77 s (−14%) | 478 → 261 MiB (−45%) | 7.8 → 9.5 MiB (+22%) | 8,139 → 8,722 (+7%) |
| Alamofire | 117 / 129 | 1.40 → 1.28 s (−9%) | 602 → 483 MiB (−20%) | 16.9 → 19.3 MiB (+14%) | 13,866 → 15,194 (+10%) |
| pretix | 1,467 / 1,473 | 11.6 → 8.5 s (−26%) | 2,718 → 2,244 MiB (−17%) | 117 → 141 MiB (+21%) | 107,118 → 93,230 (−13%) |
| CPython | 3,667 / 3,710 | 33.4 → 27.4 s (−18%) | 4,345 → 3,717 MiB (−14%) | 451 → 542 MiB (+20%) | 648,378 → 610,100 (−6%) |
| discourse | 20,034 / 20,278 | 22.5 → 24.8 s (+11%) | 3,541 → 2,936 MiB (−17%) | 370 → 424 MiB (+15%) | 324,764 → 314,193 (−3%) |

The fork indexes more files because it reads Markdown. Its edge count is higher
where it adds links (Markdown sections, bindings) and lower where it declines
uncertain ones (pretix, CPython, discourse).

## One-file sync: fork vs upstream (Node 24)

| Corpus | Edited file | Upstream | Fork | Fork, `CODEGRAPH_SYNC_RESYNTHESIS=0` |
|---|---|---|---|---|
| gin | `gin.go` | 0.36 s, 174 MiB | 0.38 s, 201 MiB | — |
| Alamofire | `Source/Core/Session.swift` | 0.48 s, 211 MiB | 0.53 s, 225 MiB | — |
| pretix | `src/pretix/base/models/orders.py` | 1.14 s, 445 MiB | 2.66 s, 588 MiB | 1.08 s |
| CPython | `Lib/json/encoder.py` | 0.68 s, 293 MiB | 4.31 s, 928 MiB | 0.79 / 1.46 s |
| discourse | `app/models/about.rb` | 2.72 s, 499 MiB | 4.70 s, 927 MiB | — |

Upstream's sync does not rebuild links inferred from dynamic dispatch (events,
callbacks, function pointers), so they go stale until a full index
(colbymchenry/codegraph#1988). The fork rebuilds them and refreshes
near-duplicates on every sync. With the rebuild off, its sync takes as long as
upstream's.

## Runtimes (fork, full index)

| Corpus | Node 24.21.0 | Node 26.10.0 | Bun 1.4.2 |
|---|---|---|---|
| pretix | 8.25 s, 2.29 GiB | 8.72 s, 1.97 GiB | 7.50 s, 1.28 GiB |
| CPython | 25.07 s, 3.54 GiB | 26.23 s, 3.35 GiB | 26.89 s, 2.87 GiB |
| discourse | 22.42 s, 2.91 GiB | 20.87 s, 2.61 GiB | 21.16 s, 2.29 GiB |

Node and edge counts are identical across the three runtimes for every corpus.

Full suite:
- Node 26.10.0: 319 files, 5,193 passed, 12 skipped. Same as Node 24.
- Bun 1.4.2 (`bun --bun x vitest run`): 319 files, 5,192 passed, 13 skipped.
  Compared with Node, Bun skips one more test: a value-reference case gated on
  oven-sh/bun#42891. This change also removes a stale Bun skip from
  `kernel-live-conn-locks.test.ts`, whose comment claimed Bun had no
  `node:sqlite`; the test passes under Bun.
