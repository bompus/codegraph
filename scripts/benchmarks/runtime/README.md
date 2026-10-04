# Runtime benchmark

These scripts produced the README's "Measured results" and the tables in
[`docs/benchmarks/fork-vs-upstream-node-bun-2026-10-03.md`](../../../docs/benchmarks/fork-vs-upstream-node-bun-2026-10-03.md).
Each arm is a runtime (a `node` or `bun` executable) running a built checkout.
The arms can differ in build, in runtime, or in both.

| Script | Measures | Output in `$BENCH_OUT` |
|---|---|---|
| `run-index-sync.sh <list>` | Full index, then two one-file syncs: wall time, peak RSS, CPU, database size, node and edge counts | `index-sync.tsv`, `logs/` |
| `run-mcp.sh <list>` | MCP server: start to first explore answer, warm explore median and p90, 8 explores at once, watcher sync, idle memory and CPU | `mcp.jsonl` |
| `run-cli.sh <corpus> <query>` | CLI start: `--version`, `status`, one `explore` (hyperfine, 20 runs) | `cli-*.json`, `cli.txt` |
| `summarize.py $BENCH_OUT` | Medians per corpus and arm from `index-sync.tsv`, and from `mcp.jsonl` when present | stdout |

Every run needs a quiet host. Each runner waits before a corpus until no
`capped-*` scope runs and the 1-minute load is under 2, and each arm runs twice
per corpus, forward and then reversed.

## Run time

The 2026-10-03 run behind the README took 1 h 47 min (17:09 to 18:56):
`run-index-sync.sh` on the seven corpora in the benchmark doc took 59 min, then
`run-mcp.sh` on gin and n8n and `run-cli.sh` on gin took 48 min, with four arms
(upstream and fork, each on Node and Bun). That includes the idle-host waits.
The n8n MCP runs took about 4 min each. Building the two worktrees took 4 min
before it. The 2026-09-28 run of the same steps took 1 h 26 min. Start a re-run
estimate from these numbers, and update them here after a run that takes much
longer or shorter.

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
