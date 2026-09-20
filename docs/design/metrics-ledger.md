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
| #44 | Binding Phase 4 | Kernel-native read+settle over persisted bindings; per-worker kernel conns on a snapshot copy |

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
| C++ iterator ADL ties | nlohmann/json | 22,817 → 22,587 | 93 wrong `calls`→`begin` removed; 12 correct implicit-this members remain; exact-match 7,146 → 6,916 |
| C++ `Type(...)` constructor retarget | nlohmann/json | 22,587 → 22,587 | main-header `items()` now `calls` the `iteration_proxy` constructor instead of only `instantiates` the class |

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

Reading: every probe answered on both builds with a same-sized rendering; the head answers 20 to 80 ms later per call (the row lookups and the larger graph), which is invisible next to an agent turn. The probe cannot say which answer an agent would stop reading at; the subsequent agent baseline is linked below.

### 5.3 Coverage

The "failed refs" column above is the coverage counter: references the extractor emitted that resolution left unbound. Between `72a5703b` and head it moves by less than 1% on every corpus (down on flask, jq, svelte, exposed; up on vite and vitest, where bare package imports that used to bind wrongly now stay unbound by design). Between 1.6.0 and head it rises with the node count, because the fork emits more references (interface members, nested functions, value references) than it can bind; that ratio is a property of the added extraction, not of resolution.

### 5.4 Resolution phase cost (Phase 4, #44)

`codegraph init` on the linux-kernel corpus (71,120 files, 5.88M unresolved refs), `CODEGRAPH_SYNTH_TIMINGS=1 CODEGRAPH_RESOLVE_PROFILE=1`, `nice -n 10`, one job on the host. Kernel-on is the merged build (per-worker `KernelResolver` on a checkpointed snapshot copy); kernel-off is the same build with `CODEGRAPH_KERNEL_RESOLVE=0`.

| Measure | Kernel-off | Kernel-on |
|---|---|---|
| Resolution phase, total | 243.0–264.8 s (n=2) | 260.8–280.2 s (n=3) |
| — batch loop stages (read+settle+persist+…) | ~80–86 s | ~95–103 s |
| — callback-synthesis inside the phase | 136.9 s | 139.7 s (cFnPtr pass 125.4 s) |
| — ref/edge index recreate | 23.9 s | 23.4 s |
| — snapshot fold+copy at pool engage | n/a | ~3 s one-time |
| Kernel-handled refs (of 5.86M loop refs) | 0 | 3,247,661 (55.4% native; rest passthrough to the TS path) |
| Edges / failed refs | 6,412,714 / 2,052,370 | 6,412,714 / 2,052,370 |

Reading: Phase 4's goal was determinism and deleting source rescans, and it delivers that — kernel-on and kernel-off produce a byte-identical edge set on the linux corpus (sorted edge dump sha256 `5ebfeee8…`, same failed-ref count). Wall-clock is **a few percent slower, not a speedup**: kernel-on means ~272s vs kernel-off ~254s (ranges overlap, n is small). The delta matches the structure: ~7–9s of kernel-outcome admission/marshal inside the settle stage (22.4–25.0s vs 13.6–15.6s) plus the one-time ~3s snapshot, i.e. ~4–7%. The phase is dominated by callback synthesis (~140s) and main-thread persist (~63s) — per-ref settle is not the bottleneck, so native resolution cannot move wall-clock here. The earlier serial-mode A/B (281s kernel-on vs 229s kernel-off, main-thread `resolveChunk` without the pool) overstated the gap.

### 5.5 cFnPtr synthesis — threaded path sweep round

§5.4 named `cFnPtrEdges` (~125s serial pass, ~half of callback-synthesis on the linux corpus) as the phase's biggest lever. Standalone probe against the live corpus DB (read-only `ReferenceResolver` + `cFnPointerDispatchEdges`, `CODEGRAPH_SYNTH_TIMINGS=1`), before/after the `cfnptrScanPaths` change:

| Stage | Before | After |
|---|---|---|
| A — extraction sweep | 66.7 s | **16.1 s** |
| B — struct layouts | 0.8 s | 0.8 s |
| C — registration | 31.2 s | 30.3 s |
| D — propagation | 10.0 s | 9.9 s |
| E — dispatch | 15.7 s | 15.2 s |
| **Pass total** | **124.5 s** | **72.3 s (−42%)** |
| Edges / sha256 | 283,931 / `a6161359…` | 283,931 / `a6161359…` (identical) |

What moved: stage A was already native (`cfnptrScanFiles`) but single-threaded on one worker, spending ~14s on 84k per-file `getNodesInFile` queries, ~10s on JS-side `readFileSync`, and the rest on the serial scan + ~1.5GB of text marshal across napi. The new `cfnptrScanPaths` takes absolute paths + bulk-prefetched struct extents (one kind-scan instead of per-file queries), reads each file inside Rust (utf-8-lossy, same bytes), and fans the batch across scoped threads (`available_parallelism`, ≤16) — reads, strips, and scans scale with cores. Output stays 1:1 with input order; unreadable/panicking files produce empty facts, which merge to nothing — the same as the JS sweep's `if (!rawText) continue`. Side benefit: stage A no longer populates the raw/strip caches, so the big all-or-nothing source cache only retains stage-C/D/E survivors (~16% of files).

Remaining floor: C (30.3s) is now the biggest stage — `processUnit` regex work over surviving units + include re-scans with macro-env expansion; its inline-struct registration order (first-wins in file order) makes naive file sharding unsafe, so a real cut is a Rust port of the linking stages, not a JS change. D (9.9s) + E (15.2s) are the same class.

### 5.6 Framework `claimsReference` port — native share round

§5.4's 55.4% native share was gated mostly by one boolean: with ANY framework detected (`frameworks_active`), every prefilter miss, empty-candidate list, and gated import deferred to TypeScript "in case a framework resolver wants it". On the linux corpus the detected set was `express` alone — which has no `claimsReference` at all — yet ~1.9M `fail:calls` refs still rode the full `resolveOneInner` dispatch to reach the same prefilter miss.

Each `claimsReference` is a pure name-shape predicate, so the kernel now evaluates that arm natively: the config carries `frameworkNames` (the detected `f.name` list), `resolve.rs` replicates every registered resolver's predicate (JS `\w` spelled `[A-Za-z0-9_]`), a resolver with no `claimsReference` claims nothing, and an unlisted name (custom `registerFrameworkResolver`) claims everything — a conservative passthrough, never a wrong verdict. A config that omits `frameworkNames` keeps the old claim-everything behavior. The `no_candidates` and gated-import arms still defer: framework `resolve()` output is not claims-gated, and a ≥0.9 framework hit can still displace the kernel's winner.

Same corpus run (`codegraph index`, `SYNTH_TIMINGS=1 RESOLVE_PROFILE=1`, kernel-on):

| Measure | Before (#46) | After |
|---|---|---|
| Kernel-handled refs (of 5.86M) | 3,247,661 (55.4%) | **4,803,308 (82.3%)** |
| Passthrough to TS | 2,616,843 | 1,036,196 |
| Edges / failed refs | 6,412,714 / 2,052,370 | 6,412,714 / 2,052,370 (identical) |
| Resolution phase, total | 260.8–280.2 s (n=3) | 278.3 s |
| — settle stage | 22.4–25.0 s | 33.7 s (more refs settled natively, ~6µs each) |
| TS-path `fail:calls` across workers | ~1.9 M | ~20 k |

Reading: verdict-identical output — the only refs whose handling changed were `!pre_pass && !claimed`, which the TS prefilter already dropped (non-JS refs fall to `matchJsStoreBindingCall`, which is null for them; JS store-bind candidates still passthrough earlier). Wall-clock is unchanged inside the run range: the moved refs were already the cheapest TS misses, and the phase floor is persist (~63s) + synthesis (~103s post-#46) + loop overhead, not per-ref verdict cost. The value is coverage — 82% of the loop now resolves where native wins compound — plus a quieter TS path (the `fail:calls` dispatch mass is gone). Remaining passthroughs are dominated by `function_ref` (~700k, an excluded kind with its own dedicated pipeline), `no_candidates` punts, gated imports, and JS store-bind files.

## 6. Agent measurements and remaining gaps

- The post-parser-swap [agent baseline](../benchmarks/binding-model-agent-baseline-2026-09-12.md) has 36 runs against 1.6.0 and frozen `86fc9dbc`. Flask and Gin used fewer tools; Vite did not show a time improvement and still required reads.
- The [receiver first-cut comparison](../benchmarks/receiver-bindings-2026-09-12.md) has a separate balanced 36 runs against `86fc9dbc` and `d3c330db`. Precision cases hold on four corpora. Agent time/read outcomes are mixed, with no general speedup or zero-read claim; Flask's deterministic outputs are byte-identical despite different agent read counts. That first cut left other-language receiver gating and a persisted `unknown-receiver` reason open; the [expanded receiver validation](../benchmarks/receiver-phase2b-2026-09-12.md) closes those contracts for the migrated languages at `fb0f239d`. All 20 precision cases pass across ten corpora. Its separate 36-run A/B still has mixed read/time outcomes; source-free inference and zero-read retrieval remain open.
- The [Flask/Vite fallback investigation](../benchmarks/explore-fallbacks-2026-09-12.md) fixes omitted requested bodies and call-site excerpts at `d79aaf19`. In its separate balanced 36-run comparison, Flask fallback calls fall from three to zero; Vite reads fall from five to one and median time from 28.2s to 21.1s. Gin retains three fallback calls per arm, with one extra total tool call and a roughly one-second higher candidate median; exact fallback-query replay retains identical source. Vite's remaining constants-list read and broader zero-read coverage stay open.
- The [explicit-constants follow-up](../benchmarks/explore-constants-2026-09-12.md) closes the Vite constants-list omission at `41d9740f`: exact-sequence declaration coverage rises from 0/8 to 8/8 lines, while twelve deterministic control responses are byte-identical. Its focused 12-run A/B reproduces a baseline constants `grep`; both candidate Vite runs use two explore calls without fallback. This closes that specific gap, not universal zero-read coverage.
- The [Gin follow-up](../benchmarks/gin-fallbacks-2026-09-12.md) addresses the recorded rendering and binding gaps at `ed5033cb`. Exact-query replay restores 19/19 rendering lines and the missing grouped-variable declaration. Eight Flask/Vite control responses remain byte-identical; Gin precision retains all three cases with no lost edges. Both arms of its fresh 16-run A/B have zero fallback access, so the replay—not a reduction in fresh fallback counts—is the omission proof. Extraction version 36 requires re-indexing Go projects for the grouped declarations.
- macOS: no run of the native-only kernel on macOS at all. Validation remains outstanding and needs a separate build/test run; see the fork release policy in [AGENTS.md](../../AGENTS.md#releases).
- Go `BindBody` now has scored present/absent controls and all seven corrected endpoints were reviewed. The C++ `begin` same-name ties have a scored truth set at the commit below: 6 present + 4 absent cases, 11/11 held, with the wrong-present inventory (75 cross-TU / `using std::begin` / vector-member edges) recorded as the backlog for the next resolution work rather than encoded (review-corrected: 75 in the main tree plus 18 same-shape edges in the vendored ABI copy, 93 wrong-present total).

### 5.7 `function_ref` bare-name port — native share round

§5.6 left `function_ref` as the biggest remaining passthrough bucket: the kind was excluded from kernel eligibility outright, so ~700k corpus refs rode the TS pipeline to reach a dedicated path (`resolveViaImport` kind-gated to function/method/Python-class, then `matchFunctionRef`) that never touches frameworks or the fuzzy matchers. The kernel now runs that arm itself: prefilter miss is terminal (`matchJsStoreBindingCall` is `calls`-gated — dead for `function_ref`), the import hit is kind-gated and *discarded* on gate failure (not pooled), then the bare name arm — same-file earliest-line at 0.95/0.9, unique-or-drop cross-file at 0.8, `bareFnOnly`/`bareClassOk` kind gates, Swift implicit-self method scoping, `is_final` so no framework merge attaches. `this.`/`Cls::m` shapes carry a separator, stay ineligible, and keep the TS arms.

Same corpus run (`codegraph index`, `SYNTH_TIMINGS=1 RESOLVE_PROFILE=1`, kernel-on, niced under a co-tenant build — timings carry contention):

| Measure | After #47 | After (#48) |
|---|---|---|
| Kernel-handled refs (of 5.86M) | 4,803,308 (82.3%) | **5,366,728 (91.5%)** |
| Passthrough to TS | 1,036,196 | 497,776 |
| Edges / failed refs | 6,412,714 / 2,052,370 | 6,412,714 / 2,052,370 (identical) |
| Resolution phase, total | 278.3 s | 264.0 s |
| — settle stage | 33.7 s | 27.4 s |
| TS-path `function_ref` across workers | ~700 k | ~490 (the non-bare `this.`/`Cls::m` shapes) |

Reading: the last excluded kind is native — 91.5% of the resolution loop now settles without a TS round-trip, verdict-identical (edges and failed refs byte-for-byte at the corpus level; the parity test pins every arm including the kind-gated import discard). Wall-clock again moves only within noise: the moved refs were cheap misses, and the floor remains persist + synthesis. Remaining passthroughs (~500k) were guessed at here as `no_candidates` punts, gated imports, JS store-bind files, and non-bare function_ref shapes — §5.13's instrumented breakdown corrects this: `no_candidates` is a handled-status marker, and the true split is non-bare names (~89%) + non-migrated languages (~11%).

### 5.8 cFnPtr stage-C env + D/E link port (#49)

§5.5's remaining floor — the serial C preprocessor registration (env-build + `processUnit` + include rescans) and the D/E body scans — moved into the kernel via two entry points. `cfnptr_file_envs` returns each file's macro-env pieces plus its stripped text in one native read+strip+parse (the LRU-bounded caches and lazy fill stay; `stripped` feeds `srcCache` so `processUnit` doesn't re-strip — the old `src()`-backed extractors warmed it for free; unreadable paths return null → the JS path/`ctx.readFile` still gets virtual files). `cfnptr_link` runs stage D's `field←field` scan, the 3-pass `reg` fixpoint, and stage E dispatch emission threaded across files with input-order output; hand-rolled byte machines replicate the JS regexes including `^`/`$` anchors over all four JS line terminators.

Same standalone probe on the live corpus DB, before = `54e969a4` with only the new calls disabled (stage A native in both arms — apples-to-apples):

| Stage | Before | After (#49) |
|---|---|---|
| A — extraction sweep | 15.1 s | 14.8 s |
| B — struct layouts | 0.7 s | 0.7 s |
| C — registration | 28.9 s (env 16.2 / unit 6.2 / inc 6.2) | **20.9 s** (env 8.2 / unit 5.5 / inc 6.9) |
| D+E — propagation + dispatch | 9.3 + 14.0 s | **4.4 s** |
| **Pass total** | **67.9 s** | **40.8 s (−40%)** |
| JS readFile/strip calls in pass | ~all survivors | **0 / 0** |
| Edges | 283,931 | 283,931 (kernel-on ≡ kernel-off under a fixed edge-set hash) |

Reading: cumulative the pass is 124.5 s → 40.8 s (−67%) since §5.5's serial baseline, with the last JS source touches gone. Stage C's serial include-walk stays in TypeScript by design (first-wins ordering over a virtual-capable FS); its cost is now mostly the conditionally-evaluated include rescans. `CODEGRAPH_KERNEL_CFNPTR=0` and kernels lacking the entry points keep the verbatim JS loops.

End-to-end confirmation (`codegraph index` on the same corpus at `34a1c20c`, `SYNTH_TIMINGS=1 RESOLVE_PROFILE=1`, `nice -n 10`, n=1, host idle):

| Measure | Kernel-off (#44) | Kernel-on #44 | Kernel-on head |
|---|---|---|---|
| Resolution phase, total | 243.0–264.8 s (n=2) | 260.8–280.2 s (n=3) | **171.4 s** |
| — callback-synthesis | 136.9 s | 139.7 s | **59.0 s** (cFnPtr pass 48.0 s in-run) |
| — batch loop stages | ~80–86 s | ~95–103 s | ~94.5 s (implied) |
| — ref/edge index recreate | 23.9 s | 23.4 s | 17.9 s |
| Kernel-handled refs | 0 | 3,247,661 (55.4%) | 5,366,728 (91.5%) |
| Edges / failed refs | 6,412,714 / 2,052,370 | 6,412,714 / 2,052,370 | 6,412,714 / 2,052,370 (identical) |

This is the round where native ports moved wall-clock, not just verdicts: kernel-on has flipped from ~4–7% slower than kernel-off (§5.4) to **~30–35% faster** (171.4 s vs the 243–265 s kernel-off range), driven entirely by the synthesis cuts — the batch loop is unchanged and now dominates the phase at ~94.5 s (of which main-thread persist ~63 s is the largest remaining single item). The cFnPtr pass runs slower inside the full index than standalone (48.0 s vs 40.8 s; stage A 19.4 s vs 14.8 s) — expected, it shares the host with the parse loop's aftermath and pool teardown.

### 5.9 Fresh-init write path (`dbbf7afc`)

The parse-loop's `store` stage was the largest single line in a fresh index. Two changes, both scoped to the fresh-DB bulk window:

- `foreign_keys = OFF` under fastInit in the store worker: every edge and unresolved_refs insert paid a parent-key probe on `nodes` (~19M B-tree lookups on this corpus). `finalizeStoreBundle`'s endpoint filter already guarantees the constraint; no deletes run in the window so `ON DELETE CASCADE` cannot fire. Unchanged (ON) on the non-fastInit path.
- `idx_literals_file`, `idx_bindings_file`, `idx_bindings_name` added to `BULK_PARSE_INDEX_NAMES`: 5.2M bindings + 0.3M literals rows were maintaining three indexes per insert; nothing reads those tables mid-window. Rebuild cost is one table scan each in `endBulkParseLoad`.

Same corpus run (`codegraph index`, `SYNTH_TIMINGS=1`, `nice -n 10`, n=1, host idle):

| Measure | Before | After |
|---|---|---|
| store-worker busy (`store=`) | 122.7 s | **56.5 s (−54%)** |
| parse-loop phase | 138.2 s | **72.5 s** |
| parse-index-rebuild | 24.6 s | 28.3 s (three more rebuilds) |
| resolution phase | 171.4 s | 176.0 s (noise — untouched path) |
| edges / failed / bindings / literals | 6,412,714 / 2,052,370 / 5,228,395 / 295,521 | identical |

Reading: the biggest remaining index-time items are now the resolution batch loop (~94.5 s, of which main-thread persist ~63 s) and `parse-index-rebuild` (28.3 s — now 17 single-scan index builds). The store-worker still pays per-file transactions and JS row materialization; both are smaller than the index-maintenance floor that was removed here.

### 5.10 Resolution loop — edge persist overlap (`a8476619`)

§5.9 named the batch loop the largest remaining item; its serialized piece was `insertEdges` between settle and the next fan-out. The dubbo-validated barrier turned out to cover only supertype edges: the ONLY mid-loop live-conn edge reads are `getOutgoingEdges(id, ['extends','implements'])` supertype walks (workers' TS conns read the live db read-only; the kernel reads a static snapshot). `contains` is extraction-sourced; synthesizers run post-loop; the pending-ref prefetch already ran pre-persist; deletes/marks already overlapped post-fan-out.

The fix partitions each batch's resolved refs by effective edge kind (`r.edgeKind ?? referenceKind` ∈ {extends, implements}): supertype edges insert eagerly before `beginBatch` — 44,034 of 6.41M corpus edges — and every other kind's `createEdges`+`insertEdges` moves after fan-out into the already-overlapped window. Dedup can't shift winners (identity index keys on kind) and crash order is preserved (edges still land before their refs' deletes).

Same corpus run (`codegraph index --force`, `SYNTH_TIMINGS=1 RESOLVE_PROFILE=1`, `nice -n 10`, n=1):

| Measure | Before (`2dd89cfe`) | After |
|---|---|---|
| resolution phase | 171.4 s | **154.6 s (−9.8%)** |
| — loop-stages `settle` (main's await) | 19.2 s | **6.7 s** — worker resolve now hides inside the persist window |
| — `insertEdges` busy (eager + overlapped) | 30.5 s | 32.9 s (same work, mostly overlapped) |
| — `createEdges` busy | 8.5 s | 9.6 s (two passes over the partition) |
| — read / backpressure / deletes / marks | 9.4 / 5.9 / 4.3 / 4.2 s | 8.6 / 6.5 / 4.1 / 4.1 s |
| callback-synthesis | 59.0 s | 52.4 s (noise — untouched) |
| edges / failed / cFnPtr fn-pointer-dispatch | 6,412,714 / 2,052,370 / 283,931 | **identical** |
| native share | 91.5% | 91.4% (same verdicts; ±5k ref-count wobble) |

Coverage: `__tests__/batched-supertype-ordering.test.ts` pins the cross-batch invariant (a call resolvable only via an earlier batch's committed extends edge) and the partition's completeness; the concurrent-visibility half is what this corpus gate proves. Remaining floor: `insertEdges` busy-time itself (~33s of B-tree probes + marshal on the main conn — shrinking it needs a different lever, not more overlap), `settle` 6.7s, `createEdges` 9.6s.

### 5.11 insertEdges deep-dive — measured dead ends (no commit landed)

§5.10 left `insertEdges` ~33s busy as the largest single loop item. Four candidate cuts were benchmarked on a corpus DB copy and/or a full `codegraph index --force` run; **none transfer to the in-loop wall-clock**, so no change was landed. Recorded to prevent re-attempts:

1. **FK-off window** (`foreign_keys=OFF` inside the bulkEdgeLoad window, endpoint filter as sole check). Standalone: 200k-edge inserts 2647ms→348ms (7.6x) on a cold, fully-indexed copy — but that arm measured cold parent-key probes. In-loop, `insertEdges` busy was **32.9s → 32.8s (≈0)**: the endpoint `SELECT` warms the `nodes` PK pages the FK probe needs, and at 4.8GB-DB scale inserts are I/O-bound on the edges B-tree/identity index + WAL, so removing CPU-side checks is invisible. Identity was exact (6,412,714 / 2,052,370 / 283,931 / 0 dangling) and the window was verified airtight (no parent-row deletes in `src/resolution/`; all edge writes filtered), but ~0s gain doesn't justify a permanent "no deletes inside the window" invariant. Reverted.
2. **Cross-call verified-endpoint set** (re-prove each id once per run, not per chunk): ~0s — the per-chunk `SELECT`s already hit cache-hot PK pages.
3. **Presorted inserts** (identity-key order → near-sequential B-tree writes): ~10–15% on warm copy (1338→1158ms/500k), but the JS sort costs ~0.6µs/edge and the effect shrank under an 8MB-cache arm — net ≈ 0 or negative. A staging-table + `ORDER BY` merge was worse (467ms append + 1573ms merge vs 1338ms direct): the merge pays the same identity probe and adds the sort on top.
4. **Driver swap** (hypothesis bun:sqlite > better-sqlite3 > node:sqlite): measured all three on the identical 500k-edge workload — bs3 ~1400ms, bun ~1500ms, node:sqlite ~1330–1810ms, within ~10–15%. An isolated per-call arm (500k inserts into an in-memory TEMP table) found node:sqlite ≈ bun:sqlite at ~0.5µs/row with better-sqlite3 ~1.8x slower — modern node:sqlite is already at the binding floor; the published bun:sqlite wins are FFI-latency micro-benches. The wall is inside SQLite's engine (B-tree insert + UNIQUE identity probe + WAL), shared identically by all three.

Reading: ~5µs/edge in-loop vs ~2.6µs/edge on a fully-cached copy; the gap is I/O and concurrency, not removable per-row CPU. `insertEdges` ≈ 33s is close to the floor for 6.4M indexed WAL inserts on this host; further cuts would need fewer bytes/rows or a different storage scheme, not check-removal or driver swaps. The remaining loop floor: `createEdges` ~10s, `read` ~10s, `settle` ~7s, `deletes+marks` ~9s, plus `parse-index-rebuild` ~28s and the synthesis internals.

### 5.12 Dual-runtime (Node vs Bun) — index parity + suite compat

`codegraph index --force` on the linux corpus under both runtimes (`bun` 1.4.2 / `node` v26.8.2, `SYNTH_TIMINGS=1`, `nice -n 10`, sequential on a verified-idle host: swap 0, dual-side load checked, orphan sweep, no agent activity during either arm):

| Measure | node | bun |
|---|---|---|
| total wall | 351.8s | 365.7s (+3.8%) |
| parse-loop | 141,183ms | 148,300ms (+5.0%) |
| parse-index-rebuild | 23,901ms | 29,049ms (+21.5%) |
| callback-synthesis | 59,479ms | 50,037ms (−15.8%) |
| resolution phase | 170,489ms | 170,247ms (~tie) |
| edges / nodes | 6,412,714 / 2,082,872 | **identical** |
| peak RSS (main proc) | 17.58 GB | 15.04 GB (−14%) |

Reading: byte-identical graph output; Bun nets ~4% slower wall (losses concentrated in the parse loop and SQLite index rebuilds, partly offset by a faster callback-synthesis) at 14% lower peak RSS. Reported upstream as oven-sh/bun#42924 per the Bun-slower-is-a-bug policy.

Follow-up arms on robobun's allocator hypothesis (`MIMALLOC_PURGE_DELAY=1000`, same corpus/protocol, build a5c106cc vs the pre-merge baseline build — no store/index code differs). n=2: wall **327.7s then 295.3s — 7–16% ahead of Node**. The consistent effects across both arms: the bulk-insert `store` leg of parse-loop halves (**132.5s → 64.4s / 65.0s**) and minor faults drop ~10% (20.9M→19.3/18.7M) with MaxRSS flat. `parse-index-rebuild` is the only consistent regression (~+5–7% vs the Bun baseline); arm 1's apparent ref/edge-index and resolution regressions did not replicate (arm 2: ref-index 6.2s, edge-index 13.2s, resolution 168.3s, maintenance 1.2s — all at or under both baselines). Interpretation posted back on the issue: the purge delay removes madvise/re-fault churn from the small-alloc insert path rather than the big-buffer `CREATE INDEX` path.

Upstream outcome: refiled with both arms as oven-sh/bun#42942 (superseding #42924) — **closed not-planned** by robobun. The 100ms default is deliberate (bun@5c83be0085 lowered it from upstream's 1000ms — Bun purges on a scavenger thread, cadence pinned by two tests), and their same-binary 2M-row insert A/B on native Linux x64 showed all purge-delay arms (0/100/1000) equal within noise. Their counter-read of our counters is fair: a purge-delay mechanism should show a large minor-fault + system-time drop; we saw faults −8% and sys time *up*, and the env arms ran a different build than the baselines — the 2× store gain may be a build/confound artifact, not the allocator knob.

Same-build check (2026-09-16, both arms on the post-merge binary the env arms used, back-to-back): store 67.3s no-env vs 65.5s `PD=1000` — **the halving was the build change, not the env var**; robobun's confound call confirmed. Wall 356.0 vs 316.6s sits inside the observed run spread (identical env arms were 327.7 vs 295.3s); faults ~19–20M and sys time ~185–207s show no purge-delay signature either way. Corrective follow-up posted on #42942. Mechanism attributed: the baseline's 132.5s store proves its binary predated `dbbf7afc` (FK parent-key probes + literals/bindings index maintenance dropped during fresh-init — ~19M B-tree lookups removed); the env arms' binary carried it. Not the merge — e5952c45 brought nothing store-side.

Suite compat (`bun --bun x vitest run` — plain `bun x vitest` honors vitest's node shebang and silently runs Node): baseline was 88/4,963 failing, ~86 of them a single root cause — Bun's `os.homedir()` snapshots $HOME at spawn and ignores runtime `process.env.HOME` mutation (oven-sh/bun#29244; fixed upstream by #42599). Handled test-side via `__tests__/bun-homedir.setup.ts` (`mock.module` + require-exports patch — Bun's builtin ESM namespace is a frozen snapshot and `mock.module` doesn't reach `require()` callers, so both paths are patched). Remaining divergences, each reported upstream:

- `process.env` writes don't reach native environ (kernel's `CODEGRAPH_VALUE_REFS` getenv gate) → oven-sh/bun#42891; the one test is `skipIf(process.versions.bun)`.
- `require.resolve` falls back to bun's global install cache / auto-install → oven-sh/bun#42893 (documented behavior; npm-shim tests pass `--no-install` to the child under Bun — a bare `node` spawn would still hit bun's node→bun PATH shim).
- `spawn` can't take a child's stdout stream as `stdio[0]` → oven-sh/bun#25498; the ppid-watchdog tree runs under real node via a bun-shim-filtered PATH lookup.

### 5.13 Phase 4 §7a-target check + instrumented passthrough taxonomy (2026-09-16)

Two `taskset -c 0-7` runs on the 15c host (pools size via affinity-honest `availableParallelism`), node v26.9.0, kernel-on (default), linux corpus, benchmark-protocol idle checks, n=1 each. Caveat: memory unconstrained (47 GB host vs §7a's 7 GB envelope) — reads optimistic vs the true 8c target class.

| Measure | Run 1 | Run 2 (instrumented) |
|---|---|---|
| wall clock | **292.6 s (4:53)** | 293.3 s (4:53) |
| resolution phase | 167.0 s | 162.3 s |
| kernel handled / passthrough | 5,366,983 / 502,521 (91.4% native) | 5,366,728 / 497,776 (91.5%) — the ±5k admission wobble §5.10 notes |
| edges / nodes | 6,412,563 / 2,082,872 | identical |

§7a's <10min-on-8c target is met at ~2× headroom. `settle` and `read` run natively (the mid-loop batch `read` stays on node:sqlite by the −shm isolation rule — `readPendingBatch` exists but the main-thread kernel conn is closed while the pool is engaged), and kernel-on is faster than kernel-off end-to-end (§5.8). Cross-runtime note: node-on-8c beats bun-on-15c (316.6–356.0 s) on this build; node faults 1.34 M vs bun ~19 M — mimalloc page-churn remains the runtime asymmetry (#42942).

**Passthrough taxonomy** (kernel now reports a `reason` per punt; §5.7's guess was wrong — `no_candidates` is a handled-status `unresolved`+`candidates:[]` marker, not a passthrough):

| Reason | Count | Share of passthroughs | What it is |
|---|---|---|---|
| `ineligible:name` | 445,067 / 440,335 | ~88.5% | Non-bare names (`.`, `::`, etc.) — the member-access pipeline |
| `ineligible:lang` | 57,454 / 57,441 | 11.4% | Non-migrated languages — mostly Rust (28.3k failed; 496 .rs files), plus objc/ruby/markdown |
| `store-bind`, `claimed`, `gated-import`, `ineligible:path` | 0 | 0% | None fired on this corpus |

Assessment — none of the tail is cheap coverage:

- **`ineligible:name` is load-bearing, not waste.** ~64k of the 445k failed; the other ~381k produced real member edges through the TS matchers (`matchDottedCallChain`/`matchMethodCall`/bound-receiver arms). Porting them is the member-resolution pipeline — Phase-5 scope. The only kernel-adjudicable slice is the prefilter-miss subset of the failed 64k (TS prefilter-miss is terminal for non-bare names — `matchJsStoreBindingCall` can't fire on a name with separators), ≤1.3% of the loop — not worth the segment-splitting prefilter port.
- **`ineligible:lang` (Rust) needs extractor work first**: the bindings table has 0 rust rows — `rustlang::extract` emits no `use`/`fn` bindings, so the kernel's import-join machinery has nothing to bind. Migrating rust = bindings emission + resolver audit, not a flag flip.
- **The `frameworkMerge` dispatches are real merges.** 3,232,822 handled refs carried a candidate list into `settleKernelOutcome`'s framework loop (split: no_candidates=140,232 / with_candidates=3,092,341). Skipping the `candidates=[]` subset is not verdict-safe under detected frameworks whose `resolve()` isn't claims-bounded — express (detected on this corpus) resolves middleware/controller/service name patterns independent of `claimsReference`, and `gateFrameworkLanguage` never gates `calls`. A safe skip needs an opt-in `resolve()` contract flag + per-resolver audit; deferred.

Phase 4 exit read: the kernel handles the entire bare-name migrated-language slice (91.4% of the loop, verdict-identical across every check); the remaining passthroughs are the deliberately-deferred member pipeline plus languages whose extractors don't emit bindings yet. §7a CPU target met. Coverage is at its design boundary — further native share is Phase 5 (member access), not Phase 4 tail.

### 5.14 Phase 5 step 1 — member-arm hit-rate instrumentation, corrected tail attribution (2026-09-17)

One `taskset -c 0-7` run on the 15c host, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2` (per-ref nm/stage rows, so wall time is not comparable to §5.13 timing arms — this run exists to count, not to measure): wall 6:15.44, MaxRSS 16.8 GB. Kernel: handled 5,366,728 / passthrough 497,776 (91.5%); `ineligible:name`=440,335, `ineligible:lang`=57,441. Output identical: 2,082,872 nodes / 6,412,563 edges. Log: `bench/20260917-node-8c-kernelon-memberarms.log`.

Instrumentation added (TS-only, diagnostic, verdict-neutral): `kernelReason` propagated onto passthrough refs and suffixed onto every `stage:`/`nm:`/outcome profile key, so each row is attributable to its kernel gate; new `nm:` tags on the previously-untagged member arms (`br:methodcall`/`br:import`/`br:factory`, `mc-infer-local(-btm/-rmot)`, `mc-infer-cpp(-btm)`, `mc-guarded`, `mc-iteration`, `mc-class`/`mc-capital`/`mc-byname`, `mc-phpprop`, `mc-rustself`, `mc-rustfield`, `mc-javafield`, store-accessor chain, deferredChain:*/deferredThisMember drains). `matchDeferredThisMember()` extracted from the drain loop for per-ref labeling. Parity suite + resolution tests green (260 tests, 5 files).

**`ineligible:name` attribution** (440,335 refs — §5.13's "~381k member edges" attribution was wrong; the measured split):

| resolvedBy | n | Producer arm |
|---|---|---|
| `import` | 327,899 | `resolveViaImport` — overwhelmingly the C/C++ `#include` branch (sibling-dir lookup → `resolveCppIncludePath` -I scan → file node), 405,592 of 413,976 `imports` edges are C |
| `file-path` | 60,261 | `matchByFilePath` — basename→file-node lookup on path-shaped names |
| `qualified-name` | 3,477 | `matchByQualifiedName` |
| `instance-method` | 1,223 | **the member matchers proper**: boundReceiver 778 + matchMethodCall 436 + ~9 |
| `exact-match` / `function-ref` | 88 / 29 | bare-name leftovers inside nameMatch |
| unresolved | 47,358 | `fail:calls` 46,378 + `fail:imports` 685 + extends/function_ref/references 295 |

By ref kind: `imports` refs are 391,782 of the bucket (89%) and resolve 99.8% of the time; `calls` refs are 48,217 (11%) and resolve only ~2,062 (4.3% — boundReceiver 1,311, viaImport 218, nameMatch 528, instance-method share above). The member-call pipeline the Phase-5 handoff scoped (~3k lines of `name-matcher.ts`) produces **~1.2k edges** on this corpus; ~46k member-syntax calls refs fail outright.

**Member-arm hit/miss detail** (calls refs, `ineligible:name`): br:methodcall 778/24,542, mc-infer-local-btm 693/~25k attempted, mc-infer-cpp 242, mc-class 65, mc-byname 418, mc-guarded /25,309 miss, mc-iteration /23,880 miss, mc-class /25,184 miss, mc-infer-local 1,319 hit /25,108 miss. Deferred drains nearly inert: deferredChain:scopedChain 547 miss + rmot-supers 263 miss (all rust `::`), zero deferredThisMember rows — the deferred-ref channel Phase 5 flagged as a prerequisite is unneeded by the dominant leg.

**`ineligible:lang` (57,441)**: ~28,966 resolved (exact-match 17,205, instance-method 5,730 — rust `self.*`/objc member arms, qualified-name 3,549, import 1,113, fuzzy 1,025), ~28,475 failed. Member matchers carry ~7k hits here but all for non-migrated languages — that workstream is extractor/bindings migration, not Phase 5.

**Port-order conclusion (measured)**: the dominant `ineligible:name` leg is C/C++ include-path resolution — ~388k edges (98.8% of the bucket's resolutions) through a ~150-line pure-DB surface (`resolveViaImport`'s c/cpp branch + `resolveCppIncludePath` + `matchByFilePath`; needs file-node-by-basename lookup, `fileExists`, include-dir list, path normalization — all already kernel-shaped, no source reads, no deferred channel). Porting it would lift native share ~7 points (91.5% → ~98%) in one leg. The member matchers are a ~3k-line port for ~1.2k edges on this corpus — still Phase 5, but demoted behind the include leg. Corpus caveat: linux is C-dominated, so member-call density is atypically low; on a JVM/TS corpus the member arms would weight higher — but per measured hits the include leg is unambiguously first.

### 5.15 Phase 5 step 2 — C/C++ include-path leg native, byte-identical output (2026-09-17)

Same host/arm shape as §5.14 (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; wall 1m 24s vs §5.14's profiled 1m 30s — settle 12.1→9.9s, insertEdges 52.4→41.6s). Log: `bench/20260917-node-8c-kernelon-includearm.log`; baseline DB preserved at `codegraph-corpora/baselines/linux-cf8b2607.db`.

| Metric | §5.14 baseline | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,366,728 | 5,734,189 | +367,461 |
| kernel passthrough | 497,776 | 110,315 | −387,461 |
| **native share** | 91.5% | **98.1%** | +6.6 pts |
| `ineligible:name` | 440,335 | 49,180 | −391,155 |
| `ineligible:lang` | 57,441 | 57,441 | 0 (untouched) |
| `member-tail` (new punt reason) | — | 3,694 | arm-internal punts to the TS spine |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all identical to baseline, and the full edge multiset (`source,target,kind,line,col,metadata` ordered) md5 `b9e532c2…` and node multiset md5 `139f79c9…` match byte-for-byte. Zero verdict drift: every ref the arm can't prove punts `member-tail`/`gated-import` into the full TS spine.

**What was ported** (`codegraph-kernel/src/resolve.rs`, +455): a dedicated arm for `(c|cpp, 'imports', non-bare)` ahead of the `name_is_bare` rejection, replicating the TS `resolveOneInner` ordering restricted to the slice — builtin → prefilter → viaImport → filePath — with the nameMatch tail (qualifiedName/cppChain/methodCall/exactName/fuzzy) deliberately left in TS behind `member-tail`. The include machinery (`resolveViaImport`'s c/cpp branch, `resolveCppIncludePath`, `fileExists`, include-dir list) was already in-kernel from an earlier phase but unreachable for non-bare names. Supporting ports: full `hasAnyPossibleMatch` (receiver/member segments around `.`/`::`/`:`/`$`, path tail, `localName.` import-prefix arm) and `matchByFilePath` (+`splitAnchor`/`splitFileSymbol`/`findSymbolInReferencedFile`/`findAnchoredMarkdownSection`/`pickClosestFileNode`/`normalizeMarkdownAnchor`/`decodeURIComponent`). Parity-test fixture extended with C includes (sibling, subdir, basename-only suffix hit, missing, stdlib) asserting `import`@0.92 / `file-path`@0.85 / `member-tail` passthrough verdicts natively.

**Remaining `ineligible:name` (49,180)**: calls-kind member syntax (boundReceiver/methodCall shapes — the deferred member-matcher port) plus the member-tail punts above. The 46k fail:calls majority never produced edges anyway; the measured member-matcher prize stays ~1.2k edges (§5.14). Next Phase-5 candidates by measured weight: `member-tail` misses (3.7k, cheap — extend the arm to fail them natively once the member arms they could hit are enumerated) and the `ineligible:lang` member arms (blocked on extractor/bindings migration, not Phase 5).

### 5.16 Phase 5 step 3 — member-access stage 1: boundReceiver DB sub-arms + qualifiedName native (2026-09-17)

Same host/arm shape as §5.15 (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; settle 9.9→7.9s, insertEdges 41.6→36.6s). Log: `bench/20260917-node-8c-kernelon-memberarms.log`; baseline DB preserved at `codegraph-corpora/baselines/linux-9f528109.db`.

| Metric | §5.15 baseline | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,734,189 | 5,775,030 | +40,841 |
| kernel passthrough | 110,315 | 89,474 | −20,841 |
| **native share** | 98.1% | **98.5%** | +0.4 pts |
| `ineligible:name` | 49,180 | **0** | eliminated |
| `ineligible:lang` | 57,441 | 57,441 | 0 (untouched) |
| `br-source` (new punt) | — | 25,320 | source-reading receiver inference delegated to TS |
| `member-tail` | 3,694 | 5,753 | now the whole unported nameMatch tail |
| `chain` (new punt) | — | 951 | `x().y` calls → matchReference-only arm |
| `via-src` (new punt) | — | 9 | objectLiteralAlias/instanceMember source reads |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all identical to baseline; edge multiset md5 `a1cd4791…`, node multiset `57f6fb95…`, and the full unresolved_refs multiset including `failure_reason` `a80221ea…` match byte-for-byte (md5s differ from §5.15's — this run's serialization includes line/col and ref rows).

**What was ported** (`codegraph-kernel/src/resolve.rs`, ~+1,070): `resolve_nonbare_ref` — `resolveOneInner`'s non-bare slice in TS order: builtin → prefilter → function_ref/jvm/php-imports punts → `resolvePhpImportedStaticCall` (terminal) → `matchBoundReceiverCall` (claim-and-decide: `br:import` for import-bound receivers + the ESM refusals — deep receiver / unbound root — are terminal natively; `br:methodcall`/`br:fieldinfer`/`br:factory` and java/kotlin boundtype punt `br-source`) → `isUnresolvedJsMemberCall` terminal → `x().y` chain punt → `resolveViaImport` member descent (go `pkg.F`, java/kotlin qualified imports, python module-member + absolute-module, `localName.member` descent: staticMember/objectLiteralMember; objectLiteralAlias/instanceMember punt `via-src`) → arkts `.`-prefix punt → filePath → `matchByQualifiedName` (exact @0.95 + call-site-file preference + last-segment suffix @0.85) → `member-tail` punt. Alias forward extended with `memberName` (`{member: fn}` shorthand/keyed bindings). `settleKernelOutcome` (index.ts) now runs `applyResolveTail` on kernel-null verdicts — `unknown-receiver` stamping on boundReceiver refusals was unreachable before this arm existed.

**Attribution check**: `br:methodcall` hit 778 + `via-src` hit 9 downstream in TS — the source-reading arms are correctly delegated, not lost. `member-tail`→`qualified-name` rows (~3k) are deferred refs (this-member/chain conformance pass) — the deferred channel owns them by design.

### 5.17 Phase 5 step 4 — member-access stage 2: source-backed receiver inference native (2026-09-17)

Same host/arm shape as §5.16 (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; loop-stages settle 7.9→9.4s, insertEdges 36.6→60.5s — the vitest suite ran concurrently during the resolve leg, so these aren't clean timing reads). Log: `bench/20260917-node-8c-kernelon-sourceinfer.log`.

| Metric | §5.16 baseline | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,775,030 | 5,794,627 | +19,597 |
| kernel passthrough | 89,474 | 64,877 | −24,597 |
| **native share** | 98.5% | **98.9%** | +0.4 pts |
| `ineligible:lang` | 57,441 | 57,441 | 0 (untouched) |
| `br-source` | 25,320 | **0** | eliminated — the whole source-backed slice is native or attributed-punt |
| `member-tail` | 5,753 | 5,750 | ~0 (the remaining unported nameMatch tail) |
| `chain` | 951 | 951 | 0 |
| `via-src` | 9 | 9 | 0 |
| `btm-supers` (new punt) | — | 726 | member-miss on a resolved owner — TS walks live supertype edges the snapshot can't see |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — counts identical to `baselines/linux-9f528109.db`, and the edge / node / unresolved_refs (incl. `failure_reason`) multisets compare byte-identical (sha256 `c7417d57…` / `67c47a1d…` / `470e3903…` — different serialization than §5.16's md5s, same content).

**What was ported** (`codegraph-kernel/src/resolve.rs`, ~+2,260 net; `__tests__/kernel-resolve-parity.test.ts` +82):

- **Schema**: `KNode` gained `return_type`, `type_parameters`, `decorators` (both JSON → `Vec<String>` via a small `parse_json_string_array`); `NODE_COLS` extended.
- **Inference**: `infer_local_receiver_type` + `local_receiver_type_patterns` for all migrated languages (anchored scope-bounded backward scan, `enclosing_scope_start_line`, `normalize_inferred_type_name`, JS-faithful lookahead guard), `infer_cpp_receiver_type` (declarator/`auto`/call-result/header fallback), `php_property_type_patterns` + assigned-property second-chance, `infer_java_field_receiver_type`, `match_ts_field_call_bound` (ESM `br:fieldinfer` owner-scan).
- **Terminals**: `resolve_bound_type` (java type-parameter scopes, import binding → viaImport + jvm fallback, php qualified owner, `!binding && !ESM` same-file/package candidates, owner-kind gate), `match_bound_type_member` (`QName::method` member filter; miss → `btm-supers` punt — TS's supertype BFS reads live edges), `resolve_method_on_type` (preferred-FQN + call-site-file disambiguation; miss → `rmot-supers` punt).
- **Matchers**: `match_go_factory_receiver` (param/typed-decl → btm; `:=` first-result-only → callee `return_type` → btm at callee's def site), `match_go_field_chain_call` (two-hop `base.field.Method`, in-module + builtin-field-type guards), `esm_factory_tail` (`br:factory` — UTF-16-correct initializer parse, await gate, `new Owner()` + `factory()` callee forms, `Promise<T>` unwrap on awaited only, `!returnType` bail).
- **Punt gates** for tree-dependent inferrers: `mc-guarded` (php `instanceof`), `mc-iteration` (go `range` / kotlin `it`/`->`), `mc-await` (ESM `const x = await` declarations).
- **`bound_receiver_claim` rewired**: java/kotlin import bindings run `resolve_bound_type` first (owner → btm or deeper-receiver refusal; miss → `br:import` descent); non-ESM local/phpVariable receivers → `match_method_call`; ESM local receivers → 2-part `br:fieldinfer` → `match_method_call` → param refusal → `br:factory` tail. `matchMethodCall` arm order preserved exactly (guarded-gate → infer → gofactory → iteration → await → btm terminal → gofield → javafield → mc-literal → strat1 → receiver-evidence refusal).
- **Fidelity details**: `requireReceiverEvidence` passed as `preserveQualifiedName` to both inferrers; `binding?.nodeId !== holder.id` object-literal evidence rule (absent binding skips all holders); `{...ref, line: binding?.line}` btm site anchoring; JS `split(/\s+/)[0]` leading-whitespace semantics on type parameters; empty-class-name falsy check; UTF-16 receiver slicing throughout.

**Fixture coverage** (parity test): `svc.run` infer→btm @0.9, `svc.call` prefilter-pass → btm miss → `btm-supers` punt → passthrough, `made.run` factory tail null (`return_type` unpopulated for TS — identical bail), `k.mymethod` java field/param infer @0.9, `svc.Run` gofactory callee-return-type → btm @0.9, `o.in.Do` gofield two-hop @0.85.

**Where the 25,320 `br-source` refs went**: ~24.6k now verdict natively (the measured ~1.2k member edges reproduce in-kernel — byte-identical edge multiset); 726 punt `btm-supers` (member-miss on a proven owner — conservative delegation, not a coverage gap). Remaining member punts: `member-tail` 5,750 (nameMatch tail: methodCall/exactName/fuzzy on non-bindingReceiver shapes), `chain` 951, `via-src` 9 — all deferred-conformance or source-alias territory by design.

### 5.18 Phase 5 step 5 — upstream-merge audit + C/C++ include-arm qualifiedName leg (2026-09-18)

**Merge audit** (`d19ab145` = upstream merge into `fork/consolidated`): upstream's resolution refactor (`8dcc52d3` retained qualified chains in kernel extraction, `889c3a9d` typed-call/store-binding arms, `b909ef2b` TS-extractor mirror — all under `4297b8e2`, landed 2026-09-16 via `e5952c45`) **predates** stage-1/2, so the kernel arms were written against the refactored spine. `d19ab145` itself changed only `README.md`. Zero drift: `resolve_nonbare_ref` order, `is_unresolved_js_member_call`, the `store-bind` punt, and `same_language_family` all already mirror current TS.

**Re-baseline** (`taskset -c 0-7`, node v26.9.0, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`): kernel-on 6:03.67 / 16.1GB MaxRSS; kernel-off 4:40.16 / 11.9GB. Byte-identical across arms — nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521; multisets match §5.17's sha256s exactly (`c7417d57…` / `67c47a1d…` / `470e3903…`), i.e. the upstream merge produced **zero** corpus-output delta. Preserved at `baselines/linux-d19ab145.db`. Logs: `bench/20260918-node-8c-kernel{on,off}-rebaseline.log`.

**Re-attribution** (kernel-on vs kernel-off, punts re-run in TS): `ineligible:lang` 57,441 / `member-tail` 5,753 / `chain` 951 / `btm-supers` 726 / `via-src` 9. Inside `member-tail`: TS-side `qualified-name` **3,047** — C/C++ include refs (`math.h`, `sys/ioctl.h`, …) that resolve to indexed `import`/`file` nodes by qualified name; `instance-method` 473; `exact-match` 88; ~2,145 genuine misses. `chain` 951: all TS-side misses on this corpus. `btm-supers` 726: 67 resolve via live-supertype BFS — confirmed permanent punts.

**Leg A — the qualified-name gap**: `resolve_c_include_import_ref` ran `resolve_via_import → match_by_file_path → member-tail punt`, skipping `match_by_qualified_name` entirely, while TS's `matchReference` runs filePath→qualifiedName in order. Fix (~10 lines): filePath-miss → `match_by_qualified_name` before the member-tail punt — the matcher is kind-agnostic and every downstream gate (`gate_language`, `is_visible_across_files`, `gate_target_kind`, framework merge) applies identically.

| Metric | Re-baseline | Leg A | Δ |
|---|---|---|---|
| kernel handled | 5,799,624 | 5,802,671 | +3,047 |
| kernel passthrough | 64,880 | 61,833 | −3,047 |
| `member-tail` | 5,753 | **2,706** | −3,047 (the qualifiedName hits, exactly) |
| `chain` / `btm-supers` / `via-src` / `ineligible:lang` | 951 / 726 / 9 / 57,441 | unchanged | 0 |

**Output identity (the gate)**: Leg A run 5:05.02 / 16.6GB MaxRSS (`bench/20260918-node-8c-kernelon-lega.log`) — nodes/edges/failed-refs counts and all three multisets **byte-identical** to the re-baseline (`c7417d57…` / `67c47a1d…` / `470e3903…`). The 3,047 edges moved from TS-resolved to kernel-resolved with zero graph delta. Post-leg snapshot at `baselines/linux-d19ab145-lega.db`. Parity fixture updated (`missing/none.h`, `stdio.h` → `qualified-name` @0.95 on their own import nodes); 6/6 parity + 223/223 resolution tests green, ast-grep kernel rules clean.

### 5.19 Phase 5 step 6 — chain arms native: cppChain / scopedChain / dottedChain (2026-09-18)

Same host/arm shape (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; 5:47.74 wall / 15.6GB MaxRSS — first attempt OOM-killed against a transient ~12GB co-tenant, rerun clean on an idle host). Log: `bench/20260918-node-8c-kernelon-chains-2.log`; post-leg snapshot `baselines/linux-d19ab145-chains.db`.

| Metric | §5.18 (Leg A) | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,802,671 | 5,807,635 | +4,964 |
| kernel passthrough | 61,833 | 61,869 | +36 (pass-event count, not verdicts) |
| `member-tail` | 2,706 | **2,664** | −42 (native chain hits) |
| `rmot-supers` (re-attributed) | — | 65 | chain-arm rmot misses — TS's live supertype walk still owns them |
| `chain` | 951 | 951 | 0 — the ts/js/py `().` guard is unchanged (storeAccessorChain stays TS) |
| `btm-supers` / `via-src` | 726 / 9 | 726 / 9 | 0 |
| `ineligible:lang` | 57,441 | 57,454 | +13 pass-count artifact — output byte-identical |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all three multisets **byte-identical** to the Leg-A baseline (`c7417d57…` / `67c47a1d…` / `470e3903…`). The passthrough-sum deltas (+36 total, +13 ineligible) are per-pass event counts — deferral timing shifted when chain refs resolved natively in pass 1 instead of riding the TS deferred drain; every verdict is unchanged.

**What was ported** (`codegraph-kernel/src/resolve.rs`, ~+140): `match_call_chain` — matchReference's per-language chain dispatch, inserted between `match_by_qualified_name` and the `member-tail` punt exactly where TS runs it (sequential early-return arms, not pooled): `match_cpp_call_chain` (c/cpp — `<inner>().<method>` via `resolve_cpp_call_result_type` → rmot @0.85 `instance-method`), `match_scoped_call_chain` (php/rust — `Cls::factory().m`, `::` required, `self` marker → factory class), `match_dotted_call_chain` (the 9-language dot list — Go bare-inner `New().M` via callee return_type, CONSTRUCTS_VIA_BARE_CALL ctor receiver, `Cls.factory().m` via `Cls::factory` return type, objc/pascal convention arms verbatim though unreachable). All compose stage-2 helpers (`resolve_cpp_call_result_type`, `lookup_callee_return_type`, `imported_fqn_of`, `resolve_method_on_type`). **Punt fidelity**: Go's bare-fallback (`matchByExactName ?? matchFuzzy`) is unported → `member-tail` punt; rmot misses → `rmot-supers` punt; either way TS's rerun produces the identical verdict. ts/js/py `().` refs still punt `chain` at the earlier guard — storeAccessorChain's JS arm is source-bound and its python arm is near-zero yield.

**Fixture coverage** (parity test +45 lines): `Pool::instance().drain` → `Pool::drain` @0.85; `Pool::instance().nope` → `rmot-supers` punt → passthrough; `Registry::make().name` → `self`→factory-class → `Registry::name` @0.85; `NewService().Run` → go bare-inner @0.85; `nosuch().Run` → member-tail punt → passthrough; `J2.getK().mymethod` → java dotted factory @0.85. 6/6 parity + 223/223 resolution tests green; ast-grep kernel rules clean.

### 5.20 Phase 5 step 7 — unbound method-call arm native: matchMethodCall requireReceiverEvidence=false (2026-09-18)

Same host/arm shape (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; 5:25.31 wall / 16.0GB MaxRSS). Log: `bench/20260918-node-8c-kernelon-memberfree.log`; post-leg snapshot `baselines/linux-d19ab145-memberfree.db`.

| Metric | §5.19 (Leg B) | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,807,635 | 5,808,105 | +470 |
| kernel passthrough | 61,869 | 61,399 | −470 |
| `member-tail` | 2,664 | **2,080** | −584 |
| `rmot-supers` (re-attributed) | 65 | 179 | +114 — free-arm rmot misses; TS's live supertype walk still owns them |
| `chain` / `btm-supers` / `via-src` | 951 / 726 / 9 | identical | 0 |
| `ineligible:lang` | 57,454 | 57,454 | 0 |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all three multisets **byte-identical** to the Leg-B baseline (`c7417d57…` / `67c47a1d…` / `470e3903…`). TS-side resolutions of `member-tail`-punted refs: `instance-method` **436 → 0** (every prior recovery now native), `exact-match` 88 and `function-ref` 29 unchanged (unported source-bound matchers — permanent punts). The −584 member-tail delta splits into 470 native hits + 114 re-attributed `rmot-supers` punts.

**What was ported** (`codegraph-kernel/src/resolve.rs`, ~+330): `match_method_call_free` — the `requireReceiverEvidence=false` half of `matchMethodCall`, reached only by refs the boundReceiver claim never takes (`this.`/`self.`/`super.`/`cls.` roots, non-`calls` kinds, `()`-chain names after the chain arms miss). Wired at both `member-tail` dispatch sites (the main `name_cand` chain after `match_call_chain`, and the c-include arm after `match_by_qualified_name`), preserving TS's sequential early-return order: PHP `$this->prop` exclusive → dot/colon prelude (cpp `operator` fallback) → infer → rmot @0.9 (java/kotlin `importedFqn`) → `mc-await` punt → ESM builtin/primitive bail (`TS_PRIMITIVE_TYPES`, 12 entries — kills Strategy-3 guesses on `new Array()`/`string` receivers) → gofield exclusive (deep receivers) → `this.field` exclusive → `match_ts_this_field_call` (innermost enclosing type; typeof → object-literal holders @0.85 or terminal null; `^[A-Z]` type gate; >1 declared → directory-nearest @0.85, `localeCompare` tie → `mc-tfield-ambig` punt; else rmot @0.85) → javafield non-exclusive → mc-literal (all holders, no binding filter — TS's check is evidence-gated) → strat1 kind+lang @0.85 `qualified-name` → strat2 capitalized @0.8 → strat3 ceiling + same-lang + unique@0.7 / overlap+lang@0.65. New helpers: `match_ts_this_field_call`, `match_ts_field_call_free`, `split_camel_case`, `TS_PRIMITIVE_TYPES`.

**Fixture coverage** (parity test, `src/notify.ts` + `src/svc.ts::split` bait): `this.mailer.send` → thisfield → `Mailer::send` @0.85; `svc4.run` (references) → infer → `Service::run` @0.9; `api2.localcall` → object-literal @0.85; `Engine.start` → strat1 @0.85 `qualified-name`; `engine.start` → strat2 `Engine` @0.8; `mystery.frobnicate` → strat3 unique @0.7; `betaThing.handle` → strat3 word-overlap `Beta` @0.65; `adat2.run` → `mc-await` punt; `arr.split` (`new Array<string>()`) → builtin bail → punt (the `Service::split` bait would claim @0.7 if the bail were missing). Two fixture corrections encoded real dispatch semantics: calls-kind unbound `x.y` is claimed-and-refused terminally (strat arms pin through `references` seeds), and function-local receivers aren't `known_names` (prefilter resolves them `unresolved` — the bail pin needs the member segment known). 6/6 parity + 223/223 resolution + 4,996/4,996 full suite green; ast-grep kernel rules clean.

**Permanent-punt ledger** (why each remaining bucket stays in TS):

| Punt reason | n | Why it stays |
|---|---|---|
| `ineligible:lang` | 57,454 | unmigrated languages (mostly Rust) — needs extractor bindings, not resolver arms |
| `member-tail` | 2,080 | genuine misses (~1,963) + `exact-match` 88 + `function-ref` 29 TS-side hits — `isLocallyBoundJsName`/`applyCppCallSiteForm`/the dedicated function_ref matcher are source-bound |
| `chain` | 951 | ts/js/py `().` store-accessor arm — `resolveStoreAction` is source-bound (JS); python arm is near-zero yield |
| `btm-supers` | 726 | boundReceiver's live supertype BFS — no snapshot equivalent |
| `rmot-supers` | 179 | rmot's supertype walk — same |
| `via-src` | 9 | objectLiteralAlias/instanceMember source reads |
| `mc-await`, `mc-guarded`, `mc-iteration`, `gofactory`, `mc-tfield-ambig` | (inside above) | evidence-gated or source-bound arms — punted conservatively, TS rerun produces identical verdicts |

**Clean timing** (the one uninstrumented post-Phase-5 run — `taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `SYNTH_TIMINGS=1` only, no `RESOLVE_PROFILE`; n=1, host idle): wall **5:16.39 (316.4s) / 15.8GB MaxRSS**; resolution phase 188.0s, callback-synthesis 68.9s, parse-loop 75.2s. Log: `bench/20260918-node-8c-kernelon-memberfree-clean.log`. Against §5.13's clean Phase-4 arms (total 292.6s; resolution 154.6–171.4s; synthesis 59.0s; parse 72.5s) the wall is ~8% slower — inside run-to-run variance at n=1, and directionally expected: the new native arms do real work (source scans, rmot, file reads) on refs that used to punt early, buying coverage (91.4%→99.0% native) rather than speed. The floor is unchanged — main-thread persist + synthesis, exactly as §5.11 recorded.

### 5.21 Phase 5 step 8 — scoped function_ref arm native + residual audits: Phase 5 closes (2026-09-18)

Same host/arm shape (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; ~5:07 wall — no MaxRSS wrapper this run). Log: `bench/20260918-node-8c-kernelon-funcref.log`; post-leg snapshot `baselines/linux-d19ab145-funcref.db`.

| Metric | §5.20 (Leg C) | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,808,105 | 5,803,170 | −4,935 pass-event count (deferral timing — same artifact class as §5.19's +13; verdicts identical, see the gate below) |
| kernel passthrough | 61,399 | 61,334 | −65 |
| `member-tail` | 2,080 | **2,028** | −52 (29 native `::` member-pointer hits + ~23 pass-event recounts) |
| `ineligible:lang` | 57,454 | 57,441 | −13 pass-event artifact |
| `chain` / `btm-supers` / `rmot-supers` / `via-src` | 951 / 726 / 179 / 9 | identical | 0 |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all three multisets **byte-identical** to every baseline since §5.17 (`c7417d57…` / `67c47a1d…` / `470e3903…`). TS-side resolutions of `member-tail`-punted refs: `function-ref` **29 → 0** (every `::` member-pointer recovery now native), `exact-match` 88 unchanged (permanent, see below), `instance-method` still 0.

**What was ported** (`codegraph-kernel/src/resolve.rs`, ~+78): `match_function_ref_scoped` — matchFunctionRef's `::` member-pointer arm, the only non-bare shape it resolves (`Cls::member`: member-name lookup → function/method + same-language-family + origin-excluded + qualifiedName-equality-or-`::`-suffix filters → same-file pool by earliest line @0.9, cross-file unique-or-drop). `resolve_nonbare_ref`'s function_ref gate now mirrors TS's block order: `resolve_via_import_member` with the callable-target gate (`a.b` function_refs still claim through member-descent imports) → scoped arm → `member-tail` punt. `this.` function_refs fall through to the punt — `resolveThisMemberFnRef` stays TS-side and the punt is verdict-safe.

**Residual audits (the closeout investigations)**:

- **`ineligible:lang` (57,441)** — 99.4% of the failed share is **Rust** (28,301; objc 160, ruby 10, markdown 9); ~29k more resolve TS-side (exact-match 17,205 / instance-method 5,730 / qualified-name 3,549 / import 1,113 / fuzzy 1,025 / function-ref 339). Porting it is a Rust-resolution workstream — migrated-language eligibility + self/field/trait/use binding logic — not a resolver arm. Confirmed out of scope.
- **Supertype walks (`btm-supers` 726 + `rmot-supers` 179)** — confirmed **permanent**. TS's `getSupertypes` walks *resolved* `implements`/`extends` edges "empty in the first resolution pass, populated in the conformance pass" (index.ts) — the kernel's pre-resolution snapshot cannot see them, and walking unresolved refs would re-implement resolution inside the walk plus replicate pass ordering. Far outside the byte-identity risk envelope.
- **`matchByExactName` (88 TS-side hits)** — confirmed **permanent**: `isLexicallyReachable`/`isSealedModule`/`isLocallyBoundJsName`/`isBoundToBareImport`/`applyCppCallSiteForm`/`findBestMatch`/`computePathProximity`/`isCrossFileReachable` — source reads plus multi-signal scoring for 88 hits.

**Fixture coverage** (parity test): `W::m` (w.cpp) → scoped arm @0.9 `function-ref`; `Pool::missing` → member-tail punt → passthrough; `api.call` (function_ref, main.ts) → member-descent import @0.9. 6/6 parity + 223/223 resolution + 4,996/4,996 full suite green; ast-grep kernel rules clean; clippy `-D warnings`.

**Phase 5 closes here.** Every remaining punt is attributed and justified above or in §5.20's table; the only unported matcher with meaningful yield is Rust resolution, a different workstream.

### 5.22 Genuine-miss audit — the `unknown-receiver` bucket (2026-09-18)

Refs that fail in **both** engines — not migration debt, actual unresolved names. `unresolved_refs` on the Leg-D snapshot (`baselines/linux-d19ab145-funcref.db`): `unknown-receiver` = 33,945 dotted-name failures (c 20,415 / python 13,214 / cpp 316) plus ~4k NULL-reason dotted (rust 8,829 / python 1,950 / c 987 / c imports 711).

**Taxonomy** (member-tail existence check — does the dotted member exist as ANY node in the corpus):

| Class | Share | Verdict |
|---|---|---|
| Member absent from the graph — external modules (python `os.path`/`re`/`sys`/`libevdev`/`gdb`), C function-pointer struct fields (`btcoexist.btc_get`, `rtc.read`), macro-synthesized names | 11,738/20,415 c refs (57%), 5,040/13,214 py refs (38%) — 75% of distinct c tails, 55% of py tails have **zero nodes** | **Correctly unresolved** — no target exists to point at |
| Receiver-type inference limits — local-var receivers (`priv`, `sk`, `buff.append`, `m.group`) whose types need intra-function data-flow | the remaining ~16k | **Out of scope** — recovery needs real type inference, a different axis |
| C ops-struct calls (`ops->read`) | subset of the above | recoverable only with function-pointer field nodes + C receiver inference — an enhancement that *changes the node set*, not a byte-identical leg |

**Conclusion**: no quick wins. The bucket is dominated by legitimately-unresolvable names — `unknown-receiver` doing its job. The one technically-recoverable class (C ops-structs) is an extraction+inference workstream, not a resolver arm.

### 5.23 Rust leg R1 — `::` module-path arm native ahead of the eligibility punt (2026-09-18)

Same host/arm shape (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; ~6:01 wall). Log: `bench/20260918-node-8c-kernelon-rustpath.log`; post-leg snapshot `baselines/linux-d19ab145-rustpath.db`.

| Metric | §5.21 (Leg D) | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,803,170 | 5,814,457 | +11,287 (native hits + pass-event recounts) |
| kernel passthrough | 61,334 | 60,047 | −1,287 |
| `ineligible:lang` | 57,441 | **56,131** | −1,310 (the native `::` path-arm hits) |
| `member-tail` / `chain` / `btm-supers` / `rmot-supers` / `via-src` | 2,028 / 951 / 726 / 179 / 9 | 2,051 / 951 / 726 / 179 / 9 | +23 pass-event recounts |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all three multisets **byte-identical** to every baseline since §5.17 (`c7417d57…` / `67c47a1d…` / `470e3903…`). TS-side `import|ineligible:lang` recoveries: **1,113 → 0** — every Rust module-path recovery now native.

**What was ported** (`codegraph-kernel/src/resolve.rs`, ~+190): `resolve_rust_path_ref` + `match_rust_path_reference` + `resolve_rust_module_file` + `rust_resolve_under` + `rust_crate_root_dir` + `rust_self_module_dir` — TS's `resolveViaImport → resolveRustPathReference` slice, run **before** the `ineligible:lang` punt for `language=='rust'` pure-`::` names: split `A::B::C` into module prefix + leaf; map the prefix to a file (`crate` → crate-root `lib.rs`/`main.rs` walk ≤64; `self` → self-module-dir — `mod.rs`/`lib.rs`/`main.rs` own their dir, `foo.rs` → `foo/`; leading `super`s walk up; bare → self-relative then crate-relative); each segment → `<seg>.rs` or `<seg>/mod.rs` via `file_exists`; leaf → kind-gated node in that file (`function|struct|union|enum|trait|type_alias|constant|method|class|interface`) → `import` @0.9. **Fidelity gates**: `function_ref` excluded (TS's block discards non-callable path hits — punt covers it); `::`+`.` names excluded (boundReceiver-claim territory); unreadable file → punt (TS's `imports.empty && !readFile` early-null falls to matchReference); prefilter miss → terminal `unresolved` (store-binding is JS-dead for rust). Mid-path `self`/`crate`/`super` skip semantics, `a::::b` collapse, and self-file exclusion all match verbatim.

**Fixture coverage** (parity test, `src/lib.rs` crate root + `sub.rs`/`deep/{mod,inner,sib}.rs`/`user.rs`): `crate::`/`self::`/`super::`/bare anchors → `import` @0.9; `<seg>/mod.rs` multi-segment; `ext::module::leaf_fn` external-crate punt; `Widget::new` struct-not-module punt; `crate::sub::missing` prefilter → `unresolved`; `a::b.c` stays punted. 6/6 parity + 223/223 resolution + 4,996/4,996 suite green; clippy `-D warnings`; ast-grep clean.

**Remaining Rust workstream** (scoping, not landed): `use`-path import rows via `bindings` emission (Phase-3-equivalent in `rustlang.rs`), the `BINDINGS_LANGUAGES`/`is_migrated_language` flip (unlocks the generic arms), `self.`/`Self::` receiver arms (enclosing impl type), trait-method dispatch (`v0.clone` → `Clone` impl), `impl Trait for T` `implements` refs. Generics (`Self`/`T`/`V`), external crates (`core::`/`serde::`), and macro-synthesized names stay unresolvable by design.

### 5.24 Rust leg R2 — eligibility flip: rust enters the native pipeline (2026-09-18)

Same host/arm shape (`taskset -c 0-7`, node v26.9.0, kernel-on, linux corpus, `CODEGRAPH_RESOLVE_PROFILE=2`; ~6:02 wall, MaxRSS 16.9GB). Log: `bench/20260918-node-8c-kernelon-rustflip.log`; post-leg snapshot `baselines/linux-d19ab145-rustflip.db`.

| Metric | §5.23 (Leg R1) | This leg | Δ |
|---|---|---|---|
| kernel handled | 5,814,457 | 5,821,013 | +6,556 (native hits + pass-event recounts) |
| kernel passthrough | 60,047 | **28,491** | **−31,556** |
| `ineligible:lang` | 56,131 | **230** | −55,901 (rust refs now pipeline-native or re-attributed) |
| `member-tail` | 2,051 | 24,763 | +22,712 (rust dot-bearing receiver shapes land here by design) |
| `rust-inh` | — | 1,633 | new bucket: rust `implements`/`extends` punts |
| `chain` / `btm-supers` / `rmot-supers` / `via-src` | 951 / 726 / 179 / 9 | 951 / 726 / 179 / 9 | unchanged |
| **native share** | 99.0% | **99.5%** | +0.5 pts |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all three multisets **byte-identical** to every baseline since §5.17 (`c7417d57…` / `67c47a1d…` / `470e3903…`).

**What changed** (`codegraph-kernel/src/resolve.rs`): `is_migrated_language` now includes `rust` — the audit showed ~29k rust refs resolve TS-side through generic arms **without bindings rows** (empty both sides), so the flip alone is byte-identical-safe. The `::` path arm hoisted above the gate: a module-path *miss* now falls through to the normal pipeline (qualified-name arm mirrors TS's `matchReference` continuation — `Widget::new` resolves natively) instead of punting outright; built-in/prefilter misses stay terminal, unreadable files stay punts.

**Two fabrication gates discovered and added** (each caught by a test-suite decline pin):
- **Dot-bearing names → `member-tail` punt.** `self.inner.take()` where `inner: Option<Inner>` must NOT resolve `Inner::take` (Option doesn't auto-deref) — TS's rust `self.`-receiver arm (enclosing impl → field type → auto-deref/container decline) is unported; without the gate the free arm's strat3 unique-method path fabricated the edge. `x.y`, `self.x`, `a::b.c` all stay TS-side.
- **`implements`/`extends` → `rust-inh` punt.** `impl Error for MapperError` with `use std::error::Error` must FAIL — the supertype is bound to a stdlib-rooted `use` path (out-of-repo); TS's inheritance locality gate drops it, and name-matching the local `type Error = String` would fabricate the edge. The gate is unported; 450 legit trait impls still resolve via TS rerun.

**Residual TS-side recoveries** (post-flip): `instance-method|member-tail` 5,245 (rust receiver calls — TS's self-field/trait arms), `qualified-name|member-tail` 1,155, `exact-match|rust-inh` 450, `exact-match|member-tail` 88 (permanent `matchByExactName`), `exact-match|ineligible:lang` 52 (objc/ruby tail).

**Gates**: 6/6 parity (updated `Widget::new` resolved pin + `a::b.c` member-tail punt), 223/223 resolution, 12/12 reference-target-kind, 4,996/4,996 suite, clippy `-D warnings`, ast-grep clean.

**Remaining Rust workstream** (scoping, not landed): `self.`/`Self::` receiver arms (enclosing-impl + field-type inference — the 5,245 `instance-method` punts), trait-method dispatch (`v0.clone` → `Clone`), the `rust-inh` locality gate (stdlib-rooted `use` → out-of-repo supertype), `use`-path bindings emission for genuinely *new* resolutions (enhancement, not migration — emitting bindings would change TS's graph too). Generics/external crates/macro names stay unresolvable by design.

### 5.25 Rust legs R3+R4 — `self.` receiver arms + inheritance locality gate native (2026-09-18)

| Metric | R2 (`b20c59dc`) | R3+R4 | Delta |
|---|---:|---:|---:|
| kernel handled | 5,821,013 | **5,839,053** | +18,040 |
| kernel passthrough | 28,491 | **25,451** | −3,040 |
| `member-tail` | 24,763 | 22,829 | −1,934 (`self.` receiver hits now native) |
| `rust-inh` | 1,633 | **0** | bucket eliminated — locality gate owns the verdicts |
| `rmot-supers` | 179 | 706 | +527 — self-field arms reach `resolveMethodOnType`'s supers-walk punt |
| `chain` / `btm-supers` / `ineligible:lang` / `via-src` | 951 / 726 / 230 / 9 | 951 / 726 / 230 / 9 | unchanged |
| **native share** | 99.5% | **99.6%** | |
| wall / maxRSS | 6:02 / 16.9GB | 5:29 / 15.9GB | |

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all three multisets **byte-identical** to every baseline since §5.17 (`c7417d57…` / `67c47a1d…` / `470e3903…`). Snapshot `baselines/linux-d19ab145-rustrx.db`.

**What changed** (`codegraph-kernel/src/resolve.rs`):
- **`match_rust_self_call`** — `self.m`: owner = enclosing impl type off the caller's `qualified_name` prefix; resolves only `Owner::m` methods (no same-name decoys); `qualified-name` @0.9.
- **`match_rust_self_field_call`** — `self.f.m`: finds the owner struct, reads its declaration lines (bounded file-line cache — same mechanism as `isRustTraitImplMethod`), regexes `f: <type>` out of them, normalizes through a ported `rustFieldTypeName` (strips `&`/`mut`/lifetimes, `Box`/`Rc`/`Arc`, `dyn`/`impl`, generics, trait bounds; rejects primitives/external/single-letter), then `resolveMethodOnType` @0.85 `instance-method`. Exclusive: inference failure returns null — never falls to bare-name strategies (the Option-autoderef decline pins stay honored). Supers-walk misses punt `rmot-supers`.
- **`is_bound_to_out_of_repo_import` rust branch** — ported `collectRustUseBindings` (nested-brace `use` expansion, `as` aliases, glob skip) + `RUST_STDLIB_ROOTS` + the already-ported `resolveRustModuleFile`. `impl Error for MapperError` bound to `use std::error::Error` is dropped as out-of-repo instead of adopting the local `type Error = String`; in-repo traits resolve. The whole `rust-inh` punt bucket went native.
- Wired into the free method-call arm at the exact TS slot (gofield → rustfield → rustself → thisfield).

**Divergence caught by the gate**: the first R3+R4 run had the dot-gate removed entirely (reasoning: non-self `x.y` had "no rust-specific TS arm"). Byte-compare **failed** — 413 edges, all `calls`, all identical source/target/kind, differing only in `confidence`: kernel strat arms produced 0.7/0.8 where TS produced 0.9. Root cause: `inferLocalReceiverType` (`let ctx: Ctx` source inference) is **not** language-gated — it runs for rust in TS's `matchMethodCall` prelude and feeds `resolveMethodOnType` @0.9. Same target, fabricated metadata = still a byte-identity failure. Fix: the member-tail gate now punts rust refs containing `.` unless they're `self.`-rooted calls. `x.y` local-var inference is a separate leg (R5); the ported `self.` arms had **zero** diverged edges.

**Gates**: 6/6 parity (new pins: `self.new` → `Widget::new` @0.9 qualified-name, `self.inner.new` → @0.85 instance-method, `self.unknown.new` decline→member-tail, `Error`/`Local` implements locality via prerequisite batch), 223/223 resolution, 12/12 target-kind, 4,996/4,996 suite, clippy `-D warnings` (lib target), ast-grep clean.

**Remaining Rust surface**: `x.y` local-var receiver inference (`inferLocalReceiverType` port — source-reading decl inference; the 413-edge class this leg re-punted), trait dispatch through `getSupertypes` (permanent — §5.21), `Self::` associated items, bindings emission (enhancement, not migration).

### 5.26 Rust leg R5 — `x.y` local receiver inference native (2026-09-18)

| Metric | R3+R4 (`d999a847`) | This leg | Delta |
|---|---:|---:|---:|
| kernel handled | 5,839,053 | **5,836,551** | −2,502¹ |
| kernel passthrough | 25,451 | **22,953** | −2,498¹ |
| `member-tail` | 22,829 | 16,641 | −6,188 (admitted `x.y` calls now resolve natively or punt deeper) |
| `rmot-supers` | 706 | 4,396 | +3,690 — admitted receivers reach `resolveMethodOnType`'s supers-walk punt |
| `chain` / `btm-supers` / `ineligible:lang` / `via-src` | 951 / 726 / 230 / 9 | 951 / 726 / 230 / 9 | unchanged |
| **native share** | 99.6% | **99.6%** | |
| wall / maxRSS | 5:29 / 15.9GB | 5:49 / 16.6GB | |

¹ The `handled`+`passthrough` counter sum shifted by exactly 5,000 (−5,000 vs §5.25) — attributed: the counters only cover `pool`/`kernel`-mode batches; `seq` batches (pre-pool-boot and below the parallel threshold) go uncounted. This run logged 25,579 seq refs (5 × 5,000 during the ~9.7s pool boot + the 579 tail, zero `batch kernel` lines — the lazy main-thread resolver never engaged); §5.25's run had 20,579. The delta is one batch's worth of scheduling, not a ref delta — seq resolves through the identical TS path. Proof is the gate below: all three output multisets are byte-identical.

**Output identity (the gate)**: nodes 2,082,872 / edges 6,412,563 / failed refs 2,052,521 — all three multisets **byte-identical** to every baseline since §5.17 (`c7417d57…` / `67c47a1d…` / `470e3903…`). Snapshot `baselines/linux-d19ab145-rustlocal.db`.

**What changed** (`codegraph-kernel/src/resolve.rs`):

- **`local_receiver_type_patterns` gained the `"rust"` arm** — the two patterns from `name-matcher.ts:2056-2063`, verbatim: `let r [mut] [: T] = [&][mut] Type` (declaration, capture = initializer/annotation type) and `r : [&][mut] Type` (binding *or* typed parameter — `fn f(r: &T)`, closure `|r: T|`). The whole inferrer — scope-bounded backward scan off `enclosing_scope_start_line`, raw-line matching, `utf16_len` guards, `normalize_inferred_type_name` — was already ported and already invoked for rust inside `match_method_call_free` at the exact TS slot; it returned `None` only because the pattern table fell through to `_ => vec![]`.
- **Member-tail gate relaxed** — rust `calls` refs containing `.` now flow through the native pipeline when they are `self.`-rooted (unchanged) or carry no `::`/`()` (new). `a::b.c`/`x::y().z` (`::`+`.`), `x().y`, and non-call `x.y`/`self.x` stay punted — TS verdicts by delegation. The gate is strictly additive over §5.25's: every admission it made before it still makes.

**Byte-identity lesson re-verified** (the §5.25 class): `let w = Widget::new()` (spaced `=`) is an inference *miss* in TS too — the pattern's `=` must immediately follow the optional `:T` group — so it resolves via the strat arms at 0.7, and the ported pipeline reproduces that exactly. Measured corpus yield the probe predicted (~4,776 `x.y` calls edges TS-side: 827 @0.9 rmot + 3,949 strat @0.65/0.7/0.8) all lands native now; the 24 `x::y().z` @0.85 scopedChain edges remain punted by the `::` exclusion, byte-identical by delegation.

**Gates**: 6/6 parity (new pins: `v.new`/`p.new`/`ctx.run` → rmot @0.9 instance-method; `w.again`/`w.new`/`z.again`/`w.inner.again` → strat unique-method @0.7 including the spaced-`=` miss and the cross-function scope bound; `z.nomethod` → prefilter-terminal `unresolved`; `Widget::new().again`/`make().run` → member-tail punt, also exercised through real extraction in the byte-compare leg), 223/223 resolution, 12/12 target-kind, 4,996/4,996 suite, clippy `-D warnings` (lib target), ast-grep clean.

**Remaining Rust surface**: `Self::` associated items (unassessed, small), trait dispatch through `getSupertypes` (permanent — §5.21), non-call `x.y`/`self.x` and `::`/`()` chain shapes (attributed punts), bindings emission (enhancement, not migration).

### 5.27 Rust enhancement — `Self::item` associated-path binding (2026-09-19)

First post-R5 **enhancement** (item 1 of the §remainder list): resolves refs the TS resolver previously failed, so the gate is **dual-engine agreement on the new verdicts**, not old-graph byte-identity.

**What changed** — `Self::item` now binds `Self` to the caller method's qualified-name owner (the same derivation `matchRustSelfCall` uses; `impl Tr for T` methods carry `T::`, so trait impls bind to the impl type, not the trait). The leaf then resolves by `owner::leaf` qualified name over the prefixed member kinds (`method`, `enum_member`, `constant`), at @0.9 `qualified-name`, with the same multi-owner file-locality disambiguation as `self.` calls. Implemented in both engines: `matchRustSelfPath` (name-matcher.ts) + `match_rust_self_path` (resolve.rs), armed in `matchMethodCall`/`match_method_call`/`match_method_call_free` ahead of the `!matched` bail, advisory — a miss falls through to the existing strategies so every verdict TS made before is still reachable.

**v1 scope (declines, identical in both engines)**: `Self::AssocType::member` (needs associated-type binding), `Self::f().chain` (return-type frontier), non-2-segment paths after turbofish strip, free-function callers (no owner prefix), ambiguous multi-owner types, absent members. Associated `const`/`type` nodes are extracted unprefixed, so they miss by construction — not by position guessing.

**Corpus evidence** (linux baseline, shadow mode `CODEGRAPH_RESOLVE_SHADOW=1`, sequential):

| Metric | Count |
|---|---:|
| `Self::*` refs re-run | 177 |
| kernel-handled, **0 divergent** vs TS | 84 |
| newly resolved @0.9 `qualified-name` | **81** (owner-verified: 81/81 target owner == caller owner) |
| `member-tail` passthrough (identical TS outcome) | 93 |
| terminal unresolved | 3 |

Pre-existing: of the 115 `Self::` edges resolved before via strat `instance-method` @0.65/0.7, 95 already pointed at the caller's owner (arm now lands them at @0.9); **~16 were fabrications** (e.g. `Self::new` inside `impl Process` → `ListLinksSelfPtr::new`) that re-target to the true `owner::new` on next resolution; ~4 preserve (generic `T`, owner-less `VTABLE`, genuinely absent members).

**Gates**: 6/6 kernel-resolve-parity (new seeds: `Self::new`/`Self::On`/`Self::helper` → resolved @0.9 qualified-name incl. enum_member + trait-impl owner; `Self::Assoc::new` → member-tail passthrough; free-fn + missing-member + ambiguous-owner declines), 8/8 rust-self-owner e2e (new: `Self::On(1)` tuple-variant → `enum_member`, trait-impl binding, multi-owner decline), tsc clean, cargo check clean, kernel rebuilt.

**Remaining Rust surface**: `Self::AssocType::*` (associated-type binding), `Self::f().chain` (return-type inference), non-call `x.y`/`self.x`, bindings emission (enhancement), trait dispatch via `getSupertypes` (permanent).

### 5.28 Rust enhancement — `Self::AssocType::member` via impl `type` decl (2026-09-19)

Extends §5.27's arm to three-segment paths: `Self::Assoc::leaf` binds the middle segment through the caller's **enclosing impl block** — `impl Tr for T { type Assoc = X; ... }` — then resolves `X::leaf` by qualified name. `rustAssocTypeBinding`/`rust_assoc_type_binding` reads the caller's file, brace-scans backward from the caller's first line to the enclosing block opener (only an `impl` opener qualifies — `fn`/`trait`/`mod` bodies decline), with a same-line fallback for single-line impls, then matches `type Assoc = X;` as a direct member (depth 1, or on the opener line) and normalizes like `inferLocalReceiverType` (`path::to::X<T>` → `X`). Impl-side only by construction: trait *default* methods (`trait Io { type Backend; fn f() { Self::Backend::g() } }`) carry no `=`, so they decline — correctly, since the concrete type is per-impl.

**Corpus evidence** (linux, `Self::*::*` refs, shadow mode, sequential): 32 re-run — **3 newly resolved** @0.9 `qualified-name`, all verified to the impl's bound type (`SetSystemInfo::init`→`GspSetSystemInfo::init` via `type Command = fw::commands::GspSetSystemInfo`; `SetRegistry::init`→`PackedRegistryTable::init`; `TableBox::deref`→`Table::from_raw` via `type Target = Table`); 26 `member-tail` passthroughs identical; **0 divergent**. The dominant real-world shape — `Self::Backend::*` inside `trait Io` default bodies (rust/kernel/io.rs) — is abstract by design and stays unresolved in both engines.

**Gates**: 9/9 rust-self-owner e2e (new: `Self::A::init` through `impl Tr for T { type A = Back; }` — single-line impl), 6/6 kernel-resolve-parity (seeded `Self::Assoc::init` from `Mode::step` → `Back::init` — seeded caller must be the *impl* method, not the trait decl `Step::step` which shares the name), tsc + cargo check clean, kernel rebuilt.

**Bugs found via corpus/fixture testing**: kernel port of the impl-opener check iterated ascending instead of descending (hit an unrelated earlier impl's member line and bailed); stale `dist/` masked a TS arm in shadow runs — corpus A/B must always rebuild dist first. Also noted: `is_rust_trait_impl_method`'s backward scan misses single-line `impl T for X { fn m() {} }` bodies (pre-existing shared helper, only visible for `private` cross-file targets — corpus targets were `public`).

**Remaining Rust surface**: `Self::f().chain` (return-type inference), non-call `x.y`/`self.x`, bindings emission (enhancement), trait dispatch via `getSupertypes` (permanent). `Self::Assoc` is now exhausted — every remaining decline is a genuinely abstract trait-default context.
### 5.29 Rust enhancement — `use` binding rows emitted by the kernel walker (2026-09-19)

Third **enhancement** leg (item 3 of the §remainder list; the "Rust bindings emission" line). `rustlang.rs` now emits one `import` binding row per bound local name for every `use` declaration — the first Rust rows in the `bindings` table — so the generic bindings pipeline (`importMappingsFromBindings` → `getImportMappings`, `resolveViaImport`, `matchesAnyImport`) activates for rust identically in both engines.

**What changed** (`codegraph-kernel/src/rustlang.rs`, `src/lib.rs`, `src/extraction/kernel/index.ts`, `src/resolve.rs` doc):

- **`emit_use_bindings`** walks the `use_declaration`'s `argument` field and flattens nested `use_list`/`scoped_use_list`/`use_as_clause`/`use_wildcard` trees into `(spec, local)` pairs: leaf or alias is the local name, `target_spec` the full `::` path as written, `target_name` its leaf. `{self}` binds the prefix leaf (`use a::b::{self}` → `b` → `a::b`); `{self as x}` binds the alias to the prefix path; a glob is recorded under the never-matching `*` name (resolver declines it); `pub use` carries `export_form=public` + `exported_as`.
- **Scope = the use's parent extent** — `source_file` for top-level, the `mod`/`declaration_list` for module-local, the `block` for function-local uses. Emitted on **both walks**: `visit_node` for item position, `visit_for_calls_and_structure` for body position (bindings only there — no import node/`imports` refs at fn scope, matching TS).
- **`rustlang::bindings_only`** + `"rust"` in `bindings_file` dispatch and `BINDINGS_LANGUAGES`, so the generic-extractor defer path gets rows too. `is_migrated_language`'s "bindings-free" comment updated — rust is now a binding language.

**Corpus evidence** (linux baseline; dual-db A/B — `bindingsFile` rows generated for all 496 rust files, 5,496 rows across 429 files, inserted into a corpus snapshot vs an identical no-bindings twin; both resolved with the same build):

| Metric | no bindings | with bindings |
|---|---:|---:|
| rust refs resolved | 81 | **150** |
| edges added vs baseline (of 3.83M refName-carrying edges) | — | **+69 / −0** |
| kernel-vs-TS shadow divergences (`CODEGRAPH_RESOLVE_SHADOW=1`, sequential) | — | **0 / 11,942 handled** |
| failure-reason churn among still-failed | — | none |

**New edge class (all 69, reviewed)**: a `use`-bound capitalized type name (`Pin`, `Chipset`, `Meta`, `Receiver`, `Label`, `Chars`) whose true target is absent from the graph (`enum Chipset` is `macro_rules!`-generated; `enum Meta` in vendored `syn` is unextracted; `Pin` is `core::`) now passes the `matchesAnyImport` pre-filter and lands fuzzy @0.5 on a case-insensitive same-named callable. That is the resolver's standard weakest-tier behavior for an imported name with no in-graph declaration — the same edge TS produces for `import { Foo }` with no `Foo` node — not a bindings defect. Both engines produce them identically.

**Gates**: 10/10 `bindings-rust` (new file: leaf/alias/list/nested-list/`{self}`/`{self as}`/glob/`pub use`/mod+fn scopes/`importMappingsFromBindings` decode), golden re-baselined (`torture.rs` +10 import rows), 6/6 parity, 8/8 self-owner, 662/662 extraction, 224/224 resolution, clippy `-D warnings` clean.

**Note**: this leg began as a delegated worktree whose agent died mid-wiring (`emit_use_bindings` written, never called); completed inline — wiring, `argument`-field lookup, `{self as x}` spec fix, `bindings_only` + gates + corpus A/B.

### 5.30 Rust enhancement — `Self::f().tail` call-chain resolution (2026-09-19)

Fourth **enhancement** leg — closes the last `Self::` frontier. A `Self::method().tail` leaf (37 refs on the linux corpus) now resolves through the receiver method's declared return type: `Self::make().step()` binds `make` via the existing 2-seg arm (or `Self::Assoc::m().tail` via the §5.28 impl `type` decl), reads `-> Ret` off the receiver's `signature`, normalizes it (`normalizeInferredTypeName`), maps bare `-> Self` to the RECEIVER's owner, then resolves `Ret::tail` by qualified name. The shared `owner::leaf` lookup (member-kind filter + same-name-owner file disambiguation) is factored into `resolveRustSelfMember`/`resolve_rust_self_member` in both engines.

**Corpus evidence**: 0/37 refs flip — verified correct per ref: every tail is a stdlib member on a `Result`/`Option`/raw-pointer return (`map`, `unwrap`, `into`, `cast_mut`, `len`) or a `macro_rules!`-generated receiver (`regs::zeroed`) with no in-graph owner, and the `Self::Backend::*` callers are trait default bodies where `type Backend` stays abstract (§5.28 declines by design). **0 shadow divergences** (1,556 kernel-handled refs checked, sequential shadow run on the same snapshot twin protocol).

**Gates**: 12/12 `rust-self-owner` (3 new: `-> Self` same-owner chain, `-> Chain` cross-type, `Self::Assoc::m().tail` through the impl decl, plus the decline battery — `Option<Self>` receiver, missing tail, missing receiver), 6/6 parity, 10/10 bindings-rust, golden unchanged (resolution-only arm), clippy `-D warnings` clean.

**Rust `Self::` surface is now exhausted**: 2-seg items (§5.27), 3-seg assoc-type (§5.28), call-chains (here). Everything still declined is abstract-by-design — trait defaults with unbound `type`, or stdlib/macro returns with no in-graph owner.

### 5.31 Rust enhancement — bare `Self` refs bind the enclosing type (2026-09-19)

Fifth **enhancement** leg — the largest remaining rust unresolved class. A bare `Self` ref (`Self { .. }` construction, `-> Self` position, `Self(..)` call — 2,245 refs on the linux corpus) binds to the caller qualified-name's owner TYPE node: `struct`/`enum`/`union`/`class` only — a `trait` owner stays the abstract implementor and declines. A type-level caller (`struct S { next: Option<Box<Self>> }`) binds to itself. Same file-pin disambiguation as the `Self::` member arms. Advisory placement before the bare-name pre-filter (identical position in both engines: kernel before `pre_pass`, TS before `preFilterPass`).

**Corpus evidence** (self-chain snapshot twin, sequential shadow run): **1,320 refs flip** (81 → 1,401 resolved; `Self`-sourced edge delta: `references→struct` +1,011, `instantiates→struct` +292, `references→enum` +94, `references→union` +4 — all correct by construction since the target is the caller's own qualified-name owner). Remaining ~900 `Self` refs decline: trait-default callers, macro-generated impls whose type node isn't in the graph, ambiguous same-name owners. **0 shadow divergences** (1,556 kernel-handled checked).

**Gates**: 15/15 `rust-self-owner` (new: `Self{}`/`->Self` → struct, `impl Tr for T` → T not Tr, trait-default decline), 6/6 parity, golden unchanged, clippy `-D warnings` clean.

### 5.32 Rust enhancement — `mod`-chain crate-root discovery (2026-09-19)

Sixth **enhancement** leg — the largest remaining `::`-path class: `crate::` refs (2,219 failed on the linux corpus) rooted at files whose crate root is not `lib.rs`/`main.rs`. Kernel-style modules root at arbitrary files (`drivers/android/binder/rust_binder_main.rs`), so the upward `lib.rs`/`main.rs` walk found nothing and every `crate::*` ref in those trees missed. `rust_crate_root_dir`/`rustCrateRootDir` now fall back to climbing the `mod` declaration chain: a file's parent module is the `.rs` file declaring `mod <stem>;` — `<dirname>.rs` one level up (2018 nested modules, `binder/node.rs → node/wrapper.rs`) or any indexed sibling `.rs` in the parent dir (flat roots, `mod.rs`, `lib.rs`). The file with no declarant is the crate root; its dir is the root dir. Cargo layouts are unaffected — the `lib.rs`/`main.rs` walk runs first. Per-file memo + a lazily built `dir → .rs files` index (from `known_files`/`getAllFiles`) keep the climb cheap. `mod x {` inline modules can't match (`;`-anchored decl regex, comment-stripped).

**Corpus evidence** (self-chain snapshot twin `crate-on.db`, sequential shadow run): **+356 edges, 0 removed/changed** — `imports` +331 (`use crate::{node::NodeRef, process::Process, …}` specs resolving to their real in-crate types across `binder/`, `nova-core/`, `pin-init` examples), `calls` +23 (`crate::trace::trace_transaction_fd_recv` and siblings → `binder/trace.rs`; `tyr` driver crate; `hostprogs` sample), `instantiates` +2. All sampled targets verified crate-local against the source `use`/call text. **0 shadow divergences** (1,887 kernel-handled refs checked).

**Gates**: 6/6 parity (5 new fixture refs: flat-root climb, bare-path crate fallback, two-level nested descent, climb-through-nested-declarant, orphan-file miss → passthrough), 15/15 rust-self-owner, 10/10 bindings-rust, clippy clean.

### 5.33 C/C++ enhancement — callable fn-pointer members as `field` nodes (2026-09-19)

The last open resolver item: `ops->read(...)`-style member calls (~12k refs on the linux corpus) had no target — C extraction emitted **no** `field` nodes at all. The kernel C/C++ walker now emits `field` nodes (qualified `Type::member`) for **callable members only**: direct `int (*fp)(int)` declarators, `hook_fn read` fn-ptr-typedef members, and `cb_t *cbp` function-type typedefs — classified via a file-local typedef registry (`typedef int (*P)(..)`/`using P = int (*)(..)` vs `typedef void F(..)`; never a cross-file guess). Scalars, arrays, bitfields, data pointers, and ptr-to-ptr mint nothing.

Resolution: `local_receiver_type_patterns` gains a C arm (`struct ops *o` / `ops_t *o` / `ops o` — single star, decl-shape gated) feeding `inferLocalReceiverType`; `matchBoundTypeMember`/`match_bound_type_member` accept `field` members for c/cpp refs (`resolvedBy: 'field-call'` @0.9); a singleton-only unique-field fallback (@0.7) covers receivers whose type can't be recovered — ambiguity falls through.

**Corpus evidence** (subset A/B — `include/linux` + `drivers/rtc` + `drivers/char` + `sound/core`, 3,343 files, dual index): **+5,858 field nodes, +7,000 edges** (`contains` +5,858 structural, `calls` +1,144 — 808 exact-match member names + 336 `field-call` incl. `rtc->read`/`entry.hook`/`auth.sign_message`/`augment.propagate` — `instantiates` +1), **0 edges removed**; 3 call edges retargeted (`ops->folio_split` → `dev_pagemap_ops::folio_split` is a precision WIN — the old free-function hit was a name-collision; 2 param-call guesses moved off clearly-wrong targets), 126 same-target confidence drifts (name pool absorbing the new kind — `capable`/`clk_enable`/`do_cmd` share names with fields). Callback-synthesis output identical (2,840 both arms).

**Full-corpus A/B** (71,116 files, `linux-assoc.db` baseline vs `field-on.db`): **+28,866 field nodes**; `calls` edges into fields = **13,592 `field-call`** (12,668 typed @0.9 + 924 unique-fallback @0.7) + 1 `qualified-name` — the arm accounts for the entire failed dotted c/cpp `calls` drop **21,769 → 8,177 (−62%)**. Unresolved refs overall 2,052,518 → 2,038,778 (−13,740) and total edges 6,458,002 → 6,500,379 (+42,377: `calls` +13,608 ≈ the field-call wins, `contains` +28,749 structural). `rtc.read` 77 → 0, `rtc.write` 57 → 0 in `rtc-ds1685.c`; `unknown-receiver` failures 20,731 → 7,139.

**Regression caught and fixed at corpus scale**: the new nodes entered the bare-name pool, so ~39k receiver-less calls (`bind`, `close`, `ioctl`, `assert`…, plus 63 `express`-framework `validate`/`sanitize`/`auth` middleware guesses) `exact-match`ed onto fn-pointer members. A bare name can never name a member — receiver access is always dotted/`::`-qualified in the ref encoding — so `matchByExactName`/`match_by_exact_name` now drop **c/cpp** `field` candidates for bare refs, keeping only the C++ implicit-this case (`this->` is dropped by the extractor) when the field's owner is the ref site's enclosing type; `resolveMiddleware` skips `field` too. Other languages' fields are exempt (Solidity `emit Event()` bare-calls an event field — golden-confirmed). Post-fix re-resolution of the resurrected refs: 32,663 correctly stayed failed, ~6.5k re-resolved to real non-field targets, and **every** `calls` edge into a field node now arrives via `field-call`. (`field-on.db` was finished by `codegraph sync`'s orphan sweep after the initial index OOM-killed in dispatch linking — resolution itself had completed.)

**Gates**: 9/9 `c-fnptr-field-calls` (incl. bare-call exclusion + C++ implicit-this guards), 6/6 parity, 8/8 golden (torture.c re-baselined: `conn_t::on_recv` field + resolved `c.on_recv` edge; tail-langs solidty emit edge preserved), kernel+TS mirrored, clippy/tsc clean.

### 5.34 Dynamic-dispatch synthesis wave — consolidated before/after (2026-09-19)

Corpus A/B evidence for the five synthesizer legs landed this session (#81–#86). Every leg is an additive `provenance:'heuristic'` arm — baseline arm count vs new-build arm count on the real repo; 0 edges removed/changed anywhere.

| Leg (PR) | Corpus | Synthesized edges before → after | Index ms before → after | Notes |
|---|---|---|---|---|
| Sidekiq `Jobs.enqueue(:sym)` (#81) | discourse `ae5ba2c` (~4k-file subset) | `sidekiq-dispatch` 6 → **189** (177 symbol + 12 class-arg → `Jobs::X#execute`) | n/a | The `< ::Jobs::Base` leading-`::` superclass fix also repaired a latent `perform_async` miss for any `class Foo < ::Base` worker. 0 leaked to abstract `Jobs::Base#execute`. |
| `object-registry` assign-then-call (#82) | warp-drive `2147c43` (978 TS files, full) | registry-dispatch 0 → **8** (`await cmd(args)` → each COMMANDS handler) | n/a | `const v = reg[k]` alias + post-assignment `v(...)` call sites; function-scope + TDZ guards. |
| MediatR `Publish(domainEvent)` fan-out (#84) | eShop `src/` (547 files, full) | mediatr-dispatch 9 → **16** (+7 erased-collection → all `INotificationHandler<T>.Handle`) | n/a | `via:'domainEvent:*'` marks the erased-arg fan-out; the 9 precise request edges unchanged. |
| Celery canvas signatures (#85) | paperless-ngx (full) / pretix (full) | celery-dispatch **33 → 48** (+15) / **65 → 67** (+2) | n/a | `.s()`/`.si()`/`signature()`/`subtask()` sites incl. `link=`/`on_error=`/comprehension elements; dedupe makes container `.delay()` adds free. `group`/`chord` container forms remain deferred. |
| Laravel queued-job dispatch (#86) | firefly-iii / koel | `laravel-job` **0 → 2** / **0 → 7**; `laravel-event` unchanged 142 / 9 | 4,776 → 4,796 (+0.4%) / 1,187 → 1,222 (+2.9%) | `XJob::dispatch`, `dispatch(new XJob)`, `Dispatcher::dispatch(new XJob)` → `handle`; `use Dispatchable` gate walks `extends` chain (koel `QueuedJob`). |
| Spring listener re-publish (#87) | halo `01d9b59` (991 java files) | `spring-event` **36 → 96** (+60 `delegate:*` from `SharedEventDispatcher.onApplicationEvent` → all 60 listeners) | n/a | `x.publishEvent(<expr>.<m>(…))` inside a registered listener body → all-listener fan-out (erased `ApplicationEvent` delegate); non-listener helpers stay silent. |

**Cross-leg precision**: every edge added by these arms was target-audited on the corpus (all sampled targets are the semantically-correct handler/handle/execute — verified against source text), and dedupe keys keep repeat dispatch sites from multiplying. Index-time deltas are at noise level — the arms are bounded source scans over already-extracted graphs.

### 5.35 Extraction — exported destructure-off-factory bindings (2026-10, #88)

Extraction-layer leg closing the last documented NgRx gap: `export const { selectRouteData } = getRouterSelectors()` minted **no nodes**, so `ngrx-select` had nothing to pin at the canonical `concatLatestFrom(() => store.select(selectRouteData))` site. Both extraction paths gained the arm: generic-side `tree-sitter.ts` and the kernel TS walker (`tsjs/extractors.rs`) — gated to *exported* `object_pattern` declarations with a `call_expression` RHS, simple shorthand/rename bindings only (nested/rest skipped). The existing RTK Query bare-identifier carve-out is untouched. Selector gating widened to `getSelectors`/`getRouterSelectors` sigs so the new constant nodes satisfy the `ngrx-select` candidate gate.

**Corpus evidence** (platform example-app, baseline `dist` vs new): `ngrx-select` **13 → 15** edges — `updateTitle$ → selectRouteData` lands end-to-end; **+5 constant nodes**, all verified factory destructures (`getRouterSelectors()` → `selectRouteData`; entity-adapter `getSelectors()` → `selectBookIds`/`selectBookEntities`/`selectAllBooks`/`selectTotalBooks`). `ngrx-dispatch` unchanged at 7. Blast radius: `export const {…} = f()` occurs at 3 sites across all 9 indexed corpora — the narrow gate is the right shape.

**Gates**: 5/5 `ngrx-effects-synthesizer` (new destructure test incl. decline cases), 8/8 golden, clippy/fmt/tsc clean.

### 5.36 Agent-eval A/B wave — end-to-end proof of the dispatch arc (2026-10)

`run-all.sh` headless, Sonnet/high, 2 runs/arm, pre-warmed daemons, CLI shim blocking `codegraph` in both arms. Nine canonical flow prompts across eShop / discourse / paperless-ngx / pretix / firefly-iii / koel / halo / platform example-app / warp-drive, each exercising a just-landed arm. Full table in `dynamic-dispatch-coverage-playbook.md` §6 tail; headline numbers:

| Metric | WITH | WITHOUT |
|---|---|---|
| File access (Read/Grep/Bash tokens) | **0 in 14/18 runs**; 0.5–2.7k tok in the rest | 1.7k–30.3k tok/run (2–14 Reads, 3–15 Bash) |
| Tool calls | 2–7 (median 2–3, all `codegraph_explore`-led) | 8–27 (median ~11) |
| Duration | 14–59s (median ~28s) | 26–195s (median ~47s) |
| Cost | $0.075–$0.398 | $0.126–$0.976 |
| Tokens processed | 132k–499k | 249k–789k |
| Answer correctness | 18/18 correct end-to-end | 18/18 correct end-to-end |
| Contamination | — | 1 blocked CLI attempt per run, 0 returned output |
| Allocation efficiency | 27.5–89.1% | — |

**Verdict**: the dispatch edges are load-bearing in retrieval, not just graph-complete — every with-run answered while every without-run paid a 4–6× tool-call and ~2× wall-clock/tokens tax to reconstruct the same trace. Honest residuals: four with-runs (koel, pretix ×2, warp-drive ×2) did small verification reads after exploring — file access 6–25× lower than their without twins, and never a recall miss (no Read of a file explore didn't return). Explore sufficiency ~50% per-call across the wave — agents re-explore rather than answer after one call; warp-drive's registry flow was the least sufficient shape (5 explores/run, 45–55k tok envelopes, 27–57% residual ctx). Worth a follow-up on whether wide fan-out envelopes should collapse their tails.

**Index cost on the final build** (all arms active, cold `init`, `/usr/bin/time -v`): ngrx example-app S (795 nodes) 0.97s / 315 MB peak RSS; paperless-ngx M (23k nodes) 4.84s / 1.59 GB; discourse L (167.7k nodes) 20.8s / **4.57 GB** — the largest corpus's peak RSS is the one number in this arc that bears watching; no pre-arc baseline exists for comparison (the kernel was already the parser).

### 5.41 Kernel review, pass 2 — performance batch B, the resolver (2026-09-20)

The resolver half of the performance pass, aimed at the cost Phase 5c left on the table (§5.38: discourse kernel-on 24 s / 5.7 GB against 21 s / 3.4 GB TypeScript-only). The memory was not where the design sketch put it. Removing the per-worker caches of owned node rows — six workers, up to four copies of every touched node — for one read-only table shared across the pool moved discourse's peak from 5.43 GB to 5.35 GB. The 2 GB were compiled regexes: eighteen sites formatted the reference, receiver or field name into a pattern and compiled it, the receiver-type tables alone one to four patterns per receiver, looked up per scanned line; each pool worker held 5–10k compiled `Regex` values (49k in all, ~36 KB each) and compiled them again on every run. That churn is also why kernel-on resolution was *slower* than TypeScript-only on discourse (10.9 s against 8.1 s).

What landed, in the order the measurements forced it:

- **One node table per run** (`NodeTable`): `nodes` and `files` loaded once per (database, run generation) and indexed by name (ORDER BY file_path, start_line), lowercase name, qualified name, file (ORDER BY start_line) and id; candidates are `Arc<KNode>` pointers into it. The pool hands its workers a generation token so they share the snapshot's table; the main thread's table over the live database is private and dropped at pool engage. The orders reproduce SQLite's — a `WHERE col = ?` scans its index in rowid order and SQLite's sorter is stable, so a stable sort of the rowid-ordered group is the same sequence; `CODEGRAPH_NODE_TABLE_VERIFY=1` re-runs every replaced query against the table (0 mismatches on eShop, warp-drive) and a crafted-tie unit test pins the argument. Loaded lazily on first use, by column index, so a worker reports ready before it.
- **No pattern per name.** Every per-name pattern is an `Affix`: the name is found as a literal and only the fixed head and tail are regexes, compiled once for the process — the receiver-type tables of sixteen languages, the PHP property patterns, the TypeScript field-type shapes and thirteen other sites; `js_const_binds` walks the `const {…name}` shape by hand (with the ASCII boundaries JavaScript's `\b` has — the old pattern used the crate's Unicode one). A pattern with a head (`const|let|var`, `type`, `$this->`) searches by the head, whose literal the regex engine prefilters on, and requires the word at the head's end; a headless `\bR\b TAIL` pattern searches the word and checks the tail's lead byte before running it. Only the C++ declarator regex, whose greedy type-capture prefix has no split form, still goes through a cache — now one FIFO-bounded map for the process. Equivalence tests replay every split pattern against the original formatted regex over 21 lines × 7 receivers and compare the whole match sequence.
- **Per-thread regexes.** A `Regex` shared across threads puts every thread but its owner on the search-cache pool's mutex (regex docs, "Sharing a regex across threads can result in contention" — exactly this workload, short haystacks searched one after another). Fixed patterns are per-site thread-locals (`re!`) and every shared static is used through this thread's clone (`thread_regex`); multi-line scans fetch the clones once (`Affix::any_line`).
- **Profiling that works without `perf`.** `CODEGRAPH_KERNEL_PROF=1` prints kernel time per verdict class (`hit:<resolvedBy>|<kind>|<language>`, `punt:<reason>|…`, `unresolved|…`) and per probed sub-step, per worker, at teardown; `CODEGRAPH_KERNEL_STATS=1` prints the per-resolver cache sizes. It is what found the last two regressions.

**Index level** (`codegraph index`, kernel-on, `nice -n 10`, single runs, Batch A kernel `dc294038` through `CODEGRAPH_KERNEL_PATH` against this one, same `dist`; wall / peak RSS / `resolution` phase):

| corpus | before | after |
|---|---|---|
| discourse | 23.7 s / 5.59 GB / 13.5 s | **18.8 s / 3.46 GB / 8.7 s** |
| pretix | 11.5 s / 3.91 GB / 8.8 s | 10.5 s / 2.62 GB / 7.8 s |
| halo | 6.8 s / 2.57 GB / 3.7 s | 7.7 s* / 2.03 GB / 3.0 s |
| warp-drive | 4.7 s / 1.99 GB / 3.0 s | 4.3 s / 1.42 GB / 2.6 s |
| paperless-ngx | 3.7 s / 1.62 GB / 2.0 s | 3.5 s / 1.17 GB / 1.8 s |
| firefly-iii | 7.3 s / 1.61 GB / 5.6 s | 7.1 s / 1.54 GB / 5.3 s |
| koel | 3.4 s / 1.34 GB / 1.8 s | 3.4 s / 1.20 GB / 1.8 s |

\* halo's wall is its maintenance phase (0.3–1.8 s run to run); its resolution fell 17%. The seven small corpora (eShop, Alamofire, os-lib, bloc, lazy.nvim, dplyr, lune) index within ±0.1 s and ±60 MB. discourse is now 1 s faster than TypeScript-only at 3.77 GB (19.9 s / 8.1 s resolution) and holds 10 compiled regexes where it held 49k.

**The trail** — two intermediate states regressed PHP and JavaScript corpora and the profiler traced both to the same shape: an `Affix` anchored on the receiver literal where the receiver is `e` or `t` in a minified line (a thousand hits a line, a regex call each) while the original pattern's prefilter was the head's literal. Disproved along the way, each by measurement: shared-`Arc` refcount contention (private tables changed nothing), pool-boot delay from the eager table load (lazy load changed nothing), and compile churn after the bounded cache (per-thread clones changed nothing on firefly).

**Gate**: `scripts/dump-graph.mjs` byte-identical against the Batch A kernel on all fourteen corpora; kernel 32/32 (four new tests: node-table orders, two affix equivalence suites, `js_const_binds`), clippy `-D warnings`, ast-grep rules; full suite 303 files / 5,178 tests.

### 5.40 Kernel review, pass 2 — performance batch A, the walkers (2026-09-20)

The mechanical half of the performance pass: changes that cannot move a verdict, applied across all fifteen walkers, so the byte-identical gate is the whole review. The one that matters is the column service. Every walker computed a node's UTF-16 column by re-measuring the line prefix from the line start (`col16`: `utf16_len(&src[line_start..pos])`), twice per node and once per reference — linear in the line, so quadratic on a long line, and a minified bundle is one long line. `textutil::Cols` replaces it: an all-ASCII file (the common case) reads columns straight off byte offsets, and a non-ASCII file builds the UTF-16 prefix table once, lazily, on first use; `col16` survives only as the test oracle `Cols::col` is checked against. The rest: `ids::node_id` hex-encodes through a nibble table instead of sixteen `format!` calls per id; the node/edge kind lookups are `match` tables instead of linear scans over the kind lists; the `{"valueRef":true}` arena put is hoisted out of the value-reference loops; `collect()`-then-iterate child walks became direct cursor iteration (29 sites); `&x.clone()` at reference-push sites became `&x` (21 sites); the TypeScript walker's `(name, line)` `String`-keyed scoped-row and `require()`-declarator lookups became line-keyed maps (no allocation per declaration) and its line count comes from the column service instead of a third newline scan; the doubled `markdown_refs_from_string` calls in the C/C++ and TypeScript walks are gone (a string node reached the second call on the same path with nothing emitted between, so reference order is unchanged); Java's Lombok owner test compares by `strip_prefix` instead of formatting a qualified name per node per class; the C paren-conversion regex runs only on a `(`-led callee; the fn-pointer sweep's `struct`/`union` keyword scan keeps each keyword's next hit instead of re-searching both to end of file per match; three `from_utf8_lossy` copies became `from_utf8` moves; `tree.rs` reuses `textutil::utf16_prefix`.

**Kernel-only extraction** (every file the index holds, in index order, through `tryKernelExtractRaw` on one thread — no decode, no store; best of 3; baseline kernel built from `e8ecd153`, loaded through `CODEGRAPH_KERNEL_PATH`):

| corpus | files | source | before | after | Δ |
|---|---:|---:|---:|---:|---:|
| koel | 2,051 | 3.6 M chars | 484 ms | 461 ms | −5% |
| firefly-iii | 1,954 | 10.6 M | 1,484 ms | 951 ms | −36% |
| warp-drive | 1,631 | 8.8 M | 869 ms | 874 ms | 0 |
| halo | 2,251 | 9.2 M | 865 ms | 815 ms | −6% |
| pretix | 1,417 | 20.0 M | 2,762 ms | 2,263 ms | −18% |
| discourse | 15,564 | 61.2 M | 29,919 ms | 10,742 ms | **−64%** |

discourse is the quadratic case (long-line JavaScript and Ruby); warp-drive's TypeScript was never column-bound. Peak RSS of the bench process is unchanged on every corpus (±1 MB).

**Index level** (`codegraph index`, kernel-on, `nice -n 10`, single runs; wall / max RSS / `parse-loop`): eShop 1.29 s / 554 MB / 250 ms → 1.13 / 554 / 210; koel 3.86 / 1.35 GB / 1,015 → 3.62 / 1.36 / 753; firefly-iii 7.95 / 1.64 / 1,165 → 7.60 / 1.64 / 876; pretix 11.29 / 3.86 / 1,364 → 11.35 / 3.95 / 1,244; discourse 22.75 / 5.56 GB / 6,309 → 22.60 / 5.42 / 6,332. The parse loop is bound by the main thread's store (discourse: `store=4.55 s` of the 6.3 s loop) with six parse workers absorbing the walker time, so the walker gain lands as CPU, not wall, on this host; a narrower pool sees it directly. Resolution (discourse 12.8 s, pretix 8.6 s) is the next batch.

**Gate**: `scripts/dump-graph.mjs` byte-identical against the baseline kernel on all fourteen corpora (eShop, Alamofire, os-lib, bloc, lazy.nvim, dplyr, lune, koel, firefly-iii, paperless-ngx, warp-drive, halo, pretix, discourse); kernel 28/28 (two new `Cols` tests), clippy `-D warnings`, ast-grep rules; full suite 303 files / 5,178 tests.

### 5.39 Kernel review, pass 1 — bugs (2026-09-20)

A whole-kernel audit (six read-only reviewer agents over the per-language walkers in disjoint batches; the core, the resolver and the TypeScript walker read directly) produced the first fix batch: crash classes, determinism, parity with the TypeScript reference, and one structural defect. No metric moves by design — the batch is verdict-neutral on ASCII corpora — so the row records the gate.

- **Crash classes**: seven recursive walker functions lacked `stack_guard!()` (`ccpp::callable_field_name`, `dart::extract_method`, `kotlin::try_visit_hook`, `java::extract_field`, `lua::extract_lua_table_functions` — the only walker on the table-constructor descent — `rustlang::impl_type_name`, and the TypeScript destructuring pair); `bindings_file` ran outside `run_guarded` on exactly the path a deferred file takes; a C++ `operator_name` byte slice, a negative `end_line` cast and an unchecked `segs[0]` in the fn-pointer sweep; a misaligned pad in `cfnptr_file_envs` when a chunk thread panics.
- **Determinism**: Java's Lombok `@Log*` pick iterated a `HashSet` — two log annotations on one class gave a different docstring per process.
- **Parity**: `\w` and `\b` are Unicode in the `regex` crate and ASCII in JavaScript — 6 identifier patterns respelled, 125 word boundaries switched to `(?-u:\b)` (a receiver named `über` inferred a type in the kernel and not in TypeScript; the parity fixture pins it); a TypeScript callee signature cut at 60 scalar values instead of 60 UTF-16 units.
- **Structure**: the seven hand-written AST-only binding passes (TypeScript, Python, Go, Java, Kotlin, PHP, C/C++) each under-emitted the walk's member rows and Go's invented closure parameter rows — proven on every fixture by the new `kernel-bindings-pass-parity` test. `bindings_file` now runs the guarded walk and keeps only the rows; the passes are deleted (~700 lines). ArkTS rides the TypeScript walker over its own grammar (`struct_body` is a scope block), gaining its member rows: `tail-langs.dump` +6 rows, 0 removed.

**Gate**: kernel 27/27, clippy `-D warnings`, ast-grep rules; full suite 303 files / 5,178 tests (38 new); `dump-graph` byte-identical to the pre-batch kernel `87d6917a` on twelve corpora — eShop, Alamofire, os-lib, bloc, lazy.nvim, dplyr, lune, discourse (the Phase 5c dumps) and halo, koel, paperless-ngx, warp-drive (a baseline kernel built from `87d6917a` in its own worktree, loaded through `CODEGRAPH_KERNEL_PATH`, against the same `dist`).

### 5.38 Resolution — the eight bindings-free walker languages enter the kernel (Phase 5c, 2026-09-20)

C#, Ruby, Swift, Scala, Dart, Lua, Luau and R have native walkers but emit no binding rows, and none had ever entered `is_migrated_language`: on a C# corpus the kernel settled 25 refs and punted 3,126 (`ineligible:lang`, **0.8% native** — eShop at `8c3eba03`). Like Rust before its `use` rows (§5.24), the flip is bindings-free on both engines; what was ported is the languages' own TypeScript arms — the receiver-type patterns (plus the Lua annotation pattern's call-form lookahead as a second inference guard), the Lua `receiver:method` / R `receiver$method` receiver shapes in both `matchMethodCall` arms, and `resolveLuaRequire` with its basename index in `getAllFiles()` order. Everything else those refs reach was already language-neutral or punts through the existing gates.

**Gate**: per corpus, kernel-on (`CODEGRAPH_RESOLVE_SHADOW=1`, `CODEGRAPH_RESOLVE_PROFILE=1`) vs `CODEGRAPH_KERNEL_RESOLVE=0`, `scripts/dump-graph.mjs` dumps compared with `cmp`. All eight dumps **byte-identical**, 0 shadow divergences (the shadow covers main-thread-settled refs only; the dump is the whole-graph check).

| corpus | language | nodes | native share | kernel handled / passthrough | top punts |
|---|---|---:|---:|---:|---|
| eShop | C# | 7,372 | 0.8% → **66.9%** | 5,455 / 2,696 | member-tail 2,043 · ineligible:lang 412 (razor/markdown/yaml) · rmot-supers 183 |
| Alamofire | Swift | 5,577 | **90.1%** | 11,528 / 1,264 | member-tail 789 · ineligible:lang 292 (markdown) · chain 116 |
| os-lib | Scala | 1,157 | **84.9%** | 5,645 / 1,004 | member-tail 944 · rmot-supers 48 · jvm 12 |
| bloc | Dart | 17,376 | **85.7%** | 17,894 / 2,974 | ineligible:lang 1,231 · member-tail 995 · rmot-supers 613 · jvm 116 |
| lazy.nvim | Lua | 1,495 | **74.7%** | 2,583 / 873 | member-tail 836 · rmot-supers 27 |
| lune | Luau | 4,622 | **72.5%** | 8,136 / 3,086 | member-tail 2,603 · rmot-supers 482 |
| dplyr | R | 7,498 | **98.2%** | 13,137 / 241 | ineligible:lang 179 (markdown) · member-tail 62 |
| discourse | Ruby | 167,753 | **63.7%** | 260,226 / 148,237 | member-tail 135,876 · chain 7,632 · store-bind 1,367 · rmot-supers 1,339 |

The remaining `ineligible:lang` refs on every corpus are markdown, razor and yaml — extractors without a walker, by design. `member-tail` is the unported nameMatch tail (exactName/fuzzy on receiver shapes the inferrers miss), verdict by delegation as before.

**Cost** (`/usr/bin/time`, `nice -n 10`, idle host, clean runs without shadow/profile, n=2): the seven small corpora index within ±0.3 s and ±70 MB of the TS-only arm. discourse: kernel-on **23.9 / 24.1 s, 5.70 / 5.75 GB** vs TS-only 20.9 / 21.0 s, 3.42 / 3.54 GB — and vs the pre-phase kernel-on record of 20.8 s / 4.57 GB (§5.36). The 260k newly native Ruby refs fill the kernel resolver's per-worker caches (`name_cache`, `file_nodes`, `qname_cache`, `node_by_id` — each an uncapped map of owned `KNode` rows, duplicated per pool worker), which is the whole-codebase perf pass's first target; the verdicts are identical either way.

**Gates**: parity fixture +8 files (one `lg = <Type>…; lg.<method>()` per language + a Lua `require`) with a pin block asserting the native `instance-method`@0.9 / `import`@0.9 verdicts and the kernel-vs-TS byte-identity leg over the extended fixture — 7/7; kernel 27/27 + clippy `-D warnings` + ast-grep rules clean.

### 5.37 Retrieval — dynamic `import()` edges + path-vocabulary seeds / importer spine (2026-10)

Follow-up to §5.36's least-sufficient shape (warp-drive's registry flow: 5 explores/run). Two legs, both engines mirrored.

**Graph leg.** `import('./x')` call sites emitted a bogus `calls import` ref (always failed); now an `imports` ref carrying the specifier. Path-shaped TS/JS `imports` refs (static module ref or dynamic specifier) resolve straight to the file node through the extension-/alias-aware import-path resolver, in the kernel's `resolve_via_import_member` and `resolveViaImport` — name-match used to bind the extensionless `./cmd.config` form to the file's own `import` **statement** node (`qualified-name` @0.95, wrong target). Interpolated / non-string specifiers emit nothing. Golden diffs (re-baselined, all three the intended flip): `vue-sfc` 3 vue-router lazy routes `() => import('@/views/X.vue')` failed → resolved `imports` → the components (edges 78 → 81, refs 27 → 24); `markdown-docs` `from './widget'` import-node → `file:src/widget.ts` @0.9; `torture-multilang` one honest failed `imports ./dynamic-module` replacing a failed `calls import`.

**Retrieval leg** (`codegraph_explore`). A bare query token that names a directory segment (`command` → `commands/`; exact or simple plural, never prefix — `payroll` must not match `payrolld/`) marks the directory's files as importer-spine starts; inbound file-level `imports` edges are walked ≤8 pulls deep (registry → entrypoint → bin), each hop scored 7/5/3 on a separate `injected` ledger that acts as a **floor** on the file's score, never a bonus. A directory with no organic hit gets ≤2 files seeded (@5, weakest tier); one that already has organic hits gets no seeding at all. Injected-only files are exempt from the score floor, graph gate and allocation cliff, and take a bounded sort tier (≤⌈maxFiles/2⌉ slots, below lexically-corroborated files, above graph mass) — *survival, not promotion*. Generated files are never seeded or spine-marked. Isolation probe: an outgoing `imports` edge counts as usage (a lazy registry constant is wired, not unused).

**Regression matrix** (9 wave queries, new `dist` vs baseline `dist`, same indexes): **8/9 rendered file sets byte-identical** (eShop, discourse, paperless, firefly, halo, ngrx, koel, pretix). warp-drive is a pure superset: **+`cmd.config.ts` (the `COMMANDS` registry) +`main.ts` (entrypoint)**, 0 files displaced. Two intermediate designs were rejected on this matrix: additive path-seed scoring inflated `usecase/payroll/cycle.go` 36 → 57 in the #1500 fixture and dragged the relative floor/cliff over `payslip_builder.go` (CG-12 slot hand-off broken); an uncapped injected-above-graph tier let the third spine hop (`cli.ts`) outrank the file the query named. Final shape passes `explore-allocation-1500` and `explore-cross-call-dedup` byte-for-byte against baseline.

**Cost** (probe process wall-clock incl. node start, 3 runs, peak RSS): every repo within ±30 ms / ±9 MB of baseline (discourse 480 → 510 ms, 278 → 277 MB; ngrx 140 → 140; warp 280 → 300). The directory index is one pass over the file table (~0.6 µs/file), lookups O(1) per token.

**Gates**: 3/3 `dynamic-import-edge`, 3/3 `explore-path-vocab-seeds` (fixture = warp-drive's shape: lazy `import()` registry + extensionless static import + lexical decoy), 329/329 explore/parity/golden, full suite 302 files / 5139 tests, kernel 27/27 + clippy `-D warnings` clean.
