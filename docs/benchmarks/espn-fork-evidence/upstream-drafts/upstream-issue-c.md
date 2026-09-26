Title: README benchmark: what changes when the baseline already has repo rules

Posted 2026-09-04: https://github.com/colbymchenry/codegraph/issues/1700

## Summary

The README's benchmark compares codegraph against Claude Code with an empty MCP config and no instruction files. Most repos that would install codegraph already carry a CLAUDE.md or equivalent, and the one I measured on tells the agent to Grep `-n` first and then Read with `offset`/`limit`. Against that baseline the tool still cuts calls, but by less than the README's percentages, and it costs a little more per task rather than less. Offered as a data point for the README, not a complaint; the tool stays installed here.

## Setup

Headless Claude Code (`claude -p`, Opus, `--effort medium`, `--permission-mode dontAsk`), one fresh session per cell, a private config dir per arm, on a ~500-file TypeScript/Vue Chrome-extension repo. Tasks are code-discovery questions with a graded answer (which function owns X, what calls Y, trace Z across files). Pairs are B − A by task; p is a paired sign test.

| Arm                                            |   n | tool calls / task | cost / task | correct |
| ---------------------------------------------- | --: | ----------------: | ----------: | ------: |
| no rules, no codegraph (the README's baseline) |   6 |              8.00 |       $0.35 |     5/6 |
| no rules + codegraph                           |   6 |              3.67 |       $0.38 |     6/6 |
| repo rules, no codegraph                       |  12 |              5.67 |       $0.51 |   12/12 |
| repo rules + codegraph (eager, no prompt hook) |  12 |              3.33 |       $0.59 |   12/12 |

With rules on, calls drop by a median 2 per task (p .015) and cost rises by a median $0.13 (p .18). The cost goes up because an explore answer averaged ~21.7k characters here against ~4.6k for a bounded Read, so the context each later turn re-reads is larger even though there are fewer turns. Without rules the tool's call saving is bigger (8 → 3.67) because the baseline is worse, which is the README's row; the rules alone already take the baseline from 8 to 5.67.

The same shape held in every framing I tried: with the vendor prompt hook installed, and with the tool deferred behind Claude Code's tool search (#1696). −2 to −2.5 calls per task, +$0.08–0.15 per cell, correctness unchanged, never under p .05 at n = 6.

> **The numbers above are what was posted as [#1700](https://github.com/colbymchenry/codegraph/issues/1700) and are left as posted.** They were scored while `renderReport` paired cells by task, which read one rep per arm and discarded the rest. With the pairing fixed to `task|rep` (2026-09-05) the rules-on rows re-score at **n = 15, 5.13 → 3.33 calls, median −2, p .033** — a wider sample and a weaker p, with the direction, the median and the 12/12 correctness unchanged. Nothing in the argument turns on the difference; the row is corrected here rather than edited above so this file keeps matching the public issue.

## What decided adoption

The model used the tool in 4 of 6 discovery tasks no matter how it was loaded or instructed. The two it skipped anchor on a storage-key string and a CLI flag, and on those a query returns padding (45 and 132 symbols, none of them the answer), so Grep is the right tool there. Prompts that name a concept and no identifier went the other way: 3 of 3 adopted, 3 of 3 correct against 2 of 3 for Grep-and-Read, at 2.3 calls against 6.3. So the trigger that held up in the repo rules is the shape of the question: an identifier, a call path, or a concept → one explore call; a string literal, flag, or regex → Grep.

## In daily use

Over three days of interactive sessions on the same repo (168 transcripts, mostly Fable), the audit counts 29 explore calls against 57 serial Grep-then-Read chains anchored on a code symbol, the chains the tool is meant to replace; on the day the rules were flipped to send markdown questions to it as well, 20 calls in 80 transcripts, about one per chain. So the headless adoption ceiling carries over: the tool replaces roughly half of the chains it could, and the half it misses is habit on string-shaped questions, not loading.

## Suggestion

A README row with a rules-on baseline (a CLAUDE.md that already says "grep, then read a range") would tell a prospective user what to expect on their own repo better than the empty-config row does. Happy to share the harness, the task bank, and the per-cell transcripts.
