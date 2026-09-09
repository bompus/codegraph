#!/usr/bin/env bash
# With/without A/B (and optional interactive) eval for a codegraph version on a
# repo. Codegraph is the ONLY variable: both arms launch claude with
# --strict-mcp-config — with = codegraph-only MCP (pointed at $CG_BIN),
# without = empty MCP. Built-in Read/Grep/Bash stay available in both arms.
#
# Usage: run-all.sh <repo-path> "<question>" [headless|tmux|all]
#
# Each headless arm reports the three feedback metrics (parse-run.mjs prints all
# three under every run, and compare-arms.mjs puts the arms side by side at the
# end when both ran):
#   residual context occupancy (CG-7)  window still held by the arm's retrieval
#   explore sufficiency        (CG-8)  was the response ENOUGH — the agent's next act
#   allocation efficiency      (CG-9)  share of returned bytes the answer cited
# docs/benchmarks/agent-eval-feedback-metrics.md is the entry point; the three
# per-metric docs it links carry the caveats.
#
# MULTI-TURN: separate questions with "||" to run them as ONE session —
#   run-all.sh <repo> "How does X work?||Where is Y handled in that path?"
# Turn 1 runs normally; every later turn `--resume`s the same session, so the
# earlier turns' tool output is still in the window (that is the whole point:
# residual context occupancy, the cost a single-question run cannot see).
# Segments land in run-<label>.jsonl, run-<label>.t2.jsonl, … and parse-run.mjs
# stitches them back into one session.
#
# Env:   CG_BIN          codegraph binary (default: command -v codegraph)
#        AGENT_EVAL_OUT  output dir (default: /tmp/agent-eval)
#        MODEL / EFFORT  claude model/effort (default: sonnet / high — the
#                        standing A/B policy; see CLAUDE.md, don't raise)
set -euo pipefail

REPO="${1:?usage: run-all.sh <repo-path> \"<question>\" [headless|tmux|all]}"
Q="${2:?question required}"
MODE="${3:-headless}"

# Split "Q1||Q2||Q3" into turns (kept bash-3.2-safe: macOS ships 3.2).
TURNS=()
rest="$Q"
while [ "$rest" != "${rest#*||}" ]; do
  TURNS+=("${rest%%||*}")
  rest="${rest#*||}"
done
TURNS+=("$rest")
CG_BIN="${CG_BIN:-$(command -v codegraph || true)}"
OUT="${AGENT_EVAL_OUT:-/tmp/agent-eval}"
HARNESS="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$OUT"

[ -n "$CG_BIN" ] || { echo "no codegraph binary on PATH (set CG_BIN)"; exit 1; }
[ -x "$CG_BIN" ] || { echo "codegraph binary is not executable: $CG_BIN"; exit 1; }
CG_BIN="$(cd "$(dirname "$CG_BIN")" && pwd)/$(basename "$CG_BIN")"

# Neutralize any ambient CodeGraph prompt-hook (~/.claude) in BOTH arms:
# the hook injects codegraph context into every prompt, which contaminates
# the without-arm (free structural context) and double-counts the with-arm.
# The A/B's only variable must be the MCP server wired below.
export CODEGRAPH_NO_PROMPT_HOOK=1

# Hide the codegraph CLI from BOTH arms, so the only way to reach codegraph is
# the MCP server wired below — which is what makes it the A/B's single variable.
# Two layers (sanitized PATH + a PreToolUse hook that blocks absolute-path
# invocations), both in no-cli-shim.sh, which ab-new-vs-baseline.sh shares; the
# reasoning and the incident that forced each layer are documented there.
# Sets $ARM_PATH and $ARM_SETTINGS, and aborts if either layer fails its probe.
. "$HARNESS/no-cli-shim.sh"
cg_no_cli_setup "$OUT" || exit 1

[ -d "$REPO/.codegraph" ] || { echo "no .codegraph index at $REPO — index it first"; exit 1; }
case "$MODE" in headless|tmux|all) ;; *) echo "mode must be headless|tmux|all (got '$MODE')"; exit 1;; esac
ARMS="${CG_ARMS:-both}"
case "$ARMS" in both|with|without) ;; *) echo "CG_ARMS must be both|with|without (got '$ARMS')"; exit 1;; esac

# MCP config files (path form avoids inline-JSON quoting through tmux).
cat > "$OUT/mcp-codegraph.json" <<JSON
{"mcpServers":{"codegraph":{"command":"$CG_BIN","args":["serve","--mcp","--path","$REPO"]}}}
JSON
echo '{"mcpServers":{}}' > "$OUT/mcp-empty.json"

echo "###### codegraph: $CG_BIN"
echo "###### repo:      $REPO"
echo "###### turns:     ${#TURNS[@]}"
for t in "${TURNS[@]}"; do echo "######   - $t"; done
echo

# Pull the session id out of a segment's result event so the next turn can
# --resume it (rather than minting a --session-id, which needs a valid uuid).
session_id_of() {
  node -e '
    const fs=require("fs");
    for (const l of fs.readFileSync(process.argv[1],"utf8").split("\n").reverse()) {
      if (!l) continue; let e; try { e=JSON.parse(l) } catch { continue }
      if (e.session_id) { console.log(e.session_id); break }
    }' "$1" 2>/dev/null
}

valid_result() {
  node -e '
    const fs=require("fs"); let result;
    for (const line of fs.readFileSync(process.argv[1], "utf8").split("\n")) {
      if (!line) continue;
      let event; try { event=JSON.parse(line) } catch { continue }
      if (event.type === "result") result=event;
    }
    if (!result || result.subtype !== "success" || typeof result.result !== "string" || !result.result.trim()) process.exit(1);
  ' "$1"
}

# Headless arm: claude -p with stream-json -> exact tool sequence + tokens/cost
# + residual context occupancy. One session, one segment file per turn.
headless() {
  local label="$1" cfg="$2"
  echo "############################## HEADLESS [$label] ##############################"
  local sid="" seg=0 out="" files=()
  rm -f "$OUT/run-$label.jsonl" "$OUT/run-$label.t"*.jsonl
  : > "$OUT/run-$label.err"
  for q in "${TURNS[@]}"; do
    seg=$((seg + 1))
    out="$OUT/run-$label.jsonl"
    [ "$seg" -gt 1 ] && out="$OUT/run-$label.t$seg.jsonl"
    local resume=()
    [ -n "$sid" ] && resume=(--resume "$sid")
    if ! ( cd "$REPO" && PATH="$ARM_PATH" claude -p "$q" \
        --output-format stream-json --verbose \
        --permission-mode bypassPermissions \
        --model "${MODEL:-sonnet}" --effort "${EFFORT:-high}" \
        --max-budget-usd 4 \
        --strict-mcp-config --mcp-config "$cfg" \
        --settings "$ARM_SETTINGS" \
        ${resume[@]+"${resume[@]}"} \
        </dev/null > "$out" 2>>"$OUT/run-$label.err" ); then
      echo "claude failed for $label turn $seg" >&2
      return 1
    fi
    [ -s "$out" ] || { echo "empty result artifact: $out" >&2; return 1; }
    valid_result "$out" || { echo "missing successful result in $out" >&2; return 1; }
    echo "exit 0 -> $out ($(wc -l < "$out" | tr -d ' ') lines) [turn $seg/${#TURNS[@]}]"
    files+=("$out")
    sid="$(session_id_of "$out")"
    if [ -z "$sid" ] && [ "$seg" -lt "${#TURNS[@]}" ]; then
      echo "no session_id in $out — cannot run later turns in the same context" >&2
      return 1
    fi
  done
  tail -2 "$OUT/run-$label.err" 2>/dev/null
  node "$HARNESS/parse-run.mjs" "${files[@]}" 2>&1
  echo
}

if [ "$MODE" = headless ] || [ "$MODE" = all ]; then
  case "$ARMS" in both|with)    headless "headless-with"    "$OUT/mcp-codegraph.json";; esac
  case "$ARMS" in both|without) headless "headless-without" "$OUT/mcp-empty.json";; esac
  # A full A/B is valid only when both freshly selected arms can be compared.
  if [ "$ARMS" = both ]; then
    node "$HARNESS/compare-arms.mjs" "$OUT" headless-with headless-without 2>&1
  fi
fi

if [ "$MODE" = tmux ] || [ "$MODE" = all ]; then
  echo "############################## INTERACTIVE [with] ##############################"
  CLAUDE_EXTRA_ARGS="--model ${MODEL:-sonnet} --effort ${EFFORT:-high} --strict-mcp-config --mcp-config $OUT/mcp-codegraph.json" \
    bash "$HARNESS/itrun.sh" "$REPO" "int-with" "${TURNS[0]}" 2>&1
  [ -s "$OUT/itrun-int-with.txt" ] || { echo "missing interactive with-arm artifact" >&2; exit 1; }
  echo
  echo "############################## INTERACTIVE [without] ##############################"
  CLAUDE_EXTRA_ARGS="--model ${MODEL:-sonnet} --effort ${EFFORT:-high} --strict-mcp-config --mcp-config $OUT/mcp-empty.json" \
    bash "$HARNESS/itrun.sh" "$REPO" "int-without" "${TURNS[0]}" 2>&1
  [ -s "$OUT/itrun-int-without.txt" ] || { echo "missing interactive without-arm artifact" >&2; exit 1; }
  echo
fi
echo "############################## RUN-ALL COMPLETE ##############################"
