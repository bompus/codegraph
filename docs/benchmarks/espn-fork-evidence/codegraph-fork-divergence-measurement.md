# Consolidated fork and upstream PR coverage

Checked 2026-09-10 (America/Denver) after publishing retrieval fix `6f5ae441fe06d48fb79c8b633a30d748f7ea0c46`.

**Yes: the consolidated fork has changes without matching submitted upstream PRs.** It is **166 commits ahead and zero behind** upstream `3ed73bc127323e63153bf6ec8354afa82ce36aaf`. The effective diff is 151 files, 10,173 insertions and 1,022 deletions. There are 112 non-merge commits in the ahead history; neither count is a count of unsubmitted fixes. `origin/main` still exactly matches upstream/main.

## No matching upstream PR found

- **Requested-source retrieval:** [`22ae1678`](https://github.com/bompus/codegraph/commit/22ae167837e174fa623dc77c2c4435ecadbc37d0) and [`6f5ae441`](https://github.com/bompus/codegraph/commit/6f5ae441fe06d48fb79c8b633a30d748f7ea0c46), covering punctuation, declarations, assertions, Vue source, selectors/cache keys and bounded allocation. These changes were pushed to the fork default branch without creating upstream PRs.
- **Managed build identity and compatible backend refresh:** `0d0bd3c7`, `f9ff7b35`, `8169951d`, `0a5301d4`; revision-aware CLI/MCP identity, backend replacement and writer-lock handover.
- **Unindexed worktree hints:** `b84902d8`; identify an indexed sibling while directing indexing to the current checkout.
- **Evaluation harness repairs:** `29ff7057`, `417d4c79`; isolated binaries, propagated failures, validation of result artifacts, and removal of machine-specific/retired-tool wiring.
- **Portable documentation corrections:** `dd3c64dc`, `8efb5313`; current tool/installer guidance. Separate these from fork-specific branch and host instructions before contributing them.

The [literal-seeding foundation and dependent retrieval contribution](codegraph-upstream-contributions-measurement.md) are prepared, validated and published as fork branches based on upstream/main. The foundation is now submitted as draft PR #1841; the dependent retrieval change remains unsubmitted. Port and revalidate the dependent change after the foundation lands. The linked record includes measurements on the actual upstream-based implementations, separate from the earlier consolidated measurements.

## Already submitted or intentionally retained

- Literal seeding is [upstream draft PR #1841](https://github.com/colbymchenry/codegraph/pull/1841), submitted 2026-09-11 UTC and updated to `3b95088b` after fixing all three review findings. It includes native store-worker capture, re-index persistence, exact same-line ownership, rebuild guidance and compiled native/WASM regression coverage.

- Nine bompus PRs remain open: [#1699 Markdown](https://github.com/colbymchenry/codegraph/pull/1699), [#1702 sessions](https://github.com/colbymchenry/codegraph/pull/1702), [#1713 bare imports](https://github.com/colbymchenry/codegraph/pull/1713), [#1717 teardown](https://github.com/colbymchenry/codegraph/pull/1717), [#1753 writer-lock test](https://github.com/colbymchenry/codegraph/pull/1753), [#1764 UI build](https://github.com/colbymchenry/codegraph/pull/1764), [#1786 resolution cleanup](https://github.com/colbymchenry/codegraph/pull/1786), [#1787 daemon cleanup](https://github.com/colbymchenry/codegraph/pull/1787), and [#1737 routing](https://github.com/colbymchenry/codegraph/pull/1737). Routing #1737 is not integrated into consolidated. Related fork fixes may differ from their PR versions; category coverage is not a claim that every hunk is identical.
- The newly integrated Windows atime commit `2977f063` is the exact head of [upstream #1472](https://github.com/colbymchenry/codegraph/pull/1472), submitted by JJordan0C. It is not an unsubmitted bompus change.
- Closed or merged submissions still count as submitted. For example, [#1697](https://github.com/colbymchenry/codegraph/pull/1697) was merged; [#1686](https://github.com/colbymchenry/codegraph/pull/1686) and several older resolver submissions are closed. Rebases, duplicated patches and merges explain part of the raw ahead count.
- Fork branch/default guidance, upstream-main mirror automation, disabled fork Pages deployment and release-branch wiring are deliberate fork configuration. Keep them out of general-purpose upstream PRs.
- Preserve `defddc65`, the revert of #1790, while [#1794](https://github.com/colbymchenry/codegraph/issues/1794) remains unresolved. It protects project-member call chains; it is not a request to restore the reverted change.
- [Fork routing PRs #2–15](https://github.com/bompus/codegraph/pulls) are a separate unintegrated backlog, not changes already deployed from consolidated.

## Evidence and limits

Fetched both upstream and fork refs; checked merge counts and the effective tree diff; listed all 17 upstream PRs authored by bompus across all states; inspected relevant PR bodies/file lists; searched upstream PRs by feature; and queried GitHub's commit-to-PR associations for the candidate changes. The Windows fix demonstrates why an empty association result alone is insufficient: a title search found #1472 and its exact head matched.

This is a feature-level contribution inventory, not a formal hunk-by-hunk equivalence proof for every historical commit. No upstream PR, comment, review, release or npm publication was created by this audit. Follow the [remaining-work plan](PLAN-codegraph-audit-and-fork-improvements.md) for actionable implementation priorities.
