<div align="center">

# CodeGraph

**bompus/codegraph** · a fork of [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph) · [how it differs](#about-this-fork)

Follow [@getcodegraph](https://x.com/getcodegraph) on X for updates.

### Supercharge Claude Code, Cursor, Codex, OpenCode, Hermes Agent, Gemini, Antigravity, Kiro, GitHub Copilot, and Devin with Semantic Code Intelligence

**The fastest complete code graph · surgical context · built for how agents actually work · 100% local**

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/rust-logo-dark.svg?v=1">
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/rust-logo.svg?v=1" height="30" alt="Rust" align="center">
</picture>&nbsp; **Kernel powered by Rust**

### [Documentation & Website →](https://colbymchenry.github.io/codegraph/)

[![npm version](https://img.shields.io/npm/v/@colbymchenry/codegraph.svg)](https://www.npmjs.com/package/@colbymchenry/codegraph)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Self-contained](https://img.shields.io/badge/Node.js-bundled%20%C2%B7%20none%20required-brightgreen.svg)](https://nodejs.org/)
[![npm provenance](https://img.shields.io/badge/npm-provenance-brightgreen.svg)](#verified-releases)
[![Attested builds](https://img.shields.io/badge/releases-signed%20%26%20attested-brightgreen.svg)](#verified-releases)

[![Windows](https://img.shields.io/badge/Windows-supported-blue.svg)](#supported-platforms)
[![macOS](https://img.shields.io/badge/macOS-supported-blue.svg)](#supported-platforms)
[![Linux](https://img.shields.io/badge/Linux-supported-blue.svg)](#supported-platforms)

[![Claude Code](https://img.shields.io/badge/Claude_Code-supported-blueviolet.svg)](#supported-agents)
[![Cursor](https://img.shields.io/badge/Cursor-supported-blueviolet.svg)](#supported-agents)
[![Codex](https://img.shields.io/badge/Codex-supported-blueviolet.svg)](#supported-agents)
[![opencode](https://img.shields.io/badge/opencode-supported-blueviolet.svg)](#supported-agents)
[![Hermes Agent](https://img.shields.io/badge/Hermes_Agent-supported-blueviolet.svg)](#supported-agents)
[![Gemini](https://img.shields.io/badge/Gemini-supported-blueviolet.svg)](#supported-agents)
[![Antigravity](https://img.shields.io/badge/Antigravity-supported-blueviolet.svg)](#supported-agents)
[![Kiro](https://img.shields.io/badge/Kiro-supported-blueviolet.svg)](#supported-agents)
[![GitHub Copilot](https://img.shields.io/badge/GitHub_Copilot-supported-blueviolet.svg)](#supported-agents)
[![Devin](https://img.shields.io/badge/Devin-supported-blueviolet.svg)](#supported-agents)

<br>

**The CodeGraph platform is coming** — for every PR, know exactly what to test, what could break, which flows are affected, and whether business logic is compromised.

<a href="https://getcodegraph.com"><img alt="Join the waitlist for early beta access" src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/waitlist.svg?v=2" height="52"></a>

<sub>Get <b>early beta access</b> to the hosted product · <a href="https://getcodegraph.com">getcodegraph.com</a></sub>

</div>

---

## About this fork

This is **bompus/codegraph**, a fork of [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph). Its default branch, `fork/consolidated`, contains upstream `main` through [`1d3619d6`](https://github.com/colbymchenry/codegraph/commit/1d3619d6) (after v1.6.2) plus the fork's own work, and it takes upstream changes as they land. Changes that suit upstream are also offered there as pull requests.

The fork publishes no releases. The install scripts, npm package, badges and `codegraph upgrade` further down this page install **upstream's** releases. To run the fork, build it from source (below).

### Install the fork from source

You need Node.js 22.13 or newer (or Bun 1.4.0 or newer), git, and a [Rust toolchain](https://rustup.rs/) 1.89 or newer for the native kernel.

```bash
git clone https://github.com/bompus/codegraph.git
cd codegraph
npm ci
npm run build:kernel   # compiles the native parser and resolver
npm run build
npm link               # puts `codegraph` on your PATH
codegraph install      # wires CodeGraph into your agents
```

Later-exported TypeScript and JavaScript store actions are read from AST export facts, including files with LF, CRLF or CR line endings.

Then run `codegraph init` in each project, as in [Get Started](#get-started). Indexes built by upstream releases should be rebuilt (`codegraph index --force`), because the fork writes tables and node kinds that upstream does not.

T3-hosted provider transcripts can appear in session search; CodeGraph does not read the T3 database. A [pure offline Codex metadata prototype](docs/design/t3-session-metadata.md) associates caller-supplied app titles and thread links with existing hits. It has no CLI or MCP caller and adds no searchable prose.

Session search uses `.codegraph/sessions-v2.db`. The first session search imports remembered roots from `sessions.db` once and reindexes available transcripts. It leaves the older file untouched for older executables; roots an older executable records after that stay in the older file. Session mentions in code answers resume after the new store is populated. Cached passages from transcripts that were unavailable when the new store was first built remain only in the older store. Retaining both files uses extra disk space. The graph index and project-local scope stay unchanged.

### What the fork adds

Compared with upstream `main` at `1d3619d6` (after v1.6.2). Each item here and in the dispatch and framework lists below was checked against upstream's tree at that commit.

| Feature | Upstream | Fork | What it does |
|---|:-:|:-:|---|
| Session search (`codegraph sessions`, `codegraph_sessions`) | — | ✓ | Searches this project's earlier Claude Code, Codex, Cursor/T3, OpenCode, AGY, Devin and Grok transcripts and its git commit messages, so an agent can find what a past session decided. `codegraph_explore` also names the sessions that mentioned the symbols it returns. `"sessions": false` in `codegraph.json` turns it off. |
| Markdown indexing | — | ✓ | Headings, sections, tables and links become graph nodes; a documentation question gets the matching section. Explicit code-file pins remain eligible in mixed documentation/code requests. |
| Missing names reported | — | ✓ | After a complete scan of indexed source without skipped files, `codegraph_explore` lists requested names it cannot find, helping an agent catch a guessed name. |
| Requested-source limits reported | — | ✓ | Explore reports requested files that did not receive pin priority within the file limit, unexamined path-like references beyond its bounded scan, and uncovered indexed continuation ranges after a capped file gather. Existing output limits still apply. |
| Local callee source with file pins | — | ✓ | When a query names functions in pinned files, `codegraph_explore` prioritizes local callee bodies through two call or callback hops, within the file and output budgets. |
| Quoted prose source | — | ✓ | Quoted spans of three or more words find script strings and template text, ignoring case and punctuation, through a capped source scan. No re-index needed. |
| Requested test source | — | ✓ | Pinned JavaScript and TypeScript test files return matching `it`/`test` callbacks, including `.each` parameterized tests. Questions about tests include source from the nearest test callers of named symbols, within the file and output budgets. |
| Near-duplicate functions | — | ✓ | `codegraph_explore` and `codegraph_node` name the near-identical copies of a function, so a fix made in one copy is not forgotten in the others. |
| External HTTP endpoints | — | ✓ | JavaScript and TypeScript calls through `fetch`, axios, ky, got and similar clients appear as endpoint nodes such as `GET https://api.github.com/…`. |
| Change questions | — | ✓ | "What did my changes touch?" or `main..HEAD` is answered from the diff: the changed functions, their callers and their tests. |
| Name-only links marked | — | ✓ | Call links matched only by a function's name, with no import or receiver type behind them, are marked, so an agent knows which hop to check. |
| Worktree seeding | — | ✓ | `codegraph init` in a new git worktree starts from a sibling worktree's index and re-reads only the files that differ. |
| Inferred links kept current on sync | Rebuilt inside the sync | Rebuilt once edits pause | Links inferred from events, callbacks, React re-renders, function pointers and similar dispatch are rebuilt after a sync, so the graph ends where a full index would. Upstream's refresh ([#2033](https://github.com/colbymchenry/codegraph/pull/2033)) runs inside each sync that touches an inferred link. The fork runs the same full refresh after edits pause, so a save is indexed without waiting the 3–5 s synthesis takes on a large repository; until the refresh runs, an inferred link can be missing, or stale when its registration moved to another file. |
| Devin | — | ✓ | `codegraph install` can wire up Devin (CLI and Desktop). |
| Reloading MCP launcher | — | ✓ (opt-in) | A long-running MCP server picks up a new build without the agent reconnecting. |
| Build revision in version output | — | ✓ (stamped builds) | `codegraph --version` and `status --json` report the source revision when the build carries `dist/build-revision.json`. A deploy step writes that file; `npm run build` does not, so a plain source build reports the package version. |

### Parsing, resolution and languages

| | Upstream | Fork |
|---|---|---|
| Parser | Native kernel for 20 languages, with a WebAssembly fallback for the rest and for files the kernel cannot parse | Native kernel only: every language is parsed in Rust. The WebAssembly parser is removed (a platform without a prebuilt kernel needs a Rust toolchain) |
| Files with syntax errors | Handed to the fallback parser | Extracted from the native parser's error recovery |
| Name resolution | In TypeScript, by import tracing and name matching over the source text | In the native kernel for every language, reading what each file actually binds (declarations, parameters, imports) for TypeScript/JavaScript, ArkTS, Python, Go, Java, Kotlin, PHP, C, C++ and Rust |
| Markdown (`.md`, `.mdx`) | — | Indexed |
| Node.js 25 and newer, Bun | Refused | Allowed from Node.js 22.13 and Bun 1.4.0, the first releases with an unflagged `node:sqlite`; Node 26.10.0 and Bun 1.4.2 passed the full suite at `48903f5` ([Measured results](#measured-results)) |

The other languages are the same in both, listed under [Supported Languages](#supported-languages).

C# property accessors and expression-bodied properties contribute calls and references owned by the property. VB.NET member bodies and field initializers also contribute their calls and references.

C# field and property initializers retain their calls and references under the member that owns them. Target-typed `new()` resolves relative declared types through enclosing namespaces and honors `global::` qualification without requiring a redundant `using`. VB.NET resolves typed receivers, enclosing and inherited members, and field or property reads through values and Shared types without choosing unrelated project declarations. Names beginning with keywords, such as `SharedCache`, `Dimension` and `NewItem`, retain their declarations.

Go declaration comments attach to the declared type, and defined types retain references to their named component types. Assertion calls follow the asserted project type and each declared return in a method chain. Local bindings shadow package imports. C/C++ types defined beside variables retain their members and comments; nested template arguments preserve receiver ownership, and leading `::` names require a visible global declaration. C++ declaration attributes and macros preserve source positions. Multiplication operands remain intact, and anonymous pointer declarations share their first declared type name. Macro-opened namespaces combine with ordinary nested scopes when resolving included owners. Renamed and default imports are retried when their module appears during sync. Named-only components are not default exports; explicit forwarded defaults retain their target. Whitespace before a type argument does not create a JSX rendering edge. React JSX respects component imports and local shadowing; lazy routes follow component exports through local barrels. Named flow paths prefer an equally long path containing more requested symbols.

The native resolver includes upstream's Go import-name assumptions, type-position checks, alias method forwarding and embedded interface method sets. Chained Go calls follow declared return types at each fluent step; an unproven receiver stays unresolved instead of borrowing an unrelated method by name. C++ bases use enclosing-scope directives, parent-scope lookup and translation-unit visibility; included test-named headers remain eligible, while declared external library receivers avoid unrelated project methods. Incremental sync retries newly available Liquid targets, lazy route modules and navigation destinations. Navigation in an imported route-table file also survives removing and restoring its destination. Route-table nodes and their references commit together, so a failed reference write can be retried without leaving an incomplete route. React layout links exclude Vue and Angular routes. C++ receiver declaration scans exclude multiline raw-string contents.

Go interfaces link named embedded interfaces and aliases that resolve to interfaces. Named scalar or struct terms, unions, underlying-type constraints and basic types are not supertypes. Package-qualified embeddings stay in their imported package. Go imports follow the nearest indexed module and the longest matching module path, including module changes during incremental sync. Unexported receivers and embedded methods stay in their declaring package. Dart imports, exports and part directives follow their library URIs and visibility rules. Library directive reads stay inside the indexed project, including resolved symlink targets. Calls through import prefixes, annotations and member chains follow the visible declaration and written receiver types, including explicit generic lookup types. Parameters and locals shadow bare calls. Top-level and field initializers contribute calls; const constructors, redirecting factories and annotated members retain their declarations and dartdoc. Getter reads become calls only when the receiver type reaches that getter and no nearer field in the visible class hierarchy overrides it; enum extensions and type-position references participate in resolution. Rust enum-variant values retain their enum references.

Named JavaScript and TypeScript object literals own their function members, including local objects and classic-script global assignments. Member calls follow the visible object; loop-local objects stay within their scope. Destructured member calls resolve across lines while preserving the source binding at the destructure declaration; unrelated bare names stay unresolved. Vue template expressions contribute calls to script bindings while preserving component ownership. Encoded attribute expressions retain their original source positions, and template-local bindings stay within their scope.

A COBOL copybook named in an explore query prioritizes its indexed source and lists its COPY and EXEC SQL INCLUDE sites. Missing indexed source is reported explicitly.

`codegraph status` reports files that need re-indexing and files with recorded parse errors. `status --json` includes `index.filesNeedingReindex` and `index.filesWithParseErrors`; `files --json` includes each file's extraction errors. A transient parser failure preserves the previous graph and retries on the next sync. When a file named by an unresolved import appears later, incremental sync retries that import without selecting unrelated namesakes.

The MCP launcher can replace a daemon from an older release when its hello confirms coordinated writer handover. `serve --mcp --path <root> --preserve-existing` opts out of replacement on initial connection and reconnect, requires an index at that exact root, and keeps fallback reads without a watcher. Adding `--initialize-index` lets the elected daemon create a missing exact-root index after acquiring writer ownership. Legacy daemons stay running while new sessions serve reads without auto-sync; stop the old MCP sessions and daemon, then reconnect with the current install. A daemon exits when its installation is deleted or its package version changes. Different managed builds of the same release retain the fork's version-identity checks.

Dispatch and framework coverage the fork adds, by kind:

**Dispatch links**

| Addition | What it links |
|---|---|
| C function pointers | `x->f = fn;` assignments, alongside table initializers |
| Drupal hooks | `invokeAll()` / `invoke()` / `alter()` call sites to hook implementations, including Drupal 11 `#[Hook]` attributes |
| NgRx effects | Dispatched actions to the effects that handle them |
| React Native `NativeModules[key]` | Computed native-module calls to the native method, when the key is a literal or a same-file literal binding |
| `window.postMessage` | Posted messages to the listeners that check the same literal `source` string or member name |

Call resolution the fork adds:

- Kotlin infix calls keep their call edge when a comment sits inside the expression.
- Kotlin receiver inference follows bounded chains of declared returns and verified receiver-preserving methods. Properties initialized by typed factory calls keep compatible imported extensions. Explicit casts, single-type `when` branches, filtered collection elements and bound generic factory arguments also supply receiver types.
- Kotlin chains on instance and extension receivers use the declared return type, including nested and multiline calls; upstream covers class and companion-factory chains. An imported return-type guess for an unknown receiver keeps confidence at most 0.7, and so does a Rust chain match without a proved receiver type.
- Kotlin multi-dollar strings keep their interpolation threshold.
- C++ typedef and using aliases follow their declaration scope, including nested owners and inherited aliases. Pointer, array and function members are not supertypes. Visible class-scoped aliases and declared complex receivers keep their method owners; unsubstituted template parameters and ambiguous owners stay unresolved.
- C++ `->` calls on a `std::unique_ptr`, `std::shared_ptr` or `std::optional` reach the type it holds, looked up through the caller's enclosing classes and their bases, its namespaces, `using` directives and aliases. A held type the project does not declare leaves the call unresolved, as a plain pointer to one does; upstream guesses a callee from the receiver's name there. A partial class template specialization is indexed as an anonymous class, so a call to a member only it declares stays unresolved.
- C++ namespace aliases and declarations in macro-opened namespaces are looked up only in the caller's include closure; upstream pools them across all files.
- C# namespace `using` directives and `using` aliases apply only inside their enclosing namespace, not to sibling namespaces in the same file (`using static` is still file-wide, as upstream).
- A Java field declared with a qualified type (`outside.Repository`) keeps its qualifier, so calls through it never resolve to an unrelated project class with the same simple name.
- Python `super().method()` resolves along the class's C3 method resolution order, starting after the class itself. Upstream resolves it like `self.method()` and drops the link only when it lands on the calling method.
- Python receivers reassigned on later lines use the latest preceding value in their lexical scope. An unknown replacement leaves the member call unresolved. Calls and method values through a captured receiver stay unlinked when a later replacement makes its invocation-time value uncertain. Same-line replacements also stay unlinked when statement order cannot establish a reliable receiver type. Unaliased imports that add submodules to the same package preserve that package receiver.
- A `require` call creates no file dependency when a local binding shadows `require`.

**Server endpoints**

| Addition | What it links |
|---|---|
| HTTP routes | Routes in Hono, Elysia, Fastify, Koa router, H3, Hyper-Express, Bun, Effect v4 and Vixeny, read from the router object a call is made on rather than the names `app` and `router`, with composed prefixes (Hono `basePath`, Koa `prefix`, Elysia `group`, Fastify `register`); Fastify plugin files with `@fastify/autoload` directory prefixes; Nuxt `server/routes/` and method suffixes. Upstream matches calls on `app` and `router` by name, composes Express `X.use('/prefix', router)` mounts across files, and reads Nuxt `server/api/` |
| Route groups | Group prefixes in route paths for gin, chi, gorilla, actix `web::scope` and GoFrame |
| TanStack Start server routes | `server.handlers` tables in file routes as method-qualified endpoints, linked to named handlers |

**Page routers**

| Addition | What it links |
|---|---|
| Analog | `src/app/pages/**/*.page.ts` file routes, linked to their page component classes |
| Angular Router | On top of upstream's reader: `provideRouter` / `RouterModule` imported under an alias (a `$`-prefixed one included) still register routes, a routes file one hop behind an NgModule's routing module, including a routing module the NgModule imports through a local barrel, sits under its lazy path, and named-`outlet` or `...spread` entries name no screen |
| Astro routes | `<a href>` and `Astro.redirect` navigation between pages, endpoint method aliases such as `export { handler as GET }`, and top-level calls in browser scripts attributed to the file rather than the component (upstream binds a page to its component and directly exported endpoint methods to handlers) |
| Nuxt pages | `(group)` folders dropped from page route paths |
| Qwik City | `src/routes` index pages and `onGet`/`onPost`-style endpoint handlers, linked to their components and handlers |
| React Router framework mode | Pages declared in `app/routes.ts`, linked to each module's default component |
| React Router `getHref` | On top of upstream's reader: a shadowed binding, a factory-created config object, a spread override or a computed path fragment is left unlinked rather than linked to a guessed route |
| RedwoodSDK routes | `defineApp` route trees (`route`, `index`, `render`, `layout`, `prefix`, method tables), linked to their handlers |
| Remix / React Router file routes | The default `app/routes/` file convention (and `flatRoutes()`), linked to each page's default component |
| Solid Router | `<Route>` JSX and route-config arrays, including arrays imported from another file, linked to their (possibly lazy) components |
| SolidStart | `src/routes/` file pages and API endpoints, linked to their components and handlers |
| Vike | `+Page` filesystem routes and `+route` overrides, linked to their page components |
| Waku | `src/pages` filesystem pages and `createPage` calls registered through `createPages`, linked to their page components |

### Measured results

Upstream `main` at `6560052` (v1.6.2) against the fork at `34cc55e`, each run on Node.js 24.21.0 and on Bun 1.4.2. Measured 2026-10-03 on a 16-vCPU WSL2 host over seven corpora, with the arms in mirrored order; index figures are the median of two runs and sync figures the median of four. Upstream refuses to start on Bun, which reports itself as Node 26, because upstream blocks Node 25 and newer. With `CODEGRAPH_ALLOW_UNSAFE_NODE=1` it runs; those numbers, the method and the graph sizes are in [`docs/benchmarks/fork-vs-upstream-node-bun-2026-10-03.md`](docs/benchmarks/fork-vs-upstream-node-bun-2026-10-03.md), which also compares this run with the 2026-09-28 one. The scripts that produced them are in [`scripts/benchmarks/runtime/`](scripts/benchmarks/runtime/).

**Full index** (`codegraph init`), time and peak memory:

| Corpus | Upstream, Node | Fork, Node | Fork, Bun |
|---|---|---|---|
| gin (Go, 119 files) | 1.03 s, 549 MiB | 1.17 s, 479 MiB | 1.11 s, 383 MiB |
| Alamofire (Swift, 129 files) | 1.62 s, 838 MiB | 2.05 s, 664 MiB | 2.08 s, 486 MiB |
| pretix (Python + JS, 1,472 files) | 21.1 s, 3.27 GiB | 10.8 s, 3.70 GiB | 9.41 s, 3.23 GiB |
| CPython (C + Python, 3,708 files) | 94.7 s, 8.34 GiB | 45.5 s, 6.41 GiB | 45.1 s, 5.73 GiB |
| discourse (Ruby + JS, 20,277 files) | 23.7 s, 3.86 GiB | 25.1 s, 2.88 GiB | 22.1 s, 2.44 GiB |
| supabase (React + Next.js + TS, 10,716 files) | 26.7 s, 4.26 GiB | 23.1 s, 3.95 GiB | 23.4 s, 3.65 GiB |
| n8n (Vue + TS, 24,434 files) | 168.9 s, 9.45 GiB | 78.4 s, 6.33 GiB | 79.1 s, 5.76 GiB |

Upstream at `6560052` takes 2.3 to 2.7 times as long to index Python as at `290e03f` (pretix 9.2 s to 21.1 s, CPython 35.6 s to 94.7 s). For pretix, [a same-host check](docs/benchmarks/fork-vs-upstream-node-bun-2026-10-03.md#since-2026-09-28) shows the cause is upstream's code, not the host.

The table predates three fork fixes, each measured on the same host on 2026-10-04 (Node, two runs each). A fix to Python name checks cut CPython's full index from 48.8 s to 39.5 s and its peak memory from 6.45 GiB to 5.90 GiB; pretix stayed at 10.0 s. A bound on the resolver's per-thread cache of name patterns then cut peak memory from 5.87 GiB to 4.25–4.35 GiB on CPython and from 3.54 GiB to 3.0 GiB on pretix, with the same index times and graphs. Bounding each resolver worker's cache of per-file bindings then cut peak memory from 4.23–4.29 GiB to 3.73–3.77 GiB on CPython and from 5.94–5.97 GiB to 5.29–5.48 GiB on n8n, again with the same graphs. The table itself has not been re-run.

**One-file sync** (edit one file, `codegraph sync`):

| Corpus | Upstream, Node | Fork, Node | Fork, Bun |
|---|---|---|---|
| gin | 0.47 s | 0.37 s | 0.32 s |
| Alamofire | 0.69 s | 0.51 s | 0.46 s |
| pretix | 3.01 s | 1.90 s | 1.67 s |
| CPython | 8.30 s | 4.67 s | 4.97 s |
| discourse | 6.03 s | 2.79 s | 2.64 s |
| supabase | 6.30 s | 2.94 s | 2.77 s |
| n8n | 13.2 s | 4.38 s | 4.06 s |

Both builds rebuild the links inferred from events, callbacks and function pointers after a sync, so a sync ends with the graph a full index would build ([colbymchenry/codegraph#1988](https://github.com/colbymchenry/codegraph/issues/1988)). Upstream reruns every inference pass inside each sync that touches an inferred link; the fork reruns the same passes a full index runs once edits pause. `CODEGRAPH_SYNC_RESYNTHESIS=0` turns the fork's rebuild off, trading it for stale inferred links.

**MCP server** on n8n (`codegraph serve --mcp`; memory and CPU summed over the server and the daemon it starts):

| | Upstream, Node | Fork, Node | Fork, Bun |
|---|---|---|---|
| Start to first explore answer | 4.23 s | 2.89 s | 2.66 s |
| Explore, warm median | 707 ms | 227 ms | 223 ms |
| 8 explores at once | 3.62 s | 2.25 s | 2.51 s |
| Edited file re-indexed by the watcher | 1.40 s | 0.87 s | 0.70 s |
| Memory while busy | 4.29 GiB | 3.84 GiB | 3.14 GiB |
| Memory at idle | 4.77 GiB | 2.07 GiB | 1.22 GiB |
| CPU at idle, share of one core | 0.47% | 0.32% | 0.67% |

**CLI startup** on gin (`hyperfine`, mean of 20 runs):

| Command | Upstream, Node | Fork, Node | Fork, Bun |
|---|---|---|---|
| `codegraph --version` | 63 ms | 31 ms | 23 ms |
| `codegraph status` | 173 ms | 117 ms | 86 ms |
| `codegraph explore "<query>"` | 224 ms | 121 ms | 99 ms |

**Node or Bun.** The fork produces the same node and edge counts on both, and the full test suite passed on Node.js 24.21.0, Node.js 26.10.0 and Bun 1.4.2 at `48903f5` (`npm run test:bun`; one test is skipped under Bun for [oven-sh/bun#42891](https://github.com/oven-sh/bun/issues/42891)). Since then the suite has grown: at `2bf24ef4` plus the directory-order fix, `npm run test:bun` fails 19 of 8,780 tests that pass on Node (writer-lock, watcher-startup and grammar-load-failure tests that replace modules or `fs` functions, a test that empties `PATH`, and a V8-only garbage-collection test); none is a known product defect. On Bun, peak index memory is 8–27% lower than on Node, the idle MCP server holds 37–41% less memory, and commands start faster. Index and sync times range from 6% slower to 15% faster than Node's. Bun has two costs. Eight concurrent explores on n8n take 12% longer; contention between worker threads reading through Bun's SQLite is the suspected cause ([oven-sh/bun#44084](https://github.com/oven-sh/bun/issues/44084), [#44187](https://github.com/oven-sh/bun/issues/44187)). Idle CPU is two to four times Node's, though still under 1% of one core.

Measured separately:

| What | Upstream | Fork | Source |
|---|---|---|---|
| New git worktree ready to query | full index: 5.8–6.1 s, 1.6 GB | seeded from a sibling's index: 0.8–0.9 s, 184 MB | svelte, 8,217 files; ledger §5.58 |
| Retained call links that are correct | 48 of 85 (56%) | 52 of 71 (73%) | repowise's 120 graded rows, TypeScript, Python, C# and Kotlin; [precision replay](docs/benchmarks/precision-replay-2026-09.md#re-run-on-upstream-main-and-the-fork-2026-09-28) |

The precision gain comes mostly from declining uncertain links rather than resolving more. On n8n, most of the call links upstream keeps and the fork drops are test globals and library calls bound to unrelated same-named code (`it` to a TypeORM test helper, `path.join` to a query builder's `join`). The fork's before-and-after measurements of its own revisions are in the measurement ledger, [`docs/design/metrics-ledger.md`](docs/design/metrics-ledger.md).

The [benchmark](#benchmark-results) and [speed](#built-for-speed--the-rust-kernel) sections further down are upstream's own measurements of upstream builds; the fork has not re-run them.

### What it costs

- **Larger database:** 11–21% bigger on six of the seven corpora above, and 37% on supabase, which has 1,978 Markdown files. It holds Markdown, binding rows and more nodes.
- **Fewer edges on some projects:** 3–5% fewer on pretix and CPython, because the fork declines links it cannot confirm. Some of those were correct links.
- **Slower full index on small projects:** on Node, gin and Alamofire take 0.14–0.43 s longer to index than on upstream. The cause has not been found.
- **On Bun:** slower concurrent explores and higher idle CPU, as above.

---

## Contents

- [About this fork](#about-this-fork)
- [Get Started](#get-started)
- [Language Support](#language-support)
- [Why CodeGraph?](#why-codegraph)
- [Key Features](#key-features)
- [Framework-aware Routes](#framework-aware-routes)
- [Mixed iOS / React Native / Expo bridging](#mixed-ios--react-native--expo-bridging)
- [Quick Start](#quick-start)
- [How It Works](#how-it-works)
- [CLI Reference](#cli-reference)
- [MCP Tools](#mcp-tools)
- [Library Usage](#library-usage)
- [Configuration](#configuration)
- [Telemetry](#telemetry)
- [Verified releases](#verified-releases)
- [Supported Platforms](#supported-platforms)
- [Supported Agents](#supported-agents)
- [Supported Languages](#supported-languages)
- [Measured cross-file coverage](#measured-cross-file-coverage)
- [Troubleshooting](#troubleshooting)
- [License](#license)

## Get Started

### 1. Install the CLI

> These commands install **upstream's** release. To run this fork, [build it from source](#install-the-fork-from-source) instead, then continue with step 2.

**No Node.js required** — one command grabs the right build for your OS:

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh

# Windows (PowerShell)
irm https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.ps1 | iex
```

<details>
<summary><b>Already have Node? Use npm instead (works on any version)</b></summary>

```bash
npm i -g @colbymchenry/codegraph
```

<sub>CodeGraph bundles its own runtime — nothing to compile, no native build, works the same everywhere. The installer puts `codegraph` on your PATH but **doesn't change your current shell** — open a new terminal before the next step so the command resolves.</sub>

<sub>**Upgrade any time** with `codegraph upgrade` — it detects how you installed (bundle, npm, or npx) and updates in place. Add `--check` to see if an update is available, or `codegraph upgrade <version>` to pin one.</sub>

</details>

### 2. Wire up your agent(s)

In a **new terminal**, run the installer to connect CodeGraph to the agents you use:

```bash
codegraph install
```

<sub>Detects and auto-configures Claude Code, Cursor, Codex CLI, opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro, and GitHub Copilot (VS Code, Copilot CLI, JetBrains IDEs) — wiring the CodeGraph MCP server into each. **This is the step that connects CodeGraph to your agent;** installing the CLI in step 1 does not do it on its own. It only wires up your agent — it does **not** index any code; building each project's graph is the separate `codegraph init` in step 3. (Shortcut: `npx @colbymchenry/codegraph` downloads and runs this in one go.)</sub>

### 3. Initialize each project

```bash
cd your-project
codegraph init
```

<sub>`codegraph init` creates the local `.codegraph/` directory and builds the full graph in the same step — one command, done. In a git worktree whose sibling worktree is already indexed, it starts from a copy of that index and re-reads only the files that differ, which usually takes seconds (`--no-seed` builds from scratch).</sub>

<div align="center">

![1_C_VYnhpys0UHrOuOgpgoyw](https://github.com/user-attachments/assets/f168182f-4d9a-44e0-94d7-08d018cc8a3a)

</div>

### 4. No more syncing!

Auto-sync is enabled by default. CodeGraph watches the project and updates the graph on every file change — while your agent edits code, or you add, modify, or delete files. **The index is never stale, and there is nothing to re-run.**

### Uninstall

Changed your mind? One command removes CodeGraph from every agent it configured **and** the CLI itself — every install it finds (standalone bundle, npm global package, launcher link), shown to you before anything is deleted:

```bash
codegraph uninstall
```

Pass `--keep-cli` to remove only the agent configurations and keep the CLI installed.

<sub>Reverses the installer — strips CodeGraph's MCP server config, instructions, and permissions from each configured agent. Your project indexes (`.codegraph/`) are left untouched; remove those per-project with `codegraph uninit`. Use `--target` to remove from specific agents, or `--yes` to run non-interactively.</sub>

---

## Language Support

Every language below gets the same treatment — full structural extraction and cross-file resolution into one graph, no per-language setup:

<p align="center">
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/typescript.svg?v=1" width="104" height="104" alt="TypeScript" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/javascript.svg?v=1" width="104" height="104" alt="JavaScript" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/arkts.svg?v=1" width="104" height="104" alt="ArkTS" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/python.svg?v=1" width="104" height="104" alt="Python" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/go.svg?v=1" width="104" height="104" alt="Go" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/rust.svg?v=1" width="104" height="104" alt="Rust" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/java.svg?v=1" width="104" height="104" alt="Java" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/csharp.svg?v=1" width="104" height="104" alt="C#" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/php.svg?v=1" width="104" height="104" alt="PHP" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/ruby.svg?v=1" width="104" height="104" alt="Ruby" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/c.svg?v=1" width="104" height="104" alt="C" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/cpp.svg?v=1" width="104" height="104" alt="C++" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/objective-c.svg?v=1" width="104" height="104" alt="Objective-C" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/metal.svg?v=1" width="104" height="104" alt="Metal" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/cuda.svg?v=1" width="104" height="104" alt="CUDA" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/swift.svg?v=1" width="104" height="104" alt="Swift" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/kotlin.svg?v=1" width="104" height="104" alt="Kotlin" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/scala.svg?v=1" width="104" height="104" alt="Scala" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/dart.svg?v=1" width="104" height="104" alt="Dart" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/svelte.svg?v=1" width="104" height="104" alt="Svelte" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/vue.svg?v=1" width="104" height="104" alt="Vue" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/astro.svg?v=1" width="104" height="104" alt="Astro" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/liquid.svg?v=1" width="104" height="104" alt="Liquid" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/delphi.svg?v=1" width="104" height="104" alt="Pascal / Delphi" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/lua.svg?v=1" width="104" height="104" alt="Lua" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/r.svg?v=1" width="104" height="104" alt="R" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/luau.svg?v=1" width="104" height="104" alt="Luau" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/cfml.svg?v=1" width="104" height="104" alt="CFML" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/cobol.svg?v=1" width="104" height="104" alt="COBOL" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/vbnet.svg?v=1" width="104" height="104" alt="Visual Basic .NET" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/erlang.svg?v=1" width="104" height="104" alt="Erlang" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/solidity.svg?v=1" width="104" height="104" alt="Solidity" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/terraform.svg?v=1" width="104" height="104" alt="Terraform / OpenTofu" />
  <img src="https://raw.githubusercontent.com/colbymchenry/codegraph/main/assets/languages/nix.svg?v=1" width="104" height="104" alt="Nix" />
</p>

<sub>This fork also indexes Markdown documentation. Per-language details — extensions, frameworks, and what exactly gets extracted — in [Supported Languages](#supported-languages).</sub>

---

## Why CodeGraph?

When an AI agent needs to understand code — to answer a question or make a change — it discovers structure the slow way: grep, glob, and Read, one file at a time, rebuilding call paths and dependencies by hand. That's a pile of tool calls and round-trips before it even starts the real work.

**CodeGraph hands the agent the exact code it needs in one call.** It's a pre-built knowledge graph of every symbol, call edge, and dependency in your codebase — so instead of crawling files, the agent asks one question and gets back the relevant source, the call paths between those symbols (including dynamic-dispatch hops grep can't follow), and the blast radius of a change. **Surgical context, not a file-by-file search** — which means fewer tool calls and faster answers on every codebase, large or small.

<img width="1536" height="1024" alt="token-cost-savings-scale" src="https://github.com/user-attachments/assets/eb74a11a-a3ab-4b01-80a6-19f78352ae8e" />

> **A note on cost:** CodeGraph's win on *every* codebase is precision — the agent stops crawling files and answers from the graph. On current models that precision is also a large direct saving: the 2026-08 re-measurement, on a harness that blocks the CLI in both arms, put it at **44% lower cost and 62% fewer tokens on average** across the seven benchmark repos, because a strong model *without* the graph burns its budget re-deriving structure. Cost tracks how much *discovery* a question demands more than raw repo size: 57–78% on questions the file-reading agent needed 28–43 tool calls to answer, near-even where it got there in 7.

> **A note on context:** the numbers above measure *throughput* — tokens processed, tools called, dollars spent to reach one answer. They don't measure what is still sitting in your context window afterward, and on that axis CodeGraph costs **more**, not less. Across the same seven repos in multi-turn sessions, CodeGraph's responses leave about **80% more retrieval context resident** at the end of a session than a file-reading agent's do — on VS Code, 67k tokens against 18k. The mechanism is the same one that makes it fast: CodeGraph returns one dense, verbatim payload that answers the question and then stays in the window, where a grep-and-read agent churns through many small results that get evicted. Fewer tokens *processed* and a larger persistent *footprint* are both real at once. If you run long sessions in a small window, budget for it. Measured per-repo: [`docs/benchmarks/residual-context-occupancy.md`](docs/benchmarks/residual-context-occupancy.md).

### Benchmark Results

> Upstream's measurement of an upstream build; this fork has not re-run it. The fork's own numbers are under [About this fork](#measured-results).

Tested across **7 real-world open-source codebases** spanning 7 languages, comparing an agent (Claude Code, headless) answering one architecture question **with** and **without** CodeGraph, at the **median of 4 runs per arm**. _Re-measured 2026-08-05 on **Claude Opus 4.8** against the current build, on a harness that blocks the `codegraph` CLI in **both** arms — contamination row: 0 of 28 without-arm runs._

> **The universal win — every repo, every size: 88% fewer tool calls · 53% faster · 62% fewer tokens · 44% cheaper · file reads cut to zero on all seven repos.**

With the index available, the agent answers from one to four `codegraph_explore` calls and stops. Without it, the agent burns its budget on discovery — up to **43 tool calls and 19 file reads** re-deriving what the graph already knew. Every repo was faster with CodeGraph in this measurement — by 35% on the narrowest question, by 3.6× on the widest.

| Codebase | Language | Tool calls | Time | File reads | Tokens | Cost |
|----------|----------|------------|------|------------|--------|------|
| **VS Code** | TypeScript · ~11k files | **2 vs 28** | **2.2× faster** (58s vs 2m 10s) | **0** vs 12 | 77% fewer | 71% cheaper |
| **Excalidraw** | TypeScript · ~640 | **2 vs 43** | **3.6× faster** (45s vs 2m 42s) | **0** vs 18 | 84% fewer | 78% cheaper |
| **Django** | Python · ~3k | 3 vs 14 | 35% faster (54s vs 1m 23s) | **0** vs 8.5 | 41% fewer | 13% cheaper¹ |
| **Tokio** | Rust · ~790 | 3 vs 29 | **2.6× faster** (1m 3s vs 2m 43s) | **0** vs 19 | 65% fewer | 64% cheaper |
| **OkHttp** | Java · ~645 | 1 vs 6 | 43% faster (33s vs 58s) | **0** vs 2 | 54% fewer | 21% cheaper |
| **Gin** | Go · ~110 | 1 vs 7 | 39% faster (28s vs 46s) | **0** vs 4 | 52% fewer | ~even¹ |
| **Alamofire** | Swift · ~110 | 4 vs 33 | **2.6× faster** (54s vs 2m 22s) | **0** vs 16.5 | 59% fewer | 57% cheaper |

<sub>¹ Cost tracks how much *discovery* the question demanded, which is why it varies far more than the other columns: 57–78% on repos where the file-reading arm needed 28–43 tool calls, but only 13% on Django and even on Gin, where it got there in 14 and 7. The with-arm still answered in 3 and 1 calls with zero file reads. **File reads** = median files opened — the surgical-context win in one column: the agent never reads a file on any of the seven repos when CodeGraph is present.</sub>

<details>
<summary><strong>Per-repo breakdown — WITH vs WITHOUT (median of 4)</strong></summary>

| Codebase | Metric | WITH cg | WITHOUT cg |
|---|---|---|---|
| **VS Code** | Time / Tools / Tokens / Cost | 58s / 2 / 155k / $0.53 | 2m 10s / 28 / 670k / $1.80 |
| **Excalidraw** | Time / Tools / Tokens / Cost | 45s / 2 / 156k / $0.54 | 2m 42s / 43 / 991k / $2.43 |
| **Django** | Time / Tools / Tokens / Cost | 54s / 3 / 183k / $0.55 | 1m 23s / 14 / 309k / $0.63 |
| **Tokio** | Time / Tools / Tokens / Cost | 1m 3s / 3 / 201k / $0.66 | 2m 43s / 29 / 573k / $1.83 |
| **OkHttp** | Time / Tools / Tokens / Cost | 33s / 1 / 107k / $0.39 | 58s / 6 / 230k / $0.50 |
| **Gin** | Time / Tools / Tokens / Cost | 28s / 1 / 87k / $0.31 | 46s / 7 / 180k / $0.31 |
| **Alamofire** | Time / Tools / Tokens / Cost | 54s / 4 / 209k / $0.54 | 2m 22s / 33 / 505k / $1.27 |

</details>

<details>
<summary><strong>Full benchmark details</strong></summary>

**Methodology.** Each arm is `claude -p` (Claude Opus 4.8, `claude-opus-4-8`) run headlessly against the repo with `--strict-mcp-config`: **WITH** = CodeGraph's MCP server enabled, **WITHOUT** = an empty MCP config. Built-in Read/Grep/Bash stay available to both. Same question per repo, **4 runs per arm, median reported**. Cost = the run's `total_cost_usd`; Tokens = total tokens processed, summed per assistant turn (input incl. cache reads + cache creation + output); Time = wall-clock; Tool calls = every tool invocation, including those inside any sub-agents the model spawns. Repos cloned at `--depth 1` and indexed by the same CodeGraph build that served them. Re-measured 2026-08-05 on the current build.

**The `codegraph` CLI is blocked in both arms.** A sanitized `PATH` plus a `PreToolUse` hook denies any Bash invocation of the CLI, in the WITHOUT arm as well as the WITH arm. This matters: without that block the control arm is not a control. On an unblocked harness we measured the WITHOUT agent finding the CLI on `PATH` and reaching CodeGraph through Bash in **26 of 28 runs** — which distorts the comparison in both directions, since a CLI call is not counted as a tool call and its output still enters the window. Earlier published figures were produced without this block. In the run reported above, all 28 WITHOUT runs attempted the CLI and **all 28 were blocked — 0 contaminated**.

**Queries:**
| Codebase | Query |
|----------|-------|
| VS Code | "How does the extension host communicate with the main process?" |
| Excalidraw | "How does Excalidraw render and update canvas elements?" |
| Django | "How does Django's ORM build and execute a query from a QuerySet?" |
| Tokio | "How does tokio schedule and run async tasks on its runtime?" |
| OkHttp | "How does OkHttp process a request through its interceptor chain?" |
| Gin | "How does gin route requests through its middleware chain?" |
| Alamofire | "How does Alamofire build, send, and validate a request?" |

**Why CodeGraph wins:** with the index available, the agent answers directly — usually one `codegraph_explore` returns the relevant source — and stops, with zero file reads on every benchmark repo. Without it, the agent spends most of its budget on discovery (find/ls/grep) before reading the right code. CodeGraph only helps when queried *directly*, so its instructions steer agents to answer directly rather than delegate exploration to file-reading sub-agents — otherwise a sub-agent reads files regardless and CodeGraph becomes overhead.

</details>

---

## Built for speed — the Rust kernel

> The timings in this section are upstream's measurements of upstream builds; this fork has not re-run them. The fork's own numbers are under [About this fork](#measured-results).

CodeGraph's parsing engine is a **native Rust kernel**, and it is the only parser: every supported grammar is compiled into it. 20 languages — TypeScript, JavaScript, Java, Python, Go, C, C++, Rust, C#, Ruby, PHP, Swift, Kotlin, Scala, Dart, R, Lua, Luau (Metal and CUDA ride the C++ path) — parse in compiled code with one boundary crossing per file. Every language shipped only after its graphs proved **byte-for-byte identical** to the reference engine on real repositories, from small libraries up to the Linux kernel; the remaining languages are parsed by the kernel and walked by the generic extractor over its tree. Files with syntax errors are extracted from the native parse's recovery. Platforms: macOS (x64, arm64), Linux glibc (x64, arm64), Windows (x64, arm64); others need a from-source kernel build.

**And it scales itself to the machine it's on.** Worker pools, parallel resolution, and analysis caches are sized from what the system actually has — real core counts (container/cgroup-aware, so a VPS that grants 2 cores gets sized for 2, not the host's 64), honestly-measured available RAM on macOS and Linux, and the measured cost of *your* project's resolution work:

- **On a workstation:** the full parallel pipeline — native parse workers, a multi-worker resolver pool that engages the moment it pays for itself, memory-gated analysis caches. The Swift compiler repository (27k files of Swift and C++) fresh-indexes in about 100 seconds; a one-file edit re-syncs in ~4.
- **On a 2-core / 6GB VPS:** the same graph, from a pipeline tuned to *finish* — the Linux kernel (70k files, 2M symbols, 6.4M relationships) indexes to completion in under 12 minutes where RAM-first designs run out of memory before reaching 1%.
- **Every day after day one:** saving a file updates the graph in well under a second — the watcher fires 300ms after a lone save and syncs exactly what changed (~0.3s of work on a 4,400-file project, ~0.4s on the 27,000-file Swift compiler repo), never re-scanning the tree. Measured against the fastest competing indexer's re-index-on-change: 2–7× faster on medium and larger repos across a 31-repo, 30-language benchmark — and the gap widens with repo size, because their cost grows with the repository and ours grows with the change.

---

## Key Features

| | |
|---|---|
| **Native Rust Kernel** | Every language is parsed by a compiled Rust engine, and 20 of them are also extracted in Rust; files with syntax errors are extracted from the parser's error recovery |
| **Adapts to Your Machine** | Sizes its worker pools and caches from what the system actually has — real core counts (container-aware), honest available RAM, measured per-project cost. A workstation gets the full parallel pipeline; a 2-core VPS gets one tuned to finish reliably |
| **Surgical Context** | One tool call returns entry points, related symbols, and code snippets — no slow file-by-file exploration |
| **Full-Text Search** | Find code by name instantly across your entire codebase, powered by FTS5 |
| **Impact Analysis** | Trace callers, callees, and the full impact radius of any symbol before making changes |
| **Always Fresh** | File watcher uses native OS events (FSEvents/inotify/ReadDirectoryChangesW) with debounced auto-sync — the graph stays current as you code, zero config |
| **20+ Languages** | TypeScript, JavaScript, ArkTS, Python, Go, Rust, Java, C#, VB.NET, PHP, Ruby, C, C++, CUDA, Objective-C, Metal, Swift, Kotlin, Scala, Dart, Lua, Luau, R, Nix, Erlang, CFML, COBOL, Solidity, Terraform/OpenTofu, Svelte, Vue, Astro, Liquid, Pascal/Delphi, and Markdown documentation |
| **Framework-aware Routes** | Recognizes web-framework routing files and links URL patterns to their handlers; the frameworks are listed under [Framework-aware Routes](#framework-aware-routes) |
| **Mixed iOS / React Native / Expo** | Closes cross-language flows that static parsing misses: Swift ↔ ObjC bridging, React Native legacy bridge + TurboModules + Fabric view components, native → JS event emitters, Expo Modules |
| **100% Local** | No data leaves your machine. No API keys. No external services. SQLite database only |

<details>
<summary><strong>How auto-syncing works — and why you don't need to run <code>codegraph sync</code> manually</strong></summary>

When your agent (Claude Code, Cursor, Codex, opencode) launches `codegraph serve --mcp`, three layers keep the index in step with your code — and make sure the agent never gets a silent wrong answer in the brief window between an edit and the next sync:

1. **File watcher with debounced auto-sync.** A native FSEvents / inotify / ReadDirectoryChangesW watcher captures every source-file create / modify / delete and triggers a re-index after a debounce window (default `2000ms`, tunable via `CODEGRAPH_WATCH_DEBOUNCE_MS`, clamped to `[100ms, 60s]`). Bursts of edits collapse into a single sync.

2. **Per-file staleness banner.** During the brief debounce window, MCP tool responses that would reference a still-pending file prepend a `⚠️` banner naming it and telling the agent to `Read` it directly. Pending files NOT referenced by the response surface as a small footer instead. Either way, the agent gets an explicit signal — validated with Claude Code, where the agent literally says "Reading the file directly for the live content" before opening it.

3. **Connect-time catch-up.** When the MCP server (re)connects, codegraph runs a fast `(size, mtime)` + content-hash reconciliation against the working tree before answering the first query — so edits made while no MCP server was running (a `git pull` from the terminal, edits from another editor, a previous agent session that exited) get absorbed on the next session's first tool call.

```
agent writes src/Widget.ts
  → watcher fires (<100ms)
  → debounce (default 2s)
  → sync; Widget.ts is in the index
  → next agent query sees it
```

**Verify any time** with `codegraph status` (CLI). If anything is pending, you'll see a `### Pending sync:` section naming the files and their edit age.

The handful of cases where manual `codegraph sync` makes sense: the watcher is disabled (sandboxed environments, or `CODEGRAPH_NO_DAEMON=1`), or you're scripting against the index outside an agent session and want a pre-flight sync at the start of your script.

→ Full deep-dive in [Guides → Indexing a Project](https://colbymchenry.github.io/codegraph/guides/indexing/#stay-fresh-automatically).

</details>

---

## Framework-aware Routes

CodeGraph detects web-framework routing files and emits `route` nodes linked by `references` edges to their handler classes or functions. Querying callers of a view/controller now surfaces the URL pattern that binds it. A `codegraph_explore` question that spells out a route (`GET /api/tasks/:id`, or a bare `/blog/:slug` for a page) starts from that route node, so the file declaring it comes first.

| Framework | Shapes recognized |
|---|---|
| **Django** | `path()`, `re_path()`, `url()`, `include()` in `urls.py` (CBV `.as_view()`, dotted paths) |
| **Flask** | `@app.route('/path', methods=[...])`, blueprint routes, `add_url_rule(…)` and a project helper that passes paths with a `view_func=` |
| **FastAPI** | `@app.get(...)`, `@router.post(...)`, all standard methods |
| **Express** | `app.get(...)`, `router.post(...)` with middleware chains; inline arrow and function-expression handlers, including wrapper calls |
| **Hono / Elysia / Fastify / Koa / H3 / Hyper-Express / Bun / Effect / Vixeny** | Literal routes on each framework's app or router builder (`new Hono().get('/users', handler)`), with same-file prefixes and mounts; an imported handler is linked, an inline one contributes its direct calls. Fastify plugin files (`export default async function (fastify) { … }`) are read too, and files loaded by a literal `@fastify/autoload` registration get their directory prefix, `autoPrefix`/`prefixOverride` exports and `routeParams` folders |
| **NestJS** | `@Controller` + `@Get/@Post/...` (with `RouterModule` prefixes, `setGlobalPrefix` and URI versioning), GraphQL `@Resolver` + `@Query/@Mutation`, `@MessagePattern`/`@EventPattern`, `@SubscribeMessage` |
| **Laravel** | `Route::get()`, `Route::resource()`, string class/action handlers with namespace paths, tuple syntax |
| **Drupal** | `*.routing.yml` routes (`_controller`, `_form`, entity handlers); `hook_*` implementations in `.module`/`.theme`/`.install`/`.inc` |
| **Rails** | `get '/x', to: 'users#index'`, hash-rocket `=>` syntax, `resources` / `resource` with literal `only:` / `except:` action filters, and the paths and controller modules of `namespace`, `scope`, nested resources and `member` / `collection` blocks; a Rails engine's `config/routes.rb` too |
| **Spring** | `@GetMapping`, `@PostMapping`, `@RequestMapping` on methods |
| **Play** | `GET`/`POST`/… verb routes in `conf/routes` → `Controller.method` actions (Scala + Java), including projects kept in subdirectories |
| **Gin / chi / gorilla / mux** | `r.GET(...)`, `router.HandleFunc(...)` |
| **Axum / actix / Rocket** | `.route("/x", get(handler))` |
| **ASP.NET** | `[HttpGet("/x")]` attributes on action methods and FastEndpoints `Configure()` verb calls |
| **Vapor** | `app.get("x", use: handler)` and route-owned closure body calls |
| **Analog** | `src/app/pages/**/*.page.ts` files (`index`, dot segments, `[param]`, `[...rest]` and `(group)` names) bound to the page's default component class; a page with a same-named folder is a layout, not a route | — |
| **Astro** | `src/pages/` file-based routes (`.astro` pages + `.ts` endpoints, `[param]`/`[...rest]` syntax); each page calls its own file's component, exported `GET`/`POST`/… endpoint methods link to their handlers, and `<a href>` / `Astro.redirect` link to the page they name |
| **RedwoodSDK** | Literal `defineApp([...])` trees with `route`, `index`, `render`, `layout` and `prefix`, plus `{ get, post, … }` method tables; each route links to its final handler and becomes a page once that handler is shown to return JSX | — |

### Routers — routes *and* the navigation between them

These frameworks additionally emit **`navigates`** edges: the function that sends a user somewhere is linked to the screen it names, so "where does tapping this go" is one hop in the graph rather than a search. Each reads a literal destination — a computed one, or a path no route serves, is left unresolved rather than guessed — and a link written in markup is marked as inferred.

| Router | Routes from | Navigation from |
|---|---|---|
| **Expo Router** | Every screen file under `app/` (`app/item/[id].tsx` → `/item/[id]`, groups stripped), bound to its default-export component; `+api` files are endpoints (`GET /hello`) bound to their handlers | `router.push` / `replace` / `navigate`, template hrefs, `{ pathname }` objects, and a helper's returned href |
| **Next.js** | App Router `app/**/page.tsx` and Pages Router pages (`(group)` stripped, `[slug]` → `:slug`); `app/api/**/route.ts` exports and `pages/api/*` are endpoints, not screens | `router.push` / `replace` / `prefetch`, `redirect()` / `permanentRedirect()` in a server action or page, `NextResponse.redirect(new URL(…))` in middleware, `<Link href>` and internal `<a href>` |
| **React Router** | `<Route path component/element>` (v5 and v6), `createBrowserRouter` / `createHashRouter` / `createMemoryRouter` arrays with nested `children`, constant paths, `Component` and lazy module exports, framework mode's `app/routes.ts` (`route`, `index`, `layout`, `prefix`), and the default file convention under `app/routes/` (Remix, or React Router with `flatRoutes()`: dot nesting, index and pathless segments, `$param`, optional `($segment)` and `$` splats), each bound to its module's default component | `history.push` / `replace`, `useNavigate`'s `navigate`, a loader's `redirect`, `<Link to>` / `<NavLink to>` / `<Navigate to>` / v5's `<Redirect to>` / react-router-bootstrap's `<LinkContainer to>`, and a `styled(Link)` wrapper |
| **TanStack Router** | `createFileRoute('/posts/$postId')` (file-based) and `createRoute({ path, getParentRoute })` composed up its parent chain (code-based); `_pathless` segments, `(group)` folders, `__root` and `<Outlet/>` layouts are not addresses; TanStack Start `server.handlers` (and `createHandlers`) in those files become method-qualified endpoints (`GET /api/users`) | `navigate({ to })`, a thrown `redirect({ to })`, `<Link to>` / `<Navigate to>` — where `to` is the route PATTERN and the values ride beside it in `params` |
| **Vue Router** / **Nuxt** | `createRouter({ routes: [...] })` / `new Router(...)` and the route tables it's given (`export const constantRoutes = [...]`, per-module route files), with the view each entry names — a lazy `() => import(…)` bound to its file — and `children` joined onto their parent's path, the parent being the layout around them; plus, in a Nuxt app, `pages/` file-based routes, each calling its own file's page component (`index` folders, root index pages, Nuxt 4 route groups), `server/api/` and `server/routes/` endpoints (method suffixes such as `.get.ts`, catch-alls) and route middleware | `router.push` / `replace`, `$router.push` / `this.$router.push`, Nuxt's `navigateTo`, `<router-link>` / `<RouterLink>` / `<NuxtLink>` — **by route name** (`push({ name: 'profile' })`) as well as by path |
| **Solid Router** | Imported `Router`/`Route` JSX and route-config arrays (`path`, `component`, `children`) with static `lazy(() => import(...))` components. A table exported from another file as `RouteDefinition[]` (the official template's `routes.ts`) is read too, prefixed by where it is registered | — |
| **SolidStart** | SolidStart 1 (`app.config` with `defineConfig`) and 2 (the `solidStart()` Vite plugin): `src/routes/` file routes (`[param]`, `[[optional]]`, `[...rest]`, `(group)` folders, `index`), each page bound to its default component; exported `GET`/`POST`/… functions in API route files become endpoints | — |
| **Vike** | `+Page` files under `pages/` (filesystem routing, `index`, `(group)` folders, `@param` segments) and `+route` string overrides, each bound to its page component | — |
| **Qwik City** | `index` files under `src/routes/` (groups, `[param]` and `[...rest]` segments) bound to their `component$` page, plus `onGet`/`onPost`/… endpoint exports; layouts and `onRequest` middleware are not routes | — |
| **Waku** | `src/pages` files (`(group)` folders, `[param]` and `[...rest]` segments) bound to their default page component, plus literal `createPage` declarations inside a `createPages` callback registered in the server entry; `_layout`, `_root` and `_slices` files are not routes | — |
| **SvelteKit** | `src/routes/**/+page.svelte` (`[slug]` → `:slug`, `[[opt]]` → `:opt?`, `[id=matcher]` → `:id`, `(group)` folders stripped), joined to the `+page.server.js` beside it so a loader's guard belongs to its page | `goto('/x')`, `redirect(status, '/x')` from a load or form action, and the plain `<a href>` that is a link in a SvelteKit app |
| **Angular** | `Routes` arrays (`RouterModule.forRoot` / `forChild`, `provideRouter`, a routes file's default export) with `component` or a lazy `loadComponent`; `children` and lazy `loadChildren` (an NgModule's through its routing module) joined into full paths; paths written as route constants or `$localize` strings; a route with children is a layout around the screens inside it | `router.navigate([...])`, `navigateByUrl`, a guard's `createUrlTree` / `parseUrl` — a command array, a route constant, or a component property holding one — `routerLink` / `[routerLink]` in the component's template, and `redirectTo`. Each template's child components (`<app-foo>`) are linked to the component that renders them. Calls in bindings, interpolation and control-flow blocks link the component member they invoke |

In a repository holding several apps, each app's routes are matched only against navigation written inside that app. Framework detection checks the root and first two directory levels, then uses shallow JavaScript and TypeScript source paths for a bounded set of deeper app manifests. Shopify section and snippet references stay inside their nearest theme; JSON under `templates/` or `sections/` is Liquid only beside a theme marker. Sync reconciles JSON membership and unchanged Liquid links when a theme marker is added or removed.

---

## Mixed iOS / React Native / Expo bridging

Real iOS and React Native codebases live across multiple languages — a Swift caller invokes an Objective-C selector that's been auto-bridged, a JS file calls into a native module via the React Native bridge, a JSX component delegates to a native view manager. Static tree-sitter extraction stops at each language boundary. CodeGraph bridges them so `codegraph_explore` connects the flow end-to-end across the gap — call paths and blast radius cross the boundary instead of stopping at it.

| Boundary | JS / Swift side | Native side | How |
|---|---|---|---|
| **Swift → ObjC** | Swift `obj.foo(bar:)` | ObjC selector `-fooWithBar:` | `@objc` auto-bridging rules (including init/property/protocol forms) + Cocoa preposition prefixes (`With`/`For`/`By`/`In`/`On`/`At`/…) |
| **ObjC → Swift** | ObjC `[obj fooWithBar:]` | Swift `@objc func foo(bar:)` | Reverse-bridge name candidates; verifies `@objc` exposure from source |
| **React Native legacy bridge** | JS `NativeModules.X.fn(...)` | ObjC `RCT_EXPORT_METHOD` / `RCT_REMAP_METHOD` · Java/Kotlin `@ReactMethod` | Parses macro/annotation declarations to build a JS-name → native-method map |
| **React Native TurboModules** | JS `import M from './NativeM'; M.fn(...)` | Native impl matching the Codegen spec | Treats the `Native<X>.ts` spec interface as ground truth |
| **RN native → JS events** | JS `new NativeEventEmitter(...).addListener('e', cb)` | ObjC `[self sendEventWithName:@"e" body:...]` · Swift `sendEvent(withName: "e", ...)` · Java/Kotlin `.emit("e", ...)` | Synthesized cross-language event channel keyed by event name, written as a literal or as a constant the language scopes to the call site |
| **Expo Modules** | JS `requireNativeModule('X').fn(...)`, directly or through a binding (`export default requireNativeModule<T>('X')`) | Swift / Kotlin `Module { Name("X"); AsyncFunction("fn") { ... } }` | Parses the Expo DSL literals into method nodes; a call on a binding resolves to module `X`'s `fn` on both platforms, else to the method on the binding's declared type |
| **Fabric view components** | JSX `<MyView prop={v}/>` | TS Codegen spec + native impl class | Spec → `component` node; convention-based name+suffix lookup (`View`/`ComponentView`/`Manager`/`ViewManager`) bridges to native |
| **Legacy Paper view managers** | JSX `<MyView prop={v}/>`, through a `requireNativeComponent('X')` module | ObjC `RCT_EXPORT_VIEW_PROPERTY` · Java/Kotlin `@ReactProp` | Same as Fabric — `requireNativeComponent('X')` is a JS `component` node, and Paper-era declarations also produce `component` + `property` nodes |

**Validated on real codebases** (small + medium + large for each bridge):

| Bridge | Small | Medium | Large |
|---|---|---|---|
| Swift ↔ ObjC | [Charts](https://github.com/danielgindi/Charts) | [realm-swift](https://github.com/realm/realm-swift) | [Wikipedia-iOS](https://github.com/wikimedia/wikipedia-ios) |
| RN legacy bridge | [AsyncStorage](https://github.com/react-native-async-storage/async-storage) | [react-native-svg](https://github.com/software-mansion/react-native-svg) | [react-native-firebase](https://github.com/invertase/react-native-firebase) |
| RN native → JS events | [RNGeolocation](https://github.com/Agontuk/react-native-geolocation-service) | — | react-native-firebase |
| Expo Modules | expo-haptics | expo-camera | expo SDK sweep (7 packages) |
| Fabric / Paper views | [react-native-segmented-control](https://github.com/react-native-segmented-control/segmented-control) | [react-native-screens](https://github.com/software-mansion/react-native-screens) | [react-native-skia](https://github.com/Shopify/react-native-skia) |

Every bridge hop says how it got into the graph. A hop matched by a bridge resolver carries `metadata.resolvedBy: 'framework'` and `metadata.framework` naming the resolver (`swift-objc-bridge`, `react-native-bridge`, `expo-modules-js`, `fabric-view`). A synthesized channel is tagged `provenance:'heuristic'` with `metadata.synthesizedBy` (`rn-event-channel`, `fabric-native-impl`).

---
## Quick Start

### 1. Run the Installer

```bash
npx @colbymchenry/codegraph
```

The installer will:

- Ask which agent(s) to configure — auto-detects installed ones from: **Claude Code**, **Cursor**, **Codex CLI**, **opencode**, **Hermes Agent**, **Gemini CLI**, **Antigravity IDE**, **Kiro**, **GitHub Copilot** (VS Code, Copilot CLI, JetBrains IDEs), **Devin** (CLI and Desktop)
- Prompt to install `codegraph` on your PATH (so agents can launch the MCP server)
- Ask whether configs apply to all your projects or just this one
- Write each chosen agent's MCP server config, plus a small marker-fenced CodeGraph section in the agent's instructions file (`CLAUDE.md` / `AGENTS.md` / `GEMINI.md`) — that's how subagents and non-MCP agents learn the `codegraph explore` command, since the MCP server's own guidance only reaches the main agent. Removed cleanly by `codegraph uninstall`.
- Set up auto-allow permissions when Claude Code is one of the targets

The installer **wires up your agents only — it does not index your code.** After it finishes, build each project's graph yourself with `codegraph init` (step 3). One global `codegraph install` covers every project; you run `codegraph init` once per project.

**Non-interactive (scripting / CI):**

```bash
codegraph install --yes                              # auto-detect agents, install global
codegraph install --yes --init                       # same, then build the current project's index (one-shot bootstrap)
codegraph install --target=cursor,claude --yes       # explicit target list
codegraph install --target=auto --location=local     # detected agents, project-local
codegraph install --target=copilot-vscode,copilot-cli,copilot-jetbrains --yes  # GitHub Copilot everywhere
codegraph install --print-config codex               # print snippet, no file writes
codegraph install --print-config copilot-vscode      # same, for Copilot in VS Code
```

| Flag | Values | Default |
|---|---|---|
| `--target` | `auto`, `all`, `none`, or csv (`claude,cursor,...`) | prompt |
| `--location` | `global`, `local` | prompt |
| `--yes` | (boolean) | prompt every step |
| `--init` | (boolean) run `codegraph init` in the current directory after wiring agents | — |
| `--no-permissions` | (boolean) skip Claude auto-allow list | permissions on |
| `--print-config <id>` | dump snippet for one agent and exit | — |

### 2. Restart Your Agent

Restart your agent (Claude Code / Cursor / Codex CLI / opencode / Hermes Agent / Gemini CLI / Antigravity IDE / Kiro / VS Code, the Copilot CLI, your JetBrains IDE for GitHub Copilot, or start a new Devin session) for the MCP server to load.

### 3. Initialize Projects

```bash
cd your-project
codegraph init
```
Builds the per-project knowledge graph index, which then auto-syncs on every file change. A single global `codegraph install` works in every project you open — no need to re-run the installer per project. Add `--yes` to skip every prompt (scripts / CI / container bootstraps).

That's it — your agent will use CodeGraph tools automatically when a `.codegraph/` directory exists.
<details>
<summary><strong>Manual Setup (Alternative)</strong></summary>

**Install globally:**

```bash
npm install -g @colbymchenry/codegraph
```

**Add to `~/.claude.json`:**

```json
{
  "mcpServers": {
    "codegraph": {
      "type": "stdio",
      "command": "codegraph",
      "args": ["serve", "--mcp"],
      "alwaysLoad": true
    }
  }
}
```

`alwaysLoad` loads all tools exposed by this server at session start, avoiding a tool-search step. In this fork that includes `codegraph_explore` and `codegraph_sessions`. See [Claude Code's MCP documentation](https://code.claude.com/docs/en/mcp#exempt-a-server-from-deferral).

**Add to `~/.claude/settings.json` (optional, for auto-allow):**
```json
{
  "permissions": {
    "allow": [
      "mcp__codegraph__*"
    ]
  }
}
```

<sub>One wildcard auto-approves every CodeGraph tool — `codegraph_explore` is the only one listed by default, but if you re-enable others via `CODEGRAPH_MCP_TOOLS` they're already permitted, no prompt.</sub>

</details>

<details>
<summary><strong>Agent Tool Guidance</strong></summary>

CodeGraph's MCP server delivers its usage guidance to your agent **automatically**, in the MCP `initialize` response. In short, it tells the agent to:

- **Answer structural questions directly with CodeGraph** — it *is* the pre-built index, so a grep/read loop just repeats work it already did. Treat the returned source as already read.
- **Reach for `codegraph_explore` for almost anything** — "how does X work", a flow/"how does X reach Y", or surveying an area. One call returns the relevant symbols' verbatim source grouped by file, the call paths between them (dynamic-dispatch hops included), and a blast-radius summary. Name a file or symbol in the query to read its current line-numbered source.
- **Trust the results — don't re-verify with grep**, and check the staleness banner after edits.
- Works **per project**: query any project that has a `.codegraph/` index by passing `projectPath` — so a monorepo where only some services are indexed, or a second repo, works in one session. A path with no index returns clean guidance to use built-in tools; indexing stays your decision.

The exact text is `src/mcp/server-instructions.ts` — the single source of truth for the main agent. Because subagents and non-MCP harnesses never see the MCP guidance, the installer also writes a short marker-fenced section into the agent's instructions file pointing at the `codegraph explore` CLI equivalent.

</details>

---

## How It Works

```
┌───────────────────────────────────────────────────────────────────┐
│                            Claude Code                            │
│                                                                   │
│   "How does a request reach the database?"                        │
│       calls CodeGraph tools directly — no Explore sub-agent       │
│                                 │                                 │
└─────────────────────────────────┬─────────────────────────────────┘
                                  │
                                  ▼
┌───────────────────────────────────────────────────────────────────┐
│                        CodeGraph MCP Server                       │
│                                                                   │
│ explore  ·  one call → verbatim source + call flow + blast radius │
│                                 │                                 │
│                                 ▼                                 │
│                       SQLite knowledge graph                      │
│          symbols · edges · files · FTS5 full-text search          │
└───────────────────────────────────────────────────────────────────┘
```

1. **Extraction** — a native **Rust kernel** parses source with [tree-sitter](https://tree-sitter.github.io/) grammars compiled into it, extracting nodes (functions, classes, methods) and edges (calls, imports, extends, implements) for 20 languages; the remaining languages are parsed by the same kernel and walked by a generic extractor over its tree.

2. **Storage** — Everything goes into a local SQLite database (`.codegraph/codegraph.db`) with FTS5 full-text search.

3. **Resolution** — After extraction, references are resolved: function calls → definitions, imports → source files, class inheritance, and framework-specific patterns.

4. **Auto-Sync** — The MCP server watches your project using native OS file events. Changes are debounced (2-second quiet window), filtered to source files only, and incrementally synced. The graph stays fresh as you code — no configuration needed.

---

## CLI Reference

```bash
codegraph                         # Run interactive installer
codegraph install                 # Run installer (explicit)
codegraph uninstall               # Remove CodeGraph from your agents AND the CLI (--keep-cli for configs only)
codegraph init [path]             # Initialize a project + build its graph (one step; --no-seed in a worktree)
codegraph uninit [path]           # Remove CodeGraph from a project (--force to skip prompt)
codegraph index [path]            # Full index (--force to re-index, --quiet for less output)
codegraph sync [path]             # Incremental update
codegraph status [path]           # Show statistics
codegraph ui [path]               # Browser viewer (alias: web; --port, --no-open); not released yet, needs CODEGRAPH_UI=1
codegraph unlock [path]           # Remove a stale lock file that's blocking indexing
codegraph query <search>          # Search symbols (--kind, --limit, --json)
codegraph explore <query>         # Relevant symbols' source + call paths in one shot (same output as the codegraph_explore MCP tool)
codegraph context <task...>       # Context for a task: relevant symbols, relationships and code (--format markdown|json, --max-nodes, --no-code)
codegraph sessions <words...>     # Search Claude Code, Codex, Cursor/T3, OpenCode, AGY, Devin, and Grok transcripts and commit messages for this project (--role, --since <days>, --session, --any, --full, --json; same output as codegraph_sessions)
codegraph node <symbol|file>      # One symbol's source + callers, or read a file with line numbers (same output as codegraph_node)
codegraph files [path]            # Show file structure (--format, --filter, --max-depth, --json)
codegraph callers <symbol>        # Find what calls a function/method (--limit, --json)
codegraph callees <symbol>        # Find what a function/method calls (--limit, --json)
codegraph impact <symbol>         # Analyze what code is affected by changing a symbol (--depth, --json)
codegraph affected [files...]     # Find test files affected by changes (see below)
codegraph daemon                  # Manage background daemons — pick one to stop (alias: daemons)
codegraph telemetry [on|off]      # Show or change anonymous usage telemetry
codegraph upgrade [version]       # Update to the latest release (--check, --force)
codegraph version                 # Print the installed version (also -v, --version)
codegraph help [command]          # Show help, optionally for one command
```

### `codegraph affected`

Traces import dependencies transitively to find which test files are affected by changed source files.

```bash
codegraph affected src/utils.ts src/api.ts         # Pass files as arguments
git diff --name-only | codegraph affected --stdin   # Pipe from git diff
codegraph affected src/auth.ts --filter "e2e/*"     # Custom test file pattern
```

| Option | Description | Default |
|--------|-------------|---------|
| `--stdin` | Read file list from stdin | `false` |
| `-d, --depth <n>` | Max dependency traversal depth | `5` |
| `-f, --filter <glob>` | Custom glob to identify test files | auto-detect |
| `-j, --json` | Output as JSON | `false` |
| `-q, --quiet` | Output file paths only | `false` |

**CI/hook example:**

```bash
#!/usr/bin/env bash
AFFECTED=$(git diff --name-only HEAD | codegraph affected --stdin --quiet)
if [ -n "$AFFECTED" ]; then
  npx vitest run $AFFECTED
fi
```

---

## MCP Tools

When running as an MCP server, CodeGraph exposes **one tool for code** — `codegraph_explore` — and one for the project's own history — `codegraph_sessions`. Measured agent behavior showed that one strong code tool steers agents better than a menu of narrower ones — fewer mis-picks, and it saves context every session:

| Tool | Purpose |
|------|---------|
| `codegraph_explore` | Answer almost any question in one call — "how does X work", a flow ("how does X reach Y"), or surveying an area — returning the relevant symbols' verbatim source grouped by file, plus the call paths between them and a blast-radius summary. Surfaces dynamic-dispatch hops (callbacks, React re-render, interface→impl) grep can't follow. Name a file or symbol in the query to read its current line-numbered source, the same shape the Read tool gives you. |
| `codegraph_sessions` | Search this project's Claude Code, Codex, Cursor/T3, OpenCode, AGY, Devin, and Grok transcripts and its last 2000 git commit messages, including active sessions: prompts, replies, and compaction summaries, excluding tool traffic. Uses stemmed, BM25-ranked full-text search, stored locally in `.codegraph/sessions-v2.db` and refreshed for changed files on each call. Each hit includes its session (`claude:`, `codex:`, `cursor:`, `opencode:`, `agy:`, `devin:`, `grok:`, or `git:`), role, time, transcript path, and matching passage; `full` returns each hit's whole stored passage instead of the snippet, up to 16,000 bytes in all, and later hits keep their snippet. Common words are dropped, and when too few passages hold every remaining word, passages holding some of them follow, marked. Harness-injected text (skill bodies, system reminders) is not indexed. Set `"sessions": false` in `codegraph.json` to opt out; `CODEGRAPH_SESSIONS_DIR` selects another Claude-format transcript directory exclusively. |

The other tools (`codegraph_node`, `codegraph_search`, `codegraph_callers`, `codegraph_callees`, `codegraph_impact`, `codegraph_files`, `codegraph_status`) stay fully functional but **unlisted by default** — everything they return already arrives inline on `codegraph_explore` (its blast-radius section, the relationship map, a symbol's body as its callee list). Re-enable any of them for the MCP surface with the `CODEGRAPH_MCP_TOOLS` environment variable (e.g. `CODEGRAPH_MCP_TOOLS=explore,node,search,callers`), or use their CLI equivalents (`codegraph node` / `query` / `callers` / `callees` / `impact` / `files` / `status`).

Even when the server's own root has no `.codegraph/` index, the tools stay available: pass `projectPath` to query any indexed project — a sub-service in a monorepo, or a second repo — in the same session. A path that has no index returns clean guidance to use built-in tools instead, so nothing fails loudly, and indexing stays your decision. A project opened this way is watched and kept in sync while the session uses it, and released after 10 minutes without a query (`CODEGRAPH_PROJECT_IDLE_TIMEOUT_MS`; `0` keeps it open).

---

## Library Usage

CodeGraph can be embedded directly. The npm package re-exports its programmatic
API, so both `import` and `require` resolve the `CodeGraph` class in your own
process — handy for embedding it in an app (e.g. an Electron main process).

```typescript
import CodeGraph from '@colbymchenry/codegraph';
// CommonJS works too:
//   const { CodeGraph } = require('@colbymchenry/codegraph');

const cg = await CodeGraph.init('/path/to/project');
// Or: const cg = await CodeGraph.open('/path/to/project');

await cg.indexAll({
  onProgress: (p) => console.log(`${p.phase}: ${p.current}/${p.total}`)
});

const results = cg.searchNodes('UserService');
const callers = cg.getCallers(results[0].node.id);
const context = await cg.buildContext('fix login bug', { maxNodes: 20, includeCode: true, format: 'markdown' });
const impact = cg.getImpactRadius(results[0].node.id, 2);

cg.watch();   // auto-sync on file changes
cg.unwatch(); // stop watching
cg.close();
```

Lower-level building blocks are exported from the same entry point for callers
that drive the graph directly: `DatabaseConnection`, `QueryBuilder`,
`getDatabasePath`, `initGrammars` / `loadGrammarsForLanguages`, and `FileLock`.

### Installer runtime control

Installers can use the supported `dist/runtime-control.js` module without loading
the graph API. Its functions are also exported from the package entry point.
`RUNTIME_CONTROL_PROTOCOL` is `1`. The API verifies daemon identity and readiness,
reports stop outcomes, and reserves the writer slot for a live updater. A
replacement bootstrap claims that reservation before running its CLI in the same
process. Keep the coordination module available separately from the executable
being replaced, including during rollback. Live legacy writers require explicit
session quiescence before cutover. See the [runtime control contract](site/src/content/docs/reference/api.md#installer-runtime-control).

For ordinary checkout startup, `startRuntimeWatcher(root, { expectedVersion,
cliPath, runtimePath?, timeoutMs? })` reuses or elects a shared daemon without
replacement. The elected daemon initializes a missing index at the exact root
under writer ownership. Startup requests a fresh reconciliation even when reusing
a watcher, and waits for file indexing and queued synthesized edges to complete.
It returns `{ pid, version, projectRoot, watching: true }` only after verifying
the exact root, build, active watcher and unchanged ownership records. An older,
uncertain, direct or promotion holder blocks startup. Failed reconciliation or a
timeout cannot return ready. The caller selects an absolute CLI path from the
same validated build and its runtime; this operation does not install, promote
or move artifacts. Legacy CLI initialization that ignores writer ownership
remains outside this guard.

**Embedding requirements**

- Install from npm (`npm i @colbymchenry/codegraph`) so the matching
  per-platform package — which carries the compiled library and its
  dependencies — is fetched alongside the shim.
- The API runs on **your** runtime, so it needs **Node 22.13+** for the built-in
  `node:sqlite` (Electron qualifies when its bundled Node is 22.13+). The CLI and
  MCP server are unaffected — they run on the self-contained bundled runtime.
- TypeScript types ship with the package. As with any Node-targeting library,
  keep `@types/node` available and `skipLibCheck: true` (the common default).

---

## Configuration

Next to none — CodeGraph is **zero-config by default**, with nothing to write or
keep in sync to get started. Language support is automatic from the file
extension; there's nothing to wire up per language. The one optional file is for
mapping [custom file extensions](#custom-file-extensions).

What it skips out of the box:

- **Dependency, build, and cache directories** — `node_modules`, `vendor`,
  `dist`, `build`, `target`, `.venv`, `Pods`, `.next`, and the like across every
  [supported stack](#supported-languages) — so the graph is your code, not
  third-party noise. This holds even with no `.gitignore`.
- **Anything in your `.gitignore`** — honored in git repos via git, and in
  non-git projects by reading `.gitignore` directly (root and nested).
- **Files larger than 1 MB** — generated bundles, minified JS, vendored blobs.

To keep something else out, add it to `.gitignore`. To pull a default-excluded
directory back **in** (say you really do want a vendored dependency indexed),
add a negation — `!vendor/`. The defaults apply uniformly, so committing a
dependency or build directory doesn't force it into the graph; the `.gitignore`
negation is the explicit opt-in.

`.gitignore` can't drop a directory you've **committed**, though. For a vendored
theme or SDK that's checked into the repo (e.g. a Metronic theme under
`static/`), list it under `exclude` in `codegraph.json` — gitignore-style
patterns, matched against repo-root-relative paths, honored on index, sync, and
watch:

```json
{
  "exclude": ["static/", "**/vendor/**"]
}
```

Conversely, when real source is gitignored on purpose — a project under a second
VCS (SVN, Perforce) that `.gitignore`s its own source so it stays out of Git —
force it back in with `include` (the opposite of `exclude`; `includeIgnored`
only revives embedded git repos, not plain source):

```json
{
  "include": ["Tools/", "Local/typescript/"]
}
```

CodeGraph discovers those files off disk, overriding `.gitignore`, on index,
sync, and watch. An explicit `exclude` still wins, and built-in skips
(`node_modules`, `dist`, `.git`) are never re-included.

Sometimes a directory shouldn't leave the index — you still want to find things
in it — it just shouldn't *outrank* your real code. A `scripts/` or
`optional-skills/` tree whose helpers use generic names (`usage`, `status`,
`run`) can win on an exact name match and crowd out the product code that
actually answers the query. Name those trees under `deprioritize`:

```json
{
  "deprioritize": ["optional-skills/", "scripts/"]
}
```

This is the ranking counterpart to `exclude`: those paths stay indexed and
findable — searching for them directly still works — they just stop winning
against first-party code. It applies to `query` / `search` and to `explore`'s
ranking. It is *not* a filter: unlike the built-in `example/`, `sample/`,
`fixture/`, `benchmark/` and `demo/` handling — which also drops those files
from some result sets outright — `deprioritize` only ever changes rank. Reach
for `exclude` when you want something gone.

### Custom file extensions

If your project uses a non-standard extension for a [supported
language](#supported-languages) — say `.dota_lua` for Lua, or `.tpl` for PHP —
those files are skipped by default, because the extension isn't one CodeGraph
recognizes. Map them with an optional **`codegraph.json`** at your project root:

```json
{
  "extensions": {
    ".dota_lua": "lua",
    ".tpl": "php"
  }
}
```

Each value is a supported language id. The mappings merge on top of the built-in
defaults and win on conflict, so you can also re-point a built-in (e.g.
`".h": "cpp"`). Commit the file to share the mapping with your team. A typo'd
language or a malformed file is warned about and skipped — it never breaks
indexing — and a project with no `codegraph.json` behaves exactly as before.
Re-index (`codegraph index`) after adding or changing mappings.

## Telemetry

CodeGraph collects **anonymous usage statistics** — which tools and commands get
used, which languages get indexed — to guide where language and agent support
work goes. **Never** any code, paths, file or symbol names, queries, or IP
addresses; usage is aggregated locally into daily totals before anything is
sent, and the ingest endpoint is [public code in this repo](telemetry-worker/)
that enforces the documented field list. The installer asks up front; turn it
off any time:

```bash
codegraph telemetry off    # or: CODEGRAPH_TELEMETRY=0, or DO_NOT_TRACK=1
```

[`TELEMETRY.md`](TELEMETRY.md) lists every field, with the off-switches and the
full data-handling story.

## Verified releases

Every artifact is built and published by the public
[Release workflow](.github/workflows/release.yml) — never from a laptop — and
carries cryptographic proof of it:

- **npm packages** are published via [trusted publishing](https://docs.npmjs.com/trusted-publishers)
  (OIDC — no long-lived npm tokens exist that could be stolen) with
  [provenance attestations](https://docs.npmjs.com/generating-provenance-statements)
  linking every version to the exact commit and workflow run that built it.
  Verify what's installed:

  ```bash
  npm audit signatures
  ```

- **GitHub Release bundles** (and `SHA256SUMS`) carry signed
  [build attestations](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations)
  (SLSA v1.0 Build Level 2). Verify any downloaded bundle:

  ```bash
  gh attestation verify codegraph-darwin-arm64.tar.gz -R colbymchenry/codegraph
  ```

Releases published before July 2026 predate this pipeline and don't carry
attestations.

## Supported Platforms

Every release ships a self-contained build (bundled Node runtime — nothing to
compile) for all three desktop OSes, on both Intel/AMD (x64) and ARM (arm64):

| Platform | Architectures | Install |
|----------|---------------|---------|
| Windows | x64, arm64 | PowerShell installer or npm |
| macOS | x64, arm64 | shell installer or npm |
| Linux | x64, arm64 | shell installer or npm |

See [Get Started](#get-started) for the one-line install commands.

## Supported Agents

The interactive installer auto-detects and configures each of these — wiring up
the MCP server (which delivers its own usage guidance, so no instructions file
is written):

- **Claude Code**
- **Cursor**
- **Codex CLI**
- **opencode** — MCP entry is OpenCode 2's `mcp.servers.codegraph` with `codemode: false` (keeps `codegraph_explore` on the native tool list; `codegraph install` migrates the older `mcp.codegraph` shape)
- **Hermes Agent**
- **Gemini CLI**
- **Antigravity IDE**
- **Kiro**
- **GitHub Copilot** — Copilot Chat in VS Code (`copilot-vscode`), the Copilot CLI (`copilot-cli`), and the Copilot plugin in JetBrains IDEs (`copilot-jetbrains`)
- **Devin** — Devin CLI and Devin Desktop; MCP entry goes to `~/.config/devin/mcp_config.json` (`%APPDATA%\devin\` on Windows) or `.devin/mcp_config.json` project-locally, with the short pointer block in `AGENTS.md`

## Supported Languages

| Language | Extension | Status |
|----------|-----------|--------|
| TypeScript | `.ts`, `.tsx` | Full support |
| JavaScript | `.js`, `.jsx`, `.mjs` | Full support |
| ArkTS (HarmonyOS) | `.ets` | Full support (everything TypeScript has, plus `@Component`/`@ComponentV2` structs with their ArkUI decorators (`@State`/`@Prop`/`@Link`/`@Local`/`@Builder`/…), `build()` view trees — parent→child component edges, chained-attribute links to `@Extend`/`@Styles` functions, `.onClick(this.handler)` event bindings — dynamic-dispatch bridges for state→`build()` re-renders, `@ohos.events.emitter` emit→subscriber pairs (static event keys only), and `router.pushUrl` literal urls → the target page struct; ohpm workspace modules resolve bare `import { X } from "data"` through `oh-package.json5` `file:` dependencies, honoring each module's `main` entry) |
| Python | `.py` | Full support |
| Go | `.go` | Full support |
| Rust | `.rs` | Full support |
| Java | `.java` | Full support |
| C# | `.cs` | Full support |
| PHP | `.php`, `.inc` | Full support (see Pascal for `.inc` include files) |
| Ruby | `.rb` | Full support |
| C | `.c`, `.h` | Full support |
| C++ | `.cpp`, `.hpp`, `.cc` | Full support |
| Objective-C | `.m`, `.mm`, `.h` | Partial support (classes, protocols, methods, `@property`, `#import`, message sends; `.mm` ObjC++ may parse incompletely) |
| Metal | `.metal` | Full support (vertex/fragment/kernel functions, structs, type aliases, call edges — MSL parses as C++, with `[[attribute]]` annotations handled) |
| CUDA | `.cu`, `.cuh` | Full support (kernels and device/host functions, structs, classes, host→kernel call edges through `<<<grid, block>>>` launch syntax — templated launches, function-pointer launches (`auto kernel = &fn<...>`), `dim3{...}` configs, and macro-defined kernels included; `__global__`/`__device__`/`__launch_bounds__` specifiers handled; CUDA in plain `.h`/`.hpp` headers recognized by content) |
| Swift | `.swift` | Full support |
| Kotlin | `.kt`, `.kts` | Full support |
| Scala | `.scala`, `.sc` | Full support (classes, traits, objects, methods, type aliases, Scala 3 enums) |
| Dart | `.dart` | Full support |
| Svelte | `.svelte` | Full support (instance and module script extraction, Svelte 5 runes, SvelteKit routes) |
| Vue | `.vue` | Full support (script + script-setup extraction with component ownership, Options API methods, computed properties, watchers and lifecycle hooks, Nuxt page/API/middleware routes) |
| Astro | `.astro` | Full support (component-owned frontmatter + browser-script extraction, template component/call references, `src/pages/` routes) |
| Liquid | `.liquid` | Full support |
| Pascal / Delphi | `.pas`, `.dpr`, `.dpk`, `.lpr`, `.inc` | Full support (classes, records, interfaces, enums, DFM/FMX form files; a `.inc` include is Pascal when it has no PHP open tag and reads as Pascal, unless `codegraph.json` maps `.inc`) |
| Lua | `.lua` | Full support (functions, methods with receivers, local variables, `require` imports, call edges) |
| R | `.R` `.r` | Full support (functions in every assignment form, S4/R5/R6 classes with methods, `library`/`require` imports, `source()` file references, call edges) |
| Luau | `.luau` | Full support (everything in Lua, plus `type`/`export type` aliases, typed signatures, and Roblox instance-path `require`) |
| CFML | `.cfc`, `.cfm`, `.cfs` | Full support (tag-based `<cfcomponent>`/`<cfinterface>`/`<cffunction>` and bare-script `component { ... }` styles, `extends`/`implements`, embedded `<cfscript>` delegation, call edges, including calls written in tags such as `<cfset>`, `<cfif>`, `<cfloop condition>` and `#…#` expressions) |
| COBOL | `.cbl`, `.cob`, `.cpy` | Full support (programs, sections/paragraphs with PERFORM/GO TO call edges, CALL 'literal' cross-program calls, COPY copybook imports — including standalone `.cpy` files — DATA DIVISION records/fields/88-levels, EXEC CICS LINK/XCTL and EXEC SQL INCLUDE targets; fixed and free format) |
| Visual Basic .NET | `.vb` | Full support (classes, Modules, interfaces, structures, enums, properties, events, `Declare` P/Invoke, `Handles`/`WithEvents`, `Inherits`/`Implements` edges, call edges through VB's call/index paren ambiguity, `As New` instantiation, interpolated strings, LINQ, Unicode identifiers) |
| Erlang | `.erl`, `.hrl`, `.escript`, `.app.src`, `.app` | Full support (functions with multi-clause/multi-arity grouping, `-spec` signatures, records with fields, `-type`/`-opaque` aliases, `-define` macros, `-include`/`-include_lib`/`-import` edges, local and `mod:fn` remote call edges, `fun name/arity` references, `spawn`/`apply`/`proc_lib`/`timer`/`rpc` MFA-argument call edges, `gen_server:call/cast(?MODULE)` → own `handle_call`/`handle_cast` links, `-behaviour` links, `-export`-based visibility) |
| Solidity | `.sol` | Full support (contracts, libraries, interfaces, structs, enums, modifiers, events, errors, state variables, `import`/`using` directives, `emit`/`revert` calls) |
| Terraform / OpenTofu | `.tf`, `.tfvars`, `.tofu` | Full support (resources, data sources, modules, variables, outputs, providers incl. aliases, `locals`; `var.`/`local.`/`module.`/resource references with Terraform's per-directory scoping enforced; module calls bridged across the boundary — inputs to the child module's variables, `module.M.out` to the child's output, `source` to the module's files; cloudposse/atmos `remote-state` cross-component wiring when the component is statically named; `provider = aws.east` selections resolved up the module tree; `moved`/`import`/`removed`/`check` block references; `.tfvars` assignments linked to the variables they set) |
| Nix | `.nix` | Full support (functions with simple/destructured/curried params, `let`/attrset bindings, `inherit`, `import ./path` file edges — `./dir` resolving through `default.nix` — plus NixOS module `imports = [ ./x.nix ]` lists and `callPackage ./pkg.nix` file edges; call edges; module-system option wiring — a config write like `launchd.user.agents.x = { ... }` links to the module declaring `options.launchd.user.agents`, so option flows trace across modules) |
| Markdown | `.md`, `.mdx`, `.markdown` | Documentation structure (headings, sections, local links, selected table rows and list items, shell command references); this fork only |
| Razor / Blazor | `.cshtml`, `.razor` | Markup linked to the C# it names (`@model`, `@inherits`, components, `@inject`) |
| XML | `.xml` | MyBatis mapper statements linked to their Java mapper methods |
| YAML | `.yml`, `.yaml` | File tracking; Drupal `*.routing.yml` routes and Spring config keys |
| Java properties | `.properties` | Spring config keys |
| Twig | `.twig` | File tracking only |

## Measured cross-file coverage

> Upstream's measurement; this fork has not re-run it.

Impact and blast-radius queries are only as good as the dependency graph behind them, so coverage is measured rather than asserted. **Fair coverage** = the share of symbol-bearing source files that have at least one *resolved cross-file dependent* — something that imports, calls, references, or (through a framework convention) routes to them — on a real-world benchmark repo per language. The residual is always a genuine static-analysis frontier (runtime dynamic dispatch, reflection / DI containers, framework-convention entry points, vendored third-party code), never hidden by gaming the denominator.

| Language | Benchmark repo | Coverage |
|---|---|---|
| TypeScript / JavaScript | this repo | 95.8% |
| Python | psf/requests | 100% |
| Go | gin-gonic/gin | 96.6% |
| Rust | BurntSushi/ripgrep | 86.7% |
| Java | google/gson | 93.3% |
| C# | jbogard/MediatR | 85.2% |
| PHP | guzzle/guzzle | 100% |
| Ruby | sidekiq/sidekiq | 100% |
| C | redis/redis | 92.2% |
| C++ | google/leveldb | 94.8% |
| Objective-C | SDWebImage | 91.6% |
| Swift | Alamofire | 95.3% |
| Kotlin | square/okhttp | 96.2% |
| Scala | gatling/gatling | 91.2% |
| Dart | flutter/packages | 92.4% |
| Svelte / SvelteKit | sveltejs/realworld | 100% |
| Vue / Nuxt | nuxt/movies | 93.5% |
| Astro | xingwangzhe/stalux | 93.0% |
| Lua | nvim-telescope/telescope.nvim | 84.2% |
| Luau | dphfox/Fusion | 92.2% |
| Liquid | Shopify/dawn | 73.8% |
| Pascal / Delphi | PascalCoin | 77.4% |

Framework routing is validated the same way, on a canonical app per framework: Express 100%, FastAPI 98%, Flask 100%, NestJS 96.8%, Gin 96.5%, Axum 100%, Rocket 93.8%, Vapor 100%, Laravel 92%, Rails 89.6%, React Router 100% — and the convention/reflection-heavy ones at their honest static-analysis ceiling: ASP.NET 83.9%, Spring 83.3%, Drupal 78.9%, Play 76.3%, Django 74.1%. SvelteKit, Vue/Nuxt, and Astro use file-based routing, so their page/endpoint coverage is the Svelte/SvelteKit (100%), Vue/Nuxt (93.5%), and Astro (93.0% — every `src/pages/` file maps to a route node on the two validation repos) figures in the table above.

## Troubleshooting

**"CodeGraph not initialized"** — Run `codegraph init` in your project directory first.

**Indexing is slow** — Check that `node_modules` and other large directories are excluded. Use `--quiet` to reduce output overhead.

**MCP hits `database is locked`** — current builds shouldn't: CodeGraph bundles its own Node runtime and uses Node's built-in `node:sqlite` in WAL mode, where concurrent reads never block on a writer. If you still see it:

- **You're on an old (pre-0.9) install.** Reinstall to get the bundled runtime — `curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh` (macOS/Linux), `irm https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.ps1 | iex` (Windows), or `npm i -g @colbymchenry/codegraph@latest`.
- **`codegraph status` shows `Journal:` other than `wal`** — WAL couldn't be enabled on this filesystem (common on network shares and WSL2 `/mnt`), so reads can block on writes. Move the project (with its `.codegraph/` folder) onto a local disk.

**MCP server not connecting** — Your agent starts the server itself, so you don't launch it by hand. Make sure the project is initialized and indexed (`codegraph status`) and that the path in your MCP config is correct. If it still won't connect, re-run `codegraph install` to rewrite the config.

**Two `codegraph serve --mcp` on one project fight over the index / auto-sync stops** — CodeGraph allows one live MCP *writer* per project (the shared background daemon, or a single direct-mode process). Extra clients should proxy to that daemon. If you set `CODEGRAPH_NO_DAEMON=1`, run only one `serve --mcp` for that project; a second instance exits with a clear writer-lock error (see `writer.pid` under `.codegraph/`; its persistent `writer.pid.mutation.lock` coordinates ownership changes and needs no stale-lock deletion). Prefer leaving the daemon enabled so multiple MCP hosts share one watcher.

**MCP tool calls fail with `Transport closed` while `codegraph status`/`sync` are healthy** — almost always WSL2 with the project on a Windows drive (a `/mnt/c` or `/mnt/d` path), where the local socket CodeGraph uses to share one background server across sessions is unreliable. CodeGraph now falls back to serving the session in-process instead of dropping the connection, and keeps trying to reach the shared server in the background: first after 5 seconds, backing off to every 5 minutes. `CODEGRAPH_DAEMON_RETRY_MS` and `CODEGRAPH_DAEMON_RETRY_MAX_MS` set those two delays, and `CODEGRAPH_DAEMON_RETRY_MS=0` stops retrying. If you still hit it, set `CODEGRAPH_NO_DAEMON=1` in your MCP server's environment to skip the shared server entirely (each session runs in its own process). Moving the project onto the Linux-native filesystem (e.g. under `~/` instead of `/mnt/`) restores the shared server.

**Missing symbols** — The MCP server auto-syncs on save (wait a couple seconds). Run `codegraph sync` manually if needed. Check that the file's language is supported and isn't inside a `.gitignore`d or default-excluded directory (e.g. `node_modules`, `dist`).

**Sharing one checkout between Windows and WSL** — Don't point both at the same `.codegraph/`: the background-server lock and the SQLite index are tied to the OS that wrote them, and SQLite locking across the WSL2/Windows filesystem boundary is unreliable (WSL reports it as a `disk I/O error`). For a project on a Windows drive (a `/mnt/c/…` path), WSL keeps its own index automatically: an index first built from WSL goes in `.codegraph-wsl/`, leaving `.codegraph/` to Windows. An index already in `.codegraph/` stays where it is, so if Windows built that one, give WSL its own by setting `CODEGRAPH_DIR=.codegraph-wsl` in WSL and running `codegraph init` there. `CODEGRAPH_DIR` always picks the name when set, on either side. CodeGraph skips any sibling `.codegraph-*` directory when indexing and watching, so the two never trip over each other.

**Very large repositories (hundreds of thousands of files), or a large `.codegraph/codegraph.db-wal` file** — The `-wal` file is SQLite's write-ahead log: writes waiting to be folded into `codegraph.db`. While a big index is being built, CodeGraph lets it grow in proportion to the index (soft threshold = the larger of 256 MB and a quarter of the index size, up to 2 GB) before folding it back, because folding too often is what made large indexes slow on ordinary disks. At rest it is trimmed to 64 MB, and a leftover from a killed session is folded and trimmed the next time the project opens — the index itself has no size limit. Two environment variables tune this: `CODEGRAPH_WAL_VALVE_MB` (the soft threshold during indexing) and `CODEGRAPH_WAL_HEAL_MB` (the resting size and the trim threshold). `CODEGRAPH_WAL_VALVE_DEBUG=1` prints every decision to stderr.

## License

MIT

---

<div align="center">

**Made for AI coding agents — Claude Code, Cursor, Codex CLI, opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro, and GitHub Copilot**

[Report Bug](https://github.com/colbymchenry/codegraph/issues) · [Request Feature](https://github.com/colbymchenry/codegraph/issues)

</div>
