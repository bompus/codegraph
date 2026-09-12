# Receiver-binding validation

Candidate `d3c330db` versus pre-receiver `86fc9dbc`, measured on 2026-09-12.
The candidate includes TS/JS receiver gating, the Go composite-literal fix,
explicit Expo receiver resolution, and the package-source prerequisite needed
to retain Vite's factory flow. It does not complete every language's receiver
rule or persist an `unknown-receiver` failure reason.

## Deterministic precision

Each build indexed its own copy of the pinned corpus. The library indexing
path includes tests. These totals are edge identity comparisons; changes only
to resolver metadata are not counted as changed endpoints. The four candidate
reports in `__tests__/evaluation/results/precision-*-d3c330db.json` include exact
scored endpoints and all passed, with no unjudgeable cases.

| Corpus | Before edges | After edges | Removed | Added | Scored cases |
|---|---:|---:|---:|---:|---|
| Vite | 28,730 | 28,174 | 677 | 121 | 6 absent, 2 present held |
| Vitest | 74,878 | 70,574 | 4,477 | 173 | 1 absent held |
| Svelte | 70,587 | 69,610 | 1,202 | 225 | 1 absent held |
| Gin | 8,544 | 8,544 | 7 | 7 | 1 absent, 1 present held |

Source review of changed edges confirmed these representative corrections:

- Vite's external `prompts.text` calls no longer reach `ErrorOverlay.text`;
  Rollup's `PluginContext.emitFile` no longer borrows Vite's internal class.
  The factory control retains `runBenchmark → ModuleRunner.import`, and
  `server.close` now reaches `ViteDevServer.close` instead of the WebSocket type.
- Vitest's `skipCli` is a Set, so its `has` call no longer reaches
  `MockerRegistry.has`. Calls such as `teamMembers.map` and `assert.equal`
  no longer target the imported container variable itself. Typed locator and
  package-installer method calls are retained or newly connected.
- Svelte's array `push` calls no longer reach `Renderer.push`. Calls on the
  concrete `BranchManager` and typed `Renderer` parameters remain connected.
- All seven changed Gin edges are composite-literal `BindBody` calls. They
  now target the explicitly constructed JSON, MessagePack, TOML, XML or YAML binding
  instead of `bsonBinding`.

This is a scored truth set plus an edge-level sample review, not proof that
every removed edge was wrong or every added edge is correct. Unknown receiver
types deliberately lose guessed links; broader recall remains a limitation.

## Static package-entry scope

Package exports must match literal bundle outputs from a single recognized
Rollup/Rolldown config and a direct package-local build script. The reader
follows constant objects, spreads, named inputs, output paths and source
barrels without executing the config. Dynamic alternatives, conflicting
outputs, detected mutations, wildcard exports and preserved-module layouts
are declined. Arbitrary plugin transformations are not interpreted.

The package-script working-directory assumption follows the
[npm script contract](https://docs.npmjs.com/cli/using-npm/scripts/).
Named bundle entries follow the bundler's
[output options](https://rolldown.rs/reference/Interface.OutputOptions).

## Code checks and review

The Linux build and TypeScript check passed. The full suite passed 282 files,
4,829 tests with 12 skips; after deleting one unreachable condition, the
25 targeted receiver/package-entry tests and TypeScript check passed again.
The native kernel was rebuilt for the extraction changes.

The golden update is one metadata row: `Widget.create` retains the same target,
but reports `instance-method` at 0.9 rather than `qualified-name` at 0.85.
All other golden rows are unchanged.

Standards review found no remaining documented violation. Spec review found no
remaining definite defect in the focused changes after owner-identity,
shadowing, type-shape and static-config mutation fixes. The broader §2.3 scope
is explicitly partial in the binding-model plan.

## Agent comparison

Fresh before/after measurement: three pinned corpora, three questions each,
two repetitions per question and build (36 runs). Each build used its own
CLI-created index and prewarmed daemon. All arms ran Sonnet with high effort,
with the CodeGraph CLI-blocking hook. Repeat one ran before then after;
repeat two reversed the order. No Windows build or test overlapped the campaign.
All 36 runs succeeded, with zero reported missing-tool races or successful CLI
bypasses. Bash and Grep/Glob tool calls were zero throughout.

Each cell is the median across six runs, with the full range in brackets.
Questions are pooled here; the [per-run data](receiver-bindings-2026-09-12.json)
retain the exact prompts, occupancy, sufficiency, allocation, corpus cases and
probe records.

| Corpus/build | Seconds | Tools | Read | CodeGraph | Cost USD | Tokens processed |
|---|---|---|---|---|---|---|
| flask/before | 21.7 [13.8–39.3] | 2.0 [1.0–4.0] | 0.0 [0.0–3.0] | 2.0 [1.0–3.0] | 0.118 [0.057–0.135] | 106279 [64423–150962] |
| flask/after | 26.6 [17.9–35.5] | 3.0 [1.0–5.0] | 1.0 [0.0–4.0] | 1.0 [1.0–3.0] | 0.097 [0.060–0.137] | 120881 [64283–171903] |
| gin/before | 14.9 [9.7–20.4] | 1.0 [1.0–3.0] | 0.0 [0.0–1.0] | 1.0 [1.0–3.0] | 0.078 [0.045–0.112] | 64878 [63202–146440] |
| gin/after | 14.5 [10.1–21.6] | 1.5 [1.0–3.0] | 0.0 [0.0–1.0] | 1.5 [1.0–3.0] | 0.082 [0.053–0.142] | 84317 [63194–150004] |
| vite/before | 25.0 [22.3–36.3] | 2.5 [2.0–3.0] | 0.5 [0.0–2.0] | 2.0 [1.0–3.0] | 0.140 [0.108–0.155] | 136507 [113335–165915] |
| vite/after | 24.7 [20.0–29.4] | 3.0 [2.0–3.0] | 1.0 [0.0–2.0] | 2.0 [1.0–3.0] | 0.157 [0.110–0.197] | 161174 [113665–182042] |

The candidate does **not demonstrate a general speedup or satisfy the broader
zero-read target**. Vite's median time is nearly unchanged, with higher reads,
cost and tokens; its ranges overlap the before arm. Flask has more reads and a
higher median time, despite identical graph records and byte-identical outputs
for all three fixed explore probes. That is observed agent variation without
a demonstrated change in returned context, not a claim that the measured
regression disappears. No additional repetitions were selected to obtain a
favorable result.

Zero-Read runs before → after: Flask 5/6 → 3/6, Gin 5/6 → 5/6, Vite 3/6 → 2/6.
Every recorded read transition followed already-returned source; none was a
missing-source read. The sufficiency gap is therefore still visible. Median
final context tokens: Flask 41,494 → 36,926; Gin 36,206 → 38,642;
Vite 49,025 → 50,451. Citation-based allocation remains a relative metric.

Twenty deterministic explore probes (three questions per corpus plus the Vite
factory control, both builds) all returned success. All Flask outputs, two Gin
outputs and Vite's server-creation output were byte-identical. The remaining
outputs changed with the corrected graph. Single-call timings ranged from
35.8 to 118.2 ms; these are one-shot probes, not a latency benchmark.

Spot review of the first candidate answer for each question found the expected
central paths: Flask dispatch, exception finalization and URL building; Gin
handler dispatch, JSON rendering and binding; Vite server/config creation and
module transforms. This does not certify every prose claim: for example,
Flask's class-based-view explanation misnames one class, and Gin's dispatch
answer overgeneralizes its redirect fallback. Those are answer-quality limits,
not newly proven graph defects.

Raw local builds, indexes, probes and transcripts are under
`/tmp/cg-receiver-ab-final`; the committed JSON retains the reported measurements.

## Windows validation

Candidate `d3c330db` passed on a Windows-local checkout with Node 24.21.0.
The native kernel and full application build succeeded. Six targeted suites
(receiver bindings, package-source entries, general resolution, Expo, chained
receivers and golden dumps) passed all 259 tests. The golden dumps matched
the reviewed Linux artifacts. Dependencies were reused because the package
manifest and lockfile were unchanged from the checkout's prior validated
revision. No Windows full-suite or macOS result is claimed for this change.
