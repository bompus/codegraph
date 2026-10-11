# What to adopt from competing code indexes, and what to skip

**Status:** assessment, 2026-09-11, with Octocode added 2026-10-10 from its README and package pages only (no source read, nothing run). Extends a private downstream project's landscape survey of 2026-09-03 (its code-index research, which carries the measured A/B numbers this document leans on) with a September 2026 re-survey of every tool it listed plus the entrants since June. Companion to [kernel-only-extraction-plan.md](kernel-only-extraction-plan.md) and [resolution-binding-model-plan.md](resolution-binding-model-plan.md).

**Judging rule.** CodeGraph optimizes one thing: the agent stops reading files because the answer was sufficient. Every candidate feature is scored on whether it moves that, what it costs, and whether it has already been measured. Vendor "N× fewer tokens" claims are almost all measured against reading whole files or the whole corpus; only trace-mcp (60 PRs scored blind against the diff), codebase-memory-mcp (author-graded, with a no-tool arm), Augment (900 judged PR attempts), Sourcegraph (time and cost only), CodeGraph's README A/B and the downstream project rules-on A/B have an agent-level arm at all. The the downstream project one is the only baseline that is a rules-on agent already using `Grep -n` and `Read` with offsets.

## 1. The field in one table

| Tool | Index | Returns | Tools | Staleness | Benchmark baseline |
|---|---|---|---|---|---|
| CodeGraph 1.6 | tree-sitter graph, SQLite FTS5, synthesized dynamic-dispatch edges, markdown, literals, sessions | verbatim line-numbered source, flow, blast radius | 2 by default, 9 total | watcher, hash per file | README A/B; the downstream project rules-on A/B |
| gortex 0.62 | tree-sitter 3-tier plus LSP bridge and SCIP; edge provenance tiers | ranked symbols with source under a 9k-token budget, bodies demote to signatures | 21 named, 175 via search | fsnotify, git-blob SHA, Merkle | recall vs ripgrep; SWE-bench page empty. the downstream project: 3 of 6 correct |
| GitNexus 1.6 | tree-sitter to LadybugDB, communities, execution "processes", optional PDG | JSON with path and lines, grouped by process | 17 | re-run; PostToolUse stale nudge; branch-pinned | none with a baseline |
| graphify | tree-sitter plus LLM pass for docs; EXTRACTED/INFERRED/AMBIGUOUS edges; rationale nodes from `# WHY:` comments | subgraphs, never source | 7 | git hooks | memory benchmarks; tokens vs whole corpus |
| chunkhound 5.2 | cAST chunks plus embeddings and rerank; git-diff semantic search | chunks with lines; LLM-written research | 3 | watcher | none. downstream: not needed |
| trace-mcp 3.16 | tree-sitter 81 langs, 87 framework edge sets, 4-tier confidence, decisions DB | JSON subgraph; source via separate call | 28 to 181 by preset | PreToolUse guard blocking Read | 60 PRs blind vs diff: 67% vs 65% at 73% fewer tokens |
| Serena 1.7 | LSP live, no index; markdown memories | symbol bodies, references, symbolic edits | ~28 | live | qualitative |
| code-review-graph 2.3 | tree-sitter 23 langs, confidence floats, communities, user `languages.toml` | JSON review context, risk scores | 30 | sha256 plus dependents; GitHub Action | impact F1 0.69, recall self-flagged circular |
| codebase-memory-mcp 0.7 | 162 grammars in one C binary; in-process type resolution; team-shared zstd graph artifact; ADRs; runtime trace ingest | metadata, no line-level source | 15 | XXH3 hash incremental | 31 repos, Opus: 0.83 vs 0.92 for a file explorer; explorer wins on full source 16 of 31 |
| tree-sitter-analyzer | facade tools, TOON output, cross-language mis-wire guard | lean payloads | 8 | | benchmarks itself against CodeGraph: 7 calls each, CodeGraph cheaper |
| agentmap | ts-morph compiler, aliases, Vue script blocks, PageRank; cache valid only on clean tree at same HEAD | dependents list | 1 | HEAD-keyed | 100% blast-radius precision vs grep 60% |
| tokensave 7.11 | libSQL FTS5 plus ONNX; multi-branch with `branch_diff`; 2,000-file catch-up cap | | 86 | staleness check per call | self-run |
| Sourcegraph Code Finder | SCIP precise plus cloud search; LLM sub-search | paths and line ranges | ~8 | server | time 1.00 vs 2.19x local search, quality unquantified |
| Cursor | Merkle tree, server-side embeddings, team index reuse by simhash | chunks | built-in | Merkle diff | "12.5% more accurate", no method |
| Augment | proprietary embeddings, per-developer real-time index | snippets | 1 to 2 | seconds | 900 attempts: +71% to +80% with vs without; code leaves host |
| Octocode 0.26 | Rust, tree-sitter in 16 languages; live symbol graph built lazily from source with no index; optional index with embeddings (local fastembed by default), BM25, rerank and LLM file descriptions | signatures, graph relationships, ranked hits, optional LSP answers; no verbatim bodies | 4 plus 6 LSP tools behind `--with-lsp` | on-demand for the live graph; index re-run for search | 127 self-written queries against its own source: Hit@5 0.73 local (its `benchmark/RESULTS.md`, measured on binary 0.17.1, 2026-06-21, not on 0.26); the README's 0.99 cloud-stack figure has no matching results file; retrieval only, no agent arm |

Continue.dev was acquired by Cursor in June 2026 and is read-only. Cline still indexes nothing by design.

## 2. Scored candidates

Each row: does it help the agent stop reading, cost, and whether the downstream research already settled it.

| Candidate | Who has it | Stops reading sooner? | Cost | Measured? |
|---|---|---|---|---|
| **A. Git-diff-seeded impact** (changed lines to affected symbols, flows, tests) | GitNexus, code-review-graph, codebase-memory-mcp, gortex, trace-mcp | Yes for review and "what does this branch break" prompts: one call instead of `git diff` plus N callers calls. No effect on discovery flows | Low: diff parsing plus line-to-symbol mapping over the existing impact machinery, rendered through explore | trace-mcp's 60-PR run: correctness flat, tokens down 73%. Not in the downstream project discovery bank |
| **B. Edge confidence on every edge** | graphify, code-review-graph, gortex, trace-mcp | Indirectly: an ambiguous label tells the agent which hop to verify instead of re-reading the neighborhood. A confident wrong caller is exactly what sends it to Grep | Low: the resolver knows how each edge was matched; synthesized edges already carry provenance | No |
| **C. Cross-language mis-wire guard** | tree-sitter-analyzer | Yes in polyglot repos: spurious callers inflate blast radius and force verification reads | Very low: callee language must match caller language unless an FFI or route edge explains it | No. Checkable on the golden corpus |
| **D. Index artifact export and import with incremental catch-up** | codebase-memory-mcp, Cursor, graphify | Adoption, not sufficiency: 9 of 11 worktrees had no index and an un-indexed root routes the agent to Grep | Moderate: copy the DB, then re-index files whose hash differs. Distinct from the borrowed-index bug the worktree guard exists for | Gap measured in the downstream project; fix not |
| **E. Git-aware staleness** (HEAD-keyed validity, branch-switch bulk resync, catch-up cap, post-commit nudge) | agentmap, tokensave, GitNexus | Yes: every avoided pending-sync banner is an avoided read burst | Low | Not measured; banner exists because the problem was observed |
| **F. Symbol-linked rationale memory** | trace-mcp, gortex, codebase-memory-mcp, graphify, Serena | Only for "why is it like this" questions | Moderate: link sessions hits to symbol ids; design in `sessions-memory-role.md` | the downstream project gates it on a recurring miss; none has fired |
| **G. Compiler-precise overlay** (SCIP import, LSP bridge, in-process type resolution) | gortex, codebase-memory-mcp, agentmap, Serena | Yes where heuristic resolution mis-wires. agentmap's 100% vs 60% is against grep, not a tree-sitter resolver, so the real gap is unmeasured | High: per-language indexer pass, `compile_commands.json` for C/C++, no Vue | the downstream project: no code until a precision gap is measured on `__tests__/evaluation` |
| **H. Long-tail language tier** (generic walker, user `languages.toml`) | gortex, codebase-memory-mcp, code-review-graph | Modestly: "file not in the index" still sends the agent to Read | Low to moderate | the downstream project: no extra language earned an index there |
| **I. Dependency and package indexing** | CodeGraphContext bundles, Serena | Yes when the flow leaves the repo | Moderate | No |
| **J. Cross-repo groups and contracts** | gortex, GitNexus, trace-mcp | Only for multi-service workspaces; in-project cross-tier route edges already exist | Moderate to high | No |
| **K. Runtime trace ingestion** | codebase-memory-mcp, gortex | Targets the reactive-runtime frontier where CodeGraph has no static edges | High, niche | No |
| L. Outline or graded-fidelity output under a token budget | gortex, aider, Repomix | **No, measured negative twice**: gortex 3 of 6 correct; CodeGraph's own symbols-only arm lost 2 of 18 at no saving. codebase-memory-mcp's 0.83 vs 0.92 corroborates | | Yes. Do not adopt |
| M. Compact wire formats | gortex, tree-sitter-analyzer, CodeGraphContext | No: the 9k-character cap was null over 54 cells, and TSA's own numbers show CodeGraph's payload is already smaller | | Yes. Skip |
| N. PreToolUse hooks enriching Grep and Read | GitNexus, graphify, gortex | No: the Grep-advisory hook gave the same 4 of 6 adoption at $0.55 more per cell | | Yes. Skip |
| O. Embeddings and concept search | chunkhound, gortex, Cursor, Augment | No: concept tasks were 3 of 3 correct without one | | Yes |
| P. Graph database engines | GitNexus, CodeGraphContext | No: sub-millisecond SQLite reads; 1.6M nodes already handled | | Yes |
| Q. LLM sub-search inside the tool | Sourcegraph, chunkhound | The only vendor result faster than local search on fuzzy tasks, but it breaks the deterministic no-LLM design | | Product stance; skip |
| R. Large tool catalogues with presets and lazy search | gortex, roam, trace-mcp, tokensave | No: the agent under-picks even a second tool | | Yes |
| **S. Structural pattern search** (AST patterns such as `.unwrap()` calls or `new X(...)`) | Octocode `structural_search` | Unknown. Symbol and call-path answers cannot find a syntactic pattern, so those tasks fall back to Grep. Whether agents ask such questions is not known | Moderate: the repository uses ast-grep only as a development lint (`check:kernel-rules`), so a shipped pattern engine, the tool, its output shape and per-language scope are all new work. Octocode's engine is unread | No. The agent-eval bank has no pattern-style task |
| **T. Live graph without an index** | Octocode `graphrag` | Targets the adoption hole in D: 9 of 11 worktrees had no index. A per-call parse cost is unmeasured, and indexing stays the user's decision | Moderate | No. Treat as a variant of D to compare, not a new direction |

## 3. Ranked shortlist

1. **Git-diff-seeded impact in explore (A).** Every peer with a review story ships it. CodeGraph has the impact machinery and none of the diff seeding. Add a "what does this diff break" task class to the agent-eval harness before shipping; the downstream project bank is discovery-only and cannot see it.
2. **Edge confidence in explore output plus the cross-language guard (B and C).** Cheapest item on the list and the one aimed most directly at the stated failure trigger. TSA's 1,259 spurious cross-language edges on HuggingFace `tokenizers` is a concrete regression probe. The golden dumps will show how many edges change tier. This fits the binding-model plan, which makes "how was this edge matched" a persisted fact.
3. **Index artifact export and import with catch-up (D).** The largest measured adoption hole. Keep the worktree mismatch guard: this copies then re-indexes, never borrows.
4. **Git-aware staleness (E).** HEAD-keyed validity, branch-switch bulk resync with a catch-up cap, an optional post-commit nudge.
5. **Long-tail language tier (H).** A generic walker turns "not in the index" into a signature-level answer. Interacts with the kernel-only plan's tail-language decisions: a generic kernel walker is a better answer for ArkTS, Terraform, VB.NET and COBOL than dropping them.
6. **Compiler overlay (G).** Keep the gate: no code until a resolver-versus-SCIP precision gap is measured.
7. **Symbol-linked rationale memory (F).** Only when the rationale trigger fires.
8. **Package indexing (I).** Only when a flow leaves the repo.
9. **Structural pattern search (S).** Add a pattern-style task class to the agent-eval harness first. If agents ask such questions and fall back to Grep, ship it; if not, drop it. The live graph (T) is compared against D when D is scoped.

Octocode's own benchmark (binary 0.17.1, 2026-06-21) supports two existing stances rather than adding a candidate. Weighting fusion toward BM25 over dense vectors raised Hit@5 from 0.60 to 0.73 on its corpus, in line with FTS-first retrieval. A generic local cross-encoder reranker lowered Hit@5 from 0.73 to 0.60, so reranking is not a free addition.

Not adopting on current evidence: L through R above, editor-buffer overlays (CLI hosts write to disk), and Vue template edges (sized as Grep-scale in the downstream project).

## 4. What the survey says about the rewrite question

Two data points bear on [greenfield-rust-core-sketch.md](greenfield-rust-core-sketch.md). codebase-memory-mcp is the "single C binary, 162 grammars, one SQLite file" design the sketch describes, and its own benchmark shows the metadata-only answer losing to a plain file explorer on 16 of 31 repos because it does not store line-level source. agentmap went the other way, a real compiler for one language, and refuses to add tree-sitter languages because name matching "will not ship". CodeGraph's position between them, verbatim source over a heuristic graph, is the one that measured best in the only rules-on A/B in the field. The language of the engine did not decide any of these outcomes; what the tool returns did.

## 5. Octocode trial, 2026-10-10

Octocode 0.26.6 (built from source, local embeddings, no API keys, data directory isolated) against this fork's build 1.6.2+a43da4f4, not upstream CodeGraph. Three corpora (flask, gin, vite), one run each, on a shared host whose load average was 5 to 23 from other sessions: timings are indicative, not benchmarks. No agent-outcome arm was run.

| Measure | Octocode | CodeGraph (fork) |
|---|---|---|
| Index, flask | 219 files (includes docs), 2m37s, 5.7 GB peak, 13 MB | 97 files, 1.1 s, 208 MB, 6.5 MB |
| Index, gin | 2m41s, 5.6 GB peak, 16 MB | 119 files, 1.1 s, 310 MB, 10 MB |
| Index, vite | 2,476 files, 14m28s, 6.7 GB peak, 84 MB | 1,719 files, 3.4 s, 455 MB, 44 MB |
| Concept query, flask (dispatch flow) | 11 to 12 s, 4.7 KB of 5 chunks; reached the dispatch functions | 0.13 s, 16 KB of verbatim source plus blast radius; reached them |
| Concept query, gin (path to handler) | 0 of 2 expected symbols; right file (`gin.go`) at rank 1, `tree.go` missing | 2 of 2 |
| Concept query, vite (bare import to file) | 1 of 2 expected symbols; right file (`plugins/resolve.ts`) at rank 1 | 0 of 2; the file was not returned until the query named a symbol |
| Callee names, 8 flask functions, ast oracle (36 true) | 31 reported, 28 true, recall 0.78 | 29 reported, 29 true, recall 0.81 |

The model-load cost does not explain the slow Octocode queries: a long-running MCP server was equally slow (10.5 to 11.5 s per query, repeated query included). Its index is dominated by embedding the chunks.

- **Edges.** The oracle matches callee names only. Octocode added dictionary-style names (`get`, `setdefault`, `update`, `close`) that CodeGraph omitted. In vite, Octocode linked `tryNodeResolve` to `optimizer/index.ts::resolveId`, but in the source `resolveId` is a local variable, so that edge is wrong. CodeGraph missed `depsOptimizer.registerMissingImport`. The sample is too small to separate the two tools.
- **Live graph (candidate T).** Octocode's `graphrag` needs no index. The first call builds the graph from source: 61 ms on flask and 12.3 s on vite (1.7 GB peak), then 2 to 35 ms per query. Its callees for `Flask.full_dispatch_request` matched the source. It costs more than a persisted CodeGraph index, but it is an answer where an un-indexed checkout otherwise sends the agent to Grep.
- **Structural search (candidate S).** `structural_search` matched call patterns in Rust, JavaScript and TypeScript, with an enclosing-symbol label on each hit, in under 100 ms. In Python and Go, call patterns with metavariables (`abort($A)`, `c.JSON($$$A)`) found no AST matches and fell back to plain-text lines, which the tool labels as not AST-verified; `$X.get($Y)` returned a parse error. Kind-based patterns and `symbol` lookups worked.

Reading: this does not support switching. CodeGraph indexes about 140 to 260 times faster in about 15 to 27 times less peak memory, returns verbatim source, and matched or exceeded Octocode on call edges in the small sample. Two findings change the shortlist. First, the vite concept query is a counter-example to row O: an embedding search found the right file where a name-driven search did not, so the "concept tasks were 3 of 3 correct" result should be re-run on natural-language queries in a TypeScript corpus. Second, S and T each have a working reference to compare against; both stay conditional on an agent-eval task class.

Not measured: agent outcomes, upstream CodeGraph, Octocode with a cloud embedder or reranker, repeated runs, and edge precision for Go and TypeScript beyond the one spot check.

## 6. Gap register, 2026-10-10

Where other tools do something this fork does not, or do it better by a published or measured result, with what is known and what would reopen each item. "Unmeasured on the fork" means the number comes from a build other than this fork (usually 1.5.0) or from a different tool. Sources are repowise's `docs/BENCHMARKS.md` and its compiler-oracle write-up (G4), the Octocode trial in section 5, and [strategy-2026-09.md](strategy-2026-09.md).

| # | Gap or idea | Source | What is known | Status and next measurement |
|---|---|---|---|---|
| 1 | Call-edge precision and recall against a compiler, TypeScript first | repowise G4 | On 1.5.0, zod 0.73 precision / 0.37 recall and hono 0.81 precision, against repowise's 0.99 / 0.70 and 0.98 / 0.81. [precision-replay-2026-09.md](../benchmarks/precision-replay-2026-09.md) shows the fork keeps fewer TypeScript edges and names recall as the larger problem | Unmeasured on the fork. Run the G4 oracle cells against the fork build |
| 2 | Natural-language file retrieval | repowise ContextBench; section 5 | On Python and Go questions repowise's local search found 0.74 of the needed files (0.88 with a hosted model) against 0.61 for CodeGraph 1.5.0. In the section 5 trial Octocode returned vite's `resolve.ts` at rank 1 for a natural-language query where the fork did not return it. One query, one repository | Unmeasured on the fork; runs against row O of section 2 ("concept tasks were 3 of 3 correct") |
| 3 | Go precision on a large repository | repowise G4 | syft: CodeGraph 1.5.0 0.87 precision against 0.94 to 0.97 for the best arms; gitleaks and cobra within about one to three points. On interface-dispatch edges CodeGraph found 35 of 3,303 against repowise's 12 | Unmeasured on the fork. Part of the same oracle run as item 1 |
| 4 | Edge origin and confidence on every edge | repowise, gortex | Strategy step 2. No shipped entry for a persisted origin or confidence shown in explore in `CHANGELOG.md` | Verify what has landed before scheduling |
| 5 | Memory floor | repowise | repowise builds its graph in about 75 MB. The fork's index run peaked at 208 MB (flask), 310 MB (gin) and 455 MB (vite) in section 5 | Strategy step 3. Set a target before optimizing |
| 6 | Structural pattern search; index-free live graph | Octocode | Section 5 and candidates S and T | Needs an agent-eval task class first |
| 7 | Test selection precision | repowise: 97.5% on 47 targets | The fork has `codegraph affected` and named-test output in explore; neither has been measured for precision | Unmeasured |
| 8 | Commit-history search, branch-delta indexes, one server for several repositories | Octocode | Commit search has no equivalent here. Sibling-worktree index seeding covers part of branch deltas. Several repositories per server is candidate J | Not scored; no measured miss |
| 9 | Dead code, health score, decision records, generated wiki | repowise | Deferred in the strategy doc; its health score was measured against defect data, the others are capabilities without a measurement | Reopen on a measured miss in our own use |

Items 1 to 3 share one experiment and items 2 and 7 each need a corpus with known answers, so those are the first measurements to run. Results go in the survey doc with the date and the build.
