# Remaining retrieval cohort on consolidated HEAD

Measured 2026-09-11 (America/Denver) against CodeGraph `61f73f20` (includes `245d00d4`) and the pinned ESPN reproduction checkout `74fcd4e1` (product source still `c570e32b`). Fixture hashes for `content/subvert-autofill.ts`, `src/components/DraftTable.vue`, and `test/draft-store.test.js` matched the original 16-case manifest. Queries ran through `ToolHandler` / `CodeGraph.openSync` on the existing stamp-27 index; this is not a rebuilt-index comparison, MCP host check, or agent A/B.

All 16 original queries returned complete expected nonblank source lines except case 8's optional nearby test (0/20), which remains a documented non-goal. Cases 9, 10, 12, and 13 are complete:

| Case | Evidence                                                        | Returned              |
| ---- | --------------------------------------------------------------- | --------------------- |
| 9    | TTD receiver/storage, prose query                               | 78/78                 |
| 10   | TTD receiver/storage, event query                               | 78/78                 |
| 12   | Vue column, survival tag, ADP/FP cells, CSS                     | 1/1 6/6 2/2 1/1 12/12 |
| 13   | Broad `test/draft-store.test.js draftPlayer` identity assertion | 31/31                 |

Largest response in this pass was 24,989 JavaScript characters. The bare `espnPlayerKey` ranking query now includes `src/utils/pickHistoryTracker.ts` as well as the definition and `consensus-pool-assembly` caller.

No publisher-to-listener graph edge existed among the four TTD endpoints at this retrieval measurement. Indexed `calls` edges were `listenForTtdLivewireAjax → receiveTtdSnapshot → persistTtdPositionPriority` only.

A later coverage slice on CodeGraph `e349f6bd` synthesizes `publishTtdLivewire → listenForTtdLivewireAjax` from the shared `TTD_AJAX_EVENT` discriminator. That was checked on a throwaway index of the pinned interceptor, receiver, and constants files, not by rebuilding this stamp-27 checkout.

The dependent upstream branch `contribute/requested-source` now includes the ported follow-up `32791f0f`. Do not open its PR until [PR #1841](https://github.com/colbymchenry/codegraph/pull/1841) lands; then rebase onto the new upstream head and rerun affected checks.
