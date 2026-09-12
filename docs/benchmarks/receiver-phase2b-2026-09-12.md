# Expanded receiver-rule validation

Candidate `a1601996475f19b0c06f32008638928be2335559` versus integrated
`8976641d217d1cbce6cc4bfcb9baf5ba6c8d0017`, measured on 2026-09-12.
The baseline has the same engine code as `d3c330db`. The final Java-only
follow-up is `fb0f239d`: parsed type-parameter declarations prevent an inner
unbounded parameter from borrowing an outer bound or same-named class.
The A/B below uses the frozen `a1601996` build. This candidate extends
ordinary receiver evidence to the migrated languages and persists
`unknown-receiver` diagnostics. Extraction version 35 requires re-indexing;
schema version 12 preserves the diagnostic across retries.

## Precision and recall

The baseline and final `fb0f239d` build indexed the same pinned corpus contents. Edge identity includes
source, target, kind and call-site position; resolver-label changes alone do
not count as added or removed edges. All 20 scored cases pass. The ten
`precision-*-fb0f239d.json` reports retain the exact endpoints and outcomes.

| Corpus | Before edges | After edges | Removed | Added |
|---|---:|---:|---:|---:|
| Vite | 28,174 | 28,174 | 0 | 0 |
| Vitest | 70,574 | 70,574 | 0 | 0 |
| Svelte | 69,610 | 69,610 | 0 | 0 |
| Flask | 5,292 | 4,732 | 1,004 | 444 |
| Gin | 8,544 | 8,402 | 315 | 173 |
| Petclinic | 1,457 | 1,450 | 7 | 0 |
| Exposed | 80,573 | 77,255 | 3,500 | 182 |
| Slim | 5,016 | 4,462 | 559 | 5 |
| jq | 7,713 | 7,713 | 0 | 0 |
| json | 24,647 | 22,817 | 1,832 | 2 |

The first candidate passed its unit suite but lost valid typed calls on real
repositories. Those findings drove regression tests and these corrections:

- JS/TS/TSX members remain compatible across their language family. This
  restores all 223 Svelte and one Vite edges initially lost.
- Python public package imports follow module-scope aliases, with root and
  `src` packages preferred over unrelated nested fixtures. Flask's deeply
  nested test `flask.py` no longer blocks the real package. Its Blueprint
  route control resolves through the actual inherited method.
- Go factory receivers use declared result types; package values and
  lowercase type names remain usable. Calls on an Engine now reach its own
  `Use` method rather than the embedded RouterGroup's same-named method.
- Java enhanced-for elements and bounded type parameters retain their
  members. Kotlin wildcard imports select the JDBC or R2DBC declaration
  actually imported, instead of whichever same-named object was picked first.
- PHP retains the positive `instanceof HttpException` control without giving
  the narrowed type to an alternate branch, reassigned value or nested function.

Representative removals are incorrect external/library matches: Go test
runner `t.Run` to `Engine.Run`, filesystem/network `Close` calls to `failRead.Close`,
Kotlin date/time calls to a test `FixedClock.now`, and JSON value `dump` calls
to an unrelated internal `serializer.dump` method. Added Flask edges include
public imports resolving to the actual Flask/Blueprint classes and the
module-level `url_for` helper.

This is a scored truth set plus source review of changed-edge samples, not a
claim that every removed edge was wrong. Untyped pytest fixtures, higher-order
callbacks, unsupported factory-result positions and other missing type flows
can lose previously guessed links. General type inference and the zero-read
retrieval target remain separate, unfinished work. The existing C++ `begin`
ambiguity follow-up is not closed by the new `dump` precision control.

## Implementation checks

The Linux native kernel and application build pass at `fb0f239d`. The full suite passes
284 files and 4,866 tests, with 12 skips. Windows rebuilt the kernel and
application at the same commit using Node 24.21.0, then passed 104 targeted
tests across nine files, including receiver rules, diagnostics, wildcard
bindings, golden dumps, recovery, visibility, PHP aliases and package-source
entries. No macOS run was added.

The final follow-up golden change is one Kotlin wildcard-import binding;
all other golden rows are unchanged from the first candidate. The complete
change from baseline also records diagnostic reasons, Python receiver text,
and the reviewed receiver-evidence changes in the earlier candidate's goldens.

Scoped iteration parsing now checks whether a name can be introduced by a
range or lambda before parsing its file. Final single-pass index times were
0.8 seconds for Gin and 12.8 seconds for Exposed; intermediate candidates
that reparsed ordinary unknown receivers took 7.4 and 58.1 seconds,
respectively. These are diagnostic observations, not a repeated performance
benchmark or a general indexing-speed claim.

## Retrieval validation

The final follow-up reproduces all ten corpus edge dumps and all ten candidate
probe responses byte-for-byte from `a1601996`, so it preserves the A/B inputs
and graph context for Flask, Gin and Vite.

Twenty deterministic explore probes returned successfully across Flask, Gin
and Vite, using each build's own index. All four Vite responses, including the
factory-resolution control, are byte-identical. Flask and Gin responses change
with their corrected graph context. Candidate probe times range from 40 to
113 milliseconds in this run.

The balanced 36-run Sonnet/high comparison completed with all runs successful,
zero tool races and zero CLI contamination. Each of three questions per corpus
ran twice per build with a prewarmed daemon; repetition two reversed arm order.
The [machine-readable report](receiver-phase2b-2026-09-12.json) contains the
questions, every run, all three feedback metrics and probe hashes.

The table reports six runs per row. Counts are totals, duration is median
(range), and the Grep tool count is zero in every row. Bash counts are included
because filesystem searches and shell `grep` are still fallback access.

| Corpus/build | Seconds | Tools | Read | Bash | CodeGraph |
|---|---:|---:|---:|---:|---:|
| Flask before | 33.77 (20.05–142.99) | 18 | 4 | 2 | 12 |
| Flask after | 27.65 (19.42–163.94) | 18 | 6 | 2 | 10 |
| Gin before | 13.03 (9.44–22.83) | 8 | 0 | 0 | 8 |
| Gin after | 14.02 (12.11–16.93) | 11 | 0 | 1 | 10 |
| Vite before | 27.06 (19.76–32.97) | 18 | 6 | 1 | 11 |
| Vite after | 28.29 (21.66–44.18) | 21 | 9 | 2 | 10 |

| Corpus/build | Median residual file-access tokens | Explore → sufficient | Explore → read returned / missed | Median allocation efficiency |
|---|---:|---:|---:|---:|
| Flask before | 602.5 | 4/12 | 3 / 0 | 88.1% |
| Flask after | 721 | 3/10 | 3 / 0 | 87.6% |
| Gin before | 0 | 6/8 | 0 / 0 | 87.3% |
| Gin after | 0 | 5/10 | 0 / 0 | 89.3% |
| Vite before | 4,051.5 | 2/11 | 4 / 0 | 56.6% |
| Vite after | 4,220.5 | 1/10 | 5 / 1 | 63.3% |

Flask's first URL-building repetition searched the filesystem for external
Werkzeug in both arms, producing the 143/164-second outliers. Those runs remain
in the results. Transient activity from another repository was observed during
the final Vite baseline run; small timing differences are not causal evidence.
Vite's graph and fixed-query responses are unchanged, while its agent query
choices and fallback reads vary. Gin makes no Read/Grep tool calls in all twelve runs,
but one candidate run reads source using `grep` through Bash. The Vite Bash
calls also run `grep`; these are not zero-source-access runs. The mixed outcomes do not establish a
general speedup, a no-fallback guarantee or completion of the zero-read target.
