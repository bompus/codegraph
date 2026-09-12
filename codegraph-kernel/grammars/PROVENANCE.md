# Vendored grammar sources — provenance

Each directory holds the generated `parser.c` (+ `scanner.c`, headers) the
kernel compiles in `build.rs`. Sources are the same revisions the vendored
wasm grammars in `src/extraction/wasm/` were built from, so native and wasm
parse identically; where they are not, the kernel-only-extraction-plan.md
Phase 4 notes record the kind-table drift.

| dir | source | revision | notes |
|---|---|---|---|
| kotlin | fwcd/tree-sitter-kotlin | 0.3.8 tag | see kotlin checklist |
| lua | tree-sitter-grammars/tree-sitter-lua | v0.4.1 | see lua-luau checklist |
| scala | tree-sitter/tree-sitter-scala | master@0aca5d0a6f | see scala checklist |
| dart | UserNobody14/tree-sitter-dart | d4d8f3e | |
| arkts, arkts-common | npm tree-sitter-arkts 0.2.0 (harmony-contrib) | package `src/` + `common/` (the shared typescript-style scanner header; include path rewritten to `../arkts-common/`) | the vendored wasm is byte-identical to the package's artifact (sha db0812971109457d…) |
| terraform | npm @tree-sitter-grammars/tree-sitter-hcl 1.2.0 | package `src/` | ABI 15 |
| vbnet | govindbanura/tree-sitter-vbnet | 538b7087 + docs/grammars/tree-sitter-vbnet.patch | regenerated with tree-sitter-cli 0.25.10 (ABI 14); see docs/grammars/tree-sitter-vbnet.md |
| cobol | yutaro-sakamoto/tree-sitter-cobol | e99dbdc3 + docs/grammars/tree-sitter-cobol.patch | regenerated with tree-sitter-cli 0.25.10 (ABI 14); see docs/grammars/tree-sitter-cobol.md |
| cfml, cfscript, cfquery, cfml-common | cfmleditor/tree-sitter-cfml | a224ff10 (2026-06-30, the last commit before the 2026-07-02 vendoring) | three grammars sharing `common/scanner.h` + `tag.h` (include paths rewritten to `../cfml-common/`); ABI 15 |
