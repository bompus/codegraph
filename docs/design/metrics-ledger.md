# Metrics ledger: what each phase changed, measured

One place for the before-and-after numbers of the kernel-only extraction plan and the resolution binding-model plan, so a phase can be judged by what it moved rather than by what it intended. Every figure links back to where it was recorded; single runs are marked `n=1`. Host: this WSL machine (15 vCPUs, Node 24) unless stated.

Sources: [kernel-only-extraction-plan.md](kernel-only-extraction-plan.md) §3a, [resolution-binding-model-plan.md](resolution-binding-model-plan.md) §3, the precision reports under `__tests__/evaluation/results/` (one JSON per corpus commit and codegraph commit; `npm run eval:precision -- <corpus>` regenerates), and the PR bodies #17 to #39 on `fork/consolidated`.

## 1. Timeline

| PR | Phase | What changed for a user |
|---|---|---|
| #17 | Kernel-only Phase 0 | Whole-graph golden dumps (regression gate; no behaviour change) |
| #18 | Kernel-only Phase 1 | Files with parse errors extracted natively (were WASM) |
| #20 | Kernel-only Phase 2 | Vue / Svelte / Astro / Razor script blocks extracted natively |
| #21 | Kernel-only Phase 3 | Read-time parse derivations served by the kernel |
| #23, #24 | Kernel-only Phase 4 | Every remaining language parsed natively; five grammars compiled in |
| #25 | Kernel-only Phase 5 | WebAssembly parser deleted; Node 25 allowed; install 41% smaller |
| #27 | Binding Phase 0 | Precision scoring on pinned real repositories (gate only) |
| #28 | Binding Phase 1 | TS/JS binding rows; `isExported` from the table |
| #29, #30, #31 | Binding Phase 2 | Resolution reads the rows; bare-call kind rule; TS/JS source regexes deleted |
| #33 to #37 | Binding Phase 3 | Rows for Python, Go, Java, Kotlin, PHP, C/C++; every import regex deleted |
| #39 | Release notes | — |

## 2. Speed, size and footprint (kernel-only plan)

| Measure | Before | After | Where |
|---|---|---|---|
| Fresh index, redis (794 C/H files), n=1 | 5.18 s, 1,301 MB peak RSS | 4.06 s, 1,355 MB | §3a, after Phase 1 |
| Files needing the WASM parser for a parse error (redis / fmt / okio) | 350 / 39 / 24 | 0 / 0 / 0 | §3a, Phase 1 |
| Grammar compile per fresh index saved by dropping WASM (8 workers) | 4.34 s | 4.06 s | §3a, Phase 5 payoff |
| MCP `initialize`, small project, median of 7 | 120 ms, 191 MB, 3 processes | 81 ms, 139 MB, 2 processes | §3a, Phase 5 payoff |
| Install (`dist/` + kernel) | 75 MB | 44 MB (10 MB + 34 MB kernel) | §3a, Phase 5 payoff |
| Runtime dependencies removed | — | `web-tree-sitter`, `tree-sitter-wasms` (54 MB in `node_modules`) | #25 |
| Lines removed with the WASM path | — | about 27k | #25 |
| Read-time parse, kernel vs WASM | bare parse 0.57 to 0.96×, tokenize 1.03 to 1.58× | | §3a Phase 3 table (`scripts/bench-parse-tree.mjs`) |

Reading: the parser swap is an install-size, cold-start and supply-chain win; peak memory at index time did not move (it is the store and resolution, not the parser).

## 3. Answer precision (binding-model plan)

Each row is one gated change. "Edges" is the resolved-edge total on the pinned corpus; the histogram is by strategy. Every absent case held and every control held on every run. Edge-level reviews (lost / gained / moved, with sampled cases) are in the plan's phase records.

| Change | Corpus | Edges before → after | Notable movement |
|---|---|---|---|
| Phase 1: TS/JS rows, later exports | vite | 28,893 → 28,893 | 12 exact-match → import |
| Phase 2: resolver reads rows | vite / vitest / svelte | 28,893 → 28,826 / 75,035 → 74,987 / 70,432 → 70,603 | 201 references moved from name guess to import on vite; 0 genuine losses after review |
| Phase 2: bare-call kind rule | vite / vitest / svelte | 28,826 → 28,688 / 74,987 → 74,887 / 70,603 → 70,589 | 102 property-target edges dropped on vitest |
| Phase 2: rows for every TS/JS path, regexes deleted | vite / vitest / svelte | 28,688 → 28,731 / 74,887 → 74,888 / 70,589 → 70,589 | 41 `declare global` edges restored (a Phase 2 misjudgement) |
| Phase 3: Python | flask | 5,268 → 5,292 | import 90 → 371; 267 moved to import, 0 lost |
| Phase 3: Go | gin | 8,544 → 8,544 | 11 same-name ties re-broken |
| Phase 3: Java | spring-petclinic | 1,457 → 1,457 | byte-identical |
| Phase 3: Kotlin | Exposed | 80,302 → 80,573 | import 3,595 → 8,032; 480 references moved to the right module, 0 the other way |
| Phase 3: PHP | Slim | 5,016 → 5,016 | byte-identical |
| Phase 3: C | jq | 7,712 → 7,713 | 0 lost, 1 gained |
| Phase 3: C++ | nlohmann/json | 24,640 → 24,647 | import 644 → 686; 319 ties re-broken |

Reading: the binding table removes wrong cross-file links (bare imports, parameters, file-local names) and replaces name guesses with import-resolved edges; the big wins are where a language had no import mappings at all (Kotlin) or no export flags (Python). The remaining churn is same-name tie-breaks, which the receiver rule (Phase 2b) is meant to settle.

## 4. Regression gates

| Gate | Catches | Result across the work |
|---|---|---|
| Golden dumps (7 fixture corpora) | Any extraction, resolution or synthesis change | Every PR's dump diff reviewed; 1 real regression caught (Phase 1 parse-collapse warning), 2 misjudgements corrected on review |
| Walker ≡ generic-extractor parity | A walker drifting from the generic path | Held for every walker language |
| AST-only rows ≡ walker rows | The bindings emitter drifting from the walk | Held for 8 languages; pinned the walkers' scoping and misparse rules in words |
| Precision cases (9 corpora) | A known-wrong edge returning, a control disappearing | All held on every run |
| Full suite | Everything else | 4,773 → 4,804 tests; green on Linux and Windows at every merge |

## 5. Measurements added for this ledger (2026-09-12)

Filled in below from runs against the current head, the last commit before binding rows (`72a5703b`), and the last published release (1.6.0, WebAssembly parser).

### 5.1 Fresh index cost: 1.6.0 (last release, WebAssembly parser) vs `72a5703b` (native-only, before binding rows) vs head

`codegraph init -y` on a clean checkout, `/usr/bin/time`, best of two runs, `nice -n 10`, one job at a time. `.codegraph` size is the graph database after init. "failed refs" is the unresolved-reference count (references extraction emitted that resolution could not bind).

| corpus | build | wall s | peak RSS MB | database MB | nodes | edges | failed refs | binding rows |
|---|---|---|---|---|---|---|---|---|
| petclinic | 1.6.0 | 0.59 | 286 | 2.9 | 935 | 1415 | 2089 |  |
| petclinic | 72a5703b | 0.55 | 308 | 3.1 | 975 | 1457 | 2105 |  |
| petclinic | head | 0.56 | 322 | 3.4 | 975 | 1457 | 2105 | 1079 |
| jq | 1.6.0 | 0.87 | 450 | 5.4 | 1682 | 7368 | 2143 |  |
| jq | 72a5703b | 0.78 | 398 | 5.8 | 1943 | 7712 | 2092 |  |
| jq | head | 0.79 | 410 | 6.3 | 1943 | 7713 | 2091 | 4682 |
| flask | 1.6.0 | 0.72 | 309 | 5.3 | 2705 | 5268 | 2847 |  |
| flask | 72a5703b | 0.74 | 347 | 5.6 | 2727 | 5268 | 2967 |  |
| flask | head | 0.75 | 361 | 6.3 | 2727 | 5292 | 2943 | 5383 |
| slim | 1.6.0 | 0.69 | 310 | 6.8 | 2166 | 4860 | 5633 |  |
| slim | 72a5703b | 0.67 | 318 | 7.0 | 2288 | 5016 | 5602 |  |
| slim | head | 0.70 | 323 | 7.8 | 2288 | 5016 | 5602 | 4358 |
| gin | 1.6.0 | 0.82 | 325 | 8.1 | 2531 | 8041 | 6858 |  |
| gin | 72a5703b | 0.87 | 433 | 8.8 | 2958 | 8544 | 6913 |  |
| gin | head | 0.93 | 441 | 9.6 | 2958 | 8544 | 6913 | 7403 |
| json | 1.6.0 | 3.14 | 712 | 28.5 | 8554 | 19875 | 33331 |  |
| json | 72a5703b | 2.79 | 807 | 33.9 | 12696 | 24640 | 32667 |  |
| json | head | 2.93 | 767 | 36.4 | 12696 | 24647 | 32660 | 14470 |
| vite | 1.6.0 | 1.99 | 671 | 33.8 | 9358 | 27751 | 24872 |  |
| vite | 72a5703b | 2.67 | 812 | 40.4 | 13804 | 28893 | 29304 |  |
| vite | head | 2.63 | 866 | 44.5 | 13804 | 28730 | 29467 | 23324 |
| vitest | 1.6.0 | 3.17 | 1040 | 64.6 | 16179 | 73083 | 35984 |  |
| vitest | 72a5703b | 3.99 | 1279 | 78.9 | 24365 | 75035 | 44576 |  |
| vitest | head | 4.14 | 1349 | 86.3 | 24365 | 74878 | 44731 | 42825 |
| svelte | 1.6.0 | 5.02 | 1166 | 80.3 | 30582 | 65249 | 28656 |  |
| svelte | 72a5703b | 5.67 | 1352 | 90.5 | 36982 | 70432 | 31061 |  |
| svelte | head | 5.91 | 1468 | 101.2 | 36982 | 70587 | 30906 | 46584 |
| exposed | 1.6.0 | 4.25 | 1147 | 90.2 | 23560 | 76658 | 37933 |  |
| exposed | 72a5703b | 4.80 | 1210 | 97.0 | 25321 | 80302 | 40369 |  |
| exposed | head | 5.20 | 1293 | 108.6 | 25321 | 80573 | 40096 | 39052 |

Medians over the ten corpora: the binding rows (head vs `72a5703b`) cost +4% wall, +4% peak RSS and +10% database size, for identical node counts. Against the last release (head vs 1.6.0) the head indexes +9% slower, uses +15% more peak memory and writes a +20% larger database, while extracting +16% more nodes (Markdown, nested functions, interface members, the exports and bindings this fork added); on the small corpora the wall times are within noise. So the parser swap's index-time saving measured on redis (§2) is spent, and more, by the extra symbols and the rows: a fresh index is not faster than 1.6.0 on these repositories, and the memory headline is worse by about a seventh. The rows are the smaller part of that; the added extraction is the larger.

### 5.2 Explore probes: 1.6.0 vs head

`scripts/agent-eval/probe-explore.mjs` (deterministic, no agent) on three corpora, two symbol-bag queries each, against each build's own index. Only the shape of the answer is measured here; whether the answer is *right* needs the agent A/B (§6).

| corpus | query | 1.6.0 ms / chars | head ms / chars |
|---|---|---|---|
| flask | `Flask dispatch_request full_dispatch_request` | 161 / 17993 | 182 / 16826 |
| flask | `url_for build` | 146 / 15305 | 177 / 17208 |
| gin | `Context JSON Render` | 158 / 14317 | 183 / 12795 |
| gin | `Engine ServeHTTP handleHTTPRequest` | 161 / 16618 | 203 / 12438 |
| vite | `createServer resolveHttpServer` | 206 / 24927 | 271 / 21961 |
| vite | `resolveConfig createLogger` | 206 / 24837 | 284 / 24877 |

Reading: every probe answered on both builds with a same-sized rendering; the head answers 20 to 80 ms later per call (the row lookups and the larger graph), which is invisible next to an agent turn. The probe cannot say which answer an agent would stop reading at; that is the unmeasured item below.

### 5.3 Coverage

The "failed refs" column above is the coverage counter: references the extractor emitted that resolution left unbound. Between `72a5703b` and head it moves by less than 1% on every corpus (down on flask, jq, svelte, exposed; up on vite and vitest, where bare package imports that used to bind wrongly now stay unbound by design). Between 1.6.0 and head it rises with the node count, because the fork emits more references (interface members, nested functions, value references) than it can bind; that ratio is a property of the added extraction, not of resolution.

## 6. Not measured

- Agent A/B (`scripts/agent-eval/run-all.sh`, with vs without CodeGraph, or new build vs baseline build) has not been re-run since the parser swap. It is the only measure of the tool-call and Read/Grep counts the project optimises for, and it needs a live Claude session per arm.
- macOS: no run of the native-only kernel on macOS at all. Validation remains outstanding and needs a separate build/test run; see the fork release policy in [AGENTS.md](../../AGENTS.md#releases).
- A truth set for same-name ties (Go `BindBody`, C++ `begin`): the changes are described and sampled, not scored.
