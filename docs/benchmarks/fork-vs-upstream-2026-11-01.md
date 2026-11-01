# Fork against upstream on Node, 2026-11-01, after the Markdown node-model work

A Node-only comparison at the fork head that lands the Markdown node-model work (#612 and #613). It supersedes [fork-vs-upstream-2026-10-31b.md](fork-vs-upstream-2026-10-31b.md) for every figure it covers. The runner changed from the hand-written `/usr/bin/time` loop to Hyperfine 2.0.0, so every arm was re-run here; figures are not directly comparable to the earlier run.

- Upstream: `cd04b21a` (`fix(python): a base's type argument is no supertype…`).
- Fork: `8313e9a0` (`fix(markdown): fold commands and table rows into their section (#613)`).
- Host: 24-core bare-metal Ubuntu, 122 GiB, Node.js 24.22.2, release kernel.
- Corpora: gin, Alamofire, pretix, CPython, discourse, supabase, n8n.
- Runner: Hyperfine 2.0.0 for whole commands. Index is one Hyperfine run per invocation and two invocations per arm (median of 2); one-file sync is two Hyperfine runs per invocation and two invocations per arm (median of 4). Arms run in mirrored order (upstream, fork, fork, upstream).
- Peak memory is Hyperfine's `memory_peak_resident` maximum. Graph counts are read from the fresh index after its timed run.
- A difference counts only when the fork/upstream gap exceeds the larger of the two arms' `(max − min) / median` for that metric. Spreads are listed per cell.
- The host benchmark lock was held for each corpus and a fresh 10-second admission check passed before it started. Receipts: `~/cg-scratch/bench-2026-11-01/admission-<corpus>.json`; raw Hyperfine JSON and `graph.tsv` beside them.

## Full index

| Corpus | Upstream | Fork | Fork / upstream | Verdict | Upstream RSS | Fork RSS | Nodes up/fork | Edges up/fork |
|---|---:|---:|---:|---|---:|---:|---:|---:|
| gin | 0.899 s | 0.893 s | 0.993 | no clear change (spreads 5.3% / 5.7%) | 484 MiB | 721 MiB | 2,660 / 2,985 | 8,050 / 8,442 |
| Alamofire | 1.238 s | 1.175 s | 0.949 | **5% faster** (0.7% / 0.7%) | 844 MiB | 1,133 MiB | 4,600 / 5,587 | 12,512 / 13,655 |
| pretix | 9.198 s | 9.079 s | 0.987 | **1% faster** (0.2% / 0.7%) | 3,574 MiB | 4,129 MiB | 32,781 / 32,795 | 79,317 / 73,765 |
| CPython | 46.357 s | 25.842 s | 0.557 | **1.79× faster** (0.0% / 0.1%) | 7,588 MiB | 6,051 MiB | 169,618 / 169,698 | 615,814 / 604,660 |
| discourse | 15.329 s | 14.128 s | 0.922 | **8% faster** (0.4% / 0.7%) | 4,398 MiB | 4,064 MiB | 163,411 / 166,865 | 314,691 / 309,103 |
| supabase | 15.732 s | 16.028 s | 1.019 | no clear change (0.4% / 2.1%), 18.5% more nodes | 5,891 MiB | 5,822 MiB | 114,440 / 135,658 | 292,021 / 311,496 |
| n8n | 71.748 s | 50.159 s | 0.699 | **1.43× faster** (0.4% / 0.4%) | 12,252 MiB | 8,754 MiB | 315,422 / 324,381 | 1,239,521 / 1,232,201 |

The fork indexes faster on five of seven corpora, by 1% (pretix) to 1.79× (CPython). Gin and supabase are no clear change: gin's 0.7% gap is far inside its 5.7% spread, and supabase's 1.9% is inside its 2.1% fork spread. Supabase is no longer an 11% loss; its graph is 18.5% larger in nodes and 6.7% larger in edges than upstream's. Peak-memory change, fork against upstream: gin +49%, Alamofire +34%, pretix +16%, CPython −20%, discourse −8%, supabase −1%, n8n −29%.

The supabase node count fell from 152,274 to 135,658 across #612 and #613; the import-node and command/table-row work removed 16,616 nodes and 16,641 edges. CPython's fork graph stays within 0.1% of upstream's node count.

## One-file sync

| Corpus | Upstream | Fork | Upstream / fork | Verdict |
|---|---:|---:|---:|---|
| gin | 0.395 s | 0.325 s | 1.22× | **18% faster** (2.7% / 3.4%) |
| Alamofire | 0.412 s | 0.295 s | 1.40× | **28% faster** (0.4% / 2.9%) |
| pretix | 1.513 s | 0.901 s | 1.68× | **40% faster** (1.3% / 2.7%) |
| CPython | 5.231 s | 2.596 s | 2.02× | **50% faster** (0.5% / 1.1%) |
| discourse | 1.070 s | 1.101 s | 0.97× | **3% slower** (1.8% / 1.5%) |
| supabase | 7.585 s | 3.450 s | 2.20× | **55% faster** (2.3% / 0.8%) |
| n8n | 11.481 s | 4.368 s | 2.63× | **62% faster** (0.4% / 1.4%) |

The fork syncs faster on six of seven corpora. Discourse is 3% slower, the one sync loss; its 2.9% gap clears the larger arm spread (1.8%). The other six clear their spreads too.

## Notes

- **Runner change.** This is the first campaign timed with Hyperfine 2.0.0 end to end. Every arm was re-run; the 2026-10-31 figures are not directly comparable and should not be mixed with these.
- **MCP.** The n8n MCP table was re-run on 2026-11-01 with the existing process-tree harness (`scripts/benchmarks/runtime/mcp-bench.mjs`), not Hyperfine: warm explore 142 ms against 244 ms, the eight-explore wave 1.89 s against 2.40 s, `initialize` 62 ms against 120 ms, and idle memory 2.08 GiB against 4.31 GiB. The watcher sync is 754 ms against 676 ms (12% slower). Keep this table separate from the Hyperfine index/sync figures, as the README does.
- **Raw evidence.** `~/cg-scratch/bench-2026-11-01/`: `*-index.json` / `*-sync.json`, `graph.tsv`, `*-index.txt` / `*-sync.txt`, and `admission-*.json`.
