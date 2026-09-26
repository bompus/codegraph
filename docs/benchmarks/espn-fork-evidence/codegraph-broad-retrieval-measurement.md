# Broad retrieval and freshness follow-up

Measured 2026-09-11 UTC. Published CodeGraph commits:

- [`3f1c3309`](https://github.com/bompus/codegraph/commit/3f1c330981ab4df6ab701fa5f2c97ee9da4a1e04): committed rename and repeated no-op sync validation, following another session's committed-file detection fix `6cb53e82`.
- [`245d00d4`](https://github.com/bompus/codegraph/commit/245d00d4162e78a11f5a929487c0b8712c88da50): remaining pinned retrieval source fixes and direct-caller ranking.

## Freshness validation

All 41 sync tests passed. The added regression commits a rename without indexing, verifies status reports the old/new paths while retaining the previous graph, syncs the rename, and verifies two no-op syncs preserve the renamed symbol's identity. Existing added/modified/deleted-file checks also passed. This does not certify crash recovery or background rebuilding across processes and operating systems.

## Retrieval changes and evidence

Compound identifier queries now retain bounded vocabulary matches when no explicit callable was named. Source allocation preserves the matching reader/writer and already-gathered local callers, bounded to two caller hops, eight nodes per selected file and bodies of at most 200 lines. Plain-prose ranking and named-callable ranking retain their previous behavior. A bare, unambiguous function query prioritizes its direct callers after its definition, within the existing candidate and output limits.

Requested test files prioritize matching test titles and complete blocks before individual setup statements. Requested Vue template/style regions receive source reservations when no supporting declaration already needs that allocation. File and character ceilings remain unchanged; no graph edges are synthesized.

The final source revision was replayed against the same stamp-27 fixture, with manifest source hashes checked before each query. Calls used the real source `ToolHandler` under Node 24/Vitest, opening the existing index with syncing disabled. They were not calls through the managed MCP daemon.

| Case                                                                 | Baseline                  | Verified `245d00d4`       |
| -------------------------------------------------------------------- | ------------------------- | ------------------------- |
| 9: broad TTD receiver/storage query                                  | 0/78 lines                | 78/78                     |
| 10: event/context/storage query                                      | 0/78                      | 78/78                     |
| 12: Vue column, survival, ADP, FP and CSS ranges                     | 0/1, 6/6, 0/2, 0/1, 12/12 | 1/1, 6/6, 2/2, 1/1, 12/12 |
| 13: broad draftPlayer test query                                     | 23/31                     | 31/31                     |
| Supplemental `espnPlayerKey` query: pickHistoryTracker lines 113–143 | 0/29                      | 29/29                     |

All 16 original cases were replayed on the committed revision. Every previously complete range remained complete; case 5 also improved from 30/31 to 31/31. Both ADP mapper bodies remain complete, while their previously classified optional 20-line test excerpt remains absent. Maximum response size was 24,989 JavaScript characters. The supplemental identity query returned both expected caller bodies in 23,730 characters; it does not promise every caller in one answer.

## Checks and limits

- 138 tests across nine suites passed with native extraction, then with WASM extraction, sequentially at low priority and one worker. Coverage includes context ranking, vocabulary seeds, named-symbol rendering, literal seeds, source allocation, output ceilings and the generated-Go allocation control.
- TypeScript checking, the agent-document size check and diff whitespace checks passed.
- New test-block, vocabulary-retention and caller-ranking regressions were seen failing on the old implementation. Intermediate candidates that lost snapshot, cache-key, ADP or Go allocation controls were rejected and narrowed before publication.
- No full build, managed deployment, cross-platform run or paid agent A/B was performed for these changes. The last verified managed deployment remains `d5607a08`.

The [audit log](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/main/docs/agents/codegraph-query-audit.jsonl) contains 80 new receipts under experiment `broad-retrieval-followup-20260911`, including intermediate candidates and the final `verified-245d00d4` / `verified-caller-245d00d4` runs. Historical rows remain byte-for-byte unchanged. Receipt identity fields describe the installed runtime; their experiment and improvement fields distinguish the source candidate. Final receipts explicitly identify commit `245d00d4162e78a11f5a929487c0b8712c88da50`. Raw responses, diagnostics and summaries remain locally under `.codegraph/broad-retrieval-followup/` in the contribution documentation worktree.

The separate publisher-to-listener event edge, older reports outside this cohort, deployment and upstream contribution follow-through remain in the [plan](PLAN-codegraph-audit-and-fork-improvements.md).
