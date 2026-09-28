#!/bin/bash
# usage: run-mcp.sh <list-file>
# Each list line: <corpus-path> <rel-file-to-edit> <queries.json>. Builds one
# index per distinct build, then runs mcp-bench.mjs for each arm in mirrored
# order. Results go to $BENCH_OUT/mcp.jsonl.
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/arms.sh"
NODE=${BENCH_NODE:-node}
while read -r corpus edit queries; do
  [ -z "$corpus" ] || [ "${corpus:0:1}" = "#" ] && continue
  cd "$corpus" || exit 1
  declare -A DIR=(); n=0
  for arm in "${ARMS[@]}"; do
    b=${BUILD[$arm]}; [ -n "${DIR[$b]:-}" ] && continue
    DIR[$b]=.cg-mcp-$((n++)); rm -rf "${DIR[$b]}"
    PRE=("${WRAP[@]}"); CGDIR=${DIR[$b]} runarm "$arm" init --yes . >/dev/null 2>&1
    [ -f "${DIR[$b]}/codegraph.db" ] || { echo "index for $arm failed in $corpus" >&2; exit 1; }
  done
  for arm in "${MIRRORED[@]}"; do
    idle
    "$NODE" "$HERE/mcp-bench.mjs" "$arm" "${RUNTIME[$arm]}" "${BUILD[$arm]}" "$corpus" "${DIR[${BUILD[$arm]}]}" "$edit" "$queries" \
      2>>"$BENCH_OUT/logs/mcp-stderr.log" | tee -a "$BENCH_OUT/mcp.jsonl"
  done
  for d in "${DIR[@]}"; do rm -rf "$d"; done
  unset DIR
done < "$1"
echo "== mcp done $(date +%T)"
