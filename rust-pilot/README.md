# Rust parity pilot

This standalone crate ports the NestJS resolver and dead-code report from the
repository at `6c8e8606`. It does not replace either TypeScript implementation
or join the native kernel's workspace.

The harness calls the original TypeScript modules through the installed
TypeScript compiler on Node 24. No compiled production tree is required.
`prepare-nest.ts` extracts the source objects from the two named NestJS tests
with the TypeScript AST, adds the edge cases, and selects real corpus files.
Both implementations receive the same source text, class spans, references,
file order, package manifests, and fixed clock (`1700000000000`). Canonical
JSON sorts string keys by UTF-16, emits array-index keys numerically first,
and preserves array order. Input objects retain JavaScript enumeration order. No result fields are removed.
A second post-extraction call checks idempotence.

For dead code, `copy-indexes.py` uses SQLite's backup API with the original
opened read-only, checks each copy's integrity, and links the copy's project
root to the real source files. Both implementations open that same copy
read-only. The seven queries cover the default, class kinds, limit one,
exported symbols, tests, disabled corroboration, and combined include flags
with additional kinds. `DeadCodeQuery` has no path filter.

## Run

Set `REPO`, `SCRATCH`, `NEST`, and `EVAL_REPOS` to absolute paths. `REPO` is this
checkout; `SCRATCH` is the pilot's supplied disk-backed receipts directory.
`NEST` is the NestJS repository; `EVAL_REPOS` holds the four indexed projects.

```sh
cargo build --manifest-path "$REPO/rust-pilot/Cargo.toml" -j 8
fnm exec --using codegraph node "$REPO/rust-pilot/harness/run-ts.cjs" \
  "$REPO/rust-pilot/harness/prepare-nest.ts" "$REPO" "$NEST" "$SCRATCH"
python3 "$REPO/rust-pilot/harness/copy-indexes.py" "$EVAL_REPOS" \
  "$SCRATCH/indexes" flask svelte vite vitest
python3 "$REPO/rust-pilot/harness/run-parity.py" "$REPO" "$SCRATCH" \
  fixtures edges nest flask svelte vite vitest
cargo test --manifest-path "$REPO/rust-pilot/Cargo.toml" -j 1
cargo clippy --manifest-path "$REPO/rust-pilot/Cargo.toml" -j 1 --all-targets -- -D warnings
```

Apply the host's memory cap and build admission rules. The comparison runner
uses `nice -n 10`, the supplied `run-capped 6G` wrapper, and Node 24. It records
exact commands, exit statuses, timestamps, output sizes, and SHA-256 hashes in
`commands.jsonl`; milestones go to `PILOT_LOG.md`. It fails if either program
fails or `cmp` differs. Existing snapshots are reused, so repeated runs do not
silently replace the inputs.

## Scope and limits

- Every NestJS resolver method and helper is ported. The dependency helper
  retains its file-count cache and bounded manifest search. Only the JS/TS
  comment-stripper branch is included; other languages and the optional
  cross-pass string memo are outside the NestJS call path. Comment blanking
  and route columns count UTF-16 code units.
- The store implements the SQL, row mapping, query chunking, and node cache
  used by the report. It is not a replacement for CodeGraph's complete store.
  Source corroboration and a custom reader are supported.
- Regexes use the linear `regex` engine where possible, with `fancy-regex`
  for lookaround and backreferences. First-match calls stop immediately.
  ECMAScript whitespace and ASCII word boundaries are explicit.
- Collation links the host's ICU4C 78 (`libicui18n.so.78`), replacing ICU4X's
  different Han ordering. This host has ICU 78.2; Node 24.21.0 has ICU 78.3.
  The test compares ordering and equality for every Unicode scalar with Node's
  `en-US` collator. Other locales and platforms are unverified. This system
  library is now a build/runtime requirement; SQLite remains bundled.
- `harness/generate-unicode.cjs` generates `src/unicode-ln.bin` from Node's
  Unicode 17 letter/number categories over all 1,114,112 code points.
  `--check` verifies every byte; Cargo tests run that check through Node 24.
  The 139,264-byte generated table is data, reported separately from Rust lines.
- JSON output uses JavaScript number formatting (`ryu-js`), numeric property
  enumeration, and UTF-16 key sorting. Manifest values use serde's raw JSON
  scanner so unused lone-surrogate values, large exponents and deep nesting
  do not prevent dependency detection.
- Rust strings contain valid Unicode scalar values. Synthetic JavaScript
  source/name strings with lone UTF-16 surrogates are outside this input format.
  Manifest contents and raw JSON metadata support escaped lone surrogates. Source
  files are decoded as UTF-8 by the TypeScript input builder; dead-code source
  reads replace invalid UTF-8 bytes as Node does.
- The fix-round harness measures the named route-heavy and corroboration
  workloads in debug and release builds. Those timings include process startup,
  TypeScript transpilation and canonical serialization. They are not isolated
  module timings or general runtime performance claims.
- This proves equality for the recorded inputs. It does not exercise every
  exclusion branch on every corpus or prove equality for all possible programs. No production files, goldens, docs-site
  pages, release material, or installed tool are changed.

The two reference module files contain 993 and 1,114 physical lines, including
comments and blanks. See the local report for the separate Rust module,
shared support, executable, and test counts. The full helper source files
are not silently added to that denominator.

## Review regressions

`cargo test` compares the committed ordering and manifest/duplicate-ID fixtures
with the real TypeScript resolver. It also checks large-file regex scans,
relative-root containment, Unicode categories, collation, number formatting
and decorator-length coercion.

`review-parity.py FIX_ROUND_DIRECTORY` replays copies of the reviewer's scripts,
inputs, queries and indexes. `--performance` runs three samples of each of six
workloads, comparing both Rust profiles with TypeScript each time. Its generous
fixed debug budgets detect the four measured performance regressions; use
`--performance --rounds 1 --baseline PRE_FIX_BINARY` to demonstrate failure.
The scripts record exact commands and statuses in JSONL. This pilot retains
its input and output receipts in the parent-provided scratch directory.

The additional review probes confirmed limits for malformed SQLite data:
non-UTF-8 TEXT and NULL node IDs are rejected by the Rust row mapper, while
Node accepts them. Stored JSON now retains lone surrogates, overflow numbers,
and deep values as raw JSON until canonical serialization. U+2028, tested SQL
ties, and non-boolean include flags match. The row-mapping limits are recorded
explicitly in the fix-round report.
