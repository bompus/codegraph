# Truth set and score for C++ `begin` same-name ties (2026-09-14)

Corpus: nlohmann/json at `aa391dc0` (the pinned Phase 3 commit).
Engine: `fork/consolidated` at `7a53807d` (extraction 36, schema v12) for the
baseline; the call-site-form fix scored at `t3code/cpp-begin-ties` on top of
`6e320a0b`.
Runner: `EVAL_REPOS=<clone> npm run eval:precision -- json`.
Cases: `__tests__/evaluation/edge-cases.ts`, source `cpp-begin-truth-set`.

## Result

**Baseline (pre-fix, `7a53807d`):** 11/11 held (5 absent, 6 present),
histogram 22,817 edges (exact-match 7,146, fuzzy 5). 105 `calls`→`begin`
edges, all `exact-match`; 93 of them wrong.

**After the call-site-form fix:** 17/17 held (11 absent, 6 present).
Histogram 22,587 edges (exact-match 6,916, fuzzy 5): −230 exact-match
edges. `calls`→`begin` is 12, and every remaining edge is one of the
correct implicit-this members (the 6 encoded present controls, the
`json.hpp` `all_of(begin(), end(), …)` probe, and the same five shapes
in the vendored ABI header). The 93 wrong-present edges are gone.

## Fix (call-site form at exact-name)

The extractor emits `this->begin()` and `a.b->begin()` as a bare `begin`
ref (column at the call expression, not the name). Exact-name used to
pick among every same-named `begin` by same-file / proximity / exported.
The matcher now reads the first `begin(` on or after that column:

- For implicit-this `begin()`, keep the existing same-file exact-name pick
  (the present controls).
- For `this->begin()`, accept only a method/function whose qualified name is the
  enclosing type's `begin`; otherwise nothing.
- For any other `.begin()` / `->begin()`, or `std::begin`, accept nothing (the
  callee is a different type or external).
- For ADL `begin(x)`, accept only a unique same-file free `function`; two
  overloads in one amalgamated header are still a guess.

Scoped to the iterator ADL names (`begin`/`end`/`rbegin`/`rend`/
`cbegin`/`cend`). Applying the same decline to every C++ identifier
dropped thousands of unrelated exact-match edges that this set did not
review.

## What was mined

All 105 `calls` edges targeting a node named `begin` (17 such nodes),
plus `array` / `iteration_proxy` controls. Every one of the 105 is
`exact-match`: the tie-break among same-named `begin`s is a pure
exact-match guess. Call sites were read at the pinned commit; verdicts
below name the read sites, and pattern counts come from the edge dump.

By target: test-helper free `begin` in `unit-user_defined_input.cpp` 49,
`basic_json::begin` recorded as free functions in `json.hpp` 24 (9 in
`json.hpp` itself, 15 from `ordered_map.hpp`), the vendored ABI copy's
own `begin` 23, `iteration_proxy::begin` 6, `Dictionary::begin` 3.

## Correct today (encoded as present controls)

- `FuzzerDictionary.h`: `ContainsWord` → `begin`, `end` → `begin`.
  Same-class member calls (`std::any_of(begin(), end(), …)`,
  `return begin() + Size`). Correct.
- `json.hpp`: `front`, `rend`, `operator[]`, `emplace` → `begin`.
  Member calls on `this` (`*begin()`, `reverse_iterator(begin())`,
  `set_parents(begin() + …)`, `auto it = begin()`). The target nodes
  are the `basic_json` members (the walker records them with kind
  `function`, unqualified). Correct entity.
- ABI copy: `items()` → `iteration_proxy` constructor (2 edges,
  `return iteration_proxy<iterator>(*this)`). Explicit construction.
  Correct. The same shape in the main header has no `calls` edge
  to the constructor (only `instantiates` edges to the class; the 2
  lost `calls` edges from the Phase 3 gate). Not encoded (the gate
  needs the main-header `calls` edge to exist first).

## Wrong at baseline (now absent)

These 93 edges were documented before the call-site-form fix so an
`absent` case would not fail the gate. Representative shapes are now
encoded as absent (destroy, ordered_map `at`, algorithms ADL, to_json
`using std::begin`, doctest `run`, ABI amalgamated `construct`). The
full 93 are gone on the post-fix index (`calls`→`begin` 105 → 12).

- Cross-TU test calls → helper free `begin` (49). All 49 attach to
  file nodes in a different TU than the target. Shapes sampled:
  bare `begin(expected)` on a `const json`
  (`unit-algorithms.cpp:237`, true callee external `std::begin`);
  member `.begin()` on a json value (`unit-diagnostics.cpp:84`,
  true callee `basic_json::begin`); member `.begin()` on a
  `std::vector` (`unit-bjdata.cpp:219`, `br.bjd_…_markers.begin()`,
  external). By source file: class_iterator 22,
  class_const_iterator 22, bjdata 2, algorithms 2, diagnostics 1.
- `using std::begin` / explicit `std::begin` / SFINAE-probe sites →
  `iteration_proxy::begin` (6, all read): `to_json.hpp` construct at
  192 (generic `CompatibleArrayType` with `using std::begin`), 220
  (`std::begin(arr)` explicit on a valarray), 277 (generic
  `CompatibleObjectType`); `input_adapters.hpp` 680/684
  (`begin(declval<ContainerType>())` / `begin(forward<…>)`);
  `json_pointer.hpp:101` (`ptr.reference_tokens.begin()`, a vector
  member). No site touches an `iteration_proxy`.
- `std::vector`-member sites → `json.hpp begin` (4, all read):
  `destroy:630`, `erase:2723`, `insert_iterator:3371/3373`
  (`m_data.m_value.array->begin()`). True callee external.
- `ordered_map` → `json.hpp begin` (15, one read: `at:127`
  `this->begin()`). `ordered_map` derives from `std::vector`, so
  every one of these is an external `std::vector::begin`. The member
  does not exist as a node.
- `doctest.h run` → `Dictionary::begin` (1, read:
  `reporters_currently_used.begin()`, a vector member). Wrong file,
  wrong type.

Inverted site worth keeping: `unit-class_iterator.cpp:217`
`j.begin()` (member on json, correct target in graph) has NO edge,
while line 218 `m_value.array->begin()` (external vector) HAS one to
the other TU's helper. The tie-break is exactly backwards there.

Tie-break asymmetry: in the main header the generic-`begin` sites
land on the `iteration_proxy` member, while the same shapes in the
vendored ABI copy land on that file's `basic_json::begin` (likewise
recorded as a `function` node, not a free function). The break depends
on file context, not just the name.

## Out of scope, checked

- `array`: no `calls` edge targets `nlohmann::array` (`json.hpp:1052`)
  or `value_t::array` today, so the Phase 3 `array` move has no
  present edge to pin; the `#include <array>` import nodes are a
  separate name and were not conflated.
- Zero-incoming member `begin`s (`alt_string_iter`, `vector3`,
  `no_iterator_type`, `fifo_map`, `MyContainer2`, `record_buffer`)
  hold trivially. The encoded absent cases use explicit cross-TU
  from-sites rather than null-from: a null-from case would forbid a
  future correct edge (e.g. `detail::concat` calls `arg.begin()` on
  template args instantiated with `alt_string_iter`), while a bare
  `begin()` in TU-A can never correctly target a test-local member
  in TU-B.

## Limits

- Verdicts are by pattern with named read sites, not all 105 sites
  read. All 23 ABI-copy edges were read line-by-line during review:
  5 correct (`operator[]`, `front`, `rend`, `basic_json`'s
  `std::all_of` probe, one `basic_json::emplace`) and 18 same-shape
  wrong (3 generic `construct`, 2 adapter probes, 1 vector-member
  `operator/=`, 8 `ordered_map` `this->begin()`, `destroy`, `erase`,
  2 `insert_iterator`). Wrong-present total is 75 main-tree + 18 ABI
  = 93, not 75 as first written.
- No agent A/B: this is a deterministic precision set, not a
  retrieval measurement. No macOS run.
