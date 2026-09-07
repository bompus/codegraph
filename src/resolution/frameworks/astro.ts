/**
 * Astro Framework Resolver
 *
 * Handles Astro component references, the `Astro` global, `astro:*` virtual
 * module imports, and Astro's `src/pages/` file-based routing.
 */

import { Node } from '../../types';
import { FrameworkResolver, UnresolvedRef, ResolvedRef, ResolutionContext } from '../types';
import { getParser, detectLanguage } from '../../extraction/grammars';
import { generateNodeId } from '../../extraction/tree-sitter-helpers';
import { httpHandlerReferences } from './http-routing';
import {
  addRouteTo,
  appRootFor,
  hrefArms,
  nthArgumentText,
  parseHrefExpression,
  routesForFile,
  type RootedRouteTable,
  type RouteTable,
} from './expo-router';
import { destinationsForHref } from './nextjs';

const tables = new WeakMap<ResolutionContext, RootedRouteTable>();
function astroTable(context: ResolutionContext): RootedRouteTable {
  const source = context.getNodesByKind('route');
  const cached = tables.get(context);
  if (cached?.source === source) return cached;
  const byRoot = new Map<string, RouteTable>();
  for (const node of source) {
    if (
      node.language !== 'astro' ||
      node.id !== `route:${node.filePath}:${node.name}:1` ||
      node.name.includes('*')
    )
      continue;
    const root = appRootFor(node.filePath);
    let table = byRoot.get(root);
    if (!table) byRoot.set(root, (table = { source, exact: new Map(), dynamic: [] }));
    addRouteTo(table, node.name, node);
  }
  const table = { source, byRoot };
  tables.set(context, table);
  return table;
}

function pageComponentId(filePath: string): string {
  return generateNodeId(
    filePath,
    'component',
    filePath
      .split(/[/\\]/)
      .pop()!
      .replace(/\.astro$/, ''),
    1,
  );
}

/**
 * Astro virtual module prefixes — framework-provided, not user code
 */
const ASTRO_VIRTUAL_MODULES = [
  'astro:content',
  'astro:assets',
  'astro:actions',
  'astro:env',
  'astro:i18n',
  'astro:middleware',
  'astro:transitions',
  'astro:components',
  'astro:schema',
];

export const astroResolver: FrameworkResolver = {
  name: 'astro',
  claimsReference: (name) =>
    name === 'astro-page-component' || name.startsWith('astro-href:') || name === 'Astro.redirect',

  detect(context: ResolutionContext): boolean {
    // Check for astro in package.json
    const packageJson = context.readFile('package.json');
    if (packageJson) {
      try {
        const pkg = JSON.parse(packageJson);
        const deps = { ...pkg.dependencies, ...pkg.devDependencies };
        if (deps.astro) {
          return true;
        }
      } catch {
        // Invalid JSON
      }
    }

    // Check for .astro files in project
    const allFiles = context.getAllFiles();
    return allFiles.some((f) => f.endsWith('.astro'));
  },

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    if (ref.referenceName === 'astro-page-component') {
      const target = context.getNodeById?.(pageComponentId(ref.filePath));
      return target
        ? { original: ref, targetNodeId: target.id, confidence: 1, resolvedBy: 'framework' }
        : null;
    }
    const link = ref.referenceName.startsWith('astro-href:');
    const redirect =
      ref.referenceName === 'Astro.redirect' &&
      ref.referenceKind === 'calls' &&
      ref.filePath.endsWith('.astro');
    if (link || redirect) {
      const routes = routesForFile(astroTable(context), ref.filePath);
      if (!routes) return null;
      const lines =
        context.getFileLines?.(ref.filePath) ?? context.readFile(ref.filePath)?.split(/\r?\n/);
      const expression = link
        ? ref.referenceName.slice('astro-href:'.length)
        : lines
          ? nthArgumentText(lines, ref.line, ref.column, ref.referenceName, 0)
          : null;
      const href = expression === null ? null : parseHrefExpression(expression);
      if (!href) return null;
      const targets = hrefArms(href)
        .filter((arm) => /^\/(?!\/)/.test(arm.path))
        .flatMap((arm) => destinationsForHref({ ...arm, alternates: undefined }, routes));
      if (!targets.length) return null;
      return {
        original: ref,
        targetNodeId: targets[0]!.node.id,
        confidence: 0.95,
        resolvedBy: 'framework',
        edgeKind: 'navigates',
        metadata: { href: targets[0]!.href.display, navMethod: link ? 'a' : 'Astro.redirect' },
        ...(targets.length > 1
          ? {
              alsoTargets: targets
                .slice(1)
                .map((t) => ({ targetNodeId: t.node.id, metadata: { href: t.href.display } })),
            }
          : {}),
      };
    }
    // Pattern 1: the `Astro` global (Astro.props, Astro.url, Astro.params, …)
    // — runtime-provided in every component's frontmatter. Resolving it as
    // framework-provided keeps it from name-matching a user symbol named Astro.
    if (ref.referenceName === 'Astro' || ref.referenceName.startsWith('Astro.')) {
      return {
        original: ref,
        targetNodeId: ref.fromNodeId,
        confidence: 1.0,
        resolvedBy: 'framework',
      };
    }

    // Pattern 2: astro:* virtual module imports (astro:content, astro:assets, …)
    if (ref.referenceKind === 'imports' && ref.referenceName.startsWith('astro:')) {
      if (ASTRO_VIRTUAL_MODULES.some((prefix) => ref.referenceName.startsWith(prefix))) {
        return {
          original: ref,
          targetNodeId: ref.fromNodeId,
          confidence: 1.0,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 3: Component references (PascalCase) — resolve to component
    // nodes. Template tags arrive as `references`, frontmatter expression
    // usages as `calls`.
    if (
      isPascalCase(ref.referenceName) &&
      (ref.referenceKind === 'references' || ref.referenceKind === 'calls')
    ) {
      const result = resolveComponent(ref.referenceName, ref.filePath, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    return null;
  },

  extract(filePath: string, content: string) {
    const nodes: Node[] = [];
    const references: UnresolvedRef[] = [];
    const now = Date.now();

    // Normalize to forward slashes
    const normalized = filePath.replace(/\\/g, '/');

    if (normalized.endsWith('.astro') && content.includes('href')) {
      // Keep offsets while omitting non-markup regions and commented examples.
      const markup = content
        .replace(/^---\s*\r?\n[\s\S]*?^---\s*$/m, (s) => s.replace(/[^\r\n]/g, ' '))
        .replace(
          /<!--[\s\S]*?-->|\{\/\*[\s\S]*?\*\/\}|<(script|style)\b[^>]*>[\s\S]*?<\/\1\s*>/gi,
          (s) => s.replace(/[^\r\n]/g, ' '),
        );
      const parser = getParser('tsx');
      if (!parser) throw new Error('Astro anchor extraction requires the tsx grammar');
      const tree = parser.parse(`<>${markup}</>`);
      if (tree)
        try {
          for (const tag of tree.rootNode.descendantsOfType([
            'jsx_opening_element',
            'jsx_self_closing_element',
          ])) {
            if (
              tag.childForFieldName('name')?.text !== 'a' ||
              tag.namedChildren.some(
                (n) => n.type === 'jsx_expression' && /^\{\s*\.\.\./.test(n.text),
              )
            )
              continue;
            const attributes = tag.namedChildren.filter(
              (n) => n.type === 'jsx_attribute' && n.namedChildren[0]?.text === 'href',
            );
            if (attributes.length !== 1) continue;
            const raw = attributes[0]!.namedChildren[1]?.text;
            if (!raw) continue;
            const expression = raw.startsWith('{') ? raw.slice(1, -1).trim() : raw;
            if (!parseHrefExpression(expression)) continue;
            references.push({
              fromNodeId: pageComponentId(filePath),
              referenceName: `astro-href:${expression}`,
              referenceKind: 'references',
              filePath,
              language: 'astro',
              line: tag.startPosition.row + 1,
              column: 0,
            });
          }
        } finally {
          tree.delete();
        }
    }

    // Markdown/MDX and custom roots are outside this default convention.
    const pagesMatch = /(?:^|\/)src\/pages\//.exec(normalized);
    if (pagesMatch && /\.(astro|ts|js)$/.test(normalized)) {
      const afterPages = normalized.substring(pagesMatch.index + pagesMatch[0].length);
      const base = afterPages.split('/').pop() || '';

      // Underscore-prefixed segments are excluded from routing by Astro;
      // a stray `*.config.*` in a pages dir is never a route.
      if (
        !afterPages.split('/').some((segment) => segment.startsWith('_')) &&
        !/\.config\.[a-z]+$/.test(base)
      ) {
        const routePath = filePathToAstroRoute(afterPages);

        const node: Node = {
          id: `route:${filePath}:${routePath}:1`,
          kind: 'route',
          name: routePath,
          qualifiedName: `${filePath}::route:${routePath}`,
          filePath,
          startLine: 1,
          endLine: 1,
          startColumn: 0,
          endColumn: 0,
          language: normalized.endsWith('.astro') ? 'astro' : detectLanguage(filePath)!,
          updatedAt: now,
        };
        if (node.language === 'astro') {
          nodes.push(node);
          references.push({
            fromNodeId: node.id,
            referenceName: 'astro-page-component',
            referenceKind: 'references',
            filePath,
            language: 'astro',
            line: 1,
            column: 0,
          });
        } else if (content.includes('export')) {
          const parser = getParser(node.language);
          if (!parser)
            throw new Error(`Astro endpoint extraction requires the ${node.language} grammar`);
          const tree = parser.parse(content);
          if (!tree) return { nodes, references };
          try {
            const methods = /^(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|ALL)$/;
            for (const statement of tree.rootNode.namedChildren) {
              if (
                statement.type !== 'export_statement' ||
                statement.children.some(
                  (n) => n.type === 'default' || n.type === 'type' || n.type === 'declare',
                ) ||
                statement.childForFieldName('source')
              )
                continue;
              const declaration = statement.childForFieldName('declaration');
              const entries =
                declaration?.type === 'lexical_declaration'
                  ? declaration.namedChildren
                  : declaration
                    ? [declaration]
                    : (statement.namedChildren.find((n) => n.type === 'export_clause')
                        ?.namedChildren ?? []);
              for (const entry of entries) {
                if (
                  !['function_declaration', 'variable_declarator', 'export_specifier'].includes(
                    entry.type,
                  ) ||
                  entry.children.some((n) => n.type === 'type')
                )
                  continue;
                const name = entry.childForFieldName('name');
                const method = entry.childForFieldName('alias')?.text ?? name?.text;
                if (!method || !methods.test(method)) continue;
                const value = entry.childForFieldName('value');
                if (
                  value &&
                  !['identifier', 'arrow_function', 'function_expression'].includes(value.type)
                )
                  continue;
                const endpoint: Node = {
                  ...node,
                  id: `route:${filePath}:${method}:${routePath}`,
                  name: `${method === 'ALL' ? 'ANY' : method} ${routePath}`,
                  qualifiedName: `${filePath}::${method}:${routePath}`,
                  startLine: entry.startPosition.row + 1,
                  endLine: entry.endPosition.row + 1,
                };
                nodes.push(endpoint);
                references.push(
                  ...httpHandlerReferences(endpoint, value?.type === 'identifier' ? value : name),
                );
              }
            }
          } finally {
            tree.delete();
          }
        }
      }
    }

    return { nodes, references };
  },
};

/**
 * Check if string is PascalCase
 */
function isPascalCase(str: string): boolean {
  return /^[A-Z][a-zA-Z0-9]*$/.test(str);
}

/**
 * Resolve an Astro component reference using name-based lookup
 */
function resolveComponent(
  name: string,
  fromFile: string,
  context: ResolutionContext,
): string | null {
  // Look for component nodes by name
  const candidates = context.getNodesByName(name);
  const components = candidates.filter((n) => n.kind === 'component');

  if (components.length === 0) return null;

  // Prefer same directory
  const fromDir = fromFile.substring(0, fromFile.lastIndexOf('/'));
  const sameDir = components.filter((n) => n.filePath.startsWith(fromDir));
  if (sameDir.length > 0) return sameDir[0]!.id;

  // No positional signal: only an UNAMBIGUOUS name may resolve — picking
  // components[0] would choose an arbitrary same-named component in a
  // multi-app monorepo (#764). Ambiguity falls through to the name-matcher,
  // whose proximity scoring decides.
  return components.length === 1 ? components[0]!.id : null;
}

/**
 * Convert a path under src/pages/ to an Astro route path.
 *
 * blog/[slug].astro        -> /blog/:slug
 * blog/[...path].astro     -> /blog/*path
 * api/posts.ts             -> /api/posts
 * index.astro              -> /
 */
function filePathToAstroRoute(afterPages: string): string {
  // Remove the extension
  const withoutExt = afterPages.replace(/\.(astro|ts|js|mjs)$/, '');

  // index files map to their parent path (index -> /, blog/index -> /blog)
  const withoutIndex = withoutExt.replace(/(^|\/)index$/, '$1').replace(/\/$/, '');

  // Convert Astro param syntax
  const route =
    '/' +
    withoutIndex
      .replace(/\[\.\.\.([^\]]+)\]/g, '*$1') // [...rest] -> *rest (catch-all)
      .replace(/\[([^\]]+)\]/g, ':$1'); // [param] -> :param

  if (route === '/') return '/';
  // Remove trailing slash
  return route.replace(/\/$/, '');
}
