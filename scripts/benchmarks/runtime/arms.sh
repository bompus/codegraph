# Sourced by the runners. Defines the arms, `runarm <arm> <args...>` and `idle`.
#
# BENCH_ARMS lists the arms as `name=RUNTIME@BUILD`, space-separated. RUNTIME
# is a node or bun executable; BUILD is a built checkout (has dist/bin/codegraph.js).
#   BENCH_ARMS="node=$(fnm exec --using codegraph which node)@$PWD bun=$(command -v bun)@$PWD"
# BENCH_OUT is the results directory. BENCH_WRAP prefixes every timed command
# (default `nice -n 10`; on a shared host add a memory cap, e.g.
# `nice -n 10 systemd-run --user --scope -p MemoryMax=16G --`).
: "${BENCH_ARMS:?set BENCH_ARMS, see arms.sh}"
: "${BENCH_OUT:?set BENCH_OUT to a results directory}"
mkdir -p "$BENCH_OUT/logs"
read -r -a WRAP <<< "${BENCH_WRAP:-nice -n 10}"
declare -A RUNTIME BUILD
ARMS=()
for spec in $BENCH_ARMS; do
  name=${spec%%=*}; rest=${spec#*=}
  RUNTIME[$name]=${rest%@*}; BUILD[$name]=${rest##*@}
  [ -f "${BUILD[$name]}/dist/bin/codegraph.js" ] || { echo "arm $name: no dist/bin/codegraph.js under ${BUILD[$name]}" >&2; exit 1; }
  ARMS+=("$name")
done
# Each arm runs twice per corpus, in order and then reversed, so drift over
# the run lands on every arm alike.
MIRRORED=("${ARMS[@]}"); for ((i=${#ARMS[@]}-1; i>=0; i--)); do MIRRORED+=("${ARMS[$i]}"); done

# CODEGRAPH_ALLOW_UNSAFE_NODE lets a build that refuses Bun's reported Node
# version (upstream refuses Node 25 and newer) start; the fork ignores it.
BENCH_ENV=(CODEGRAPH_TELEMETRY=0 DO_NOT_TRACK=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_ALLOW_UNSAFE_NODE=1)
runarm() {
  local arm=$1; shift
  env "${BENCH_ENV[@]}" CODEGRAPH_DIR="${CGDIR:-.cg-bench}" "${PRE[@]}" "${RUNTIME[$arm]}" "${BUILD[$arm]}/dist/bin/codegraph.js" "$@"
}

# Wait until no capped job is running and the 1-minute load is under 2.
idle() {
  until [ -z "$(systemctl --user list-units 'capped-*' --no-legend --state=running 2>/dev/null)" ] && awk '{exit !($1<2)}' /proc/loadavg; do sleep 20; done
}
