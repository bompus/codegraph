Title: `codegraph_explore`: a character or symbol-only budget beside `maxFiles`

Posted 2026-09-04: https://github.com/colbymchenry/codegraph/issues/1701

## Summary

`maxFiles` is the only size knob on `codegraph_explore`, and it counts files, not characters. Each file that makes the cut contributes whole symbol bodies plus its share of the blast-radius and relationships sections, so a small `maxFiles` does not give a small answer. Ask: a `maxChars` budget, or a mode that returns only the named symbols' source with caller names and no bodies for the neighbours.

## Measured (1.6.0, Windows, ~500-file TypeScript/Vue repo)

One symbol query, the same every time, varying only `maxFiles`:

| `maxFiles` | answer size |
| ---------: | ----------: |
|          1 |        5.4k |
|          3 |        8.1k |
|          6 |       20.5k |
|         12 |       20.7k |
|      unset |        7.7k |

Natural-language queries are worse: at `maxFiles: 3` the answer was still 13–24k characters in 7 of 8 calls, because a file that matches on a prose term renders its matched sections whole.

## Why it matters on the client side

Every later turn re-reads the whole context, so answer size is the cost that compounds. On a paired A/B (headless Claude Code, Opus, n = 12) the tool cut a discovery task from 5.67 to 3.33 calls and still raised cost per task by a median $0.13, and the ~21.7k-character mean answer is the reason (a bounded Read averages ~4.6k on the same repo). Telling the model about `maxFiles` in the repo rules did not help: a rule naming the cap changed calls +0.67 and cost −$0.14 per task, neither under p .3 at n = 9.

What did shrink answers was a shape change, not a count: the section-first doc tier in #1699 renders a markdown hit's top sections under a per-file source cap and drops the graph sections when no code file rendered, and the doc-question chain went from a median 4 calls to 1 with cost per cell $0.30 against $0.54. A code-side equivalent would be the ask here: a character budget the tool spends in rank order, or a symbols-only mode where neighbours appear as `name (file:line)` rather than as source.

Happy to send a PR for either if the maintainer has a preference; #1699's doc tier in `src/mcp/tools.ts` already spends a per-file cap in rank order, so `maxChars` is mostly plumbing a value from the input schema into the same place.

---

## Follow-up comment — measured both asks, neither pays

Posted 2026-09-04: https://github.com/colbymchenry/codegraph/issues/1701#issuecomment-5549083912 (one comment in place of the clarifying and numbers comments the plan row named).

Correction to the issue first: the tool already has a tiered character budget. `getExploreOutputBudget` in `src/mcp/tools.ts` picks `maxOutputChars` by code-file count (18,000 for a repo under 500 code files, hard ceiling 1.5×, capped at 25,000), and `allocateExploreBudget` spends it in rank order with the overflow rendered as pointers. The 20.7k above is that ceiling, not an unbounded answer. So the ask reduces to exposing the budget, and I measured both shapes before offering a PR:

- `maxChars` input / `CODEGRAPH_EXPLORE_MAX_CHARS` lowering `maxOutputChars` (clamped 2000–25000, never raising a tier);
- `CODEGRAPH_EXPLORE_SYMBOLS_ONLY=1` rendering source only for files that define a named symbol, every other ranked file as pointers.

Branch: `bompus/codegraph:feat/explore-payload-budget` (tests in `__tests__/explore-payload-budget.test.ts`; pushed only if the PR is wanted). Paired A/B, headless Claude Code on Opus, 9 code-discovery tasks × 2 reps × 3 arms = 54 cells, same repo as above, control = the unmodified build:

| arm          | correct | tool calls / cell | $ / cell | cost median vs control (p) |
| ------------ | ------: | ----------------: | -------: | -------------------------: |
| control      | 16 / 18 |               4.1 |    $0.50 |                          — |
| cap 9000     | 17 / 18 |               4.8 |    $0.50 |             −$0.10 (p .82) |
| symbols-only | 14 / 18 |               4.4 |    $0.50 |             −$0.04 (p .50) |

Answers at the 9k cap ran 9.5k–13.4k characters against 16.6k–23.7k unmodified, and the model still made the same calls at the same cost: the payload saving is inside the noise floor at this n. Symbols-only lost two tasks the control answered (both walk-through tasks whose call path leaves the named symbol's file) and saved nothing, which matches what we saw earlier with an outline-shaped tool.

Recommendation from this side: do not lower the tier defaults, and skip a symbols-only mode. The `maxChars` input is cheap and harmless as an opt-in for a caller that knows it wants one symbol; I can open that PR alone if wanted, otherwise this issue can close as measured-and-not-needed.
