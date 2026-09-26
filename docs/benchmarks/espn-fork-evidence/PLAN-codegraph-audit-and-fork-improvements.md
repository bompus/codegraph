# CodeGraph audit and fork improvements

**New session:** this file is the board. Do not invent work. Only unchecked items are in play. Daily CodeGraph (`~/.local/share/bompus-espn-draft/codegraph-daily`) was serving `f82e4898` on 2026-09-12, a pre-kernel build; consolidated is at `d585de41` (see item 7). The `codegraph-fork-sync` tooling in setupLinuxHost is kernel-only as of `54a2ba8` on `main` (WASM arm and wasm copy step removed, adapters restored), so the next `merge` can build the kernel fork; verify the serving revision before comparing results. Do not run `codegraph-fork-sync merge` unless asked.

Status: Consolidated is at `d585de41` (2026-09-12). On top of `34cc0f64` (sessions hosts + CLI id prefix), the TTD window-message edge `e349f6bd` and retrieval `245d00d4` / `61f73f20`, the fork landed the kernel-only extraction chain (fork PRs #16–#26: golden-dump gate, native error recovery, SFC blocks through the kernel, parse-tree service, tail grammars compiled in, WebAssembly parser removed in `de37daee`) and the resolution binding model (#27–#38: TS/JS, Python, Go, Java/Kotlin, PHP and C/C++ binding rows replace the resolver's import, sealed-module and local-binding regexes). The 16-case source-evidence cohort, including cases 9, 10, 12 and 13, is complete ([measurement](codegraph-remaining-retrieval-measurement.md)) against the pre-kernel engine. PR #1841 is ready for upstream review at `3b95088b`; dependent branch `contribute/requested-source` includes the retrieval follow-up at `32791f0f` and stays unsubmitted until #1841 lands.

Audit log note (2026-09-16): no new `codegraph-query-audit.jsonl` receipts since `a92993b4` (2026-09-14 12:52 MDT); verified benign — intervening work was war-room/watch/sync with no discovery queries, writer `--report` healthy, all checkouts at 759 rows.

- [x] 1. Close the remaining retrieval source gaps — gate: broad TTD receiver queries, remaining Vue cells and broad assertion queries return sufficient evidence. Event-edge synthesis is not claimed.
- [x] 2. Deploy the newly merged transaction and committed-file freshness fixes through managed gates when selected.
- [x] 3. Complete index freshness validation — confirm rename/no-op controls and recovery preserve the last usable index; do not reimplement the merged detection fixes.
- [ ] 4. Respond to upstream review of PR #1841, then submit the dependent retrieval fixes after it lands. Do not open `contribute/requested-source` before that.
- [x] 5. Next coverage slice: TTD publisher-to-listener window-message edge — implemented on `e349f6bd` with discriminator pairing and negative controls. Further coverage still needs a concrete unanswered query.
- [x] 6. Multi-host `codegraph_sessions` — Claude, Codex, Cursor/T3, OpenCode, and AGY (AGY only with a `file://` workspace URI). Hits are `claude:` / `codex:` / `cursor:` / `opencode:` / `agy:`. `"sessions": false` opts the whole feature off. ESPN `"queryAudit"` in `codegraph.json` (absent = on) is a separate receipt log; do not index that JSONL into `sessions.db`.
- [ ] 7. Rust kernel and binding model (owned by the "Rust Rewrite for Speed and Safety" session, worktree `t3code/recreate-codegraph-in-rust`). Landed through `d585de41` (2026-09-12): binding-model Phase 3 is done (#35 Java/Kotlin, #36 PHP, #37 C/C++ rows; every per-language import regex is deleted; #38 records the Windows validation). Remaining candidates there: the Phase 2b receiver rule, kernel-side binding lookup (Phase 4), and release prep; #39 refreshes the Unreleased highlights and #40 adds `docs/design/metrics-ledger.md` with phase measurements and their limits; fork PR numbers continue from #41. Resolution changes are gated by `npm run eval:precision -- <corpus>` on the pinned corpora. Do not start a parallel port. The changes touch indexing and resolution only; the MCP transport and `codegraph_sessions` are unchanged. Follow-ups for this board: re-index the ESPN checkout (extraction stamp moved 28 → 31 and schema v11 adds a bindings table, so the stamp-27 reproduction index re-indexes on next open), replay the pinned 16 queries plus the routing and Markdown controls against the kernel engine, and rerun the managed identity check on the serving build. `contribute/requested-source` (`32791f0f`) does not touch the resolver files the fork rewrote, so it needs no porting onto the bindings table.

This is a remaining-work board. Completed tooling and retrieval fixes are evidence below; git history retains the earlier research.

## Verified baseline

Checked local source and fetched remote refs on **2026-09-10**:

- **CodeGraph published release** — [v1.6.0](https://github.com/colbymchenry/codegraph/releases/tag/v1.6.0), published 2026-08-26.

- **Fork default branch** — `origin/fork/consolidated` includes the above through `e349f6bd`, plus multi-host session search through `34cc0f64`, plus the kernel-only extraction and binding-model chain through `d585de41` (2026-09-12). The native Rust kernel under `codegraph-kernel/` is now the only parser; a source checkout needs a prebuild, which `scripts/ensure-kernel.mjs` builds with a Rust toolchain when missing, and the whole-graph golden dumps are the extraction regression gate. Design and status: CodeGraph's `docs/design/kernel-only-extraction-plan.md` and `docs/design/resolution-binding-model-plan.md`. The [PR coverage inventory](codegraph-fork-divergence-measurement.md) is a historical snapshot separating unsubmitted features, submitted work and intentional fork configuration.

- **Transaction recovery** — `c8343e1d` fixes the reproduced failure reported by [PR #1725](https://github.com/colbymchenry/codegraph/pull/1725): SQLite automatic rollback masked the constraint error and left a later failed transaction's row committed. The adapter now preserves the original error and resets depth in `finally`. Six regression tests cover automatic rollback, subsequent atomicity, normal callback failure, nested work/failure and deferred commit failure. All 13 tests across three SQLite suites passed on Node 24 with one worker; the agent-document check and diff check passed. No full-suite or deployment claim for this commit.

- **Managed deployment** — See [broad deployment](codegraph-broad-deployment-measurement.md) and [corrected deployment](codegraph-corrected-deployment-measurement.md) for the verified heads, test counts, and MCP probes. No release or npm publication was performed.

- **Index and audit evidence** — The reproduction checkout is rebuilt at extraction stamp 27, which is now behind the kernel engine's stamp 31 (raised by `cba92567`, `4fc1f23f` and `91902770`); treat that index as pre-kernel evidence and rebuild before any new comparison (item 7). All 4,768 literal rows and their content hash survived an unchanged full index. All 16 original queries were replayed through fresh MCP connections; the actual host connection also returned complete snapshot assertions and cache-key/evaluator bodies. The 18 new v4 receipts use experiment `corrected-literal-deployment-20260911`. Exclude investigative experiments from organic productivity statistics and preserve historical rows.

The released version, consolidated engine and unmerged PR heads are distinct baselines. PR validation comments below are contributor reports, not tests rerun during this documentation review.

For another evaluation, rerun the audit report and managed identity check from the intended ESPN checkout. Follow [agent-tooling.md](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/main/docs/agents/agent-tooling.md#codegraph-worktrees) for checkout-local setup and explicit `projectPath`; verify the serving revision and index extraction stamp before comparing results. Matching source/build hashes alone do not establish either.

## 1. Retrieval: remaining source and ranking gaps

The [original pinned reproduction](codegraph-retrieval-repro-measurement.md) established 16 cases. The first implementation is complete in [CodeGraph `22ae1678`](https://github.com/bompus/codegraph/commit/22ae167837e174fa623dc77c2c4435ecadbc37d0): path/symbol punctuation, named declaration priority, matching test/Vue source regions, complete requested bodies during trimming, and summary-size accounting.

The [upstream / consolidated / candidate comparison](https://github.com/bompus/codegraph/blob/22ae167837e174fa623dc77c2c4435ecadbc37d0/docs/benchmarks/explore-requested-evidence.md) records 341 passing targeted tests and unchanged complete source controls. Snapshot assertions, the draft identity assertion, the full named interface, consensus-evaluation and stamping bodies, the exact Vue column/CSS control, and explicit TTD endpoint bodies now return. These are completed source fixes, not new backlog items.

The [selector/cache-key follow-up](https://github.com/bompus/codegraph/blob/6f5ae441fe06d48fb79c8b633a30d748f7ea0c46/docs/benchmarks/explore-selector-cache-evidence.md) closes cases 3, 4 and 11: prop wiring 2/2, selectors 22/22, cache key 50/50 and evaluator 73/73; the broad hover query also returns the key 50/50. All other fixture ranges retain their prior completeness. The change passed 344 targeted tests and fresh MCP smoke checks; remove these omissions from the implementation backlog.

Completed on 2026-09-11 against consolidated `61f73f20` using the pinned 16 queries and the existing stamp-27 index ([measurement](codegraph-remaining-retrieval-measurement.md)):

- **Broad identity-caller ranking.** Bare `espnPlayerKey` now returns `pickHistoryTracker` along with the definition and `consensus-pool-assembly` caller.
- **Cases 9 and 10.** Both original TTD prose/event queries return the receiver/storage range at 78/78.
- **Case 12.** The broad layout query returns the survival column, tag, ADP/FP cells and CSS ranges.
- **Case 13.** The broad path-and-function query returns the identity assertion (31/31), matching case 5.

- **TTD window-message edge (item 5).** Consolidated `e349f6bd` synthesizes `publishTtdLivewire → listenForTtdLivewireAjax` when both sides share a distinctive `source` discriminator (`TTD_AJAX_EVENT`). Existing calls still complete receive → persist. Fixture tests cover a matching pair, a different constant, and a listener with no check. A throwaway index of the pinned interceptor/receiver/constants files showed that edge and an explore path through the four named endpoints. The stamp-27 reproduction checkout was not rebuilt; a live project needs a re-index before an existing graph shows the hop. Pairing is not on the generic DOM event name `message`.

The metadata-producer/caller report `20260909.c46-recap-identity.astra.1` and the six unpublished receipts' watcher/streaming reports were outside this selected cohort. They remain unverified follow-up evidence; replay them if the next candidate touches those paths. The ADP mapper answer and named `normalizeName` body remain passing controls, not active defects.

For another candidate, keep these exact queries and source hashes, test source/caller/assertion completeness, and retain path/literal, Markdown/link, flow and allocation controls. The first candidate's 16-case comparison held extraction fixed; independently rebuilt indexes and organic agent sufficiency were not compared. Paid agent A/B remains a separate decision: use the existing harness, eight paired rounds unless the user changes scope, rotated arm order, spread and medians, and the repository's evaluation guidance.

Historical scope caveats remain: the nine `codex-fresh-room-586950276-rootcause-*` attempts intentionally queried a primary index from another editing worktree; the nested-index proof was not a wrong-checkout response; the nonexistent guessed path was discovery, not omission. Preserve historical receipts and unpublished additions, and exclude investigative experiments from organic productivity summaries using the existing [audit contract](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/main/docs/agents/agent-tooling.md#codegraph-query-audit).

## 2. Deployment follow-up

Completed: managed runtime verification is complete for the merged chain (`d5607a08` baseline, `245d00d4` candidate) with the broad fixture set (stamp 27) and corrected retrieval checks. Preserve the existing worker-limit decision and shared-host load checks. Any remaining deployment action should only be to repeat in this chat session’s host if stale cached MCP state is suspected.

## 3. Index freshness and recovery

Completed: CLI stale-extraction guard (`b9d5535e`) and committed-file detection (`6cb53e82`) regressions are in place and covered in broad verification. This includes rename/no-op and transaction rollback/commit scenarios; do not reproduce the pre-fix defect as if the fix were absent. Use Windows-variant validation before making lifecycle changes.

Start with truthful detection and a bounded recovery behavior. [#1798's full background-rebuild proposal](https://github.com/colbymchenry/codegraph/issues/1798) remains a later design decision: it needs one rebuild across concurrent clients, a usable old index while rebuilding, catch-up of intervening edits, failure recovery and safe Windows/POSIX handover. The fork's compatible backend hot refresh does not itself regenerate stale stored extraction.

Any lifecycle change must cover interruption, repeated no-op sync, and preservation of the last usable graph. Use CodeGraph's repository guidance for platform validation; a passing Linux check does not certify Windows file locking.

## 4. Upstream contributions and fork maintenance

The [two prepared contributions](codegraph-upstream-contributions-measurement.md) are published and validated: corrected literal-seeding foundation `3b95088b` is [upstream PR #1841](https://github.com/colbymchenry/codegraph/pull/1841), now ready for review; dependent branch `contribute/requested-source` is `32791f0f` and remains unsubmitted. As of 2026-09-12 #1841 remains open at `3b95088b` with no reviews or comments. After the foundation lands, rebase this dependent branch onto the new upstream head and rerun affected checks.

The [divergence audit](codegraph-fork-divergence-measurement.md) also found unsubmitted backend refresh/revision identity, sibling-worktree hints and harness repairs. Those are separate contributions. Fork branch/Pages/release wiring stays fork-only.

On 2026-09-10, all **nine** open upstream PRs by bompus are `MERGEABLE`, with `REVIEW_REQUIRED`, no reviews and an empty status-check rollup. Their `BLOCKED` label is not a conflict or a failed-CI diagnosis.

- **[#1699 Markdown](https://github.com/colbymchenry/codegraph/pull/1699), head `cfecb697`** — Rebase completed 2026-09-10 against `3ed73bc`; await review. Its comment reports native initializer fixes, but the fork already has separate Markdown-reference deduplication. Compare behavior/parity before importing any delta; a different patch does not prove a missing fork fix.

- **[#1702 sessions](https://github.com/colbymchenry/codegraph/pull/1702), head `f78c217b`; [#1713 bare imports](https://github.com/colbymchenry/codegraph/pull/1713), head `71033cc0`** — Rebases completed 2026-09-09 against `3ed73bc`; await review or new actionable feedback. #1702 remains Claude-only.

- **[#1717 Windows teardown](https://github.com/colbymchenry/codegraph/pull/1717); [#1753 writer lock](https://github.com/colbymchenry/codegraph/pull/1753); [#1764 UI build](https://github.com/colbymchenry/codegraph/pull/1764); [#1787 daemon cleanup](https://github.com/colbymchenry/codegraph/pull/1787)** — Fork work is complete: #1717's exact head is an ancestor; the other three are patch-equivalent in consolidated. They are still upstream contributions, not duplicates to close merely because our fork contains them.

- **[#1786 resolution cleanup](https://github.com/colbymchenry/codegraph/pull/1786)** — Consolidated already removes every temp project and closes the relevant handles, using `finally` cleanup plus `11f087f8`. Do not reimplement the fix; reconcile the differing patch only if needed for upstream review. No new Windows run was performed here.

- **[#1737 static HTTP / Nuxt routes](https://github.com/colbymchenry/codegraph/pull/1737), head `d256fd03`** — Not integrated in consolidated. Its last validation report is 2026-09-08 against `43271f3` (59 targeted tests). Before integration, validate against the current base and the repository's required real-repo routing probes; keep it ahead of new framework expansion.

Keep the fork's `defddc65` revert of #1790 while [#1794](https://github.com/colbymchenry/codegraph/issues/1794) remains unresolved; it is still an ancestor of `d585de41`. Preserve that revert when resolving conflicts with rebased upstream branches, which still inherit #1790. The binding model (item 7) is the intended replacement: it must retain project-member call chains while rejecting unsupported built-in edges, and with WASM removed there is only native extraction to validate.

The fork's [routing PRs #2–15](https://github.com/bompus/codegraph/pulls) remain open; none of their exact heads is an ancestor of consolidated. #2 conflicts with consolidated; #3–15 are clean only against their stacked parents. Treat this as an existing backlog, not an automatic integration queue. Select a slice only for a measured route miss, and read CodeGraph's `docs/design/framework-coverage.md` before changing coverage.

## 5. Conditional next coverage decisions

The selected TTD window-message slice is done on `e349f6bd`. Multi-host session search is done on `34cc0f64` (item 6) — do not start another reader. Remaining ideas still need a concrete unanswered query:

- **Shared export facts, [#1721](https://github.com/colbymchenry/codegraph/issues/1721)** — Being closed by the binding model (item 7): extraction now emits a bindings table and TS/JS and Python resolution read it instead of source regexes. The upstream issue is still open; do not scope a separate export-fact change here. Native/WASM parity is no longer a consideration because the WASM parser is gone.

- **Additional routing coverage** — Use the existing #1737 / #2–15 work only when a concrete query requires it, with end-to-end evidence and the required small/medium/large repo validation.

- **Further Markdown or ranking work** — Require an existing named section, linked proof or literal that the controlled cohort fails to return. Markdown indexing and exact-literal seeding already exist; do not plan them as new capabilities.

Performance tuning is not an active task without a measured bottleneck. The retained work above should determine the next implementation; the old session-relay request is not a prerequisite.
