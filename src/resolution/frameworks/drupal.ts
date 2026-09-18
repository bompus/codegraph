/**
 * Drupal Framework Resolver
 *
 * Supports Drupal 8/9/10/11 (Composer-based projects). Drupal 7 is not supported.
 *
 * ## What this resolver does
 *
 * 1. **Detection** — reads composer.json and checks for any `drupal/*` dependency in
 *    `require` or `require-dev`.
 *
 * 2. **Route extraction** — parses `*.routing.yml` files and emits `route` nodes for each
 *    Drupal route, with `references` edges to the `_controller`, `_form`, or entity handler
 *    class/method.
 *
 * 3. **Hook detection** — scans `.module`, `.install`, `.theme`, `.inc`, and `.php` files
 *    for Drupal hook implementations. Three strategies are used:
 *      a. Docblock: `@Implements hook_X()` → precise, no false positives.
 *      b. Name pattern: function `{moduleName}_{hookSuffix}()` → catches hooks without
 *         docblocks but may produce false positives on helper functions.
 *      c. Attribute: `#[Hook('name')]` on class methods or hook classes → the Drupal 11
 *         attribute era, where nearly all hooks are attributes on `src/Hook/*.php`
 *         classes instead of procedural `module_hook()` functions.
 *    Detected hooks emit an `UnresolvedRef` from the implementing function/method/class
 *    node to the canonical `hook_X` name, linking implementations to the hook when
 *    `codegraph_callers` is invoked.
 *
 * ## Design decisions (review in future iterations)
 *
 * - Hook graph resolution (v1): hook references are stored as UnresolvedRef pointing to the
 *   canonical `hook_X` name. If Drupal core is indexed, these will resolve to core hook
 *   definitions. Without core, they remain unresolved but are still searchable via
 *   `codegraph_search("form_alter")`. Full hook-node creation (virtual nodes for every hook)
 *   is deferred to a future iteration.
 *
 * - Services / plugins (out of scope for v1): `*.services.yml` service definitions and plugin
 *   annotations (`@Block`, `@FormElement`, etc.) are not extracted. Add a TODO below when
 *   ready to implement.
 *
 * - Twig templates (out of scope for v1): `.twig` files are tracked as file nodes but no
 *   symbol extraction is performed (no tree-sitter Twig grammar). Implement when a Twig
 *   grammar WASM is available.
 *
 * ## TODOs for future iterations
 *
 * - TODO: Extract service definitions from `*.services.yml` files (class → service-id edges).
 * - TODO: Extract plugin annotations (`@Block`, `@FormElement`, `@Field`, etc.) from PHP
 *   docblocks and emit plugin nodes with references to the annotated class.
 * - TODO: Add Twig symbol extraction when a tree-sitter Twig grammar becomes available.
 * - TODO: Improve hook resolution: create virtual `hook_*` nodes so `codegraph_callers`
 *   returns all implementations even when Drupal core is not indexed.
 */

import { generateNodeId } from '../../extraction/tree-sitter-helpers';
import { Node } from '../../types';
import { FrameworkResolver, ResolutionContext, ResolvedRef, UnresolvedRef } from '../types';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Parse the last PHP namespace segment from a FQCN like `\Drupal\mymodule\Controller\Foo`.
 * Returns `null` for strings that don't look like a FQCN.
 */
function lastSegment(fqcn: string): string | null {
  const clean = fqcn.replace(/^\\+/, '').trim();
  if (!clean.includes('\\')) return null;
  const parts = clean.split('\\');
  return parts[parts.length - 1] ?? null;
}

/**
 * Derive the Drupal module name from a file path.
 * e.g. `web/modules/custom/my_module/my_module.module` → `my_module`
 */
function moduleNameFromPath(filePath: string): string | null {
  const match = filePath.match(/\/([^/]+)\.[^./]+$/);
  return match ? match[1]! : null;
}

// ---------------------------------------------------------------------------
// Route extraction helpers
// ---------------------------------------------------------------------------

/**
 * Extract route nodes and handler references from a Drupal `*.routing.yml` file.
 *
 * Drupal routing YAML format:
 *
 *   route.name:
 *     path: '/some/path'
 *     defaults:
 *       _controller: '\Drupal\module\Controller\MyController::method'
 *       _form: '\Drupal\module\Form\MyForm'
 *       _title: 'Page title'
 *     requirements:
 *       _permission: 'access content'
 *     methods: [GET, POST]   # optional
 */
function extractDrupalRoutes(
  filePath: string,
  content: string
): { nodes: Node[]; references: UnresolvedRef[] } {
  const nodes: Node[] = [];
  const references: UnresolvedRef[] = [];
  const now = Date.now();

  const lines = content.split('\n');

  type PendingRoute = { name: string; lineNum: number };
  let pending: PendingRoute | null = null;
  let currentPath: string | null = null;
  let handlerRefs: string[] = [];
  let methods: string[] = [];

  const flushRoute = () => {
    if (!pending || !currentPath) return;

    const methodTag = methods.length > 0 ? ` [${methods.join(',')}]` : '';
    const routeNode: Node = {
      id: `route:${filePath}:${pending.lineNum}:${currentPath}`,
      kind: 'route',
      name: `${currentPath}${methodTag}`,
      qualifiedName: `${filePath}::${pending.name}`,
      filePath,
      startLine: pending.lineNum,
      endLine: pending.lineNum,
      startColumn: 0,
      endColumn: 0,
      language: 'yaml',
      updatedAt: now,
    };
    nodes.push(routeNode);

    for (const handler of handlerRefs) {
      references.push({
        fromNodeId: routeNode.id,
        referenceName: handler,
        referenceKind: 'references',
        line: pending.lineNum,
        column: 0,
        filePath,
        language: 'yaml',
      });
    }
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const trimmed = line.trim();

    if (!trimmed || trimmed.startsWith('#')) continue;

    // Top-level route name: no leading whitespace, ends with a colon (no value after)
    if (/^\S.*:\s*$/.test(line) && !/^\s/.test(line)) {
      flushRoute();
      pending = { name: trimmed.slice(0, -1).trim(), lineNum: i + 1 };
      currentPath = null;
      handlerRefs = [];
      methods = [];
      continue;
    }

    // path: '/some/path'
    const pathMatch = trimmed.match(/^path:\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$/);
    if (pathMatch) {
      currentPath = pathMatch[1]!.trim();
      continue;
    }

    // _controller: '\Drupal\...\Class::method'
    const controllerMatch = trimmed.match(/^_controller:\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$/);
    if (controllerMatch) {
      handlerRefs.push(controllerMatch[1]!.trim());
      continue;
    }

    // _form: '\Drupal\...\Form\MyForm'
    const formMatch = trimmed.match(/^_form:\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$/);
    if (formMatch) {
      handlerRefs.push(formMatch[1]!.trim());
      continue;
    }

    // _entity_form / _entity_list / _entity_view: entity.type
    const entityMatch = trimmed.match(/^_(entity_form|entity_list|entity_view):\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$/);
    if (entityMatch) {
      handlerRefs.push(entityMatch[2]!.trim());
      continue;
    }

    // methods: [GET, POST]  or  methods: [GET]
    const methodsMatch = trimmed.match(/^methods:\s*\[([^\]]+)\]/);
    if (methodsMatch) {
      methods = methodsMatch[1]!.split(',').map((m) => m.trim().toUpperCase()).filter(Boolean);
      continue;
    }
  }

  flushRoute();
  return { nodes, references };
}

// ---------------------------------------------------------------------------
// Hook detection helpers
// ---------------------------------------------------------------------------

const HOOK_FILE_EXTENSIONS = ['.module', '.install', '.theme', '.inc'];

function isDrupalHookFile(filePath: string): boolean {
  return HOOK_FILE_EXTENSIONS.some((ext) => filePath.endsWith(ext));
}

/**
 * Extract hook implementation references from a Drupal PHP file.
 *
 * Strategy A (primary): look for docblocks containing `Implements hook_X().`
 * followed immediately by the function definition. This is the Drupal coding
 * standard and is precise.
 *
 * Strategy B (fallback): for functions whose name starts with `{moduleName}_`,
 * treat the suffix as the hook name. Catches hooks without docblocks but may
 * produce false positives on non-hook helper functions.
 *
 * Strategy C (Drupal 11): `#[Hook('name')]` attributes — on a method the
 * attributed method is the implementation; on a class the `method:`/second arg
 * or `__invoke` method is. Only core's `Drupal\Core\Hook\Attribute\Hook`
 * counts — `LegacyHook`, `RemoveHook`, `ReorderHook`, hux's `Hook`, etc. are
 * rejected by exact-name matching.
 *
 * Each detected hook emits an UnresolvedRef from the implementing function node
 * (identified by computing the same ID tree-sitter would generate) to the
 * canonical hook name, e.g. `hook_form_alter`.
 */
function extractDrupalHooks(
  filePath: string,
  content: string
): { nodes: Node[]; references: UnresolvedRef[] } {
  const references: UnresolvedRef[] = [];

  // Strategy C runs first (Drupal 11): `#[Hook('name')]` attributes on class
  // methods or on hook classes under `src/Hook/`. Scan RAW content — PHP
  // treats `#` as a line comment, so stripCommentsForRegex would blank every
  // `#[…]` group. Gated cheaply on the two bytes that must both be present.
  const attrScan =
    content.includes('Hook') && content.includes('#[')
      ? scanPhpHookAttributes(content)
      : null;

  // Build a map of function name → 1-indexed line number for all top-level functions.
  // This mirrors tree-sitter's line numbering so we can reconstruct node IDs.
  const funcLineMap = new Map<string, number>();
  const funcDef = /^function\s+(\w+)\s*\(/gm;
  let fm: RegExpExecArray | null;
  while ((fm = funcDef.exec(content)) !== null) {
    const name = fm[1]!;
    if (!funcLineMap.has(name)) {
      // line = number of newlines before match start + 1
      funcLineMap.set(name, content.slice(0, fm.index).split('\n').length);
    }
  }
  // An attributed declaration's node starts at its first `#[` line, not at
  // `function` — the regex above records the keyword line, so override it for
  // prefixed decls (and pick up same-line `#[…] function` decls the
  // line-anchored regex misses). First-match wins for repeats.
  if (attrScan) {
    for (const decl of attrScan.decls) {
      if (decl.kind !== 'function') continue;
      if (decl.startIndex !== decl.declIndex || !funcLineMap.has(decl.name)) {
        funcLineMap.set(decl.name, lineAt(content, decl.startIndex));
      }
    }
  }

  // nodeId|hookName pairs already emitted, so strategies A/B don't re-emit a
  // ref the attribute pass produced for the same declaration.
  const emitted = new Set<string>();
  const emitRef = (
    hookName: string,
    kind: 'function' | 'method' | 'class',
    name: string,
    lineNum: number,
    refLine = lineNum
  ) => {
    const nodeId = generateNodeId(filePath, kind, name, lineNum);
    const key = `${nodeId}|${hookName}`;
    if (emitted.has(key)) return;
    emitted.add(key);
    references.push({
      fromNodeId: nodeId,
      referenceName: hookName,
      referenceKind: 'references',
      line: refLine,
      column: 0,
      filePath,
      language: 'php',
    });
  };

  const emitHookRef = (hookName: string, funcName: string) => {
    const lineNum = funcLineMap.get(funcName);
    if (lineNum === undefined) return;
    emitRef(hookName, 'function', funcName, lineNum);
  };

  if (attrScan) {
    for (const impl of attrScan.impls) {
      const target = hookAttributeTarget(impl, attrScan.decls);
      emitRef(
        `hook_${impl.hook}`,
        target.kind,
        target.name,
        lineAt(content, target.startIndex),
        impl.line
      );
    }
  }

  // Strategy A: docblock `Implements hook_X().` followed by function definition.
  // Blank lines and single-line `#[…]` attribute groups may sit between the
  // docblock and the function — D11 transitional modules put `#[Hook]` /
  // `#[LegacyHook]` there.
  const docblockPattern =
    /\/\*\*[\s\S]*?(?:@|\*\s+)Implements\s+(hook_\w+)\s*\(\)[\s\S]*?\*\/\s*\n(?:[^\S\n]*(?:#\[[^\]\n]*\])?[^\S\n]*\n)*function\s+(\w+)\s*\(/g;
  const docblockMatched = new Set<string>();
  let match: RegExpExecArray | null;
  while ((match = docblockPattern.exec(content)) !== null) {
    const [, hookName, funcName] = match;
    emitHookRef(hookName!, funcName!);
    docblockMatched.add(funcName!);
  }

  // Strategy B: fallback name-pattern matching for functions without docblocks.
  // Only applies to functions whose name starts with {moduleName}_ and that were
  // not already matched by Strategy A.
  const moduleName = moduleNameFromPath(filePath);
  if (moduleName) {
    const prefix = moduleName + '_';
    for (const [funcName] of funcLineMap) {
      if (docblockMatched.has(funcName)) continue;
      if (!funcName.startsWith(prefix)) continue;
      const hookSuffix = funcName.slice(prefix.length);
      if (!hookSuffix) continue;
      // Emit a reference to hook_{suffix} — the resolver will link it if the
      // hook is defined somewhere in the indexed graph (e.g. Drupal core).
      emitHookRef(`hook_${hookSuffix}`, funcName);
    }
  }

  return { nodes: [], references };
}

// ---------------------------------------------------------------------------
// Hook attribute helpers (Drupal 11 `#[Hook]` era)
// ---------------------------------------------------------------------------

/**
 * The one core attribute this resolver keys on. Contrib/core lookalikes —
 * `Drupal\hux\Attribute\Hook`, `LegacyHook`, `RemoveHook`, `ReorderHook`,
 * `StopProceduralHookScan` — are rejected by exact-name matching in
 * {@link isCoreHookAttrName}.
 */
const HOOK_ATTRIBUTE_FQCN = 'Drupal\\Core\\Hook\\Attribute\\Hook';

/** Modifiers that may sit between an attribute block and its declaration. */
const PHP_DECL_MODIFIERS = new Set([
  'public',
  'protected',
  'private',
  'static',
  'abstract',
  'final',
  'readonly',
]);

/** Keywords that open a type body whose `function` members are methods. */
const PHP_TYPE_KEYWORDS = new Set(['class', 'interface', 'trait', 'enum']);

interface PhpAttr {
  /** Attribute name exactly as written: `Hook`, `\Drupal\...\Hook`, aliases. */
  name: string;
  /** Raw text inside `(...)` — `null` when the attribute takes no args. */
  args: string | null;
  /** Index of the `#[` opening this attribute's group. */
  groupStart: number;
}

interface PhpDecl {
  /** `method` when inside a class-like body, `function` at top level, `class`
   *  for class-like declarations themselves. */
  kind: 'function' | 'method' | 'class';
  name: string;
  /** Index where the declaration's node starts: the first `#[` of its
   *  attribute block, else the first modifier, else the `function`/`class`
   *  keyword. The kernel's node position starts at the attribute block. */
  startIndex: number;
  /** Index of the `function`/`class` keyword itself. */
  declIndex: number;
  /** Index just past the closing `}` of a class body (class decls only). */
  bodyEnd?: number;
  /** Enclosing class name for method decls (anonymous classes → undefined). */
  ownerName?: string;
}

interface HookAttrImpl {
  /** Short hook name — the `hook_` prefix is added when the ref is emitted. */
  hook: string;
  /** `method:` named arg / second positional arg (class-level attributes). */
  methodArg: string | null;
  /** The declaration the attribute block attaches to. */
  decl: PhpDecl;
  /** 1-indexed line of the `#[` group holding the Hook attribute. */
  line: number;
}

/** 1-indexed line number of `index` in `content` (same convention as above). */
function lineAt(content: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index; i++) if (content[i] === '\n') line++;
  return line;
}

/**
 * If `content[i]` opens a string literal, comment, or heredoc/nowdoc, return
 * the index just past it; else `null`. Shared by the declaration scanner and
 * the balanced-paren matcher so `#[`, `)`, and `use` inside strings/comments
 * are never mistaken for real tokens.
 */
function skipPhpStringOrComment(content: string, i: number): number | null {
  const n = content.length;
  const c = content[i]!;
  const c2 = content[i + 1] ?? '';

  // // line comment
  if (c === '/' && c2 === '/') {
    let j = i + 2;
    while (j < n && content[j] !== '\n') j++;
    return j;
  }

  // /* … */ block comment
  if (c === '/' && c2 === '*') {
    let j = i + 2;
    while (j < n && !(content[j] === '*' && content[j + 1] === '/')) j++;
    return j < n ? j + 2 : n;
  }

  // # line comment — `#[` is an attribute opener, NOT a comment (PHP 8).
  if (c === '#' && c2 !== '[') {
    let j = i + 1;
    while (j < n && content[j] !== '\n') j++;
    return j;
  }

  // '…' / "…" / `…` strings. PHP single/double-quoted strings may span lines.
  if (c === "'" || c === '"' || c === '`') {
    let j = i + 1;
    while (j < n && content[j] !== c) {
      if (content[j] === '\\' && j + 1 < n) {
        j += 2;
        continue;
      }
      j++;
    }
    return j < n ? j + 1 : n;
  }

  // Heredoc / nowdoc: <<<LABEL / <<<"LABEL" / <<<'LABEL' … LABEL at line start.
  if (c === '<' && c2 === '<' && content[i + 2] === '<') {
    const m = /^<<<[ \t]*(['"]?)([A-Za-z_]\w*)\1[^\S\n]*\n/.exec(content.slice(i));
    if (m) {
      const label = m[2]!;
      let j = i + m[0].length;
      while (j < n) {
        const nl = content.indexOf('\n', j);
        const lineStart = nl < 0 ? n : nl + 1;
        if (content.startsWith(label, lineStart)) {
          const after = content[lineStart + label.length] ?? '';
          if (after === '' || /[\s;,)\]]/.test(after)) return lineStart + label.length;
        }
        if (nl < 0) return n;
        j = lineStart;
      }
      return n;
    }
  }

  return null;
}

/** Advance past whitespace and comments (PHP extras — transparent tokens). */
function skipPhpWsAndComments(content: string, i: number): number {
  const n = content.length;
  let j = i;
  while (j < n) {
    const c = content[j]!;
    if (c === ' ' || c === '\t' || c === '\n' || c === '\r') {
      j++;
      continue;
    }
    const skipped = skipPhpStringOrComment(content, j);
    if (skipped === null || content[j] === "'" || content[j] === '"' || content[j] === '`') break;
    j = skipped;
  }
  return j;
}

/**
 * Index just past the `)` matching the `(` at `open`, or -1 when unbalanced.
 * Strings, comments, and heredocs inside the parens are skipped.
 */
function matchPhpParen(content: string, open: number): number {
  const n = content.length;
  let depth = 1;
  let i = open + 1;
  while (i < n) {
    const skipped = skipPhpStringOrComment(content, i);
    if (skipped !== null) {
      i = skipped;
      continue;
    }
    const c = content[i]!;
    if (c === '(') depth++;
    else if (c === ')') {
      depth--;
      if (depth === 0) return i;
    }
    i++;
  }
  return -1;
}

const PHP_ATTR_NAME_RE = /^\\?[A-Za-z_\u0080-\uffff][\w\u0080-\uffff]*(?:\\[A-Za-z_\u0080-\uffff][\w\u0080-\uffff]*)*/;

/**
 * Parse one `#[…]` attribute group starting at the `#` (`content[start]`).
 * Attributes are comma-separated inside a single group (`#[A, B('x')]`).
 * Returns the parsed attributes and the index just past `]`, or `null` when
 * the text after `#[` isn't a well-formed attribute group.
 */
function parsePhpAttrGroup(
  content: string,
  start: number
): { attrs: { name: string; args: string | null }[]; end: number } | null {
  const attrs: { name: string; args: string | null }[] = [];
  let pos = start + 2;

  for (;;) {
    pos = skipPhpWsAndComments(content, pos);
    const m = PHP_ATTR_NAME_RE.exec(content.slice(pos, pos + 512));
    if (!m) return null;
    const name = m[0];
    pos += name.length;
    pos = skipPhpWsAndComments(content, pos);

    let args: string | null = null;
    if (content[pos] === '(') {
      const close = matchPhpParen(content, pos);
      if (close < 0) return null;
      args = content.slice(pos + 1, close);
      pos = skipPhpWsAndComments(content, close + 1);
    }
    attrs.push({ name, args });

    if (content[pos] === ',') {
      pos++;
      continue;
    }
    if (content[pos] === ']') return { attrs, end: pos + 1 };
    return null;
  }
}

/** Split `args` on top-level commas, respecting nested ()[]{} and strings. */
function splitPhpArgs(args: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  let i = 0;
  const n = args.length;
  while (i < n) {
    const skipped = skipPhpStringOrComment(args, i);
    if (skipped !== null) {
      i = skipped;
      continue;
    }
    const c = args[i]!;
    if (c === '(' || c === '[' || c === '{') depth++;
    else if (c === ')' || c === ']' || c === '}') depth--;
    else if (c === ',' && depth === 0) {
      parts.push(args.slice(start, i));
      start = i + 1;
    }
    i++;
  }
  parts.push(args.slice(start));
  return parts;
}

/** Value of a PHP string literal arg, or `null` for non-literal expressions. */
function phpStringArg(raw: string | undefined): string | null {
  if (!raw) return null;
  const t = raw.trim();
  const q = t[0];
  if ((q !== "'" && q !== '"') || t.length < 2 || t[t.length - 1] !== q) return null;
  return t.slice(1, -1);
}

/**
 * Pull `hook` and `method` out of a `#[Hook(…)]` argument list.
 * Constructor: `Hook(string $hook, string $method = '', ?string $module = null,
 * ?OrderInterface $order = null)` — the hook is the first positional or the
 * `hook:` named arg; the method the second positional or `method:`.
 */
function parseHookAttrArgs(args: string | null): { hook: string | null; method: string | null } {
  if (args === null) return { hook: null, method: null };
  const positional: string[] = [];
  const named = new Map<string, string>();
  for (const part of splitPhpArgs(args)) {
    if (!part.trim()) continue;
    const m = /^([A-Za-z_]\w*)\s*:\s*([\s\S]+)$/.exec(part.trim());
    if (m) named.set(m[1]!, m[2]!);
    else positional.push(part);
  }
  const hook = phpStringArg(named.get('hook')) ?? phpStringArg(positional[0]);
  const method = phpStringArg(named.get('method')) ?? phpStringArg(positional[1]);
  return { hook, method };
}

/**
 * True when an attribute name token is core's `Hook` — exactly `Hook` (bound
 * through a `use` import when the file imports one, loose when unimported) or
 * the fully-qualified `Drupal\Core\Hook\Attribute\Hook` with optional leading
 * `\`. `LegacyHook`, `Hookable`, and `Drupal\hux\Attribute\Hook` all fail.
 */
function isCoreHookAttrName(name: string, useAliases: Map<string, string>): boolean {
  const clean = name.replace(/^\\+/, '');
  if (clean.includes('\\')) return clean === HOOK_ATTRIBUTE_FQCN;
  const bound = useAliases.get(clean);
  if (bound !== undefined) return bound === HOOK_ATTRIBUTE_FQCN;
  return clean === 'Hook';
}

/**
 * Add the import bindings of one `use` statement (the text between `use` and
 * `;`) to `aliases`: local alias → FQCN. Handles `use A\B;`, `use A\B as C;`,
 * and grouped `use A\{B, C as D};`. `use function`/`use const` are skipped —
 * they bind functions/constants, not the class names attributes resolve by.
 */
function collectUseAliases(stmt: string, aliases: Map<string, string>): void {
  const text = stmt.trim();
  if (/^(?:function|const)\s/i.test(text)) return;
  // Expand one level of grouped imports: `A\{B, C}` → `A\B`, `A\C`.
  const grouped = /^([^\\{]*\\)\{([\s\S]*)\}$/.exec(text);
  const items: string[] = [];
  if (grouped) {
    for (const item of grouped[2]!.split(',')) items.push(grouped[1]! + item.trim());
  } else {
    for (const item of text.split(',')) items.push(item.trim());
  }
  for (const item of items) {
    const m = /^\\?([A-Za-z_\w\\]*?)\s*(?:\s+as\s+(\w+))?$/i.exec(item);
    if (!m || !m[1]) continue;
    const fqcn = m[1].replace(/^\\+/, '');
    const alias = m[2] ?? fqcn.split('\\').pop()!;
    if (alias) aliases.set(alias, fqcn);
  }
}

/**
 * Scan a PHP file for `#[Hook('name')]` attributes — the Drupal 11 hook
 * mechanism — and collect every named declaration's position so a class-level
 * attribute can pinpoint its implementing method (`method:` arg or `__invoke`).
 *
 * Single regex-free pass over RAW source: a small state machine skips strings,
 * `//` / `/* *\/` / `#` comments and heredocs, tracks class-body braces
 * (method vs function), and parses each `#[…]` group's attributes with
 * balanced-paren args. The contiguous `#[` groups plus modifiers preceding a
 * `function`/`class` keyword form that declaration's prefix — the kernel
 * starts an attributed declaration's node at its first `#[` line.
 */
function scanPhpHookAttributes(content: string): { decls: PhpDecl[]; impls: HookAttrImpl[] } {
  const decls: PhpDecl[] = [];
  const impls: HookAttrImpl[] = [];
  const useAliases = new Map<string, string>();

  /** Index of the first token of the pending declaration prefix (attribute
   *  block or modifier run); -1 when no prefix is open. */
  let prefixStart = -1;
  /** Attributes parsed since `prefixStart` opened. */
  let pendingAttrs: PhpAttr[] = [];
  /** Brace stack: `isClass` marks class/interface/trait/enum bodies. */
  const braces: { isClass: boolean; decl: PhpDecl | null }[] = [];
  /** Set when a class-like keyword was just parsed; consumed by its `{`. */
  let typeBracePending = false;
  let pendingTypeDecl: PhpDecl | null = null;
  /** Inside `<?php … ?>`; `#[` outside PHP tags is template text. */
  let inPhp = content.startsWith('<?') || !content.includes('<?');

  const attachAttributes = (decl: PhpDecl) => {
    for (const attr of pendingAttrs) {
      if (!isCoreHookAttrName(attr.name, useAliases)) continue;
      const { hook, method } = parseHookAttrArgs(attr.args);
      if (!hook || !/^[A-Za-z_]\w*$/.test(hook)) continue;
      impls.push({ hook, methodArg: method, decl, line: lineAt(content, attr.groupStart) });
    }
  };

  const resetPrefix = () => {
    prefixStart = -1;
    pendingAttrs = [];
  };

  const n = content.length;
  let i = 0;
  while (i < n) {
    // Outside PHP tags everything is template text — skip until `<?`.
    if (!inPhp) {
      if (content.startsWith('<?', i)) inPhp = true;
      i++;
      continue;
    }

    const skipped = skipPhpStringOrComment(content, i);
    if (skipped !== null) {
      i = skipped;
      continue;
    }

    const c = content[i]!;

    // `?>` ends PHP mode (when not inside a string/comment, handled above).
    if (c === '?' && content[i + 1] === '>') {
      inPhp = false;
      resetPrefix();
      i += 2;
      continue;
    }

    // Attribute group opener.
    if (c === '#' && content[i + 1] === '[') {
      const group = parsePhpAttrGroup(content, i);
      if (!group) {
        i += 2;
        resetPrefix();
        continue;
      }
      if (prefixStart < 0) prefixStart = i;
      for (const attr of group.attrs) pendingAttrs.push({ ...attr, groupStart: i });
      i = group.end;
      continue;
    }

    // Identifier-ish token.
    if (/[A-Za-z_\u0080-\uffff]/.test(c)) {
      const m = /^[A-Za-z_\u0080-\uffff][\w\u0080-\uffff]*/.exec(content.slice(i, i + 128))!;
      const word = m[0];
      const end = i + word.length;

      if (PHP_DECL_MODIFIERS.has(word)) {
        if (prefixStart < 0) prefixStart = i;
        i = end;
        continue;
      }

      if (word === 'function') {
        // Optional `&` (returns by reference), then the name.
        let j = skipPhpWsAndComments(content, end);
        if (content[j] === '&') j = skipPhpWsAndComments(content, j + 1);
        const nm = /^[A-Za-z_\u0080-\uffff][\w\u0080-\uffff]*/.exec(content.slice(j, j + 128));
        if (nm) {
          const inClass = braces.length > 0 && braces[braces.length - 1]!.isClass;
          const decl: PhpDecl = {
            kind: inClass ? 'method' : 'function',
            name: nm[0],
            startIndex: prefixStart >= 0 ? prefixStart : i,
            declIndex: i,
            ownerName: inClass ? braces[braces.length - 1]!.decl?.name : undefined,
          };
          decls.push(decl);
          if (pendingAttrs.length) attachAttributes(decl);
        }
        typeBracePending = false;
        pendingTypeDecl = null;
        resetPrefix();
        i = end;
        continue;
      }

      if (PHP_TYPE_KEYWORDS.has(word)) {
        // `Foo::class` is a constant, not a declaration — check for `::`
        // before the keyword (whitespace between `::` and `class` is legal).
        let k = i - 1;
        while (k >= 0 && (content[k] === ' ' || content[k] === '\t')) k--;
        if (content[k] === ':' && content[k - 1] === ':') {
          resetPrefix();
          i = end;
          continue;
        }
        const j = skipPhpWsAndComments(content, end);
        const nm = /^[A-Za-z_\u0080-\uffff][\w\u0080-\uffff]*/.exec(content.slice(j, j + 128));
        const decl: PhpDecl | null = nm
          ? {
              kind: 'class',
              name: nm[0],
              startIndex: prefixStart >= 0 ? prefixStart : i,
              declIndex: i,
            }
          : null; // anonymous class — still opens a class body
        if (decl) {
          decls.push(decl);
          if (pendingAttrs.length) attachAttributes(decl);
        }
        typeBracePending = true;
        pendingTypeDecl = decl;
        resetPrefix();
        i = end;
        continue;
      }

      if (word === 'use' && braces.length === 0) {
        // File-level import — closure `use ($x)` contains `(`, bail on it.
        const semi = content.indexOf(';', end);
        const paren = content.indexOf('(', end);
        if (semi > 0 && (paren < 0 || paren > semi) && semi - end < 512) {
          collectUseAliases(content.slice(end, semi), useAliases);
          resetPrefix();
          i = semi + 1;
          continue;
        }
      }

      // Any other token breaks the pending declaration prefix.
      resetPrefix();
      i = end;
      continue;
    }

    if (c === '{') {
      braces.push({ isClass: typeBracePending, decl: pendingTypeDecl });
      typeBracePending = false;
      pendingTypeDecl = null;
      i++;
      continue;
    }
    if (c === '}') {
      const open = braces.pop();
      if (open?.decl) open.decl.bodyEnd = i;
      i++;
      continue;
    }
    if (c === ' ' || c === '\t' || c === '\n' || c === '\r') {
      i++;
      continue;
    }
    if (c === ';') {
      typeBracePending = false;
      pendingTypeDecl = null;
    }
    // Any other punctuation (`$`, `=`, `(`, `,` …) ends a declaration prefix —
    // but never a pending type body: `class Foo implements A, B {` has commas.
    resetPrefix();
    i++;
  }

  return { decls, impls };
}

/**
 * The node a `#[Hook]` ref should hang off: the attributed method itself for
 * method-level attributes; for class-level ones the `method:`/second-arg or
 * `__invoke` method when it exists in the class body, else the class node.
 */
function hookAttributeTarget(
  impl: HookAttrImpl,
  decls: PhpDecl[]
): { kind: 'function' | 'method' | 'class'; name: string; startIndex: number; owner?: string } {
  const decl = impl.decl;
  if (decl.kind !== 'class') {
    return { kind: decl.kind, name: decl.name, startIndex: decl.startIndex, owner: decl.ownerName };
  }
  // Class-level attribute: pinpoint the implementing method — `method:` arg /
  // second positional arg, else `__invoke` (the D11 single-hook convention).
  const methodName = impl.methodArg ?? '__invoke';
  const method = decls.find(
    (d) =>
      d.kind === 'method' &&
      d.name === methodName &&
      d.startIndex > decl.startIndex &&
      d.startIndex < (decl.bodyEnd ?? Number.MAX_SAFE_INTEGER)
  );
  if (method) {
    return {
      kind: 'method',
      name: method.name,
      startIndex: method.startIndex,
      owner: decl.name,
    };
  }
  return { kind: 'class', name: decl.name, startIndex: decl.startIndex };
}

/**
 * `filePath → attribute scan` for `src/Hook/*.php` files — the Drupal 11
 * auto-registration convention (`Drupal\mymodule\Hook\*` classes carrying
 * `#[Hook]` attributes). Built once per resolution context (WeakMap) so a run
 * of `hook_X` refs costs one file sweep, not one sweep per ref; rebuilt when
 * the indexed file count changes (incremental syncs may reuse the context).
 */
const hookAttributeIndexes = new WeakMap<
  ResolutionContext,
  { files: number; index: Map<string, { decls: PhpDecl[]; impls: HookAttrImpl[] }> }
>();

function hookAttributeIndex(
  context: ResolutionContext
): Map<string, { decls: PhpDecl[]; impls: HookAttrImpl[] }> {
  const files = context.getAllFiles();
  const cached = hookAttributeIndexes.get(context);
  if (cached && cached.files === files.length) return cached.index;
  const index = new Map<string, { decls: PhpDecl[]; impls: HookAttrImpl[] }>();
  for (const file of files) {
    if (!file.endsWith('.php')) continue;
    if (!file.includes('/src/Hook/') && !file.startsWith('src/Hook/')) continue;
    const content = context.readFile(file);
    if (!content || !content.includes('Hook') || !content.includes('#[')) continue;
    const parsed = scanPhpHookAttributes(content);
    if (parsed.impls.length > 0) index.set(file, parsed);
  }
  hookAttributeIndexes.set(context, { files: files.length, index });
  return index;
}

/**
 * Find the node a `#[Hook('X')]` attribute marks as the `hook_X`
 * implementation: the attributed method, the class-level `method:`/`__invoke`
 * method, or the hook class itself.
 */
function findHookAttributeImpl(
  hookSuffix: string,
  context: ResolutionContext
): Node | null {
  for (const [filePath, parsed] of hookAttributeIndex(context)) {
    for (const impl of parsed.impls) {
      if (impl.hook !== hookSuffix) continue;
      const target = hookAttributeTarget(impl, parsed.decls);
      const fileNodes = context.getNodesInFile(filePath);
      const node =
        fileNodes.find(
          (nd) =>
            nd.kind === target.kind &&
            nd.name === target.name &&
            (target.owner === undefined ||
              nd.qualifiedName.includes(`${target.owner}::`))
        ) ?? fileNodes.find((nd) => nd.kind === target.kind && nd.name === target.name);
      if (node) return node;
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

export const drupalResolver: FrameworkResolver = {
  name: 'drupal',
  languages: ['php', 'yaml'],

  // Drupal route handlers are FQCNs (`\Drupal\…\Class::method`, the single-colon
  // controller-service form `\Drupal\…\Class:method`, or a bare `\…\FormClass`)
  // and hook refs are canonical `hook_*` names — none match a declared symbol, so
  // resolveOne's pre-filter would drop them before resolve() runs. Claim the
  // shapes resolve() handles (mirrors the Rails `controller#action` claim).
  claimsReference(name: string): boolean {
    return (
      name.startsWith('hook_') ||
      name.includes('\\') ||
      /^[A-Za-z_]\w*::?\w+$/.test(name)
    );
  },

  detect(context: ResolutionContext): boolean {
    // Primary: composer.json identifies a Drupal project/module/theme/profile.
    // A contrib module often has an EMPTY `require` (no `drupal/*` dep) but still
    // declares `"name": "drupal/<module>"` and `"type": "drupal-module"`, so check
    // those too — checking deps alone misses every standalone contrib module.
    const composer = context.readFile('composer.json');
    if (composer) {
      try {
        const json = JSON.parse(composer) as {
          name?: string;
          type?: string;
          require?: Record<string, string>;
          'require-dev'?: Record<string, string>;
        };
        if (typeof json.name === 'string' && json.name.startsWith('drupal/')) return true;
        if (typeof json.type === 'string' && json.type.startsWith('drupal-')) return true;
        const deps = { ...json.require, ...(json['require-dev'] ?? {}) };
        if (Object.keys(deps).some((k) => k.startsWith('drupal/'))) return true;
      } catch {
        // malformed composer.json — fall through to file-based detection
      }
    }

    // Fallback (composer-less module, or a non-Drupal composer.json): the
    // unmistakable Drupal signature is a `*.info.yml` manifest alongside a
    // Drupal PHP/route file. Require both so a stray `.info.yml` elsewhere
    // doesn't trigger a false positive.
    const files = context.getAllFiles();
    const hasInfoYml = files.some((f) => f.endsWith('.info.yml'));
    if (!hasInfoYml) return false;
    return files.some(
      (f) =>
        f.endsWith('.routing.yml') ||
        f.endsWith('.module') ||
        f.endsWith('.install') ||
        f.endsWith('.theme')
    );
  },

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    const name = ref.referenceName;

    // _controller: '\Drupal\module\...\ClassName::methodName' (double colon) or the
    // single-colon controller-service form '\Drupal\...\ClassName:methodName'.
    const controllerMatch = name.match(/^\\?(?:Drupal\\[^:]+\\)?([^\\:]+):{1,2}(\w+)$/);
    if (controllerMatch) {
      const [, className, methodName] = controllerMatch;
      const classNodes = context.getNodesByName(className!);
      for (const cls of classNodes) {
        if (cls.kind !== 'class') continue;
        const fileNodes = context.getNodesInFile(cls.filePath);
        const method = fileNodes.find((n) => n.kind === 'method' && n.name === methodName);
        if (method) {
          return { original: ref, targetNodeId: method.id, confidence: 0.9, resolvedBy: 'framework' };
        }
        return { original: ref, targetNodeId: cls.id, confidence: 0.7, resolvedBy: 'framework' };
      }
    }

    // _form / _entity_form: '\Drupal\module\...\ClassName'  (bare FQCN, no method)
    if (name.includes('\\') && !name.includes(':')) {
      const className = lastSegment(name);
      if (className) {
        const classNodes = context.getNodesByName(className);
        const cls = classNodes.find((n) => n.kind === 'class');
        if (cls) {
          return { original: ref, targetNodeId: cls.id, confidence: 0.85, resolvedBy: 'framework' };
        }
      }
    }

    // hook_X — find any function whose name ends in _{hookSuffix} in a hook file
    if (name.startsWith('hook_')) {
      const hookSuffix = name.slice(5); // strip 'hook_'
      const candidates = context.getNodesByKind('function').filter(
        (n) => n.name.endsWith(`_${hookSuffix}`) && isDrupalHookFile(n.filePath)
      );
      if (candidates.length > 0) {
        return {
          original: ref,
          targetNodeId: candidates[0]!.id,
          confidence: 0.75,
          resolvedBy: 'framework',
        };
      }

      // Drupal 11 attribute era: no procedural `*_X` exists — hooks live on
      // `#[Hook('X')]` classes under `src/Hook/`. Bounded scan (cached per
      // context) resolves the ref to the attributed implementation node.
      const attrImpl = findHookAttributeImpl(hookSuffix, context);
      if (attrImpl) {
        return {
          original: ref,
          targetNodeId: attrImpl.id,
          confidence: 0.75,
          resolvedBy: 'framework',
        };
      }
    }

    return null;
  },

  extract(filePath: string, content: string): { nodes: Node[]; references: UnresolvedRef[] } {
    if (filePath.endsWith('.routing.yml')) {
      return extractDrupalRoutes(filePath, content);
    }

    if (isDrupalHookFile(filePath) || filePath.endsWith('.php')) {
      return extractDrupalHooks(filePath, content);
    }

    return { nodes: [], references: [] };
  },
};
