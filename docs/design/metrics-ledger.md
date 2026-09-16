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

Reading: the last excluded kind is native — 91.5% of the resolution loop now settles without a TS round-trip, verdict-identical (edges and failed refs byte-for-byte at the corpus level; the parity test pins every arm including the kind-gated import discard). Wall-clock again moves only within noise: the moved refs were cheap misses, and the floor remains persist + synthesis. Remaining passthroughs (~500k) are `no_candidates` punts, gated imports, JS store-bind files, and the non-bare function_ref shapes.

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
