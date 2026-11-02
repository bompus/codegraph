# Node and Bun on the reconciled fork, 2026-11-02

A runtime re-measure at `671171d4` (the `f72c9d2e` reconcile plus the batch-3a follow-ups), prompted by Bun 1.4.3 → 1.4.4. Both arms run the same build; this is not an upstream comparison and does not supersede the [2026-11-01 Node run](fork-vs-upstream-2026-11-01.md).

- Build: `671171d4` (`fix(resolution): three review follow-ups on the batch-3a ports (#601)`), `dist` and the native kernel built from that tree.
- Runtimes: Node.js 24.22.2 and Bun 1.4.4.
- Host: 24-core bare-metal Ubuntu, 122 GiB, release kernel.
- Corpora: gin, Alamofire, pretix, CPython, discourse, supabase, n8n, at the same pins as 2026-11-01.
- Runner: Hyperfine 2.0.0. Arms in mirrored order (`node`, `bun`, `bun`, `node`), two invocations per arm; index is the median of 2 runs and one-file sync the median of 4. The 2026-11-01 rule applies: a cell is a change only when the gap exceeds the larger arm's `(max − min) / median`, otherwise it is marked no clear change.
- Admission: one 10-second host check before the campaign under the benchmark lock; the receipt is `admission.json` in the raw directory.
- MCP: gin and n8n, process-tree harness, mirrored arms.
- CLI: gin, Hyperfine, 20 runs per command per arm.
- Raw evidence: `~/cg-scratch/bench-bun-20261102/` (`*-index.json`, `*-sync.json`, `mcp.jsonl`, `cli-*.json`, `admission.json`). Bun 1.4.3 was not measured: the user chose the latest versions, and the documented old binary was absent.

## Full index

| Corpus | Node | Bun | Bun / Node | Verdict | Node RSS | Bun RSS | Spread Node / Bun |
|---|---:|---:|---:|---|---:|---:|---:|
| gin | 0.908 s | 0.811 s | 0.894 | no clear change (10.6% gap, 16.3% Bun spread) | 679 MiB | 591 MiB | 8.3% / 16.3% |
| Alamofire | 1.220 s | 0.972 s | 0.797 | **20% faster** | 1,055 MiB | 909 MiB | 0.6% / 0.3% |
| pretix | 8.133 s | 8.005 s | 0.984 | no clear change | 4,071 MiB | 3,969 MiB | 0.1% / 1.1% |
| CPython | 26.045 s | 25.653 s | 0.985 | no clear change | 5,546 MiB | 5,953 MiB | 0.8% / 1.6% |
| discourse | 14.023 s | 12.542 s | 0.894 | **11% faster** | 3,227 MiB | 3,680 MiB | 0.2% / 0.0% |
| supabase | 15.666 s | 14.499 s | 0.926 | **7% faster** | 4,498 MiB | 5,290 MiB | 0.4% / 0.8% |
| n8n | 50.401 s | 47.855 s | 0.949 | **5% faster** | 8,158 MiB | 9,743 MiB | 0.9% / 0.4% |

Bun indexes faster on four of seven corpora where the gap clears the spread, by 5% (n8n) to 20% (Alamofire). gin's 10.6% gap is inside its 16.3% Bun spread; pretix and CPython are near ties. Peak RSS is lower on gin, Alamofire and pretix and higher on the larger corpora.

## One-file sync

| Corpus | Node | Bun | Reciprocal speed | Less time | Spread Node / Bun | Node RSS | Bun RSS |
|---|---:|---:|---:|---:|---:|---:|---:|
| gin | 0.325 s | 0.224 s | 1.45× | 31% | 2.1% / 4.2% | 170 MiB | 159 MiB |
| Alamofire | 0.307 s | 0.225 s | 1.37× | 27% | 4.7% / 1.6% | 184 MiB | 165 MiB |
| pretix | 0.861 s | 0.640 s | 1.35× | 26% | 1.2% / 1.7% | 381 MiB | 367 MiB |
| CPython | 2.331 s | 1.761 s | 1.32× | 24% | 0.3% / 1.2% | 1,022 MiB | 944 MiB |
| discourse | 1.592 s | 1.217 s | 1.31× | 24% | 2.2% / 0.7% | 632 MiB | 618 MiB |
| supabase | 3.269 s | 2.699 s | 1.21× | 17% | 2.9% / 3.7% | 911 MiB | 1,218 MiB |
| n8n | 4.378 s | 3.556 s | 1.23× | 19% | 0.8% / 0.7% | 1,183 MiB | 1,369 MiB |

Every corpus's one-file sync is faster on Bun, by 17% (supabase) to 31% (gin).

## MCP

| Corpus | Arm | Initialize | First answer | Warm explore (median / p90) | Eight at once | Watcher sync | Idle memory |
|---|---|---:|---:|---:|---:|---:|---:|
| gin | Node | 54 ms | 299 ms | 12 / 14 ms | 84 ms | 357 ms | 774 MB |
| gin | Bun | 41 ms | 158 ms | 10 / 11 ms | 144 ms | 332 ms | 480 MB |
| n8n | Node | 56 ms | 2,734 ms | 134 / 176 ms | 1,838 ms | 753 ms | 2,269 MB |
| n8n | Bun | 44 ms | 2,240 ms | 114 / 163 ms | 1,576 ms | 562 ms | 914 MB |

Idle memory is 38% lower on gin (774 → 480 MB) and 60% lower on n8n (2,269 → 914 MB). The eight-explore wave is 71% slower on gin at these millisecond scales and 14% faster on n8n.

## CLI startup

On gin, Hyperfine, mean of 20 runs.

| Command | Node | Bun | Less time |
|---|---:|---:|---:|
| `codegraph --version` | 37.2 ms | 22.9 ms | 38% |
| `codegraph status` | 118.7 ms | 77.2 ms | 35% |
| `codegraph explore "<query>"` | 113.7 ms | 71.7 ms | 37% |

## Against the previous Bun figures

The 2026-10-28 campaign reported Bun indexing 4–15% faster on six of seven corpora (CPython a tie), sync 26–37% faster, and MCP idle memory 29–30% lower. This run: index faster on four of seven by 5–20%, sync 17–31% faster, and idle memory 38% lower on gin and 60% lower on n8n. The build changed substantially between the two campaigns (the `f72c9d2e` reconcile), so these figures are the current truth for `671171d4` rather than a clean measurement of the runtime upgrade alone.
