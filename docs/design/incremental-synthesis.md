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

Status: not built. The gain is modest (~30% of a Ruby-edit sync on discourse, ~2% of a JavaScript-edit sync), and the audit it needs is error-prone.

The narrower variant was built and measured, then left out. It skips only `cFnPtrEdges` when the fingerprint of the indexed C/C++ files (path, content hash, generated flag) is unchanged, and reuses the pass's raw output cached in the long-running process. Making it exact first required #208: the pass had been resolving handler names across every language. On CPython (1,126 C/C++ files, 2,368 Python) the pass takes ~0.55 s of a ~3.9 s resynthesis, spread over 30-odd passes, so the reuse saves ~0.5 s per Python edit. That is not enough to justify a module-level cache holding up to ~40 MB. The patch is kept outside the repository in case a mixed C project with a multi-second pass appears.

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

## Content-only skip cache (built 2026-09-26)

Most passes read every file of their languages and drop nearly all of them on a
test of the bytes alone: no `x[k](` for the object registry, no `emit(` or
`.on(` for the emitter pass, no JSX for the JSX-child pass. A sync paid that
read-and-test for every file on every run.

`synth_skips` (schema 15; one row per file since schema 16) records, per pass,
the files it dropped for content alone, keyed by the SHA-256 of the bytes it read. That is the same digest as
`files.content_hash`, so a skip applies only while the file's indexed hash still
equals it; an edited, reverted or deleted file is read again. The rows belong to
one build: `SYNTH_SKIPS_VERSION` is the package version plus a fingerprint of the
compiled `src/resolution/` modules, and any other version is ignored and
replaced. Only a decision that depends on the file's bytes alone may be
recorded; a pass that drops a file because of the graph or another file does not
record it. `src/resolution/synth-skips.ts` owns the helpers, and pool workers
send their recorded skips back for the main thread to write.

Passes using it: object registry, EventEmitter, window messages, JSX children,
Vue templates, Vuex dispatch, Pinia stores, NgRx effects, React Native events
(which also stops reading file types it never matches) and the cross-tier pass.
The same change stopped a sync from detecting frameworks twice when the
resolver was created fresh.

`__tests__/synth-skips.test.ts` checks that a sync using the skips ends with the
same synthesized edges as a fresh index after a skipped file gains a registry,
reverts to its skipped bytes, or is deleted, and after a build change.

### Measured (2026-09-26, idle host)

Paired one-file syncs, `fork/consolidated` `57b56018` against the branch,
alternating base, branch, branch, base, median of four syncs:

| Corpus | Before | After | Peak memory |
|---|---|---|---|
| gin | 0.39 s | 0.39 s | 212 → 212 MiB |
| Alamofire | 0.55 s | 0.52 s | 263 → 226 MiB |
| pretix | 2.53 s | 2.32 s | 585 → 577 MiB |
| CPython | 4.98 s | 4.79 s | 1,059 → 863 MiB |
| discourse | 5.91 s | 5.13 s | 1,065 → 969 MiB |
| supabase | 6.19 s | 5.15 s | 967 → 928 MiB |
| n8n | 13.79 s | 9.38 s | 1,616 → 1,374 MiB |

The object-registry pass on n8n went from 3.2 s to 0.02 s. The estimate before
building was 3.5–4× on n8n; the result is 1.47×, because the costliest passes
that remain do their work on files the content test keeps:

- The cross-tier pass matches about half of n8n's JavaScript and TypeScript
  files (any `.get(`); its work there depends on the graph, since the receiver
  can be an imported client. About 2 s on n8n.
- JSX children match most `.tsx` files on supabase (about 1 s); Pinia about
  0.9 s on n8n.
- Framework detection still reads every JavaScript file when no dependency
  names a framework (the HTTP-routing check), about 0.5 s per detection on n8n.
- Kernel resolution retries every previously failed reference on each sync
  (1,236 on n8n), about 1 s, plus 0.4 s to close the kernel connection.
- Full-table SQLite reads, about 1 s in total.

Each of these needs its own cache or scoping, not a larger skip set.

## Second round (built 2026-09-27)

Four changes to the costs listed above:

- **Framework detection.** The HTTP-routing check records the JavaScript
  files it read without finding a framework hint as `detect:http-routing`
  skips. The extraction phase of a sync passes the files it is re-indexing as
  stale, because their indexed hash is still the old one at that point. The
  resolver persists its detection skips; pool workers load them read-only.
  About 1.0 s to 0.34 s on n8n.
- **Cross-tier pass, empty files.** 2,205 of the 2,350 n8n files that pass the
  cross-tier gates add nothing. A file now records a skip when it collected
  nothing and never read past its own content: an import's target, a
  project-wide name lookup, or a framework route on the same line. A `live`
  flag on the file's facts marks each of those reads. About 2.2 s to 0.6 s on
  n8n.
- **Pinia.** The consumer scan reads only files that name one of the store
  factories, and skips the bound-call scan when the file binds none. Both are
  exact: every link starts at a factory's name.
- **Table shape.** One row per (pass, file) meant 248,563 rows on n8n, read
  twice per sync. Schema 16 stores one row per file with a space-separated
  pass list (24,435 rows). Saving merges passes recorded against the same
  hash, and a new hash replaces the row.

`__tests__/synth-skips.test.ts` adds two cases. A framework import added to a
skipped file must produce the routes a fresh index does. A caller whose
receiver comes from an import must not be skipped, and must gain its HTTP edge
when only the imported module changes. Removing the `live` mark on an
import's target fails the second.

### Measured (2026-09-27)

Same method, `fork/consolidated` `013f814a` against the branch, median of four:

| Corpus | Before | After | Peak memory |
|---|---|---|---|
| gin | 0.39 s | 0.39 s | 216 → 217 MiB |
| Alamofire | 0.53 s | 0.54 s | 225 → 226 MiB |
| pretix | 2.34 s | 1.85 s | 579 → 580 MiB |
| CPython | 4.41 s | 4.62 s | 903 → 902 MiB |
| discourse | 5.04 s | 4.04 s | 979 → 945 MiB |
| supabase | 4.72 s | 3.56 s | 930 → 971 MiB |
| n8n | 9.54 s | 6.95 s | 1,386 → 1,261 MiB |

Other sessions raised the load to 4–7 during the discourse and n8n arms;
CPython's difference is within run-to-run spread.

Not done:

- Retrying only the failed references that could now resolve: about 0.24 s on
  n8n, and narrowing to the changed definitions misses an export or kind
  change that keeps a name.
- Storing Pinia's per-file facts (a new table) for about 0.5 s on Vue-heavy
  projects.

What remains on an n8n sync (about 6.9 s): kernel resolution about 0.9 s,
full-table node reads for the method and function passes about 1.2 s, `git
ls-files` about 0.4 s, and closing the kernel connection about 0.4 s.

## Third round (built 2026-09-27)

Most passes that scan a node kind keep a handful of rows: one language, a
name pattern, or a signature. They read every row of the kind and dropped the
rest in JavaScript. On n8n the observer pass alone turned about 68,000 method
and function rows into objects per sync; 3,500 of them have a registrar or
dispatcher name, and the Swift/Kotlin closure pass matches none. `iterateNodesByKind` now takes an
optional filter (languages, `LIKE` patterns on the name or signature) that the
query applies. The patterns are a superset of each pass's own check, which
stays, and a filtered scan orders by rowid, the order the kind index gives an
unfiltered one. A fresh index yields identical edges with and without the
change on gin, Alamofire and halo.

On n8n the node-scan cost went from about 710 ms to about 180 ms, and callback
synthesis from about 3.0 s to 2.4 s.

### Measured (2026-09-27)

Same method, `fork/consolidated` `41e7f7df` against the branch, median of four:

| Corpus | Before | After | Peak memory |
|---|---|---|---|
| gin | 0.39 s | 0.37 s | 217 → 211 MiB |
| Alamofire | 0.48 s | 0.48 s | 227 → 228 MiB |
| pretix | 1.77 s | 1.69 s | 578 → 586 MiB |
| CPython | 4.50 s | 3.65 s | 881 → 849 MiB |
| discourse | 4.02 s | 3.56 s | 968 → 932 MiB |
| supabase | 3.88 s | 3.48 s | 973 → 977 MiB |
| n8n | 6.66 s | 6.24 s | 1,230 → 1,236 MiB |

Supabase's base syncs ranged from 3.55 s to 4.37 s under load from other
sessions; its difference is mostly noise.

What remains on an n8n sync: kernel resolution about 0.9 s, `git ls-files`
about 0.4 s, closing the kernel connection about 0.4 s, and the class and
constant scans that still read their whole kind (tens of milliseconds each).

## Fourth round (built 2026-09-27)

A profile of an n8n sync put three fixed costs next to kernel resolution:

- **The resolver's file list.** The framework detectors and synthesis passes
  asked the resolver context for every indexed path about 35 times per sync,
  and each request re-read the `files` table: about 230 ms. The resolver now
  keeps the list as long as its node-kind cache and hands each caller a copy.
- **Freeing the kernel's node table** on close took about 0.35 s of the sync.
  Moving the free onto a short-lived thread gave no measurable gain in a
  paired A/B on CPython and n8n, so it was not kept.
- **The ignore filter** in the file scan, about 0.3 s: roughly 220 rules
  checked against 36,000 paths. A persisted per-path decision cache would need
  every writer, the watcher included, to invalidate it when a rule file
  changes, and watcher-scoped syncs skip the scan anyway. Prefiltering to
  source files first saves about 50 ms but changes the scan's skip counts.
  Neither was built.

### Measured (2026-09-27)

Same method, `fork/consolidated` `19be510f` against the branch, median of four:

| Corpus | Before | After | Peak memory |
|---|---|---|---|
| gin | 0.37 s | 0.37 s | 213 → 211 MiB |
| Alamofire | 0.48 s | 0.47 s | 228 → 225 MiB |
| pretix | 1.70 s | 1.75 s | 590 → 571 MiB |
| CPython | 3.58 s | 3.43 s | 834 → 840 MiB |
| discourse | 3.93 s | 3.46 s | 926 → 945 MiB |
| supabase | 3.51 s | 3.36 s | 933 → 970 MiB |
| n8n | 6.14 s | 6.01 s | 1,239 → 1,220 MiB |

The pretix difference is within its run-to-run spread, and one slow base sync
(5.34 s) raises discourse's base median.

What remains on an n8n sync: kernel resolution about 0.9 s, freeing the
kernel's node table about 0.35 s, and the ignore filter about 0.3 s.

## Fifth round (built 2026-09-27)

The kernel-resolution cost left above was not resolution. The scoped sync
defers `this.<member>` function references to a pass that runs after the
supertype edges exist (`resolveDeferredThisMemberRefs`), and their rows stay
`pending` until it does. The orphan sweep, which exists to finish refs a
killed run left behind, ran before that pass and counted them. Any sync of a
file with such a reference then ran the whole batched resolver, which loaded
the full node table (about 0.75 s on n8n, and 0.35 s to free it), and ended in
a full synthesis pass instead of the incremental refresh.

The sweep now counts pending rows outside the resolver's deferred row ids, so
a real orphan still triggers it. When the batched path does run on fewer than
15,000 pending refs, it looks nodes up by query, as the scoped path already
did. A one-file n8n sync yields identical edges before and after, as do full
indexes of gin and Alamofire.

### Measured (2026-09-27)

Same method, `fork/consolidated` `32ad57cd` against the branch, median of four:

| Corpus | Before | After | Peak memory |
|---|---|---|---|
| gin | 0.38 s | 0.37 s | 210 → 211 MiB |
| Alamofire | 0.48 s | 0.47 s | 225 → 226 MiB |
| pretix | 1.78 s | 1.64 s | 570 → 569 MiB |
| CPython | 4.03 s | 3.50 s | 843 → 845 MiB |
| discourse | 3.22 s | 3.29 s | 937 → 803 MiB |
| supabase | 3.17 s | 3.19 s | 970 → 970 MiB |
| n8n | 6.19 s | 4.90 s | 1,228 → 961 MiB |

Only the n8n and discourse benchmark files hold a deferred reference; the
other differences are run-to-run spread. Discourse's second syncs went from
3.1–3.3 s to 2.7 s; its median includes the noisier first syncs.

What remains on an n8n sync: the failed-ref retry (about 1,200 refs, 0.25 s,
most of them imports that never resolve), the ignore filter about 0.3 s, and
the registry and tier synthesis passes.

### Fixed costs: ignore filter, comment stripping, Pinia (2026-09-27)

Three costs every n8n sync paid regardless of the file changed:

- The `.gitignore` filter tested each of 24,435 paths against every rule in
  turn (170 rules). One regex joining all rules answers the common case, no
  rule matches; a path some rule matches still goes through `ignore`'s own
  rule walk (`src/extraction/ignore-prefilter.ts`).
- `stripCStyle` built a per-character array for every file the synthesis
  passes read; it now copies unchanged slices. A result that no longer holds a
  non-Latin-1 character is re-encoded to one byte, since the passes' regexes
  run about a third slower over two-byte text.
- The Pinia pass matched every `a.b(` call and dropped unbound receivers; it
  now searches for the bound receivers only.

Full indexes of n8n, nocodb, supabase, koel, firefly-iii and discourse are
identical before and after (files, node ids, every edge column). On the n8n
benchmark file the scan went from 495 to 313 ms, Pinia from 525 to 270 ms and
tier from 600 to 540 ms; the second syncs went from 4.5–4.6 s to 3.9–4.2 s
(benchmark table in `docs/benchmarks/fork-vs-upstream-2026-09-26.md`).

Tier's remaining time is its HTTP, queue and event regexes over files whose
results depend on other files. Cutting it needs a per-file result cache.

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
