# One MCP server for code graph, shared memory and agent-to-agent mail?

**Status:** assessment, 2026-09-11. Answers the question "combine a code and docs graph index, cross-agent shared memory, and agent-to-agent communication into one solution served over MCP". Companion to [competitive-landscape-adoption.md](competitive-landscape-adoption.md). Source links are the user's notes file plus a September 2026 survey; facts marked unverified were not confirmed past repository metadata.

**Recommendation: do not bundle.** Keep CodeGraph at its two default tools. Test the shared-memory and mail hypothesis as a separate two-tool sidecar server over a plain local SQLite store, with a one-day experiment through the existing A/B harness. Adopt nothing from A2A until a coding host implements it natively. The reasons are in §3 and §4.

## 1. What the referenced projects actually are

| Project | What it is | Protocol and storage | State |
|---|---|---|---|
| mcp_agent_mail (Python, Rust port) | Per-project agent identities, inboxes, threads, searchable archive, advisory TTL file reservations enforced by an optional pre-commit hook | MCP only; SQLite plus a git archive of Markdown; an A2A handshake proposal was closed | Single author. 45 tools plus 25 resources. Own tracker records SQLite corruption loops under fan-out, id renumbering that orphans mail, a fleet-wide crash loop, and a self-heal that rewrote every MCP client config in cwd. Author acknowledges agents forget to check mail and patched it with hooks. License is MIT with a rider excluding OpenAI and Anthropic and anyone acting for them, which taints reuse in an MIT npm package |
| executor.sh | MCP gateway: many servers behind one `execute` tool; the agent writes sandboxed TypeScript and discovers tools with `search()` | MCP; encrypted SQLite for credentials; local daemon or hosted | MIT, 3.7k stars, YC-backed. Open issue: agents do not realize other servers are reachable through it unless told |
| supermemory, honcho, mem0, Letta, Zep/Graphiti, Cognee, LangMem | LLM-extracted memory stores with user profiles, vector or graph retrieval, some with MCP servers | Each needs at least one of an LLM call, an embedding service, or an external database; honcho is AGPL | The category's shared critique: the model that hallucinates also writes the record, vendor benchmarks do not reproduce under independent harnesses, and memory poisoning is now a security topic |
| spotify/portal-ai-plugins | Plugin marketplace wrapping Spotify's commercial Backstage product, plus a hook that routes bulk reads to cheaper models | No MCP server, no storage, requires a Portal account | Not relevant to memory or graphs |
| Agent-Bridge | A coordinator spawns local worker CLIs as subprocesses | MCP on the coordinator side; state in a home-directory folder | 54 stars, created 2026-08-19, kills itself after idle in long threads. Too young to draw on |

## 2. Protocol and host facts that constrain the design

- **MCP 2026-07-28** removed sessions and the `initialize` handshake, replaced server-initiated elicitation and sampling with multi-round-trip tool results, moved tasks to a poll-based extension, and deprecated roots, sampling and logging with a twelve-month window. There is no push channel. A server cannot deliver mail to an idle agent. Separately, the `initialize` instructions channel that CodeGraph uses to steer agents is going away in this version, which is its own follow-up.
- **A2A 1.0** shipped in April under the Linux Foundation and joined the Agentic AI Foundation alongside MCP in August. Neither Claude Code nor Codex CLI implements it. Its production evidence is enterprise platforms, not developer tooling.
- **Claude Code** has cross-session messaging since August: same machine over a per-session socket, cross-machine only through Anthropic's servers with a claude.ai login, plain text, no delivery to a session that never ran, WSL and native Windows cannot see each other. Agent teams are experimental, one team per session, in-process teammates do not survive resume.
- **Codex** multi-agent and **Cursor** cloud agents are host-internal.
- **Tool count**: a pet-store experiment went from perfect at 10 tools to complete failure at 107; a selection-accuracy study fell from 43% to under 14% as tools grew; GitHub Copilot cut 40 tools to 13 for a measurable SWE-bench gain. CodeGraph's two-tool default and its rule that early `isError` responses teach abandonment are load-bearing.

## 3. What each concern would actually add

### 3.1 Shared memory versus what CodeGraph already has

CodeGraph already answers structure through the graph and "what was decided" through transcript search over Claude, Codex, Cursor, OpenCode and AGY sessions, per checkout, with provenance and no LLM rewrite step. Kind by kind:

- **Decisions and rationale.** Already covered by transcript search, with the property the memory products lack: nothing hallucinated is written. The real gap is curation, a "current decision" record rather than a time-ordered transcript. That is a file the graph already indexes, such as an ADR under `docs/`, not a server.
- **Task state.** Hosts own it and each keeps it session-scoped and private. A cross-host ledger is a coordination concern, not memory.
- **User preferences.** Small, cross-project, must be trusted. Instruction files already do this. Automatic extraction is exactly the poisoning risk.
- **Cross-repo facts.** The one kind a per-checkout design cannot answer. The cheapest fix is a multi-root option on `codegraph_sessions` that unions several checkouts' session databases.

Net: the measurable gap is multi-repo scope plus a curated current-truth layer. Both fit the existing local SQLite and FTS design without LLM extraction.

### 3.2 Agent-to-agent mail versus host-native mechanisms

Hosts stop at the same machine, live sessions only, no durable inbox, one team per session, plain text, no cross-vendor addressing. What mail adds is a cross-host handoff, a durable async inbox, and advisory file leases across worktrees. All three are pull-based under MCP, so the agent has to remember to poll, which is the failure the agent-mail author had to fix with hooks.

## 4. Why bundling is the wrong shape

- **Tool bloat.** Two tools plus 45 mail tools plus five to ten memory tools crosses the measured collapse threshold. Collapsing to a few verbs changes the query style explore depends on. An error from the mail side, such as "no inbox" or "lease held", teaches abandonment of the code graph too.
- **Consistency.** The graph is derived and rebuildable. Mail and memory are authoritative state. Mixing them in one store means re-index, WAL healing and `codegraph init` now put user data at risk. The agent-mail corruption and reconstruct incidents show what that looks like.
- **Privacy.** Session transcripts never leave the checkout today. Cross-machine delivery necessarily ships transcript-derived text off machine. Bundling makes "did codegraph upload my transcripts" a legitimate question. Two of the reference projects carry licenses that cannot be reused in this package.
- **Release coupling.** Extraction changes need golden re-baselines; a mail protocol change is a compatibility event across running agents. One version number, three cadences, and the installer rewrites every host config on each release.
- **Spec drift.** Building a push-shaped feature on a protocol that just removed sessions and server-initiated requests is building against the current.

## 5. What to do instead

1. **Multi-root sessions.** Add a `projectPath` list to `codegraph_sessions` so cross-repo "what did we decide" is answerable from data that already exists. Half a day.
2. **A two-tool sidecar, separate server, shared local store convention.** A plain SQLite file per machine or per project with append-only `notes` and `mail` tables, entries written explicitly by agents, no LLM extraction, optional human-readable mirror. One tool `notes_write`, one tool `inbox` with a `mode` argument for read and write. A Claude `SessionStart` or `UserPromptSubmit` hook and a Codex equivalent inject unread mail, because agent polling does not happen reliably.
3. **Measure with the existing harness.** Three flow prompts with and without the sidecar. Count Read and Grep, wall clock, and whether explore call counts regress. Pass bar is the existing one: no regression in explore's zero-read behaviour, and a measurable drop in re-explaining across sessions and hosts.
4. **Only then** decide whether a code-mode single-tool front like executor is needed to hold the tool count. Do not adopt A2A until a coding host ships it natively.

## 6. Verification notes

GitHub metadata was read through the API on 2026-09-11 and 2026-09-12. Agent-mail, executor, Agent-Bridge and portal facts come from their READMEs and issue trackers. Memory-provider facts are repository metadata plus one third-party comparison; supermemory's local-mode parity, honcho and mem0 tool counts, and Letta's MCP exposure were not verified.
