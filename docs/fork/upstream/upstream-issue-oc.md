Title: `codegraph install` writes OpenCode's v1 entry shape, so OpenCode 2 keeps `codegraph_explore` behind Code Mode and `codemode: false` cannot be set on the entry

## Summary

OpenCode 2 (the `@opencode-ai/cli@beta` line) exposes MCP tools through Code Mode by default rather than on the model's native tool list, and the documented per-server opt-out is `"codemode": false`: "Set `codemode` to `false` on a server when its tools should remain on the provider's native tool list" (https://opencode.ai/docs/mcp-servers/). For a one-tool server whose instructions say "call `codegraph_explore` instead of Read", that default matters more than Claude Code's deferral (#1696): the tool is not on the list at all.

`codegraph install` writes the v1 entry shape, `mcp.codegraph` with `type`, `command` and `enabled`. OpenCode 2 still reads that shape and connects, normalizing it in memory to its native shape (`mcp.servers.codegraph` with `disabled`), but a `codemode` key on a v1-shaped entry is dropped in that normalization. So neither the installer nor the user can opt CodeGraph out of Code Mode on the entry the installer writes. The key survives only on the native shape, which OpenCode 1.18 also reads.

## What I checked

`codegraph install` from a 1.6.0 build into a temp HOME on Windows, then each version's `mcp list`, and OpenCode 2's `debug config`, which prints the normalized config per source file:

| entry in `opencode.jsonc`                                               | OpenCode 1.18.28 `mcp list` | OpenCode 2 (beta 19086) `debug config`, normalized entry                                   |
| ----------------------------------------------------------------------- | --------------------------- | ------------------------------------------------------------------------------------------ |
| installer's: `mcp.codegraph` + `enabled: true`                          | connected                   | `mcp.servers.codegraph`, `disabled: false`; `mcp list` connected from a project-level file |
| installer's shape plus `"codemode": false`                              | not checked                 | same, `codemode` absent                                                                    |
| native: `mcp.servers.codegraph` + `disabled: false` + `codemode: false` | connected                   | `codemode: false` kept                                                                     |

One caveat on the OpenCode 2 column: its `mcp list` did not reflect the temp HOME's global file in my runs (it reported no servers while `debug config` listed that file), so the connected result there is from a project-level `opencode.jsonc`.

## Proposed fix

`codegraph install` writes the native shape for OpenCode, with the opt-out:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "codegraph": {
        "type": "local",
        "command": ["codegraph", "serve", "--mcp"],
        "disabled": false,
        "codemode": false
      }
    }
  }
}
```

Re-running install would migrate an existing `mcp.codegraph` entry to the new shape, uninstall would remove either, and `printConfig` plus the README snippet follow. I have not measured what Code Mode does to the model's use of the tool (no provider login in the test HOME); this is from the docs and the config behaviour above. Happy to send the PR if the shape change is welcome.
