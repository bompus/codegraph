# CodeGraph native-runtime measurement — 2026-09-07

Verdict: Bun is faster in these isolated indexing and retrieval checks, but uses more peak memory. Follow-up validation supports Bun serving with Node build/test harnesses. Native loading is working; Markdown uses its JavaScript extractor. Configuration adoption is recorded below; existing sessions are not assumed to have switched.

## Inputs and limits

Windows x64; Node 24.16.0 with `--liftoff-only`; Bun 1.4.2. Source/build `df01afe6267dbc40d3b6965ae5437a525092e366` remained unchanged throughout indexing. The deployed native addon was used by both arms. Telemetry, daemon use, watchdog and runtime relaunch were disabled in isolated CLI arms. Bun requires the existing unsafe-Node override and still emits an inaccurate V8 warning based on its compatibility version; this is a DX follow-up, not evidence Bun uses V8.

The repo corpus is a frozen copy of tracked working files, including existing uncommitted changes, excluding human-only `notes/`. It indexed 624 files. Vite is the archive at `8492422b8f110625a90c702f42f30784e8cf19dc`, indexing 1,717 files. Windows could not extract five archive symlinks; both arms used the same resulting corpus. These counts differ from older benchmarks and must not be compared as equivalent workloads.

Eight paired full-index rounds alternated Node/Bun then Bun/Node. Each runtime ran first four times. Runs used `index --force` over the same initialized database, not a newly created database each time. These are warm filesystem/index rebuild measurements with process startup/shutdown included. Initial Node initialization was recorded separately; Bun's subsequent init was a no-op and is **not** a cold-init comparison. Other desktop work, including an isolated refresh trial, was active during part of the run. No controlled machine-idle or statistically significant claim is made.

## Results

| Measure                                                 | Node                | Bun                 |
| ------------------------------------------------------- | ------------------- | ------------------- |
| Repo full index, median (range), ms                     | 3,999 (3,828–4,549) | 3,334 (2,923–3,990) |
| Vite full index, median (range), ms                     | 4,528 (4,312–6,728) | 3,390 (3,079–3,563) |
| First explore call, median (range), ms                  | 863 (838–898)       | 735 (706–768)       |
| Subsequent explore calls, pooled median (range), ms     | 285 (218–358)       | 238 (185–305)       |
| MCP idle working set, MiB                               | 75–76               | 60–61               |
| MCP peak after nine queries, MiB                        | 281–368             | 402–471             |
| Native-enabled index peak, two reversed samples, MiB    | 790, 768            | 862, 885            |
| Forced-WASM index peak, two reversed samples, MiB       | 735, 744            | 1,234, 1,170        |
| One-worker native index peak, two reversed samples, MiB | 738, 754            | 782, 771            |
| Synthetic session CLI, median (range), ms               | 121 (108–153)       | 55 (53–58)          |

Index timing samples in execution order, milliseconds:

- Repo Node: 3828, 4086, 4549, 4354, 4225, 3856, 3912, 3829.
- Repo Bun: 3327, 3009, 3963, 3423, 3990, 3234, 3341, 2923.
- Vite Node: 4533, 4453, 4523, 4465, 6728, 4704, 4686, 4312.
- Vite Bun: 3276, 3079, 3544, 3379, 3563, 3401, 3182, 3505.

Memory runs were separate from timing runs. Windows process peak working set was sampled every 50 ms for indexing; this includes threads, not arbitrary child processes. The CLI arms disabled relaunch/watchdog. This is not a complete process-tree measurement. MCP memory uses the existing `cg-probe.ts` sampler. Bun's process-resource API returned no usable metrics here. One-worker samples establish only a modest memory reduction; they do not establish a throughput recommendation.

## Correctness and extraction

All 32 full-index runs exited successfully. Sorted node, edge and literal records matched across all native-enabled runtime runs on each corpus, excluding node update timestamps and generated edge IDs. Repo: 14,507 nodes, 37,096 edges, 4,354 literals. Vite: 13,791 nodes, 30,143 edges, 3,842 literals. Counts alone were not the parity check. Unresolved-reference tables and forced-WASM graph equivalence were not exhaustively compared.

Existing `cg-probe.ts` ran nine Markdown/blast-radius queries in eight fresh MCP processes per runtime: 144 calls. All 63 paired payloads in rounds 1–7 were byte-identical. Round 0 had six Markdown footer differences because the incremental fixture added one file between arms; answering content and probe needles were unchanged. The other three payloads matched. Existing needle misses occurred equally in both arms; this is runtime parity, not perfect retrieval or agent-level correctness.

Both runtimes returned identical hits for an isolated two-document session fixture. First initialization and warm queries were mixed in the CLI aggregate, so it is a compatibility/small-fixture timing check, not a real-history scalability result. Incremental checks covered four changed-file and four no-op syncs per runtime; all passed. They alternate first writer, so changed and no-op timings must not be pooled as equivalent work. Probe processes were terminated by the existing harness; this does not prove graceful MCP shutdown or daemon lifecycle compatibility.

A single-process diagnostic pass over 625 files (including the incremental fixture) found 463 successful native extractions: 289 TypeScript, 173 JavaScript and one Python; zero native deferrals or extraction errors. The 138 Markdown files used the custom extractor; nine YAML files were file-level handling. Fifteen Vue files used their custom wrapper, whose script blocks instantiate `TreeSitterExtractor` directly and therefore still use WASM.

Markdown headings and links also extracted successfully with WASM compile/instantiate functions disabled. The native TypeScript smoke passed in that same process. Rust `markdown.rs` handles documentation paths in code, not Markdown document parsing.

The diagnostic pass spent roughly 811 ms in Node extraction and 778 ms in Bun extraction, including approximately 86/72 ms on Markdown and 64/56 ms on Vue. This is one diagnostic pass, not the production parallel pipeline. It does not separately quantify resolution, database writes or rendering. Worker initialization explicitly loads supplied WASM grammars, even though many files subsequently use the native kernel; actual WASM use is therefore broader than native deferral counts.

## Bun adoption follow-up

Fresh project initialization passed on two fresh copies per runtime, with reversed order: Node 4,103/4,135 ms, Bun 2,982/3,072 ms. These were fresh databases with warm OS caches, not machine-cold trials.

A frozen project transcript snapshot contained 268 JSONL files, 406,025,066 bytes and 11,118 indexed prose documents. The existing Claude reader selected the snapshot and excluded its memory directory. Two fresh session databases per runtime took 3,431/3,795 ms on Node versus 2,265/2,369 ms on Bun for initial refresh plus four queries. Eight subsequent refresh/query cycles per process returned unchanged results. All top-ten hits for `codegraph native`, `markdown`, `Bun` and `turn readiness dedupe` matched across both runtimes. Four concurrent initializers against a new shared database also passed per runtime. All sixteen initialization/session child processes exited zero. Raw results and local frozen inputs are in `.scratch/cg-adoption/`; transcript contents are not published in this report.

The coordinated routers task reports **16/16 existing daemon/roots/initialize tests passing with a Node harness launching verified Bun MCP executables**, including stdin EOF, proxy/daemon exit and pidfile removal, daemon sharing, concurrent launch, failover and recovery. Bun stdin/startup/parent-watchdog unit tests pass **29/29**. The Bun-hosted test runner retains a repeatable EBUSY fixture-cleanup failure (15/16 original suite; 18/20 refresh prototype), while the targeted Node-harness/Bun-server control passes. This isolates a test-host resource issue rather than a demonstrated deployed-server leak. No prototype processes remained. These lifecycle results were supplied by the owning task, not rerun here. A scoped cleanup check in this task was rejected by execution policy before running; disposable corpora remain and deletion behavior is unverified here.

Bun issue [#41438](https://github.com/oven-sh/bun/issues/41438) remains open. Fix PR [#41449](https://github.com/oven-sh/bun/pull/41449) is open; [#41452](https://github.com/oven-sh/bun/pull/41452) was closed without merging. Do not assume the installed Bun includes the fix or adopt unstable JSC tuning flags as a production solution.

## Decisions and preserved follow-ups

- **CG-02 retain existing guards:** remote consolidated, local source, build and daemon checks agreed; upstream `b9ca4b7` is included. The hourly task's checked run succeeded. Existing staging builds native artifacts and gates native/WASM tests and retrieval probes. No demonstrated stale deployment required a change. Existing client refresh remains separately owned.
- **CG-03/04 Bun serving activated:** after the user's restart, this task verified Bun MCP client processes and daemon PID 22552, successful live code/Markdown/session queries, and matching canonical/source/build/running revision df01afe6. The hourly task's 14:55 run returned zero; remote upstream main remains b9ca4b7 and is included. Node remains the build/test harness. The routers task reports all four host configs aligned, Bun promotion probes, 28 updater tests, `check:vp` and independent final review passing. It now owns the separately approved Codex-first automatic-refresh rollout; another task audits other hosts. No additional runtime edits were made here. Full-suite/native-WASM parity is distinct from this scoped evidence.
- **CG-05 next measured targets:** investigate eager grammar allocation and Vue's WASM script path before proposing native Markdown. Preserve error-recovery fallback. Profile phase costs before changing storage. These are follow-ups, not changes implemented here.

Reproduction helpers, raw stdout, per-query payloads and JSON samples are retained locally under `.scratch/cg-priority1/`: `bench.mjs`, `followup.mjs`, `memory.ps1`, `profile.cjs`, `workers.mjs`, `results.json`, `followup.json`, and `workers.json`. This ignored evidence directory includes frozen corpora; it is not part of the deployment. The persistent backlog is [CodeGraph experiments](PLAN-codegraph-experiments.md).
