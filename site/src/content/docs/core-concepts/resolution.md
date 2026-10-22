---
title: Resolution & Frameworks
description: How CodeGraph connects references and links routes to handlers.
---

Extraction produces nodes and raw edges; **resolution** turns names into real connections.

## Reference resolution

After parsing, CodeGraph resolves:

- **Imports** → the source files they point at, including tsconfig/jsconfig path aliases (each file uses its nearest config's `paths`), workspace packages (an entry that points at uncommitted `dist/` output maps back to its `src/` file) and cargo workspace members.
- **Calls** → their definitions, by import resolution and name matching. A member call such as `i18n.baseText()` resolves through the receiver's type: a `new` or type annotation, or the value a factory like `useI18n()` returns. Vue, Svelte and Astro single-file components get the same treatment as TypeScript files. A name matches a definition only in its own case, except in PHP, Pascal, CFML, COBOL and VB.NET, whose names ignore case, and a `super` call in an override goes to the parent's method, never back to the override itself. A call written without a receiver reaches only what is in scope there: in Java, Dart, Ruby and CFML a method of the class it is written in or of that class's supertypes and mixins, in C# a member of the types around it, their base types or a `using static` type, in Objective-C a message to `self` or `super` a method of the sender's class hierarchy, and in Kotlin a top-level function from the file's own package or one it imports, or a member of an implicit class, extension or DSL receiver. A VB.NET member access reaches a member only through `Me`, `MyBase`, `MyClass` or the member's own type or module. A Lua `local`, or a variable at the top of an R test file, is not visible from other files.
JavaScript and TypeScript calls after braceless `if`, `while`, `for` and `do` statements are bare calls. They do not borrow unrelated class methods. Calls through an explicit receiver keep their targets.

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
- Go calls through function-valued struct fields, including one field-to-field assignment

Every synthesized edge is marked `provenance: 'heuristic'` with the site that wired it, and is shown inline wherever a path crosses it.

Java, C# and Kotlin field or property receivers use their declared types, including inherited generic members. A declared type outside the project prevents a guess at an unrelated project method. Scala local binders and class ancestry, Swift implicit receivers and argument labels, and Rust/Go bare versus chained call forms constrain name matches. Names destructured from a composable or hook result resolve to the functions it returns under those keys. Unexported ESM bindings stay local unless explicitly exposed.

Kotlin chains use the callee position and declared return type, including nested and multiline calls; imported return-type hypotheses for standard method names keep confidence at most 0.7 when the receiver type is unknown.

Kotlin receiver inference follows bounded chains of declared returns and verified receiver-preserving methods. Properties initialized by typed factory calls retain compatible imported extensions. Explicit casts, single-type `when` branches, filtered collection elements and bound generic factory arguments also supply receiver types.

Rust chains follow declared method returns and field types. Matches without a proved receiver type keep confidence at most 0.7.

Rust `Self::` calls use the enclosing type and prefer its inherent method. Calls into project trait implementations on standard-library types need evidence from the argument type or an in-scope project trait.

C++ declarations give each listed variable its shared type, including callable locals. A source definition can implement its adjacent same-name header through transitive includes. Objective-C globals defined in source files stay local; other files reach them through header declarations.

Sync retries unchanged importers when a module's default export moves or a declaration file starts supplying a package. Python module resolution follows package roots and refreshes affected imports when those roots or packages change. Python attribute calls follow declared or assigned types, including factory returns and inherited initializers.

Git sync hooks retain their project root across reinstall and removal. On Windows, drive-letter casing and short directory names do not change ownership, including hooks from older installs.

Python attribute typing follows one field hop. An untyped parameter's attributes and missing members of a module-global instance do not borrow unrelated class methods by name. Deeper chains, package qualifiers absent from the indexed roots, and subscripted bases without indexed inheritance edges can remain unresolved. Runtime changes to Python import paths, such as pytest adding a directory with `syspath_prepend`, do not establish a source root for resolution. Calls through those imports can remain unresolved.

Rust paths and calls respect module imports, associated items, generic bounds and receiver types. C++ qualified calls stay within the named type or namespace. A concrete standard-library overload taking mutable class references requires matching argument types; unproved arguments remain unresolved. Go symbol lookup uses package scope. A Go interface's unexported methods match methods declared in that same package, including methods promoted through embedding. TypeScript type-only imports and re-exports retain their type boundary.
