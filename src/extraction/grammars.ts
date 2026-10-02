/**
 * Language detection and the language table.
 *
 * Which file extension is which language, which languages have a grammar in
 * the native kernel, and which are handled by custom extractors. Parsing
 * itself lives in parse-tree.ts (the kernel is the only parser since Phase 5
 * of docs/design/kernel-only-extraction-plan.md).
 */

import { Language } from '../types';

export type GrammarLanguage = Exclude<Language, 'svelte' | 'vue' | 'astro' | 'liquid' | 'razor' | 'yaml' | 'twig' | 'xml' | 'properties' | 'markdown' | 'unknown'>;

/**
 * Every language the native kernel carries a grammar for (codegraph-kernel/src/
 * langs.rs). `tsx` uses the TSX grammar and `jsx` uses JavaScript.
 * This is the single list `isLanguageSupported`,
 * `hasTreeSitterGrammar` and `getSupportedLanguages` answer from.
 */
const GRAMMAR_LANGUAGES: ReadonlySet<GrammarLanguage> = new Set<GrammarLanguage>([
  'typescript',
  'tsx',
  'javascript',
  'jsx',
  'python',
  'go',
  'rust',
  'java',
  'c',
  'cpp',
  'csharp',
  'php',
  'ruby',
  'swift',
  'kotlin',
  'dart',
  'pascal',
  'scala',
  'lua',
  'r',
  'luau',
  'objc',
  'cfml',
  'cfscript',
  'cfquery',
  'cobol',
  'vbnet',
  'erlang',
  'solidity',
  'terraform',
  'arkts',
  'nix',
]);

/**
 * File extension to Language mapping
 */
export const EXTENSION_MAP: Record<string, Language> = {
  '.ts': 'typescript',
  '.tsx': 'tsx',
  // ESM/CJS TypeScript module extensions — parsed as TS (no JSX). (#366)
  '.mts': 'typescript',
  '.cts': 'typescript',
  // ArkTS (HarmonyOS / OpenHarmony) — a TypeScript superset with declarative
  // UI (`@Component struct` + `build()`). Own grammar (a tree-sitter-typescript
  // -style fork); plain `.ts` in an ArkTS project stays TypeScript. (#648)
  '.ets': 'arkts',
  '.js': 'javascript',
  '.mjs': 'javascript',
  '.cjs': 'javascript',
  // SAP HANA XS Classic server-side JavaScript. (#556)
  '.xsjs': 'javascript',
  '.xsjslib': 'javascript',
  '.jsx': 'jsx',
  '.py': 'python',
  '.pyw': 'python',
  '.go': 'go',
  '.rs': 'rust',
  '.java': 'java',
  '.c': 'c',
  '.h': 'c', // Could also be C++, defaulting to C
  '.cpp': 'cpp',
  '.cc': 'cpp',
  '.cxx': 'cpp',
  '.hpp': 'cpp',
  '.hxx': 'cpp',
  '.cs': 'csharp',
  // ASP.NET Razor / Blazor markup — custom RazorExtractor (links @model/@inject/
  // component tags to their C# types; markup isn't a tree-sitter grammar).
  '.cshtml': 'razor',
  '.razor': 'razor',
  '.php': 'php',
  // Drupal-specific PHP file extensions
  '.module': 'php',
  '.install': 'php',
  '.theme': 'php',
  '.inc': 'php',
  // YAML (used for Drupal routing files; no symbol extraction, file-level tracking only)
  '.yml': 'yaml',
  '.yaml': 'yaml',
  // Twig templates (file-level tracking only, no symbol extraction)
  '.twig': 'twig',
  '.md': 'markdown',
  '.mdx': 'markdown',
  '.markdown': 'markdown',
  '.rb': 'ruby',
  '.rake': 'ruby',
  '.swift': 'swift',
  '.kt': 'kotlin',
  '.kts': 'kotlin',
  '.dart': 'dart',
  '.liquid': 'liquid',
  '.svelte': 'svelte',
  '.vue': 'vue',
  '.astro': 'astro',
  '.r': 'r',
  '.pas': 'pascal',
  '.dpr': 'pascal',
  '.dpk': 'pascal',
  '.lpr': 'pascal',
  '.dfm': 'pascal',
  '.fmx': 'pascal',
  '.scala': 'scala',
  '.sc': 'scala',
  '.lua': 'lua',
  '.luau': 'luau',
  '.m': 'objc',
  '.mm': 'objc',
  '.sol': 'solidity',
  // CFML: .cfc/.cfm parse with the tag-aware `cfml` grammar (custom CfmlExtractor
  // dialect-switches to cfscript for bare-script content); .cfs is pure CFScript.
  '.cfc': 'cfml',
  '.cfm': 'cfml',
  '.cfs': 'cfscript',
  // Metal Shading Language ≈ C++14: the C++ grammar extracts its functions,
  // structs, and calls. MSL-specific `[[attribute]]` annotations are blanked
  // pre-parse for `.metal` files (see blankMetalAttributes in c-cpp.ts). (#1121)
  '.metal': 'cpp',
  // CUDA ≈ C++ plus execution-space specifiers (`__global__` …) and
  // `<<<grid, block>>>` kernel-launch syntax: the C++ grammar extracts its
  // functions/structs/classes/calls once blankCudaConstructs (pre-parse; gated
  // by these extensions OR by content for CUDA living in `.h`/`.hpp` headers —
  // see c-cpp.ts) blanks the CUDA-only tokens. (#387)
  '.cu': 'cpp',
  '.cuh': 'cpp',
  '.nix': 'nix',
  // XML: file-level tracking; the MyBatis extractor matches `<mapper namespace="...">`
  // shape and emits SQL-statement nodes (other XML returns empty).
  '.xml': 'xml',
  // COBOL: programs (.cbl/.cob) and copybooks (.cpy). Vendored grammar
  // (patched yutaro-sakamoto/tree-sitter-cobol) handles fixed-format column
  // rules, EXEC CICS/SQL blocks, and standalone copybook fragments.
  '.cbl': 'cobol',
  '.cob': 'cobol',
  '.cobol': 'cobol',
  '.cpy': 'cobol',
  // VB.NET: vendored grammar (patched govindbanura/tree-sitter-vbnet) — classes,
  // modules, interfaces, structures, properties, events, Handles clauses, LINQ.
  '.vb': 'vbnet',
  // Erlang: modules (.erl) and header files (.hrl). Vendored WhatsApp/
  // tree-sitter-erlang grammar (the ELP grammar).
  '.erl': 'erlang',
  '.hrl': 'erlang',
  // escripts parse natively — the grammar has a first-class `shebang` node.
  // (`.app`/`.app.src` resource files route via isErlangAppFile below: their
  // last-dot extension is too generic for this map.)
  '.escript': 'erlang',
  // Spring config: `application.properties` / `application-*.properties`. Same
  // shape as the `.yml` variants — the YAML/properties extractor emits one node
  // per leaf key, and the Spring resolver links `@Value("${k}")` references.
  '.properties': 'properties',
  // Terraform / OpenTofu / HCL config — tree-sitter-terraform dialect of HCL.
  '.tf': 'terraform',
  '.tfvars': 'terraform',
  '.tofu': 'terraform',
};

/** MPEG transport stream: fixed 188-byte packets, each opening with 0x47. */
const MPEG_TS_PACKET_SIZE = 188;
const MPEG_TS_SYNC_BYTE = 0x47;
/**
 * Consecutive packets whose sync byte must line up before a file counts as
 * video — 3 KB of head. A stream shorter than that is cheap to parse anyway;
 * the cost #1910 is about comes from clips hundreds of KB long.
 */
const MPEG_TS_MIN_PACKETS = 16;
/**
 * Share of the head that must be control bytes (below 0x20, other than the
 * whitespace ones) for it to count as binary. Compressed audio and video put
 * about one byte in eight there; source text puts none.
 */
const MPEG_TS_MIN_CONTROL_SHARE = 1 / 64;
/**
 * How many bytes of a file's head `isMpegTransportStream` needs — enough to
 * see `MPEG_TS_MIN_PACKETS` sync bytes plus the packets between them.
 */
export const MPEG_TS_SNIFF_BYTES = MPEG_TS_PACKET_SIZE * MPEG_TS_MIN_PACKETS;

/**
 * Whether these leading bytes are an MPEG transport stream — the OTHER thing a
 * `.ts` file can be. Golden video fixtures (`testdata/*.ts`, e2e clips) share
 * TypeScript's extension, and tree-sitter takes ~28 s to chew through a 900 KB
 * clip for zero symbols (#1910), so the decision has to be made from the head
 * of the file, before any parse.
 *
 * Two conditions, both required:
 *   1. the sync byte 0x47 sits at every 188-byte packet boundary of the first
 *      `MPEG_TS_MIN_PACKETS` packets — every packet of a transport stream
 *      opens with it, and nothing else pads to 188;
 *   2. the head is binary: at least `MPEG_TS_MIN_CONTROL_SHARE` of it is
 *      control bytes, as any compressed payload is.
 * 0x47 is the letter `G`, so (1) alone could match source whose lines happen
 * to put a `G` at every 188-byte stride. Checking for a single NUL was not
 * enough to close that: one NUL in a comment is still TypeScript. (2) asks for
 * dozens of control bytes, which no source file carries.
 *
 * `head` is the first `MPEG_TS_SNIFF_BYTES` (or fewer) bytes of the file.
 */
export function isMpegTransportStream(head: Uint8Array): boolean {
  const lastSync = MPEG_TS_PACKET_SIZE * (MPEG_TS_MIN_PACKETS - 1);
  if (head.length <= lastSync) return false;
  for (let off = 0; off <= lastSync; off += MPEG_TS_PACKET_SIZE) {
    if (head[off] !== MPEG_TS_SYNC_BYTE) return false;
  }
  let control = 0;
  for (let i = 0; i < head.length; i++) {
    const b = head[i]!;
    // Tab, newline, vertical tab, form feed and carriage return are text.
    if (b < 0x20 && (b < 0x09 || b > 0x0d)) control++;
  }
  return control >= head.length * MPEG_TS_MIN_CONTROL_SHARE;
}

/** Whether `filePath` carries the one extension MPEG-TS shares with a language. */
export function hasMpegTsExtension(filePath: string): boolean {
  return filePath.length > 3 && filePath.slice(-3).toLowerCase() === '.ts';
}

/**
 * Whether a file is one CodeGraph can parse, based purely on its extension.
 * This is the single source of truth for "should we index this file" — derived
 * from EXTENSION_MAP so parser support and indexing selection never drift.
 *
 * `overrides` is the project's validated custom extension → language map (from
 * `codegraph.json`); when present its extensions count as indexable in addition
 * to the built-ins. Omitting it is byte-identical to the zero-config behavior.
 */
export function isSourceFile(filePath: string, overrides?: Record<string, Language>): boolean {
  if (isPlayRoutesFile(filePath)) return true; // Play `conf/routes` is extensionless
  if (isShopifyLiquidJson(filePath)) return true; // Shopify OS 2.0 JSON templates / section groups
  if (isErlangAppFile(filePath)) return true; // OTP `.app`/`.app.src` resource files
  const dot = filePath.lastIndexOf('.');
  if (dot < 0) return false;
  const ext = filePath.slice(dot).toLowerCase();
  return ext in EXTENSION_MAP || (!!overrides && ext in overrides);
}

/**
 * Shopify OS 2.0 JSON template (`templates/*.json`) or section group
 * (`sections/*.json`) — these reference sections by `"type"`, so the Liquid
 * extractor links them. (config/ + locales/ JSON have no section refs.)
 */
export function isShopifyLiquidJson(filePath: string): boolean {
  // Allow nested template dirs (`templates/customers/login.json`), not just
  // top-level (`templates/product.json`).
  return /(^|\/)(templates|sections)\/.+\.json$/i.test(filePath);
}

/**
 * OTP application resource file: `<app>.app.src` (checked into every rebar3/
 * erlang.mk app) or its compiled `<app>.app`. Erlang TERMS, not forms — the
 * grammar parses them as top-level expressions, and the Erlang extractor's
 * application-tuple handler turns `{mod, {Mod, _}}` and `{applications, […]}`
 * into entry-module and dependency edges. Routed by full suffix because the
 * last-dot extension (`.src`) is far too generic for EXTENSION_MAP.
 */
export function isErlangAppFile(filePath: string): boolean {
  return /\.app(?:\.src)?$/i.test(filePath);
}

/**
 * Play Framework routes file: the extensionless `conf/routes` (and included
 * `conf/*.routes`). No grammar — route extraction is done by the Play framework
 * resolver, so it's processed through the no-grammar (`yaml`-style) path.
 */
export function isPlayRoutesFile(filePath: string): boolean {
  return (
    filePath === 'conf/routes' ||
    filePath.endsWith('/conf/routes') ||
    filePath.endsWith('.routes')
  );
}

/**
 * Grammar loading is a no-op since the wasm path was removed
 * (docs/design/kernel-only-extraction-plan.md, Phase 5): every grammar is
 * compiled into the native kernel and parsing goes through
 * `parseSourceTreeSync` (parse-tree.ts). These entry points are kept so the
 * public API (`src/index.ts`) and the many callers that "warm" grammars keep
 * working unchanged; they resolve immediately.
 */
export async function initGrammars(): Promise<void> {}

export async function loadGrammarsForLanguages(_languages: Language[]): Promise<void> {}

export async function loadAllGrammars(): Promise<void> {}

/**
 * Detect language from file extension.
 *
 * `overrides` is the project's validated custom extension → language map (from
 * `codegraph.json`); when present its mappings take precedence over the built-in
 * `EXTENSION_MAP`. Omitting it is byte-identical to the zero-config behavior.
 */
export function detectLanguage(filePath: string, source?: string, overrides?: Record<string, Language>): Language {
  // Play `conf/routes` has no grammar — route through the no-symbol path; the
  // Play framework resolver extracts route nodes from it.
  if (isPlayRoutesFile(filePath)) return 'yaml';
  const ext = filePath.substring(filePath.lastIndexOf('.')).toLowerCase();
  // Shopify OS 2.0 JSON templates / section groups → the Liquid extractor (it
  // links each section `"type"` to its `sections/<type>.liquid`).
  if (isShopifyLiquidJson(filePath)) return 'liquid';
  // OTP `.app`/`.app.src` resource files — Erlang terms the grammar parses as
  // top-level expressions (last-dot ext `.src` is too generic for the map).
  if (isErlangAppFile(filePath)) return 'erlang';
  const lang = (overrides && overrides[ext]) || EXTENSION_MAP[ext] || 'unknown';

  // A Flow-typed `.js` (`// @flow` in its leading comments) parses as TSX:
  // the JavaScript grammar can't read its annotations — `render(): React.Node`
  // cut a class short — and TypeScript's syntax covers most of Flow's.
  if ((lang === 'javascript' || lang === 'jsx') && source && hasFlowPragma(source)) return 'tsx';

  // .h files could be C, C++, or Objective-C — check source content
  if (lang === 'c' && ext === '.h' && source) {
    if (looksLikeCpp(source)) return 'cpp';
    if (looksLikeObjc(source)) return 'objc';
  }

  // `.inc` is PHP's include extension (Drupal) and Pascal/Delphi's too
  // (`{$I defs.inc}`: directive blocks, declaration fragments), so it is
  // decided per file by content (#2279). An explicit codegraph.json mapping
  // for `.inc` is the user's answer and is not second-guessed.
  if (lang === 'php' && ext === '.inc' && source && !(overrides && overrides[ext]) && looksLikePascalInclude(source)) {
    return 'pascal';
  }

  return lang;
}

/** A PHP open tag: `<?php` in any case, or the short echo `<?=` (never `<?xml`). */
const PHP_OPEN_TAG_RE = /<\?(?:php|=)/i;

// Building blocks for the Pascal line shapes below. A line start allows
// indentation and the BOM Windows editors write (a BOM'd Delphi include must
// not drop back to PHP). A section keyword's break runs to the end of its line
// and over blank / comment-only lines, up to the first declaration.
const PAS_LINE = String.raw`^[ \t\uFEFF]*`;
const PAS_NAME = String.raw`[a-z_]\w*(?:<[^>\n]*>)?`;
const PAS_QNAME = String.raw`[a-z_][\w.]*(?:<[^>\n]*>)?`;
const PAS_COMMENT = String.raw`(?:\/\/[^\n]*|\{[^$}\n][^}\n]*\}[ \t]*)?`;
const PAS_SECTION_BREAK = String.raw`[ \t]*${PAS_COMMENT}(?:\r?\n[ \t]*${PAS_COMMENT})+`;
const pascalLine = (shape: string): RegExp => new RegExp(PAS_LINE + shape, 'im');

/**
 * Line shapes only Pascal writes, any one of which makes an untagged `.inc`
 * Pascal. Each leans on Pascal's own punctuation, so the dialects that share a
 * keyword with it stay out: JavaScript `const x = 1;` / `function f() {`,
 * SourcePawn `function void (int client);`, C++ `const T X::Y = …`, VBScript
 * `Const X = 1` / `Function F(a)`, Smarty `{$var}`, Makefile `X := y`, prose
 * with `Begin` on a line of its own. Every pattern stays linear on a long
 * whitespace run: no run can be split two ways between neighbouring
 * quantifiers.
 */
const PASCAL_INCLUDE_SIGNALS: readonly RegExp[] = [
  // A compiler directive: a name and an argument (`{$IFDEF X}`, `{$DEFINE X}`,
  // `{$I file.inc}`, `{$WARN X OFF}`), a bare `{$ELSE}` / `{$ENDIF}` /
  // `{$IFEND}`, or a switch (`{$R-}`, `{$A+,B-}`) — or the `{%MainUnit x.pp}`
  // line Lazarus opens its include files with.
  pascalLine(String.raw`\{(?:\$(?:[a-z]\w*[ \t]+[^\s}]|(?:else|endif|ifend)[ \t]*\}|[a-z][+-][,}])|%MainUnit\b)`),
  // A routine header, closed by `;`: `procedure Foo;`, `procedure TForm1.Click(Sender: TObject);`,
  // `class constructor Create;` — and a function's result type after its
  // parameters (`function Bar(A: Integer): string;`; bare `function Bar;` is
  // the implementation-section short form). The parameter list stops at any
  // parenthesis, so a file of unclosed `procedure X(` lines stays linear.
  pascalLine(
    String.raw`(?:class[ \t]+)?(?:(?:procedure|constructor|destructor)[ \t]+${PAS_QNAME}[ \t]*(?:\([^()]*\)[ \t]*)?` +
      String.raw`|function[ \t]+${PAS_QNAME}[ \t]*(?:(?:\([^()]*\)[ \t]*)?:[ \t]*[\w.]+(?:<[^>\n]*>)?[ \t]*)?);`
  ),
  // `unit Foo;` and a `uses A, B;` clause.
  pascalLine(String.raw`unit[ \t]+[a-z_][\w.]*[ \t]*;`),
  pascalLine(String.raw`uses\s+[a-z_][\w.]*(?:\s*,\s*[a-z_][\w.]*)*\s*;`),
  // A `const` section, then `X = …` / `X: T = …`; or a typed constant on one
  // line (`const Max: Integer = 10;`). The type never holds a `:`.
  pascalLine(
    String.raw`(?:const|resourcestring)(?:${PAS_SECTION_BREAK}[a-z_]\w*[ \t]*(?::[^=;:\n]+)?=|[ \t]+[a-z_]\w*[ \t]*:[^=;:\n]+=)`
  ),
  // A `var` section: `G, H: Integer;`, on the keyword's line or below it.
  pascalLine(
    String.raw`(?:var|threadvar)(?:${PAS_SECTION_BREAK}|[ \t]+)[a-z_]\w*(?:[ \t]*,[ \t]*[a-z_]\w*)*[ \t]*:(?!:)[^;\n]*;`
  ),
  // A `type` section, then `TFoo =`; or `type TFoo = class…` (record /
  // interface / set of / array / procedure type) on one line.
  pascalLine(
    String.raw`type(?:${PAS_SECTION_BREAK}${PAS_NAME}[ \t]*=|[ \t]+${PAS_NAME}[ \t]*=[ \t]*(?:packed[ \t]+)?` +
      String.raw`(?:class|record|object|interface|dispinterface|set[ \t]+of|array|reference[ \t]+to|procedure|function)\b)`
  ),
];

/** A `begin` … `end;` block: both halves needed, so neither alone flips a file. */
const PASCAL_BEGIN_RE = pascalLine(String.raw`begin\b`);
const PASCAL_END_RE = pascalLine(String.raw`end[ \t]*[;.][ \t]*$`);

/**
 * Whether an `.inc` file is a Pascal include rather than a PHP one (#2279).
 *
 * A PHP include always opens a PHP tag somewhere, so a tag anywhere keeps the
 * file PHP. Without one, a Pascal-only line shape (`PASCAL_INCLUDE_SIGNALS`,
 * or a `begin` … `end;` pair) makes it Pascal. Anything else keeps the PHP
 * mapping — untagged text is inline HTML to PHP, so nothing is extracted —
 * rather than handing a C / assembly / POV-Ray / ASP `.inc` to the Pascal
 * grammar's error recovery.
 *
 * Deliberately per file, not "does this project have `.pas` files": the
 * answer depends only on the file's own bytes, so a full index, a sync of one
 * edited include, and a fresh re-index always agree — a project-level gate
 * would flip an untouched include whenever the last `.pas` file came or went.
 */
function looksLikePascalInclude(source: string): boolean {
  if (PHP_OPEN_TAG_RE.test(source)) return false;
  if (PASCAL_INCLUDE_SIGNALS.some((re) => re.test(source))) return true;
  return PASCAL_BEGIN_RE.test(source) && PASCAL_END_RE.test(source);
}

/** Whether a JavaScript file's leading comments carry Flow's `@flow` pragma (and not `@noflow`). */
export function hasFlowPragma(source: string): boolean {
  const head = source.slice(0, 4096).replace(/^#![^\n]*\n/, '');
  const lead = /^(?:\s*(?:\/\/[^\n]*|\/\*[\s\S]*?\*\/))*/.exec(head)?.[0] ?? '';
  return /@flow\b/.test(lead) && !/@noflow\b/.test(lead);
}

/**
 * A class/struct BASE CLAUSE — `struct Derived : Base {`, `class Foo final :
 * public Bar, private Baz {`, `struct D : ns::B<T> {` — which is never valid
 * C. In C the only thing that can follow `struct <tag>` is `{`, `;`, `*`, an
 * identifier (declarator), or a closing `)`: a bit-field's `:` sits after a
 * member NAME inside the body (`unsigned a : 3;`), a ternary's `:` is
 * separated from the tag by `)` / `*` / a declarator (`sizeof(struct foo) :
 * 0`), and a label such as `struct_end:` has no whitespace after `struct`. An
 * optional access specifier / `virtual` after the colon and an optional
 * `final` before it cover the spelled-out forms; the base may be scoped
 * (`ns::Base`) and carry template arguments, and must be followed by the
 * body's `{` or a `,` introducing the next base — prose like
 * `struct timeval: seconds and microseconds` inside a string never has that
 * terminator. Comments are stripped before the scan (see `looksLikeCpp`).
 */
const CPP_BASE_CLAUSE_RE =
  /\b(?:class|struct)\s+\w+\s*(?:final\s*)?:\s*(?:(?:public|protected|private|virtual)\s+)*[A-Za-z_][\w:]*(?:\s*<[^{};]*>)?\s*[{,]/;

/** Block and line comments, for a code-only scan. Lazy block match → linear. */
const C_COMMENT_RE = /\/\*[\s\S]*?\*\/|\/\/[^\n]*/g;

/**
 * Heuristic: does a .h file contain C++ constructs?
 *
 * Two passes. The first checks the first ~8KB for patterns that are unique to
 * C++ and never valid C. The second scans the FULL source for a class/struct
 * base clause (`CPP_BASE_CLAUSE_RE`): a large header with a long C-compatible
 * preamble — include guards, `#define`s, plain C typedefs — can put its only
 * C++ signal past the sample, and the cost of that miss is the C extractor
 * (classTypes: []) dropping the derived type entirely and minting a phantom
 * `function Base` from the base clause instead (#1592). The base-clause regex
 * is anchored on a `struct`/`class` keyword followed by a tag and a colon, a
 * shape with no C reading, so widening it to the whole file cannot drag a C
 * header over to C++.
 */
function looksLikeCpp(source: string): boolean {
  const sample = source.substring(0, 8192);
  // The `class MACRO Name : Base` / `class MACRO Name { … }` branch mirrors what
  // `blankCppExportMacros` recovers: an ALL-CAPS export/visibility macro
  // (`ENGINE_API`, `MYMODULE_API`, `*_EXPORT`, …) sitting between `class`/`struct`
  // and the type name. Without it, a header whose ONLY C++ signal is such a
  // macro-annotated class — common for lean Unreal-Engine types that carry just
  // `GENERATED_BODY()` and no explicit `public:`/`virtual` — is misdetected as C,
  // routed through the C extractor (which extracts no classes), and its class
  // definition silently vanishes. The two-token shape (`<KW> <MACRO> <Name>`
  // before a `[:{]`) never occurs in valid C, so this can't misclassify C headers.
  if (/\bnamespace\b|\bclass\s+\w+\s*[:{]|\b(?:class|struct)\s+[A-Z][A-Z0-9_]+\s+\w+\s*(?:final\s*)?[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)/.test(sample)) {
    return true;
  }
  // Plain `struct Derived : Base` (no export macro, no `class` keyword, no
  // explicit access section) — the #1159 branch above only recognizes the
  // macro-annotated form. Scanned over the whole file, not the sample, with
  // comments removed so a doc comment's prose (`struct foo: x, y`) can't
  // flip a C header.
  return CPP_BASE_CLAUSE_RE.test(source.replace(C_COMMENT_RE, ' '));
}

/**
 * Heuristic: does a .h file contain Objective-C constructs?
 */
function looksLikeObjc(source: string): boolean {
  const sample = source.substring(0, 8192);
  return /@(?:interface|implementation|protocol|synthesize)\b/.test(sample);
}

/**
 * Whether a language has a tree-sitter grammar of its own.
 *
 * Narrower than {@link isLanguageSupported}, which also answers true for the
 * formats handled by custom extractors (SFCs, Liquid, Razor, YAML, XML,
 * properties) — those have extraction but no grammar, so anything that needs to
 * PARSE the file (the viewer's syntax classification, for one) has to ask this
 * instead.
 */
export function hasTreeSitterGrammar(language: string | undefined | null): boolean {
  return !!language && GRAMMAR_LANGUAGES.has(language as GrammarLanguage);
}

/**
 * Check if a language is supported (has a grammar defined).
 * Returns true if the grammar exists, even if not yet loaded.
 */
export function isLanguageSupported(language: Language): boolean {
  if (language === 'svelte') return true; // custom extractor (script block delegation)
  if (language === 'vue') return true; // custom extractor (script block delegation)
  if (language === 'astro') return true; // custom extractor (frontmatter/script block delegation)
  if (language === 'liquid') return true; // custom regex extractor
  if (language === 'razor') return true; // custom RazorExtractor (.cshtml/.razor markup)
  if (language === 'yaml') return true; // file-level tracking only; Drupal routing extraction via framework resolver
  if (language === 'twig') return true; // file-level tracking only
  if (language === 'xml') return true; // MyBatis mapper extractor
  if (language === 'properties') return true; // Spring config keys
  if (language === 'markdown') return true; // custom documentation extractor
  if (language === 'unknown') return false;
  return GRAMMAR_LANGUAGES.has(language as GrammarLanguage);
}

/**
 * Check if a grammar has been loaded and is ready for parsing.
 */
export function isGrammarLoaded(language: Language): boolean {
  // No loading step exists any more; a supported language is always ready.
  return isLanguageSupported(language);
}

/**
 * Languages tracked at the file-record level only: parsing emits zero symbol
 * nodes, but the file is still stored (and framework resolvers may add per-file
 * references later, e.g. Drupal routing yml, Spring `@Value` against
 * application.properties). This is the canonical set behind the no-symbol
 * branch in `tree-sitter.ts`; `xml` is intentionally excluded because its
 * MyBatis extractor emits a file node. Callers use this to count such files as
 * indexed rather than skipped, so it must stay in sync with that branch.
 */
export function isFileLevelOnlyLanguage(language: Language): boolean {
  return language === 'yaml' || language === 'twig' || language === 'properties';
}

/**
 * Get all supported languages (those with grammar definitions).
 */
export function getSupportedLanguages(): Language[] {
  return [...GRAMMAR_LANGUAGES, 'svelte', 'vue', 'astro', 'liquid', 'markdown'];
}

/**
 * Get language display name
 */
export function getLanguageDisplayName(language: Language): string {
  const names: Record<Language, string> = {
    typescript: 'TypeScript',
    javascript: 'JavaScript',
    tsx: 'TypeScript (TSX)',
    jsx: 'JavaScript (JSX)',
    python: 'Python',
    go: 'Go',
    rust: 'Rust',
    r: 'R',
    java: 'Java',
    c: 'C',
    cpp: 'C++',
    csharp: 'C#',
    razor: 'Razor/Blazor',
    php: 'PHP',
    ruby: 'Ruby',
    swift: 'Swift',
    kotlin: 'Kotlin',
    dart: 'Dart',
    svelte: 'Svelte',
    vue: 'Vue',
    astro: 'Astro',
    liquid: 'Liquid',
    pascal: 'Pascal / Delphi',
    scala: 'Scala',
    lua: 'Lua',
    luau: 'Luau',
    objc: 'Objective-C',
    solidity: 'Solidity',
    nix: 'Nix',
    yaml: 'YAML',
    twig: 'Twig',
    xml: 'XML',
    properties: 'Java properties',
    markdown: 'Markdown',
    cfml: 'CFML',
    cfscript: 'CFScript',
    cfquery: 'CFQuery (SQL)',
    cobol: 'COBOL',
    vbnet: 'Visual Basic .NET',
    erlang: 'Erlang',
    terraform: 'Terraform',
    arkts: 'ArkTS',
    unknown: 'Unknown',
  };
  return names[language] || language;
}
