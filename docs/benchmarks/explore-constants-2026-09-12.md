# Explicit constants in explore

Candidate `41d9740f26d2039a80085cd106574460e0bc4b94` versus
`c32a76c526215cc94f2ececc2cc67f361453a998`, measured on 2026-09-12.

## Cause and correction

The [previous comparison](explore-fallbacks-2026-09-12.md) retained one Vite
read of `constants.ts` after the agent explicitly named `DEFAULT_CONFIG_FILES`.
The index already contains its complete declaration at lines 94–101. The
retrieval isolation penalty assigned it a score of 4, below the file-selection
floor of 10, because it has no qualifying usage edge. A listing still named the
constant, but its source file never reached the renderer.

Precisely named constants and variables now retain their normal kind weight
when usage edges are absent. The existing exact-name and token-shape checks
limit this exemption: incidental matches, uncorroborated natural-language words
and substring matches retain the isolation penalty. Output budgets, graph
edges, extraction and resolution are unchanged.

Replaying the captured two-call sequence (`resolveConfig loadConfigFromFile`,
then `bundleAndLoadConfigFile runnerImportConfigFile nativeImportConfigFile
DEFAULT_CONFIG_FILES`) with session history returns 8/8 declaration lines,
previously 0/8. The second response is 24,936 characters, previously 24,810,
within the 25,000-character ceiling.

## Validation

The new regression fixture names a constant alongside three loader functions
without any usage edge. It fails with the isolation exemption removed and
passes with the fix. Existing relevance tests still exclude unused local name
matches. The full Linux suite passes 4,871 tests in 285 files, with 12 skips;
the application build passes. This platform-neutral selection change has no
new Windows or macOS run.

Twelve deterministic control queries (three Flask, four Gin, five Vite) return
byte-identical responses across builds. These include the three earlier flow
questions per corpus and the explicit Vite server/Gin JSON follow-ups. Candidate
probe latency is at most 117 ms in this replay.

## Agent comparison

A fresh 12-run Sonnet/high comparison covers Vite config loading, Flask request
dispatch and Gin JSON rendering, with two repetitions per build and reversed
arm order in the second repetition. Each arm owns its indexed corpus clones
and prewarmed daemons, with the CLI-contamination guard enabled. This focused
comparison supplements the deterministic controls; it does not repeat every
question in the earlier 36-run campaign.

All twelve runs complete successfully without tool races or CLI contamination.
The second Vite baseline run reproduces the exact two explore queries, then
uses Bash `grep` to retrieve the missing declaration. Both candidate Vite runs
use two explore calls with no fallback; the second adds `mergeConfig` to the
follow-up query. The exact original query is separately covered by replay.
All six candidate runs have no Read, Grep or Bash fallback.

Counts are totals across two runs per arm. Time is median (range).

| Corpus | Build | Seconds | Tools | Read | Grep | Bash | Explore |
|---|---|---:|---:|---:|---:|---:|---:|

| flask | before | 22.4 (19.9–25.0) | 4 | 0 | 0 | 0 | 4 |
| flask | after | 23.1 (21.5–24.8) | 4 | 0 | 0 | 0 | 4 |
| gin | before | 18.1 (17.4–18.7) | 6 | 2 | 0 | 1 | 3 |
| gin | after | 17.6 (14.3–20.9) | 5 | 0 | 0 | 0 | 5 |
| vite | before | 24.7 (21.4–27.9) | 5 | 0 | 0 | 1 | 4 |
| vite | after | 22.0 (18.9–25.1) | 4 | 0 | 0 | 0 | 4 |

The sample supports the specific constants-retrieval correction. The control
queries already return identical source deterministically, so different Gin
fallback counts are not evidence of a Gin source-coverage improvement. Flask's
median is slightly higher; these short runs do not establish a general speedup.

### Feedback metrics

Residual file-access tokens and allocation efficiency are per-run medians.
Next-action counts are totals in the order
`explore again / read returned / read missed / search / sufficient`.
Allocation efficiency is citation-based and comparable for the same question.

| Corpus | Build | Residual tokens | Next actions | Allocation efficiency |
|---|---|---:|---|---:|
| flask | before | 0.0 | 2 / 0 / 0 / 0 / 2 | 0.736 |
| flask | after | 0.0 | 2 / 0 / 0 / 0 / 2 | 0.853 |
| gin | before | 718.0 | 1 / 1 / 0 / 0 / 1 | 1.000 |
| gin | after | 0.0 | 3 / 0 / 0 / 0 / 2 | 0.921 |
| vite | before | 60.5 | 2 / 0 / 0 / 1 / 1 | 0.818 |
| vite | after | 0.0 | 2 / 0 / 0 / 0 / 2 | 0.897 |

[Machine-readable evidence](explore-constants-2026-09-12.json) includes every
run, control probe and exact-sequence coverage count. The earlier Gin fallback
work, source-free inference and broader zero-read coverage remain open.
