---
name: codegraph-retrieval
description: Use when changing `codegraph_explore` or any MCP tool's output or error shape, retrieval budgets, adding a synthesizer or framework resolver, extending routers or web frameworks, or dynamic-dispatch synthesis.
---

## Retrieval performance & dynamic-dispatch coverage (do not regress)

CodeGraph's core value is letting an agent answer **structural/flow** questions ("how does X reach Y", trace, impact, callers) with a few **fast** codegraph calls and **zero Read/Grep**. The optimization target is **wall-clock latency + tool-call count**, not token cost: the savings come from far fewer turns over a much smaller accumulated context, not from caching. Measure tokens by **summing per-turn assistant usage**, not `result.usage` (last turn only). Evidence: `docs/benchmarks/call-sequence-analysis.md`. **An agent falls back to Read/Grep the instant a codegraph answer is insufficient.** So every change is judged by one question — is codegraph's answer sufficient enough to *stop* the agent from reading?

**Target behavior:** a flow question resolves in **1 codegraph call on small repos, scaling to 3–5 on large**, with **Read/Grep = 0**. When reviewing a PR or trying something new, do not regress this.

### Adapt the tool to the agent — don't try to change the agent

The lever that decides whether a retrieval change lands. **Test before building anything here: does this make a tool the agent _already calls_ do more with the input it _already gives_? If it instead needs the agent to behave differently — pick a different tool, query differently, learn from examples — it hits the low-salience wall and won't land.**

CodeGraph's only channels to influence the agent are low-salience: the MCP `initialize` instructions (`server-instructions.ts`) and the tool descriptions. Changing them does **not** reliably move the agent's tool _choice_ or query style — validated: trace-first steering ported into the server-instructions + tool descriptions (3 wording variants) never reproduced what a CLI `--append-system-prompt` achieved, and **regressed** wall-clock vs baseline. New tools fare worse (rarely chosen — the agent under-picks even `trace`); "better examples" is the same steering. The agent's tool-choice does improve on its own as host models get better at tool use — but that is not ours to force.

What works is meeting the agent where it already is:
- **explore-flow** — `codegraph_explore` is the PRIMARY tool the agent reliably calls; its query is a precise bag of symbol names (incl. qualified `Class.method`) spanning the flow the agent is after; explore finds the call path _among those named symbols_ (riding synthesized edges) and leads its output with it. (`buildFlowFromNamedSymbols`: segment/co-naming disambiguation; ≤1 unnamed bridge so it never wanders a god-function's fan-out. Overload-aware: a PascalCase type token in the query biases an overloaded name to that type's own def — `DataRequest task` → DataRequest's `task`, not the abstract base; named-symbol files sort first.)
- **Sufficiency** — make `codegraph_explore` complete enough that the agent stops. It returns full bodies plus caller/callee context, and for an ambiguous name returns every overload's body in one call (validated on Alamofire/gin).
- **Errors teach abandonment** — one or two `isError: true` responses early in a session and the agent stops calling codegraph entirely (maintainer-observed, repeatedly). `isError` is reserved for genuine "stop trying" cases: security refusals (`PathRefusalError`) and real malfunctions (which carry a retry-once note). Every expected/recoverable condition — project not indexed, symbol not found, file not in the index — returns a **SUCCESS-shaped response carrying the guidance** (`NotIndexedError` → `textResult`, see `ToolHandler.execute`'s catch). The same principle is why the tool surface is **always exposed, even at an un-indexed root** (the old empty-`tools/list` gate was removed in #964 — it broke monorepos where only sub-projects carry a `.codegraph/`, and hid the tools from a session that started before `codegraph init`): safety comes from the response SHAPE (success-shaped guidance, never `isError`), not from hiding tools. An un-indexed root's `initialize` sends a per-project variant (`SERVER_INSTRUCTIONS_NO_ROOT_INDEX` — "pass `projectPath` to a project that has a `.codegraph/`"), not an "inactive" note; indexing is still deliberately the user's call, never the agent's.

What fails is the inverse — folding a precise answer into a **fuzzy-input** tool: a tool that takes a description instead of symbols cannot disambiguate a flow's endpoints and surfaces the _wrong feature_. Precise output needs precise input — explore takes a symbol bag for exactly this reason, and a separate flow tool is redundant with explore-flow.

The remaining lever under this axis is **coverage**: every flow made to connect statically (a new dynamic-dispatch synthesizer, or extracting symbols static parsing skipped — e.g. object-literal store actions in `create((set,get)=>({...}))`) is then surfaced automatically by explore-flow, no agent change needed. Reactive/reconciler runtimes (Halo's `ReactiveExtensionClient`, MediatR, Vue Proxy) are the frontier — flows there have no static edges, so nothing surfaces (correctly — silent beats wrong). Full investigation + A/B record: `docs/benchmarks/call-sequence-analysis.md`.

### Explore budgets

`getExploreBudget` and `getExploreOutputBudget` in `src/mcp/tools.ts` are the source of truth for the size tiers; `__tests__/explore-output-budget.test.ts` pins them. Keep both budgets monotonic with indexed file count: a larger tier must never receive fewer calls or a smaller `maxCharsPerFile` than a smaller tier. A regression here truncates large files and forces agents back to Read.

- Explore output must **never tell the agent to "use Read"** — steer to another `codegraph_explore` and "treat returned source as already Read."

### Dynamic-dispatch coverage — the flow must EXIST in the graph end-to-end

Static tree-sitter extraction misses computed/indirect calls, so flows break at dynamic dispatch and the agent reads to reconstruct them. Synthesizers/resolvers bridge these so `codegraph_explore` connects them end-to-end (`src/resolution/callback-synthesizer.ts`, `src/resolution/frameworks/`). Channels today: callback/observer, EventEmitter, **React re-render** (`setState`→`render`), **JSX child** (`render`→child component), **React Native native→JS events** (`sendEvent(withName:)` / JVM `emit` → the `addListener` handler, named or inline, `rn-event-channel`), django ORM descriptor. The JS→native direction is a *resolver* (`frameworks/react-native.ts`: `RCT_EXPORT_METHOD`, `RCT_EXTERN_MODULE` Swift shims, TurboModules), which trusts receiver evidence — an alias bound to `NativeModules.X` — over the import resolver. All synthesized edges are `provenance:'heuristic'` with `metadata.synthesizedBy` + `registeredAt` (the wiring site), surfaced inline in `codegraph_explore`'s Flow section.

**Principle: partial coverage is WORSE than none.** Bridging one boundary but not the next reveals a hop the agent then drills + reads to finish. Measured on excalidraw: react-render alone *raised* reads to 5–7; only completing the flow (adding the jsx-child hop) dropped it to 0–1. **Always close the flow end-to-end and re-measure** — never ship a half-bridged flow.


- **Before adding or extending a router, a web framework, or a language's `WHEN` rules, read `docs/design/framework-coverage.md`.** It is the standing answer to "what is supported and what is left" across the three axes (route nodes → Entry points, `navigates` edges → Screens, branch-guard rules → the `WHEN` labels), with what each remaining item needs, the traps that have already cost debugging time, and the queries to re-verify it. Update it in the same change that moves a row.
