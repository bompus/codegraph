# Requested source evidence in explore

Measured 2026-09-10 (America/Denver). This change fixes omissions of requested source without increasing explore's size tiers or adding graph edges. It improves a selected cohort; it does not close every retrieval gap.

## Causes and changes

- A trailing colon prevented a named file from being pinned and a named symbol from receiving source priority. Prose punctuation now leaves those references recognizable. A pinned callback-only test file can also enter through its indexed file node when it has no named definitions; configuration-source guards remain in force.
- A named interface was exempt from declaration damping but did not receive source priority. It also triggered the camel-infix fallback, promoting a calculation method whose name merely contained the interface name. Exact declarations now seed their own bodies and suppress that fallback.
- Test call-site windows stopped inside multiline assertions, while Vue's whole-component envelope was discarded without a replacement for template/CSS evidence. Requested files now supplement indexed ranges with matching test/setup statements and bounded Vue regions. Test statement boundaries come from the existing tree-sitter grammar; this does not change extraction or index contents.
- The final cluster trim could show a requested definition's opening line while cutting off a body that would fit. Complete requested bodies now receive room before unrelated source-order filler. Oversized bodies and flow-spine clusters retain their existing window behavior.
- The result summary was inserted after fitting a shorter placeholder. Reserving its actual maximum size prevents that late insertion from pushing these responses beyond the existing limit.

## Controlled comparison

The 16 queries and expected source ranges come from the [ESPN reproduction fixture](https://github.com/bompus/chrome-ext-bompus-espn-draft/blob/74fcd4e147020d834a3110ac161cb1f6d10cf4b2/docs/agents/codegraph-retrieval-repro.json). All 13 referenced source files were checked against their recorded SHA-256 hashes at ESPN commit `c570e32b22177ab59d3fa8f4c162419bac196422`.

Three engines queried the same checkout-local index, with a fresh ToolHandler per query and no explicit `maxFiles` override:

- Upstream `3ed73bc127323e63153bf6ec8354afa82ce36aaf`.
- Consolidated baseline `823e75501088e707490bdc2b9a041ef1beb8a554`.
- Candidate: the implementation in the commit introducing this report, based on that consolidated baseline.

The source fixture was held fixed; the index also contains the documentation added by the earlier reproduction. Consequently these are fresh baseline comparisons, not comparisons against the original saved response hashes. This holds extraction fixed and compares retrieval/rendering; it does not compare independently rebuilt indexes.

Each cell counts returned / expected nonblank source lines across the fixture's ranges. This is evidence completeness, not an automatic answer-quality score.

| Case | Evidence | Upstream | Baseline | Candidate |
| --- | --- | --- | --- | --- |
| 1 | Snapshot implementation and assertion | 33/52 | 33/52 | 52/52 |
| 2 | Named normalization body | 9/9 | 9/9 | 9/9 |
| 3 | Prop wiring and table selectors | 0/24 | 0/24 | 2/24 |
| 4 | Cache key and consensus evaluation | 52/123 | 52/123 | 73/123 |
| 5 | Draft identity test | 0/31 | 0/31 | 30/31 |
| 6 | Named interface | 0/132 | 0/132 | 132/132 |
| 7 | Priority stamping body | 0/33 | 12/33 | 33/33 |
| 8 | ADP mappers and secondary test | 97/117 | 97/117 | 97/117 |
| 9 | TTD receiver/storage, prose query | 0/78 | 0/78 | 0/78 |
| 10 | TTD receiver/storage, event query | 0/78 | 0/78 | 0/78 |
| 11 | Hover-related cache key | 0/50 | 0/50 | 0/50 |
| 12 | Vue cells and column styles | 0/22 | 0/22 | 18/22 |
| 13 | Draft identity test, broad path control | 0/31 | 0/31 | 23/31 |
| 14 | Focused stamping control | 33/33 | 33/33 | 33/33 |
| 15 | Exact Vue column literal and CSS | 0/4 | 0/4 | 4/4 |
| 16 | Explicit sender/receiver/storage bodies | 113/128 | 113/128 | 128/128 |

The decisive assertions are present in cases 1 and 5, including the identity check at test line 1505. Cases 2, 8 and 14 preserve their previously complete implementation bodies. Case 16 now includes the sender discriminator and payload as well as receiver/storage source, but still has no publisher-to-listener graph edge.

## Remaining gaps

- Cases 3, 4 and 11 still omit table selectors or the cache key. Case 4's evaluation body is now complete; losing the two formerly returned closing lines of the cache-key range does not establish that the key was previously usable.
- Cases 9 and 10 still omit the receiver file unless the query explicitly names its endpoints. Selecting that file and synthesizing the actual message boundary are separate future changes.
- Case 12 returns the survival tag and requested CSS ranges, but still omits the column declaration and ADP/FP cells in that broad query. The exact column-literal control, case 15, is complete.
- Case 13 improves selected test source but still omits the identity assertion. Its broad path-and-function query has less discriminating information than case 5. Do not score it as solved from its filename or partial line count.
- The ADP question remains helpful without the secondary test; no test-inclusion improvement is claimed for case 8.

## Validation and reproduction

TypeScript compilation and `npm run check:agent-docs` passed. The targeted retrieval run passed **341 tests across 28 files**, covering allocation/displacement, oversized members, named definitions, declaration damping, path/literal precision, Markdown sections, result counts, session dedup and the new source-evidence fixtures. The new test-file and Vue gates were also run against upstream and failed on the original omissions. Tests used Node 24 through `fnm exec --using codegraph`, with at most two workers.

```bash
fnm exec --using codegraph npx vitest run __tests__/explore- \
  __tests__/adaptive-explore-sizing.test.ts __tests__/literal-seeds.test.ts \
  __tests__/query-paths.test.ts --maxWorkers=2 --minWorkers=2
```

For a source probe, build the intended engine's TypeScript and copy its assets, then run the existing harness **from that engine's build directory**:

```bash
fnm exec --using codegraph node scripts/agent-eval/probe-explore.mjs \
  /home/bompus/bompus-espn-draft-codegraph-repro \
  'src/components/DraftTable.vue "col-w-survival"'
```

Use the fixture's exact queries for the other cases. Match numbered lines inside each file's fenced Source section, not occurrences in the blast-radius header or omitted-symbol pointers. Recheck fixture hashes first. The direct harness binds the project through `CodeGraph.openSync`; it is not a deployed MCP or agent A/B run.

The candidate's largest response in this pass was **24,995 characters**; the baseline and upstream each reached 25,023. Total characters were 392,595 / 391,458 / 395,653 for candidate / baseline / upstream. Direct-call median latencies were 202 / 197 / 212.5 ms, with ranges 65–258 / 64–266 / 69–297 ms. One sequential pass per engine is not a speedup or regression estimate.

No paid agent evaluation, token/cost measurement, full application suite, release or deployed-runtime update was performed. No new language, framework resolver or synthesized edge is introduced.
