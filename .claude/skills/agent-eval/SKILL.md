---
name: agent-eval
description: Benchmark CodeGraph retrieval quality on a real codebase by comparing agent behavior with vs without CodeGraph. Use when the user runs /agent-eval or asks to test, benchmark, audit, or validate a codegraph version (the local dev build or a published npm version) against a language's repo.
---

# CodeGraph Quality Audit

Measures how much CodeGraph helps an agent versus plain grep/read, for a chosen
codegraph version on a chosen real-world repo. Drives the harness in
`scripts/agent-eval/`.

## Prerequisites
- `tmux` 3+, a logged-in `claude` CLI, `node`, `git` (macOS/Linux) for the Claude harness below.
- Model availability and fallback follow `docs/AGENTS.md`. An alternate provider needs a compatible runner; preserve matched arms and the CLI contamination guard, and report unsupported metrics. A focused correctness-only check can use one candidate run without a performance claim.
- Run from the codegraph repo root.

## Workflow

Copy this checklist:
```
- [ ] 1. Pick version (local or npm)
- [ ] 2. Pick language
- [ ] 3. Pick repo by size
- [ ] 4. Pick harness (headless / tmux / both)
- [ ] 5. Run audit.sh in the background
- [ ] 6. Report results
```

Use selections already supplied by the user. Ask for missing version, language,
repo and harness choices under the active host's question policy; bundle independent
choices where practical. This skill requires no particular question-card API.

**Step 1 — version.** Offer "Local dev build", "Latest published" and a specific
version (e.g. `0.7.10`). Map the answer to a VERSION token:
- "Local dev build" → `local`
- "Latest published" → `latest`
- a typed version → that string (e.g. `0.7.10`)

**Step 2 — language.** Read `.claude/skills/agent-eval/corpus.json` and use a language with an entry.

**Step 3 — repo.** From the chosen language's entries, ask which repo. Label each
option with its size and file count, e.g. `excalidraw — Medium (~600 files)`.
Each entry carries the `repo` URL and a representative `question`.

**Step 4 — harness.** Map the selected harness to a MODE token:
- "Headless" → `headless` — `claude -p` with stream-json: exact tokens/cost and a
  clean tool sequence (2 runs, fast, no TTY).
- "Interactive (tmux)" → `tmux` — drives the real Claude TUI in tmux: faithful
  Explore-subagent behavior, metrics from session logs (2 runs, slower).
- "Both" → `all` — headless + interactive (4 runs).

**Step 5 — run.** Record the selected arms, corpora, controls, run count and
correctness criteria before launching. Follow the host's admission and benchmark
policy when available; no personal skill installation is required. Paid calls
must fit the user's selected scope. A single exploratory pair supports no
performance claim; set repetitions and variation criteria before confirmation.

Run from an owned task checkout. Use a task-owned corpus: `audit.sh` deletes its
index and rebuilds it. Do not point it at a real project's or another session's
index. Set `TMPDIR` and `AGENT_EVAL_OUT` to owned disk directories, because the
runner defaults to temporary storage and overwrites named result files.

After local admission, launch the selected run with isolated paths:
```bash
# Replace these values with the selected version, repo and owned disk location.
EVAL_ROOT=/absolute/task-owned/disk-directory
mkdir -p "$EVAL_ROOT/tmp" "$EVAL_ROOT/corpus" "$EVAL_ROOT/results"
TMPDIR="$EVAL_ROOT/tmp" CORPUS="$EVAL_ROOT/corpus" \
  AGENT_EVAL_OUT="$EVAL_ROOT/results" \
  bash scripts/agent-eval/audit.sh local repo-name repo-url "question" headless
```
For confirmation repetitions, give each run a distinct results directory and
retain earlier results. Use the same binary/native build to index and serve each
arm. Preserve strict MCP configuration and the CLI contamination guard.

**Step 6 — report.** When the job finishes, read the log and report per arm:
- Headless (`parse-run.mjs`): total tool calls, file `Read`s, Grep/Bash,
  codegraph-tool calls, duration, **total cost**.
- Interactive (`parse-session.mjs`): the `VERDICT: codegraph_explore used Nx |
  Read N | Grep/Bash N` and `TOKENS:` lines.
- Both paths also print the three feedback metrics — residual context occupancy,
  explore sufficiency, allocation efficiency — and a headless A/B ends with a
  side-by-side `ARM COMPARISON` table. Report that table, and check its
  contamination row first: `CLI calls that RETURNED output` > 0 means the arm
  reached codegraph through Bash and its numbers are void. How to read the rest:
  `docs/benchmarks/agent-eval-feedback-metrics.md`.

Lead with cost + tool/Read counts — they are the reliable signals; raw token
in/out are confounded by subagent delegation and prompt caching. State whether
codegraph reduced effort and whether both arms reached a correct answer.

## Notes
- The index is rebuilt every run (`audit.sh` wipes `.codegraph`) — different
  versions extract differently, so an index must be served by the same binary
  that built it.
- `audit.sh local` builds the owning checkout and passes its exact executable as
  `CG_BIN`. Published versions use `npm install --prefix` in an isolated directory
  under `TMPDIR`, removed on exit. Neither path links or restores a global install.
- `run-all.sh` accepts an explicit `CG_BIN`; PATH is its fallback. Do not use
  `local-install.sh` to select an experiment binary: that script changes the global install.
- Corpus repos are cloned to `$CORPUS` (default `~/codegraph-corpora`, on the
  workspace disk) and reused if already present.
- Add or edit repos in `corpus.json` (fields: `name`, `repo`, `size`, `files`,
  `question`).
