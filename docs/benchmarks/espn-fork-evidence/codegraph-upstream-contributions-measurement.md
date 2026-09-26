# Prepared CodeGraph upstream contributions

Prepared and validated 2026-09-11 UTC. Both branches are published in the fork. The foundation is submitted as [upstream PR #1841](https://github.com/colbymchenry/codegraph/pull/1841) and has been marked ready for review; the dependent retrieval contribution remains unsubmitted.

## Contribution 1: literal-seeding foundation

Branch: [contribute/literal-seeds](https://github.com/bompus/codegraph/tree/contribute/literal-seeds), head `3b95088b`, based directly on upstream `3ed73bc1`. The diff covers 19 files. It combines literal seeding, native deferred/store-worker capture, exact source-range ownership, re-index backfill, usage/changelog guidance, and an extraction-stamp advance so existing indexes receive a rebuild recommendation. The fixes and stamp 27 are integrated into consolidated `d5607a08`; the managed deployment and fixture rebuild are now [verified](codegraph-corrected-deployment-measurement.md).

Prepared PR title: **feat(explore): find code by literal keys and flags**

Prepared PR body:

> A query naming a storage key such as `draft.pick` currently relies on symbol-name search even when the relevant functions contain that exact string. Capture bounded identifier-shaped literals, persist their enclosing symbols, and use exact matches to seed context and preserve holder files in explore output. This covers WASM extraction, native extraction, and deferred native-buffer decoding.
>
> The schema migration creates the new side table without backfilling source content. Existing indexes receive a rebuild recommendation; full indexing populates the table. Capture is deliberately bounded to identifier-shaped strings, 32 distinct literals per node and 24 query holders. It uses source scanning, so qualifying strings in comments can also match; these are retrieval hints, not inferred graph edges.
>
> Validation on the upstream-based branch: application and native-kernel builds passed; the revised code passed TypeScript compilation, 101 tests across six native suites, 43 tests across four WASM suites, and three compiled-index regressions. The compiled tests cover native-worker, native-main-thread and WASM-worker paths through fresh indexing, unchanged re-indexing, migration backfill, deleted-file cleanup and same-line ownership after non-ASCII source. No full-suite, cross-platform, or agent A/B claim.

## Contribution 2: requested-source retrieval

Branch: [contribute/requested-source](https://github.com/bompus/codegraph/tree/contribute/requested-source), head `b56cd7ee`; implementation `0bf5b98d` now includes the corrected foundation. Its [focused diff](https://github.com/bompus/codegraph/compare/contribute/literal-seeds...contribute/requested-source) covers eight implementation/test/guidance files plus one validation report. Fork Markdown/session handling and deployment configuration are excluded.

Prepared PR title: **fix(explore): preserve requested definitions and supporting source**

Prepared PR body:

> Explore can name the right file while cutting off the requested declaration or assertion, and Vue layout questions can omit the relevant selectors. Preserve punctuated file/symbol references, prioritize exact declarations and matching test/Vue source ranges, and reserve bounded source space for named bodies and supporting declarations such as cache keys. The existing output tiers remain unchanged, and source excerpts do not add graph edges.
>
> Depends on the corrected literal-seeding foundation. The updated stack passed TypeScript compilation, all 346 tests across 28 retrieval suites on each backend, and three compiled-index regressions. The original 16-query fixed-index comparison restored snapshot assertions, selectors, cache keys and other requested definitions without losing expected range coverage; its maximum response was 24,989 JavaScript characters. Those measurements predate the extraction/lifecycle corrections. Broad receiver queries and some Vue/assertion evidence remain incomplete. The [validation report](https://github.com/bompus/codegraph/blob/b56cd7ee/docs/benchmarks/upstream-requested-source-evidence.md) separates revisions, checks and limitations; it makes no agent A/B or latency-improvement claim.

## Submission order and remaining work

Foundation draft PR #1841 targets upstream/main at validated head `3b95088b`. Its three review findings are fixed and covered by compiled regressions. The updated dependent branch passed 346 retrieval tests on each backend and three compiled-index tests. Consolidated integration passed its build, 358 tests on each backend, three compiled-index tests and the agent-document size check. The earlier 16-query measurements remain historical fixed-index evidence. A subsequent [consolidated deployment replay](codegraph-corrected-deployment-measurement.md) verifies the corrected build on a rebuilt fixture index; it does not compare independently rebuilt contribution branches.

PR #1841 is ready for upstream review. The dependent branch is reviewable against the foundation branch; an upstream/main PR would currently include both. After the foundation lands, port the dependent commits onto that upstream head and rerun affected checks before submitting a focused PR. The later [broad retrieval follow-up](codegraph-broad-retrieval-measurement.md), consolidated `245d00d4`, closes the remaining pinned source cases and the identity-caller control but is not yet included in dependent branch `b56cd7ee`.

The [32 audit receipts](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/main/docs/agents/codegraph-query-audit.jsonl) use experiment `upstream-contributions-20260911`, variants `foundation` and `requested-source`. They attribute exact source/build revisions explicitly because these upstream-based binaries do not carry the fork's revision-aware CLI version string. Both variants read the same existing indexed fixture; this comparison does not claim independently rebuilt real-project index parity.

Other unsubmitted features remain in the [divergence inventory](codegraph-fork-divergence-measurement.md). Preparing these two branches does not submit those features or resolve feedback on existing PRs.
