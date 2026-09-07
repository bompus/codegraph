/**
 * React Router — routes declared in markup, navigation written as a string.
 *
 * `frameworks/react.ts` already reads the route table out of the markup:
 * `<Route path="/payment" component={PaymentScreen}/>` (v5),
 * `<Route path="/payment" element={<PaymentScreen/>}/>` (v6) and
 * `createBrowserRouter([{ path, element }])` (v6.4+) each become a `route`
 * node named by its path, bound to the component that renders it. That is
 * half of what "how does this app flow" means. This file is the other half.
 *
 * **Navigation is a string.** `history.push('/placeorder')` (v5, and the
 * `useHistory` hook), `navigate('/placeorder')` (v6's `useNavigate`),
 * `router.navigate(…)` on a data router, `redirect('/login')` from a loader
 * or an action: the extractor records each as a call that resolves to
 * nothing, because the target is a path, not a symbol. `resolve()` claims
 * those refs, reads the argument off the source with the Expo Router readers
 * (a string, a template with holes, a `{ pathname }` object, a conditional
 * whose arms agree, a local `const href = …`), matches it against this
 * framework's own route table, and returns a **`navigates`** edge carrying
 * the href — the edge the Screens picture is drawn from and the step Steps
 * draws as another page. `<Link to>` and `<Navigate to>` are JSX attributes
 * rather than calls, so a synthesizer reads them instead
 * (`react-router-synthesizer.ts`).
 *
 * Precision rests on the string naming a real route: a computed path, a path
 * no route serves, and a conditional that forks are left unresolved rather
 * than guessed. `push` and `replace` are two of the most common method names
 * in JavaScript, so the receiver has to name a router — a bare `push` is an
 * array's, and is never claimed.
 *
 * Known limits, both deliberate: a nested route's path is relative to its
 * parent (`<Route path="team">` inside `<Route path="/dashboard">`), and the
 * markup scan does not compose that tree, so only an absolute path is a
 * destination an href can name; and a splat (`/admin/*`) matches anything, so
 * it is never the answer to a concrete href.
 */

import type { Language, Node } from '../../types';
import type { Node as SyntaxNode } from 'web-tree-sitter';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { resolveImportPath } from '../import-resolver';
import { stripCommentsForRegex } from '../strip-comments';
import { matchBracket } from './object-literal';
import type { FrameworkResolver, ResolutionContext, ResolvedRef, UnresolvedRef } from '../types';
import { dependsOn } from './package-deps';
import {
  addRouteTo,
  appRootFor,
  firstArgumentText,
  parseHrefExpression,
  readHrefViaLocal,
  routesForFile,
  type RootedRouteTable,
  type RouteTable,
} from './expo-router';
// `pageForHref` is framework-agnostic — it takes any RouteTable and decides
// which of its routes an href names (absolute URLs, holes, a conditional's
// two arms). It lives in `nextjs.ts` because that is where it was first
// needed; duplicating it here would be a second derivation of the same rule.
import { destinationsForHref } from './nextjs';

const ROUTE_LANGUAGES: readonly Language[] = ['typescript', 'javascript', 'tsx', 'jsx'];

/** Default Remix flat filenames; bracket escapes retain their literal meaning. */
export function remixFileRoutePath(filePath: string): string | null {
  const file = filePath.replace(/\\/g, '/');
  const match = /^app\/routes\/([^/]+)(?:\/(route))?\.[jt]sx?$/.exec(file);
  if (!match || match[1]!.startsWith('.')) return null;
  const segments: { raw: string; text: string }[] = [];
  let raw = '',
    text = '';
  const stem = match[1]!;
  for (let i = 0; i < stem.length; i++) {
    const char = stem[i]!;
    if (char === '[') {
      const end = stem.indexOf(']', i + 1);
      if (end < 0) return null;
      raw += stem.slice(i, end + 1);
      text += stem.slice(i + 1, end);
      i = end;
    } else if (char === '.') {
      segments.push({ raw, text });
      raw = '';
      text = '';
    } else {
      raw += char;
      text += char;
    }
  }
  segments.push({ raw, text });
  const path: string[] = [];
  for (const [i, segment] of segments.entries()) {
    let { raw, text } = segment;
    if (!raw) return null;
    const optional = raw.startsWith('(') && raw.endsWith(')');
    if (optional) {
      raw = raw.slice(1, -1);
      text = text.slice(1, -1);
    } else if (/[()]/.test(raw.replace(/\[[^\]]*\]/g, ''))) return null;
    if (raw === '_index') {
      if (i !== segments.length - 1) return null;
      continue;
    }
    if (raw.startsWith('_')) {
      if (i === segments.length - 1) return null;
      continue;
    }
    if (raw.endsWith('_')) text = text.slice(0, -1);
    if (raw === '$') text = '*';
    else if (raw.startsWith('$')) text = ':' + text.slice(1);
    if (optional) text += '?';
    path.push(text);
  }
  return '/' + path.join('/');
}

/** Only a direct default call or a top-level spread registers default flat routes. */
export function usesDefaultFlatRoutes(content: string): boolean {
  // Template-driven configuration is outside this literal registration reader.
  if (content.includes('`')) return false;
  const safe = stripCommentsForRegex(content, 'typescript');
  const masked = safe.replace(/"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'/g, (m) =>
    m.replace(/[^\r\n]/g, ' '),
  );
  const imports = /^[ \t]*import\s*\{([^}]+)\}\s*from\s*['"]@react-router\/fs-routes['"]/gm;
  const aliases: string[] = [];
  for (const match of safe.matchAll(imports)) {
    if (!/^\s*import\b/.test(masked.slice(match.index!, match.index! + match[0].indexOf('{'))))
      continue;
    for (const spec of match[1]!.split(',')) {
      const binding = /^\s*flatRoutes(?:\s+as\s+([A-Za-z_$][\w$]*))?\s*$/.exec(spec);
      if (binding) aliases.push(binding[1] ?? 'flatRoutes');
    }
  }
  const exported = /^[ \t]*export\s+default\s+/m.exec(masked);
  if (!exported) return false;
  const expression = masked
    .slice(exported.index + exported[0].length)
    .split(';', 1)[0]!
    .trim();
  for (const alias of aliases) {
    const escaped = alias.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    if (new RegExp(`^(?:await\\s+)?${escaped}\\s*\\(\\s*\\)\\s*;?\\s*$`).test(expression))
      return true;
    if (!expression.startsWith('[')) continue;
    const end = matchBracket(expression, 0);
    if (end < 0) continue;
    if (!/^\s*(?:satisfies\s+[A-Za-z_$][\w$]*)?\s*;?\s*$/.test(expression.slice(end + 1))) continue;
    let i = 1;
    while (i < end) {
      while (/[\s,]/.test(expression[i] ?? '')) i++;
      if (
        new RegExp(
          `^\\.\\.\\.\\s*\\(?\\s*(?:await\\s+)?${escaped}\\s*\\(\\s*\\)\\s*\\)?\\s*(?:,|\\])`,
        ).test(expression.slice(i))
      )
        return true;
      while (i < end && expression[i] !== ',') {
        if ('([{'.includes(expression[i]!)) {
          const close = matchBracket(expression, i);
          if (close < 0) return false;
          i = close + 1;
        } else i++;
      }
      i++;
    }
  }
  return false;
}

/** Root-level default conventions; custom roots and route overrides are not inferred. */
export const reactRouterFilesResolver: FrameworkResolver = {
  name: 'react-router-files',
  languages: [...ROUTE_LANGUAGES],
  detect(context) {
    for (const name of ['remix.config', 'react-router.config', 'vite.config']) {
      for (const extension of ['js', 'cjs', 'mjs', 'ts']) {
        const config = context.readFile(`${name}.${extension}`);
        if (config) {
          const safe = stripCommentsForRegex(config, 'typescript');
          if (
            /\b(?:appDirectory|rootDirectory|ignoredRouteFiles|v3_routeConfig|routes)['"]?\s*:|\.\.\./.test(
              safe,
            )
          )
            return false;
          if (
            !/(?:export\s+default\s+(?:defineConfig\s*\(\s*)?\{|module\.exports\s*=\s*\{)/.test(
              safe,
            )
          )
            return false;
          if (
            [...safe.matchAll(/\b(?:remix|reactRouter)\s*\(([^)]*)\)/g)].some((m) => m[1]!.trim())
          )
            return false;
        }
      }
    }
    const config = context.readFile('app/routes.ts') ?? context.readFile('app/routes.js');
    if (config !== null) return usesDefaultFlatRoutes(config);
    try {
      const pkg = JSON.parse(context.readFile('package.json') ?? '{}');
      const deps = { ...pkg.dependencies, ...pkg.devDependencies };
      return Boolean(deps['@remix-run/dev'] || deps['@remix-run/react']);
    } catch {
      return false;
    }
  },
  resolve: () => null,
  extract(filePath, content) {
    const routePath = remixFileRoutePath(filePath);
    if (routePath === null) return { nodes: [], references: [] };
    const language = detectLanguage(filePath)!;
    const parser = getParser(language);
    if (!parser) throw new Error(`File-route extraction requires the ${language} grammar`);
    const tree = parser.parse(content);
    if (!tree) return { nodes: [], references: [] };
    try {
      const exported = tree.rootNode.namedChildren.find(
        (n) => n.type === 'export_statement' && n.children.some((c) => c.type === 'default'),
      );
      if (!exported) return { nodes: [], references: [] };
      let declaration =
        exported.childForFieldName('declaration') ?? exported.childForFieldName('value');
      if (declaration?.type === 'identifier') {
        const name = declaration.text;
        declaration =
          tree.rootNode.namedChildren
            .map((n) =>
              n.type === 'export_statement' ? (n.childForFieldName('declaration') ?? n) : n,
            )
            .flatMap((n) => (n.type === 'lexical_declaration' ? n.namedChildren : [n]))
            .find((n) => n.childForFieldName('name')?.text === name) ?? null;
      }
      const component =
        declaration?.type === 'variable_declarator'
          ? declaration.childForFieldName('value')
          : declaration;
      const body = component?.childForFieldName('body');
      const returned =
        body?.type === 'statement_block'
          ? body.namedChildren
              .filter((n) => n.type === 'return_statement')
              .map((n) => n.namedChildren[0])
          : [body];
      if (
        returned.length > 0 &&
        returned.every((n) => {
          while (n?.type === 'parenthesized_expression') n = n.namedChildren[0];
          return (
            n?.type === 'jsx_self_closing_element' && n.childForFieldName('name')?.text === 'Outlet'
          );
        })
      )
        return { nodes: [], references: [] };
      const node: Node = {
        id: `route:react-router:${filePath}:file:${routePath}`,
        kind: 'route',
        name: routePath,
        qualifiedName: `${filePath}::${routePath}`,
        filePath,
        language,
        startLine: exported.startPosition.row + 1,
        endLine: exported.endPosition.row + 1,
        startColumn: exported.startPosition.column,
        endColumn: exported.endPosition.column,
        updatedAt: Date.now(),
      };
      const module = filePath.replace(/\\/g, '/').split('/').pop()!;
      return {
        nodes: [node],
        references: [
          {
            fromNodeId: node.id,
            referenceName: `react-router-module:${module}`,
            referenceKind: 'references',
            filePath,
            language,
            line: node.startLine,
            column: node.startColumn,
          },
        ],
      };
    } finally {
      tree.delete();
    }
  },
};

// =============================================================================
// Route table — the routes `frameworks/react.ts` read out of the markup
// =============================================================================

export type ReactRouterTable = RootedRouteTable;

/** The app a route file belongs to — the shared rule (`appRootFor`). */
export const reactRouterRoot = appRootFor;

/**
 * True for a route node `frameworks/react.ts` emitted, and no other.
 *
 * Its id is a verbatim reconstruction of the node's own fields, which no
 * other framework's route id is: a server route carries its METHOD
 * (`route:file:12:POST:/login`), a file-based page carries no line.
 */
function isReactRouterRoute(node: Node): boolean {
  return (
    node.id.startsWith(`route:react-router:${node.filePath}:`) ||
    ((node.language === 'tsx' || node.language === 'jsx') &&
      node.id === `route:${node.filePath}:${node.startLine}:${node.name}`)
  );
}

/** Expand bounded optional segments, including Remix's optional language prefix. */
function optionalRoutePaths(path: string): string[] {
  const segments = path.split('/').slice(1);
  if (segments.filter((s) => s.endsWith('?')).length > 4) return [];
  let variants = [''];
  for (const segment of segments) {
    variants = segment.endsWith('?')
      ? variants.flatMap((p) => [p + '/' + segment.slice(0, -1), p])
      : variants.map((p) => p + '/' + segment);
  }
  const unique = new Map<string, string>();
  for (const variant of variants) unique.set(variant.replace(/:[^/]+/g, ':'), variant || '/');
  return [...unique.values()];
}

const tables = new WeakMap<ResolutionContext, ReactRouterTable>();

export function reactRouterTable(context: ResolutionContext): ReactRouterTable {
  const all = context.getNodesByKind('route');
  const cached = tables.get(context);
  if (cached && cached.source === all) return cached;
  const byRoot = new Map<string, RouteTable>();
  const shortened: { root: string; path: string; node: Node }[] = [];
  const tableAt = (root: string): RouteTable => {
    let t = byRoot.get(root);
    if (!t) byRoot.set(root, (t = { source: all, exact: new Map(), dynamic: [] }));
    return t;
  };
  for (const node of all) {
    if (!isReactRouterRoute(node)) continue;
    // A nested route's path is relative to its parent; without the tree it is
    // not a destination. A splat matches everything, so it answers nothing.
    if (!node.name.startsWith('/') || node.name.endsWith('*')) continue;
    const root = reactRouterRoot(node.filePath);
    const path =
      node.name.length > 1 && node.name.endsWith('/') ? node.name.slice(0, -1) : node.name;
    tableAt(root);
    if (!path.includes('?')) addRouteTo(tableAt(root), path, node);
    else
      for (const variant of optionalRoutePaths(path)) shortened.push({ root, path: variant, node });
  }
  for (const s of shortened) {
    const t = byRoot.get(s.root);
    if (t && !t.exact.has(s.path)) addRouteTo(t, s.path, s.node);
  }
  const table: ReactRouterTable = { source: all, byRoot };
  tables.set(context, table);
  return table;
}

// =============================================================================
// Navigation calls
// =============================================================================

/**
 * `history.push` / `.replace` (v5, `useHistory`), `navigate(…)` (v6,
 * `useNavigate`), `router.navigate(…)` (a data router), `redirect(…)` (a
 * loader or an action).
 *
 * The receiver is required for `push` / `replace`: an unqualified `push` is
 * an array's, and claiming it would put every `paths.push('/tmp/x')` in the
 * repo one string-match away from a route.
 */
const NAV_CALL = /^(?:history|navigate|router)\.(?:push|replace|navigate)$|^(?:navigate|redirect)$/;

/** The verb a navigation call name stands for, or null. */
export function reactRouterNavVerb(name: string): string | null {
  if (!NAV_CALL.test(name)) return null;
  const dot = name.lastIndexOf('.');
  return dot < 0 ? name : name.slice(dot + 1);
}

// =============================================================================
// The resolver
// =============================================================================

export const reactRouterResolver: FrameworkResolver = {
  name: 'react-router',
  languages: [...ROUTE_LANGUAGES],

  detect(context: ResolutionContext): boolean {
    return dependsOn(
      context,
      'react-router',
      'react-router-dom',
      'react-router-native',
      '@react-router/dev',
      '@remix-run/react',
      '@remix-run/dev',
    );
  },

  claimsReference(name: string): boolean {
    return NAV_CALL.test(name) || name.startsWith('react-router-module:');
  },

  extract: extractReactRouterConfig,

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    if (ref.referenceName.startsWith('react-router-module:')) {
      if (ref.referenceKind !== 'references' || !ref.fromNodeId.startsWith('route:react-router:'))
        return null;
      const module = ref.referenceName.slice('react-router-module:'.length);
      const targetPath = resolveImportPath('./' + module, ref.filePath, ref.language, context);
      if (!targetPath) return null;
      const source = context.readFile(targetPath);
      const parser = getParser(detectLanguage(targetPath)!);
      const tree = source === null ? null : parser?.parse(source);
      if (!tree) return null;
      let target: Node | undefined;
      try {
        const exported = tree.rootNode.namedChildren.find(
          (n) => n.type === 'export_statement' && n.children.some((c) => c.type === 'default'),
        );
        let declaration =
          exported?.childForFieldName('declaration') ?? exported?.childForFieldName('value');
        if (declaration?.type === 'identifier') {
          const name = declaration.text;
          declaration = tree.rootNode.namedChildren
            .map((n) =>
              n.type === 'export_statement' ? (n.childForFieldName('declaration') ?? n) : n,
            )
            .flatMap((n) => (n.type === 'lexical_declaration' ? n.namedChildren : [n]))
            .find((n) => n.childForFieldName('name')?.text === name);
        }
        const name = declaration?.childForFieldName('name')?.text;
        if (name && declaration) {
          const line = declaration.startPosition.row + 1;
          target = context
            .getNodesInFile(targetPath)
            .find(
              (n) =>
                n.name === name &&
                n.startLine <= line &&
                n.endLine >= line &&
                ['function', 'class', 'component', 'constant', 'variable'].includes(n.kind),
            );
        }
      } finally {
        tree.delete();
      }
      return target
        ? { original: ref, targetNodeId: target.id, confidence: 0.95, resolvedBy: 'framework' }
        : null;
    }
    if (ref.referenceKind !== 'calls') return null;
    const verb = reactRouterNavVerb(ref.referenceName);
    if (!verb) return null;
    if (!ROUTE_LANGUAGES.includes(ref.language)) return null;
    const routes = routesForFile(reactRouterTable(context), ref.filePath);
    if (!routes || routes.exact.size === 0) return null;
    const lines =
      context.getFileLines?.(ref.filePath) ??
      context.readFile(ref.filePath)?.split(/\r?\n/) ??
      null;
    if (!lines) return null;

    const arg = firstArgumentText(lines, ref.line, ref.column, verb);
    if (arg === null) return null;
    let href = parseHrefExpression(arg);
    if (!href) {
      const enclosing = context.getNodeById?.(ref.fromNodeId);
      const start =
        enclosing && enclosing.filePath === ref.filePath
          ? enclosing.startLine
          : Math.max(1, ref.line - 40);
      href = readHrefViaLocal(lines, ref.line, ref.column, verb, start);
    }
    if (!href) return null;
    // Every arm of a conditional destination is somewhere this call goes; the
    // first is this reference's resolution and the rest ride as `alsoTargets`.
    const targets = destinationsForHref(href, routes);
    const target = targets[0];
    if (!target) return null;
    return {
      original: ref,
      targetNodeId: target.node.id,
      ...(targets.length > 1
        ? {
            alsoTargets: targets.slice(1).map((t) => ({
              targetNodeId: t.node.id,
              metadata: { href: t.href.display, navMethod: verb },
            })),
          }
        : {}),
      confidence: 0.95,
      resolvedBy: 'framework',
      edgeKind: 'navigates',
      metadata: { href: target.href.display, navMethod: verb },
    };
  },
};

/** Framework-mode helpers are evaluated only inside the exported literal route tree. */
export function extractReactRouterConfig(filePath: string, content: string) {
  const nodes: Node[] = [];
  const references: UnresolvedRef[] = [];
  if (
    !/(?:^|\/)app\/routes\.[jt]s$/.test(filePath.replace(/\\/g, '/')) ||
    !content.includes('@react-router/dev/routes')
  )
    return { nodes, references };
  const language = detectLanguage(filePath)!;
  const parser = getParser(language);
  if (!parser) throw new Error(`React Router extraction requires the ${language} grammar`);
  const tree = parser.parse(content);
  if (!tree) return { nodes, references };
  const unwrap = (node: SyntaxNode | null): SyntaxNode | null => {
    while (
      node &&
      ['satisfies_expression', 'as_expression', 'parenthesized_expression'].includes(node.type)
    )
      node = node.namedChildren[0] ?? null;
    return node;
  };
  const literal = (node: SyntaxNode | null): string | null => {
    node = unwrap(node);
    return node?.type === 'string' && !node.text.includes('\\') ? node.text.slice(1, -1) : null;
  };
  try {
    const helpers = new Map<string, string>();
    for (const statement of tree.rootNode.namedChildren) {
      if (
        statement.type !== 'import_statement' ||
        literal(statement.childForFieldName('source')) !== '@react-router/dev/routes'
      )
        continue;
      for (const spec of statement.descendantsOfType('import_specifier')) {
        if (spec.text.startsWith('type ')) continue;
        const name = spec.childForFieldName('name')?.text;
        const alias = spec.childForFieldName('alias')?.text ?? name;
        if (name && alias && ['route', 'index', 'layout', 'prefix'].includes(name))
          helpers.set(alias, name);
      }
    }
    const visit = (value: SyntaxNode | null, parent: string): void => {
      value = unwrap(value);
      if (value?.type !== 'array') return;
      for (let item of value.namedChildren) {
        if (item.type === 'comment') continue;
        const spread = item.type === 'spread_element';
        item = (spread ? item.namedChildren[0] : item)!;
        if (item?.type !== 'call_expression') continue;
        const fn = item.childForFieldName('function');
        const helper = fn?.type === 'identifier' ? helpers.get(fn.text) : undefined;
        if (!helper || spread !== (helper === 'prefix')) continue;
        const args =
          item.childForFieldName('arguments')?.namedChildren.filter((n) => n.type !== 'comment') ??
          [];
        const segment = helper === 'route' || helper === 'prefix' ? literal(args[0] ?? null) : '';
        if (segment === null) continue;
        const routePath = (parent + '/' + segment).replace(/\/+/g, '/').replace(/\/$/, '') || '/';
        if (helper === 'prefix') {
          visit(args[1] ?? null, routePath);
          continue;
        }
        const module = literal(args[helper === 'route' ? 1 : 0] ?? null);
        if (module === null) continue;
        const tail = args.slice(helper === 'route' ? 2 : 1);
        // Options may carry an id, but spread/computed options are not statically known.
        if (
          tail.some(
            (n) =>
              n.type !== 'array' &&
              (n.type !== 'object' ||
                n.namedChildren.some(
                  (p) =>
                    p.type !== 'pair' ||
                    p.childForFieldName('key')?.type === 'computed_property_name',
                )),
          )
        )
          continue;
        const children = tail.find((n) => n.type === 'array');
        const beforeChildren = nodes.length;
        if (children && helper !== 'index') visit(children, routePath);
        if (helper !== 'layout' && !nodes.slice(beforeChildren).some((n) => n.name === routePath)) {
          const line = item.startPosition.row + 1;
          const node: Node = {
            id: `route:react-router:${filePath}:${item.startIndex}:${routePath}`,
            kind: 'route',
            name: routePath,
            qualifiedName: `${filePath}::${routePath}`,
            filePath,
            language,
            startLine: line,
            endLine: item.endPosition.row + 1,
            startColumn: item.startPosition.column,
            endColumn: item.endPosition.column,
            updatedAt: Date.now(),
          };
          nodes.push(node);
          references.push({
            fromNodeId: node.id,
            referenceName: `react-router-module:${module}`,
            referenceKind: 'references',
            filePath,
            language,
            line,
            column: item.startPosition.column,
          });
        }
      }
    };
    for (const statement of tree.rootNode.namedChildren) {
      if (
        statement.type === 'export_statement' &&
        statement.children.some((n) => n.type === 'default')
      )
        visit(statement.childForFieldName('value'), '');
    }
    return { nodes, references };
  } finally {
    tree.delete();
  }
}
