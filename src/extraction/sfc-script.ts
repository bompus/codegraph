/** Fold embedded scripts into one SFC file/component hierarchy, preserving nested owners. */

import type { Binding, Edge, ExtractionError, ExtractionResult, Language, Node, UnresolvedReference } from '../types';
import * as path from 'path';

const TS_LANG_ATTR = /lang\s*=\s*["'](ts|typescript)["']/;
const TSX_LANG_ATTR = /lang\s*=\s*["']tsx["']/;

/**
 * The grammar a `<script>` block is read with, from its opening tag's
 * attributes — by the Vue and Svelte extractors, and by the viewer's syntax
 * regions, so a component's code is highlighted with the grammar its symbols
 * came from.
 *
 * `ts` / `typescript` is TypeScript. A Vue block can also be `tsx` (#2570):
 * TypeScript with JSX, which only the TSX grammar reads — the TypeScript
 * grammar has no JSX, and the JavaScript one stops at the first type, so a
 * `defineProps<{…}>()` or an `interface` was lost. Svelte has no TSX scripts.
 * Anything else is JavaScript, whose grammar reads JSX (`lang="jsx"`) already.
 */
export function sfcScriptLanguage(attrs: string, host: Language): Language {
  if (TS_LANG_ATTR.test(attrs)) return 'typescript';
  if (host === 'vue' && TSX_LANG_ATTR.test(attrs)) return 'tsx';
  return 'javascript';
}

/** The SFC's own file node — the whole file, as the tree-sitter extractor makes one. */
export function sfcFileNode(filePath: string, source: string, language: Language): Node {
  return {
    id: `file:${filePath}`,
    kind: 'file',
    name: path.basename(filePath),
    qualifiedName: filePath,
    filePath,
    language,
    startLine: 1,
    endLine: source.split('\n').length,
    startColumn: 0,
    endColumn: 0,
    isExported: false,
    updatedAt: Date.now(),
  };
}

const NAME = /[A-Za-z_$][\w$]*/g;
/** Quoted text and comments, which can hold arrow-like text that declares nothing. */
const QUOTED_OR_COMMENT = /'(?:\\.|[^'\\])*'|"(?:\\.|[^"\\])*"|`(?:\\.|[^`\\])*`|\/\*[\s\S]*?\*\/|\/\/.*$/gm;

/** The parameter names of one arrow function and the span of its body in the expression. */
export interface ArrowScope {
  names: Set<string>;
  start: number;
  end: number;
}

function closeOf(code: string, open: number): number {
  let depth = 0;
  for (let i = open; i < code.length; i++) {
    if ('([{'.includes(code[i]!)) depth++;
    else if (')]}'.includes(code[i]!) && --depth === 0) return i;
  }
  return code.length;
}

function openOf(code: string, close: number): number {
  let depth = 0;
  for (let i = close; i >= 0; i--) {
    if (')]}'.includes(code[i]!)) depth++;
    else if ('([{'.includes(code[i]!) && --depth === 0) return i;
  }
  return 0;
}

/** Where an expression-bodied arrow stops: a comma or closer its own nesting does not open. */
function bodyEnd(code: string, start: number): number {
  let depth = 0;
  for (let i = start; i < code.length; i++) {
    const ch = code[i]!;
    if ('([{'.includes(ch)) depth++;
    else if (')]}'.includes(ch)) {
      if (--depth < 0) return i;
    } else if (ch === ',' && depth === 0) return i;
  }
  return code.length;
}

function paramNames(list: string): string[] {
  const names: string[] = [];
  let depth = 0;
  let from = 0;
  for (let i = 0; i <= list.length; i++) {
    const ch = list[i];
    if (ch !== undefined && '([{'.includes(ch)) depth++;
    else if (ch !== undefined && ')]}'.includes(ch)) depth--;
    else if ((ch === ',' || ch === undefined) && depth === 0) {
      const param = list.slice(from, i);
      const declared = param.slice(0, topLevelEquals(param)).trim();
      const pattern = declared.startsWith('{') || declared.startsWith('[');
      names.push(...(declared.match(NAME)?.slice(0, pattern ? undefined : 1) ?? []));
      from = i + 1;
    }
  }
  return names;
}

function topLevelEquals(param: string): number {
  let depth = 0;
  for (let i = 0; i < param.length; i++) {
    if ('([{'.includes(param[i]!)) depth++;
    else if (')]}'.includes(param[i]!)) depth--;
    else if (param[i] === '=' && depth === 0) return i;
  }
  return param.length;
}

/**
 * The arrow functions in one markup expression, each with the names it declares
 * as parameters and the span of its body (`rows.map((count) => …)`,
 * `rows.map(count => …)`, `({ count }) => …`). Markup has no binding rows, so a
 * call on one of these names inside the body would otherwise link to a script
 * import of the same name; a call outside the body still does.
 */
export function arrowScopes(expr: string): ArrowScope[] {
  const code = expr.replace(QUOTED_OR_COMMENT, (text) => ' '.repeat(text.length));
  const scopes: ArrowScope[] = [];
  for (const arrow of code.matchAll(/=>/g)) {
    let end = arrow.index - 1;
    while (end >= 0 && /\s/.test(code[end]!)) end--;
    let names: string[];
    if (code[end] === ')') names = paramNames(code.slice(openOf(code, end) + 1, end));
    else names = code.slice(0, end + 1).match(/[A-Za-z_$][\w$]*$/)?.slice(0, 1) ?? [];
    if (names.length === 0) continue;
    let start = arrow.index + 2;
    while (/\s/.test(code[start] ?? '')) start++;
    scopes.push({ names: new Set(names), start, end: code[start] === '{' ? closeOf(code, start) + 1 : bodyEnd(code, start) });
  }
  return scopes;
}

/** Whether `name`, used at `index` in the expression, is a parameter of an arrow function around it. */
export function declaredByArrow(scopes: ArrowScope[], name: string, index: number): boolean {
  return scopes.some((scope) => index >= scope.start && index < scope.end && scope.names.has(name));
}

export interface ScriptFold {
  filePath: string;
  componentNodeId: string;
  /** Lines before the block in the SFC file: the block's line 1 is `lineOffset + 1`. */
  lineOffset: number;
  language: Language;
  /** The block runs once per component instance — Vue `<script setup>`, a Svelte instance script, Astro frontmatter. */
  perInstance: boolean;
}

export interface ScriptSink {
  nodes: Node[];
  edges: Edge[];
  unresolvedReferences: UnresolvedReference[];
  errors: ExtractionError[];
  bindings: Binding[];
}

/** What a per-instance block does at its top level, as opposed to what it imports. */
const INSTANCE_REF_KINDS: ReadonlySet<string> = new Set(['calls', 'references', 'instantiates', 'function_ref']);

export function foldScriptResult(result: ExtractionResult, fold: ScriptFold, sink: ScriptSink): void {
  const blockFile = `file:${fold.filePath}`;
  // Nested symbols retain their script parent; top-level symbols belong to the component.
  const parented = new Set<string>();
  for (const edge of result.edges) {
    if (edge.kind === 'contains' && edge.source !== blockFile) parented.add(edge.target);
  }

  // A per-instance block's top-level `const data = useFetch(…)` is component
  // state: its initializer runs on every instance, as the component's doing.
  const instanceValues = new Set<string>();
  if (fold.perInstance) {
    for (const node of result.nodes) {
      if ((node.kind === 'constant' || node.kind === 'variable') && !parented.has(node.id)) instanceValues.add(node.id);
    }
  }
  const runsAsComponent = (id: string) => id === blockFile || instanceValues.has(id);

  for (const node of result.nodes) {
    if (node.kind === 'file') continue;
    node.startLine += fold.lineOffset;
    node.endLine += fold.lineOffset;
    node.language = fold.language;
    sink.nodes.push(node);
    if (!parented.has(node.id)) sink.edges.push({ source: fold.componentNodeId, target: node.id, kind: 'contains' });
  }

  for (const edge of result.edges) {
    if (edge.kind === 'contains' && edge.source === blockFile) continue;
    if (edge.line) edge.line += fold.lineOffset;
    // What a top-level value DOES is the component's; what it HOLDS stays its
    // own — `const api = { load() {…} }` keeps `api::load` (#2300).
    if (fold.perInstance && runsAsComponent(edge.source) && edge.kind !== 'imports' && edge.kind !== 'contains') {
      edge.source = fold.componentNodeId;
    }
    sink.edges.push(edge);
  }

  for (const ref of result.unresolvedReferences) {
    ref.line += fold.lineOffset;
    ref.filePath = fold.filePath;
    ref.language = fold.language;
    if (fold.perInstance && runsAsComponent(ref.fromNodeId) && INSTANCE_REF_KINDS.has(ref.referenceKind)) {
      ref.fromNodeId = fold.componentNodeId;
    }
    sink.unresolvedReferences.push(ref);
  }

  for (const binding of result.bindings ?? []) {
    sink.bindings.push({
      ...binding,
      filePath: fold.filePath,
      scopeStart: binding.scopeStart + fold.lineOffset,
      scopeEnd: binding.scopeEnd + fold.lineOffset,
      line: binding.line + fold.lineOffset,
    });
  }

  for (const error of result.errors) {
    if (error.line) error.line += fold.lineOffset;
    sink.errors.push(error);
  }
}
