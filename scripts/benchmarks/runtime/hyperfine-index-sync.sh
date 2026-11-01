#!/bin/bash
# Hyperfine whole-command runner for the seven-corpus index/sync campaign.
#
# usage: BENCH_ARMS="base=<node>@<build> cand=<node>@<build>" \
#          BENCH_OUT=<fresh-results-dir> \
#          hyperfine-index-sync.sh <list-file>
#
# Each list line: <corpus-path> <rel-file-to-edit> <comment-prefix: // or #>.
# For each corpus the arms run in mirrored order (base, candidate, candidate,
# base). Each invocation builds a fresh .cg-bench with one Hyperfine run and
# records the graph counts, then times two `codegraph sync` runs against that
# index. Hyperfine's --prepare appends a unique marker before every sync
# (hyperfine-append-probe.sh), so each run sees a one-file change. The probe
# file must be tracked and clean; its original bytes are kept outside the repo
# and restored after every arm and on exit.
#
# Peak RSS comes from Hyperfine's memory_peak_resident in the exported JSON.
# This script does no host admission or locking; a campaign wraps it in the
# host's benchmark lock and admission check.
set -u
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/arms.sh"
LIST=${1:?usage: hyperfine-index-sync.sh <list-file>}
[ -f "$LIST" ] || { echo "list file not found: $LIST" >&2; exit 2; }
if compgen -G "$BENCH_OUT/*-index.json" >/dev/null; then
  echo "results directory already contains a Hyperfine campaign; use a fresh BENCH_OUT" >&2
  exit 2
fi
WRAP_PREFIX=""
((${#WRAP[@]})) && WRAP_PREFIX="${WRAP[*]} "

CURRENT_FILE=""
CURRENT_BACKUP=""
cleanup() {
  if [ -n "$CURRENT_FILE" ] && [ -n "$CURRENT_BACKUP" ] && [ -f "$CURRENT_BACKUP" ]; then
    cp -- "$CURRENT_BACKUP" "$CURRENT_FILE" 2>/dev/null
    rm -f "$CURRENT_BACKUP"
  fi
}
trap cleanup EXIT INT TERM

while read -r corpus file cm; do
  [ -z "$corpus" ] || [ "${corpus:0:1}" = "#" ] && continue
  NAME=$(basename "$corpus" | tr '[:upper:]' '[:lower:]')
  cd "$corpus" || exit 1
  git ls-files --error-unmatch -- "$file" >/dev/null 2>&1 || {
    echo "probe file is not tracked: $corpus/$file" >&2; exit 2
  }
  if ! git diff --quiet -- "$file" || ! git diff --cached --quiet -- "$file"; then
    echo "probe file has uncommitted changes: $corpus/$file" >&2; exit 2
  fi
  backup=$(mktemp "$BENCH_OUT/.probe.XXXXXX") || exit 2
  cp -- "$file" "$backup"
  CURRENT_FILE="$file"; CURRENT_BACKUP="$backup"

  for arm in "${MIRRORED[@]}"; do
    runtime=${RUNTIME[$arm]}; build=${BUILD[$arm]}
    rm -rf .cg-bench
    hyperfine --warmup 0 --runs 1 --style basic --prepare 'rm -rf .cg-bench' \
      --export-json "$BENCH_OUT/$NAME-$arm-$(date +%s%N)-index.json" \
      "$WRAP_PREFIX""env CODEGRAPH_TELEMETRY=0 DO_NOT_TRACK=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_ALLOW_UNSAFE_NODE=1 CODEGRAPH_DIR=.cg-bench $runtime $build/dist/bin/codegraph.js init --yes ." \
      >> "$BENCH_OUT/$NAME-index.txt" 2>&1 || exit 7
    db=.cg-bench/codegraph.db
    printf '%s\t%s\t%s\t%s\t%s\n' "$NAME" "$arm" \
      "$(sqlite3 "$db" 'select count(*) from nodes')" \
      "$(sqlite3 "$db" 'select count(*) from edges')" \
      "$(stat -c %s "$db")" >> "$BENCH_OUT/graph.tsv"
    # A probe excluded by codegraph.json is silently absent from the index and
    # every sync below would be a no-op. Reject the arm instead.
    indexed=$(sqlite3 "$db" "select content_hash from files where path = '$file'")
    [ -n "$indexed" ] || {
      echo "probe file is not in the fresh index: $corpus/$file" >&2; exit 7
    }

    hyperfine --warmup 0 --runs 2 --style basic \
      --prepare "bash $HERE/hyperfine-append-probe.sh '$file' '$cm'" \
      --export-json "$BENCH_OUT/$NAME-$arm-$(date +%s%N)-sync.json" \
      "$WRAP_PREFIX""env CODEGRAPH_TELEMETRY=0 DO_NOT_TRACK=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_ALLOW_UNSAFE_NODE=1 CODEGRAPH_DIR=.cg-bench $runtime $build/dist/bin/codegraph.js sync ." \
      >> "$BENCH_OUT/$NAME-sync.txt" 2>&1 || exit 7
    # The last sync must have stored the last appended bytes. A mismatch means
    # the run timed an "already up to date" no-op (excluded file, failed watch,
    # or a stale index), not a one-file sync.
    file_hash=$(sha256sum -- "$file" | cut -d' ' -f1)
    db_hash=$(sqlite3 "$db" "select content_hash from files where path = '$file'")
    [ "$file_hash" = "$db_hash" ] || {
      echo "sync did not process the probe: $corpus/$file" >&2; exit 7
    }
    cp -- "$backup" "$file"
    rm -rf .cg-bench
  done
  rm -f "$backup"
  CURRENT_FILE=""; CURRENT_BACKUP=""
done < "$LIST"
