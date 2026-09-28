import * as path from 'path';
import type { TreeNode as SyntaxNode } from '../../extraction/parse-tree';
import { detectLanguage } from '../../extraction/grammars';
import { parseSourceTreeSync } from '../../extraction/parse-tree';
import type { Node } from '../../types';
import type {
  FrameworkExtractionResult,
  FrameworkResolver,
  ResolutionContext,
  UnresolvedRef,
} from '../types';
import { dependsOn } from './package-deps';
import { recordSkip, skipped } from '../synth-skips';

const DETECT_PASS = 'detect:http-routing';

type Framework = 'hono' | 'elysia' | 'fastify' | 'hyper-express' | 'koa' | 'h3' | 'vixeny';
type Binding =
  | Router
  | { kind: 'module'; source: string }
  | { kind: 'factory'; framework: Framework }
  | { kind: 'serve' }
  | { kind: 'effect' }
  | { kind: 'wrap'; prefix: string | null }
  | { kind: 'middleware'; router: Router };
type Scope = Map<string, Binding | null>;
interface Router {
  kind: 'router';
  framework: Framework;
  prefix: string | null;
  mounts: { parent: Router; path: string | null; before?: number }[];
  routePath?: string | null;
}
interface PendingRoute {
  router: Router;
  method: string;
  path: string;
  site: SyntaxNode;
  handler: SyntaxNode | null;
}

const PACKAGES = [
  'hono',
  'elysia',
  'fastify',
  'hyper-express',
  '@koa/router',
  'koa-router',
  'h3',
  'vixeny',
  'effect',
  '@types/bun',
];
const METHODS = new Set([
  'GET',
  'POST',
  'PUT',
  'PATCH',
  'DELETE',
  'HEAD',
  'OPTIONS',
  'TRACE',
  'CONNECT',
]);
const FUNCTIONS = new Set([
  'arrow_function',
  'function_expression',
  'function_declaration',
  'generator_function',
  'method_definition',
]);
const SOURCE_HINT =
  /\bBun\s*\.\s*serve\b|['"](?:hono|elysia|fastify|hyper-express|@koa\/router|koa-router|h3|vixeny|bun|effect\/unstable\/http(?:\/HttpRouter)?)['"]/;
/** A Fastify plugin file names its instance `fastify`, or imports a Fastify package or type. */
const FASTIFY_PLUGIN_HINT = /\bfastify\b|\bFastify(?:Instance|Plugin)/;
const FASTIFY_TYPED = /['"](?:fastify|@fastify\/[\w-]+)['"]|\bFastify(?:Instance|Plugin)/;
/**
 * Marks, in `qualifiedName`, a route declared on the instance a default-exported
 * Fastify plugin receives. Its full path depends on how the plugin is
 * registered, which `postExtract` reads from @fastify/autoload.
 */
export const FASTIFY_PLUGIN_MARKER = 'fastify-plugin:';

function field(node: SyntaxNode, name: string): SyntaxNode | null {
  return node.childForFieldName(name);
}
function unwrap(node: SyntaxNode | null): SyntaxNode | null {
  while (
    node &&
    [
      'parenthesized_expression',
      'as_expression',
      'satisfies_expression',
      'non_null_expression',
    ].includes(node.type)
  )
    node = node.namedChildren[0] ?? null;
  return node;
}
function literal(node: SyntaxNode | null): string | null {
  node = unwrap(node);
  if (!node || !['string', 'template_string'].includes(node.type)) return null;
  if (
    node.namedChildren.some((n) => n.type === 'template_substitution') ||
    node.text.includes('\\')
  )
    return null;
  return node.text.slice(1, -1);
}
function strings(node: SyntaxNode | null): string[] {
  if (node?.type === 'array') {
    const items = node.namedChildren.map(literal);
    return items.every((s) => s !== null) ? (items as string[]) : [];
  }
  const s = literal(node);
  return s === null ? [] : [s];
}
function key(node: SyntaxNode | null): string | null {
  return node &&
    ['property_identifier', 'identifier', 'shorthand_property_identifier'].includes(node.type)
    ? node.text
    : literal(node);
}
function properties(node: SyntaxNode | null): Map<string, SyntaxNode> {
  const out = new Map<string, SyntaxNode>();
  node = unwrap(node);
  if (node?.type !== 'object') return out;
  for (const child of node.namedChildren) {
    // A spread or computed key can replace any preceding field.
    if (child.type === 'spread_element' || field(child, 'key')?.type === 'computed_property_name')
      return new Map();
    const name = key(field(child, 'key') ?? field(child, 'name') ?? child);
    const value = child.type === 'pair' ? field(child, 'value') : child;
    if (name && value) out.set(name, value);
  }
  return out;
}
function optionPrefix(node: SyntaxNode | null): string | null {
  if (!node) return '';
  node = unwrap(node);
  if (
    node?.type !== 'object' ||
    node.namedChildren.some(
      (n) => n.type === 'spread_element' || field(n, 'key')?.type === 'computed_property_name',
    )
  )
    return null;
  const value = properties(node).get('prefix');
  return value ? literal(value) : '';
}
function join(prefix: string | null, path: string | null): string | null {
  if (prefix === null || path === null) return null;
  return prefix.replace(/\/$/, '') + (path.startsWith('/') ? path : '/' + path);
}
function library(source: string, name: string): Binding | null {
  if (source === 'bun') return name === 'serve' ? { kind: 'serve' } : null;
  if (
    (source === 'effect/unstable/http' && name === 'HttpRouter') ||
    (source === 'effect/unstable/http/HttpRouter' && name === '*')
  )
    return { kind: 'effect' };
  const framework: Framework | undefined = (
    {
      hono: 'hono',
      elysia: 'elysia',
      fastify: 'fastify',
      'hyper-express': 'hyper-express',
      '@koa/router': 'koa',
      'koa-router': 'koa',
      h3: 'h3',
      vixeny: 'vixeny',
    } as Record<string, Framework>
  )[source];
  if (!framework) return null;
  const constructors: Record<Framework, string[]> = {
    hono: ['Hono'],
    elysia: ['Elysia', 'default'],
    fastify: ['fastify', 'default'],
    'hyper-express': ['Server', 'Router'],
    koa: ['default', 'Router'],
    h3: ['H3', 'createRouter', 'createApp'],
    vixeny: ['wrap'],
  };
  return constructors[framework].includes(name) ? { kind: 'factory', framework } : null;
}

/** Source-level declarations only; the same hook runs after native and WASM extraction. */
export function extractHttpRoutes(
  filePath: string,
  source: string,
): FrameworkExtractionResult & { callStarts: Set<number> } {
  const result = {
    nodes: [] as Node[],
    references: [] as UnresolvedRef[],
    callStarts: new Set<number>(),
  };
  if (
    !/\.(?:[cm]?[jt]s|[jt]sx)$/.test(filePath) ||
    (!SOURCE_HINT.test(source) && !FASTIFY_PLUGIN_HINT.test(source))
  )
    return result;
  const language = detectLanguage(filePath);
  const tree = parseSourceTreeSync(source, language);
  if (!tree) return result;
  const pending: PendingRoute[] = [];
  const plugin = fastifyPlugin(tree.rootNode, source);
  const pluginRouter: Router | null = plugin
    ? { kind: 'router', framework: 'fastify', prefix: '', mounts: [] }
    : null;
  const scopes: Scope[] = [new Map([['Bun', { kind: 'module', source: 'bun' }]])];
  const scope = () => scopes[scopes.length - 1]!;
  const lookup = (name: string): Binding | null => {
    for (let i = scopes.length - 1; i >= 0; i--)
      if (scopes[i]!.has(name)) return scopes[i]!.get(name) ?? null;
    return null;
  };
  const router = (framework: Framework, prefix: string | null = ''): Router => ({
    kind: 'router',
    framework,
    prefix,
    mounts: [],
  });
  function bindPattern(node: SyntaxNode | null, value: Binding | null = null): void {
    if (!node) return;
    if (node.type === 'identifier') {
      scope().set(node.text, value);
      return;
    }
    if (
      ['required_parameter', 'optional_parameter', 'assignment_pattern', 'rest_pattern'].includes(
        node.type,
      )
    ) {
      bindPattern(
        field(node, 'pattern') ?? field(node, 'left') ?? node.namedChildren[0] ?? null,
        value,
      );
    } else if (node.type === 'object_pattern' || node.type === 'array_pattern') {
      for (const child of node.namedChildren) {
        const name = field(child, 'key')?.text ?? child.text;
        bindPattern(
          field(child, 'value') ?? child,
          value?.kind === 'module' ? library(value.source, name) : null,
        );
        if (child.type === 'shorthand_property_identifier_pattern')
          scope().set(child.text, value?.kind === 'module' ? library(value.source, name) : null);
      }
    }
  }
  function predeclare(block: SyntaxNode): void {
    for (const raw of block.namedChildren) {
      const node = raw.type === 'export_statement' ? (field(raw, 'declaration') ?? raw) : raw;
      if (['lexical_declaration', 'variable_declaration'].includes(node.type)) {
        for (const d of node.namedChildren) bindPattern(field(d, 'name'));
      } else if (['function_declaration', 'class_declaration'].includes(node.type))
        bindPattern(field(node, 'name'));
    }
  }
  function visitFunction(node: SyntaxNode, first: Binding | null = null): void {
    if (plugin && node.startIndex === plugin.startIndex && node.type === plugin.type)
      first ??= pluginRouter;
    scopes.push(new Map());
    const parameters =
      field(node, 'parameters')?.namedChildren ??
      [field(node, 'parameter')].filter((n): n is SyntaxNode => !!n);
    parameters.forEach((p, i) => bindPattern(p, i === 0 ? first : null));
    bindPattern(field(node, 'name'));
    const body = field(node, 'body');
    if (body) {
      predeclare(body);
      visit(body, false);
    }
    scopes.pop();
  }
  function add(
    r: Router,
    methods: string[],
    paths: string[],
    site: SyntaxNode,
    handler: SyntaxNode | null,
  ): void {
    for (let method of methods) {
      method = method.toUpperCase();
      if (method === '*' || method === 'ALL' || method === 'ANY') method = 'ANY';
      if (!METHODS.has(method) && method !== 'ANY') continue;
      for (const path of paths) {
        if (path.startsWith('/') || path === '*')
          pending.push({ router: r, method, path, site, handler });
      }
    }
  }
  function member(node: SyntaxNode): { object: SyntaxNode; name: string } | null {
    const object = field(node, 'object');
    const name = key(field(node, 'property'));
    return object && name ? { object, name } : null;
  }
  function evalCall(node: SyntaxNode): Binding | null {
    const callee = field(node, node.type === 'new_expression' ? 'constructor' : 'function');
    const args = field(node, 'arguments')?.namedChildren ?? [];
    if (!callee) return null;
    if (callee.text === 'require' && !scopes.some((s) => s.has('require'))) {
      const source = literal(args[0] ?? null);
      return source && SOURCE_HINT.test(JSON.stringify(source)) ? { kind: 'module', source } : null;
    }
    const m = callee.type === 'member_expression' ? member(callee) : null;
    const owner = m ? evaluate(m.object) : null;
    const binding = m
      ? owner?.kind === 'module'
        ? library(owner.source, m.name)
        : null
      : evaluate(callee);
    if (
      binding?.kind === 'factory' ||
      (binding?.kind === 'module' &&
        ['fastify', '@koa/router', 'koa-router', 'elysia'].includes(binding.source))
    ) {
      const f =
        binding.kind === 'factory'
          ? binding.framework
          : (library(binding.source, 'default') as { framework: Framework }).framework;
      if (f === 'vixeny') {
        // Vixeny's wrap options depend on the terminal operation (unwrap vs
        // compose). Only the option-free builder has an unambiguous path here.
        return { kind: 'wrap', prefix: args.length ? null : '' };
      }
      return router(f, ['elysia', 'koa'].includes(f) ? optionPrefix(args[0] ?? null) : '');
    }
    if (binding?.kind === 'wrap') return router('vixeny', binding.prefix);
    if (binding?.kind === 'serve') {
      const routes = properties(properties(args[0] ?? null).get('routes') ?? null);
      const root = router('h3');
      for (const [path, value] of routes) {
        if (value.type === 'false' || value.type === 'undefined') continue;
        const entries = properties(value);
        if (value.type === 'object' && !entries.has('dir')) {
          for (const [method, handler] of entries)
            if (/^(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)$/.test(method))
              add(root, [method], [path], value, handler);
        } else add(root, ['ANY'], [path], value, value);
      }
      return null;
    }
    if (owner?.kind === 'effect' && m && ['add', 'route'].includes(m.name)) {
      const allowed =
        m.name === 'add'
          ? /^(GET|POST|PATCH|PUT|DELETE|OPTIONS|\*)$/
          : /^(GET|POST|PATCH|PUT|DELETE|OPTIONS|HEAD|TRACE|\*)$/;
      add(
        router('h3'),
        strings(args[0] ?? null).filter((s) => allowed.test(s)),
        strings(args[1] ?? null),
        node,
        args[2] ?? null,
      );
      return null;
    }
    if (owner?.kind === 'router' && m) {
      result.callStarts.add(node.startIndex);
      const r = owner;
      const f = r.framework;
      const method = m.name;
      if (method === 'basePath' && f === 'hono')
        return { ...r, prefix: join(r.prefix, literal(args[0] ?? null)) };
      if (method === 'prefix' && f === 'koa') {
        r.prefix = literal(args[0] ?? null);
        return r;
      }
      if ((method === 'group' && f === 'elysia') || (method === 'register' && f === 'fastify')) {
        const callback = f === 'elysia' ? args[args.length - 1] : args[0];
        const child = router(f);
        const prefix = f === 'elysia' ? literal(args[0] ?? null) : optionPrefix(args[1] ?? null);
        child.mounts.push({ parent: r, path: prefix });
        if (callback && FUNCTIONS.has(callback.type)) visitFunction(callback, child);
        return r;
      }
      if (
        (method === 'route' && f === 'hono') ||
        (method === 'use' && ['koa', 'hyper-express'].includes(f)) ||
        (method === 'mount' && f === 'h3')
      ) {
        const value = args[1] ? evaluate(args[1]) : null;
        const child = value?.kind === 'middleware' ? value.router : value;
        if (child?.kind === 'router')
          child.mounts.push({
            parent: r,
            path: literal(args[0] ?? null),
            before: f === 'hono' ? pending.length : undefined,
          });
        return r;
      }
      if (method === 'routes' && f === 'koa') return { kind: 'middleware', router: r };
      if (method === 'route' && f === 'hyper-express')
        return { ...r, routePath: literal(args[0] ?? null) };
      if (f === 'vixeny') {
        if (['get', 'post', 'put', 'delete', 'route'].includes(method)) {
          const opts = properties(args[0] ?? null);
          add(
            r,
            method === 'route' ? strings(opts.get('method') ?? null) : [method],
            strings(opts.get('path') ?? null),
            node,
            opts.get('f') ?? null,
          );
        }
        return ['get', 'post', 'put', 'delete', 'route'].includes(method) ? r : null;
      }
      if (method === 'route' && f === 'fastify') {
        const opts = properties(args[0] ?? null);
        add(
          r,
          strings(opts.get('method') ?? null),
          strings(opts.get('url') ?? opts.get('path') ?? null),
          node,
          opts.get('handler') ?? null,
        );
      } else if (
        (method === 'on' && ['hono', 'h3'].includes(f)) ||
        (method === 'route' && f === 'elysia')
      ) {
        add(
          r,
          strings(args[0] ?? null),
          strings(args[1] ?? null),
          node,
          f === 'hono' ? (args[args.length - 1] ?? null) : (args[2] ?? null),
        );
      } else if (
        METHODS.has(method.toUpperCase()) ||
        method === 'all' ||
        (method === 'any' && f === 'hyper-express') ||
        (method === 'del' && f === 'koa')
      ) {
        const pathIndex =
          f === 'koa' && args[1] && ['string', 'template_string', 'array'].includes(args[1].type)
            ? 1
            : 0;
        const paths =
          r.routePath !== undefined
            ? r.routePath === null
              ? []
              : [r.routePath]
            : strings(args[pathIndex] ?? null);
        let handler = ['elysia', 'h3'].includes(f) ? args[pathIndex + 1] : args[args.length - 1];
        if (f === 'fastify' && handler?.type === 'object')
          handler = properties(handler).get('handler');
        add(r, [method === 'del' ? 'DELETE' : method], paths, node, handler ?? null);
      } else if (method !== 'use') return null;
      // Inspect inline handler bodies in their own scope, never as router callbacks.
      for (const arg of args) if (FUNCTIONS.has(arg.type)) visitFunction(arg);
      return r;
    }
    for (const arg of args) evaluate(arg);
    return null;
  }
  function evaluate(raw: SyntaxNode): Binding | null {
    const node = unwrap(raw)!;
    if (node.type === 'identifier') return lookup(node.text);
    if (node.type === 'call_expression' || node.type === 'new_expression') return evalCall(node);
    if (node.type === 'member_expression') {
      const m = member(node);
      const value = m ? evaluate(m.object) : null;
      return value?.kind === 'module' ? library(value.source, m!.name) : null;
    }
    if (FUNCTIONS.has(node.type)) {
      visitFunction(node);
      return null;
    }
    if (
      ['assignment_expression', 'augmented_assignment_expression', 'update_expression'].includes(
        node.type,
      )
    ) {
      const left = field(node, 'left') ?? field(node, 'argument');
      const right = field(node, 'right');
      if (right) evaluate(right);
      if (left?.type === 'identifier')
        for (let i = scopes.length - 1; i >= 0; i--)
          if (scopes[i]!.has(left.text)) {
            scopes[i]!.set(left.text, null);
            break;
          }
      return null;
    }
    for (const child of node.namedChildren) visit(child);
    return null;
  }
  function visit(node: SyntaxNode, newScope = true): void {
    if (node.type === 'catch_clause') {
      scopes.push(new Map());
      bindPattern(field(node, 'parameter'));
      const body = field(node, 'body');
      if (body) visit(body, false);
      scopes.pop();
      return;
    }
    if (node.type === 'import_statement') {
      if (/^import\s+type\b/.test(node.text)) return;
      const source = literal(field(node, 'source'));
      if (!source) return;
      const clause = node.namedChildren.find((n) => n.type === 'import_clause');
      for (const item of clause?.namedChildren ?? []) {
        if (item.type === 'identifier')
          scope().set(item.text, library(source, 'default') ?? { kind: 'module', source });
        if (item.type === 'namespace_import')
          scope().set(
            item.namedChildren[0]!.text,
            library(source, '*') ?? { kind: 'module', source },
          );
        if (item.type === 'named_imports')
          for (const spec of item.namedChildren) {
            const name = field(spec, 'name');
            const alias = field(spec, 'alias') ?? name;
            if (name && alias)
              scope().set(
                alias.text,
                /^type\s/.test(spec.text) ? null : library(source, name.text),
              );
          }
      }
      return;
    }
    if (node.type === 'program' || node.type === 'statement_block') {
      if (newScope) scopes.push(new Map());
      predeclare(node);
      for (const child of node.namedChildren) visit(child);
      if (newScope) scopes.pop();
      return;
    }
    if (node.type === 'variable_declarator') {
      const value = field(node, 'value');
      const binding = value ? evaluate(value) : null;
      bindPattern(field(node, 'name'), node.parent?.text.startsWith('const ') ? binding : null);
      return;
    }
    evaluate(node);
  }
  function prefixes(r: Router, index: number, seen = new Set<Router>()): string[] {
    if (seen.has(r) || r.prefix === null) return [];
    if (!r.mounts.length) return [r.prefix];
    const next = new Set(seen).add(r);
    return r.mounts.flatMap((m) =>
      m.before !== undefined && index >= m.before
        ? []
        : prefixes(m.parent, index, next).flatMap((p) => {
            const prefix = join(join(p, m.path), r.prefix);
            return prefix === null ? [] : [prefix.replace(/\/$/, '')];
          }),
    );
  }
  const rootedInPlugin = (r: Router, seen = new Set<Router>()): boolean => {
    if (r === pluginRouter) return true;
    if (seen.has(r)) return false;
    seen.add(r);
    return r.mounts.some((m) => rootedInPlugin(m.parent, seen));
  };
  try {
    visit(tree.rootNode, false);
    const emitted = new Set<string>();
    for (const [index, entry] of pending.entries())
      for (const prefix of prefixes(entry.router, index)) {
        // Hono and Fastify both serve a prefixed `/` at the bare prefix.
        const path =
          ['hono', 'fastify'].includes(entry.router.framework) && prefix && entry.path === '/'
            ? prefix
            : join(prefix, entry.path)!;
        const line = entry.site.startPosition.row + 1;
        const name = `${entry.method} ${path}`;
        const id = `route:${filePath}:${line}:${entry.site.startPosition.column}:${name}`;
        if (emitted.has(id)) continue;
        emitted.add(id);
        const node: Node = {
          id,
          kind: 'route',
          name,
          qualifiedName: `${filePath}::${rootedInPlugin(entry.router) ? FASTIFY_PLUGIN_MARKER : ''}${name}`,
          filePath,
          language,
          startLine: line,
          endLine: entry.site.endPosition.row + 1,
          startColumn: entry.site.startPosition.column,
          endColumn: entry.site.endPosition.column,
          updatedAt: Date.now(),
        };
        result.nodes.push(node);
        result.references.push(...httpHandlerReferences(node, entry.handler));
      }
    return result;
  } finally {
    tree.delete();
  }
}

/**
 * The function a file exports as its default Fastify plugin: `export default`,
 * or `module.exports =`, of a function or of a top-level `const`/function
 * declaration naming one. A wrapped export (`fastify-plugin`'s `fp(...)`)
 * shares its parent's context and is not a route plugin here.
 */
function fastifyPlugin(program: SyntaxNode, source: string): SyntaxNode | null {
  let value: SyntaxNode | null = null;
  for (const statement of program.namedChildren) {
    if (statement.type === 'export_statement' && statement.children.some((c) => c.type === 'default'))
      value = field(statement, 'declaration') ?? field(statement, 'value');
    const assignment = statement.type === 'expression_statement' ? statement.namedChildren[0] : null;
    if (assignment?.type === 'assignment_expression' && field(assignment, 'left')?.text === 'module.exports')
      value = field(assignment, 'right');
  }
  value = unwrap(value);
  if (value?.type === 'identifier') {
    const name = value.text;
    value = null;
    for (const raw of program.namedChildren) {
      const node = raw.type === 'export_statement' ? (field(raw, 'declaration') ?? raw) : raw;
      if (node.type === 'function_declaration' && field(node, 'name')?.text === name) value = node;
      if (node.type === 'lexical_declaration' && node.text.startsWith('const'))
        for (const d of node.namedChildren)
          if (field(d, 'name')?.text === name) value = unwrap(field(d, 'value'));
    }
  }
  if (!value || !FUNCTIONS.has(value.type)) return null;
  const first =
    field(value, 'parameters')?.namedChildren[0] ?? field(value, 'parameter') ?? null;
  if (!first) return null;
  const name = first.type === 'identifier' ? first : field(first, 'pattern');
  return name?.text === 'fastify' || FASTIFY_TYPED.test(source) ? value : null;
}

/** Bind a named handler, or the direct calls made by an anonymous handler. */
export function httpHandlerReferences(route: Node, raw: SyntaxNode | null): UnresolvedRef[] {
  const references: UnresolvedRef[] = [];
  const reference = (target: SyntaxNode, kind: 'references' | 'calls'): void => {
    const name = target.text;
    if (!name || !/^[A-Za-z_$][\w$]*$/.test(name)) return;
    references.push({
      fromNodeId: route.id,
      referenceName: name,
      referenceKind: kind,
      filePath: route.filePath,
      language: route.language,
      line: target.startPosition.row + 1,
      column: target.startPosition.column,
    });
  };
  const handler = unwrap(raw);
  if (!handler) return references;
  if (['identifier', 'shorthand_property_identifier'].includes(handler.type))
    reference(handler, 'references');
  if (!FUNCTIONS.has(handler.type)) return references;
  const locals = new Set<string>();
  const namesIn = (node: SyntaxNode | null): void => {
    if (!node) return;
    if (['identifier', 'shorthand_property_identifier_pattern'].includes(node.type))
      locals.add(node.text);
    else for (const child of node.namedChildren) namesIn(child);
  };
  namesIn(field(handler, 'parameters') ?? field(handler, 'parameter'));
  namesIn(field(handler, 'name'));
  const declarations = (node: SyntaxNode): void => {
    if (
      node.type === 'variable_declarator' ||
      node.type === 'function_declaration' ||
      node.type === 'class_declaration'
    )
      namesIn(field(node, 'name'));
    if (node.type === 'catch_clause') namesIn(field(node, 'parameter'));
    if (!FUNCTIONS.has(node.type)) for (const child of node.namedChildren) declarations(child);
  };
  const calls = (node: SyntaxNode): void => {
    if (FUNCTIONS.has(node.type)) return;
    if (node.type === 'call_expression') {
      const callee = field(node, 'function');
      // The normal extraction pass retains member receivers. A bare member
      // name here could otherwise resolve to an unrelated same-named function.
      if (callee?.type === 'identifier' && !locals.has(callee.text)) reference(callee, 'calls');
    }
    for (const child of node.namedChildren) calls(child);
  };
  const body = field(handler, 'body');
  if (body) {
    declarations(body);
    calls(body);
  }
  return references;
}

// @fastify/autoload 6.5.0 defaults (index.js): which files load, and which are hooks.
const AUTOLOAD_SCRIPT = /(?:(?:^.?|\.[^d]|[^.]d|[^.][^d])\.ts|\.js|\.cjs|\.mjs|\.cts|\.mts)$/i;
const AUTOLOAD_INDEX = /^index(?:\.ts|\.js|\.cjs|\.mjs|\.cts|\.mts)$/i;
const AUTOLOAD_HOOKS = /^[_.]?auto_?hooks(?:\.ts|\.js|\.cjs|\.mjs|\.cts|\.mts)$/i;
/** Options that change which files load or where they mount are not modelled. */
const AUTOLOAD_OPTIONS = new Set([
  'dir',
  'options',
  'dirNameRoutePrefix',
  'routeParams',
  'appendAutoPrefix',
  'forceESM',
  'autoHooks',
  'cascadeHooks',
  'overwriteHooks',
]);

interface AutoloadRegistration {
  dir: string;
  prefix: string;
  dirNames: boolean;
  routeParams: boolean;
  appendAutoPrefix: boolean;
}

function bool(node: SyntaxNode | undefined, fallback: boolean): boolean | null {
  if (!node) return fallback;
  node = unwrap(node)!;
  return node.type === 'true' ? true : node.type === 'false' ? false : null;
}

/** `path.join(__dirname, 'routes')` or `join(import.meta.dirname, 'routes')`, relative to the project. */
function autoloadDir(file: string, node: SyntaxNode | undefined): string | null {
  node = unwrap(node ?? null) ?? undefined;
  if (node?.type !== 'call_expression') return null;
  if (!/^(?:path\.)?(?:join|resolve)$/.test(field(node, 'function')?.text ?? '')) return null;
  const [base, ...rest] = field(node, 'arguments')?.namedChildren ?? [];
  if (!base || !['__dirname', 'import.meta.dirname'].includes(base.text)) return null;
  const segments = rest.map(literal);
  if (!segments.length || segments.some((s) => s === null || s.startsWith('/'))) return null;
  const dir = path.posix.join(path.posix.dirname(file), ...(segments as string[]));
  return dir.startsWith('..') ? null : dir;
}

/** Literal `fastify.register(AutoLoad, { dir, ... })` calls in one file. */
function autoloadRegistrations(file: string, source: string): AutoloadRegistration[] {
  const tree = parseSourceTreeSync(source, detectLanguage(file));
  if (!tree) return [];
  const out: AutoloadRegistration[] = [];
  const bindings = new Set<string>();
  const isAutoload = (node: SyntaxNode | null) =>
    /^['"](?:@fastify\/autoload|fastify-autoload)['"]$/.test(node?.text ?? '');
  const walk = (node: SyntaxNode): void => {
    if (node.type === 'import_statement' && isAutoload(field(node, 'source'))) {
      for (const item of node.namedChildren.find((n) => n.type === 'import_clause')?.namedChildren ?? []) {
        if (item.type === 'identifier') bindings.add(item.text);
        if (item.type === 'named_imports')
          for (const spec of item.namedChildren)
            if (['fastifyAutoload', 'default'].includes(field(spec, 'name')?.text ?? ''))
              bindings.add((field(spec, 'alias') ?? field(spec, 'name'))!.text);
      }
      return;
    }
    if (node.type === 'variable_declarator') {
      const value = unwrap(field(node, 'value'));
      const name = field(node, 'name');
      if (
        value?.type === 'call_expression' &&
        field(value, 'function')?.text === 'require' &&
        isAutoload(field(value, 'arguments')?.namedChildren[0] ?? null) &&
        name?.type === 'identifier'
      )
        bindings.add(name.text);
    }
    if (node.type === 'call_expression') {
      const callee = field(node, 'function');
      const args = field(node, 'arguments')?.namedChildren ?? [];
      if (
        callee?.type === 'member_expression' &&
        key(field(callee, 'property')) === 'register' &&
        bindings.has(args[0]?.text ?? '')
      ) {
        const registration = autoloadRegistration(file, args[1] ?? null);
        if (registration) out.push(registration);
      }
    }
    for (const child of node.namedChildren) walk(child);
  };
  try {
    walk(tree.rootNode);
  } finally {
    tree.delete();
  }
  return out;
}

function autoloadRegistration(file: string, raw: SyntaxNode | null): AutoloadRegistration | null {
  const node = unwrap(raw);
  if (node?.type !== 'object') return null;
  const props = properties(node);
  if (!props.size || [...props.keys()].some((k) => !AUTOLOAD_OPTIONS.has(k))) return null;
  const dir = autoloadDir(file, props.get('dir'));
  const dirNames = bool(props.get('dirNameRoutePrefix'), true);
  const routeParams = bool(props.get('routeParams'), false);
  const appendAutoPrefix = bool(props.get('appendAutoPrefix'), false);
  if (!dir || dirNames === null || routeParams === null || appendAutoPrefix === null) return null;
  // Options forwarded from the caller (`opts`, `{ ...opts }`) are taken to
  // carry no prefix; only a literal `prefix` key sets one.
  let prefix: string | null = '';
  const options = unwrap(props.get('options') ?? null);
  if (options?.type === 'object')
    for (const child of options.namedChildren)
      if (child.type === 'pair' && key(field(child, 'key')) === 'prefix')
        prefix = literal(field(child, 'value'));
  if (prefix === null) return null;
  return { dir, prefix: prefix.replace(/\/$/, ''), dirNames, routeParams, appendAutoPrefix };
}

/**
 * The prefix values a plugin file exports for autoload: `autoPrefix`,
 * `prefixOverride`, `autoConfig.prefix` and `autoload`. Null when one is set
 * to something other than a literal.
 */
function pluginExports(file: string, source: string): Map<string, string | boolean | undefined> | null {
  const tree = parseSourceTreeSync(source, detectLanguage(file));
  if (!tree) return null;
  const out = new Map<string, string | boolean | undefined>();
  const owners = new Set(['module.exports', 'exports']);
  try {
    for (const statement of tree.rootNode.namedChildren) {
      if (statement.type === 'export_statement' && statement.children.some((c) => c.type === 'default')) {
        const value = field(statement, 'value') ?? field(field(statement, 'declaration') ?? statement, 'name');
        if (value?.type === 'identifier') owners.add(value.text);
      }
      const assignment = statement.type === 'expression_statement' ? statement.namedChildren[0] : null;
      if (assignment?.type === 'assignment_expression') {
        const left = field(assignment, 'left');
        const right = field(assignment, 'right');
        if (left?.text === 'module.exports' && right?.type === 'identifier') owners.add(right.text);
      }
    }
    const set = (name: string, value: SyntaxNode | null): boolean => {
      if (!['autoPrefix', 'prefixOverride', 'autoload', 'autoConfig'].includes(name)) return true;
      const node = unwrap(value);
      if (name === 'autoload') {
        const flag = bool(node ?? undefined, true);
        out.set(name, flag ?? true);
        return flag !== null;
      }
      if (name === 'autoConfig') {
        if (node?.type !== 'object' || node.namedChildren.some((n) => n.type === 'spread_element'))
          return false;
        const prefix = properties(node).get('prefix');
        out.set('configPrefix', prefix ? (literal(prefix) ?? undefined) : undefined);
        return !prefix || literal(prefix) !== null;
      }
      const text = literal(node);
      out.set(name, text ?? undefined);
      return text !== null;
    };
    for (const statement of tree.rootNode.namedChildren) {
      const declared = statement.type === 'export_statement' ? field(statement, 'declaration') : null;
      if (declared?.type === 'lexical_declaration')
        for (const d of declared.namedChildren)
          if (!set(field(d, 'name')?.text ?? '', field(d, 'value'))) return null;
      const assignment = statement.type === 'expression_statement' ? statement.namedChildren[0] : null;
      const left = assignment?.type === 'assignment_expression' ? field(assignment, 'left') : null;
      if (left?.type === 'member_expression' && owners.has(field(left, 'object')?.text ?? ''))
        if (!set(key(field(left, 'property')) ?? '', field(assignment!, 'right'))) return null;
    }
  } finally {
    tree.delete();
  }
  return out;
}

/** Fastify joins a prefix and `/` to the bare prefix. */
function fastifyJoin(prefix: string, route: string): string {
  if (!prefix) return route;
  return route === '/' ? prefix : prefix.replace(/\/$/, '') + route;
}

/**
 * @fastify/autoload mounts each plugin file under its directory path. This
 * renames the routes a default-exported plugin declares to the path they are
 * served at, recomputed from the in-file name kept after the
 * `qualifiedName` marker, so a repeat run (or a removed registration) is exact.
 */
function fastifyAutoloadRoutes(context: ResolutionContext): Node[] {
  const marked = new Map<string, Node[]>();
  const routes = context.iterateNodesByKind?.('route') ?? context.getNodesByKind('route');
  for (const route of routes) {
    if (!route.qualifiedName.includes(`::${FASTIFY_PLUGIN_MARKER}`)) continue;
    const list = marked.get(route.filePath) ?? [];
    list.push(route);
    marked.set(route.filePath, list);
  }
  if (!marked.size) return [];

  const files = context.getAllFiles();
  const registrations: AutoloadRegistration[] = [];
  for (const file of files) {
    if (!/\.(?:[cm]?[jt]s|[jt]sx)$/.test(file)) continue;
    const source = context.readFile(file);
    if (source && /['"](?:@fastify\/autoload|fastify-autoload)['"]/.test(source))
      registrations.push(...autoloadRegistrations(file, source));
  }
  const indexed = new Set(
    files.filter((f) => AUTOLOAD_INDEX.test(path.posix.basename(f))).map((f) => path.posix.dirname(f)),
  );

  const updates: Node[] = [];
  for (const [file, list] of marked) {
    const name = path.posix.basename(file);
    const directory = path.posix.dirname(file);
    let prefix: string | null | undefined;
    const loaded =
      AUTOLOAD_SCRIPT.test(name) &&
      !AUTOLOAD_HOOKS.test(name) &&
      (AUTOLOAD_INDEX.test(name) || !indexed.has(directory));
    const covering = loaded ? registrations.filter((r) => file.startsWith(r.dir + '/')) : [];
    const exported = covering.length ? pluginExports(file, context.readFile(file) ?? '') : null;
    for (const r of covering) {
      if (!exported || exported.get('autoload') === false) {
        prefix = null;
        break;
      }
      const segments = path.posix.relative(r.dir, directory).split('/').filter(Boolean);
      const dirPrefix = r.dirNames ? segments.map((s) => '/' + s).join('') : '';
      const autoPrefix = exported.get('autoPrefix') as string | undefined;
      const override = exported.get('prefixOverride') as string | undefined;
      // handlePrefixConfig in @fastify/autoload 6.5.0.
      const base = r.prefix || ((exported.get('configPrefix') as string | undefined) ?? '').replace(/\/$/, '');
      const own =
        r.appendAutoPrefix && autoPrefix !== undefined && dirPrefix
          ? `${dirPrefix}/${autoPrefix}`
          : (autoPrefix ?? dirPrefix);
      let full = override !== undefined ? override : own ? base + own.replace(/\/+/g, '/') : base;
      if (r.routeParams && full)
        full = /__/.test(full) ? full.replace(/__/g, ':') : full.replace(/\/_/g, '/:');
      full = full.replace(/\/$/, '');
      // Two registrations reaching one file at different paths are ambiguous.
      prefix = prefix === undefined || prefix === full ? full : null;
      if (prefix === null) break;
    }
    for (const route of list) {
      const original = route.qualifiedName.slice(
        route.qualifiedName.indexOf(`::${FASTIFY_PLUGIN_MARKER}`) + 2 + FASTIFY_PLUGIN_MARKER.length,
      );
      const space = original.indexOf(' ');
      const renamed = prefix
        ? `${original.slice(0, space)} ${fastifyJoin(prefix, original.slice(space + 1))}`
        : original;
      if (renamed !== route.name) updates.push({ ...route, name: renamed });
    }
  }
  return updates;
}

export const httpRoutingResolver: FrameworkResolver = {
  name: 'http-routing',
  languages: ['javascript', 'typescript', 'jsx', 'tsx'],
  detect(context) {
    return (
      dependsOn(context, ...PACKAGES) ||
      context.fileExists('bun.lock') ||
      context.fileExists('bun.lockb') ||
      context.fileExists('bunfig.toml') ||
      context.getAllFiles().some((f) => {
        if (!/\.[cm]?[jt]sx?$/.test(f) || skipped(context, DETECT_PASS, f)) return false;
        const source = context.readFile(f);
        if (source !== null && SOURCE_HINT.test(source)) return true;
        recordSkip(context, DETECT_PASS, f, source);
        return false;
      })
    );
  },
  resolve: () => null,
  extract: extractHttpRoutes,
  postExtract: fastifyAutoloadRoutes,
};
