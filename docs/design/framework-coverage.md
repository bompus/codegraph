# Framework & language coverage — what is done, what is left

**Last verified: 2026-08-29** (Angular row: 2026-09-29) against the build at that date. Re-verify with the
queries in [Checking this file is still true](#checking-this-file-is-still-true)
before trusting a row; this is a snapshot, not a live view.

This file exists to be read cold. It says, for every framework and language the
README claims, **which of the three pictures it can draw today** and what is
missing from the ones it cannot — so a fresh session can pick up the next piece
without re-deriving the map.

---

## The three axes

A framework's support is not one thing. Three separate facts in the graph
unlock three different pictures, and a framework can have any subset:

| Fact in the graph | Unlocks | Produced by |
|---|---|---|
| **`route` nodes** bound to a handler or component | the **Entry points** tab; an endpoint or page can be a Steps anchor | a framework resolver's `extract()` |
| **`navigates` edges** from the code that sends a user somewhere to the route it names | the **Screens** tab — without a single one, `buildScreens` returns `routed: false` and the tab stays hidden | a resolver's `resolve()` (calls) + a synthesizer (markup) |
| **branch-guard rules** for the language | the `WHEN` label on every arrow, in Steps, Screens and `codegraph_explore`'s Flow section | `src/graph/branch-guards.ts` |

The Screens picture is a pure function of the first two: *any* framework that
produces route nodes and `navigates` edges lands on the tab, with no view code
to write. That is why "add a router" is a small, self-contained job.

---

## Routers — routes AND navigation (done)

Eight, counting the fork's Astro resolver. Each reads a literal destination
and leaves a computed one, a path no route serves, and a conditional whose
arms disagree unresolved rather than guessed.

| Router | Resolver | Markup synthesizer | Tests | Validated on |
|---|---|---|---|---|
| Expo Router | `frameworks/expo-router.ts` | `expo-router-synthesizer.ts` | `expo-router.test.ts`, `monorepo-app-frameworks.test.ts`, `monorepo-app-frameworks-sync.test.ts` | upstream: evanbacon.dev (`+api` endpoints), react-native-true-sheet (nearest manifest decides the app) |
| Next.js | `frameworks/nextjs.ts` | `next-router-synthesizer.ts` | `nextjs.test.ts` | next-saas-starter |
| React Router / Remix | `frameworks/react-router.ts` | `react-router-synthesizer.ts` | `react-router.test.ts`, `react-router-framework.test.ts`, `remix-routes.test.ts` | proshop (44 edges), proshop-v2 (28), react-redux-realworld (22), react-boilerplate (`styled(Link)`), takenote (v5 `<Redirect>`); pinned official framework config and flat filenames; bulletproof-react: nested children, lazy routes and paths.x.path constants through per-app aliases; paths.x.getHref() links and navigate calls read literal or template destinations; fork recheck: 11 navigates on bulletproof-react `9506629` (2026-10-01), all destinations checked against config and route registrations |
| TanStack Router / Start | `frameworks/tanstack-router.ts` | `tanstack-router-synthesizer.ts` | `tanstack-router.test.ts`, `tanstack-start.test.ts` | TanStack examples, fastapi-template frontend; pinned Start server-handler syntax |
| Vue Router / Nuxt | `frameworks/vue-router.ts` (Nuxt file routes: `nuxtResolver` in `frameworks/vue.ts`) | `vue-router-synthesizer.ts` | `vue-router.test.ts` | vue-realworld (23 edges); vue-element-admin (62 routes), vue-admin-template (14), vben (192), halo console (34) — named tables, module files, `children` + layouts; Nuxt: mealie, elk, nuxt/movies |
| SvelteKit | `frameworks/sveltekit-router.ts` | `sveltekit-synthesizer.ts` | `sveltekit-router.test.ts`, `sveltekit-route-names.test.ts` | sveltekit-realworld (31 edges); shadcn-svelte and skeleton (`(group)` layouts: 13 and 23 edges), svelte.dev (74), kit's test apps (47) |
| Angular | `frameworks/angular-router.ts` | `angular-template-synthesizer.ts` | `angular-router.test.ts`, `angular-routes.test.ts` | angular-realworld (31 edges, 18 renders), Ghostfolio (189 edges, 170 renders), ngx-admin (routes and renders; its menus are config), angular-spotify (Nx libs behind barrels: 14 routes), jira-clone (class-constant paths, mount-only redirects), jhipster (60), ionic-conference (18), Angular-JumpStart (18) |
| Astro | `frameworks/astro.ts` | — | `astro-routes.test.ts` | pinned endpoint fixture; exact page components and source navigation |
| RedwoodSDK | `frameworks/redwood.ts` | — | `redwood-routes.test.ts` | pinned 1.7.3 typed-routes worker; exact page and API roots |
| Analog | `frameworks/analog.ts` | — | `analog-routes.test.ts` | pinned 2.7.1 sign-up page; filename/layout and registration sync controls |
| Solid Router | `frameworks/solid-router.ts` | — | `solid-router.test.ts` | pinned 0.16.3 README lazy example; exact component roots, nested paths and fresh workers |
| SolidStart | `frameworks/solid-start.ts` | — | `solid-start.test.ts` | pinned 2.0.4 About/API fixtures; page/API coexistence, file hierarchy and config sync |
| Vike | `frameworks/vike.ts` | — | `vike-routes.test.ts` | pinned 0.4.266 About/product examples; nearest route inheritance, config guards, sync and workers |
| Qwik City | `frameworks/qwik-city.ts` | — | `qwik-city.test.ts` | pinned 1.20.0 page/API sources; anonymous component roots, body-call ownership, sync and workers |
| Waku | `frameworks/waku.ts` | — | `waku-routes.test.ts`, `waku-programmatic.test.ts` | pinned 1.0.0-rc.0 home/Cloudflare/createPages fixtures; exact roots, staticPaths, config sync and workers |

Waku programmatic coverage recognizes default/Cloudflare adapter registration of `createPages` (imported from `waku` or `waku/router/server`) in the default server module, including a local `const` binding. Direct calls to its injected `createPage` helper (including aliases) are read from synchronous/async callbacks without executing them. Literal paths, render modes, static paths and `exactPath` are supported; roots bind exact named local functions or directly imported named/default function exports. The [official typegen fixture](https://github.com/wakujs/waku/blob/9f425e94996e018983cb629709544397b5f1e93b/packages/waku/tests/fixtures/plugin-fs-router-typegen-with-createpages/waku.server.tsx) supplies the registration pattern. Conditional calls, nested helpers, arbitrary registration wrappers, computed options, component wrappers, re-exports and anonymous imported targets remain unsupported. `createLayout`/`createRoot`/`createSlice`/`createApi` do not become pages; `unstable_skipBuild` does not remove runtime routes.

Waku filesystem coverage targets **1.0.0-rc.0**, with default `src/pages` discovery or a literal default/Cloudflare adapter around `fsRouter(import.meta.glob('./pages/**/*.{tsx,ts}'))` (default extension subsets are honored). [Official home](https://github.com/wakujs/waku/blob/9f425e94996e018983cb629709544397b5f1e93b/e2e/fixtures/fs-router-build-split/src/pages/index.tsx), [Cloudflare entry](https://github.com/wakujs/waku/blob/9f425e94996e018983cb629709544397b5f1e93b/e2e/fixtures/cloudflare-adapter/src/waku.server.tsx). Local default functions, including anonymous functions, bind exact component roots. Pages default to static rendering; literal `getConfig` return objects may set `render` and `staticPaths`, with static parameters expanded only for declared values. Dynamic parameters, mixed slug segments, terminal catchalls and groups are supported. `_root`, `_layout`, top-level `_slices`/`_interceptors`/`_api`, and `_actions`/`_components`/`_hooks` directories are excluded. Custom roots/globs/options, component/path overrides, unknown config, optional parameters, wrappers and re-exports are unsupported; no navigation is inferred. A `waku.config` that only adds Vite plugins, as the official templates do, keeps discovery on; `srcDir`, `basePath` or any other Vite option turns it off.

Vike recognizes local named default or `Page` ES-module exports in JS/TS `+Page` files when a default-root Vite config registers option-free `vike()`. Filesystem URLs remove complete `pages`, `src`, `index`, `renderer` and group segments; configuration inheritance removes only `pages`/`renderer`. The nearest inherited literal `+route` wins and `@` parameters become named graph parameters. [About source](https://github.com/vikejs/vike/blob/715c15d11be196caa69159b929ae09697f3ce734/examples/react-minimal/pages/about/%2BPage.tsx), [product override](https://github.com/vikejs/vike/blob/715c15d11be196caa69159b929ae09697f3ce734/examples/file-structure-domain-driven/product/pages/index/%2Broute.js). Canonical `extends: vikeReact` and literal arrays from `vike-react/config` are accepted; its [0.6.26 config](https://github.com/vikejs/vike-react/blob/b51cdccaba8cda8cb951f66edc61e359f77ae239/packages/vike-react/src/config.ts) affects rendering rather than paths. Dynamic/ambiguous route overrides, configured Page/root overrides, unknown extensions and metadata do not receive guessed paths; `onBeforeRoute` blocks app-wide inference. Custom roots/plugin options, inherited Page-only targets, anonymous/wrapped/re-exported components and non-JS/TS template files are unsupported. No navigation is inferred.

Qwik City recognizes default `src/routes/**/index.{js,jsx,ts,tsx}` with option-free `qwikCity()` in literal Vite configuration, including direct-return synchronous/async config callbacks. Pages additionally require imported `QwikCityProvider`/`RouterOutlet` in the default root component. Named functions, named `component$` bindings and anonymous default `component$` calls have exact roots; anonymous component symbols own only their callback's existing file-level references. [Official page](https://github.com/QwikDev/qwik/blob/971465f941e44e5adf2b2c2e44566b590d0990d8/starters/apps/qwikcity-test/src/routes/issue2441/abc.page/index.tsx), [API fixture](https://github.com/QwikDev/qwik/blob/971465f941e44e5adf2b2c2e44566b590d0990d8/packages/docs/src/routes/demo/qwikcity/middleware/json/index.tsx). Groups, legacy `__` directories, dynamic/mixed parameters, catchalls and default trailing slashes follow 1.20.0. Index method exports create endpoints without implicit HEAD. Layout methods and `onRequest` remain middleware, not independent endpoints. Custom roots/options, route rewrites, layout-override index names, optional parameters, Markdown/MDX, re-exports and arbitrary component wrappers are unsupported. No navigation is inferred.

SolidStart supports default `src/routes/**/*.{js,jsx,ts,tsx}` with option-free `solidStart()` in a literal Vite config. Page discovery additionally requires imported `FileRoutes` directly inside the imported `Router` in the default app component. Named local default functions and method exports bind exact targets. Raw file hierarchy determines page layouts before route groups are removed; API-only descendants do not turn pages into layouts. Dots stay literal, optional page parameters and named catchalls are preserved, and GET supplies HEAD unless explicitly exported. [Official page](https://github.com/solidjs/solid-start/blob/5d23efbcbb47997a70978be8b0e468df50d774a8/apps/fixtures/basic/src/routes/about.tsx), [API fixture](https://github.com/solidjs/solid-start/blob/5d23efbcbb47997a70978be8b0e468df50d774a8/apps/fixtures/experiments/src/routes/api/hello/%5Bname%5D.ts), [route construction](https://github.com/solidjs/solid-start/blob/5d23efbcbb47997a70978be8b0e468df50d774a8/packages/start/src/config/fs-router.ts). Version 2.0.4 excludes OPTIONS-only APIs and rejects optional API parameters. Custom roots, plugin options, dynamic configuration, page `route` overrides, non-default routers, anonymous/re-exported handlers and Markdown are unsupported. No navigation is inferred.

Solid Router reads imported `Router`/`Route` JSX and registered literal or module-constant config trees. It composes bases and nested paths, removes parent splats, and emits only leaf pages. Components bind through imports or same-file declarations; static `solid-js` lazy imports bind named default exports. [Pinned example](https://github.com/solidjs/solid-router/blob/e8d3a7f719020ef01f8879a0110d2123b8597caa/README.md), [path composition](https://github.com/solidjs/solid-router/blob/e8d3a7f719020ef01f8879a0110d2123b8597caa/src/utils.ts). Tests cover shadowed imports, mutated tables, exact targets, edit/delete sync and fresh workers.

Route tables in their own file are read when they are exported top-level consts typed `RouteDefinition[]` (annotation, `satisfies` or `as`, with `RouteDefinition` imported from `@solidjs/router`), as in the [official template](https://github.com/solidjs/templates/blob/43885617/vanilla/with-solid-router/src/routes.ts). Their routes carry a `solid-table:` marker in `qualifiedName`. The resolver's `postExtract` follows each import of the table to where it is registered: a `<Router base>` child, a `<Route path>` child, or `children: table` in a route object, through `[...table]` array spreads, local const tables and tables that are themselves registered from another file. Uses of a name an inner parameter or declaration shadows are not registrations. A table nested in another exported table of its own file is read through that table only. One distinct prefix renames the table's routes; none, several, a non-literal path, or props spread onto the registering element leave the paths as the table declares them. Deleting the registering file restores them on the next sync. Lazy components also accept `@/`, `~/` and `#/` sources, which bind when the project's path aliases resolve them, and an `async () => await import()` callback. Validation (routes before → after; every new route resolves to its page component):

| Repository | Pinned | Routes | Shape |
|---|---|---|---|
| solidjs/solid-site | 0028cfd5 | 0 → 14 | `routes.ts` table registered by `<Router>{routes}</Router>` in `App.tsx` |
| weblink | d51f9f72 | 3 → 6 | `satisfies RouteDefinition[]` default export; `@/routes/...` lazy aliases |
| CatColab | 8f94f126 | 10 → 14 | `help/routes.ts` nested under `path: "/help"` in `App.tsx`'s local table |
| Pentive | 91b50ee4 | 7 → 15 | `lazy(async () => await import(...))`; registered through a service container, so no prefix |

Explore probes that name the route path (`/blog/:slug`, `/help/guides/:id`, `/share`) return the page component; question-phrased probes can miss it, a ranking gap that is not specific to Solid. CatColab's `lazyMdx(...)` pages stay unlinked, since the wrapper is project-specific.

Other router variants, untyped cross-file arrays, re-exported tables, local function config bindings, dynamic declarations, object spreads inside route entries, inline/anonymous components and lazy re-exports remain unsupported. No navigation is inferred.

Analog recognizes `src/app/pages/**/*.page.ts` named default classes when a root Vite config registers the platform plugin and source registers option-free `provideFileRouter()`. Directory hierarchy determines layouts before dots become URL separators; index/pathless names, parameters and catchalls follow the pinned conventions. [Official page fixture](https://github.com/analogjs/analog/blob/0896a7eaaa2acf26443ca184bc1dd9aa1a06f4d6/apps/analog-app/src/app/pages/%28auth%29/sign-up.page.ts), [route construction](https://github.com/analogjs/analog/blob/0896a7eaaa2acf26443ca184bc1dd9aa1a06f4d6/packages/router/src/lib/routes.ts). Fresh workers and config/file add/edit/delete refresh existing pages, including after reopening. Custom roots, extra route directories, `app/routes`, metadata overrides, router options, optional catchalls, Markdown, anonymous defaults and re-exports remain unsupported. No navigation is inferred. The Vite config may be an object or a `defineConfig` callback that directly returns one, as the create-analog templates write it; `prerender.routes` is allowed.

RedwoodSDK follows imported `defineApp` registrations with literal/local-constant arrays, `route`, `index`, `render`, `layout` and `prefix`. Handler arrays bind only their last handler. Standard method tables remain method-qualified; ordinary handlers remain `ANY` unless the handler returns JSX. [Pinned worker fixture](https://github.com/redwoodjs/sdk/blob/39da7118f712bd86450e493cb2c213815b1893bb/playground/typed-routes/src/worker.tsx), [router semantics](https://github.com/redwoodjs/sdk/blob/39da7118f712bd86450e493cb2c213815b1893bb/sdk/src/runtime/lib/router.ts#L682). Tests cover handler classification changes/deletion and fresh workers. Cross-file route arrays, custom methods, mutated builders, dynamic paths, duplicate/computed/spread method tables and wrapped/anonymous exported components remain unsupported; no navigation is inferred.

Remix default `app/routes/` conventions and React Router configs registering an imported, option-free `flatRoutes()` call support JS/TS pages and immediate `folder/route` modules. [Pinned filename parser](https://github.com/remix-run/react-router/blob/7aea711dd1ae2bc5a076d13ff17291829690fa74/packages/react-router-fs-routes/flatRoutes.ts#L351): dot nesting, index/pathless segments, parameters, optional segments, splats and bracket escapes. Resource-only and direct `Outlet`-only defaults are excluded. Config-only full/scoped sync and reopening an index refresh existing pages. Custom configuration, folder `index` fallback, Markdown/MDX, anonymous defaults and re-exports remain unsupported.

React Router framework mode reads default exported literal arrays in `app/routes.ts` or `app/routes.js`, using imported `route`, `index`, `layout`, and spread `prefix` helpers. Module paths bind named default components; nested index pages take precedence over their parent. [Official source fixture](https://github.com/remix-run/react-router/blob/7aea711dd1ae2bc5a076d13ff17291829690fa74/docs/start/framework/routing.md#L28): seven expected pages, verified through indexing and navigation. Tests also cover module/config sync and a fresh compiled process using parse/resolver workers. Custom app directories, computed arrays, `relative`, anonymous defaults, and re-exports remain unsupported.

TanStack Start reads imported `createFileRoute` calls assigned to exported `const Route`: literal `server.handlers` tables, including `ANY`, and the destructured `createHandlers` callback form. Named handlers and direct inline calls bind through the existing HTTP reader. Page/API combinations retain both nodes; server-only routes do not become pages. [Pinned official handler syntax](https://github.com/TanStack/router/blob/a58e01c604e2d189ef8c8c1ad6ac8747e03aa88c/docs/start/framework/react/guide/server-routes.md#L172), [executable middleware fixture](https://github.com/TanStack/router/blob/a58e01c604e2d189ef8c8c1ad6ac8747e03aa88c/e2e/react-start/server-routes/src/routes/api/middleware-context.ts). Computed/spread tables, member handlers, custom factories and server `update` chains remain unresolved. `createServerFn` has no declared public route and gets no fabricated endpoint.

Shared machinery all eight use, in `frameworks/expo-router.ts`: `RouteTable` /
`RootedRouteTable`, `routesForFile`, `addRouteTo`, `matchRoute`, `appRootFor`,
`parseHrefExpression`, `readHrefViaLocal`, `nthArgumentText`, `readStringAt`,
`toHref`. Plus `pageForHref` in `frameworks/nextjs.ts` (framework-agnostic
despite where it lives) and the object-literal walker in
`frameworks/object-literal.ts`.

Angular is the one whose markup is not indexed: a component's template is a
`templateUrl` file (or an inline `template:` string) read at synthesis time,
which also yields the component tree (`<app-foo>` by element selector) — the
edge a navigation in a child component rides to its screen. A `routerLink:`
field written in a component's class (a tab bar's or a menu's config, bound
in a loop elsewhere) counts as a link from that component. An event binding
(`(click)="save()"`) is a `calls` edge from the component to its own method
carrying `metadata.trigger`, which Steps uses in place of reading a trigger
at the edge's line (the binding is in the template, not the source there). A route with
`children` is a layout: its component carries a `references` edge marked
`layout: true` from each screen nested in it, and `routeLayouts` in
`route-roots.ts` gives Screens every screen a layout serves. Known limits: a
route with a custom `matcher` has no static address, a relative navigation
(`relativeTo`) is left unresolved, and an edit to a template file alone is
picked up at the next sync of any source file (templates are not watched).
A routes array is read where it is declared, so an untyped array in a file
that does not import `@angular/router` is not one, even when another file
hands it to `provideRouter`. The fork adds to upstream's reader: `provideRouter`
and `RouterModule` imported under an alias (`$`-prefixed ones included) still
open a routes array, and an entry with a named `outlet` (`/(side:x)`, not a
path) or a `...spread` (fields the scan cannot see) names no screen.
`angular-routes.test.ts` covers those, sync after reopening, and fresh parse
workers. Since that verification, a lazy NgModule's routes are also found
through a routing module in the module's own directory, a barrel in front of
it, or the routes file it hands `forChild`. `angular-router.test.ts` covers
that on fixtures; no corpus has been re-checked, so the date above stands.

---

## Static JavaScript HTTP declarations

Added 2026-09-06 in `frameworks/http-routing.ts`: Hono, Elysia, Fastify,
Hyper-Express, Koa router, H3, Bun, Effect v4 and option-free Vixeny builders.
These produce method-qualified endpoint nodes, named-handler references and
direct calls from inline handlers. They do not add Screens navigation.
`http-routing.test.ts` covers all nine through full indexing and imported-handler
resolution, as well as false-positive controls, prefixes and same-file mounts.

Nuxt file routing in `frameworks/vue.ts` includes root index pages, `index`
folders anywhere in the path, Nuxt 4 route groups, server method suffixes,
`server/routes/` and server catch-all segments. Each page route calls the component
extracted from the same file. `page-component.ts` resolves that exact file's
component, including scriptless pages, without choosing a same-named component
elsewhere. Astro pages use the same call contract. `nuxt-routes.test.ts` checks
extraction and imported-handler resolution; the existing Next Pages/App Router
and Vue navigation tests remain controls. The older coverage rows above retain
their recorded measurements.

Untouched official source checks (routes and indexing, no application execution):

| Framework | Pinned source | Scope |
|---|---|---|
| Hono | [examples basic](https://github.com/honojs/examples/blob/3b0b62875a0e1265763fea1c6388866d5697ef81/basic/src/index.ts) | 16 registrations, including 3 mounted paths |
| Fastify | [winston logger](https://github.com/fastify/example/blob/d3032da0b307afa8749e967aa0dbdf239c348341/winston-logger/winston-logger.js) | `GET /hello` |
| Elysia | [CORS example](https://github.com/elysiajs/elysia-cors/blob/58adc6030a3c790e2494e2e8bd45dd7938b9b024/example/index.ts) | `POST /` |
| H3 | [router example](https://github.com/h3js/h3/blob/a5fdc86a6075506d71510aa5208739aa0b2bec29/examples/router.mjs) | 6 explicit methods at `/` |
| Bun | [serve route tests](https://github.com/oven-sh/bun/blob/d316760e8cae0d69ae927898d5afc933ecf34671/test/js/bun/http/bun-serve-routes.test.ts) | Extraction of 9 declarations starting in lines 1–135 |
| Effect v4 | [HTTP server tests](https://github.com/Effect-TS/effect-smol/blob/3a1128c7684e04d34d9f541f77adaac38a513056/packages/platform-node/test/NodeHttpServer.test.ts) | Extraction of 3 declarations starting in lines 1–90 |

These are bounded fixtures, not whole-framework recall measurements. Hyper-Express,
Koa and Vixeny have synthetic indexing tests only: the inspected official fixtures
import relative framework source, which the package-provenance reader deliberately
does not infer. Dynamic paths, cross-file mounts, runtime mutation and plugin
factories remain outside coverage, except for Fastify plugin files.

Fastify plugin files (added 2026-09-28). A default-exported function
(`export default`, `module.exports =`, or a top-level `const` or function
declaration either one names) is a plugin when its first parameter is
`fastify` or the file names a Fastify package or `FastifyInstance`/`FastifyPlugin*`
type. Routes declared on that parameter carry a `fastify-plugin:` marker in
`qualifiedName`; `postExtract` then applies literal `register(AutoLoad, { dir })`
registrations, following @fastify/autoload 6.5.0
([source](https://github.com/fastify/fastify-autoload/blob/fcfe8a2d0382c1cbd90a16b4e18239dece9b4aa7/index.js)):
directory prefixes, index files hiding their siblings, autohooks files, the
default ignore of dot-named entries,
literal `options.prefix`, `routeParams`, `appendAutoPrefix`,
`dirNameRoutePrefix: false` and literal `autoPrefix`, `prefixOverride`,
`autoConfig.prefix` and `autoload = false` exports. Options forwarded from the
caller (`opts`, `{ ...opts }`) are taken to carry no prefix. `fastify-plugin`
wrapped exports, route-object exports, filter/pattern/`maxDepth`/`encapsulate`
options, a prefix on the context that registers autoload, and `dir` values
other than `join(__dirname | import.meta.dirname, 'literal'…)` are not modelled,
nor is an `export { autoPrefix }` clause; those files keep their in-file paths.
The file scan for registrations records content-only skips, so a sync re-reads
only files that changed, and deleting the registering file restores the paths. Files autoload does not load keep theirs too,
since `postExtract` can rename a node but not remove it.

| Repository (pinned) | Routes before → after |
|---|---|
| [fastify/demo](https://github.com/fastify/demo/tree/5cd560125b3c2f0d42192bc7f493e8e3b9e75e52) | 1 → 15 |
| [riccardoperra/codeimage](https://github.com/riccardoperra/codeimage/tree/27b185f18d36f2baec3a8cc5a43e8794586096c3) | 4 → 16 (`/api/v1/project/:id/clone` from `options.prefix` and `routeParams`) |
| [jamcalli/Pulsarr](https://github.com/jamcalli/Pulsarr/tree/c3faac0e8cc115a66e20ea21f532e7a3e1c7beb3) | 12 → 173 (159 route calls under `src/routes`) |
| [pingcap/ossinsight](https://github.com/pingcap/ossinsight/tree/d98a10e7244cfc9e2d1da02bace1c2a9fe9d7738) | 71 → 106; 8 `GET /` routes renamed to their mounted paths |

Explore probes: a bare route name (`DELETE /v1/approval/requests/:id` on
Pulsarr) and the demo's upload question return the route file first. In
ossinsight, Next.js handlers named `GET` outrank a full route name in
unfiltered search; that ranking gap is shared by every framework's routes. Vixeny options require terminal-operation
dataflow and are omitted. See the [route guide](../../site/src/content/docs/guides/framework-routes.md)
for the supported declaration shapes and Nuxt configuration limits.

## What is left

Ordered by cost-to-value. Each row says what is missing, not merely that
something is.

### 1. Astro — custom configuration and additional page formats

Default `.astro` pages bind exact same-file components; `.ts`/`.js` exported HTTP methods bind handlers (`ALL` becomes `ANY`). Literal/bound anchor destinations and `Astro.redirect` link to local pages. External URLs, non-href attributes, type-only exports and underscore-prefixed paths are excluded. [Pinned endpoint fixture](https://github.com/withastro/astro/blob/9870f95601690d9d98799b6fa78a0bc76165ee06/packages/astro/test/fixtures/api-routes/src/pages/binary.dat.ts), [extension rules](https://github.com/withastro/astro/blob/9870f95601690d9d98799b6fa78a0bc76165ee06/packages/astro/src/core/routing/create-manifest.ts#L140).

Remaining: custom roots/base/config redirects, Markdown/MDX pages, cross-file re-exports, client transition calls, and navigation to rest routes. These are not inferred from default file conventions.

### 2. Server-rendered frameworks — a redirect is a transition, not just a response

**Fourteen frameworks** have route nodes and no navigation: Django, Flask,
FastAPI, Express, NestJS, Laravel, Drupal, Rails, Spring, Play, Gin/chi/gorilla,
Axum/actix/Rocket, ASP.NET, Vapor.

Route reading for three of them was widened on 2026-09-30 (upstream #2154 to
#2156): Flask reads `add_url_rule(…)` and a project helper that hands a list of
paths and a `view_func=` over (`__tests__/flask-url-rules.test.ts`; flaskbb 0
→ 103 routes); Rails reads `routes.rb` as nested blocks, so `namespace`,
`scope`, nested `resources` and `member` / `collection` add their paths and
controller modules, and a namespaced `controller#action` reaches its own
module's controller (`__tests__/rails-scoped-routes.test.ts`; solidus 0 →
319 route links); Play detects a `conf/routes` in any subdirectory
(`__tests__/play-nested-projects.test.ts`; play-samples 0 → 139 routes). The
numbers are upstream's measurements. None of these adds navigation.

Be precise about what is missing. `redirect_to`, `HttpResponseRedirect`,
`res.redirect`, PHP's `redirect()` are **already recognised as `response`
effects** (`ui-server/api/effects.ts`), so they draw as a box in the Steps
picture. What is missing is the edge to the page they name — so two pages never
connect on the Screens tab.

For a pure API this is correct and nothing should change: an endpoint is not a
screen. It matters for the **server-rendered** half, where a classic MVC app
gets no Screens picture at all today:

| Framework | The destination to read | Why it is harder than a client router |
|---|---|---|
| Rails | `redirect_to :dashboard`, `redirect_to users_path` | destinations are named helpers (`*_path`/`*_url`) generated from `routes.rb`, not literals |
| Django | `redirect('profile')`, `reverse('profile')` | same — a route *name*, like Vue's `{ name }`, which `vue-router.ts` already shows how to index |
| Laravel | `redirect()->route('home')`, `->view()` | route names again |
| Spring | `"redirect:/x"`, `RedirectView` | a literal inside a string return value |
| ASP.NET | `RedirectToAction("Index", "Home")` | controller + action pair, not a path — needs the route table's reverse mapping |
| Flask | `redirect(url_for('profile'))` | nested call; the name is `url_for`'s argument |

The Vue name-index (`VueAppRoutes.byName`) is the closest existing precedent for
all of these.

### 3. Native UI — no route nodes at all

| Platform | Routes would come from | Navigation would come from |
|---|---|---|
| SwiftUI | `NavigationStack(path:)`, `.navigationDestination(for:)` | `NavigationLink(value:)`, `path.append(…)` |
| Jetpack Compose | `NavHost { composable("route") { … } }` | `navController.navigate("route")` |
| Flutter / Dart | `MaterialApp(routes: {…})`, `GoRouter([...])` | `Navigator.push`, `context.go('/x')` |

All three are named in `scripts/try-repo.sh`'s presets as not modelled
(`icecubes`, `nowinandroid`). Compose and go_router are the most tractable —
both name routes with string literals, which is the same shape every router
above reads.

### 4. ArkTS / HarmonyOS — closest to done of anything here

**Has:** the hard half already. `arkuiRouterEdges` in
`callback-synthesizer.ts` resolves `router.pushUrl('/pages/Detail')` to the
target page struct.
**Missing:** it emits a **`calls`** edge and no `route` node, so it never
reaches the Screens tab.
**Size:** an edge-kind change plus route nodes for `pages/` entries — no new
analysis.

---

## Languages

All ~30 languages in the README have full structural extraction; nothing is
outstanding on that axis. The gap that is language-shaped is the **`WHEN`
label**.

**Guard rules exist** (`RULES_BY_LANGUAGE` in `src/graph/branch-guards.ts`) for:
TypeScript, TSX, JavaScript, JSX, Swift, Python, Java, Kotlin, C#, Go, C, C++,
Objective-C.

(Metal and CUDA parse **as** C++ and ArkTS does **not** parse as TypeScript, so
the first two inherit the C rules and the third has none.)

**No rules** — boxes draw, arrows carry no condition, and no arguments or
trigger labels are read: PHP, Ruby, Rust, Scala, Dart, Erlang, Lua, Luau, R,
Solidity, COBOL, CFML, VB.NET, Nix, Terraform, Pascal/Delphi, Liquid, Razor,
Twig, ArkTS, and the `.svelte` / `.vue` / `.astro` template languages.

A language with no rules yields **nothing**, never a wrong label — that is the
design, so an absent row here is a missing feature, not a bug.

**Ruby and Rust sting most**: both have server frameworks in the README's table
(Rails, Axum/actix/Rocket), so their Steps pictures draw responses and database
calls with no conditions on any arrow. `scripts/try-repo.sh`'s `bookstack`
preset says exactly this for PHP.

---

## Traps a new router will hit

Each of these cost real debugging time; they are not hypothetical.

1. **A `references` edge cannot cross a language family.** The kernel's
   language gate (`apply_language_gate` in `codegraph-kernel/src/resolve/names.rs`) filters
   `references` candidates to
   `sameLanguageFamily`, so a `.js` router config can never name a `.vue`
   component — it silently binds to a same-named `.js` function in a store
   instead. Bind a route to its component with **`calls`**, which
   `route-roots.ts` reads as "the page a screen file exports".
2. **One address, one screen.** A layout and the index route beside it resolve
   to the same path (`+layout.svelte` vs `+page.svelte`, `dashboard.route.tsx`
   vs `dashboard.index.tsx`, `_auth.invoices.tsx` vs `_auth.invoices.index.tsx`).
   Emitting both puts one address on the map twice. Decide **per file** — the
   sibling is not visible at extraction time.
3. **The route table must be per app.** A repository with two apps has two `/`
   and two `/login`; a global table hands the address to whichever was indexed
   first. Measured at **82% of navigations pointing into a different app** on a
   477-app monorepo before `RootedRouteTable` / `routesForFile`. The `roots`
   list decides only *whether* to resolve.
4. **Read fields from the object, not from a window around one.** A Vue route's
   `name` is written above its `path`, so a text window handed every entry its
   predecessor's name — silently, for every route in the file. Use
   `frameworks/object-literal.ts`.
   A lazy view binds by the FILE it imports, never by the import's last
   segment: vue-element-admin's views are all `…/index.vue`.
   Nuxt's `pages/` convention belongs to a Nuxt app only — a plain Vue app's
   `pages/` folder (halo's console) holds components a router config names.
5. **A receiver is required for a generic verb.** `push` and `replace` are two
   of the most common method names in JavaScript; claiming a bare one puts every
   `paths.push('/tmp/x')` one string-match away from a route.
6. **One component can be several screens.** A listing rendered at `/`,
   `/search/:keyword` and `/page/:n` is one component and three addresses;
   `screenOfComponent` maps a component to **every** route it serves, and
   `collapseSharedChrome` counts distinct screen COMPONENTS, not addresses —
   counting addresses collapsed one component's four routes into an origin and
   took the navigation away from all of them.
7. **A destination can name several routes.** `parseHrefExpression` returns
   one `HrefLiteral` carrying `alternates`, and `destinationsForHref` turns it
   into one `{ node, href }` per arm. A synthesizer emits an edge apiece; a
   resolver puts the first on the `ResolvedRef` and the rest in `alsoTargets`,
   which `createEdges` fans out — the reference still resolves ONCE, so the
   pipeline's cleanup and counts are untouched. Label each edge with the arm
   that named it, or an edge points at one route while naming another's path.
8. **Read markup with the same reader as calls.** A synthesizer that peeks at
   the first character of `to={…}` misses every conditional and template the
   `push(…)` path handles. Use `parseHrefExpression` on the balanced brace
   contents.
9. **A condition is read the same way for markup as for a call.** The Screens
   walk used to skip the `when` on any synthesized edge, so every
   `<Link to>` read as *always* while the `push()` beside it carried its guard.
   The reader works fine at a markup site — a JSX `{step1 ? <Link/> : …}` is a
   ternary like any other — and the site's own verb (`link`, `a`) is the honest
   label; `return` belongs only to an edge whose destination came from
   elsewhere, which is what `registeredAt` pointing at another line means.
10. **A route is not always a screen.** The Screens picture is about
   navigation, so it draws only routes named by a path; a route named with the
   HTTP method that reaches it is an endpoint and belongs on Entry points. Nuxt
   is the exception that names an endpoint like a page (`/api/users` from
   `server/api/`), and is excluded by file path.
11. **Detection runs before any file is indexed.** `declaredDependencies` caches
   per file-count for exactly this reason — an earlier version cached the empty
   pre-index answer and every framework whose dependency lived one directory
   down stayed undetected.

---

## The bar for calling one done

Per `CLAUDE.md`'s validation methodology, and what was actually done for the
four routers added on 2026-08-29:

1. **A real repo, not only a fixture.** Every defect in this session's work was
   caught by a real repository and none by the fixture written first.
2. **Recall against ground truth.** `grep` every navigation site in the source
   and account for each one: resolved, or correctly unresolved because it is
   computed.
3. **Precision, site by site.** For every synthesized edge, read the line its
   `registeredAt` names and confirm the tag or call there names that
   destination. Target: zero false positives.
4. **Controls re-indexed.** Node, edge, route and `navigates` counts on repos
   the change should not touch — a change to shared machinery is not done until
   they are byte-identical or the difference is explained.
5. **Full suite green**, and a CHANGELOG entry in the user-facing voice.

---

## Checking this file is still true

```bash
# Which frameworks emit navigates edges
grep -rn "edgeKind: 'navigates'\|kind: 'navigates'" src --include="*.ts" | sed 's|:.*||' | sort -u

# Which languages have branch-guard rules
sed -n "/^const RULES_BY_LANGUAGE/,/^\]);/p" src/graph/branch-guards.ts

# Whether a repo's Screens tab is on, and how many transitions it has
scripts/try-repo.sh <preset>        # prints the navigation count and says which tab is on
```

```sql
-- In a repo's .codegraph/codegraph.db
select count(*) from edges where kind='navigates';
select name, file_path from nodes where kind='route' order by name;   -- duplicates = a layout drawn as a screen
```

## Name heuristics and inline handlers

Framework name heuristics share the native resolver's lexical and cross-file visibility checks. They prefer the calling file and applicable package/directory, and refuse unrelated function-local declarations, nested types and test-suite symbols from production code. Rust heuristics also honor module/import reachability. React and Express naming conventions require imports to reach another file. NestJS provider lookup also supports convention siblings in the same directory. Vue/React conventions accept only script references; Astro component-name conventions accept only `.astro` references. Inline HTTP handler references retain simple dotted receivers, including optional access; calls on arbitrary returned expressions remain unresolved.

Interface dispatch synthesis links supported base methods to concrete overrides. JavaScript and TypeScript constructors are excluded because construction does not dispatch to descendant constructors.

### Express inline handler bodies

Express uses the last registration argument as the handler, excluding a trailing comma. Inline arrows and function expressions contribute their body calls, including handlers inside wrapper calls and chained `router.route(path)` registrations. An earlier inline middleware does not replace a named final handler. Regression coverage is in `__tests__/express-inline-function-handler.test.ts`.


### Single-file component ownership

`extraction/sfc-script.ts` folds Vue, Svelte and Astro script results into one
whole-file node containing the component. Top-level script members have the
component as their only parent; nested symbols retain their script parent. Vue
`<script setup>`, Svelte instance scripts and Astro frontmatter
assign top-level calls and references, including constant initializers, to the
component. Imports, module-level execution and Astro browser scripts remain with the file. Svelte
recognizes `context="module"` and `<script module>`. The native embedded-block
seam preserves literal metadata and rebases binding scopes and lines to full-file
positions. `sfc-component-owns-script.test.ts` covers hierarchy and execution
ownership.

### Rails resource action filters

`frameworks/ruby.ts` reads literal `only:` and `except:` action filters for both
`resources` and `resource`. Supported forms are symbol/string arrays, a single
symbol or string, `%i`/`%w` lists with brackets or parentheses, and hash-rocket
options such as `:only => [:index, :show]`. These filters restrict which REST
actions receive routes; namespaces, scopes, nested resources and member/collection
blocks keep their existing path and controller rules.

### Laravel string controller handlers

Laravel reads string `Class@action` handlers and string resource controllers,
including namespace paths under the controllers root. Resource options follow
the controller argument. Exact written paths distinguish same-named controller
classes; ambiguous or missing namespace targets remain unresolved.
`laravel-controller-strings.test.ts` covers these through full indexing.

### Vapor trailing closure handlers

Vapor route closures contribute direct body-call references from the route,
retaining each written call site and its argument labels. Route-call receiver
inference respects local and conditional bindings; unsupported loop, guard,
pattern and capture bindings stay unresolved instead of reusing an outer type.
Named `use:` handlers keep their existing typed
resolution. `vapor-closure-route-body.test.ts` checks closure ownership and
false-positive controls through full indexing.
