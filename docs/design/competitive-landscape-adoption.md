# Competing code indexes — what to adopt, what to skip

**Status:** assessment, 2026-09-11. Extends the espn-draft landscape survey of 2026-09-03 (`bompus-espn-draft/docs/agents/code-index-research.md`, which carries the measured A/B numbers this document leans on) with a September 2026 re-survey of every tool it listed plus the entrants since June. Companion to [kernel-only-extraction-plan.md](kernel-only-extraction-plan.md) and [resolution-binding-model-plan.md](resolution-binding-model-plan.md).

**Judging rule.** CodeGraph optimizes one thing: the agent stops reading files because the answer was sufficient. Every candidate feature is scored on whether it moves that, what it costs, and whether it has already been measured. Vendor "N× fewer tokens" claims are almost all measured against reading whole files or the whole corpus; only trace-mcp (60 PRs scored blind against the diff), codebase-memory-mcp (author-graded, with a no-tool arm), Augment (900 judged PR attempts), Sourcegraph (time and cost only), CodeGraph's README A/B and the espn-draft rules-on A/B have an agent-level arm at all. The espn-draft one is the only baseline that is a rules-on agent already using `Grep -n` and `Read` with offsets.

## 1. The field in one table

| Tool | Index | Returns | Tools | Staleness | Benchmark baseline |
|---|---|---|---|---|---|
| CodeGraph 1.6 | tree-sitter graph, SQLite FTS5, synthesized dynamic-dispatch edges, markdown, literals, sessions | verbatim line-numbered source, flow, blast radius | 2 by default, 9 total | watcher, hash per file | README A/B; espn-draft rules-on A/B |
| gortex 0.62 | tree-sitter 3-tier plus LSP bridge and SCIP; edge provenance tiers | ranked symbols with source under a 9k-token budget, bodies demote to signatures | 21 named, 175 via search | fsnotify, git-blob SHA, Merkle | recall vs ripgrep; SWE-bench page empty. espn-draft: 3 of 6 correct |
| GitNexus 1.6 | tree-sitter to LadybugDB, communities, execution "processes", optional PDG | JSON with path and lines, grouped by process | 17 | re-run; PostToolUse stale nudge; branch-pinned | none with a baseline |
| graphify | tree-sitter plus LLM pass for docs; EXTRACTED/INFERRED/AMBIGUOUS edges; rationale nodes from `# WHY:` comments | subgraphs, never source | 7 | git hooks | memory benchmarks; tokens vs whole corpus |
| chunkhound 5.2 | cAST chunks plus embeddings and rerank; git-diff semantic search | chunks with lines; LLM-written research | 3 | watcher | none. espn-draft: not needed |
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

Continue.dev was acquired by Cursor in June 2026 and is read-only. Cline still indexes nothing by design.

## 2. Scored candidates

Each row: does it help the agent stop reading, cost, and whether the espn-draft research already settled it.

| Candidate | Who has it | Stops reading sooner? | Cost | Measured? |
|---|---|---|---|---|
| **A. Git-diff-seeded impact** (changed lines to affected symbols, flows, tests) | GitNexus, code-review-graph, codebase-memory-mcp, gortex, trace-mcp | Yes for review and "what does this branch break" prompts: one call instead of `git diff` plus N callers calls. No effect on discovery flows | Low: diff parsing plus line-to-symbol mapping over the existing impact machinery, rendered through explore | trace-mcp's 60-PR run: correctness flat, tokens down 73%. Not in the espn-draft discovery bank |
| **B. Edge confidence on every edge** | graphify, code-review-graph, gortex, trace-mcp | Indirectly: an ambiguous label tells the agent which hop to verify instead of re-reading the neighborhood. A confident wrong caller is exactly what sends it to Grep | Low: the resolver knows how each edge was matched; synthesized edges already carry provenance | No |
| **C. Cross-language mis-wire guard** | tree-sitter-analyzer | Yes in polyglot repos: spurious callers inflate blast radius and force verification reads | Very low: callee language must match caller language unless an FFI or route edge explains it | No. Checkable on the golden corpus |
| **D. Index artifact export and import with incremental catch-up** | codebase-memory-mcp, Cursor, graphify | Adoption, not sufficiency: 9 of 11 worktrees had no index and an un-indexed root routes the agent to Grep | Moderate: copy the DB, then re-index files whose hash differs. Distinct from the borrowed-index bug the worktree guard exists for | Gap measured in espn-draft; fix not |
| **E. Git-aware staleness** (HEAD-keyed validity, branch-switch bulk resync, catch-up cap, post-commit nudge) | agentmap, tokensave, GitNexus | Yes: every avoided pending-sync banner is an avoided read burst | Low | Not measured; banner exists because the problem was observed |
| **F. Symbol-linked rationale memory** | trace-mcp, gortex, codebase-memory-mcp, graphify, Serena | Only for "why is it like this" questions | Moderate: link sessions hits to symbol ids; design in `sessions-memory-role.md` | espn-draft gates it on a recurring miss; none has fired |
| **G. Compiler-precise overlay** (SCIP import, LSP bridge, in-process type resolution) | gortex, codebase-memory-mcp, agentmap, Serena | Yes where heuristic resolution mis-wires. agentmap's 100% vs 60% is against grep, not a tree-sitter resolver, so the real gap is unmeasured | High: per-language indexer pass, `compile_commands.json` for C/C++, no Vue | espn-draft: no code until a precision gap is measured on `__tests__/evaluation` |
| **H. Long-tail language tier** (generic walker, user `languages.toml`) | gortex, codebase-memory-mcp, code-review-graph | Modestly: "file not in the index" still sends the agent to Read | Low to moderate | espn-draft: no extra language earned an index there |
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

## 3. Ranked shortlist

1. **Git-diff-seeded impact in explore (A).** Every peer with a review story ships it. CodeGraph has the impact machinery and none of the diff seeding. Add a "what does this diff break" task class to the agent-eval harness before shipping; the espn-draft bank is discovery-only and cannot see it.
2. **Edge confidence in explore output plus the cross-language guard (B and C).** Cheapest item on the list and the one aimed most directly at the stated failure trigger. TSA's 1,259 spurious cross-language edges on HuggingFace `tokenizers` is a concrete regression probe. The golden dumps will show how many edges change tier. This dovetails with the binding-model plan, which makes "how was this edge matched" a persisted fact.
3. **Index artifact export and import with catch-up (D).** The largest measured adoption hole. Keep the worktree mismatch guard: this copies then re-indexes, never borrows.
4. **Git-aware staleness (E).** HEAD-keyed validity, branch-switch bulk resync with a catch-up cap, an optional post-commit nudge.
5. **Long-tail language tier (H).** A generic walker turns "not in the index" into a signature-level answer. Interacts with the kernel-only plan's tail-language decisions: a generic kernel walker is a better answer for ArkTS, Terraform, VB.NET and COBOL than dropping them.
6. **Compiler overlay (G).** Keep the gate: no code until a resolver-versus-SCIP precision gap is measured.
7. **Symbol-linked rationale memory (F).** Only when the rationale trigger fires.
8. **Package indexing (I).** Only when a flow leaves the repo.

Not adopting on current evidence: L through R above, editor-buffer overlays (CLI hosts write to disk), and Vue template edges (sized as Grep-scale in espn-draft).

## 4. What the survey says about the rewrite question

Two data points bear on [greenfield-rust-core-sketch.md](greenfield-rust-core-sketch.md). codebase-memory-mcp is the "single C binary, 162 grammars, one SQLite file" design the sketch describes, and its own benchmark shows the metadata-only answer losing to a plain file explorer on 16 of 31 repos because it does not store line-level source. agentmap went the other way, a real compiler for one language, and refuses to add tree-sitter languages because name matching "will not ship". CodeGraph's position between them, verbatim source over a heuristic graph, is the one that measured best in the only rules-on A/B in the field. The language of the engine did not decide any of these outcomes; what the tool returns did.
