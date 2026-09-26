# Incremental synthesis after a sync

Status: proposal, 2026-09-26. Nothing here is built yet; Milestone 1 was measured and set aside (see below).

## Problem

Since #197, every sync that changes a file deletes every synthesized edge
and re-runs every synthesis pass over the whole graph. That is exact: a sync
ends with the edges a fresh index would hold. The cost grows with the project,
not with the change.

Measured on the Linux kernel, one-file sync of `kernel/sched/core.c`
(`CODEGRAPH_SYNTH_TIMINGS=1`, after #199):

| Step | Time |
|---|---|
| Whole sync | ~1:30 |
| Resynthesis | ~67–71 s |
| C function-pointer pass | ~51 s |
| — stage A, per-file scan (kernel) | ~9–11 s |
| — stage C, registrations (env 3.4 s, units 5.7 s, include re-scans 6.5 s) | ~16 s |
| — bare field assignments | ~21 s |
| — stages D+E, propagation and dispatch (kernel) | ~23 s |
| Other passes, plus deleting and re-inserting ~330k edges | ~17–19 s |

On ordinary repositories resynthesis costs 1–3 s, so this is a large-C-project
problem first. It is also the only part of a Linux one-file sync that isn't
proportional to the edit.

## Goal

- Exact: after any sequence of syncs, the synthesized edges equal a fresh
  index. The gates are `__tests__/c-fnptr-sync.test.ts` (extended below) and
  the `fn-pointer-dispatch` edge hash on Linux after scripted edits.
- A one-file edit to a `.c` file on Linux resynthesizes in a few seconds,
  not ~70 s.
- No slower on small repositories; the current full rerun stays as the
  fallback whenever the incremental path can't prove it is exact.

## Milestone 1: skip passes whose inputs did not change

Every pass in `SYNTH_PASSES` already has a `gate` that says which languages it
needs in the project. A sync can use the same information the other way
round: if no changed, added or removed file is in a language the pass reads,
its output cannot have changed, so keep its edges and skip it.

What this needs:

1. **Edge ownership.** Each pass lists the `synthesizedBy` tags it emits, so a
   sync deletes only the edges of the passes it re-runs. `refreshSynthesis`
   replaces its wholesale delete with a per-pass delete.
2. **Declared inputs.** A `reads` language list per pass, next to `gate`.
   `ALWAYS` passes read every language and always re-run. Cross-language
   passes (React Native events, Expo cross-platform) list every language they
   touch.
3. **Dependencies between passes.** The Go pre-passes persist `contains` /
   `implements` edges that `ifaceEdges` reads, so re-running a Go pre-pass
   forces `ifaceEdges`. Any other pass that reads synthesized edges from the
   DB must be found and declared the same way before this ships.
4. **First-seen dedup.** `synthesizeCallbackEdges` keeps the first copy of a
   duplicate edge in pass order. When an earlier pass re-runs and emits an
   edge a later, skipped pass already stored, a fresh index would keep the
   earlier pass's copy (its metadata). The insert for re-run passes has to
   replace a kept edge from a later pass, not `INSERT OR IGNORE` it.

Effect: in a mixed repository, a TypeScript edit no longer re-runs the C pass,
and a C edit no longer re-runs the JavaScript passes. It does nothing for the
Linux case above, because the edited file is C. It is small, general, and a
prerequisite for Milestone 2's edge ownership, so it goes first.

### Measured before building (2026-09-26)

On discourse (15,564 files, Ruby and JavaScript), a one-file sync takes ~5 s, of which resynthesis is ~3 s. With `CODEGRAPH_SYNTH_TIMINGS=all`:

| Edit | Passes a language gate could skip | Time they take |
|---|---|---|
| One Ruby file | `tierEdges`, `rnEventEdgesList`, `rnXPlatEdges`, `windowMessageEdges`, `jsxEdges`, … | ~1.5 s |
| One JavaScript file | `sidekiqEdges` and a few small ones | ~0.1 s |

The `ALWAYS` passes (`emitterEdges`, `registryEdges`, `fieldEdges`, `closureCollEdges`) take ~1 s and re-run on every edit.

Three problems the plan above missed:

- **A gate is not a list of inputs.** `tierEdges` is gated on JavaScript but links front-end calls to backend routes, so a Ruby route edit changes its output. Every pass needs a hand-audited `reads` list, and a missed input shows up as silently stale edges.
- **Passes read edges.** Kept edges from skipped passes would be in the database while the re-run passes execute, and some passes query edges. A fresh index never shows them those edges. Kept edges would have to be moved aside during the re-run.
- **Shadowed duplicates.** When a re-run pass stops emitting an edge that used to shadow a skipped pass's duplicate, a fresh index would contain the skipped pass's copy, which was never stored. Being exact needs each pass's raw output, either persisted or cached in a long-lived process.

Status: not built. The gain is modest (~30% of a Ruby-edit sync on discourse, ~2% of a JavaScript-edit sync), and the audit it needs is error-prone. A narrower variant is exact with much less audit: skip only `cFnPtrEdges` when no C/C++ file changed, reusing its raw output cached in the daemon. It helps mixed C projects (CPython, Node.js) and does nothing for the Linux kernel, where every edit is C.

## Milestone 2: an incremental C function-pointer pass

### What the pass computes today

| Stage | Per file or global | Reads |
|---|---|---|
| A. Scan (kernel `cfnptrScanPaths`) | per file | the file's text, its struct node extents |
| B. Struct layouts | global | every file's struct facts, in kind-scan order |
| C. Registrations (`processUnit`, include re-scans) | per file, writing global tables | the file, its two-level include closure (macros, defines), global layouts, typedefs, aliases, `resolveFn` (a global name lookup) |
| Bare field assignments | per file, writing `reg` | the file's function bodies, global layouts, `resolveFn` |
| D. Propagation (kernel) | per file, then a global fixpoint | the file, global tables |
| E. Dispatch (kernel) | per file | the file, the final `reg` / `arrayReg` |

The per-file parts dominate the time. The global parts (B, the D fixpoint)
are cheap once the per-file outputs exist.

### Order hazards

The pass is exact today because it processes files in one fixed order. Three
places depend on that order. An incremental version has to reproduce each one:

1. **`reg` insertion order.** Dispatch emits at most `FANOUT_CAP` (300)
   targets per site, in `reg` order, so which handlers survive the cap depends
   on the order registrations were added.
2. **Layouts found during stage C.** An inline `struct TAG { … } var[]` table
   registers its layout while stage C runs (`registerStructLayout` inside the
   unit scan), so only files processed after it see that layout.
3. **`globalVarType` is last-write-wins** across files.

Step 2a changes no behavior: tag every contribution with `(file ordinal,
sequence within the file)` and rebuild the global tables from the tagged
contributions by a sorted merge. The golden dumps and the Linux hash must be
unchanged. This makes the order explicit before anything is cached.

### Persisted per-file contributions

The stage-A experiment in #199 showed that caching per-file output as JSON
blobs doesn't pay: 52k rows and 77 MB on Linux, and reading them back cost
nearly as much as rescanning. So persist the small, derived contributions in
relational tables instead:

- `cfn_reg(file, seq, key, fn_id)` and `cfn_arr_reg(file, seq, name, fn_id)`: registrations.
- `cfn_prop(file, to_key, from_key)`: propagations.
- `cfn_layout(file, seq, struct, fields)` and `cfn_var_type(file, seq, var, struct)`: stage-C globals.
- `cfn_includes(file, target)`: the resolved include edges, for reverse lookups.
- `cfn_ref_names(file, name)`: the function names a file's registrations resolved, for `resolveFn` invalidation.
- `cfn_dispatch_keys(file, key)`: the `struct.field` / array keys a file dispatches on.

Each row carries its file's ordinal, and every table cascades with its file.

### Which files a sync must redo

Start from the changed, added and removed files, then add:

- the includers of any changed header, up to the depth `buildEnv` follows (2),
  via `cfn_includes`;
- files whose `cfn_ref_names` contain a function name that the change added
  or removed (`resolveFn` may now answer differently);
- the dispatch files whose `cfn_dispatch_keys` hit a key whose final `reg` /
  `arrayReg` list changed after the fixpoint.

Fall back to the full pass when:

- the global typedef, alias or struct-layout sets change;
- a changed file contributed a stage-C layout (hazard 2);
- the redo set passes a size threshold (a common header such as
  `include/linux/fs.h` reaches thousands of includers, and the full pass is
  simpler there).

Then:

1. Run A, C, the bare-assignment scan and D for the redo set only, and
   replace those files' rows.
2. Rebuild `reg` from all rows in ordinal order and run the fixpoint in memory.
3. Run E for the affected dispatch files.
4. Replace only the `fn-pointer-dispatch` edges those files own. Milestone 1's
   ownership makes that a scoped delete.

### Expected effect

A `.c` edit on Linux redoes one file plus the dispatchers of any `reg` key it
changed. The remaining cost is loading the contribution tables and running
the fixpoint. That should be a few seconds, but it is unmeasured until step 2a
reports the table sizes. Header edits often fall back to the full ~50 s.

### Validation

- Extend `c-fnptr-sync.test.ts` with a header edit that changes includers,
  a change to a function name that other files register, an edit that crosses
  the fan-out cap, and each fallback trigger.
- A test-only switch that runs both the incremental and the full pass on every
  sync and fails on any difference, used by the sync suites.
- A Linux script that applies N scripted edits (`.c` bodies, tables, headers)
  and compares the edge hash with a fresh index after each.

## Milestone 3: the rest of resynthesis

With Milestone 1's ownership, deleting and re-inserting ~330k edges shrinks to
the re-run passes' edges. The `ALWAYS` passes still re-run on every sync;
measure them one by one before designing anything for them.

## Costs and risks

- **Size.** The contribution tables add rows proportional to registrations,
  not to source text. They're unmeasured until step 2a; size them before
  committing to step 2b.
- **Complexity.** The pass is already ~1,700 lines with a kernel half. The
  incremental path adds invalidation logic whose bugs show up as silently
  missing or stale edges. The dual-run test switch is what keeps that honest.
- **Order.** Hazards 1–3 are the exactness risk; step 2a exists to retire
  them before any caching.

## Order of work

1. Milestone 1: pass ownership, declared inputs, dependency and dedup rules.
2. Step 2a: order-tagged contributions, with no behavior change.
3. Measure the contribution table sizes on Linux, then decide on step 2b.
4. Step 2b: persisted contributions, the redo set, fallbacks, and the dual-run gate.
