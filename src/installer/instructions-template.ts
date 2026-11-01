/**
 * The marker-fenced agent-instructions block the installer writes into each
 * agent's instructions file (CLAUDE.md / AGENTS.md / GEMINI.md).
 *
 * History: pre-#529 the installer wrote a full usage playbook here, which
 * duplicated the MCP `initialize` instructions for the main agent — so it
 * was removed and `mcp/server-instructions.ts` became the single source of
 * truth. A much smaller block returned for #704, because the MCP
 * instructions cannot reach two audiences that the instructions FILE does
 * reach:
 *
 *  - **Task-tool subagents** — they receive the project instructions file
 *    in their context but NOT the MCP initialize instructions. They hold
 *    the codegraph MCP tools only as deferred names and rarely think to
 *    load them: measured on a forced-delegation flow question (excalidraw,
 *    sonnet, high effort), subagents loaded + used codegraph in ~1 of 9
 *    runs without this block, and consistently with it — including runs
 *    with zero Read/grep fallback.
 *  - **Non-MCP harnesses** — agents with no MCP client at all can still
 *    run the `codegraph explore` CLI, which prints the same output as the
 *    MCP tool.
 *
 * Keep this block SHORT. The main agent reads it every turn on top of the
 * server instructions — the #529 duplication-cost argument still bounds
 * its size. Command names, the two surfaces and the one query-first
 * instruction below, nothing more.
 */

/** Markers used by the marker-based section write/removal. */
export const CODEGRAPH_SECTION_START = '<!-- CODEGRAPH_START -->';
export const CODEGRAPH_SECTION_END = '<!-- CODEGRAPH_END -->';

/**
 * The query-first instruction inside the block. Without it, agents that
 * have the block still reach for Grep and Read first; as text in the
 * instructions file it cut tool calls on Opus 5.5, Astra 6 and Sol about as
 * much as the same sentence in a system prompt. It sits ahead of the
 * "no \`.codegraph/\` directory" line so that line still scopes it to
 * indexed repositories. Keep the wording stable: the measured text is this
 * sentence.
 */
export const CODEGRAPH_QUERY_FIRST_SENTENCE =
  'Query the codegraph MCP tools first for every repo question — symbols, files, call paths, and documentation alike — before any Grep, Glob, or Read. Use Grep or Read only to verify a result or when the index returns nothing relevant.';

/**
 * The full block, markers included, exactly as written to disk.
 *
 * The wording is deliberately CONDITIONAL ("in repositories indexed by…"):
 * a global install writes this into a user-scope file (~/.claude/CLAUDE.md,
 * ~/.codex/AGENTS.md) that applies to every project the user opens —
 * including unindexed ones, where an unconditional "this repository is
 * indexed" claim would send subagents into failing codegraph calls (the
 * noise the unindexed-session policy exists to prevent).
 */
export const CODEGRAPH_INSTRUCTIONS_BLOCK = `${CODEGRAPH_SECTION_START}
## CodeGraph

In repositories indexed by CodeGraph (a \`.codegraph/\` directory exists at the repo root), reach for it before grep/find or reading files when you need to understand or locate code:

- **MCP tool** (when available): \`codegraph_explore\` answers most code questions with a compact map — the matching symbols with their files and lines, the call paths between them (including dynamic-dispatch hops grep can't follow) and a blast-radius summary. Fetch one symbol's body with \`codegraph_node\` (\`includeCode: true\`), or pass \`mode: "source"\` for the verbatim, line-numbered source. If a tool is listed but deferred, load it by name via tool search.
- **Shell** (always works): \`codegraph explore "<symbol names or question>" [--mode source]\` prints the same output.

${CODEGRAPH_QUERY_FIRST_SENTENCE}

If there is no \`.codegraph/\` directory, skip CodeGraph entirely — indexing is the user's decision.
${CODEGRAPH_SECTION_END}`;
