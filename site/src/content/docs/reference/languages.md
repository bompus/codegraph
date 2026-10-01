---
title: Languages
description: Every language CodeGraph parses, and the extensions it recognizes.
---

Language support is automatic from the file extension — there's nothing to configure.

| Language | Extensions | Status |
|---|---|---|
| TypeScript | `.ts`, `.tsx` | Full support |
| JavaScript | `.js`, `.jsx`, `.mjs` | Full support |
| Python | `.py` | Full support |
| Go | `.go` | Full support |
| Rust | `.rs` | Full support |
| Java | `.java` | Full support |
| C# | `.cs` | Full support |
| PHP | `.php` | Full support |
| Ruby | `.rb` | Full support |
| C | `.c`, `.h` | Full support |
| C++ | `.cpp`, `.hpp`, `.cc` | Full support |
| Objective-C | `.m`, `.mm`, `.h` | Partial support (classes, protocols, methods, `@property`, `#import`, message sends; `.mm` ObjC++ may parse incompletely) |
| Swift | `.swift` | Full support |
| Kotlin | `.kt`, `.kts` | Full support |
| Scala | `.scala`, `.sc` | Full support (classes, traits, objects, methods, type aliases, Scala 3 enums) |
| Dart | `.dart` | Full support |
| Svelte | `.svelte` | Full support (script extraction, Svelte 5 runes, SvelteKit routes) |
| Vue | `.vue` | Full support (script + script-setup, Options API methods, computed properties, watchers and lifecycle hooks, Nuxt page/API/middleware routes) |
| Astro | `.astro` | Full support (frontmatter + script extraction, template component/call references, `src/pages/` routes) |
| Liquid | `.liquid` | Full support |
| Pascal / Delphi | `.pas`, `.dpr`, `.dpk`, `.lpr` | Full support (classes, records, interfaces, enums, DFM/FMX forms) |
| Lua | `.lua` | Full support (functions, methods, locals, `require` imports, call edges) |
| R | `.R`, `.r` | Full support (functions, S4/R5/R6 classes with methods, `library`/`require` imports, `source()` file references, call edges) |
| Luau | `.luau` | Full support (Lua, plus typed signatures, `type` aliases, Roblox `require`) |
| Markdown | `.md`, `.mdx`, `.markdown` | Documentation structure (headings, sections, local links, selected table rows/list items, shell command references) |

Markdown files use a dedicated documentation extractor. `.mdx` files receive the same documentation indexing; embedded JSX and JavaScript are not parsed as MDX code.

JavaScript files with a Flow pragma use the TSX grammar after Flow-only syntax is blanked, preserving source locations.

Kotlin infix expressions, such as `Users.id eq id1`, contribute calls to the indexed infix function. The native scanner preserves surrounding declarations for same-line infix names beginning with `e`. Numeric bitwise calls remain unresolved when a project extension cannot be distinguished from Kotlin built-ins.

Kotlin infix call extraction accepts parenthesized operands and comments, including nested block comments. Numeric bitwise guesses are filtered while methods and extensions on project types remain eligible.

Kotlin `when` guards, open-ended ranges, multi-dollar strings and nullable receivers in function types are normalized before parsing while preserving source offsets. Comments stay intact, and multi-dollar strings keep their interpolation threshold.

C++ calls through namespace-opening macros and namespace aliases can reach a unique visible declaration; ambiguous overloads remain unresolved. Objective-C `super` messages target the superclass. Solidity bare calls follow the enclosing contract’s inheritance, and Erlang bare calls follow explicit module imports.

Calls through class names follow inherited class methods in Python, Pascal, Ruby, PHP, JavaScript, TypeScript and the Java family. Receiver-name guesses exclude dispatched request handlers and test doubles the caller does not mention. Rust and Go chains preserve declared project receiver types while filtering unrelated standard-library method guesses; Rust factory lookup honors local function shadowing. R bare calls stay with functions, and JavaScript fetch response methods remain external.

Lua local aliases can call a function exported by a required project module, including a renamed field and bounded module re-exports. The returned module table determines its exported fields. Standard-library aliases, external modules and out-of-scope locals do not become project calls.

CommonJS calls follow explicit `module.exports` defaults, module forwarding, destructured bindings and `require('./module').member` aliases. Namespace re-exports such as `export * as core` expose members through their namespace, including nested namespaces, without exposing those members as flat wildcard exports.

C++ access macros and conditionals inside declarations preserve class members. A normalized mid-declaration conditional indexes its first branch and preserves source locations.

C++ receiver lookup follows visible class-scoped type aliases, declared fields, project subscript return types and explicitly typed standard-container elements. Unsubstituted template parameters and ambiguous method owners remain unresolved.
