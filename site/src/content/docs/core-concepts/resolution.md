---
title: Resolution & Frameworks
description: How CodeGraph connects references and links routes to handlers.
---

Extraction produces nodes and raw edges; **resolution** turns names into real connections.

## Reference resolution

After parsing, CodeGraph resolves:

- **Imports** → the source files they point at, including tsconfig/jsconfig path aliases (each file uses its nearest config's `paths`), workspace packages (an entry that points at uncommitted `dist/` output maps back to its `src/` file) and cargo workspace members.
- **Calls** → their definitions, by import resolution and name matching. A member call such as `i18n.baseText()` resolves through the receiver's type: a `new` or type annotation, or the value a factory like `useI18n()` returns. Vue, Svelte and Astro single-file components get the same treatment as TypeScript files. A name matches a definition only in its own case, except in PHP, Pascal, CFML, COBOL and VB.NET, whose names ignore case, and a `super` call in an override goes to the parent's method, never back to the override itself. A call written without a receiver reaches only what is in scope there: in Java, Dart, Ruby and CFML a method of the class it is written in or of that class's supertypes and mixins, and in Kotlin a top-level function from the file's own package or one it imports. A Lua `local`, or a variable at the top of an R test file, is not visible from other files.
- **Inheritance** → `extends` / `implements` between types.

## Framework awareness

CodeGraph recognizes web-framework routing files and emits `route` nodes linked by `references` edges to their handler classes or functions — so querying the callers of a view or controller surfaces the URL pattern that binds it. See [Framework Routes](/codegraph/guides/framework-routes/) for the full list of recognized frameworks.

## Dynamic-dispatch coverage

Static parsing misses computed and indirect calls, so flows can break at dynamic dispatch. CodeGraph bridges several of these boundaries with synthesizers so a flow connects end-to-end:

- Callback / observer registration
- `EventEmitter` channels
- React re-render (`setState` → `render`)
- JSX child (`render` → child component)
- Interface → implementation dispatch

Every synthesized edge is marked `provenance: 'heuristic'` with the site that wired it, and is shown inline wherever a path crosses it.
