# Diff-impact eval: first baseline

**Question:** When an agent is asked which other files an uncommitted change
affects, does having CodeGraph help it name them?

**Short answer:** This set of cases cannot tell yet. Both arms named every other
changed file in 34 of 36 sessions (each arm missed one test file once), and the arm with CodeGraph called
`codegraph_explore` in only 3 of its 18 sessions. The agent read `git diff` and
used grep instead, which is enough on repositories the size of gin and express.
The result is a ceiling, not evidence for or against the diff-seeded explore path.

## Task class

Six real commits, four from gin and two from express, defined in
`scripts/agent-eval/diff-impact-cases.json`. For each, the fixture is the
commit's parent as a single-commit repository (no history to read the answer
from) with one file edited and left uncommitted. The agent is asked which other
source files and tests are affected. The answer is scored on whether it names the
other code files the real commit changed.

That truth set is a co-change proxy. A file can depend on the edit without
having been changed in the same commit, so **recall** (the share of those files
named) is the main score. **Precision floor** is the share of named files that
are in the truth set. It is a lower bound, because a named file the commit did
not touch may still be affected.

Each case has 2 to 3 expected files (one source file plus one or two tests).
One missed file moves a case's recall by 33 to 50 points.

## Setup

- Run date: 2026-10-11. Model sonnet, effort high, through `run-all.sh`'s
  headless arms (`headless-with` has only the CodeGraph MCP server,
  `headless-without` has none; the `codegraph` CLI is blocked in both).
- CodeGraph: the 1.6.2 `dist` build of this repository from 2026-10-10, which
  already contains the diff-seeded explore.
- 6 cases × 2 arms × 3 repetitions = 36 sessions, each capped at $4.
- Command: `CG_BIN=<dist/bin/codegraph.js> scripts/agent-eval/run-diff-impact.sh <clones-dir> <out-dir> 3`.

## Result

| Arm | Sessions | Recall | Precision floor | Mean cost per session | Mean time per session |
|---|---|---|---|---|---|
| With CodeGraph | 18 | 97% | 25% | $0.150 | 19.1 s |
| Without | 18 | 97% | 29% | $0.139 | 19.2 s |

Per case, all three repetitions of every case scored 100% in both arms except
`express-merge-dep`, where each arm missed `test/res.cookie.js` once (83% in both).
The cost and time differences are within what three repetitions of 19-second
sessions can show, so they are no clear change.

| What the agent did | With CodeGraph | Without |
|---|---|---|
| Sessions that called `codegraph_explore` | 3 of 18 | not available |
| Sessions that ran `git diff` or `git status` | 18 of 18 | 18 of 18 |

## What this shows and does not show

- It does show that the harness runs end to end, scores answers, and costs about
  $5 and 13 minutes for 36 sessions.
- It does not show that CodeGraph helps or does not help on this task. With the
  question text pointing at `git diff`, a repository this small is solved without
  a code graph, so both arms hit the ceiling.
- It does show an adoption gap: told there were uncommitted changes, the agent
  rarely reached for the tool whose diff-seeded path was built for exactly that.
  Whether the cause is the question wording or the tool description is not
  separated here.

## Next experiments this baseline points to

- Drop the "see git diff" hint from the question, so the diff has to be found by
  some route.
- Add a larger repository, where grep over a changed symbol returns too many
  candidates to read, and a case whose truth set has more than three files.
- Compare the forced path (a prompt that names `codegraph_explore`) against the
  default, to separate tool quality from tool adoption.
