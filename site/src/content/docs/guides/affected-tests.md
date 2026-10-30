---
title: Affected Tests in CI
description: Run only the tests a change actually touches.
---

`codegraph affected` traces import dependencies transitively to find which test files are affected by a set of changed source files — so CI can run only the relevant tests.

```bash
codegraph affected src/utils.ts src/api.ts          # pass files as arguments
git diff --name-only | codegraph affected --stdin    # pipe from git diff
codegraph affected src/auth.ts --filter "e2e/*"      # whole-name glob; matches at any depth
```

## Options

| Option | Description | Default |
|---|---|---|
| `--stdin` | Read the file list from stdin | `false` |
| `-d, --depth <n>` | Max dependency traversal depth | `5` |
| `-f, --filter <glob>` | Whole-name glob for test files: no slash matches the file name at any depth, a pattern with `/` matches from any folder boundary, a leading `/` or `./` anchors it at the project root, a trailing `/` takes everything under that folder | auto-detect |
| `-j, --json` | Output as JSON | `false` |
| `-q, --quiet` | Output file paths only | `false` |

## CI / hook example

```bash
#!/usr/bin/env bash
AFFECTED=$(git diff --name-only HEAD | codegraph affected --stdin --quiet)
if [ -n "$AFFECTED" ]; then
  npx vitest run $AFFECTED
fi
```
