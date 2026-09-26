Status: Priority 1 and Bun activation verified after restart — [results and limitations](codegraph-native-runtime-measurement.md). Live clients and daemon use Bun at df01afe6; code, Markdown and session queries pass. Node build/test harnesses retained. CG-11 Codex-first refresh rollout is approved and owned by routers task; other later priorities remain deferred.

# CodeGraph experiments

## Baseline and execution

Use `bompus/codegraph:fork/consolidated` by default, including current upstream main. Pin isolated research arms explicitly. Preserve the last verified deployment if integration or promotion fails. Coordinate updater and connection work with task `01a07a0c-95f6-7da2-a6bf-113be168b169` (codegraph: routers).

Markdown uses a custom JavaScript extractor, not WASM. The deployed addon need not advertise Markdown. Verify native code extraction and Markdown extraction with WASM disabled separately.

Reuse existing evaluation/probe/parity harnesses. Compare identical inputs in eight interleaved, order-reversed paired rounds; report cold runs separately, medians and spread, process-tree memory where available, and actual fallback usage. No paid model evaluations in Priority 1. Close experiments as adopt, reject with evidence, or defer with a named trigger. API/schema/dependency changes require a concrete implementation plan.

## Priority 1 — approved

- [x] **CG-01 Save full backlog:** this board preserves all nineteen items; link from research and retain deferred work.
- [x] **CG-02 Deployment guards:** freshness/upstream/artifact/daemon checks passed at df01afe6; native and Markdown-without-WASM smokes passed. No demonstrated guard defect; updater unchanged.
- [x] **CG-03 Baselines:** eight paired index rounds each on frozen repo and pinned Vite archive; timing and normalized graph records saved. Deviation: five Vite symlinks absent on both arms; phase breakdown and controlled cold-init comparison remain unmeasured.
- [x] **CG-04 Bun/Node:** native graph parity, 144 retrieval calls, session and changed/no-op sync checks passed; native/WASM memory controls recorded. Fresh init and 406 MB session checks passed. Routers task reports 16/16 verified Bun-server lifecycle tests and 29/29 Bun lifecycle units passing; EBUSY is isolated to Bun test-host cleanup. Bun activation verified after restart; full WASM parity is not claimed.
- [x] **CG-05 Extraction costs:** 463 native extractions, zero deferrals in repo diagnostic; Markdown custom JS, Vue scripts WASM. One-worker memory check performed. Eager grammar loading and Vue routing are measured follow-up candidates; no native Markdown work warranted.

## Priority 2 — deferred

- [ ] **CG-06 Retrieval/agent evaluation:** re-run literals, Markdown, symbols, concepts and multi-hop tasks; flags/regexes remain Grep controls. Score answers, useful pointers, calls, tokens and cost by host/model; replace stale adoption conclusions.
- [ ] **CG-07 Omitted symbols:** reproduce upstream #1711; test elided names/locations within output budgets, completeness claims and deduplication. Gate: targeted answer recovery without correctness loss.
- [ ] **CG-08 Resolver/SCIP:** preserve known import/call regressions; compare compiler references and manually adjudicated edges. Gate: precision/recall by relationship type; overlay only for consequential misses.
- [ ] **CG-09 Export facts:** investigate #1721 direct/later exports, re-exports and CommonJS. Gate: measured inconsistencies, native/WASM parity and explicit persistence compatibility before implementation.
- [ ] **CG-10 Routing contributions:** track upstream #1737 and fork PRs #3–15. Revalidate bases, dependencies, negative cases, handlers, docs and native/WASM behavior before integration. Waku programmatic support depends on #1706. Existing routers task owns this work.

## Priority 3 — deferred except separately approved CG-11

- [ ] **CG-11 Automatic refresh:** Codex-first rollout separately approved and active in the owning routers task; it reports 20 Bun refresh tests and the real Codex/Bun A/B refresh trial passing. Receipt: `codegraph-codex-trial-result-01a07a0c.md` under the managed fork parent directory. No duplicate implementation here.
- [ ] **CG-12 Incremental reliability:** edits, renames, deletion, branch changes, interruption, concurrent sessions initialization and repeated Windows start/stop. Gate: no stale records, wrong source slices or leaked handles.
- [ ] **CG-13 Sessions:** assess rationale usefulness/freshness/project attribution; investigate Codex reader first, Cursor when history is missing. Gate: correct host-qualified identity; exclude thinking/tool traffic and unrelated projects.
- [ ] **CG-14 Repo memory:** require real benefit beyond Markdown search. Repository-only, explicit exclusions and filesystem boundaries, including human-only notes. Revisit source-type vs speaker-role design. Absence of corroborating search hits does not prove staleness. Existing fork note: `docs/design/sessions-memory-role.md`.
- [ ] **CG-15 Stemming/ranking:** prose-specific stemming with exact identifier/literal preservation; session Porter stemming already exists. Gate: judged relevance gain without identifier regressions.
- [ ] **CG-16 SQLite:** measure plans, writes, contention, traversal, WAL and memory. Try indexes/batching/connections first, Bun SQLite separately if warranted. Gate: end-to-end gains and transaction/FTS/recovery parity before another engine.
- [ ] **CG-17 Packaging:** after Bun compatibility, compare ordinary execution, bytecode and standalone. Verify addon/workers/assets/custom extractors/fallback grammars/watchdog/persistence/shutdown outside checkout. Gate: complete operation plus measured DX, startup and size tradeoffs.

## Conditional research — deferred

- [ ] **CG-18 Targeted triggers:** spawn-by-path edges when another task needs them; Vue templates when missing edges lose answers; git-history/co-change when existing rationale tools fail; embeddings when concept tasks expose recurring lexical gaps. Require observed failures first.
- [ ] **CG-19 New engine:** reconsider after profiling. Prototype one file-change → extraction → storage → useful answer path using reusable components where licensing permits. Gate: demonstrated correctness, speed, memory, DX and maintenance advantages before expanding coverage.
