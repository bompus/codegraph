---
name: codegraph-issue-triage
description: Use when assessing an issue, PR comment, or external report against CodeGraph's released, merged, and in-progress state.
---

- **When the user references issues, PR comments, or external reports, anchor them to a date and version before drawing conclusions.** Check the comment's `createdAt` against:
  - The **last released version** — `grep -m1 '^## \[' CHANGELOG.md` shows the top-of-file version (older releases follow). A comment dated before the latest `## [X.Y.Z] - YYYY-MM-DD` is reacting to *released* state — work that's only on `fork/consolidated` or on an unmerged branch doesn't apply.
  - The **last default-branch commit** — `git log --first-parent origin/fork/consolidated -1 --format='%ai %h %s'`. A comment after the last release but before a fix on `fork/consolidated` may already be addressed there but unreleased.
  - The **current branch's tip** — your own unmerged work obviously can't be what the comment is reacting to.
  Always disambiguate "released," "merged-but-unreleased," and "in-progress" before agreeing that a user-reported problem is unfixed (or that a fix is incomplete). A user saying "your fix only covers X" about a recent PR is usually pointing at the *released* shortcomings — your in-flight branch may already address them but they have no way to know that.
