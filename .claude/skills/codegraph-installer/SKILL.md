---
name: codegraph-installer
description: Use when editing anything under `src/installer/`, changing `codegraph install` or uninstall, adding or removing a supported agent target, or changing installer configuration or the instructions template.
---

### Multi-agent installer

`src/installer/` is the entry point for `codegraph install` (and the bare `codegraph`/`npx @colbymchenry/codegraph` invocation). Architecture:

- `targets/registry.ts` is the supported-agent inventory.
- `targets/types.ts` defines the `AgentTarget` interface. Adding an agent is **one new file in `targets/` + one entry in `registry.ts`**. Each target owns its config-file location, MCP-server JSON/TOML/JSONC writing, and any instructions-file integration described below.
- `targets/toml.ts` is a hand-rolled TOML serializer scoped to `[mcp_servers.codegraph]` (used by Codex). Sibling tables and `[[array_of_tables]]` are preserved verbatim. No new dependency.
- opencode reads `opencode.jsonc` by default; the installer prefers existing `.jsonc`, falls back to `.json`, and creates `.jsonc` for greenfield installs. Edits are surgical via `jsonc-parser` so user comments and formatting survive install/re-install/uninstall round-trips. The MCP entry is OpenCode 2's native `mcp.servers.codegraph` with `disabled: false` and `codemode: false` (so `codegraph_explore` stays on the native tool list); a pre-#1698 `mcp.codegraph` + `enabled` entry is migrated on re-install and removed by uninstall.
- `instructions-template.ts` holds a deliberately short marker-fenced pointer for subagents and non-MCP harnesses that cannot receive MCP `initialize` instructions. Claude, Codex, opencode, and Gemini upsert it into their instructions files; uninstall removes it. Cursor and Kiro do not write an instructions block and only strip legacy blocks. Keep detailed tool behavior in `server-instructions.ts` so the short pointer does not recreate the pre-#529 duplicated playbook.
- All installer changes need matching coverage in `__tests__/installer-targets.test.ts`, including install idempotency, sibling preservation, uninstall reverses install, byte-equal re-runs returning `unchanged`, and partial-state recovery.

### Cursor MCP working-directory quirk

Cursor launches MCP subprocesses with the wrong cwd and doesn't pass `rootUri` in `initialize`. The installer injects `--path` into Cursor's MCP args — absolute path for local installs, `${workspaceFolder}` for global installs. If you touch Cursor wiring, preserve this.

### House rule

- Any change to `src/installer/` (especially `targets/`) needs corresponding test coverage and a CHANGELOG entry — installer regressions break every new install silently.
