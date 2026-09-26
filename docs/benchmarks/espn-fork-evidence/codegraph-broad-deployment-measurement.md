# Broad retrieval deployment verification

Deployed `245d00d4162e78a11f5a929487c0b8712c88da50` on 2026-09-11 UTC, following the [source validation](codegraph-broad-retrieval-measurement.md). Managed source, detached daily checkout, built revision and the documentation-worktree daemon agree on that revision. The previous `3f1c3309` deployment remains available for rollback.

## Managed gates and promotion

The build, native/WASM test gates and five source probes passed at low priority with a three-worker cap. The final promotion precondition then rejected a daemon still running `d5607a08` against rollback artifacts at `3f1c3309`. After reconciling that daemon, the existing guarded promotion function promoted the unchanged tested staging artifacts under the updater lock at `2026-09-11T06:04:56.643Z`.

Successful temporary test reports had already been cleaned before the promotion exception, so individual deployment test counts were not retained. Gate success is established by the runner reaching promotion only after `stageAndGate` returned success; no exact test-count claim is made for this run. The source revision separately has 138 passing targeted tests on each backend. No worker-limit setting was changed.

## Deployed behavior

Compiled-build checks passed for:

- Preserving SQLite's original constraint error after automatic rollback, rolling back a later failed transaction, and committing a subsequent successful transaction.
- Detecting a committed rename without indexing, syncing it, and preserving symbol identity through two no-op syncs.
- Opening the existing fixture at extraction stamp 27 with complete state, no stale-extraction flag and no changed files. This deployment requires no fixture rebuild.

All 17 fresh MCP probes passed their required evidence checks: the original 16 cases plus the bare identity-caller control. Cases 9/10 return 78/78 receiver/storage lines, case 12 returns all selected Vue ranges, case 13 returns 31/31 assertion lines, and the supplemental identity query returns 29/29 caller lines. Previous complete ranges remain complete. The optional ADP test excerpt remains absent, as recorded in the source measurement. Maximum output was 24,989 JavaScript characters.

## Remaining host reconnect

The CodeGraph-root and fixture daemons were refreshed to `245d00d4`. A first fixture-daemon start overlapped a short-lived fresh probe's writer lock; retrying after the probe ended succeeded without deleting a live lock.

This chat's existing MCP connection still returns the old behavior: case 9 gives 0/78 lines and case 13 gives 23/31. Case 9 remained 0/78 after the fixture daemon was restored. These actual-host results are distinct from the successful fresh connections above.

The [refresh launcher](https://github.com/bompus/codegraph/blob/245d00d4/src/mcp/refresh-launcher.ts) retains its existing child when the initialize contract changes and requests a host reconnect. This revision changes server instructions; the behavior is consistent with that guard. A matching daemon revision alone does not certify this chat's connection. Reconnect its CodeGraph MCP server, then repeat the receiver/assertion controls before closing host verification. No callable host reconnect tool was available during this run.

The [audit log](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/main/docs/agents/codegraph-query-audit.jsonl) contains 20 new receipts under experiment `broad-retrieval-deployment-20260911`: 17 fresh connections and three actual-host checks, including the failed controls. Historical rows are unchanged. Raw responses, summaries and compiled recovery evidence remain locally under `.codegraph/broad-deployment-smoke/` in the contribution documentation worktree. These measurements do not establish cross-platform behavior or organic agent latency improvements.
