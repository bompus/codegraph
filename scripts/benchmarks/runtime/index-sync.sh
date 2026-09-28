#!/bin/bash
# usage: index-sync.sh <corpus-path> <rel-file> <comment-prefix>
# Per arm (mirrored order): a timed full index, then two timed one-file syncs
# after appending a comment and a symbol to <rel-file>. Appends rows to
# $BENCH_OUT/index-sync.tsv and restores the file with git.
set -u
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/arms.sh"
C=$1; F=$2; CM=$3; NAME=$(basename "$C"); cd "$C" || exit 1
TIME=(/usr/bin/time -v)
for arm in "${MIRRORED[@]}"; do
  rm -rf .cg-bench
  log=$BENCH_OUT/logs/idx-$NAME-$arm-$(date +%s).log
  read -r l1 _ < /proc/loadavg
  PRE=("${TIME[@]}" "${WRAP[@]}"); runarm "$arm" init --yes . > "$log" 2>&1; rc=$?
  db=.cg-bench/codegraph.db; sqlite3 "$db" "PRAGMA wal_checkpoint(TRUNCATE)" >/dev/null 2>&1
  printf "%s\t%s\tindex\t%s\t%s\t%s\t%s\t%s\t%s\t%s\tload=%s\n" "$NAME" "$arm" "$rc" \
    "$(grep 'Elapsed (wall' "$log" | awk '{print $NF}')" "$(grep 'Maximum resident' "$log" | awk '{print $NF}')" \
    "$(grep 'Percent of CPU' "$log" | awk '{print $NF}')" "$(stat -c %s "$db" 2>/dev/null)" \
    "$(sqlite3 "$db" 'select count(*) from nodes' 2>/dev/null)" "$(sqlite3 "$db" 'select count(*) from edges' 2>/dev/null)" "$l1" \
    | tee -a "$BENCH_OUT/index-sync.tsv"
  for n in 1 2; do
    printf "\n%s cg bench probe %s\nvoid_probe_marker_%s = 1\n" "$CM" "$n" "$n" >> "$F"
    [ "$CM" = "#" ] || sed -i '$ s/.*/\/\/ probe end/' "$F"
    log=$BENCH_OUT/logs/sync-$NAME-$arm-$n-$(date +%s).log
    read -r l1 _ < /proc/loadavg
    PRE=("${TIME[@]}" "${WRAP[@]}"); runarm "$arm" sync . > "$log" 2>&1; rc=$?
    printf "%s\t%s\tsync%s\t%s\t%s\t%s\t%s\t\t\t\tload=%s\n" "$NAME" "$arm" "$n" "$rc" \
      "$(grep 'Elapsed (wall' "$log" | awk '{print $NF}')" "$(grep 'Maximum resident' "$log" | awk '{print $NF}')" \
      "$(grep 'Percent of CPU' "$log" | awk '{print $NF}')" "$l1" | tee -a "$BENCH_OUT/index-sync.tsv"
  done
  git checkout -q -- "$F"
  rm -rf .cg-bench
done
