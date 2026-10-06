# Worker brief: kernel resolution fixes

Shared brief for a worker fixing reproduced resolution or extraction bugs in its own worktree. The dispatcher's prompt adds the worktree, branch, base commit, scratch directory, the bugs (with their evidence file, fixture and control directories) and the precision corpora to use. Read `AGENTS.md` and this skill's `SKILL.md` first.

## Environment

- Every shell: `export PATH=$HOME/.cargo/bin:$PATH`; run Node tooling through `fnm exec --using codegraph …`.
- Use the dependencies prepared by the dispatcher; shared dependency paths are read-only. Never `git add node_modules`; add specific task paths only.
- Stamp the start and end of the task with `date -Is` and report both. Report elapsed time from those stamps, not from memory.
- Before a kernel build, full suite, golden re-baseline or `eval:precision`, follow the active host's admission, shared-lock, memory-cap and worker limits. Obtain its admitted window before launching; a cap alone does not reserve resources. Do not introduce a second lock or fixed cap in this brief. When the host has no policy, agree on resource bounds before the run.
- Release the local reservation only after the command and its children exit. Remote review and CI waits hold no local reservation.
- A targeted vitest run rebuilds the kernel first when its sources changed (`scripts/ensure-kernel.mjs`), which makes it a heavy step. Build under the lock before running targeted tests after a kernel edit.
- Kernel build: `npm run -s build:kernel`. Full suite: `npx vitest run --minWorkers=1 --maxWorkers="$CG_WORKERS"` (set `CG_WORKERS` to the host-approved worker count for this run), then read raw stderr for `Native stack trace` or `Worker exited unexpectedly`; a green summary alone is not proof.
- Put scratch in the task-owned disk location required by the active host, never shared tmpfs. Precision corpora may use `~/cg-scratch/` under the repository guard. Never kill processes by pattern.
- Precision: `EVAL_REPOS=~/cg-scratch/eval-repos npm run -s eval:precision -- <corpus>`. The run writes a tracked report under `__tests__/evaluation/results/`; delete exactly the files your run created, by name, before committing.

## Method per bug

1. Reproduce at your base. If it no longer reproduces, record that and skip it.
2. Fix at the root cause in `codegraph-kernel/src/`. Keep the fix narrow and in the style of the surrounding code. Fix a TS fallback with the same bug only if it is cheap.
3. Add a regression test under `__tests__/` from the fixture and its control: the smallest source that shows the wrong or missing edge. Assert the correct edge and that the control is unchanged.
4. Prove the test fails on base. Save a copy of the base kernel build before editing, then run the test with `CODEGRAPH_KERNEL_PATH=<saved base .node>` (must fail) and against your build (must pass).
5. If a fix is too risky or needs a design decision, stop on that bug and report it. Don't ship a heuristic that trades one wrong edge for others.

## Before opening the PR

- Goldens: run `__tests__/kernel-golden-dumps.test.ts` with `UPDATE_GOLDEN=1` and justify every changed edge in the `.dump` diff. Unjustified churn means the fix is wrong.
- `eval:precision` on the named corpora, base and fixed builds, plus `node scripts/index-metrics.mjs <base.db> <fixed.db>` for the call edges gained or lost. No precision regression; report the deltas.
- Full suite green, with the stderr check.
- One user-facing CHANGELOG bullet in the unreleased Fixes section.
- Check README "About this fork"; these fixes normally change no row. Name the rows checked in the PR body.
- Conventional commit message (`fix(resolve): …`). No AI or tool attribution, no `Co-Authored-By`. Never name private projects.
- Push and open a PR with `gh pr create --repo bompus/codegraph --base fork/consolidated`. Body: one short paragraph per bug (trigger, wrong vs right edge, root cause, fix), the verification, and "README rows checked: …". Do not merge.

## Report back

PR URL, commit SHA, per-bug status (fixed, no longer reproduces, or deferred and why), test names, golden changes, precision deltas, suite counts, anything uncertain, and the start and end stamps.
