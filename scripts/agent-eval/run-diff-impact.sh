#!/usr/bin/env bash
# Diff-impact A/B: does codegraph help an agent say what an uncommitted edit
# affects? Per case it builds a fixture (see diff-impact-fixture.mjs), indexes
# it with $CG_BIN, runs the with/without headless arms through run-all.sh, and
# scores each arm's answer against the commit's other changed files.
#
# Usage: run-diff-impact.sh <clones-dir> <out-dir> [runs-per-case] [case-id ...]
#   <clones-dir>  holds a full-history clone per repo named in the cases file
#                 (for example gin/ and express/)
#   <out-dir>     owned disk directory; one subdirectory per case and run
#   [runs]        repetitions per case (default 1)
#   [case-id ..]  limit to these cases (default: all)
# Env: CG_BIN (codegraph binary, required), MODEL / EFFORT (as run-all.sh),
#      CG_ARMS=both|with|without (default both)
#
# Every run is a real model session; run-all.sh caps each at $4. Record the
# cases, runs and arms before launching, and keep the number of runs fixed for
# the whole comparison.
set -euo pipefail

CLONES="${1:?usage: run-diff-impact.sh <clones-dir> <out-dir> [runs] [case-id ...]}"
OUT="${2:?out-dir required}"
RUNS="${3:-1}"
shift $(( $# < 3 ? $# : 3 ))
HARNESS="$(cd "$(dirname "$0")" && pwd)"
CASES="$HARNESS/diff-impact-cases.json"
: "${CG_BIN:?set CG_BIN to the codegraph binary under test}"
mkdir -p "$OUT"

QUESTION='I have uncommitted changes in this repository (see git diff). Which other files are affected by this change: callers and dependents in the source, and tests that cover the changed code? List the file paths, source files first and then tests, with one line on why each is affected. Do not edit anything.'

ids=("$@")
if [ ${#ids[@]} -eq 0 ]; then
  while IFS= read -r id; do ids+=("$id"); done < <(node -e 'for (const c of require(process.argv[1]).cases) console.log(c.id)' "$CASES")
fi

for id in "${ids[@]}"; do
  repo="$(node -e 'const c=require(process.argv[1]).cases.find(x=>x.id===process.argv[2]); if(!c) process.exit(1); console.log(c.repo)' "$CASES" "$id")"
  fx="$OUT/$id/fixture"
  node "$HARNESS/diff-impact-fixture.mjs" "$id" "$CLONES/$repo" "$fx" >/dev/null
  ( cd "$fx/repo" && "$CG_BIN" init -i >/dev/null )
  for run in $(seq 1 "$RUNS"); do
    dir="$OUT/$id/run-$run"
    mkdir -p "$dir"
    AGENT_EVAL_OUT="$dir" bash "$HARNESS/run-all.sh" "$fx/repo" "$QUESTION" headless > "$dir/run-all.log" 2>&1 \
      || echo "run-all failed for $id run $run (see $dir/run-all.log)" >&2
    for arm in headless-with headless-without; do
      [ -s "$dir/run-$arm.jsonl" ] || continue
      node "$HARNESS/diff-impact-score.mjs" "$dir/run-$arm.jsonl" "$fx/truth.json" "$fx/repo" \
        | node -e 'const s=JSON.parse(require("fs").readFileSync(0,"utf8")); console.log(JSON.stringify({...s, arm:process.argv[1], run:Number(process.argv[2])}))' "$arm" "$run" \
        >> "$OUT/scores.jsonl"
    done
  done
done

node "$HARNESS/diff-impact-summary.mjs" "$OUT/scores.jsonl"
