/**
 * Ruby scope helpers for the parameter-flow adapter (./ruby-parameter-flow).
 *
 * The fork's Ruby resolution lives in the Rust kernel, which exposes no
 * per-query scope surface. The parameter-flow pass needs the same answers the
 * kernel gives its own Ruby receiver resolution, so this module ports the
 * helpers upstream's TS name-matcher and ruby-constant-paths provided at
 * 483a0988: ordered ancestry, the methods an owner declares, a class method
 * reached through extends/concerns, constant lookup from the call site, and
 * the class an `initialize` assigns an instance variable.
 *
 * Keep this in step with codegraph-kernel/src/resolve/{lang_scope,ruby_paths}.rs
 * (`ruby_ancestry_ordered`, `ruby_class_method_of`, `ruby_constant_qn`,
 * `ruby_ivar_class`).
 */

import type { Node } from '../types';
import type { ResolutionContext, UnresolvedRef } from './types';
import { isTestPath } from '../search/query-utils';
import { stripCommentsForRegex } from './strip-comments';

/** What one declaration of a Ruby class or module adds to its ancestors, each list in the order Ruby looks there. */
interface RubyMixins {
  prepended: string[];
  included: string[];
  extended: string[];
  superclass: string | null;
}

const RUBY_MIXINS = new WeakMap<ResolutionContext, Map<string, RubyMixins>>();
const RUBY_ANCESTRY = new WeakMap<ResolutionContext, Map<string, Set<string>>>();
const RUBY_METHOD_OWNERS = new WeakMap<ResolutionContext, Map<string, Map<string, Node[]>>>();
const IVAR_CLASSES = new WeakMap<ResolutionContext, Map<string, Node | null | undefined>>();

const NAMESPACE_KINDS: ReadonlySet<string> = new Set(['class', 'module']);
const CONSTANT_KINDS: ReadonlySet<string> = new Set(['class', 'module', 'variable', 'constant']);
const VALUE_KINDS: ReadonlySet<string> = new Set(['variable', 'constant']);

const isRubyNamespace = (n: Node): boolean =>
  n.language === 'ruby' && (n.kind === 'class' || n.kind === 'module');

/** A file's lines, as the resolver caches them. */
function linesOf(file: string, context: ResolutionContext): string[] {
  return context.getFileLines?.(file) ?? context.readFile(file)?.split(/\r?\n/) ?? [];
}

/**
 * What a Ruby class or module declaration mixes in, read from its own body
 * (a class nested in it has its own mixins) and found by constant lookup from
 * there. The module mixed in last is asked first: rails' `save!` is
 * `Transactions`', which `Base` includes after `Persistence`.
 */
function rubyMixinsOf(decl: Node, context: ResolutionContext): RubyMixins {
  let memo = RUBY_MIXINS.get(context);
  if (!memo) RUBY_MIXINS.set(context, (memo = new Map()));
  const hit = memo.get(decl.id);
  if (hit) return hit;
  const q = decl.qualifiedName;
  const found: RubyMixins = { prepended: [], included: [], extended: [], superclass: null };
  const lines = linesOf(decl.filePath, context);
  const outer = q.includes('::') ? q.slice(0, q.lastIndexOf('::')) : '';
  const sup = /^\s*class\s+[\w:]+\s*<\s*(::)?([A-Z][\w:]*)/.exec(lines[decl.startLine - 1] ?? '');
  if (sup) found.superclass = rubyConstantQn(sup[2]!, sup[1] ? '' : outer, context);
  const nested = context.getNodesInFile(decl.filePath).filter((n) => (n.kind === 'class' || n.kind === 'module') && n.id !== decl.id &&
    n.startLine > decl.startLine && n.endLine < decl.endLine);
  for (let i = decl.startLine; i < decl.endLine && i < lines.length; i++) {
    // A module a call returns (`include RouteSet.new.tap { … }.url_helpers`,
    // actionpack's tests) is no mixin of the constant the call starts at.
    const mix = /^\s*(include|extend|prepend)\s+([A-Z:][\w:]*(?:\s*,\s*[A-Z:][\w:]*)*)(?![\w:]|\s*[.(])/.exec(lines[i]!);
    if (!mix || nested.some((n) => n.startLine <= i + 1 && n.endLine >= i + 1)) continue;
    const names = mix[2]!.split(/\s*,\s*/).map((name) => name.startsWith('::') ? rubyConstantQn(name.slice(2), '', context) : rubyConstantQn(name, q, context));
    const list = mix[1] === 'include' ? found.included : mix[1] === 'prepend' ? found.prepended : found.extended;
    list.unshift(...names);
  }
  memo.set(decl.id, found);
  return found;
}

/**
 * A Ruby class's qualified name, those of its superclasses, and of every
 * module mixed into any of them, in the order Ruby looks a method up. For
 * what an instance answers (`instance`), a module the class `extend`s is
 * left out: its methods are the class's own.
 */
export function rubyAncestry(qn: string, context: ResolutionContext, instance = false): Set<string> {
  let memo = RUBY_ANCESTRY.get(context);
  if (!memo) RUBY_ANCESTRY.set(context, (memo = new Map()));
  const key = instance ? `instance ${qn}` : qn;
  const hit = memo.get(key);
  if (hit) return hit;
  const seen = new Set<string>();
  const order = new Set<string>();
  const visit = (q: string): void => {
    if (seen.has(q) || seen.size >= 250) return;
    seen.add(q);
    const mixins = context.getNodesByQualifiedName(q)
      .filter(isRubyNamespace)
      .map((decl) => rubyMixinsOf(decl, context));
    for (const m of mixins) m.prepended.forEach(visit);
    order.add(q);
    for (const m of mixins) {
      m.included.forEach(visit);
      if (!instance) m.extended.forEach(visit);
    }
    for (const m of mixins) if (m.superclass) visit(m.superclass);
  };
  visit(qn);
  memo.set(key, order);
  return order;
}

/** The Ruby methods `name` of the class or module `owner`. A walk up a class's ancestors asks this of each one, for each method name. */
export function rubyMethodsOf(owner: string, name: string, context: ResolutionContext): Node[] {
  let memo = RUBY_METHOD_OWNERS.get(context);
  if (!memo) RUBY_METHOD_OWNERS.set(context, (memo = new Map()));
  let owners = memo.get(name);
  if (!owners) {
    memo.set(name, (owners = new Map()));
    for (const n of context.getNodesByName(name)) {
      if (n.kind !== 'method' || n.language !== 'ruby') continue;
      const at = n.qualifiedName.lastIndexOf('::');
      if (at < 0) continue;
      const of = n.qualifiedName.slice(0, at);
      const methods = owners.get(of);
      if (methods) methods.push(n);
      else owners.set(of, [n]);
    }
  }
  return owners.get(owner) ?? [];
}

/** The innermost class or module around `ref`, if any. */
function innermostNamespace(ref: UnresolvedRef, context: ResolutionContext): Node | undefined {
  let inner: Node | undefined;
  for (const n of context.getNodesInFile(ref.filePath)) {
    if (!NAMESPACE_KINDS.has(n.kind) || n.language !== 'ruby' || n.startLine > ref.line || n.endLine < ref.line) continue;
    // Of the ranges around a point, the innermost starts last.
    if (!inner || n.startLine > inner.startLine || (n.startLine === inner.startLine && n.startColumn > inner.startColumn)) inner = n;
  }
  return inner;
}

/**
 * The class method `name` a Ruby class answers past its own: up its
 * superclasses, a `def self.` of theirs, an instance method of a module one of
 * them `extend`s, or of the `ClassMethods` a module they include brings along
 * (ActiveSupport::Concern). Never an instance method of a module they
 * include, nor a `def self.` of one, which stays that module's:
 * rails' `Topic.create` is no `Reflection.create`.
 */
export function rubyClassMethodOf(qn: string, name: string, context: ResolutionContext): Node | null {
  const named = (owner: string, isStatic: boolean): Node | undefined =>
    rubyMethodsOf(owner, name, context).find((n) => (n.isStatic === true) === isStatic);
  const seen = new Set<string>();
  for (let cls: string | null = qn; cls !== null && !seen.has(cls);) {
    seen.add(cls);
    const decls: Node[] = context.getNodesByQualifiedName(cls).filter(isRubyNamespace);
    if (cls !== qn) {
      const own = named(cls, true);
      if (own) return own;
    }
    for (const decl of decls) {
      const mixins = rubyMixinsOf(decl, context);
      for (const module of mixins.extended) {
        for (const owner of rubyAncestry(module, context, true)) {
          const method = named(owner, false);
          if (method) return method;
        }
      }
      for (const module of mixins.included) {
        for (const owner of rubyAncestry(module, context, true)) {
          const method = named(`${owner}::ClassMethods`, false);
          if (method) return method;
        }
      }
    }
    cls = decls.map((decl) => rubyMixinsOf(decl, context).superclass).find((up) => up !== null) ?? null;
  }
  return null;
}

/** The class or module a constant written inside `scope` names: the nearest enclosing namespace that has it, else the name itself. */
export function rubyConstantQn(name: string, scope: string, context: ResolutionContext): string {
  for (let prefix = scope; ; prefix = prefix.includes('::') ? prefix.slice(0, prefix.lastIndexOf('::')) : '') {
    const qn = prefix ? `${prefix}::${name}` : name;
    if (context.getNodesByQualifiedName(qn).some(isRubyNamespace)) return qn;
    if (!prefix) return name;
  }
}

/** Anything before a value, then the reference's own file, then production code; the index's order within each. */
function ordered(nodes: Node[], ref: UnresolvedRef): Node[] {
  const rank = (n: Node): number =>
    (VALUE_KINDS.has(n.kind) ? 4 : 0) + (n.filePath === ref.filePath ? 0 : 2) + (isTestPath(n.filePath) ? 1 : 0);
  return nodes.length < 2 ? nodes : [...nodes].sort((a, b) => rank(a) - rank(b));
}

/**
 * The class the constant path `path` names from `ref`'s lexical scope, or null
 * when the first constant of the name is a value or the project declares none.
 */
function constantClass(path: string, absolute: boolean, ref: UnresolvedRef, context: ResolutionContext): Node | null {
  const production = !isTestPath(ref.filePath);
  let prefix = absolute ? '' : (innermostNamespace(ref, context)?.qualifiedName ?? '');
  for (;;) {
    const qn = prefix ? `${prefix}::${path}` : path;
    const found = context.getNodesByQualifiedName(qn).filter((n) =>
      n.language === 'ruby' && CONSTANT_KINDS.has(n.kind) && !(production && isTestPath(n.filePath)));
    if (found.length > 0) {
      const first = ordered(found, ref)[0]!;
      return first.kind === 'class' ? first : null;
    }
    if (!prefix) return null;
    const cut = prefix.lastIndexOf('::');
    prefix = cut >= 0 ? prefix.slice(0, cut) : '';
  }
}

/** The index past the parenthesized group opening at `i`, Ruby strings inside it skipped; -1 when it never closes. */
function skipParens(s: string, i: number): number {
  let depth = 0;
  for (let j = i; j < s.length; j++) {
    const c = s[j]!;
    if (c === '"' || c === "'") {
      for (j++; j < s.length && s[j] !== c; j++) if (s[j] === '\\') j++;
      continue;
    }
    if (c === '(') depth++;
    else if (c === ')' && --depth === 0) return j + 1;
  }
  return -1;
}

/**
 * The constant a value written at `from` constructs when it is a lone
 * `Const.new`, `A::B.new(…)` or `::Foo.new …`: its arguments in parentheses
 * or without them, a block, or a statement modifier may follow, but nothing
 * the value is passed through (`.tap`, `||`). Null otherwise.
 */
function constructedValue(s: string, from: number): { path: string; absolute: boolean } | null {
  const m = /^\s*(::)?([A-Z]\w*(?:::[A-Z]\w*)*)\s*\.\s*new(?![\w?!])/.exec(s.slice(from));
  if (!m) return null;
  let i = from + m[0].length;
  if (s[i] === '(') {
    i = skipParens(s, i);
    if (i < 0) return null;
  }
  // The end of the statement, a block, or (after a space) a modifier or
  // arguments without parentheses: anything but a chain or an operator.
  const next = s[i];
  const ok = next === undefined || next === '\n' || next === ';' || next === '{' ||
    /^[ \t]+(?:$|\n|;|\{|[\w:@$"'[])/.test(s.slice(i, i + 200));
  // A chain may go on at the start of the next line: `Foo.new\n  .tap {…}`.
  const continued = /^[ \t]*\n\s*&?\./.test(s.slice(i, i + 200));
  return ok && !continued ? { path: m[2]!, absolute: m[1] !== undefined } : null;
}

/**
 * Each assignment to `@name` in `body` (an `initialize`, comments blanked):
 * the constant whose `.new` it assigns, and the assignment's line within
 * `body`. Undefined when one of them is anything else: not at the start of
 * a statement (`@a, @b = …`), or not a lone `Const.new(…)` (`Foo.new.tap
 * {…}`, `writer || Foo.new`, `build_writer`).
 */
function constructedAssignments(body: string, name: string): { path: string; absolute: boolean; line: number }[] | undefined {
  const out: { path: string; absolute: boolean; line: number }[] = [];
  const target = new RegExp(String.raw`(?<![@\w])@${name}\s*(?:\|\||&&|\*\*|<<|>>|[-+*/%|&^])?=(?![=~>])`, 'g');
  for (const m of body.matchAll(target)) {
    const at = m.index!;
    const statementStart = Math.max(body.lastIndexOf('\n', at - 1), body.lastIndexOf(';', at - 1)) + 1;
    if (body.slice(statementStart, at).trim() !== '') return undefined;
    const value = constructedValue(body, at + m[0].length);
    if (!value) return undefined;
    let line = 0;
    for (let i = body.indexOf('\n'); i !== -1 && i < at; i = body.indexOf('\n', i + 1)) line++;
    out.push({ ...value, line });
  }
  return out;
}

/**
 * The class of what the instance variable `@name` holds where `ref` is
 * written, an instance method of a class: read from that class's
 * `initialize`, where every assignment to the variable must be
 * `@name = Const.new(…)` (or `||=`) of one constant. Then it is the class
 * Ruby's lookup finds for that constant from there, or null when the
 * project declares no such class. Undefined, for the other strategies,
 * when the call is not in an instance method of a class, or `initialize`
 * doesn't assign the variable, or assigns it anything else.
 */
export function ivarClass(name: string, ref: UnresolvedRef, context: ResolutionContext): Node | null | undefined {
  const caller = context.getNodeById?.(ref.fromNodeId);
  if (!caller || caller.kind !== 'method' || caller.language !== 'ruby' || caller.isStatic === true) return undefined;
  const owner = innermostNamespace(ref, context);
  if (!owner || owner.kind !== 'class') return undefined;
  let memo = IVAR_CLASSES.get(context);
  if (!memo) IVAR_CLASSES.set(context, (memo = new Map()));
  const key = `${owner.qualifiedName}\0${name}`;
  if (memo.has(key)) return memo.get(key);
  const found = ivarClassFromInitialize(name, owner.qualifiedName, context);
  memo.set(key, found);
  return found;
}

function ivarClassFromInitialize(name: string, owner: string, context: ResolutionContext): Node | null | undefined {
  let written: { path: string; absolute: boolean; file: string; line: number } | undefined;
  for (const init of context.getNodesByQualifiedName(`${owner}::initialize`)) {
    if (init.kind !== 'method' || init.language !== 'ruby' || init.isStatic === true) continue;
    const lines = context.getFileLines?.(init.filePath) ?? context.readFile(init.filePath)?.split(/\r?\n/);
    if (!lines) return undefined;
    const body = stripCommentsForRegex(lines.slice(init.startLine - 1, init.endLine).join('\n'), 'ruby');
    const assigned = constructedAssignments(body, name);
    if (!assigned) return undefined;
    for (const a of assigned) {
      if (written && (written.path !== a.path || written.absolute !== a.absolute)) return undefined;
      written ??= { path: a.path, absolute: a.absolute, file: init.filePath, line: init.startLine + a.line };
    }
  }
  if (!written) return undefined;
  const at: UnresolvedRef = {
    fromNodeId: '', referenceName: written.path, referenceKind: 'references',
    line: written.line, column: 0, filePath: written.file, language: 'ruby',
  };
  return constantClass(written.path, written.absolute, at, context);
}

/** Drops what was read from the files of a context whose caches are cleared. */
export function clearRubyFlowScopeMemos(context: ResolutionContext): void {
  RUBY_MIXINS.delete(context);
  RUBY_ANCESTRY.delete(context);
  RUBY_METHOD_OWNERS.delete(context);
  IVAR_CLASSES.delete(context);
}
