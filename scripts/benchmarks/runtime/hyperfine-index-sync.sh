#!/bin/bash
# Hyperfine whole-command runner for the seven-corpus index/sync campaign.
#
# usage: BENCH_ARMS="base=<node>@<build> cand=<node>@<build>" \
#          BENCH_OUT=<fresh-results-dir> \
#          hyperfine-index-sync.sh <list-file>
#
# Each list line: <corpus-path> <rel-file-to-edit> <comment-prefix: // or #>.
# For each corpus the arms run in mirrored order (base, candidate, candidate,
# base). Each invocation builds a fresh, uniquely named .cg-bench-<campaign> with one Hyperfine run and
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
# An exclusively owned index directory, never the corpus's own .cg-bench (which
# may hold a real index and viewer trails). CODEGRAPH_DIR must be a plain name,
# so the campaign id cannot contain a path separator.
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
CG_DIR=".cg-bench-$RUN_ID"

CURRENT_FILE=""
CURRENT_BACKUP=""
cleanup() {
  if [ -n "$CURRENT_FILE" ] && [ -n "$CURRENT_BACKUP" ] && [ -f "$CURRENT_BACKUP" ]; then
    # Never delete the only copy of the probe's original bytes. A failed
    # restore keeps the backup and says where it is.
    if cp -- "$CURRENT_BACKUP" "$CURRENT_FILE"; then
      rm -f "$CURRENT_BACKUP"
    else
      echo "could not restore $CURRENT_FILE; original bytes kept at $CURRENT_BACKUP" >&2
    fi
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
  # Register the backup only after it is a verified byte copy. A full results
  # filesystem can leave mktemp's file empty or partial, and the EXIT handler
  # would then copy that over the probe.
  if ! cp -- "$file" "$backup" || ! cmp -s -- "$file" "$backup"; then
    rm -f "$backup"
    echo "could not back up probe file: $corpus/$file" >&2
    exit 2
  fi
  CURRENT_FILE="$file"; CURRENT_BACKUP="$backup"

  for arm in "${MIRRORED[@]}"; do
    runtime=${RUNTIME[$arm]}; build=${BUILD[$arm]}
    rm -rf -- "$CG_DIR"
    hyperfine --warmup 0 --runs 1 --style basic --prepare "rm -rf -- $CG_DIR" \
      --export-json "$BENCH_OUT/$NAME-$arm-$(date +%s%N)-index.json" \
      "$WRAP_PREFIX""env CODEGRAPH_TELEMETRY=0 DO_NOT_TRACK=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_ALLOW_UNSAFE_NODE=1 CODEGRAPH_DIR=$CG_DIR $runtime $build/dist/bin/codegraph.js init --yes ." \
      >> "$BENCH_OUT/$NAME-index.txt" 2>&1 || exit 7
    db=$CG_DIR/codegraph.db
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
      "$WRAP_PREFIX""env CODEGRAPH_TELEMETRY=0 DO_NOT_TRACK=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_ALLOW_UNSAFE_NODE=1 CODEGRAPH_DIR=$CG_DIR $runtime $build/dist/bin/codegraph.js sync ." \
      >> "$BENCH_OUT/$NAME-sync.txt" 2>&1 || exit 7
    # The last sync must have stored the last appended bytes. A mismatch means
    # the run timed an "already up to date" no-op (excluded file, failed watch,
    # or a stale index), not a one-file sync.
    file_hash=$(sha256sum -- "$file" | cut -d' ' -f1)
    db_hash=$(sqlite3 "$db" "select content_hash from files where path = '$file'")
    [ "$file_hash" = "$db_hash" ] || {
      echo "sync did not process the probe: $corpus/$file" >&2; exit 7
    }
    if ! cp -- "$backup" "$file" || ! cmp -s -- "$file" "$backup"; then
      echo "could not restore probe file: $corpus/$file" >&2; exit 7
    fi
    rm -rf -- "$CG_DIR"
  done
  rm -f "$backup"
  CURRENT_FILE=""; CURRENT_BACKUP=""
done < "$LIST"
