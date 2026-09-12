# Flask and Vite fallback-read investigation

Candidate `d79aaf19c5f7f4552253e0d08fa33f50887b72c4` versus integrated
`4b7a92eba3ffd83f2e4cb9f5c7ece14251c49887`, measured on 2026-09-12.
This changes retrieval and source rendering; it does not change extraction,
resolution, persisted edges or the extraction version.

## Evidence and cause

The [receiver comparison](receiver-phase2b-2026-09-12.md) recorded agents
reading source immediately after explore had returned the right file. Replaying
their actual queries showed missing requested ranges:

| Query/definition | Before lines returned | After lines returned |
|---|---:|---:|
| Flask dispatch query: `Flask.dispatch_request` | 5/25 | 25/25 |
| Flask dispatch query: `Flask.ensure_sync` | 0/13 | 13/13 |
| Flask exception query: `Flask.handle_user_exception` | 0/31 | 31/31 |
| Flask exception query: `Flask.handle_exception` | 0/52 | 52/52 |
| Flask URL query: `Flask.url_for` | 90/121 | 121/121 |
| Vite server query: core `createServer` | 0/5 | 5/5 |
| Vite transform query: `transformRequest` | 48/71 | 71/71 |
| Vite transform query: `doTransform` | 8/58 | 58/58 |
| Vite transform query: `loadAndTransform` | 0/208 | 208/208 |

These are exact definition-range coverage counts, not occurrences of a name
in a header, signature or call site. All twelve candidate probes stay within the
25,000-character response ceiling, taking 47–116 ms in this single replay.
The Gin controls retain their relevant function bodies; their peripheral
allocation changes, so their responses are not byte-identical.

Existing rules combined to hide the requested source:

- A cluster of adjacent functions spanning over 200 lines was windowed around
  its first flow call before checking whether the functions fit the byte budget.
  The fix removes that premature cut and retains the existing bounded trim.
- Symbol-only queries did not receive the source reservations already used by
  explicit file queries. Named callable bodies now receive that reservation.
- A two-symbol flow discarded its call-site information when there was no
  narrative to render. Keeping the evidence allows a long caller to be windowed
  around the relevant call, even without a three-node Flow section.
- An overloaded name without a matching class/file context fell back to the
  longest body. Other query symbols in the same file now corroborate the
  candidate, after explicit type context takes priority. This selects Vite's
  core wrapper rather than a longer playground implementation.
- An explicitly requested implementation file could still be reduced to
  signatures by the polymorphic-sibling renderer. Explicit file requests now
  use the normal budgeted source renderer; replaying the Gin JSON query restores
  the `WriteJSON` implementation. This gap was present in both baseline and
  initial candidate, as verified by exact-query replay.
- A larger named function could be dropped after shorter requested symbols
  were selected. It now receives a bounded excerpt before lower-priority
  declarations consume the remaining room. Files with more corroborating
  query symbols also rank ahead of weaker same-name matches.

Whole requested bodies are protected before filling the remaining room.
When a body is too large, bounded windows retain relevant call sites. For Vite's
841-line `resolveConfig`, the response includes the config-loading block;
for `_createServer`, it includes config resolution and HTTP-server creation.
This does not promise every large function in full. Gap markers remain visible,
and the source notes distinguish returned ranges from omitted source.

## Checks

The application builds on Linux and Windows. The full Linux suite passes
285 files and 4,870 tests, with 12 skips. Windows passes 88 targeted tests in
six files, including source bodies, allocation, displacement, oversized members
and cross-call deduplication. No native-code change or macOS validation was added.

The adjacent-flow fixture fails before the fix and passes afterward. Additional
regressions cover an oversized two-symbol caller and a corroborated overload.
Existing displacement and oversized-member gates still pass: a source fix
cannot consume another file's reserved budget or bypass the response ceiling.

## Agent comparison

Two preliminary comparisons (15 and 24 completed runs) were retained as
diagnostic evidence. They exposed the pinned-file and oversized-body gaps,
which were fixed before restarting. They are not mixed into the final
comparison. A replay of 64 recorded explore calls, with per-session source
history, verifies the actual follow-up sequences; the Vite range at lines
506–635 improves from 6/108 to 108/108 nonblank lines returned before the read.

The final balanced 36-run Sonnet/high comparison uses three questions per
corpus and two repetitions per build. Each build owns its corpus clones and
prewarmed daemons; the second repetition reverses arm order. All runs completed
successfully, with no tool races or CLI contamination. Full per-run evidence,
including the separate preliminary comparisons, is in the
[JSON report](explore-fallbacks-2026-09-12.json).

Counts below are totals across six runs per arm; duration is median (range).
Bash counts include fallback source searches, so zero Read/Grep alone is not
called source-free.

| Corpus | Build | Seconds | Tools | Read | Grep | Bash | Explore |
|---|---|---:|---:|---:|---:|---:|---:|
| flask | before | 24.9 (19.1–109.2) | 14 | 1 | 0 | 2 | 11 |
| flask | after | 25.7 (21.8–30.2) | 14 | 0 | 0 | 0 | 14 |
| gin | before | 15.2 (13.3–21.9) | 11 | 1 | 0 | 2 | 8 |
| gin | after | 16.3 (11.3–22.6) | 12 | 2 | 0 | 1 | 9 |
| vite | before | 28.2 (21.2–33.1) | 16 | 5 | 0 | 0 | 11 |
| vite | after | 21.1 (18.2–27.0) | 11 | 1 | 0 | 0 | 10 |

Flask uses no fallback source access in the candidate's six runs. Its median
is slightly higher, so this is a sufficiency improvement, not a measured median
speedup. The baseline's 109.2-second run searched the filesystem for external
Werkzeug source; that run is retained.

Vite uses fewer tools and reads, with a lower median time. Its remaining read
is `packages/vite/src/node/constants.ts:85–104`, after asking for
`DEFAULT_CONFIG_FILES` alongside the config loaders. This is a remaining
constant-retrieval gap; the requested function-body fixes do not establish
universal zero-read retrieval.

Gin has three fallback calls in each arm, one extra total tool call in the
candidate, and a median roughly one second higher. Exact replay of the three
candidate queries preceding those fallbacks finds identical source in both
builds: the JSON and binding-declaration responses are byte-identical, and the
binding-flow response differs only in its source-note sentence. These runs do
not establish a control speedup or statistical equivalence; they also do not
show a new source omission. The earlier explicitly pinned JSON-file query does
recover previously omitted implementation source.

### Feedback metrics

Residual file-access tokens and allocation efficiency are per-run medians.
Next-action counts are summed across explore calls, in the order
`explore again / read returned / read missed / search / sufficient`.
Allocation efficiency is citation-based; compare the same questions across arms.

| Corpus | Build | Residual tokens | Next actions | Allocation efficiency |
|---|---|---:|---|---:|
| flask | before | 0.0 | 5 / 1 / 0 / 1 / 4 | 0.861 |
| flask | after | 0.0 | 8 / 0 / 0 / 0 / 6 | 0.879 |
| gin | before | 0.0 | 2 / 1 / 0 / 1 / 4 | 1.000 |
| gin | after | 0.0 | 2 / 2 / 0 / 1 / 4 | 0.892 |
| vite | before | 2033.5 | 4 / 4 / 0 / 0 / 3 | 0.722 |
| vite | after | 0.0 | 4 / 0 / 1 / 0 / 5 | 0.929 |

The sample is two repetitions per question on three pinned corpora. It supports
the specific rendering fixes and the observed Vite improvement, not a general
latency guarantee. Source-free inference, external dependency source, constant
retrieval and broader zero-read coverage remain separate open work.
