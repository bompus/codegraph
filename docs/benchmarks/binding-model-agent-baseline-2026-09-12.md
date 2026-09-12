# Agent baseline after the binding-model migration

Measured 2026-09-12: npm platform package 1.6.0 versus a frozen build of `86fc9dbc`, before Phase 2b receiver changes. These are whole-build comparisons, including the runtime and MCP instructions; they do not isolate the cost or benefit of binding rows.

Three pinned repositories (Flask, Gin and Vite), three flow questions each, two repetitions per question and build: 36 runs. Each build used its own CLI-created index; test files were excluded by that CLI policy. The first repetition ran old then head, the second head then old. Daemons were prewarmed. All arms used Sonnet with high effort through `run-all.sh`, with the CLI-blocking hook enabled.

All 36 runs completed successfully, with no successful CodeGraph CLI bypass and no reported missing-tool race. Both builds exposed a connected MCP server. The agent used CodeGraph in every head run, but only three old runs; tool choice is part of this unsteered measurement.

The [per-run data and exact questions](binding-model-agent-baseline-2026-09-12.json) retain costs, tokens, occupancy, sufficiency and allocation metrics. Raw local transcripts and frozen builds are under `/tmp/cg-binding-ab`; those temporary artifacts are not a durable dependency.

## Results

Each cell is the median across six runs (three questions × two repetitions); brackets show the full range. Different question costs are pooled here, so use the per-question records for detailed comparison.

| Corpus/build | Seconds | Tools | Read | Bash | CodeGraph | Cost USD | Tokens processed |
|---|---|---|---|---|---|---|---|
| flask/1.6.0 | 35.9 [23.4–194.0] | 7.5 [5.0–11.0] | 4.0 [0.0–4.0] | 4.0 [1.0–11.0] | 0.0 [0.0–0.0] | 0.117 [0.090–0.174] | 235,536 [122,973–391,938] |
| flask/86fc9dbc | 24.2 [17.7–28.1] | 2.5 [1.0–4.0] | 0.0 [0.0–3.0] | 0.0 [0.0–1.0] | 1.0 [1.0–3.0] | 0.112 [0.053–0.175] | 118,051 [62,981–150,680] |
| gin/1.6.0 | 17.8 [13.6–30.9] | 5.0 [1.0–11.0] | 1.0 [0.0–4.0] | 4.0 [0.0–11.0] | 0.0 [0.0–1.0] | 0.089 [0.051–0.164] | 133,530 [88,495–307,067] |
| gin/86fc9dbc | 13.2 [11.6–25.1] | 1.5 [1.0–3.0] | 0.0 [0.0–0.0] | 0.0 [0.0–0.0] | 1.5 [1.0–3.0] | 0.082 [0.052–0.130] | 83,530 [63,940–143,574] |
| vite/1.6.0 | 24.6 [23.0–36.7] | 3.0 [1.0–8.0] | 1.5 [0.0–4.0] | 1.5 [0.0–4.0] | 0.0 [0.0–1.0] | 0.102 [0.081–0.137] | 132,542 [92,851–296,359] |
| vite/86fc9dbc | 25.7 [19.1–28.3] | 2.5 [2.0–3.0] | 1.0 [0.0–1.0] | 0.0 [0.0–0.0] | 2.0 [1.0–2.0] | 0.135 [0.116–0.157] | 138,973 [109,208–170,030] |

Grep/Glob tool calls were zero in every run. This does **not** mean no searching occurred: old arms frequently searched or read through Bash. Tokens are summed per assistant request, deduplicated by message ID, using the existing `parseSession` accounting.

Flask and Gin used fewer tools and had lower median time. Vite had a similar median time (24.6 → 25.7 seconds), higher median cost and larger final context; the ranges overlap. These samples do not establish a general speedup or a statistically significant regression.

## Sufficiency and quality limits

All six head Gin runs used zero Read/Bash/Grep and one to three CodeGraph calls. Vite still read source in five of six head runs: four reads followed returned source and one followed missing source. That is an unresolved sufficiency problem, not a passing zero-read result.

Median final context increased: Flask 36,272 → 36,606 tokens; Gin 33,569 → 37,997; Vite 36,934 → 48,386. Per-run residual attribution and citation-based allocation are in the data file; citation-based allocation is a relative measure, not proof that every returned byte was needed.

Manual spot review of the first head answer for each question found the expected central paths: Flask dispatch/ensure_sync, exception finalization and URL error handlers; Gin handler dispatch, rendering and JSON binding; Vite server/config creation and transform/load flow. This was a path review, not exhaustive verification of every prose claim. The Gin binding answer describes interface dispatch as “resolved at compile time”; that wording overstates what the source establishes, although the concrete binding in this path is correct.

This campaign fills the missing post-parser-swap agent baseline. It is not the before/after test of Phase 2b, a with-versus-without comparison, or evidence for untested repositories.
