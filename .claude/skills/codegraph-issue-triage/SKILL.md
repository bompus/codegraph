---
name: codegraph-issue-triage
description: Use when assessing an issue, PR comment, or external report against CodeGraph's released, merged, and in-progress state.
---

Anchor the report to its stated CodeGraph version or revision and its date before
deciding whether a fix applies. Ask for a missing tested version under the active
host's question policy; a comment date alone does not identify the binary tested.

Compare that evidence with:

- **Released state:** `rg -m1 '^## \[[0-9][^]]*\] - ' CHANGELOG.md` selects
  the first dated version heading, skipping `[Unreleased]`. Inspect the entry
  containing the fix; a report predating it does not verify that later release.
- **Merged-but-unreleased state:** `git log --first-parent origin/fork/consolidated -1 --format='%ai %h %s'`
  identifies the fetched integration tip. Check the fix's commit and ancestry;
  presence there does not prove the user's released binary contains it.
- **In-progress state:** record the task branch and revision. An unmerged fix
  applies only when the reported test actually used that build.

State which of these three states supports the conclusion. Verify the reported
behavior before calling a problem fixed or a fix incomplete.

If the active host supplies shared bug-diagnosis/reporting guidance, use it for
reproduction and submission steps. Otherwise follow this repository's contribution
instructions. Neither path requires a personal house-rules installation. This
skill owns CodeGraph's release and default-branch lookup, not the host's question API.
