#!/usr/bin/env bash
# Bisect one precision case: `git bisect run scripts/precision/bisect-case.sh <corpus> <case-id>`
#
#   git bisect start --first-parent <bad> <good>
#   EVAL_REPOS=<dir holding the corpus> git bisect run scripts/precision/bisect-case.sh vite vite-factory-receiver-control
#
# Each step rebuilds the kernel for the checked-out commit and runs the
# precision runner. Exit 0 when the case holds, 1 when it is violated.
#
# A step that cannot score the case (kernel build fails, runner errors, the
# case is absent from the report) ABORTS the bisect with 255 rather than
# skipping with 125: a systematic failure otherwise skips every commit and the
# bisect ends with nothing learned. Step logs go to $BISECT_LOGS. Run the
# precision runner once first so the corpus is already checked out.
#
# `git bisect run` exports GIT_DIR naming this repository; the runner (and the
# indexer under it) would then run every corpus git command against the engine.
# The runner clears those variables itself, but commits older than that fix do
# not, so this script clears them before calling it.
set -uo pipefail
corpus="${1:?usage: bisect-case.sh <corpus> <case-id>}"
case_id="${2:?usage: bisect-case.sh <corpus> <case-id>}"
: "${EVAL_REPOS:?set EVAL_REPOS to the directory holding the corpus checkout}"
engine="$(git rev-parse --show-toplevel)"
sha="$(git rev-parse --short HEAD)"
logs="${BISECT_LOGS:-${XDG_DATA_HOME:-$HOME/.local/share}/agent-documents/tmp/bisect-$corpus}"
mkdir -p "$logs"
log="$logs/$sha.log"
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
cd "$engine" || exit 255

abort() { echo "bisect-case: $1 at $sha (see $log)" >&2; exit 255; }

npm run -s build:kernel >"$log" 2>&1 || abort "kernel build failed"
npx tsx __tests__/evaluation/precision-runner.ts "$corpus" >>"$log" 2>&1
# The runner writes a report under results/; drop it so the next checkout is clean.
git clean -qf -- __tests__/evaluation/results/ && git checkout -q -- __tests__/evaluation/results/ 2>/dev/null

grep -q "fetching " "$log" && abort "runner re-fetched the corpus instead of reusing $EVAL_REPOS/$corpus"
line="$(grep -E "^[[:space:]]*$case_id[[:space:]]" "$log" | head -1)"
[ -n "$line" ] || abort "case $case_id not in the runner output"
case "$line" in
  *HELD*) exit 0 ;;
  *VIOLATED*|*MISSING*) exit 1 ;;
  *) abort "unrecognised verdict: $line" ;;
esac
