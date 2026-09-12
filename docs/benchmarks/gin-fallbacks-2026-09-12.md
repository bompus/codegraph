# Gin rendering and binding fallback investigation

Candidate `ed5033cb73599f148fe953d8c70a4daaf2207796` versus
`3a53c0dcfb3a358366ef2f0f284594ed251c082b`, measured on 2026-09-12.

## Reproduced gaps

The [earlier fallback comparison](explore-fallbacks-2026-09-12.md) recorded
reads of `render/json.go` and shell searches for `binding.JSON`. The
[constants follow-up](explore-constants-2026-09-12.md) reproduced the JSON reads
in its baseline arm. Exact-query replay separates two causes:

- `Context JSON render write response` names lowercase `render`, whereas the
  exported Go method is `Render`. Exact-name seeding misses it, and longer
  sibling renderers can occupy the source budget. Recovery now considers a
  capitalized method only when its receiver type is explicitly named. Exact
  receiver owners also precede filename-only matches. Recovered methods use
  the existing bounded same-file neighbor expansion, so `JSON.Render` brings
  its `WriteJSON` callee with it.
- `binding.JSON variable declaration jsonBinding{}` cannot find the declaration
  because grouped Go variables have a `var_spec_list` wrapper. The native
  walker previously expected direct specs. Symbol extraction, package binding
  collection and local shadow binding collection now share the wrapper-aware
  traversal. The index now contains both build-tag variants of `binding.JSON`.

| Recorded query | Requested range | Before | After |
|---|---|---:|---:|
| JSON rendering | `render/json.go:57–75` | 0/19 lines | 19/19 lines |
| Binding declaration | `binding/binding.go:77` | 0/1 line | 1/1 line |

Responses are 15,922 and 13,982 characters respectively, below the 25,000
character ceiling. The change uses the existing tools and input shapes.

Extraction version advances from 35 to 36. Existing Go indexes need a
re-index to include grouped variables; this is not a schema migration.

## Checks and control evidence

- Native kernel and application builds pass on Linux and Windows.
- Linux passes 4,873 tests in 285 files, with 12 skips. Windows passes 38
  targeted tests, including Go bindings, golden dumps and retrieval guards.
- The grouped-variable test fails against the old native kernel and passes
  with the fix. It checks package export/storage and local shadow bindings.
  A rendering fixture covers receiver disambiguation and callee source; it
  also passes on the baseline, so the recorded real-query replay supplies the
  before/after omission proof for rendering.
- All eight golden corpora pass. The payroll fixture adds grouped package
  variables and a local shadow; its dump adds four nodes, their declaration
  and containment records, and local binding.
- Gin's pinned precision gate retains its one absent and two present cases.
  The edge comparison loses zero existing edges and gains 191: 76 containment,
  57 references, 53 instantiations and five calls. These follow newly extracted
  declarations and their initializers. The report preserves the full edge
  delta; the three scored cases are not a universal precision proof.
- Three Flask and five Vite deterministic control responses are byte-identical.
  Gin control allocation changes with the newly visible declarations. An
  intermediate expansion of every named seed trimmed Vite ranges; narrowing
  it to recovered receiver methods restored the identical controls before A/B.
- No macOS native validation was added.

## Agent comparison

A fresh 16-run Sonnet/high comparison covers Gin JSON rendering and JSON
binding, plus Flask dispatch and Vite server creation as controls. Each question
runs twice per build; the second repetition reverses arm order. Each build
owns its corpus clones, index and prewarmed daemon. The CLI-contamination guard
is enabled. No heavy builds or tests run during this timing window.

All sixteen runs complete successfully with no CLI contamination or tool races.
Both arms use zero Read, Grep or Bash fallback calls in this fresh sample. This
supports stability; it does not reproduce the older fallback frequency. The
exact recorded queries above supply the omitted-source before/after proof.
Gin's candidate uses five explore calls across four runs, versus six before.

Counts are totals; time is median (range). Gin has four runs per arm, the other
corpora two each.

| Corpus | Build | Seconds | Tools | Read | Grep | Bash | Explore |
|---|---|---:|---:|---:|---:|---:|---:|

| flask | before | 21.9 (21.1–22.6) | 4 | 0 | 0 | 0 | 4 |
| flask | after | 20.1 (19.5–20.6) | 4 | 0 | 0 | 0 | 4 |
| gin | before | 14.6 (10.6–18.6) | 6 | 0 | 0 | 0 | 6 |
| gin | after | 13.0 (11.7–15.8) | 5 | 0 | 0 | 0 | 5 |
| vite | before | 23.3 (23.0–23.7) | 4 | 0 | 0 | 0 | 4 |
| vite | after | 22.5 (21.2–23.8) | 4 | 0 | 0 | 0 | 4 |

### Feedback metrics

Residual file-access tokens and allocation efficiency are per-run medians.
Next-action counts are totals in the order
`explore again / read returned / read missed / search / sufficient`.
Allocation efficiency is citation-based and comparable for the same questions.

| Corpus | Build | Residual tokens | Next actions | Allocation efficiency |
|---|---|---:|---|---:|
| flask | before | 0.0 | 2 / 0 / 0 / 0 / 2 | 0.813 |
| flask | after | 0.0 | 2 / 0 / 0 / 0 / 2 | 0.837 |
| gin | before | 0.0 | 2 / 0 / 0 / 0 / 4 | 0.826 |
| gin | after | 0.0 | 1 / 0 / 0 / 0 / 4 | 0.993 |
| vite | before | 0.0 | 2 / 0 / 0 / 0 / 2 | 0.839 |
| vite | after | 0.0 | 2 / 0 / 0 / 0 / 2 | 0.822 |

[Machine-readable evidence](gin-fallbacks-2026-09-12.json) includes every run,
exact-query coverage, deterministic probes, precision result and full Gin edge
delta. The two recorded Gin gaps are addressed; broader zero-read coverage and
source-free inference remain open. These short runs do not establish a general
speedup or universal fallback-free behavior.
