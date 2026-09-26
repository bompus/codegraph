# CodeGraph retrieval omission reproduction

Measured 2026-09-10 (America/Denver; retained query timestamps are 2026-09-11 UTC). **Requested evidence still disappears from fresh-index answers.** Prioritize source selection and rendering: preserve a requested assertion or Vue region before adding unrelated context. No engine change was made or evaluated.

## Pinned baseline

- ESPN source: `c570e32b22177ab59d3fa8f4c162419bac196422`.
- CodeGraph source, managed daily checkout, build and serving runtime: `823e75501088e707490bdc2b9a041ef1beb8a554`, package `1.6.0+823e75501088e707490bdc2b9a041ef1beb8a554`.
- Explicit MCP `projectPath`: `/home/bompus/bompus-espn-draft-codegraph-repro`, isolated branch `test/codegraph-retrieval-repro`.
- Checkout-local index: extraction stamp 26/current 26, complete, no pending references, no worktree mismatch, no reindex recommendation. Initial status reported 610 indexed files and zero pending file changes; explore counts 496 code files. The plan's status text was the only tracked edit during retained queries; the product source matched the pinned commit.
- Setup used the existing `bun scripts/codegraph-worktree.ts`, retaining the repository's human-only `notes/` exclusions. Runtime identity was checked with the watcher active. The checker can return `running: null` after idle shutdown; that later observation does not establish a different query-time engine.

The [case data](codegraph-retrieval-repro.json) contains 16 exact queries, timestamps, measured latency/response size, response hashes, returned filenames, and expected source ranges with file hashes. These are 12 historical queries plus four focused controls. Expected source fixtures are the files at the pinned ESPN commit, not today's moving main branch.

## Reproduced omissions

Counts below are returned / expected **nonblank source lines** within the stated range. They measure evidence completeness, not whether an agent could guess an answer from another fragment.

- **Case 1: snapshot assertion cut mid-expression.** `recommendedPickSnapshot` is complete (29/29), but `test/recommended-pick.test.js:113–135` returns only 4/23 lines. The expected fields and second row are missing despite the test file being selected. This is a source selection/rendering failure, not a missing test relationship.
- **Cases 5 and 13: explicit test path, then missing assertion.** The historical `draftPlayer` query omits the indexed `test/draft-store.test.js` altogether. The focused query restores that filename but still returns 0/31 lines of the test at 1479–1511, including the identity assertion at 1505: `expect(draftSessionStore.draftState.recentPicks[0].playerId).toBe("4430871");`. Path selection and assertion preservation are separate failures.
- **Cases 3, 12 and 15: Vue regions disappear.** The historical layout query returns only DraftTable lines 1–4. The exact-path/literal control returns some script code, but neither the `col-w-survival` column at line 19 nor its CSS at 790–792. The graph contains a DraftTable component spanning 1–812; its separate file-kind node spans the script region, 392–763. This rules out absence of the whole component from the index, but does not yet isolate the engine routine responsible for region selection. Case 3 also omits PopoutApp's actual recommendation prop wiring at 617–618 and DraftTable selectors at 497–520. The current table consumes supplied props; the historical query's assumption of an independent drafted-name filter is not established.
- **Cases 4, 6 and 11: named definitions lose essential regions.** Case 4 returns 2/50 cache-key lines and 50/73 `evaluateConsensus` lines. Case 6 returns later code from `shared/consensus-engine.ts` but none of its indexed `ConsensusPlayer` interface at 57–188. Case 11 includes the pipeline filename but none of the cache key. The definitions exist; discovery of the filename is insufficient.
- **Cases 7 and 14: stamping body is recoverable.** The historical query returns only 12/33 lines of `applyTtdPositionPriority`, ending before the loop that claims one representative per position. A focused path-and-symbol query returns all 33 lines and caller source. Retain this control when changing output selection; requiring every agent to learn a different query is not the proposed solution.

The evidence establishes missing files versus missing regions of selected files. It does **not** yet prove whether each latter omission originates in ranking within a file, excerpt construction, per-file budget allocation or final clipping. Diagnose that code path before choosing an implementation. Several responses approach 25,000 characters; larger output alone is not an acceptance criterion.

## TTD event flow: two distinct gaps

Historical cases 9 and 10 omit `content/subvert-autofill.ts`, although it is indexed and contains the receiver and persistence functions. Case 16 explicitly names the four endpoints and restores all 78 nonblank lines of receiver/persistence evidence at 559–639. Its Flow section correctly connects `listenForTtdLivewireAjax` → `receiveTtdSnapshot` → `persistTtdPositionPriority`.

The sender remains incomplete: `publishTtdLivewire` returns 35/50 source lines and stops at line 233, before the discriminator and payload. Source inspection establishes the boundary: `entrypoints/ttd-interceptor.content.ts:232–243` posts a message whose `source` is `DraftHubConstants.MESSAGES.TTD_AJAX_EVENT`; `content/subvert-autofill.ts:619–637` registers the window message listener, checks that same discriminator and calls `receiveTtdSnapshot`. Persistence validates weights and writes `bompus_ttd_position_priority` at line 575.

A read-only query of edges among those four indexed endpoint nodes found the two receiver-side `calls` edges and no publisher-to-listener edge. That is a graph coverage gap in addition to retrieval omission. This check does not assert that every possible indirect path in the graph was searched. A future event synthesizer needs end-to-end wiring evidence and negative controls; do not invent a link merely because two names appear together.

## Controls and limits

- **Case 2 passes:** the named `normalizeName` body at `shared/espn-player-key.ts:43–51` is complete (9/9). Remove this historical omission from active work. Source and query timing differ from the original session, so this is not evidence of an intervening engine improvement.
- **Case 8 remains helpful:** both ADP mapper bodies are complete (36/36 and 61/61). Its nearby regression test is absent (0/20), but the original question asked about source behavior. Retain test inclusion as a secondary target without relabeling the historical helpful result.
- **Case 14 passes** for the complete priority-stamping body. **Case 16 is a partial call-chain control**, not a passing sender-to-storage flow.
- No upstream build, candidate build, control repository or paid agent A/B was run. These selected cases cannot establish a general accuracy rate, speedup, token saving or regression since release. Markdown/link precision still needs a control before a retrieval fix is accepted.

## Reproduction and accounting

Use an isolated checkout of the pinned ESPN commit, run its checkout-local setup helper, and verify engine/runtime identity plus extraction stamp. Submit each case's exact `query` to `codegraph_explore` with that checkout's absolute `projectPath`; omit `maxFiles`, as in this measurement. Keep query text unchanged when comparing engines. Use each expected file's SHA-256 to detect fixture drift before comparing the fenced Source section's line numbers.

The existing CodeGraph `scripts/agent-eval/probe-explore.mjs` also reproduced case 15 against the managed build. Run it from the intended engine build directory because it resolves `dist/` relative to cwd:

```bash
fnm exec --using codegraph node /home/bompus/bompus-codegraph/scripts/agent-eval/probe-explore.mjs \
  /home/bompus/bompus-espn-draft-codegraph-repro \
  'src/components/DraftTable.vue "col-w-survival"'
```

That direct ToolHandler probe returned 24,987 characters and the same missing Vue regions. It binds the checkout through `CodeGraph.openSync`; it does not pass a `projectPath` tool argument. Its unrelated built-in React checks are not acceptance tests for this case. Full raw responses are local ignored artifacts under `.codegraph/retrieval-repro/`; the committed queries, hashes and source expectations support a fresh rerun.

The audit experiment is `retrieval-omissions-20260910`: 12 discovery calls, 12 retained identical-query replays, four focused controls and one direct library probe, **29 attempts total**. The initial 12 responses were not retained; their deferred receipts explicitly use reconstruction timestamps and only metrics actually recorded. They are not independent additional cases, and replay response hashes must not be attributed to them. The direct probe likewise has no retained start timestamp or latency measurement.

All receipts were appended through the existing v4 writer. Directory and revision stamps describe append-time state. Per-receipt saved steps are zero; cost added counts the single known retrieval. Fallback entries group shared source/index verification or the recovery replay, rather than claiming exact per-attempt shell-operation accounting. Exclude this investigative experiment when reporting organic agent productivity. Token usage was not available.

## Next implementation gate

Start with preserving requested assertions and Vue source regions in the existing explore path. Cases 1, 5/13 and 12/15 provide bounded regression targets; keep cases 2, 8 and 14 as source controls. Preserve exact paths/literals, add a Markdown/link control, and check complete source evidence rather than filenames. Treat event-edge coverage as a separate candidate requiring its own supported-flow and negative controls.
