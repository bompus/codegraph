# Runtime benchmark

These scripts produced the README's "Measured results" and the tables in
[`docs/benchmarks/fork-vs-upstream-node-bun-2026-10-14.md`](../../../docs/benchmarks/fork-vs-upstream-node-bun-2026-10-14.md).
Each arm is a runtime (a `node` or `bun` executable) running a built checkout.
The arms can differ in build, in runtime, or in both.

| Script | Measures | Output in `$BENCH_OUT` |
|---|---|---|
| `run-index-sync.sh <list>` | Full index, then two one-file syncs: wall time, peak RSS, CPU, database size, node and edge counts | `index-sync.tsv`, `logs/` |
| `run-mcp.sh <list>` | MCP server: start to first explore answer, warm explore median and p90, 8 explores at once, watcher sync, idle memory and CPU | `mcp.jsonl` |
| `run-cli.sh <corpus> <query>` | CLI start: `--version`, `status`, one `explore` (hyperfine, 20 runs) | `cli-*.json`, `cli.txt` |
| `sync-ab.mjs --corpus … --arm base=… --arm cand=…` | One change on one corpus: a one-file sync before and after, with same-build controls, until the result qualifies or stays inside run-to-run variation (see below) | `runs.jsonl`, `summary.md`, `summary.json`, `chart.json`, `logs/` in `--out` |
| `summarize.py $BENCH_OUT` | Medians per corpus and arm from `index-sync.tsv`, and from `mcp.jsonl` when present | stdout |
| `render-charts.mjs $BENCH_OUT --date D --upstream SHA --fork SHA --node V --bun V` | The README chart (`--cards`), the benchmark doc's heatmap (`--heatmap`) and the current run's result tables as Markdown (`--tables`; the benchmark doc's comparison with an earlier run is written by hand), from the accepted attempts in `$BENCH_OUT` (directories `index-<corpus>-aN`, `mcp-<corpus>-aN` and `cli-gin-aN` whose `status` file reads `accepted`) | SVG and Markdown files |

Every run needs a quiet host. Each runner waits before a corpus until no
`capped-*` scope runs and the 1-minute load is under 2, and each arm runs twice
per corpus, forward and then reversed.

## Run time

The 2026-10-03 run behind the README took 1 h 47 min (17:09 to 18:56):
`run-index-sync.sh` on the seven corpora in the benchmark doc took 59 min, then
`run-mcp.sh` on gin and n8n and `run-cli.sh` on gin took 48 min, with four arms
(upstream and fork, each on Node and Bun). That includes the idle-host waits.
The n8n MCP runs took about 4 min each. Building the two worktrees took 4 min
before it. The 2026-09-28 run of the same steps took 1 h 26 min. The
2026-10-12 run took 3 h 2 min (00:16 to 03:18), most of it waiting for the host
to go quiet and re-running attempts it rejected. Its measuring time per corpus
was about 5 min for discourse and supabase, 18 min for n8n index and sync, and
26 min for the n8n MCP run. The 2026-10-13 run took 1 h 26 min (13:49 to 15:15; index and sync 40 min, MCP 43 min, CLI under 1 min), with the host quiet enough that no attempt was rejected. The 2026-10-14 run took 1 h 23 min (17:24 to 18:47; index and sync 38 min, MCP 42 min, CLI under 1 min) with no attempt rejected. Start a re-run estimate from these numbers, and
update them here after a run that takes much longer or shorter.

## Arms

```bash
export BENCH_OUT=$HOME/cg-scratch/bench-$(date +%F)
export BENCH_ARMS="node=$(fnm exec --using codegraph which node)@$PWD bun=$(command -v bun)@$PWD"
export BENCH_WRAP="nice -n 10 systemd-run --user --scope -p MemoryMax=16G --"
```

`BENCH_ARMS` entries are `name=RUNTIME@BUILD`. `BENCH_WRAP` prefixes every
timed command; it defaults to `nice -n 10`. `run-mcp.sh` runs `mcp-bench.mjs`
with `$BENCH_NODE` (default `node`, 22.13 or newer).

## Corpus lists

One corpus per line; `#` starts a comment. `run-index-sync.sh` takes
`<corpus-path> <file-to-edit> <comment-prefix>`, and `run-mcp.sh` takes
`<corpus-path> <file-to-edit> <queries.json>`. The file to edit must be
tracked, because each run restores it with `git checkout`.

```text
/path/to/gin gin.go //
/path/to/cpython Lib/os.py #
```

`queries/` holds the explore questions used for gin and n8n.

## One-file sync comparison

`sync-ab.mjs` answers "does this change move a one-file sync's peak memory, wall
time or CPU time on this corpus?" in one command, for a pull request's numbers.
It needs a saved index of the corpus (a `.codegraph` directory made with
`codegraph init` and copied aside) and two built checkouts or two runtimes.

```bash
node scripts/benchmarks/runtime/sync-ab.mjs \
  --corpus ~/cg-scratch/n8n --file packages/workflow/src/utils.ts \
  --append '\nexport const benchProbeMarker = 1;\n' --index ~/cg-scratch/n8n-index \
  --arm base=$(fnm exec --using codegraph which node)@$HOME/worktrees/base \
  --arm cand=$(fnm exec --using codegraph which node)@$PWD \
  --out ~/cg-scratch/ab-$(date +%F-%H%M) --save-noise ~/cg-scratch/noise.json
```

Each timed run restores the saved index into `--cgdir` (default `.cg-ab`) and
checks it byte for byte. It appends `--append` to `--file`, runs `sync` under
`/usr/bin/time -v`, and restores the file. Then it hashes the edges and
near-duplicate tables. The order of the two builds alternates within each
session. A session has `--pairs` before/after pairs (default 3) and
`--controls` same-build pairs (default 2), whose largest deviation is the
noise floor. A metric qualifies when every session's median after/before ratio
lies outside both the `--band` (default 0.03) and that floor, in the same
direction.

It stops early in three cases:

- **The two builds did different work.** After the first pair it compares the
  graph hashes and the counts in the sync's `[phase-timing]` and
  `[synth-timing]` lines (references resolved, files resynthesized, pairs
  re-scored), and checks whether either build rebuilt the synthesis skip list.
  When the graph differs, it exits with status 3. When only the work differs,
  it also exits with status 3, unless you pass `--allow-work-diff` for a change
  meant to do less work.
- **The answer is already clear.** After the first session it stops when every
  pair lies outside the band and outside twice the control spread on `--metric`
  (default `peak`).
- **The noise floor is already known.** `--noise FILE` reuses the floor that
  `--save-noise` stored for the same host, kernel, runtime version, corpus
  revision, edit and `--env`, and then runs no controls. A kernel or runtime
  upgrade changes the key, so it is measured again.

The saved index keeps its synthesis skip list under a fingerprint of the
resolution code that wrote it. A build whose resolution code differs would
ignore the list and rescan every file. That is a real cost, the first sync
after an upgrade, but it is not the change under test. So each build first
runs once untimed, and when that run rebuilt the list, the build gets a copy of
the index labelled with its own fingerprint. The run prints a note when this
happens. `--no-relabel` measures the rescan instead.

`--proxies` runs each build once under `perf stat -e instructions:u`, and on
Node once with `alloc-total.cjs`, which totals the bytes allocated. These
counts vary much less than peak memory or wall time, so they screen a change
in four runs before the full comparison. The summary reports the after/before
ratio of each.

What the proxies predicted for four landed changes, against the results their
pull requests measured (2026-10-18; a repeat of two of them moved each ratio by
0.003 at most):

| Change | Runtime | Measured result | Instructions | Bytes allocated |
|---|---|---|---:|---:|
| #493 (Python import line) | Node | sync 29% faster | 0.676 | 1.000 |
| #493 | Bun | sync 35% faster | 0.658 | n/a |
| #494 (bases memo) | Node | sync about 3% faster | 0.957 | 0.999 |
| #494 | Bun | sync time no clear change | 0.959 | n/a |
| #528 (near-duplicates) | Node | peak memory no clear change | 0.998 | 0.947 |
| #528 | Bun | peak memory 12% lower | 1.036 | n/a |
| #529 (import-only lookup) | Node | peak memory 4.7% lower, wall no clear change | 0.952 | 0.930 |
| #529 | Bun | peak memory 5.7% lower | 0.971 | n/a |

The instruction count tracked large time changes closely (#493). Changes of
about 5% in instructions did not reliably show up in wall time, whose
same-build variation on n8n was up to 7%. Bytes allocated did not predict peak
memory: #528 allocated 5% less on Node with no change in peak, and its 12% drop
on Bun had no proxy at all. Use `--proxies` to tell whether a change does less
work, and run the full comparison for any memory claim.

`summary.md` holds the result table and the per-session ratios for a pull
request body. `chart.json` holds the qualified rows in the input format of the
before/after chart script. Every timed run is a line in `runs.jsonl`.
Admission waits until no `capped-*` scope runs, at least `--min-avail-gib`
(default 8) is free and the 1-minute load is under `--max-load` (default 4).
`BENCH_WRAP` prefixes each run, for example a memory cap; unlike the other
runners, it defaults to nothing.

Measured on n8n 7cc0450a, comparing #528 (5ccc8336) with #529 (the import-only
lookup), on Node 24.22.0:

| Command | Runs | Wall-clock | Result |
|---|---:|---|---|
| Default, two sessions | 22 | 5 min | Peak 961 to 915.5 MiB, 4.7% lower, qualified (controls 2.6%) |
| `--noise` with the floor stored by the run above | 14 | 3 min 30 s | 953 to 910.5 MiB, 4.5% lower, qualified |
| `--proxies` | 6 | 1 min 30 s | Instructions 0.952, bytes allocated 0.930 (1,692 to 1,574 MiB) |
| `--no-relabel` | 4 | 1 min 30 s | Stopped after the first pair: the after build rebuilt the skip list |

## After a Bun upgrade

Keep the previous Bun executable and run both against the same build, with
Node as the control. A difference between the two Bun arms is then Bun's
alone:

```bash
export BENCH_ARMS="bun-old=$HOME/.bun/previous/bun@$PWD bun-new=$(command -v bun)@$PWD node=$(fnm exec --using codegraph which node)@$PWD"
```

Run `npm run test:bun` on the new Bun first. Then rerun the repros for the
oven-sh/bun issues the README cites, and update only the README rows that
moved.
