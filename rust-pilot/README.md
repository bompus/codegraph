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
JSON sorts object keys and preserves array order. No result fields are removed.
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
- Rust uses `fancy-regex` with ECMAScript whitespace and ASCII word classes,
  `IndexMap` for insertion order, and ICU4X for English collation. The measured
  Node environment uses `en-US` and ICU 78.3. Other default locales and arbitrary
  Unicode collation differences between ICU versions remain unverified.
- Rust strings contain valid Unicode scalar values. Synthetic JavaScript
  strings with lone UTF-16 surrogates are outside this input format. Source
  files are decoded as UTF-8 by the TypeScript input builder; dead-code source
  reads replace invalid UTF-8 bytes as Node does.
- This proves equality for the recorded inputs. It does not measure Rust
  execution speed, exercise every exclusion branch on every corpus, or prove
  equality for all possible programs. No production files, goldens, docs-site
  pages, release material, or installed tool are changed.

The two reference module files contain 993 and 1,114 physical lines, including
comments and blanks. See the local report for the separate Rust module,
shared support, executable, and test counts. The full helper source files
are not silently added to that denominator.
