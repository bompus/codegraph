
import { Binding } from '../types';
import { UnresolvedRef } from './types';

/**
 * Resolution rules shared by the TypeScript side of the resolver: the
 * language-family gates applied to framework results, the ambiguous-name
 * ceiling handed to the kernel, and binding-row helpers.
 */

/**
 * Ceiling on how many same-named definitions a FUZZY name-match strategy will
 * score. A name defined more times than this is "ubiquitous" — a method/symbol
 * re-declared across a vendored theme or SDK (e.g. `init`/`update`/`render` on
 * every widget of a committed Metronic theme — #999). No directory-proximity or
 * receiver-word-overlap score can reliably pick THE one true target among
 * thousands, so the fuzzy strategies (matchByExactName's findBestMatch, and
 * matchMethodCall Strategy 3) decline above the ceiling instead of emitting a
 * low-confidence, almost-certainly-wrong edge. This also caps their per-ref cost
 * at O(ceiling): without it, K same-named refs each scored K candidates — the
 * O(K²) blow-up that pinned a core for 15-28 min at "Resolving refs … 94%" on a
 * repo vendoring a large JS/TS theme (#999). The PRECISE strategies are
 * unaffected: qualified-name, import-based, and class-name (Strategy 1/2)
 * resolution all still run and resolve a ubiquitous name when the context names
 * its exact target. Real repos top out near ~40 same-named methods, so a normal
 * codebase never reaches this; only bulk-vendored code does. Tune via
 * `CODEGRAPH_AMBIGUOUS_NAME_CEILING`.
 */
const DEFAULT_AMBIGUOUS_NAME_CEILING = 500;
/** Exported so the kernel resolver can be configured with the same ceiling. */
export function resolveAmbiguousNameCeiling(): number {
  const raw = process.env.CODEGRAPH_AMBIGUOUS_NAME_CEILING;
  if (!raw) return DEFAULT_AMBIGUOUS_NAME_CEILING;
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_AMBIGUOUS_NAME_CEILING;
}

/**
 * Language families that share a type system / runtime, so a same-language-only
 * reference may still resolve across them (a Kotlin `Foo.BAR` can name a Java
 * `Foo`). Anything not listed forms its own singleton family.
 */
const LANGUAGE_FAMILY: Record<string, string> = {
  java: 'jvm', kotlin: 'jvm', scala: 'jvm',
  swift: 'apple', objc: 'apple',
  // ArkTS is a TS superset — every HarmonyOS project mixes `.ets` UI with
  // `.ts` logic modules, so refs must cross freely between them.
  typescript: 'web', tsx: 'web', javascript: 'web', jsx: 'web', arkts: 'web',
  c: 'c', cpp: 'c',
  // Razor/Blazor markup names C# types — same family so `@model Foo` /
  // `<MyComponent/>` resolve to their `.cs` class through the cross-family gate.
  csharp: 'dotnet', razor: 'dotnet',
};
export function sameLanguageFamily(a: string, b: string): boolean {
  if (a === b) return true;
  const fa = LANGUAGE_FAMILY[a];
  return fa !== undefined && fa === LANGUAGE_FAMILY[b];
}
/**
 * True when `lang` belongs to a known multi-language family (jvm/apple/web/c).
 * Languages not listed (php, python, go, ruby, rust, dart, …) and config
 * formats (yaml/xml/blade) form their own singleton families and return
 * `false` — used to leave config↔code framework bridges (whose config side is
 * never a known programming-language family) out of the cross-family gate.
 */
function isKnownLanguageFamily(lang: string): boolean {
  return LANGUAGE_FAMILY[lang] !== undefined;
}
/**
 * True when `a` and `b` are two DIFFERENT *known* language families — the
 * signature of a coincidental cross-language name collision (a TS `import
 * React` matching a Swift `import React`, a C++ `#include "X.h"` matching a
 * same-named ObjC header on another platform). The both-*known* test is
 * deliberately weaker than {@link sameLanguageFamily}'s negation: a
 * single-file-component language that carries its own tag (`vue`/`svelte`)
 * importing a `.ts` module, or any singleton-family language (php/go/ruby/…),
 * returns `false` here and is left alone.
 */
export function crossesKnownFamily(a: string, b: string): boolean {
  return isKnownLanguageFamily(a) && isKnownLanguageFamily(b) && !sameLanguageFamily(a, b);
}
/**
 * Languages whose code can name each other's symbols directly. Wider than
 * {@link LANGUAGE_FAMILY}: single-file components join the web group,
 * C/C++/ObjC/Swift share one native group (ObjC is a C superset; Swift calls
 * both through bridging headers), and every other programming language is a
 * group of its own. Markup, config and template languages are absent:
 * framework bridges start there.
 */
const CODE_INTEROP_GROUP: Record<string, string> = {
  typescript: 'web', tsx: 'web', javascript: 'web', jsx: 'web', arkts: 'web', svelte: 'web', vue: 'web', astro: 'web',
  java: 'jvm', kotlin: 'jvm', scala: 'jvm',
  c: 'native', cpp: 'native', objc: 'native', swift: 'native',
  csharp: 'dotnet', razor: 'dotnet', vbnet: 'dotnet',
  cfml: 'cfml', cfscript: 'cfml', cfquery: 'cfml',
  lua: 'lua', luau: 'lua',
  python: 'python', go: 'go', rust: 'rust', php: 'php', ruby: 'ruby', dart: 'dart', pascal: 'pascal',
  r: 'r', solidity: 'solidity', erlang: 'erlang', cobol: 'cobol', terraform: 'terraform', nix: 'nix',
};
/**
 * True when `a` and `b` are programming languages that cannot name each
 * other's symbols: a same-named hit across them is a coincidence (a Rust
 * `Ok(..)` is not a Scala enum member, a Go `Context` is not a C struct).
 */
export function crossesCodeBoundary(a: string, b: string): boolean {
  const ga = CODE_INTEROP_GROUP[a];
  const gb = CODE_INTEROP_GROUP[b];
  return ga !== undefined && gb !== undefined && ga !== gb;
}
/**
 * The binding row for `name` whose scope contains `line`, innermost (narrowest
 * scope) first; a row of any scope when `line` is unknown. Rows are few per
 * file and the callers memoise, so a linear scan is fine.
 */
export function innermostBinding(rows: Binding[], name: string, line?: number): Binding | undefined {
  let best: Binding | undefined;
  for (const r of rows) {
    if (r.name !== name) continue;
    if (line !== undefined && (line < r.scopeStart || line > r.scopeEnd)) continue;
    if (!best || r.scopeEnd - r.scopeStart < best.scopeEnd - best.scopeStart) best = r;
  }
  return best;
}

/** Languages whose module boundary is `import`/`export` (or CommonJS). */
const ESM_FAMILY = new Set<string>(['typescript', 'tsx', 'javascript', 'jsx', 'arkts']);

/** Ordinary member calls covered by the migrated binding model. */
export function isBindingReceiverCall(ref: UnresolvedRef): boolean {
  return ref.referenceKind === 'calls' &&
    (ESM_FAMILY.has(ref.language) || ['python', 'go', 'java', 'kotlin', 'php', 'c', 'cpp'].includes(ref.language)) &&
    /^.+\.[\w$]+$/.test(ref.referenceName) &&
    !ref.referenceName.includes('()') && !/^(this|self|super|cls)(\.|$)/.test(ref.referenceName);
}
