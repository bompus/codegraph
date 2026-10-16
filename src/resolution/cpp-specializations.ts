import type { Node } from '../types';
import type { ResolutionContext } from './types';

/**
 * C++ explicit specializations for the TypeScript synthesizers: the kernel
 * resolver reads the same arguments natively (resolve/cpp_specializations.rs).
 */

/** Per context: a C++ function's id → the template arguments it is an explicit specialization for (null: none). */
const CPP_SPECIALIZATION_ARGUMENTS = new WeakMap<ResolutionContext, Map<string, string[] | null>>();

/**
 * The template arguments the C++ function or method `n` is an explicit
 * specialization for, as its definition writes them after its name:
 * `int32_t` and `WireFormatLite::TYPE_INT32` for protobuf's `template <>
 * inline bool WireFormatLite::ReadPrimitive<int32_t, WireFormatLite::TYPE_INT32>(…)`.
 * A method a macro defines is read from its expansion. Null for a template's
 * own definition, any other function, and a specialization that leaves its
 * arguments to be deduced.
 */
export function cppSpecializationArguments(n: Node, context: ResolutionContext): string[] | null {
  // A constructor, whose signature is its parameter list, is never specialized
  // by name: gmock's `template <typename Func> Action(const Action<Func>& action)`
  // writes `Action<Func>` for a parameter.
  if (n.language !== 'cpp' || (n.kind !== 'function' && n.kind !== 'method') || n.signature?.startsWith('(')) return null;
  let memo = CPP_SPECIALIZATION_ARGUMENTS.get(context);
  if (!memo) CPP_SPECIALIZATION_ARGUMENTS.set(context, (memo = new Map()));
  const hit = memo.get(n.id);
  if (hit !== undefined) return hit;
  // (A method a macro defines carries its expansion as its signature.)
  const lines = n.signature ? [] : context.getFileLines?.(n.filePath) ?? context.readFile(n.filePath)?.split(/\r?\n/) ?? [];
  const head = n.signature || lines.slice(n.startLine - 1, n.startLine + 11).join('\n').slice(n.startColumn);
  const args = cppTemplateArgumentsAfter(head, n.name, 0);
  memo.set(n.id, args);
  return args;
}

/**
 * The template arguments `text` writes right after `name`, the first time
 * from `from` on it is written as a word of its own, as they compare
 * (cppTemplateArgumentKey). Null when it writes none there, or `<>`.
 */
function cppTemplateArgumentsAfter(text: string, name: string, from: number): string[] | null {
  for (let at = text.indexOf(name, from); at >= 0; at = text.indexOf(name, at + 1)) {
    const open = at + name.length;
    if (/[\w$]/.test(text[at - 1] ?? '') || /[\w$]/.test(text[open] ?? '')) continue;
    const end = text[open] === '<' ? cppTemplateArgumentsEnd(text, open) : -1;
    const args = end < 0 ? [] : splitCppTopLevel(text.slice(open + 1, end)).map(cppTemplateArgumentKey);
    return args.length > 0 ? args : null;
  }
  return null;
}

/** A C++ template argument as it compares: spaced only between words. */
function cppTemplateArgumentKey(arg: string): string {
  return arg.replace(/\s*([^\w\s])\s*/g, '$1').replace(/\s+/g, ' ');
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

/**
 * Where the template argument list the `<` at `at` opens closes: the index
 * of its `>`, or -1 when the `<` compares or shifts.
 */
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
