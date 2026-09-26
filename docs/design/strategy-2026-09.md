# Where this fork goes next — base, upstream, and priorities

**Status:** assessment, 2026-09-25. Written to answer three questions: is the Rust rewrite worth it, how does CodeGraph compare with the tools we would otherwise use, and should this fork keep feeding upstream or become its own project. Companions: [competitive-landscape-adoption.md](competitive-landscape-adoption.md) (the 2026-09-11 survey this updates), [greenfield-rust-core-sketch.md](greenfield-rust-core-sketch.md) (the rewrite-from-scratch option and its reopen conditions), and [metrics-ledger.md](metrics-ledger.md) (every number below that we measured ourselves).

**Summary.** Keep CodeGraph as the base. It is still the best fit for how we use a code index, and nothing else on the market returns what our measurements say agents need. Stop treating upstream as the delivery path for the engine: run the engine as an independent line, send upstream only small single-concern fixes, and stop merging `upstream/main` wholesale. Spend the next stretch of work on edge correctness, not speed — an outside benchmark puts our call-edge precision at 58.6% against 85.7% for the best competitor, and that is the gap most likely to send an agent back to Grep.

## 1. What we need from a code index

Our use comes first; "best all-around" is the second test. From our private downstream project's code-index research and the fork's own history:

- **Agent-driven work on a shared host.** Many sessions across Claude Code, Codex, Cursor, OpenCode, AGY and Devin, most of them in their own git worktrees, on one WSL machine that has been OOM-killed by aggregate memory pressure.
- **One call that answers.** The measured win is an agent that stops reading files because the answer came back complete: verbatim, line-numbered source plus the flow between the named symbols. In the rules-on A/B (Opus, headless, 6 discovery + 3 concept tasks) CodeGraph cut 2 to 2.5 calls per task and kept 6 of 6 correct. Two shapes that return less than source measured worse: a symbols-only answer lost 2 of 18 cells, and gortex's budgeted outline answered 3 of 6.
- **The index has to be right.** A wrong caller or a wrong edge is what sends the agent back to Grep; a missing one mostly costs a follow-up call.
- **Local, deterministic, no LLM in the index.** Answers must be reproducible, and nothing leaves the machine.
- **Fresh in every checkout.** Worktrees had no index in 9 of 11 cases in the downstream audit; an un-indexed root routes the agent to Grep.
- **Also used:** string literals and storage keys (the `literals` table), Markdown sections, and transcript search across hosts (`codegraph_sessions`).

What we do not need, on current evidence: embeddings, LLM-written documentation, large tool catalogues, compact wire formats, PreToolUse hooks. Each was measured null or negative, or has no use here (see the survey's rows L–R).

## 2. The field on 2026-09-25

Versions and stars from the GitHub API on 2026-09-25; claims link to their sources in §7.

| Tool | Version | License, language | How the graph is built | Edge trust | Tools | Where it beats us |
|---|---|---|---|---|---|---|
| **CodeGraph (upstream)** | 1.6.0, 2026-08-26 | MIT; TypeScript + Rust kernel (WASM fallback kept) | tree-sitter, heuristic resolution, synthesized dispatch edges | provenance: extracted vs heuristic | 2 default, 9 total | — |
| **repowise** | 0.53.0, 2026-09-24 | AGPL-3.0 (commercial for enterprise); Python | tree-sitter call sites, then a resolver that types receivers from declarations; five layers: graph, git history, LLM wiki, decisions, code health | every call edge carries one of 29 named origins with a fixed confidence (0.95 same-file … 0.50 "global unique" guess); declines when it cannot prove the receiver | 10 task-shaped | precision, git risk and history, "why" answers, file retrieval, memory |
| **Serena** | 1.7.0, 2026-08-09 | GPL-3.0 (SolidLSP MIT); Python | live language servers (40+ languages), no persisted graph | compiler-grade, one hop at a time | 29 | exact references, safe symbol edits and rename |
| **code-review-graph** | 2.3.9, 2026-09-18 | MIT; Python | tree-sitter, framework resolvers, optional embeddings | EXTRACTED / INFERRED / AMBIGUOUS | 30 + 5 prompts | diff-to-impact with risk score, GitHub Action that gates PRs |
| **graphify** | 0.9.68, 2026-09-25 | Apache-2.0; Python | tree-sitter for code, LLM pass for docs and media, Leiden communities | EXTRACTED (1.0) / INFERRED / AMBIGUOUS | 7 | code + docs + rationale in one graph; PR triage |
| **gortex** | 0.64.5, 2026-09-23 | Apache-2.0; Go single binary | tree-sitter in three tiers (257 languages) plus a 17-server LSP/SCIP bridge | six tiers from `lsp_resolved` to `speculative`; an LSP pass confirms ambiguous edges | ~21 facade, 175+ total | compiler-confirmed edges, cross-repo graph, PR verdicts |

Also relevant: codebase-memory-mcp (MIT, C, one binary, 1.1 GB median build memory in repowise's bench) sits in the same space; codeseek and aoci-code are new since June and not yet assessed.

Two things the table shows. First, **every graph-based competitor now labels how sure it is of an edge** (Serena, having no graph, relies on the language server instead); CodeGraph records only extracted-versus-synthesized. Second, **four of five ship diff- or PR-seeded impact**; CodeGraph has the impact machinery and no diff entry point.

## 3. The one outside measurement of CodeGraph

repowise's [BENCHMARKS.md](https://github.com/repowise-dev/repowise/blob/main/docs/BENCHMARKS.md) measures CodeGraph **1.5.0** (2026-07-21) head to head, Aug 2–9 2026. It is a competitor's page, but an unusually careful one: it publishes its losses, pre-registers runs, seals a held-out half, grades Go and TypeScript against the compilers' own call graphs, and publishes every graded row. Its hand-graded table is graded by repowise on both sides, which the compiler oracle partly checks (the two methods agree within a point on Go).

| Question | CodeGraph 1.5.0 | repowise | Our reading |
|---|---|---|---|
| Call-edge precision, hand-graded, 9 languages × 30 rows | **58.6%** (164/280) | 85.7% (240/280) | our largest gap. By language: TypeScript **7/30**, Kotlin 13/30, Python 19/30, C# 20/30 separate; Go, Java, Swift, C++, Rust are ties |
| Precision / recall against `tsc` and Go RTA | zod 0.729 / 0.373, hono 0.805 / 0.684; Go 0.86–0.97 | zod 0.992 / 0.703 | confirms the TypeScript result with a judge nobody controls |
| Right files found (ContextBench, 42 sealed, Python/Go) | 0.610 | 0.742 local search; 0.876 with a hosted LLM | behind; our score was identical on both halves |
| Agent loop, django, Codex, 43 questions | −24.4% output tokens, 4.0 calls | −31.6%, 3.8 calls | a close second; both p < 0.0001 |
| Agent loop, Claude Code, 15 questions | used 13/15, −11.7% (n.s.) | 15/15, −15.9% | adoption is unstable for every tool on Claude Code (reruns: 2/14 for us) |
| Answer quality | no measurable change | no measurable change | no tool moved quality either way |
| Graph build, 35 repositories | 3.65 s median, **757 MB** peak, fastest on 16 | 2.77 s, **75 MB**, fastest on 14 | speed level, memory 10× theirs |
| Full index, django | **16.4 s** | 367 s (1,058 s with prose) | our clearest win |

The TypeScript result matters most to us: TypeScript is our downstream project's language and the language of most of the README corpora.

The precision gap has a likely shape. repowise types a receiver from its declaration where it can, and stores every weaker binding under a named origin with a lower confidence (its cross-file "global unique" match is recorded at 0.50 as a guess); CodeGraph's exact-name and fuzzy strategies bind a call to a same-named definition that passes the gates, with nothing marking it as a guess. The fork already found one instance while writing this: the kernel bound a Python call to a same-named Nix binding, which the TypeScript resolver rejects (fix pending). The 1.5.0 numbers predate the fork's binding model (#27–#38) and the member-access kernel work, so some of the 116 wrong rows may already be fixed — that has to be measured, not assumed (§6, step 1).

## 4. Is the Rust rewrite worth it?

**Yes, as a finished piece; no, as the place to keep spending.**

What it bought, all measured in the ledger (§5.38–5.43):

- One parser. The kernel is the only extractor; the WASM runtime, the second grammar supply chain and the Node 25 workarounds are gone, along with about 27k lines of TypeScript.
- Resolution at parity or better with the TypeScript engine it mirrors: discourse indexes in about 20 s at about 3.7 GB, 1 s faster than TypeScript-only, after the September batches turned an earlier regression (24 s / 5.7 GB) around.
- A faster index than anyone else measured: 16.4 s on django against 45–1,058 s for the others.

What it did not buy:

- **Answer quality or precision.** The kernel resolver is a bug-for-bug mirror of the TypeScript one, so it reproduces the 58.6%. The survey's conclusion still holds: "The language of the engine did not decide any of these outcomes; what the tool returns did."
- **Memory.** Our index run peaks near 400 MB even on a repository of 1,200–1,500 nodes (os-lib, lazy.nvim in the ledger's A/B), so the floor is Node plus the worker pool, not the kernel. repowise builds its graph in 75 MB. On a host that has been OOM-killed by aggregate pressure, this matters more to us than seconds do.
- **One implementation.** Every kernel punt still runs the TypeScript resolver, so there are two resolvers to keep in step, and a parity bug the dump gate cannot see (a language no corpus contains) ships silently.

The greenfield sketch's reopen conditions are close to met: both incremental plans have landed and resolution runs in the kernel with golden-dump parity. Its third condition — "a memory ceiling the daemon cannot meet" — now has outside evidence behind it. That argues for measuring the memory floor before deciding anything larger, not for a rewrite.

## 5. Which base, and what to do about upstream

### The options

| Option | For | Against |
|---|---|---|
| **A. Stay a thin fork, deliver through upstream PRs** | shared maintenance, 72k-star distribution | upstream merges 5% of outside PRs (17 of 324 in 90 days), the largest merged outside PR was 765 lines, and the maintainer usually re-lands outside fixes himself; five of our 26 PRs landed that way, all under ~1,050 lines, while our five PRs over 700 lines have had no review in 10–20 days. The kernel work cannot go this way at all (below) |
| **B. Independent engine line on the CodeGraph base** (recommended) | keeps everything that measured best for us; our kernel, binding model and synthesizers stay ours to change; MIT allows it | we carry maintenance alone; upstream extraction fixes no longer apply mechanically |
| **C. Switch base to another tool** | repowise's precision, gortex's LSP bridge, Serena's exact references | repowise is AGPL and Python with a 20–60× slower full index and a hosted-LLM answer path; gortex's facade lost answers in our A/B; Serena has no persisted graph, no flows, and was barely called by agents in repowise's runs. None returns verbatim source plus flow, which is what measured best for us |
| **D. Greenfield core** | a clean design | the sketch's §1 inventory (schema, 36 synthesis passes, 33 framework resolvers, explore semantics, daemon model) is months of work with no measured payoff; B already gives us the crate |

### Why the Rust kernel cannot go upstream as a PR

Upstream has its own kernel: 25k lines of Rust, 39 of its 42 commits by the maintainer, started 2026-07-16 and released in 1.6.0 as "the Rust engine release". It keeps a per-file WASM fallback and still ships 29 `.wasm` grammars. Ours is 35.7k lines, rewrites theirs (+26k / −16k in `codegraph-kernel/src`), deletes the fallback and vendors 4.8M lines of grammar C. The chance of that merging as a PR is effectively nil, and splitting it would mean re-deriving it against a kernel we do not control. If the kernel-only design is worth proposing, the venue is an issue describing the design and its measurements, not a PR.

### The cost of staying close to upstream

Between 2026-09-07 and 09-17 the fork took 20 upstream merges; 16 conflicted (CHANGELOG 11 times, `name-matcher.ts` 6, `extraction/tree-sitter.ts` 5), and every conflicted resolution differed from the automatic merge by 3 to 375 lines. The fork has since deleted the 30 WASM-path files that upstream still edits, including the top conflict hotspot, so from here on every upstream extraction fix is a hand port into Rust walkers that have already diverged. At upstream's pace (about 460 commits a quarter, almost all from the maintainer) that is a standing workload, not an occasional merge. Upstream has merged nothing since 2026-09-16.

### Recommendation

Take option B:

1. **Declare the engine an independent line.** Stop merging `upstream/main` wholesale into `fork/consolidated`. Watch upstream's commit log and port the fixes that matter to us by hand, the same way a downstream distribution does. Keep the MIT notice and the attribution; no rename or npm publication is needed while the only users are our own hosts.
2. **Keep an upstream channel for small fixes only.** Of the fork's 205 non-merge first-parent commits since the merge-base, about 67 are independent TypeScript fixes or features, and 25 of those cherry-pick onto `upstream/main` without conflict. Send those as goodwill in the shape the maintainer actually merges: one concern, under ~600 lines, a fail-to-pass test, no CHANGELOG hunk. Do not wait on them; nothing in our plan depends on them landing.
3. **Close or re-cut the stalled large PRs** (#1699, #1867, #1737, #1702, #1841) so they stop costing rebases.

This also changes the downstream project's stated goal of keeping the fork thin so that retiring it stays a one-line swap. That goal assumed upstream would absorb our work; the numbers above say it will not. The downstream kernel entry should name the fork as the served engine on purpose, not as a temporary divergence.

## 6. Priorities for the engine

Ordered by what moves "the agent stops reading" and by what the outside benchmark showed. Speed is not on the list; we already lead it.

1. **Measure our precision today.** Replay repowise's published graded rows (`repowise-bench/graph/experiments/g1-edge-precision/rows`: call site, bound declaration, verdict, reason) against the current fork and report what is still wrong per language. Rerun their `tsc` oracle cells (zod, hono) if the harness runs locally. Cost: small to moderate. This turns 58.6% into a concrete backlog, or shows the binding model already closed much of it. **Done 2026-09-25:** [precision-replay-2026-09.md](../benchmarks/precision-replay-2026-09.md) — on the 120 rows for the four separating languages the fork keeps 66 edges, 46 correct (70%, against 49% for 1.5.0 on the same rows), mostly by declining; TypeScript member-call recall collapsed, and Kotlin is now the least precise language.
2. **Stop guessing, and say how sure an edge is.** Two parts, in order:
   - Tighten the strategies that bind by name alone (exact-name across files, fuzzy) where the precision replay shows them wrong, starting with TypeScript: decline when the receiver's type is unknown rather than pick a same-named method.
   - Persist a resolution origin with a confidence on every edge, like repowise's 29 origins or gortex's tiers, and show it in explore's flow so the agent verifies the uncertain hop instead of re-reading the neighbourhood. Add the cross-language mis-wire guard with it (survey item C; the Nix bug is this class).
   Gate: the precision replay improves, the golden dumps change only where intended, and the agent A/B shows no recall loss that costs answers.
3. **Measure and cut the memory floor.** Profile a small-repo index and the idle daemon: Node heap, per-worker node tables and caches, SQLite page cache. Set a target (for example, under 200 MB for a small repository) before optimizing. This is the one cost where we measured 10× worse than a competitor, and it is the one our host actually suffers from.
4. **Diff-seeded impact in explore** (survey item A). Every review-oriented competitor ships it; we have the impact machinery and no entry point. Add a "what does this diff break" task class to the agent-eval harness first.
5. **Index artifacts for worktrees** (survey item D). Copy an index into a new worktree and catch up by file hash, so a fresh worktree answers from the first call. **Done 2026-09-26:** `codegraph init` seeds from the closest compatible sibling worktree index and syncs (ledger 5.58).
6. **Finish the resolver port, then delete the TypeScript resolver.** Only after steps 1–2, so the kernel moves one design forward instead of two mirrors in step. This removes the dual-maintenance cost for good.

Explicitly deferred: git-history and code-health layers (repowise's strengths, but no measured miss in our use), an LSP/SCIP overlay (survey item G stays gated on a precision gap that heuristics cannot close — step 1 will say whether one exists), and further resolver performance work.

## 7. Sources

- repowise benchmark page: https://github.com/repowise-dev/repowise/blob/main/docs/BENCHMARKS.md ; graph layer: https://github.com/repowise-dev/repowise/blob/main/docs/layers/GRAPH.md ; roadmap: https://github.com/repowise-dev/repowise/blob/main/ROADMAP.md
- Serena: https://github.com/oraios/serena , changelog https://github.com/oraios/serena/blob/main/CHANGELOG.md
- code-review-graph: https://github.com/tirth8205/code-review-graph , https://pypi.org/project/code-review-graph/
- graphify: https://github.com/Graphify-Labs/graphify , https://github.com/Graphify-Labs/graphify/blob/v8/docs/how-it-works.md
- gortex: https://github.com/zzet/gortex , https://github.com/zzet/gortex/blob/main/docs/lsp.md , https://github.com/zzet/gortex/blob/main/BENCHMARK.md
- CodeGraph upstream releases: https://github.com/colbymchenry/codegraph/releases
- codebase-memory-mcp: https://github.com/DeusData/codebase-memory-mcp
- Upstream PR and merge statistics: `gh pr list -R colbymchenry/codegraph --state all --limit 200`, 90 days to 2026-09-25; divergence from `git diff --shortstat upstream/main...origin/fork/consolidated` at merge-base `ba3c21e5`.
- Our own measurements: [metrics-ledger.md](metrics-ledger.md) §5.38–5.43; the rules-on A/B in the downstream project's code-index research.

Limits: competitor facts are from their own documentation except where repowise measured them; the precision and retrieval figures are for CodeGraph 1.5.0, not this fork; the 67 / 25 upstream-able counts come from a path-and-subject classification and a patch-level apply check, with no build or tests run.
