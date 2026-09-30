# Kotlin grammar provenance and rebuild

The kernel compiles `codegraph-kernel/grammars/kotlin/` from
[fwcd/tree-sitter-kotlin](https://github.com/fwcd/tree-sitter-kotlin), MIT,
tag `0.3.8`, commit `e1a2d5ad1f61f5740677183cd4125bb071cd2f30`.
Use that tag's checked-in `src/parser.c` and apply
[`tree-sitter-kotlin.patch`](tree-sitter-kotlin.patch) to `src/scanner.c`.
The generated parser stays unchanged.

| File | Upstream SHA-256 | Patched SHA-256 |
|---|---|---|
| `src/parser.c` | `54104a7ef1555c265b746c790e0f8bb953cc17806e9df0c3af82f7f62c06a70a` | unchanged |
| `src/scanner.c` | `27f73337ec357fc341fa57538f34c14277b0346980c3405dc30beab6202ec6d0` | `2ca842dd04b60b0a6df59a03e9ad01209d8c5641f079a79a84d65708669b6cb6` |

## Scanner patch

On the same line, an infix call beginning with `e`, such as `Users.id eq id1`,
continues the expression. The scanner must not insert an automatic semicolon
before that word. The patch keeps the surrounding class and its methods
available to extraction.

## Rebuild

1. Clone tag `0.3.8` into a disk-backed scratch directory and verify the
   upstream hashes above.
2. Apply `docs/grammars/tree-sitter-kotlin.patch` to the clone's `src/scanner.c`.
   Verify the patched hash, then copy it into
   `codegraph-kernel/grammars/kotlin/scanner.c`.
3. From the CodeGraph checkout, rebuild with the supported runtime and Rust
   toolchain as described in the `codegraph-build-validation` skill.
4. Run `kotlin-infix-e-parse.test.ts`, `kotlin-infix-calls.test.ts` and the
   kernel golden dumps. Re-baseline and inspect dump changes, then run the
   Kotlin precision corpus and the full suite. Parse and extraction checks
   use the native parser.
