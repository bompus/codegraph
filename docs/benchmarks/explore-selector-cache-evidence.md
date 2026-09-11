# Selector and cache-key source retrieval

Measured 2026-09-10 (America/Denver), following the [first source-evidence fix](explore-requested-evidence.md). The candidate fixes cases 3, 4 and 11 without changing extraction, graph edges or the 25,000-character response limit.

## Cause and change

The missing selectors and cache key already had indexed declarations. A requested file's source selection ignored matching declarations outside the gathered symbols. File allocation could not afford a named body together with its strongest supporting declaration, and an earlier cluster could consume the space needed by an equally relevant later body.

Requested files now score their indexed declarations using the existing query terms and literal weights. Exact named bodies retain priority; exact template matches retain their wiring evidence. Allocation reserves the named bodies, strongest supporting declarations (including ties), and strongest template/style region before distributing the remaining pool by relevance. Unaffordable minimums share the same pool proportionally. Cluster selection also reserves space for equally relevant later bodies before adding neighbouring filler. Duplicate indexed ranges are not charged twice.

The existing guards for stale source, file admission, oversized containers, flow spines, per-file maximum share and total response size remain in force. Source matching supplies evidence, not new graph relationships.

## Fixed-source comparison

The exact 16 queries and 13 source-file hashes are from the [ESPN fixture](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/74fcd4e147020d834a3110ac161cb1f6d10cf4b2/docs/agents/codegraph-retrieval-repro.json), pinned to ESPN source `c570e32b22177ab59d3fa8f4c162419bac196422`. Both engines used the same existing index and a fresh ToolHandler per call. Baseline results are the retained `22ae167837e174fa623dc77c2c4435ecadbc37d0` comparison; candidate results were rerun with each referenced source hash checked. This compares source completeness, not agent answer quality or independently rebuilt indexes.

| Case | Evidence | 22ae1678 | Candidate |
| --- | --- | --- | --- |
| 3 | Popout prop wiring | 2/2 | 2/2 |
| 3 | DraftTable selectors | 0/22 | 22/22 |
| 4 | Cache key | 0/50 | 50/50 |
| 4 | Consensus evaluator | 73/73 | 73/73 |
| 11 | Cache key from broad hover/invalidation query | 0/50 | 50/50 |

All other fixture ranges retain their baseline completeness. The snapshot assertion, draft identity assertion, named interface, stamping body, ADP mapper bodies, exact Vue column/CSS, and explicit TTD endpoint bodies remain passing controls. Cases 9/10 still omit the broad-query receiver; case 12 still omits the column declaration and ADP/FP cells; case 13 still omits the identity assertion. No sender-to-listener edge was added.

The candidate's maximum response was 24,989 characters; all 16 responses totalled 392,850 characters. Single-pass median latency was 197.5 ms, range 69–268 ms. The earlier baseline was 202 ms, range 65–258 ms. Runs were not interleaved: these timings do not establish a speedup or regression.

## Validation

- TypeScript compilation and `npm run check:agent-docs` passed.
- 344 tests across 28 retrieval files passed, including allocation/displacement, path/literal, Markdown, named-body, deduplication and source-evidence controls. New assertions cover requested source minimums, insufficient shared capacity and nonfinite demand.
- The fixed-source cases caught intermediate candidates that displaced the evaluator, prop wiring or endpoint controls; those candidates were not integrated.
- Removing an experimental change to the cluster overshoot rule preserved all final evidence results, so that change was removed. The existing overshoot rule is unchanged.

The targeted command is recorded in the [first report](explore-requested-evidence.md#validation-and-reproduction). Direct probes bind the fixture through `CodeGraph.openSync`; they are not deployed MCP or paid agent A/B measurements. Managed deployment checks and the remaining-work board belong to the ESPN repository.
