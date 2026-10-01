---
title: Framework Routes
description: CodeGraph links URL patterns to the handlers that serve them.
---

CodeGraph detects web-framework routing files and emits `route` nodes linked by `references` edges to their handler classes or functions. Querying the callers of a view or controller then surfaces the URL pattern that binds it. A `codegraph_explore` question that spells out a route (`GET /api/tasks/:id`, or a bare `/blog/:slug` for a page) starts from that route node, so the file declaring it comes first.

| Framework | Shapes recognized |
|---|---|
| **Django** | `path()`, `re_path()`, `url()`, `include()` in `urls.py` (CBV `.as_view()`, dotted paths) |
| **Flask** | `@app.route('/path', methods=[...])`, blueprint routes, `add_url_rule(…)` and a project helper that passes paths with a `view_func=` |
| **FastAPI** | `@app.get(...)`, `@router.post(...)`, all standard methods |
| **Express** | `app.get(...)`, `router.post(...)` with middleware chains; inline arrow and function-expression handlers, including wrapper calls |
| **Hono** | Imported `Hono` instances, method/path arrays, `basePath()` and same-file `.route()` mounts |
| **Elysia** | Imported `Elysia` instances, method calls, `.route()`, literal constructor prefixes and `.group()` callbacks |
| **Fastify** | Imported factories, shorthand methods, `.route({ method, url, handler })`, inline `.register()` callbacks with literal prefixes; default-exported plugin files, mounted at their `@fastify/autoload` directory prefix |
| **Hyper-Express** | Imported `Server` / `Router`, method calls, `.route(path)` chains and same-file `.use()` mounts |
| **Koa router** | `@koa/router` / `koa-router` instances, named routes, literal prefixes and same-file `.use(path, child.routes())` mounts |
| **H3** | Imported `H3`, `createRouter` / `createApp`, method calls, `.on()` / `.all()` and same-file router mounts |
| **Bun** | `Bun.serve()` or imported `serve()` with a literal `routes` table; direct handlers, method tables and static responses |
| **Effect v4** | `HttpRouter.add(method, path, handler)` / `.route()` from `effect/unstable/http` or its `HttpRouter` submodule |
| **Vixeny** | Option-free `wrap()()` builders with `.get/.post/.put/.delete` or `.route({ method, path, f })` |
| **NestJS** | `@Controller` + `@Get/@Post/...` (with `RouterModule` prefixes, `setGlobalPrefix` and URI versioning), GraphQL `@Resolver` + `@Query/@Mutation`, `@MessagePattern`/`@EventPattern`, `@SubscribeMessage` |
| **Laravel** | `Route::get()`, `Route::resource()`, `Controller@action`, tuple syntax; `Route::prefix()` and group prefixes, and the `/api` mount of `routes/api.php` |
| **Drupal** | `*.routing.yml` routes (`_controller`, `_form`, entity handlers); `hook_*` implementations in `.module`/`.theme`/`.install`/`.inc` |
| **Rails** | `get '/x', to: 'users#index'`, hash-rocket `=>` syntax, `resources` / `resource`, and the paths and controller modules of `namespace`, `scope`, nested resources and `member` / `collection` blocks; a Rails engine's `config/routes.rb` too |
| **Spring** | `@GetMapping`, `@PostMapping`, `@RequestMapping` on methods |
| **Play** | `GET`/`POST`/… verb routes in `conf/routes` → `Controller.method` actions (Scala + Java), including projects kept in subdirectories |
| **Gin / chi / gorilla / mux** | `r.GET(...)`, `router.HandleFunc(...)` |
| **Axum / actix / Rocket** | `.route("/x", get(handler))` |
| **ASP.NET** | `[HttpGet("/x")]` attributes on action methods and FastEndpoints `Configure()` verb calls |
| **Vapor** | `app.get("x", use: handler)` and closure handlers; `use: Controller.show` links to that type's `show` (nested types and extensions included), `use: self.index` to the collection's own |
| **React Router / Remix** | JSX/data-router pages with nested children, constant paths, `Component` and lazy module exports; literal framework-mode arrays; default Remix and registered `flatRoutes()` file pages, linked to named default components |
| **SvelteKit** | Route component nodes, named by URL: `(group)` folders dropped, `[id=matcher]` read as `:id` |
| **TanStack Router / Start** | Page routes plus literal `server.handlers` method tables and `createHandlers` callbacks on exported file routes; middleware is excluded from handler links |
| **Next.js** | App Router and Pages Router pages; `app/api/**/route.ts` method exports and `pages/api/**` default handlers |
| **Expo Router** | `app/` screen files linked to their default-export components; `+api` files' method exports as endpoints (`GET /hello`). In a monorepo, each file-based router reads only the app whose `package.json` declares it |
| **Vue Router** / **Nuxt** | Vue route tables, named or split across module files (`export const constantRoutes = [...]`, `new Router(...)`), with nested `children` joined onto their parent's path and linked to the parent's layout component, and lazy `() => import(…)` views linked to the file they import; in a Nuxt app, `.vue` pages in `pages/` or Nuxt 4 `app/pages/`, each linked to its page component, with `index` folders, dynamic/optional/catch-all segments and route groups; `server/api/` and `server/routes/` with method suffixes; route middleware |
| **Astro** | `src/pages/` `.astro` pages linked to components; `.ts`/`.js` HTTP-method exports linked to handlers; anchors and `Astro.redirect` link to local pages |
| **RedwoodSDK** | Registered `defineApp` trees with `route`, `index`, `render`, `layout`, `prefix` and standard method tables; exact handlers and JSX page classification |
| **Angular Router** | `Routes` arrays (`RouterModule.forRoot` / `forChild`, `provideRouter`, a routes file's default export), including routes in Nx workspace libraries reached through barrel files, nested children, lazy `loadComponent` / `loadChildren` (an NgModule's through its routing module, a barrel in front of it, or the routes file it imports), route-constant and `$localize` paths; navigation from `router.navigate`, `navigateByUrl`, `createUrlTree`, `routerLink` and `redirectTo` |
| **Analog** | Registered default `src/app/pages/**/*.page.ts` pages, linked to named default classes; directory layouts, dot paths, index/pathless segments and parameters |
| **Solid Router** | Imported `Router`/`Route` JSX and registered literal configuration, nested paths, path arrays and bases; exported `RouteDefinition[]` tables prefixed by where they are registered; imported/local components and static lazy defaults |
| **SolidStart** | Default file pages and HTTP-method exports; exact local targets, nested layouts, groups, parameters and GET-to-HEAD fallback |
| **Vike** | Default JS/TS `+Page` modules and nearest inherited literal `+route` overrides; exact local named components and `@` parameters |
| **Qwik City** | Default index pages, named/anonymous `component$` components and method-specific endpoint exports; groups, parameters and catchalls |
| **Waku** | Default filesystem pages and registered `createPages`/`createPage` declarations, exact component roots, dynamic parameters and literal `staticPaths` expansion |

Waku coverage targets 1.0.0-rc.0. Default `src/pages` and literal default/Cloudflare `fsRouter` adapters are supported. A `waku.config` that only adds Vite plugins, as the official templates do, keeps discovery on; `srcDir`, `basePath` or any other Vite option turns it off. Parameter pages need explicit dynamic rendering or declared static paths. Layouts, roots, slices, interceptors and API modules are excluded. Custom routing configuration, unknown `getConfig`, wrappers and re-exports remain unsupported.

Programmatic Waku pages require a `createPages` callback, imported from `waku` or `waku/router/server`, registered in the default server module. Direct `createPage` calls support aliases, async callbacks, literal paths/render/staticPaths and `exactPath`, with exact named local or imported function targets. Computed declarations, conditional calls, nested helpers and arbitrary wrappers are omitted.

Vike coverage targets 0.4.266 with default roots and option-free `vike()` configuration. Canonical `vike-react/config` rendering configuration is supported. Dynamic overrides, custom roots/options, configured or inherited-only Page targets, unknown extensions/metadata, anonymous/re-exported/wrapped components and template-language pages remain unsupported; these do not receive fallback paths.

Qwik City coverage targets 1.20.0 with default roots and option-free `qwikCity()` configuration. Page discovery requires the root provider/outlet; API endpoints do not. Parent index pages remain pages, while layouts and generic `onRequest` middleware do not become endpoints. Custom routing options, layout-override index names, optional parameters, Markdown/MDX, re-exports and arbitrary wrappers remain unsupported.

SolidStart coverage targets version 2.0.4: option-free `solidStart()` in Vite and, for pages, `FileRoutes` directly inside the default app's `Router`. A file can supply both a page and endpoints. Named local functions and constant function exports are supported. Custom roots/options, dynamic config, page route overrides, anonymous/re-exported handlers and Markdown remain unsupported. Optional parameters apply to pages only; OPTIONS-only APIs are excluded by this version's runtime.

Solid Router emits leaf routes, preserving nested path composition even when a child begins with `/`. Parent components and the router root remain layouts. A table exported as `RouteDefinition[]` (annotated, `satisfies` or `as`) is read in its own file and takes the prefix of the one place that registers it; a table registered under several prefixes keeps the paths it declares. Untyped cross-file arrays, other router variants, dynamic declarations, object spreads inside route entries, inline/anonymous components and lazy re-exports are unsupported.

Route resolution is automatic — there's nothing to configure. If a framework file is recognized, its routes appear in the graph after the next index or sync.

Analog requires the platform plugin in a default-root Vite config and option-free `provideFileRouter()` registration. Custom roots, extra route directories, `app/routes`, route metadata overrides, router options, optional catchalls, Markdown and anonymous/re-exported defaults remain unsupported. A directory makes its corresponding page a layout; a dotted filename alone does not. The Vite config may be an object or a `defineConfig` callback that directly returns one, as the create-analog templates write it; `prerender.routes` is allowed.

Angular reads a routes array where it is declared: typed `Routes` / `Route[]`, handed to `provideRouter` or `RouterModule.forRoot` / `forChild` (under any name the file imports them as), or a routes file's default export. A `loadChildren` file's routes sit under the path that loads it, also when an NgModule reaches them through a routing module in its own folder, a barrel there, or a routes file that routing module imports. A route with `children` is a layout around the screens inside it. Redirects, a `**` catch-all, custom matchers, named outlets and entries that spread another object name no screen. An untyped array in a file that does not import `@angular/router` is not read, and a relative navigation (`relativeTo`) is left unresolved. Templates are read at synthesis time: `routerLink`, child component tags and event bindings link from their component.

RedwoodSDK handler arrays use the final handler as the route root. A route stays `ANY /path` until its handler is shown to return JSX. Cross-file route arrays, custom methods, mutations, dynamic paths, ambiguous method tables and wrapped/anonymous exported components remain unsupported.

Astro supports default roots, `[param]`/`[...rest]` filenames and exported handlers, including `ALL` as `ANY`. Type-only exports, underscore-prefixed paths and `.mjs` endpoints are excluded. Custom routing configuration, Markdown/MDX, cross-file re-exports, client transition calls and navigation to rest routes remain unsupported.

React Router framework mode assumes the default `app/` directory. Computed arrays, custom app directories, `relative` helpers, anonymous defaults, and re-exports remain unsupported. Layout helpers contribute nesting without creating extra pages; an index page takes precedence over its parent layout.

Remix default `app/routes/` filenames and React Router configs registering an imported, option-free `flatRoutes()` call support JS/TS pages, immediate `folder/route` modules, dot nesting, index/pathless segments, parameters, optional segments, splats, and bracket escapes. Resource-only and direct `Outlet`-only defaults are excluded. Custom configuration, folder `index` fallback, and Markdown/MDX are unsupported.

The JavaScript HTTP readers require a recognized package import (ES modules or CommonJS), except for the global `Bun.serve`. They follow immutable local router bindings and literal declarations, without executing your application. Named handlers produce references; direct calls inside inline handlers produce call edges. Static responses have an endpoint without an invented handler. Member handlers remain unresolved by this reader.

TanStack Start server routes support imported `createFileRoute` calls assigned to exported `const Route`. Computed/spread tables, custom factories, member handlers, and `update` chains remain unsupported. RPC functions created with `createServerFn` are not presented as public route URLs.

Computed paths, spread configuration, cross-file mounts, plugin factories, mutable router aliases, and runtime method replacement are outside this static reading. Imports and captured router bindings must precede their use in source. Vixeny builders with options are omitted because their effective paths depend on the terminal operation. Nuxt custom route configuration, page metadata overrides, non-Vue page extensions, and custom server handler wrappers are not interpreted. Re-index after upgrading to add the new endpoints to an existing graph.

React Router `<Link to>` and `navigate` can read destinations returned by a route-config object's arrow helper, such as `paths.app.discussion.getHref(id)`. Whole-segment template parameters remain route parameters, and optional query suffixes are omitted from the path. Factory-created config objects, shadowed bindings, spread overrides and computed path fragments remain unresolved. Arrow-function attributes before a link's `to` attribute are supported.

Inline HTTP handler calls retain simple member receivers. Framework name conventions prefer visible declarations in the calling file, then the applicable package or directory; they exclude unrelated nested types and test-local declarations. React and Express naming conventions require imports to reach another file. NestJS provider lookup also supports convention siblings in the same directory. Vue/React conventions apply to script references, and Astro component conventions apply to `.astro` markup.

Express treats the last registration argument as the handler, excluding a trailing comma. Inline arrow and function-expression handlers contribute their body calls, including when passed through a wrapper call. An inline middleware before a named handler does not replace that handler.
