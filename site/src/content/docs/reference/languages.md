---
title: Languages
description: Every language CodeGraph parses, and the extensions it recognizes.
---

Language support is automatic from the file extension — there's nothing to configure.

C# property accessors and expression-bodied properties contribute calls and references owned by the property. VB.NET member bodies and field initializers also contribute their calls and references.

C# field and property initializers retain their calls and references under the member that owns them. Target-typed `new()` resolves relative declared types through enclosing namespaces and honors `global::` qualification without requiring a redundant `using`. VB.NET resolves typed receivers, enclosing and inherited members, and field or property reads through values and Shared types without choosing unrelated project declarations. Names beginning with keywords, such as `SharedCache`, `Dimension` and `NewItem`, retain their declarations.

Go interfaces link named embedded interfaces and aliases that resolve to interfaces. Named scalar or struct terms, unions, underlying-type constraints and basic types are not supertypes. Package-qualified embeddings stay in their imported package. Go imports follow the nearest indexed module and the longest matching module path, including module changes during incremental sync. Unexported receivers and embedded methods stay in their declaring package. Dart imports, exports and part directives follow their library URIs and visibility rules. Library directive reads stay inside the indexed project, including resolved symlink targets. Calls through import prefixes, annotations and member chains follow the visible declaration and written receiver types, including explicit generic lookup types. Parameters and locals shadow bare calls. Top-level and field initializers contribute calls; const constructors, redirecting factories and annotated members retain their declarations and dartdoc. Getter reads become calls only when the receiver type reaches that getter and no nearer field in the visible class hierarchy overrides it; enum extensions and type-position references participate in resolution. Rust enum-variant values retain their enum references.

Named JavaScript and TypeScript object literals own their function members, including local objects and classic-script global assignments. Member calls follow the visible object; loop-local objects stay within their scope. Destructured member calls resolve across lines while preserving the source binding at the destructure declaration; unrelated bare names stay unresolved. Vue template expressions contribute calls to script bindings while preserving component ownership. Encoded attribute expressions retain their original source positions, and template-local bindings stay within their scope.

A COBOL copybook named in an explore query prioritizes its indexed source and lists its COPY and EXEC SQL INCLUDE sites. Missing indexed source is reported explicitly.

| Language | Extensions | Status |
|---|---|---|
| TypeScript | `.ts`, `.tsx` | Full support |
| JavaScript | `.js`, `.jsx`, `.mjs` | Full support |
| Python | `.py` | Full support |
| Go | `.go` | Full support |
| Rust | `.rs` | Full support |
| Java | `.java` | Full support |
| C# | `.cs` | Full support |
| VB.NET | `.vb` | Full support |
| PHP | `.php`, `.inc` | Full support (see Pascal for `.inc` include files) |
| Ruby | `.rb` | Full support |
| C | `.c`, `.h` | Full support |
| C++ | `.cpp`, `.hpp`, `.cc` | Full support |
| Objective-C | `.m`, `.mm`, `.h` | Partial support (classes, protocols, methods, `@property`, `#import`, message sends; `.mm` ObjC++ may parse incompletely) |
| Swift | `.swift` | Full support |
| Kotlin | `.kt`, `.kts` | Full support |
| Scala | `.scala`, `.sc` | Full support (classes, traits, objects, methods, type aliases, Scala 3 enums) |
| Dart | `.dart` | Full support |
| Svelte | `.svelte` | Full support (instance and module script extraction, Svelte 5 runes, SvelteKit routes) |
| Vue | `.vue` | Full support (script + script-setup with component ownership, Options API methods, computed properties, watchers and lifecycle hooks, Nuxt page/API/middleware routes) |
| Astro | `.astro` | Full support (component-owned frontmatter + browser-script extraction, template component/call references, `src/pages/` routes) |
| Liquid | `.liquid` | Full support |
| Pascal / Delphi | `.pas`, `.dpr`, `.dpk`, `.lpr`, `.inc` | Full support (classes, records, interfaces, enums, DFM/FMX forms; a `.inc` include is Pascal when it has no PHP open tag and reads as Pascal, unless `codegraph.json` maps `.inc`) |
| Lua | `.lua` | Full support (functions, methods, locals, `require` imports, call edges) |
| R | `.R`, `.r` | Full support (functions, S4/R5/R6 classes with methods, `library`/`require` imports, `source()` file references, call edges) |
| Luau | `.luau` | Full support (Lua, plus typed signatures, `type` aliases, Roblox `require`) |
| CFML | `.cfc`, `.cfm`, `.cfs` | Full support (tag-based `<cfcomponent>`/`<cfinterface>`/`<cffunction>` and bare-script `component { ... }` styles, `extends`/`implements`, embedded `<cfscript>`, calls written in tags such as `<cfset>`, `<cfif>`, `<cfloop condition>` and `#…#` expressions) |
| Markdown | `.md`, `.mdx`, `.markdown` | Documentation structure (headings, sections, local links, selected table rows/list items, shell command references) |

Vue, Svelte and Astro files have one file node containing their component. Top-level script members belong to that component, while nested symbols retain their own parents. Vue `<script setup>`, Svelte instance scripts and Astro frontmatter assign top-level execution, including constant initializers, to the component. Imports, module-level execution and Astro browser scripts remain with the file. Svelte recognizes both `context="module"` and `<script module>`.

Markdown files use a dedicated documentation extractor. `.mdx` files receive the same documentation indexing; embedded JSX and JavaScript are not parsed as MDX code.

JavaScript files with a Flow pragma use the TSX grammar after Flow-only syntax is blanked, preserving source locations.

Kotlin infix expressions, such as `Users.id eq id1`, contribute calls to the indexed infix function. The native scanner preserves surrounding declarations for same-line infix names beginning with `e`. Numeric bitwise calls remain unresolved when a project extension cannot be distinguished from Kotlin built-ins.

Kotlin infix call extraction accepts parenthesized operands and comments, including nested block comments. Numeric bitwise guesses are filtered while methods and extensions on project types remain eligible.

Kotlin `when` guards, open-ended ranges, multi-dollar strings and nullable receivers in function types are normalized before parsing while preserving source offsets. Comments stay intact, and multi-dollar strings keep their interpolation threshold.

C++ calls through namespace-opening macros and namespace aliases can reach a unique visible declaration; ambiguous overloads remain unresolved. Objective-C `super` messages target the superclass. Solidity bare calls follow the enclosing contract’s inheritance, and Erlang bare calls follow explicit module imports.

Calls through class names follow inherited class methods in Python, Pascal, Ruby, PHP, JavaScript, TypeScript and the Java family. A Python call on `self` inside a class skips a same-named import unless the class or an in-repo ancestor binds the name as an attribute, and links to the method or nested class the class declares or inherits through single bases. Receiver-name guesses exclude dispatched request handlers and test doubles the caller does not mention. Rust and Go chains preserve declared project receiver types while filtering unrelated standard-library method guesses; Rust factory lookup honors local function shadowing. R bare calls stay with functions, and JavaScript fetch response methods remain external.

Lua local aliases can call a function exported by a required project module, including a renamed field and bounded module re-exports. The returned module table determines its exported fields. Standard-library aliases, external modules and out-of-scope locals do not become project calls.

CommonJS calls follow explicit `module.exports` defaults, module forwarding, destructured bindings and `require('./module').member` aliases.

Literal local `require` calls create file dependencies even when used for side effects or inside functions. Computed specifiers, external packages and a locally shadowed `require` do not create these dependencies.

Namespace re-exports such as `export * as core` expose members through their namespace, including nested namespaces, without exposing those members as flat wildcard exports.

C++ access macros and conditionals inside declarations preserve class members. A normalized mid-declaration conditional indexes its first branch and preserves source locations.

C++ receiver lookup follows visible class-scoped type aliases, declared fields, project subscript return types and explicitly typed standard-container elements. Unsubstituted template parameters and ambiguous method owners remain unresolved.

Java/Kotlin enum constants with bodies retain their own methods and calls. Explicit imports, aliases and nested types participate in JVM name resolution. Scala block locals and package objects, C# block/file namespaces and Java nested types are scoped to their declarations and imports. Overload calls use argument shape, with Swift argument labels preserved. Python package imports can follow bounded package re-exports; pytest fixture returns can supply receiver types.

Pytest fixture receiver inference uses default test function and file names, or an explicit fixture decorator. Custom collection naming without that source evidence remains unresolved.

Python receiver inference follows the latest preceding assignment on a separate line within its lexical scope. An unknown replacement stays untyped, and a later local assignment in the same function cannot supply a type to an earlier call. A bare annotation does not replace an existing value. Assignments in nested functions do not change the outer receiver. Calls and method values through an outer-scope receiver stay unlinked when a later replacement makes its invocation-time value uncertain. A single future initialization remains supported; this conservative rule can omit valid calls made before a later replacement. Multiple bindings on the selected assignment line, or a replacement on the call line after an earlier binding, leave the receiver unlinked. Compatible unaliased imports such as `import pkg.a, pkg.b` bind the same package object and count as one binding. A single same-line constructor assignment before the call remains supported; a call before that assignment stays unlinked. This can omit valid same-line calls whose type is clear at runtime. It uses the indexed lexical bindings and does not track arbitrary mutation through other closures or dynamic writes.

Python `super()` and `super(Cls, self)` calls follow the class's method resolution order to the method it inherits. A call stays unresolved where Python would raise or only runtime could tell: outside a method's own frame (a class body, an f-string, a `@staticmethod`, or a lambda or generator expression for the zero-argument form), with `super` or the named class rebound where the call can see it, under a metaclass that may reorder the bases, past a base the index does not hold, or when a class on the way binds the name other than by `def` or makes it a property.

A Python method passed as a value through a module-level variable, such as `pool.submit(settings.conn.fetch)` after `global conn; conn = Store()`, links to the method of the class assigned to it. Those classes come from constructor calls or annotations (`conn: Store = make_store()`) in the variable's module, at module level or in a function or class body that declares it `global`, and from writes like `settings.conn = Store()` in other production modules. With several classes, the link goes to the declaration they all inherit. The module may be spelled in full (`import pkg.settings`, then `pkg.settings.conn.fetch`), but a longer chain such as `settings.conn.pool.fetch` reads an attribute and keeps the usual lookup. The usual lookup also decides when an unannotated assignment is something other than a constructor call (a factory, a conditional, a tuple target, a loop, `with`, `del`, an import or a star import), when an annotation and the constructor name different classes, when the writing function or its module rebinds the class's name (`Store = Decoy`) or the file imports it from two places, when `setattr`, `patch.object`, `__dict__` (a computed key included), `globals()`, `vars()` or `sys.modules[__name__]` can rebind the variable, when another module writes it through a name its function or module rebinds or that it imports from two places, when a class or function of the same name rebinds it, and when the calling function binds the name itself. Test files and `if __name__ == "__main__":` blocks don't change the variable's classes. A reference links to nothing when any of the variable's classes lacks a callable method of that name, or when it sits in a test file that installs its own double.

C++ receivers declared through typedef or using aliases follow the alias declaration scope and nested owners. Pointer, array and function members are not supertypes. Shopify section and snippet references stay inside their nearest theme; JSON under `templates/` or `sections/` is Liquid only beside a theme marker.
