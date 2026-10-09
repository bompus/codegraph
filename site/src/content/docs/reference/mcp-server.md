---
title: MCP Server
description: The tools CodeGraph exposes to AI agents over MCP.
---

CodeGraph runs as a [Model Context Protocol](https://modelcontextprotocol.io/) server. Agents configured by the installer launch it automatically — you don't start it by hand:

```bash
codegraph serve --mcp
```

The tools below are listed in every workspace. Where the project has no `.codegraph/` index, a call returns guidance (for example, to pass `projectPath` for a sub-project that is indexed) instead of an error, and indexing stays your decision. A project opened by `projectPath` is watched and kept in sync while the session uses it, and released after 10 minutes without a query; set `CODEGRAPH_PROJECT_IDLE_TIMEOUT_MS` to change that (`0` keeps it open until the server exits). A session that can't reach the shared background server serves itself and keeps retrying the server, first after 5 seconds and backing off to every 5 minutes; `CODEGRAPH_DAEMON_RETRY_MS` and `CODEGRAPH_DAEMON_RETRY_MAX_MS` set those delays, and `CODEGRAPH_DAEMON_RETRY_MS=0` turns retrying off.

## Two tools by default: `codegraph_explore` and `codegraph_sessions`

By default the server exposes `codegraph_explore` for code and `codegraph_sessions` for project history.

`codegraph_explore` It's Read-equivalent: give it a natural-language question or a bag of symbol and file names, and it returns the **verbatim, line-numbered source** of the relevant symbols grouped by file — the same shape the `Read` tool gives you — plus the call paths between them (including dynamic-dispatch hops like callbacks, React re-render, and JSX children that grep can't follow) and a blast-radius summary of what depends on them. One call usually answers the whole question. When a complete scan of indexed source finishes without skipped files, the summary lists requested names it could not find, helping catch a guessed name.

`codegraph_sessions` searches this project's earlier agent sessions (Claude Code, Codex, Cursor/T3, OpenCode, AGY, Devin and Grok transcripts, plus git commit messages) for what a previous session asked, decided or tried. It answers "why is X like this" questions, which the code graph cannot. Set `"sessions": false` in `codegraph.json` to turn it off.

T3-hosted provider transcripts can appear in session search; CodeGraph does not read the T3 database. The source includes a disabled-by-default offline Codex metadata prototype that associates caller-supplied T3 titles and thread links with existing hits. It has no CLI or MCP caller, adds no searchable prose and leaves provider titles, keys and ranking unchanged.

Session history lives in `.codegraph/sessions-v2.db`, separate from the code graph. The first session search imports remembered roots from `sessions.db` once and reindexes available transcripts. The older file remains untouched for older executables, and roots an older executable records after that stay in the older file. Session mentions in code answers resume after the new store is populated. Cached passages from unavailable transcripts remain only in the older store. Retaining both files uses extra disk space. Indexes that predate remembered roots start with none. A damaged newer roots table stops initialization.

Exposing one strong code tool is deliberate. Measured agent behavior showed that one well-aimed tool steers agents to a direct answer better than a menu of narrower ones — fewer mis-picks — and agents reach for it both when answering questions and while editing code.

Explore reports requested files that did not receive pin priority within the file limit. Those files may still appear through other matches. References beyond the bounded path scan are marked unexamined. When a capped file gather leaves indexed source uncovered, the summary offers continuation ranges for another explore call. Existing file and output limits still apply.

Explicit code-file pins remain eligible when the query also names documentation. Quoted terms used to select template or style source remain meaningful even when they match the filename stem. File and output budgets still apply.

When a query names functions in pinned files, `codegraph_explore` prioritizes their local callee bodies through two call or callback hops. It selects at most sixteen local helpers per file, each at most 200 lines. File and output budgets still apply.

Quoted spans of three or more words also find current indexed code by word sequence, ignoring case and punctuation. Matches return an enclosing symbol or source around the matching lines, including script strings and unquoted template text. No re-index is needed.

The scan checks its 300 ms budget between files and stops at sixteen matches or 64 MiB read. Files over 1 MiB are skipped. Up to four spans of at most 300 characters each are considered. File and output budgets still apply. Edits since the last sync can leave indexed symbol extents behind the matching source.

When you ask about tests, `codegraph_explore` includes source from the nearest test callers within three caller hops. These links describe static callers, not measured runtime coverage. Pinned JavaScript and TypeScript test files prioritize matching `it`/`test` callbacks, including `.each` parameterized tests. File and output budgets still apply.

## Daemon lifecycle

Writer handovers, readiness updates and releases share an OS file lock. The persistent `.codegraph/writer.pid.mutation.lock` file stays in place; the OS releases its lock when the process exits.

The launcher can replace a daemon from an older release when its hello confirms coordinated writer handover. Legacy daemons stay running; new sessions serve reads without auto-sync. Stop the old MCP sessions and daemon, then reconnect with the current install to migrate. A daemon exits when its installation is deleted or its package version changes. Managed builds of the same release still require matching build identities.

`CODEGRAPH_DAEMON_INSTALL_CHECK_MS` sets the installation-check interval in milliseconds. It defaults to `30000`; `0` disables the check.

## The other tools

Seven more tools exist and stay fully functional, but are **unlisted by default** — everything they return already arrives inline on a `codegraph_explore` response (its blast-radius section, the relationship map, a symbol's body and its callee list):

| Tool | Purpose |
|---|---|
| `codegraph_node` | One symbol's source + caller/callee trail, or a whole file read with line numbers (Read-parity). Returns every overload's body for an ambiguous name. |
| `codegraph_search` | Find symbols by name across the codebase (locations only) |
| `codegraph_callers` | Find what calls a function |
| `codegraph_callees` | Find what a function calls |
| `codegraph_impact` | Analyze what code is affected by changing a symbol |
| `codegraph_files` | Get the indexed file structure (faster than filesystem scanning) |
| `codegraph_status` | Check index health and statistics |

Re-enable any of them with the `CODEGRAPH_MCP_TOOLS` environment variable — a comma-separated allowlist of short names that replaces the default:

```bash
CODEGRAPH_MCP_TOOLS=explore,sessions,node,search,callers
```

Each also has a CLI equivalent (`codegraph node` / `query` / `callers` / `callees` / `impact` / `files` / `status`) for scripts and non-MCP harnesses.

## Copybooks

A named COBOL copybook prioritizes its indexed source and lists COPY and EXEC SQL INCLUDE sites. If no source is indexed, the result says so.

## How agents should use it

CodeGraph *is* the pre-built search index. For "how does X work?", architecture, a flow ("how does X reach Y"), or where-is-X questions — and while editing code — an agent should answer with `codegraph_explore` and stop, typically with **zero file reads**, rather than re-deriving the answer with `grep` + `Read`. A direct CodeGraph answer is one to a few calls; a grep/read exploration is dozens.

The MCP server delivers this guidance to the main agent automatically, in the MCP `initialize` response. Because subagents and non-MCP harnesses never see that response, the installer also writes a short marker-fenced section into each agent's instructions file pointing at the `codegraph explore` CLI equivalent.
