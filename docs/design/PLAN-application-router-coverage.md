Status: 1/13 — all steps approved; step 1 verified, preparing its PR

- [x] 1 React Router framework mode — seven official-fixture pages bind exact components; nested index/navigation, config/module sync and fresh compiled workers verified; build passes, 112 WASM focused/control tests pass, full native suite 4,353 pass / 46 skip; independent review clear.
- [ ] 2 TanStack Start server routes — method-qualified endpoints from literal `server.handlers` tables and documented `createHandlers` callback forms. Keep page and endpoint nodes when both exist; omit phantom pages for server-only files. Gate: shared proof, mixed page/API fixtures, middleware exclusion, and handler call edges.
- [ ] 3 Remix and React Router file conventions — support the pinned default file convention, including index, nested, parameter, pathless, and splat cases; require evidence that the convention is enabled. Gate: shared proof, explicit-config/file-route coexistence, and layout-only controls.
- [ ] 4 Astro route completion — connect existing page routes to components and API method exports, then navigation where the source declares it. Gate: shared proof, page/API distinction, underscore exclusions, and cross-file handler resolution.
- [ ] 5 RedwoodSDK — imported `rwsdk/router` declarations, method tables, and statically resolvable registration/prefix context. Gate: shared proof, `defineApp`/`render` composition, and interrupters excluded from page roots.
- [ ] 6 Angular Router — registered literal route tables, nested children, component references, and statically resolvable lazy modules/components. Gate: shared proof, router registration and nesting, plus unregistered objects and custom matcher negatives.
- [ ] 7 Analog — default file routes and page components using Analog-specific index, dot, parameter, and layout conventions. Gate: shared proof and fixtures that distinguish parent layouts from matching pages.
- [ ] 8 Solid Router — imported JSX/config declarations, nested path composition, and router base. Gate: shared proof, nested matching semantics, component links, and unrelated JSX negatives.
- [ ] 9 SolidStart — pinned-version file routes, default page exports, and HTTP-method exports. Gate: shared proof, page/API coexistence, layouts, and dynamic parameters.
- [ ] 10 Qwik City — default file routes, page components, and method-specific endpoint exports. Gate: shared proof, parameters, layouts, and `onRequest`/middleware exclusions.
- [ ] 11 Vike — default `+Page` conventions and literal `+route` overrides. Gate: shared proof, parameters, exact component links, and no fallback route when an unsupported override changes routing.
- [ ] 12 Waku filesystem routes — default pages, parameters, and layout exclusions for a pinned version. Gate: shared proof and `_root`/`_layout`/`_slices` controls.
- [ ] 13 Waku programmatic routes — literal `createPage` declarations within the documented `createPages` registration. Gate: shared proof, async registration syntax without executing it, and computed path negatives.

All 13 steps approved. Each step gets a separate stacked PR so its diff remains reviewable. Existing [PR #2](https://github.com/bompus/codegraph/pull/2) remains unchanged; the first new PR is based on its branch.

Acceptance: supported static declarations produce accurate route paths and exact component/handler links through normal indexing. Method-specific endpoints remain distinguishable from pages. Unsupported dynamic declarations produce no invented paths or links. Keep existing Next/OpenNext, Nuxt, Express, React Router, and TanStack page behavior as controls.

Non-goals: Wrangler routes, `run_worker_first`, asset fallbacks, deployment mappings, generated OpenNext workers, executing application configuration, arbitrary URL branch analysis, custom route roots, or new dependencies/schema. Markdown/MDX Astro pages are a separate extension. TanStack `createServerFn` is RPC rather than a declared public route; inspect existing function/call linking before proposing a separate change, and never fabricate its generated URL.

Implementation: extend existing framework resolvers and reuse parser, module-resolution, and route-reference machinery. Config-to-module links must use resolved paths, not matching basenames. New framework files are justified by distinct framework semantics; introduce no shared routing layer unless two implementations demonstrate the same needed operation. Route discovery belongs in extraction/resolution: the existing post-extraction hook cannot add new route nodes or references.

Shared proof for every step:

- Pin a framework version and official source fixture before implementation; record the expected routes and handler/component targets independently of extractor output.
- Add focused positive and negative assertions for aliases, shadowed bindings, computed paths, layouts/middleware, and unrelated declarations where applicable.
- Index a real fixture through the normal pipeline and verify route roots/call edges; verify add, edit, and delete through incremental sync, including newly introduced framework detection.
- Run focused tests through native and WASM paths, existing-router controls, TypeScript/build, and the full suite at each PR boundary. Format changed files and run an independent correctness/complexity review before opening the PR.
- Update the framework coverage matrix, user guide, and changelog with supported syntax and explicit limits; mark this board as each approved step lands.

Source references:

| Scope                            | Official documentation                                                                                                                                                                                                                                               |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cloudflare framework inventory   | [Full-stack applications](https://developers.cloudflare.com/workers/static-assets/routing/full-stack-application/), [single-page applications](https://developers.cloudflare.com/workers/static-assets/routing/single-page-application/)                             |
| React Router / Remix conventions | [Framework routing](https://reactrouter.com/start/framework/routing), [file routes](https://reactrouter.com/how-to/file-route-conventions)                                                                                                                           |
| TanStack Start                   | [Server routes](https://tanstack.com/start/latest/docs/framework/react/guide/server-routes), [server functions](https://tanstack.com/start/latest/docs/framework/react/guide/server-functions)                                                                       |
| Astro                            | [Routing](https://docs.astro.build/en/guides/routing/), [endpoints](https://docs.astro.build/en/guides/endpoints/)                                                                                                                                                   |
| RedwoodSDK                       | [Routing](https://docs.rwsdk.com/core/routing/)                                                                                                                                                                                                                      |
| Angular / Analog                 | [Angular route definitions](https://angular.dev/guide/routing/define-routes), [Analog routing](https://analogjs.org/docs/features/routing/overview)                                                                                                                  |
| Solid                            | [Router Route API](https://docs.solidjs.com/solid-router/reference/components/route), [configuration](https://docs.solidjs.com/solid-router/getting-started/config), [SolidStart routing](https://docs.solidjs.com/solid-start/v2/building-your-application/routing) |
| Qwik City                        | [Routing](https://qwik.dev/docs/routing/)                                                                                                                                                                                                                            |
| Vike                             | [Routing](https://vike.dev/routing)                                                                                                                                                                                                                                  |
| Waku                             | [Official documentation](https://waku.gg/)                                                                                                                                                                                                                           |
