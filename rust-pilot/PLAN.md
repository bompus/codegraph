Owner: rust-pilot-20261013 implementation author; parent CalmPike
Status: completed
Purpose: Port the NestJS resolver and dead-code report to standalone Rust and prove byte-level parity.
Start: 2026-10-12 23:42:08 MDT, first host timestamp. The milestone log opened at 23:42:16 MDT.

## Tasks
- completed: Port NestJS and compare existing fixture sources, six added edge-case sets, and the NestJS corpus.
- completed: Port dead code and compare seven queries on each of four copied indexes.
- completed: Tests, Clippy, final comparisons, line counts, local implementation commit, and retained receipts.

## Ledger
- [x] Module A parity; evidence: final cmp exit 0 for four fixture file sets, six added edge-case sets, and 1,301 selected files from nest 4514623. One parity fix corrected ECMAScript whitespace.
- [x] Module B parity; evidence: final cmp exit 0 on flask, svelte, vite, and vitest, seven queries each. No reindex or parity fix.
- [x] Tests and Clippy; evidence: six Rust tests pass; clippy --all-targets -- -D warnings exits 0. No compiler, test, or Clippy fix rounds.
- [x] Local implementation commit and retained receipts; evidence: c9a5156ab7d18da90cbc90f83ec8d9ab3cc4df21 contains all 23 created files; commands.jsonl and final-parity.log record every comparison. The final reply reports the commit and linked receipts.
- [x] Owned checkout rust-pilot-20261013 on pilot/rust-port-nestjs-deadcode-20261013; outcome: retained for the parent, with local commits only. The existing dependency symlinks are parent-owned and unchanged.
- [x] Parent-supplied scratch directory rust-pilot-20261013; outcome: snapshots, canonical inputs/outputs, logs, and hashes retained for the parent's measurements.
- [-] NocoDB real-corpus run; by: parent, replaced with nest 4514623 after confirming that the supplied sparse checkout lacks NestJS source.
- [-] Publishing, PR, landing, and branch cleanup; by: user, explicitly outside this pilot.

## Measurement notes

Quota is unavailable to the author; the parent measures it externally. Whole-task
ETA was unmeasured because no comparable complete port run was available.
Initial dependencies compiled in 6.70 seconds. The first Module B build waited
for another benchmark, then ran as a bounded incremental check with one job.
It used 1.86 seconds wall, 1.70 seconds CPU, and 300,276 KiB peak RSS.
The first TypeScript harness attempt failed because tsx was absent; the harness
now uses the installed TypeScript compiler. These setup failures and waits are
included in elapsed author time.

The final TypeScript comparisons used Node 24.21.0, ICU 78.3, and en-US collation;
Rust used rustc/cargo 1.99.0. Full commands and statuses are in the supplied
scratch directory's commands.jsonl and final-parity.log. No reference source
changed. The README's parser, resolution, framework, and measured-results rows
were checked; the standalone pilot changes none of those production claims.

## Lessons

A sparse checkout can contain a frontend while omitting the framework backend.
Count matching inputs before treating a corpus comparison as evidence. The
original NocoDB selection found zero; the replacement selected 1,301 files.
JavaScript whitespace differs from Rust regex whitespace at U+FEFF and U+0085.
The added differential fixture failed before the translation and passes after it.

Completed: 2026-10-13 00:11:18 MDT. All requested comparisons pass; local-only scope is retained.

## Fix round 1

Started: 2026-10-13 00:43:07 MDT. The earlier comparisons cover only their listed inputs; review found additional defects.

- completed: H1–H3 harness ordering and before/after receipts.
- completed: N1–N4 resolver fixes and regressions.
- completed: D1–D6 dead-code fixes and regressions.
- completed: seven original comparisons and 55 named reviewer comparisons (all cmp 0), 16 Rust tests, clean Clippy, local fix commit containing this plan. Official frozen-commit timings transferred to parent CalmPike at the parent’s direction.

- [x] H1: evidence: copied reviewer case cmp exit 0; attempts: 1. Before receipts retained.
- [x] H2: evidence: copied reviewer case cmp exit 0; attempts: 1. Before receipts retained.
- [x] H3: evidence: copied reviewer case cmp exit 0; attempts: 1. Before receipts retained.
- [x] N1: evidence: copied reviewer case cmp exit 0; attempts: 1. Before receipts retained.
- [x] N2: evidence: copied reviewer case and surrogate-key fixture match; attempts: 2. Before receipts retained.
- [x] N3: evidence: copied reviewer case cmp exit 0; attempts: 1. Before receipts retained.
- [x] N4: evidence: named reviewer cmp exit 0, regression tests and timing receipts retained; attempts: 2.
- [x] D1: evidence: named reviewer cmp exit 0, regression tests and timing receipts retained; attempts: 1.
- [x] D2: evidence: named reviewer cmp exit 0, regression tests and timing receipts retained; attempts: 1.
- [x] D3: evidence: named reviewer cmp exit 0, regression tests and timing receipts retained; attempts: 1.
- [x] D4: evidence: named reviewer cmp exit 0, regression tests and timing receipts retained; attempts: 1.
- [x] D5: evidence: named reviewer cmp exit 0, regression tests and timing receipts retained; attempts: 1.
- [x] D6: evidence: named reviewer cmp exit 0, regression tests and timing receipts retained; attempts: 2.
- [x] Fix-round scratch and binary baseline: retained under the parent-supplied scratch directory for before/after evidence; outcome: handed to parent CalmPike.
- [x] Fix-round owned changes: contained in the local fix commit that includes this plan; evidence: final report records its SHA. No push or PR is authorized.

- [x] Additional suspicion checks: each probe took under five minutes. SQL ties and U+2028 match; includeTests:1 fixed; stored JSON surrogates fixed with D6. Invalid UTF-8 SQLite TEXT and NULL node IDs remain unsupported; investigation is complete and the parent receives the boundary evidence.
- [x] Baseline performance gates: original binary fails many1000, deep1500, deep3000 and P budgets; earlier corrected binary passes. Evidence: performance-before.jsonl and performance-unrefreshed.jsonl; these are unrefreshed reference measurements, not frozen-commit results.

Fix-round closeout prepared: 2026-10-13 01:46:04 MDT. Exact end and commit SHA are recorded in the parent-supplied report and PILOT_LOG.

- [x] Refreshed timings: parent directed skipping the wait and owns the official benchmark. Cancellation raced with lock release; the already queued build and timing job finished before cancellation succeeded. The late timing records are excluded from benchmark conclusions.
- [x] Kept worktree and pilot branch so the parent can benchmark the local commit; parent-owned dependency symlinks remain unchanged.

Fix-round lesson: canonicalization must preserve the input enumeration semantics before comparing output. An empty cmp is insufficient evidence when either producer failed; the replay runner checks all three exit statuses.
