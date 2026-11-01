#!/bin/bash
# Hyperfine whole-command runner for the seven-corpus index/sync campaign.
#
# usage: BENCH_ARMS="upstream=<node>@<build> fork=<node>@<build>" \
#          BENCH_OUT=<results-dir> \
#          hyperfine-index-sync.sh <list-file>
#
# Each list line: <corpus-path> <rel-file-to-edit> <comment-prefix: // or #>.
# For each corpus the arms run in mirrored order (upstream, fork, fork,
# upstream). Each invocation builds a fresh .cg-bench with one Hyperfine run
# and records the graph counts, then times two `codegraph sync` runs against
# that index. Hyperfine's --prepare appends a unique marker before every sync
# (hyperfine-append-probe.sh), so each run sees a one-file change; the file is
# restored after the arm.
#
# Peak RSS comes from Hyperfine's memory_peak_resident in the exported JSON.
# This script does no host admission or locking; the campaign wraps it in the
# host's benchmark lock and admission check.
set -u
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/arms.sh"
while read -r corpus file cm; do
  [ -z "$corpus" ] || [ "${corpus:0:1}" = "#" ] && continue
  NAME=$(basename "$corpus")
  cd "$corpus" || exit 1
  for arm in "${MIRRORED[@]}"; do
    runtime=${RUNTIME[$arm]}; build=${BUILD[$arm]}
    rm -rf .cg-bench
    hyperfine --warmup 0 --runs 1 --style basic --prepare 'rm -rf .cg-bench' \
      --export-json "$BENCH_OUT/$NAME-$arm-$(date +%s%N)-index.json" \
      "env CODEGRAPH_TELEMETRY=0 DO_NOT_TRACK=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_ALLOW_UNSAFE_NODE=1 CODEGRAPH_DIR=.cg-bench $runtime $build/dist/bin/codegraph.js init --yes ." \
      >> "$BENCH_OUT/$NAME-index.txt" 2>&1 || exit 7
    db=.cg-bench/codegraph.db
    printf '%s\t%s\t%s\t%s\t%s\n' "$NAME" "$arm" \
      "$(sqlite3 "$db" 'select count(*) from nodes')" \
      "$(sqlite3 "$db" 'select count(*) from edges')" \
      "$(stat -c %s "$db")" >> "$BENCH_OUT/graph.tsv"
    hyperfine --warmup 0 --runs 2 --style basic \
      --prepare "bash $HERE/hyperfine-append-probe.sh '$file' '$cm'" \
      --export-json "$BENCH_OUT/$NAME-$arm-$(date +%s%N)-sync.json" \
      "env CODEGRAPH_TELEMETRY=0 DO_NOT_TRACK=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_ALLOW_UNSAFE_NODE=1 CODEGRAPH_DIR=.cg-bench $runtime $build/dist/bin/codegraph.js sync ." \
      >> "$BENCH_OUT/$NAME-sync.txt" 2>&1 || exit 7
    git checkout -q -- "$file"
    rm -rf .cg-bench
  done
done
