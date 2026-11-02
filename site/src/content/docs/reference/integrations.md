---
title: Integrations
description: Supported agents, and manual MCP setup.
---

The interactive installer auto-detects and configures each supported agent — wiring the CodeGraph MCP server into each. For the agents that use an instructions file, it also writes a short marker-fenced CodeGraph section (`CLAUDE.md`, `AGENTS.md`, or `GEMINI.md`) so subagents and non-MCP harnesses learn the `codegraph explore` command; `codegraph uninstall` removes it.

## Supported agents

- **Claude Code**
- **Cursor**
- **Codex CLI** — the Codex IDE extension reads the same config: a global install writes `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`), and a local install writes `.codex/config.toml`, which Codex uses only in a project you've marked as trusted
- **opencode**
- **Hermes Agent**
- **Gemini CLI**
- **Antigravity IDE**
- **Kiro**
- **GitHub Copilot** — Copilot Chat in VS Code (`copilot-vscode`), the Copilot CLI (`copilot-cli`), and the Copilot plugin in JetBrains IDEs (`copilot-jetbrains`)
- **Devin** (CLI and Desktop)
- **IBM Bob** (`bob`) — Bob IDE and Bob Shell: `~/.bob/settings/mcp.json`, or `.bob/mcp.json` in a project
- **Factory Droid** (`droid`) — `~/.factory/mcp.json`, or `.factory/mcp.json`, which is meant to be committed; Droid reloads the file on its own
- **Windsurf** (`windsurf`) — global only: `~/.codeium/windsurf/mcp_config.json`, which Devin, Windsurf's new name, imports by default
- **Qwen Code** (`qwen`) — `settings.json` in `$QWEN_HOME` (default `~/.qwen`) or `.qwen/`, keeping any comments in it
- **DeepSeek Harness** (`dsh`) — global only: one entry in `$DSH_HOME/cordis.patch.yml` (default `~/.dsh`) covers every profile
- **Grok Build** (`grok`) — `$GROK_HOME/config.toml` (default `~/.grok`), or `.grok/config.toml`, which Grok uses only in a trusted folder
- **Kimi Code** (`kimi`) — `$KIMI_CODE_HOME/mcp.json` (default `~/.kimi-code`), or `.kimi-code/mcp.json`, which Kimi Code uses only in a trusted folder
- **Qoder CLI** (`qoder`) — `$QODER_CONFIG_DIR/settings.json` (default `~/.qoder`), or `.qoder/settings.local.json`; in the Qoder IDE, add the server under Settings > MCP
- **Pi** (`pi`) — `mcp.json` in Pi's agent directory (default `~/.pi/agent`), or `.pi/mcp.json`, which Pi reads once you trust the project; `exposure: "direct"` keeps `codegraph_explore` on the model's tool list

Run `npx @colbymchenry/codegraph` and pick your agent(s); see [Installation](/codegraph/getting-started/installation/) for the non-interactive flags.

## Manual setup

If you'd rather wire it up yourself, install globally:

```bash
npm install -g @colbymchenry/codegraph
```

Add the MCP server to `~/.claude.json`:

```json
{
  "mcpServers": {
    "codegraph": {
      "type": "stdio",
      "command": "codegraph",
      "args": ["serve", "--mcp"]
    }
  }
}
```

Optionally auto-allow CodeGraph's tools in `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__codegraph__*"
    ]
  }
}
```

One wildcard auto-approves every CodeGraph tool. The server lists a single tool by default — `codegraph_explore` — but if you re-enable others via the `CODEGRAPH_MCP_TOOLS` environment variable, they're already permitted with no prompt.

:::tip
Cursor launches MCP subprocesses with the wrong working directory. The installer handles this for you by injecting a `--path` argument; if you wire Cursor up by hand, pass the project path explicitly.
:::
