---
title: Framework Routes
description: CodeGraph links URL patterns to the handlers that serve them.
---

CodeGraph detects web-framework routing files and emits `route` nodes linked by `references` edges to their handler classes or functions. Querying the callers of a view or controller then surfaces the URL pattern that binds it.

| Framework | Shapes recognized |
|---|---|
| **Django** | `path()`, `re_path()`, `url()`, `include()` in `urls.py` (CBV `.as_view()`, dotted paths) |
| **Flask** | `@app.route('/path', methods=[...])`, blueprint routes |
| **FastAPI** | `@app.get(...)`, `@router.post(...)`, all standard methods |
| **Express** | `app.get(...)`, `router.post(...)` with middleware chains |
| **Hono** | Imported `Hono` instances, method/path arrays, `basePath()` and same-file `.route()` mounts |
| **Elysia** | Imported `Elysia` instances, method calls, `.route()`, literal constructor prefixes and `.group()` callbacks |
| **Fastify** | Imported factories, shorthand methods, `.route({ method, url, handler })`, inline `.register()` callbacks with literal prefixes |
| **Hyper-Express** | Imported `Server` / `Router`, method calls, `.route(path)` chains and same-file `.use()` mounts |
| **Koa router** | `@koa/router` / `koa-router` instances, named routes, literal prefixes and same-file `.use(path, child.routes())` mounts |
| **H3** | Imported `H3`, `createRouter` / `createApp`, method calls, `.on()` / `.all()` and same-file router mounts |
| **Bun** | `Bun.serve()` or imported `serve()` with a literal `routes` table; direct handlers, method tables and static responses |
| **Effect v4** | `HttpRouter.add(method, path, handler)` / `.route()` from `effect/unstable/http` or its `HttpRouter` submodule |
| **Vixeny** | Option-free `wrap()()` builders with `.get/.post/.put/.delete` or `.route({ method, path, f })` |
| **NestJS** | `@Controller` + `@Get/@Post/...`, GraphQL `@Resolver` + `@Query/@Mutation`, `@MessagePattern`/`@EventPattern`, `@SubscribeMessage` |
| **Laravel** | `Route::get()`, `Route::resource()`, `Controller@action`, tuple syntax |
| **Drupal** | `*.routing.yml` routes (`_controller`, `_form`, entity handlers); `hook_*` implementations in `.module`/`.theme`/`.install`/`.inc` |
| **Rails** | `get '/x', to: 'users#index'`, hash-rocket `=>` syntax |
| **Spring** | `@GetMapping`, `@PostMapping`, `@RequestMapping` on methods |
| **Play** | `GET`/`POST`/… verb routes in `conf/routes` → `Controller.method` actions (Scala + Java) |
| **Gin / chi / gorilla / mux** | `r.GET(...)`, `router.HandleFunc(...)` |
| **Axum / actix / Rocket** | `.route("/x", get(handler))` |
| **ASP.NET** | `[HttpGet("/x")]` attributes on action methods |
| **Vapor** | `app.get("x", use: handler)` |
| **React Router / Remix** | JSX/data-router pages; literal framework-mode arrays; default Remix and registered `flatRoutes()` file pages, linked to named default components |
| **SvelteKit** | Route component nodes |
| **TanStack Router / Start** | Page routes plus literal `server.handlers` method tables and `createHandlers` callbacks on exported file routes; middleware is excluded from handler links |
| **Next.js** | App Router and Pages Router pages; `app/api/**/route.ts` method exports and `pages/api/**` default handlers |
| **Vue Router** / **Nuxt** | Vue route tables; `.vue` pages in `pages/` or Nuxt 4 `app/pages/`, dynamic/optional/catch-all segments and route groups; `server/api/` and `server/routes/` with method suffixes; route middleware |
| **Astro** | `src/pages/` `.astro` pages linked to components; `.ts`/`.js` HTTP-method exports linked to handlers; anchors and `Astro.redirect` link to local pages |
| **RedwoodSDK** | Registered `defineApp` trees with `route`, `index`, `render`, `layout`, `prefix` and standard method tables; exact handlers and JSX page classification |
| **Angular Router** | `provideRouter` / `RouterModule.forRoot` arrays, nested children, relative component imports and static lazy components/route arrays/NgModules |
| **Analog** | Registered default `src/app/pages/**/*.page.ts` pages, linked to named default classes; directory layouts, dot paths, index/pathless segments and parameters |
| **Solid Router** | Imported `Router`/`Route` JSX and registered literal configuration, nested paths, path arrays and bases; imported/local components and static lazy defaults |

Solid Router emits leaf routes, preserving nested path composition even when a child begins with `/`. Parent components and the router root remain layouts. Cross-file configuration, other router variants, dynamic/spread declarations, inline/anonymous components and lazy re-exports are unsupported.

Route resolution is automatic — there's nothing to configure. If a framework file is recognized, its routes appear in the graph after the next index or sync.

Analog requires the platform plugin in a default-root Vite config and option-free `provideFileRouter()` registration. Custom roots, extra route directories, `app/routes`, route metadata overrides, router options, optional catchalls, Markdown and anonymous/re-exported defaults remain unsupported. A directory makes its corresponding page a layout; a dotted filename alone does not.

Angular follows literal or constant route arrays from runtime router imports. `forChild` contributes routes only through a statically imported lazy NgModule. Route nodes belong to the registration file, and imported table changes refresh that owner. Redirects, named outlets, custom matchers, dynamic factories, conditional registrations, spread objects, path aliases and re-export modules remain unsupported. No navigation is inferred.

RedwoodSDK handler arrays use the final handler as the route root. A route stays `ANY /path` until its handler is shown to return JSX. Cross-file route arrays, custom methods, mutations, dynamic paths, ambiguous method tables and wrapped/anonymous exported components remain unsupported.

Astro supports default roots, `[param]`/`[...rest]` filenames and exported handlers, including `ALL` as `ANY`. Type-only exports, underscore-prefixed paths and `.mjs` endpoints are excluded. Custom routing configuration, Markdown/MDX, cross-file re-exports, client transition calls and navigation to rest routes remain unsupported.

React Router framework mode assumes the default `app/` directory. Computed arrays, custom app directories, `relative` helpers, anonymous defaults, and re-exports remain unsupported. Layout helpers contribute nesting without creating extra pages; an index page takes precedence over its parent layout.

Remix default `app/routes/` filenames and React Router configs registering an imported, option-free `flatRoutes()` call support JS/TS pages, immediate `folder/route` modules, dot nesting, index/pathless segments, parameters, optional segments, splats, and bracket escapes. Resource-only and direct `Outlet`-only defaults are excluded. Custom configuration, folder `index` fallback, and Markdown/MDX are unsupported.

The JavaScript HTTP readers require a recognized package import (ES modules or CommonJS), except for the global `Bun.serve`. They follow immutable local router bindings and literal declarations, without executing your application. Named handlers produce references; direct calls inside inline handlers produce call edges. Static responses have an endpoint without an invented handler. Member handlers remain unresolved by this reader.

TanStack Start server routes support imported `createFileRoute` calls assigned to exported `const Route`. Computed/spread tables, custom factories, member handlers, and `update` chains remain unsupported. RPC functions created with `createServerFn` are not presented as public route URLs.

Computed paths, spread configuration, cross-file mounts, plugin factories, mutable router aliases, and runtime method replacement are outside this static reading. Imports and captured router bindings must precede their use in source. Vixeny builders with options are omitted because their effective paths depend on the terminal operation. Nuxt custom route configuration, page metadata overrides, non-Vue page extensions, and custom server handler wrappers are not interpreted. Re-index after upgrading to add the new endpoints to an existing graph.
