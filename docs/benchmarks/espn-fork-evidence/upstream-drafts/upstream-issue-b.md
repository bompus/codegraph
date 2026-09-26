Title: Claude Code defers `codegraph_explore` behind ToolSearch, so most sessions never call it; the tool and the installer should opt out (`alwaysLoad`)

## Summary

Claude Code defers every MCP tool behind its `ToolSearch` step by default: a fresh session sees only the tool's name, not its schema, until the model searches for it. So the server instructions' "call `codegraph_explore` instead of Read" arrive with nothing loaded to act on. Measured on a default `codegraph install` setup (no project rules naming the tool), 4 of 6 sessions never called `codegraph_explore` at all and did the task with Grep/Read; with the schema in context from the first turn, 6 of 6 did.

| 6 code-discovery tasks, Opus, headless, no project rules | sessions that called `codegraph_explore` | tool calls / task | cost / task | correct |
| -------------------------------------------------------- | ---------------------------------------: | ----------------: | ----------: | ------: |
| deferred (as installed)                                  |                                      2/6 |              4.83 |       $0.46 |     6/6 |
| schema loaded, tool search off                           |                                      6/6 |              3.67 |       $0.38 |     6/6 |

Same entry both rows; the second row runs the session with tool search disabled, which puts the schema in context the same way `alwaysLoad` does. The four deferred sessions that skipped the tool ran 9, 3, 7 and 4 Grep/Read/Bash calls; the tool was in their list the whole time, by name only.

Claude Code has two opt-outs, and CodeGraph sets neither:

- `"alwaysLoad": true` on the server entry in `.mcp.json` / `~/.claude.json` (docs: https://code.claude.com/docs/en/mcp, "Exempt a server from deferral").
- `_meta: { "anthropic/alwaysLoad": true }` on the tool definition itself, which works on existing installs without touching the user's config.

## Reproduction

Claude Code 2.1.261, Opus, headless, `--strict-mcp-config` with only `{"mcpServers":{"codegraph":{"command":"codegraph","args":["serve","--mcp"]}}}` (what `codegraph install` writes today), prompt "Use codegraph_explore to find which function defines clearExtensionStorage":

| server                                       | tool calls in order               |
| -------------------------------------------- | --------------------------------- |
| 1.6.0 as installed                           | `ToolSearch`, `codegraph_explore` |
| same, `"alwaysLoad": true` on the entry      | `codegraph_explore`               |
| same config, tool carries `_meta` alwaysLoad | `codegraph_explore`               |

That prompt names the tool outright, so the model searches for it. The tasks in the summary table do not name it, which is the normal case, and there the deferred session mostly never searches. Of the two deferred sessions that did use the tool, one searched first and one called it by name unsearched and got through, so the search turn is usual, not required.

With a project rule that names the tool (our own repo's `CLAUDE.md`), 4 of 6 sessions used it with or without the key, so a rule recovers most of the adoption. A default install has no such rule.

## Proposed fix

Both, so existing installs and new ones are covered:

1. `codegraph_explore` carries `_meta: { "anthropic/alwaysLoad": true }` in its tool definition. Claude Code reads it; other hosts ignore `_meta`.
2. The Claude Code installer target writes `"alwaysLoad": true` on the server entry (Claude only, since the key is host-specific), and the README's manual `~/.claude.json` snippet shows it.
3. Same shape for GitHub Copilot CLI: its tool search is on by default and defers MCP tools once ~30 tools are connected (Claude and GPT-5.4+ models), and the per-server opt-out is `"deferTools": "never"` in `~/.copilot/mcp-config.json` ([docs](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/tool-search)). Not measured, docs only.

Checked the other installer targets' docs for the same thing: Codex, Cursor, Antigravity, Kiro and Hermes load MCP schemas eagerly by default (Kiro's and Hermes's tool search are opt-in, with no per-server pin), so nothing to set there. OpenCode 2 is a different case and gets its own issue: it puts MCP tools behind Code Mode by default, and the per-server opt-out `"codemode": false` only survives on its native v2 entry shape, which the installer does not write yet.

PR incoming.
