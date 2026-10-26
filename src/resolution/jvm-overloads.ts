/**
 * JVM-side helpers the Play resolver needs from the deleted name-matcher:
 * whether a name an import binds points outside the project's packages
 * (isJavaOutsideImport), and the self-call overload retarget that moves a
 * resolution between same-owner overloads by the written argument list
 * (retargetSelfOverload). The kernel resolves these natively for ordinary
 * refs; a framework resolver that picks candidates in TypeScript keeps the
 * TypeScript versions here.
 */

import type { Node } from '../types';
import type { ResolutionContext, ResolvedRef, UnresolvedRef } from './types';
import { escapeRegExp } from '../utils';

const JVM_CALLABLE_KINDS: ReadonlySet<string> = new Set(['method', 'function']);
const JVM_TYPE_KINDS: ReadonlySet<string> = new Set(['class', 'interface', 'enum', 'struct', 'trait', 'type_alias', 'annotation']);

const JVM_PACKAGES = new WeakMap<ResolutionContext, Set<string>>();

export function isJavaOutsideImport(name: string, ref: UnresolvedRef, context: ResolutionContext): boolean {
  const binding = context.getImportMappings(ref.filePath, ref.language).find((m) => m.localName === name);
  if (!binding) return false;
  // A member the file declares itself is in scope before any import —
  // Exposed's `toLocalDateTime(value)` inside the column type that defines it —
  // in the same namespace only: gson's `new URI(…)` is `java.net.URI` beside
  // `TypeAdapters`' field `URI`, as a type always is beside a value.
  const call = ref.referenceKind === 'calls' && name === ref.referenceName;
  const kinds = call ? JVM_CALLABLE_KINDS : JVM_TYPE_KINDS;
  if ((context.getNodesInFileNamed?.(ref.filePath, name) ?? context.getNodesInFile(ref.filePath).filter((n) => n.name === name))
    .some((n) => kinds.has(n.kind))) return false;
  let packages = JVM_PACKAGES.get(context);
  if (!packages) {
    packages = new Set<string>();
    for (const n of context.getNodesByKind('namespace')) {
      if (n.language === 'java' || n.language === 'kotlin' || n.language === 'scala') packages.add(n.qualifiedName);
    }
    JVM_PACKAGES.set(context, packages);
  }
  const parts = binding.source.split('.');
  for (let i = 1; i < parts.length; i++) {
    if (packages.has(parts.slice(0, i).join('.'))) return false;
  }
  return true;
}

type Arity = { min: number; max: number } | null;

const OVERLOADING_LANGUAGES: ReadonlySet<string> = new Set(['csharp', 'java', 'kotlin', 'swift', 'cpp', 'scala', 'dart', 'vbnet', 'solidity']);

const OVERLOAD_SETS = new WeakMap<ResolutionContext, Map<string, { name: string; own: Arity; siblings: Array<{ id: string; arity: Arity }> } | null>>();

export function retargetSelfOverload(result: ResolvedRef, ref: UnresolvedRef, context: ResolutionContext): ResolvedRef {
  if (ref.referenceKind !== 'calls' || !OVERLOADING_LANGUAGES.has(ref.language)) return result;
  // C++ overload sets (templates, SFINAE tags, a `data()` on any container) are
  // only trusted for a method's call to itself.
  if (ref.language === 'cpp' && result.targetNodeId !== ref.fromNodeId) return result;
  let memo = OVERLOAD_SETS.get(context);
  if (!memo) OVERLOAD_SETS.set(context, (memo = new Map()));
  let set = memo.get(result.targetNodeId);
  if (set === undefined) {
    set = overloadSetOf(result.targetNodeId, context);
    memo.set(result.targetNodeId, set);
  }
  // Only an overload set has a sibling to move to.
  if (!set || set.own === null) return result;
  const args = cppParenListAfter(ref.filePath, ref.line, Math.max(0, ref.column), set.name, context);
  if (args === null) return result;
  // Swift overloads by argument label as much as by count: Alamofire's
  // `self.tableView(tableView, numberOfRowsInSection: section)` inside
  // `tableView(_:titleForHeaderInSection:)` is the other `tableView`.
  if (ref.language === 'swift') {
    const labels = args.trim() === '' ? [] : splitCppTopLevel(args).map((a) => /^\s*([A-Za-z_]\w*)\s*:(?!:)/.exec(a)?.[1] ?? '_');
    const fitsLabels = (id: string): boolean | null => {
      const decl = context.getNodeById?.(id);
      const list = decl ? cppParenListAfter(decl.filePath, decl.startLine, 0, set!.name, context) : null;
      return list === null ? null : swiftLabelsFit(labels, list);
    };
    if (fitsLabels(result.targetNodeId) !== false) return result;
    const fit = set.siblings.filter((sib) => fitsLabels(sib.id) === true);
    return fit.length === 1 ? { ...result, targetNodeId: fit[0]!.id } : result;
  }
  const argc = args.trim() === '' ? 0 : splitCppTopLevel(args).length;
  if (argc >= set.own.min && argc <= set.own.max) return result;
  const fits = set.siblings.filter((s) => s.arity !== null && argc >= s.arity.min && argc <= s.arity.max);
  return fits.length === 1 ? { ...result, targetNodeId: fits[0]!.id } : result;
}

/**
 * Whether a Swift call's argument labels (`_` for none) fit a declaration's
 * parameter list: each parameter's external label in order, one with a
 * default value or a variadic one free to be left out.
 */
function swiftLabelsFit(labels: string[], paramList: string): boolean {
  const params = paramList.trim() === '' ? [] : splitCppTopLevel(paramList).map((p) => {
    const head = /^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:inout\s+)?([A-Za-z_]\w*)(?:\s+([A-Za-z_]\w*))?\s*:/.exec(p);
    return { label: head?.[1] ?? '_', optional: /=/.test(p) || /\.\.\./.test(p) };
  });
  let i = 0;
  for (const param of params) {
    if (i < labels.length && labels[i] === param.label) { i++; continue; }
    if (!param.optional) return false;
  }
  return i === labels.length;
}

/** A method's same-owner overloads and every one's arity, read from its declaration. */
function overloadSetOf(id: string, context: ResolutionContext): { name: string; own: Arity; siblings: Array<{ id: string; arity: Arity }> } | null {
  const self = context.getNodeById?.(id);
  if (!self || (self.kind !== 'method' && self.kind !== 'function')) return null;
  const name = self.name;
  const owner = self.qualifiedName.slice(0, Math.max(0, self.qualifiedName.lastIndexOf('::')));
  const siblings = (context.getNodesInFileNamed?.(self.filePath, name) ?? context.getNodesInFile(self.filePath).filter((n) => n.name === name))
    .filter((n) => n.id !== self.id && (n.kind === 'method' || n.kind === 'function') &&
      n.qualifiedName.slice(0, Math.max(0, n.qualifiedName.lastIndexOf('::'))) === owner);
  if (siblings.length === 0) return null;
  const arity = (n: Node): Arity => {
    const list = cppParenListAfter(n.filePath, n.startLine, 0, name, context);
    if (list === null) return null;
    const params = splitCppTopLevel(list).filter((p) => p !== '' && p !== 'void');
    const pack = (p: string) => /\.\.\.|\bparams\s|\bvararg\s/.test(p.replace(/<[^<>]*>/g, ''));
    const min = params.filter((p) => !/=/.test(p) && !pack(p)).length;
    return { min, max: params.some(pack) ? Infinity : params.length };
  };
  return { name, own: arity(self), siblings: siblings.map((n) => ({ id: n.id, arity: arity(n) })) };
}

function cppParenListAfter(file: string, line: number, column: number, name: string, context: ResolutionContext): string | null {
  const lines = context.getFileLines?.(file) ?? context.readFile(file)?.split(/\r?\n/) ?? [];
  return cppParenListIn(lines.slice(line - 1, line + 11).join('\n'), column, name);
}

/** The `( … )` that follows `name` in `text`, from `column` on, without its parentheses. */
function cppParenListIn(text: string, column: number, name: string): string | null {
  const at = new RegExp(`\\b${escapeRegExp(name)}\\s*(?:<[^<>()]*>)?\\s*\\(`).exec(text.slice(column));
  if (!at) return null;
  const open = column + at.index + at[0].length - 1;
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === '(') depth++;
    else if (text[i] === ')' && --depth === 0) return text.slice(open + 1, i);
  }
  return null;
}

/** Split `a, f(b, c), d<e, f>` at its top-level commas. */
function splitCppTopLevel(text: string): string[] {
  const out: string[] = [];
  let depth = 0;
  // Where each template argument list open around the current character closes.
  const closes: number[] = [];
  let cur = '';
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!;
    if (ch === '"' || ch === "'") {
      let j = i + 1;
      while (j < text.length && text[j] !== ch) j += text[j] === '\\' ? 2 : 1;
      cur += text.slice(i, j + 1);
      i = j;
      continue;
    }
    if (ch === '(' || ch === '[' || ch === '{') depth++;
    else if (ch === ')' || ch === ']' || ch === '}') depth = Math.max(0, depth - 1);
    else if (ch === '<') {
      const end = cppTemplateArgumentsEnd(text, i);
      if (end > i) closes.push(end);
    } else if (ch === '>' && closes[closes.length - 1] === i) closes.pop();
    if (ch === ',' && depth === 0 && closes.length === 0) { out.push(cur.trim()); cur = ''; continue; }
    cur += ch;
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}

function cppTemplateArgumentsEnd(text: string, at: number): number {
  if (!/\w/.test(text[at - 1] ?? '') || text[at + 1] === '<' || text[at + 1] === '=') return -1;
  let angles = 0;
  let others = 0;
  for (let i = at; i < text.length; i++) {
    const ch = text[i]!;
    if (ch === '"' || ch === "'") {
      let j = i + 1;
      while (j < text.length && text[j] !== ch) j += text[j] === '\\' ? 2 : 1;
      i = j;
    } else if (ch === '(' || ch === '[' || ch === '{') others++;
    else if (ch === ')' || ch === ']' || ch === '}') {
      if (--others < 0) return -1;
    } else if (others > 0) continue;
    else if (ch === '<') angles++;
    else if (ch === '>' && text[i - 1] !== '-' && --angles === 0) return i;
  }
  return -1;
}
