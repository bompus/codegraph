# Corrected CodeGraph deployment verification

Measured 2026-09-11 UTC on consolidated `d5607a08671f89da8cf81ae95808da3b84640d37`. This verifies the deployed literal corrections and requested-source retrieval, following the [contribution checks](codegraph-upstream-contributions-measurement.md).

## Deployment gates

The managed deployment completed at `2026-09-11T04:59:07.177Z`: application/native builds, 4,746 passing native tests, 4,744 passing WASM tests and five source probes, with zero failures. Each backend reported 4,756 total tests; skipped tests account for the difference. Managed source, detached daily checkout, built revision and running daemon agreed on `d5607a08`. The previous deployment remains available for rollback.

The approved run used low priority and three workers, with both Vitest worker bounds set. CPU affinity exposed seven of the host's 16 logical CPUs to the runner's half-CPU calculation. Peak RAM was not measured. Passing this run does not establish that eight workers are safe; no permanent two-worker setting was introduced.

## Rebuilt fixture and persistence

The pinned reproduction checkout was rebuilt with extraction stamp 27 and two parse/resolve workers. Its 13 manifest source hashes matched before each fresh MCP query. The rebuilt index contained 611 files, 15,025 nodes and 4,768 literal rows, with no stale extraction flag.

A second unchanged `CodeGraph.indexAll()` preserved all literal rows and their SHA-256 content hash, `b87f3d01d64ce444a53785aff79185c1ea9d716ad55ef9329686a19b073594b9`. This exercises the previously failing unchanged-index path. The fixture watcher was restored on `d5607a08`; its worktree remained clean.

## Retrieval verification

All 16 original queries ran through fresh MCP connections after rebuilding and repeating the index. Maximum output was 24,989 JavaScript characters. Selected evidence remained complete:

| Case | Returned evidence                                                    |
| ---- | -------------------------------------------------------------------- |
| 1    | Snapshot implementation 29/29 lines and assertion 23/23              |
| 3    | Prop wiring 2/2 and selectors 22/22                                  |
| 4    | Cache key 50/50 and evaluator 73/73                                  |
| 5    | Identity assertion present at fixture line 1505; overall range 30/31 |
| 8    | Both ADP mappers, 36/36 and 61/61; optional test range still absent  |
| 11   | Cache key 50/50                                                      |
| 16   | Explicit sender and receiver/storage bodies, 50/50 and 78/78         |

Known omissions remain in broad receiver cases 9/10, Vue allocation case 12 and broad assertion case 13. Returning explicit endpoint bodies does not establish a synthesized event edge. The [remaining-work plan](PLAN-codegraph-audit-and-fork-improvements.md) retains these gaps.

After refreshing the CodeGraph-root daemon, this chat's actual MCP connection independently repeated cases 1 and 4 with complete coverage shown above. Both the root and fixture daemons reported `d5607a08`. Host verification is complete; a reconnect is no longer a deployment blocker.

The [audit log](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/main/docs/agents/codegraph-query-audit.jsonl) contains 18 new v4 receipts under experiment `corrected-literal-deployment-20260911`: 16 `fresh-mcp-after-repeat-index` and two `host-mcp` calls. Historical rows remain unchanged. Raw responses and index diagnostics are retained locally under `.codegraph/corrected-deployment-smoke/` in the contribution documentation worktree. These are deterministic source checks, not an agent A/B, cross-platform validation or a latency-improvement measurement.
