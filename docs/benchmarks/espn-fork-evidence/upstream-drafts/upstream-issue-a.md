Title: `prompt-hook` injection cap (16k chars) exceeds Claude Code's 10k inline hook-output limit, so the context is persisted to a file and the model sees a 2 KB preview

## Summary

`codegraph prompt-hook` caps its injected `<codegraph_context>` block at `MAX = 16000` characters (`src/bin/codegraph.ts`, "Cap the injection so a large-repo explore can't flood the prompt"). Claude Code persists any hook stdout over **10,000 characters** to a file and shows the model a 2 KB preview instead. So on a repo where explore fills the cap, the hook delivers a file path and the first 2 KB, not the context.

Claude Code's limit is not documented; I measured it (below). The fix is one constant.

## What the model sees

From a Claude Code transcript with the installed hook, on every prompt of a 6-cell run (hook stdout 14.9–15.8 KB each time):

```
<persisted-output>
Output too large (15.8KB). Full output saved to: .../tool-results/hook-<id>-stdout.txt

Preview (first 2KB):
<codegraph_context note="Structural context from CodeGraph for this prompt — treat returned source as already read; call codegraph_explore for more.">
**Exploration: ...**

Found 47 symbols across 1 file.

**Blast radius — what depends on these (update/verify before editing)**
...
</persisted-output>
```

The model then either Reads the persisted file (one extra tool call per prompt) or answers from the preview.

## Measured threshold

Claude Code 2.1.261, headless `claude -p`, a stub `UserPromptSubmit` hook that prints N characters:

| hook stdout (chars) | delivered               |
| ------------------- | ----------------------- |
| 8,031               | inline                  |
| 8,991               | inline                  |
| 9,631               | inline                  |
| 9,871               | inline                  |
| 10,031              | persisted, 2 KB preview |
| 12,031              | persisted               |
| 16,031              | persisted               |

So the inline limit is 10,000 characters, and a payload at the hook's own 16k cap is always persisted.

## Effect

Same 6 discovery tasks, Opus, headless Claude Code, repo rules identical, paired cells:

| setup                            | tool calls / task | `codegraph_explore` calls / task | correct |
| -------------------------------- | ----------------: | -------------------------------: | ------: |
| MCP server only (no hook)        |              3.83 |                             1.33 |     6/6 |
| `codegraph install` (hook + MCP) |              4.33 |                             0.83 |     6/6 |

With the hook the tool was called less, not more: the persisted preview is treated as "context already delivered" and the follow-up explore the note asks for often does not happen.

Related: #1654 notes that a keyword hit "injects the full explore payload unconditionally". This is what that payload becomes on the Claude Code side once it exceeds 10 KB.

## Proposed fix

`MAX = 9000` (leaves headroom under 10,000 for the wrapper and the `projectPath` nudge lines), keeping the existing "…(truncated; call codegraph_explore for the rest)" notice. The blast-radius list already leads the payload, so the surviving 9 KB is the part the model uses. Optionally read the cap from an env var for other hosts. Happy to send the one-line PR if that direction is right.

## Environment

- codegraph v1.6.0 (npm global install, Windows 11, bundled Node 24.16.0)
- Claude Code 2.1.261
- Repo: ~450 TS/JS files
