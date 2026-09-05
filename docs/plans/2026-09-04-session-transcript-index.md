# Session transcript index — plan

Status: 8/8 — done 2026-09-05; upstream PR colbymchenry/codegraph#1702 from `pr/session-index` (this commit on
upstream main, minus this plan file and the alwaysLoad `_meta` that waits on #1697). Decided 2026-09-04: separate
`sessions.db`; new `codegraph_sessions` tool in the default surface with alwaysLoad; on by default,
`"sessions": false` opts out; the repo script is deleted once the fork command works.

Branch `feat/session-index` off `experimental`, worktree `codegraph-sessions`. Ports the prototype
`scripts/transcript-search.ts` from the bompus-espn-draft repo into CodeGraph as a first-class source, shaped
for an upstream PR.

## Acceptance

- `codegraph sessions <words...> [-p path] [--limit 10] [--role user|assistant|summary] [--since <days>]
  [--any] [--session <id-prefix>] [--json] [--reindex]` searches the agent-session transcripts that belong to
  the project. First run builds the index; later runs re-read only files whose mtime or size moved.
- MCP tool `codegraph_sessions` with the same parameters returns hits as text: session id, title, role,
  timestamp, snippet with `[match]` marks, ranked by BM25.
- Source, phase 1: Claude Code transcripts under `~/.claude/projects/<slug>/**/*.jsonl` (`slug` = project
  path with `[:\\/]` → `-`, leading `-` dropped, lowercased; `memory/` skipped). Indexed prose: user prompts,
  assistant text blocks, compaction summaries. Skipped: tool_use, tool_result, thinking, `isMeta`, text under
  20 chars. `CODEGRAPH_SESSIONS_DIR` overrides the source directory (tests, unusual homes).
- Porter stemming (`tokenize = 'porter unicode61'`), every query word quoted so flags and paths cannot break
  FTS5 syntax; words ANDed, `--any` ORs.
- `codegraph.json` carries the switch (`sessions`), documented in `project-config.ts`.
- Tests: unit (`__tests__/sessions-index.test.ts`: doc extraction, query quoting, stemming, filters,
  incremental refresh, replaced file) and CLI end-to-end against the built binary
  (`__tests__/cli-sessions-command.test.ts`). `mcp-tool-annotations.test.ts` keeps passing.
- README section + CHANGELOG `[Unreleased]` entry.

## Non-goals

Cursor / other agent transcript readers (the reader is one module so a second one can land later);
embeddings; cross-project search; watcher-driven refresh (refresh-on-query is 47 ms incremental on 237
files); a UI tab; changes to `codegraph.db`'s schema or bulk-load path.

## Proof

- `bun node_modules/typescript/bin/tsc -p tsconfig.json` clean; fork vitest green for the new files and
  `mcp-tool-annotations`, `mcp-tool-allowlist`.
- From bompus-espn-draft: `codegraph sessions turn readiness dedupe` returns the C43 session; the MCP
  server lists the tool and answers the same query; `bun scripts/cg-probe.ts` unchanged.
- Ablation: the separate `sessions.db` stays only because putting the tables in `codegraph.db` touches
  `schema.sql`, `migrations.ts` and `endBulkNodeLoad`'s FTS rebuild — three merge surfaces for no query gain.

## Steps

- [x] 1 `src/sessions/claude-code.ts` — reader: `claudeSessionsDir(projectRoot)` (exact and lowercased slug),
      `walkJsonl`, `parseEntries`, `transcriptDocs`, `transcriptTitle` · sessions-index.test.ts 6 pass
- [x] 2 `src/sessions/index.ts` — `SessionsIndex` (open/refresh/search/close over `createDatabase`),
      `querySessions`, `formatSessionHits`, `NoSessionsError` · same test; deleted files are forgotten too
- [x] 3 `src/project-config.ts` — `sessions?: boolean`, `loadSessionsEnabled` · tsc clean
- [x] 4 `src/bin/codegraph.ts` — `sessions <words...>` · cli-sessions-command.test.ts 3 pass (on Node; on Bun
      the temp-dir cleanup hits EBUSY in every CLI test, cli-query-command included — Bun's node:sqlite keeps a
      prepared statement's file handle until GC)
- [x] 5 `src/mcp/tools.ts` — tool def (alwaysLoad), `DEFAULT_MCP_TOOLS` = explore + sessions, tiny-repo core
      set, `handleSessions`; `server-instructions.ts` names it · mcp-tool-allowlist, -annotations, -unindexed pass
- [x] 6 README (CLI table, MCP Tools table) + CHANGELOG `[Unreleased]`
- [x] 7 Merge into `experimental`, rebuild `codegraph-daily`, run the proof queries — de0dbca fast-forwarded,
      tsc + copy-assets clean, pushed to `fork` · full suite on Node 32 failed / 3997 passed against a 33 / 3987
      baseline, same environmental files · CLI `sessions turn readiness dedupe` 3 hits over 237 transcripts
      (6.5 s first, 141 ms after) · direct-mode MCP lists explore + sessions with alwaysLoad and answers the
      query · the running server (pid 18176, started before the merge) is left for its owner to restart
- [x] 8 bompus-espn-draft: `scripts/transcript-search.ts`, its test and the lib additions deleted (repo commit
      `refactor(transcript-search): drop the repo prototype`); `code-index-research.md` points at
      `codegraph sessions` / `codegraph_sessions` · vp check 0 errors · 1661 tests pass
