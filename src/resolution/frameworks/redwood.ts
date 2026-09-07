import type { Node as SyntaxNode } from 'web-tree-sitter';
import type { Node } from '../../types';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { resolveViaImport } from '../import-resolver';
import { dependsOn } from './package-deps';
import { httpHandlerReferences } from './http-routing';

const FUNCTIONS = new Set([
  'arrow_function',
  'function_expression',
  'function_declaration',
  'method_definition',
]);
const METHODS = new Set(['get', 'post', 'put', 'patch', 'delete', 'head']);
const unwrap = (raw: SyntaxNode | null | undefined): SyntaxNode | null => {
  let node = raw ?? null;
  while (
    node &&
    ['parenthesized_expression', 'as_expression', 'satisfies_expression'].includes(node.type)
  )
    node = node.namedChildren[0] ?? null;
  return node;
};
const literal = (node: SyntaxNode | null | undefined): string | null =>
  node?.type === 'string' && !node.text.includes('\\') ? node.text.slice(1, -1) : null;
function returnedJsx(handler: SyntaxNode): SyntaxNode[] {
  const found: SyntaxNode[] = [];
  const expression = (node: SyntaxNode): void => {
    if (FUNCTIONS.has(node.type)) return;
    if (['jsx_element', 'jsx_self_closing_element', 'jsx_fragment'].includes(node.type))
      found.push(node);
    else for (const child of node.namedChildren) expression(child);
  };
  const statements = (node: SyntaxNode): void => {
    if (FUNCTIONS.has(node.type)) return;
    if (node.type === 'return_statement') {
      for (const child of node.namedChildren) expression(child);
    } else for (const child of node.namedChildren) statements(child);
  };
  const body = handler.childForFieldName('body');
  if (body?.type === 'statement_block') statements(body);
  else if (body) expression(body);
  return found;
}

/** Only a registered defineApp tree contributes routes; middleware bodies are opaque. */
export function extractRedwoodRoutes(filePath: string, content: string): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!content.includes('rwsdk/')) return result;
  const language = detectLanguage(filePath)!;
  if (!['typescript', 'javascript', 'tsx', 'jsx'].includes(language)) return result;
  const parser = getParser(language);
  if (!parser) throw new Error(`RedwoodSDK extraction requires the ${language} grammar`);
  const tree = parser.parse(content);
  if (!tree) return result;
  try {
    const helpers = new Map<string, string>();
    const bindings = new Map<string, SyntaxNode>();
    for (const statement of tree.rootNode.namedChildren) {
      if (
        statement.type === 'import_statement' &&
        !statement.children.some((n) => n.type === 'type')
      ) {
        const source = literal(statement.childForFieldName('source'));
        for (const spec of statement.descendantsOfType('import_specifier')) {
          if (spec.children.some((n) => n.type === 'type')) continue;
          const name = spec.childForFieldName('name')?.text;
          const alias = spec.childForFieldName('alias')?.text ?? name;
          if (
            name &&
            alias &&
            ((source === 'rwsdk/worker' && name === 'defineApp') ||
              (source === 'rwsdk/router' &&
                ['route', 'index', 'render', 'layout', 'prefix'].includes(name)))
          )
            helpers.set(alias, name);
        }
      }
      const declaration =
        statement.type === 'export_statement'
          ? statement.childForFieldName('declaration')
          : statement;
      if (
        declaration?.type === 'lexical_declaration' &&
        declaration.children.some((n) => n.type === 'const')
      ) {
        for (const variable of declaration.namedChildren) {
          const name = variable.childForFieldName('name');
          const value = variable.childForFieldName('value');
          if (name?.type === 'identifier' && value) bindings.set(name.text, value);
        }
      }
    }
    const resolve = (
      raw: SyntaxNode | null | undefined,
      seen = new Set<string>(),
    ): SyntaxNode | null => {
      const node = unwrap(raw);
      if (node?.type !== 'identifier' || !bindings.has(node.text)) return node;
      if (seen.has(node.text)) return null;
      seen.add(node.text);
      return resolve(bindings.get(node.text), seen);
    };
    const emit = (
      site: SyntaxNode,
      routePath: string,
      method: string,
      raw: SyntaxNode | null,
    ): void => {
      let handler = unwrap(raw);
      if (handler?.type === 'array') {
        if (handler.namedChildren.some((n) => n.type === 'spread_element')) return;
        handler = unwrap(handler.namedChildren.filter((n) => n.type !== 'comment').at(-1));
      }
      if (
        !handler ||
        (!FUNCTIONS.has(handler.type) &&
          !['identifier', 'shorthand_property_identifier'].includes(handler.type))
      )
        return;
      const jsx = method === 'ANY' && FUNCTIONS.has(handler.type) ? returnedJsx(handler) : [];
      const page = jsx.length > 0;
      const node: Node = {
        id: `route:redwood:${filePath}:${site.startIndex}:${method}:${routePath}`,
        kind: 'route',
        name: page ? routePath : `${method} ${routePath}`,
        qualifiedName: `${filePath}::${method} ${routePath}`,
        filePath,
        language,
        startLine: site.startPosition.row + 1,
        endLine: site.endPosition.row + 1,
        startColumn: site.startPosition.column,
        endColumn: site.endPosition.column,
        updatedAt: Date.now(),
      };
      if (result.nodes.some((n) => n.id === node.id)) return;
      result.nodes.push(node);
      result.references.push(...httpHandlerReferences(node, handler));
      if (page)
        for (const tag of jsx.flatMap((n) =>
          n.descendantsOfType(['jsx_opening_element', 'jsx_self_closing_element']),
        )) {
          const name = tag.childForFieldName('name');
          if (name && /^[A-Z][\w$]*$/.test(name.text))
            result.references.push(...httpHandlerReferences(node, name));
        }
    };
    const visit = (raw: SyntaxNode | null | undefined, prefix: string, depth = 0): void => {
      if (depth > 32) return;
      const node = resolve(raw);
      if (!node) return;
      if (node.type === 'array') {
        for (const child of node.namedChildren)
          visit(
            child.type === 'spread_element' ? child.namedChildren[0] : child,
            prefix,
            depth + 1,
          );
        return;
      }
      if (node.type !== 'call_expression') return;
      const fn = node.childForFieldName('function');
      const helper = fn?.type === 'identifier' ? helpers.get(fn.text) : undefined;
      const args =
        node.childForFieldName('arguments')?.namedChildren.filter((n) => n.type !== 'comment') ??
        [];
      if (helper === 'render' || helper === 'layout') {
        visit(args[1], prefix, depth + 1);
        return;
      }
      if (helper === 'prefix') {
        const part = literal(args[0]);
        if (part !== null) visit(args[1], prefix + '/' + part.replace(/^\/+|\/+$/g, ''), depth + 1);
        return;
      }
      if (helper !== 'route' && helper !== 'index') return;
      const part = helper === 'index' ? '/' : literal(args[0]);
      if (part === null) return;
      const routePath = (prefix + '/' + part.replace(/^\/+|\/+$/g, '')).replace(/\/$/, '') || '/';
      const rawHandler = unwrap(args[helper === 'index' ? 0 : 1]);
      const resolvedHandler = resolve(rawHandler);
      const handler =
        resolvedHandler && ['object', 'array'].includes(resolvedHandler.type)
          ? resolvedHandler
          : rawHandler;
      if (handler?.type !== 'object') {
        emit(node, routePath, 'ANY', handler);
        return;
      }
      if (
        handler.namedChildren.some(
          (n) =>
            n.type === 'spread_element' ||
            (n.childForFieldName('key') ?? n.childForFieldName('name'))?.type ===
              'computed_property_name' ||
            (n.type === 'method_definition' &&
              n.children.some((c) => c.type === 'get' || c.type === 'set')),
        )
      )
        return;
      const keys = handler.namedChildren
        .map((n) => {
          const key =
            n.childForFieldName('key') ??
            n.childForFieldName('name') ??
            (n.type === 'shorthand_property_identifier' ? n : null);
          return literal(key) ?? key?.text;
        })
        .filter((key) => key !== undefined);
      if (new Set(keys).size !== keys.length) return;
      for (const pair of handler.namedChildren) {
        const key =
          pair.childForFieldName('key') ??
          pair.childForFieldName('name') ??
          (pair.type === 'shorthand_property_identifier' ? pair : null);
        const method = literal(key) ?? key?.text;
        if (method && METHODS.has(method))
          emit(
            node,
            routePath,
            method.toUpperCase(),
            pair.type === 'method_definition' || pair.type === 'shorthand_property_identifier'
              ? pair
              : pair.childForFieldName('value'),
          );
      }
    };
    const register = (raw: SyntaxNode | null | undefined): void => {
      let node = resolve(raw);
      if (node?.type === 'object') {
        if (
          node.namedChildren.some(
            (n) =>
              n.type === 'spread_element' ||
              n.childForFieldName('key')?.type === 'computed_property_name',
          )
        )
          return;
        const fields = node.namedChildren.filter(
          (n) =>
            (literal(n.childForFieldName('key')) ?? n.childForFieldName('key')?.text) === 'fetch',
        );
        if (fields.length !== 1) return;
        const fetch = fields[0]!.childForFieldName('value');
        if (
          fetch?.type === 'member_expression' &&
          fetch.childForFieldName('property')?.text === 'fetch'
        )
          node = resolve(fetch.childForFieldName('object'));
      }
      const fn = node?.childForFieldName('function');
      if (
        node?.type === 'call_expression' &&
        fn?.type === 'identifier' &&
        helpers.get(fn.text) === 'defineApp'
      )
        visit(node.childForFieldName('arguments')?.namedChildren[0], '');
    };
    for (const statement of tree.rootNode.namedChildren) {
      if (statement.type !== 'export_statement') continue;
      if (statement.children.some((n) => n.type === 'default'))
        register(statement.childForFieldName('value'));
      const declaration = statement.childForFieldName('declaration');
      if (declaration?.type === 'lexical_declaration')
        for (const variable of declaration.namedChildren)
          register(variable.childForFieldName('value'));
    }
    return result;
  } finally {
    tree.delete();
  }
}

/** Promote only handlers that return JSX; names and nested helpers are not evidence. */
function finalizePages(context: ResolutionContext): Node[] {
  const updates: Node[] = [];
  const routes = context.getNodesByKind('route').filter((n) => n.id.startsWith('route:redwood:'));
  for (const file of new Set(routes.map((n) => n.filePath))) {
    const source = context.readFile(file);
    if (source === null) continue;
    const extracted = extractRedwoodRoutes(file, source);
    for (const route of extracted.nodes) {
      if (!route.name.startsWith('ANY ')) continue;
      const ref = extracted.references.find(
        (r) => r.fromNodeId === route.id && r.referenceKind === 'references',
      );
      if (!ref) continue;
      const imported = resolveViaImport(ref, context);
      const target = imported
        ? context.getNodeById?.(imported.targetNodeId)
        : context
            .getNodesInFile(file)
            .find(
              (n) => n.name === ref.referenceName && ['function', 'component'].includes(n.kind),
            );
      let page = target?.kind === 'component';
      if (target?.kind === 'function') {
        const source = context.readFile(target.filePath);
        const parser = getParser(target.language);
        const tree = source === null ? null : parser?.parse(source);
        if (tree)
          try {
            const declaration = tree.rootNode.namedChildren
              .map((n) =>
                n.type === 'export_statement' ? (n.childForFieldName('declaration') ?? n) : n,
              )
              .flatMap((n) => (n.type === 'lexical_declaration' ? n.namedChildren : [n]))
              .find((n) => n.childForFieldName('name')?.text === target.name);
            const handler =
              declaration?.type === 'variable_declarator'
                ? unwrap(declaration.childForFieldName('value'))
                : declaration;
            page = !!handler && returnedJsx(handler).length > 0;
          } finally {
            tree.delete();
          }
      }
      const existing = routes.find((n) => n.id === route.id);
      const name = page ? route.name.slice(4) : route.name;
      if (existing && existing.name !== name) updates.push({ ...existing, name });
    }
  }
  return updates;
}

export const redwoodResolver: FrameworkResolver = {
  name: 'redwood',
  languages: ['typescript', 'javascript', 'tsx', 'jsx'],
  detect: (context) => dependsOn(context, 'rwsdk'),
  resolve: () => null,
  extract: extractRedwoodRoutes,
  postExtract: finalizePages,
};
