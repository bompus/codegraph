#!/bin/bash
# usage: run-cli.sh <corpus-path> <explore query>
# CLI startup per arm with hyperfine: --version, status, and one explore,
# against one index per distinct build. Results: $BENCH_OUT/cli-*.json.
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/arms.sh"
cd "$1" || exit 1; Q=$2
declare -A DIR=(); n=0
for arm in "${ARMS[@]}"; do
  b=${BUILD[$arm]}; [ -n "${DIR[$b]:-}" ] && continue
  DIR[$b]=.cg-cli-$((n++)); rm -rf "${DIR[$b]}"
  PRE=(); CGDIR=${DIR[$b]} runarm "$arm" init --yes . >/dev/null 2>&1
  [ -f "${DIR[$b]}/codegraph.db" ] || { echo "index for $arm failed" >&2; exit 1; }
done
for sub in --version status explore; do
  idle
  args=()
  for arm in "${ARMS[@]}"; do
    cmd="env ${BENCH_ENV[*]} CODEGRAPH_DIR=${DIR[${BUILD[$arm]}]} ${RUNTIME[$arm]} ${BUILD[$arm]}/dist/bin/codegraph.js $sub"
    [ "$sub" = status ] && cmd+=" ."
    [ "$sub" = explore ] && cmd+=" '$Q'"
    args+=(-n "$arm" "$cmd")
  done
  "${WRAP[@]}" hyperfine -N --warmup 3 --runs 20 --export-json "$BENCH_OUT/cli-${sub#--}.json" "${args[@]}" 2>&1 | tee -a "$BENCH_OUT/cli.txt"
done
for d in "${DIR[@]}"; do rm -rf "$d"; done
