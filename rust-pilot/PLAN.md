Owner: rust-pilot-20261013 implementation author; parent CalmPike
Status: active
Purpose: Port the NestJS resolver and dead-code report to standalone Rust and prove byte-level parity.
Start: 2026-10-12 23:42:08 MDT, first host timestamp. The milestone log opened at 23:42:16 MDT.

## Tasks
- completed: Port NestJS and compare existing fixture sources, six added edge-case sets, and the NestJS corpus.
- completed: Port dead code and compare seven queries on each of four copied indexes.
- in_progress: Finish the local commit and measurement report. Tests, Clippy, and final comparisons have passed.

## Ledger
- [x] Module A parity; evidence: final cmp exit 0 for four fixture file sets, six added edge-case sets, and 1,301 selected files from nest 4514623. One parity fix corrected ECMAScript whitespace.
- [x] Module B parity; evidence: final cmp exit 0 on flask, svelte, vite, and vitest, seven queries each. No reindex or parity fix.
- [x] Tests and Clippy; evidence: six Rust tests pass; clippy --all-targets -- -D warnings exits 0. No compiler, test, or Clippy fix rounds.
- [ ] Local commit and final report; authorized by the pilot brief; next: commit this directory and give the parent the receipts.
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
