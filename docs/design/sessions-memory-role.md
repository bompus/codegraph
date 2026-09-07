# Repo memory files in the session index

Status: idea, not built. Written 2026-09-05 after the session index shipped (PR #1702). Build it only when the
gate in § When to build holds.

## The distinction that makes it worth doing

A session transcript is evidence: what was asked, what was tried, what came back. It is immutable and written
by the host, not by the agent's judgement. A memory file is the agent's own distilled conclusion: mutable,
unreviewed, and stale the moment the code moves under it. Both are worth searching, but they carry different
trust, and an agent reading a hit needs to know which one it is holding.

So the feature is not "index memory files". It is a fourth `role` in `sessions.db` — `memory` beside `user`,
`assistant`, `summary` — so one query returns the memory claim and the session where it was actually decided,
and `--role memory` alone shows what earlier agents believe about the project. A memory hit with no session hit
behind it is the stale-claim signal; that check stays a query pattern the agent runs, not a feature.

## Repo-level only

Only files the project checks in qualify. Global and user-level memory is per machine, unreviewed, and not
reproducible across contributors; indexing it would make two clones of the same repo answer differently.

Where the hosts keep memory, and why most of it is already excluded:

| Host        | Memory location                                                   | In scope                                                               |
| ----------- | ----------------------------------------------------------------- | ---------------------------------------------------------------------- |
| Claude Code | `~/.claude/projects/<slug>/memory/` — user profile, keyed by path | No. `walkJsonl` already skips `memory/`; that exclusion is deliberate   |
| Cursor      | Its own database                                                  | No                                                                     |
| Codex       | `AGENTS.md` is instructions, not memory; memory store unverified  | No unless a project commits it                                         |
| Any host    | A directory the team commits: `.claude/memory/`, `notes/`, `.agents/` | Yes, by opt-in glob                                                |

In practice "repo-level memory" is whatever a team commits on purpose. Projects that route agent knowledge into
rule and doc markdown (the reference repo does: it has zero memory files by policy, and `codegraph_explore`
already serves `.agents/` and `docs/agents/` section-first) get nothing new from this; the role exists for
projects that commit a memory directory instead.

## Shape

- `codegraph.json`: `"sessions": { "memory": ["<glob>", ...] }` — opt-in, empty by default. `sessions: false`
  still turns the whole source off.
- Each matched file is one or more docs in the existing `docs` FTS table, `role = 'memory'`, `file` = the
  repo-relative path, `ts` = file mtime, split on top-level headings so a hit lands on a section, not a file.
- Refresh rides the existing mtime-or-size check in `SessionsIndex.refresh`; a deleted file is forgotten the
  same way a deleted transcript is.
- Honour the code index's ignore handling. A project can mark a directory human-only (`.cursorignore`, or a
  `notes/` the agents never read); an indexer that serves it puts the wrong text in agent context.
- Not in `codegraph.db`: memory files are not symbols, and the session index stayed separate for the same
  merge-surface reason the plan's ablation recorded.

## Non-goals

Global or user-level memory. Per-vendor memory detectors: locations and formats differ per host and move
between releases, and a glob the project declares needs no maintenance from this side. Writing memory:
CodeGraph reads; what to remember stays the host's job. Embeddings, as before.

## When to build

A rationale question ("why is this like this", "what did the last agent decide") that recurs and that neither
`codegraph_explore` over the project's markdown nor `codegraph_sessions` answers, on a project that commits a
memory directory. The reference repo cannot supply the test corpus, since it has no memory files; the first
build needs a project that does. Proof at that point: the same unit test file as the transcripts
(`__tests__/sessions-index.test.ts`) with a memory glob fixture, and `--role memory` returning the section that
answers the question.
