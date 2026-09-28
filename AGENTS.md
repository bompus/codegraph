# AGENTS.md

Canonical project guidance for coding agents working in this repository (Codex/Astra, Claude Code via `@AGENTS.md`, Cursor, etc.).

**Instruction budget:** Keep this root file below 32,768 UTF-8 bytes; `npm run check:agent-docs` enforces the limit. Put conditional procedures, evidence, and worked examples in linked documents. Codex user configuration may set `project_doc_max_bytes = 65536` as a safety margin, but the larger limit does not replace the repository guard.

**Completion and branch boundary:**

- This repository's default integration branch is `fork/consolidated`.
- A worktree or other feature branch is intermediate.
- Before committing or integrating, confirm the checkout is this repository and that its integration target is `fork/consolidated` (worktrees included). Never commit or merge this repository's work into another repository or its default branch (`main` included), and never merge another repository's work here.
- For authorized repository work, finish by committing the verified change, integrating it through any required checks or PR into `fork/consolidated`, pushing, and confirming that its remote head contains the commit.
- Stop earlier only when the user requests it or integration is blocked; report the exact blocker.
- Do not run `npm publish` without explicit authorization.
- Fork GitHub release policy is defined under Releases below.

**Public repository:** never name private projects, their repositories, source paths or home directories in commits, docs, tests or fixtures; write "a private downstream project" instead.

**Branch roles:** `origin/main` is an exact mirror of `upstream/main`; never commit or merge fork work into it. `.github/workflows/sync-upstream-main.yml` maintains that mirror. Merge upstream updates into `fork/consolidated`, which is this fork's canonical/default development branch. Base focused upstream contributions on `upstream/main` so they do not include the consolidated branch's experimental history.

## Skills

Conditional procedures live in `.claude/skills/<name>/SKILL.md`. Read the matching one before the work it covers, whether or not your harness loads skills on its own:

- `codegraph-build-validation`: build and test commands, the kernel, the test suite layout, golden dumps, cross-platform and Windows validation.
- `codegraph-retrieval`: the retrieval evidence, explore budgets, dynamic-dispatch channels and the Excalidraw worked example.
- `codegraph-installer`: `src/installer/` internals, agent targets, the Cursor `--path` quirk.
- `codegraph-release`: changelog format and the upstream release flow.
- `codegraph-issue-triage`: anchoring issues and external reports to released, merged and in-progress state.
- `add-lang`, `agent-eval`: adding a language; running agent A/B evals.

## Build and test guards

These stay here because breaking them is silent. The full procedures are in `codegraph-build-validation`.

- Run build and test commands through `fnm exec --using codegraph` (Node 24) with a Rust toolchain on PATH; the system Node gives misleading failures.
- Any extraction, resolution or synthesis change re-baselines the kernel golden dumps (`UPDATE_GOLDEN=1`, `__tests__/kernel-golden-dumps.test.ts`); the `.dump` diff is the review artifact. A resolution change is also gated by `npm run eval:precision -- <corpus>`.
- Gate platform-dependent assertions with `it.runIf(process.platform === 'win32')` or `!== 'win32'`; never merge a Windows-gated test you haven't seen run.
- A new SQL asset must be copied into `dist/` by `copy-assets`, or it won't ship.
- Real corpora and `VACUUM INTO` snapshots never go on `/tmp` (shared tmpfs; it fills and OOMs the host). Use `~/cg-scratch/` or the corpus tree and delete them when done.

## Retrieval performance (do not regress)

An agent falls back to Read/Grep the instant a codegraph answer is insufficient. A flow question should resolve in 1 codegraph call on small repos and 3–5 on large ones, with Read/Grep at 0; judge every change by wall-clock time and tool-call count.

- Don't try to change the agent's tool choice through instructions or new tools. Make `codegraph_explore` answer better from the input it already gets.
- Reserve `isError` for security refusals and real malfunctions. Recoverable conditions (not indexed, symbol not found, file not in the index) return success-shaped guidance, and the tools stay exposed at an un-indexed root.
- Explore output never tells the agent to "use Read". Keep `getExploreBudget` and `getExploreOutputBudget` monotonic with indexed file count.
- Partial coverage is worse than none: bridge a dynamic-dispatch flow end to end and re-measure, or don't ship it. Synthesized edges are `provenance:'heuristic'` with `metadata.synthesizedBy` and `registeredAt`.
- Before adding or extending a router, web framework or a language's `WHEN` rules, read `docs/design/framework-coverage.md`, and update it in the same change.

Evidence, budgets, channels and the worked example: `codegraph-retrieval`.

## Project Overview

CodeGraph is a local-first code intelligence library + CLI + MCP server. It parses any supported codebase with tree-sitter, stores symbols/edges/files in SQLite (FTS5), and exposes a knowledge graph to AI agents (Claude Code, Cursor, Codex CLI, opencode) over MCP. Per-project data lives in `.codegraph/`. Extraction is deterministic — derived from AST, not LLM-summarized.

Distributed as `@colbymchenry/codegraph` on npm; same binary serves as installer, indexer, and MCP server.

## Architecture

### Layered pipeline

```
files → ExtractionOrchestrator (tree-sitter) → DB (nodes/edges/files)
              ↓
       ReferenceResolver (imports, name-matching, framework patterns)
              ↓
       GraphQueryManager / GraphTraverser (callers, callees, impact)
              ↓
       ContextBuilder (markdown/JSON for AI consumption)
```

The public API surface is `src/index.ts`, which re-exports types and the `CodeGraph` class from `src/codegraph.ts`. That class wires all the layers; extraction, resolution and the watcher load on first use, so read-only paths (query workers, the MCP engine) import `src/codegraph.ts` directly and never load them. Library users only touch `src/index.ts`; the MCP server and CLI also drive the class.

### Module layout

- `src/index.ts` is the public library API; `src/codegraph.ts` is the `CodeGraph` class that wires the system together.
- `src/db/` owns the `node:sqlite` database, schema, and prepared queries. Source development requires Node 22.5 or newer; published bundles carry their own supported runtime.
- `src/extraction/` parses supported languages; `src/resolution/` connects imports, names, frameworks, callbacks, and cross-tier flows.
- `src/graph/` owns shared graph derivations. If more than one surface renders a derivation, put it here rather than in an individual handler.
- `src/context/` and `src/search/` format and retrieve context; `src/sync/` owns watching and git-hook helpers.
- `src/mcp/` defines the MCP server and its agent-facing instructions; `src/installer/` defines host integrations.
- `src/bin/codegraph.ts` is the CLI. `src/ui/` is the terminal UI; `src/ui-server/` and `ui/` implement the browser viewer and component package.

### NodeKind / EdgeKind

Defined in `src/types.ts`. Both extractors and resolvers must use these exact strings.

- **NodeKind**: `file`, `module`, `class`, `struct`, `interface`, `trait`, `protocol`, `function`, `method`, `property`, `field`, `variable`, `constant`, `enum`, `enum_member`, `type_alias`, `namespace`, `parameter`, `import`, `export`, `route`, `component`, `union`, `endpoint`.
- **EdgeKind**: `contains`, `calls`, `imports`, `exports`, `extends`, `implements`, `references`, `type_of`, `returns`, `instantiates`, `overrides`, `decorates`.

### MCP server instructions

`src/mcp/server-instructions.ts` is sent to the main agent in the MCP `initialize` response and is the **single source of truth for detailed tool behavior**. `src/installer/instructions-template.ts` contains only the short command-and-surface pointer for subagents and non-MCP harnesses. Change detailed guidance in the server instructions; update both files only when a command or available surface changes.

### Validation methodology & worked examples

**Required** for every new language/framework: validate on small/medium/large real repos with >=3 flow prompts; deterministic `scripts/agent-eval/probe-explore.mjs` probes, then agent A/B (`scripts/agent-eval/run-all.sh` / `ab-new-vs-baseline.sh`). Pass bar: ~0 Read/Grep within the explore-call budget, faster than without-codegraph, no control-repo regression.

Full methodology (feedback metrics, CLI contamination guard, Sonnet/`--effort high` model policy, daemon pre-warm), the Excalidraw worked example, and coverage matrix live in:
- `docs/AGENTS.md` (nested; also loaded when cwd is under `docs/`)
- `docs/design/dynamic-dispatch-coverage-playbook.md`
- `docs/design/callback-edge-synthesis.md`
- `docs/benchmarks/call-sequence-analysis.md` / `docs/benchmarks/agent-eval-feedback-metrics.md`

## Releases

**Fork policy:** `bompus/codegraph` does not publish GitHub releases. Do not prepare or create fork GitHub releases, create release tags, or dispatch `.github/workflows/release.yml` on this fork. Missing release credentials or upstream publishing targets are not fork setup tasks. Authorized fork work completes on remote `fork/consolidated`; validation must not depend on publishing a release.

## House rules

- Any change to `src/installer/` (especially `targets/`) needs matching `__tests__/installer-targets.test.ts` coverage and a CHANGELOG entry: installer regressions break every new install silently. Details: `codegraph-installer`.
- Before agreeing that a reported problem is unfixed, anchor the report's date to released (`CHANGELOG.md` top), merged-but-unreleased (`origin/fork/consolidated`) or in-progress (this branch) state; a comment made before a release reacts to released state. Details: `codegraph-issue-triage`.
- Changelog or release-format work: read `codegraph-release` first.

- **Keep README.md and the docs site current in the same PR.** A change that adds, removes or renames a user-visible feature, CLI command, MCP tool, language, framework, agent target or config key updates README.md (including the "About this fork" tables) and the matching `site/src/content/docs/` page. A change that moves a number the README quotes updates that row and its source. Before each commit, check the "About this fork" section against the change. This covers resolution and extraction fixes, ported upstream features, and upstream merges: update the upstream merge point, any fork-vs-upstream row the change makes wrong, and the measured results it moves. If a number is stale but re-measuring is heavy, record the re-run in the plan and say so in the PR body. Every PR body states which README rows it checked. `__tests__/readme-sync.test.ts` gates the lists; tables and numbers are on the author.
- When changing MCP tool behavior or usage, edit `src/mcp/server-instructions.ts`, the single source of truth for detailed guidance. If a command or available surface changes, also update the short subagent/non-MCP pointer in `src/installer/instructions-template.ts`. Claude, Codex, opencode, and Gemini install that pointer; Cursor and Kiro do not. (The repo's own checked-in `.cursor/rules/codegraph.mdc` is dogfooding config — update it too if you use Cursor on this repo, but it ships nowhere.)
- CodeGraph provides **code context**, not product requirements. For new features, ask the user about UX, edge cases, and acceptance criteria — the graph won't tell you.
- **Version-tag every image referenced in `README.md`.** GitHub caches README images (`raw.githubusercontent.com` with a 5-minute TTL; third-party hosts sit behind the long-lived camo proxy), so updating an asset in place can keep showing the stale version. Give each README image URL a `?v=N` query tag and **bump `N` in the same commit whenever the asset bytes change** — e.g. `assets/waitlist.svg?v=2`. The changed URL sidesteps every cache so the new image shows immediately instead of waiting on a TTL to expire.
