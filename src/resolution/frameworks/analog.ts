import type { Node as SyntaxNode } from 'web-tree-sitter';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { dependsOn } from './package-deps';

const ROOT = 'src/app/pages/';
export const isAnalogPage = (file: string): boolean =>
  file.startsWith(ROOT) && file.endsWith('.page.ts');
const literal = (node: SyntaxNode | null | undefined): string | null =>
  node?.type === 'string' && !node.text.includes('\\') ? node.text.slice(1, -1) : null;
const BODIES = new Set([
  'function_declaration',
  'function_expression',
  'arrow_function',
  'method_definition',
  'statement_block',
  'binary_expression',
  'ternary_expression',
  'if_statement',
  'for_statement',
  'for_in_statement',
  'while_statement',
  'do_statement',
  'switch_statement',
]);
const states = new WeakMap<ResolutionContext, { active: boolean; files: string[] }>();

function importedNames(root: SyntaxNode, source: string, name: string): Set<string> {
  const names = new Set<string>();
  for (const statement of root.namedChildren) {
    if (
      statement.type !== 'import_statement' ||
      statement.children.some((n) => n.type === 'type') ||
      literal(statement.childForFieldName('source')) !== source
    )
      continue;
    const clause = statement.namedChildren.find((n) => n.type === 'import_clause');
    if (name === 'default')
      for (const child of clause?.namedChildren ?? [])
        if (child.type === 'identifier') names.add(child.text);
    for (const spec of statement.descendantsOfType('import_specifier'))
      if (
        !spec.children.some((n) => n.type === 'type') &&
        spec.childForFieldName('name')?.text === name
      )
        names.add(spec.childForFieldName('alias')?.text ?? name);
  }
  return names;
}

/** Checks registration and default roots once per extraction batch. */
function project(context: ResolutionContext): { active: boolean; files: string[] } {
  const cached = states.get(context);
  if (cached) return cached;
  const all = context.getAllFiles().filter((file) => context.fileExists(file));
  const state = { active: false, files: all.filter(isAnalogPage) };
  states.set(context, state);
  let plugin = false;
  for (const extension of ['ts', 'js', 'mts', 'mjs']) {
    const file = `vite.config.${extension}`;
    const content = context.readFile(file);
    if (content === null) continue;
    const tree = getParser(detectLanguage(file)!)?.parse(content);
    if (!tree) return state;
    try {
      const names = importedNames(tree.rootNode, '@analogjs/platform', 'default');
      const exported = tree.rootNode.namedChildren.find(
        (n) => n.type === 'export_statement' && n.children.some((c) => c.type === 'default'),
      );
      let config = exported?.childForFieldName('value');
      if (config?.type === 'call_expression') {
        const defineConfig = importedNames(tree.rootNode, 'vite', 'defineConfig');
        if (!defineConfig.has(config.childForFieldName('function')?.text ?? '')) return state;
        config = config.childForFieldName('arguments')?.namedChildren[0];
      }
      if (config?.type !== 'object') return state;
      const keys = config.namedChildren
        .filter((n) => n.type !== 'comment')
        .map((n) =>
          n.type === 'pair'
            ? (literal(n.childForFieldName('key')) ?? n.childForFieldName('key')?.text)
            : null,
        );
      if (
        keys.some((key) => !key) ||
        new Set(keys).size !== keys.length ||
        config.namedChildren.some(
          (n) => n.childForFieldName('key')?.type === 'computed_property_name',
        )
      )
        return state;
      if (
        config.descendantsOfType('spread_element').length ||
        config
          .descendantsOfType('pair')
          .some((n) =>
            ['root', 'workspaceRoot', 'additionalPagesDirs', 'routes'].includes(
              n.childForFieldName('key')?.text.replace(/^['"]|['"]$/g, '') ?? '',
            ),
          )
      )
        return state;
      const plugins = config.namedChildren
        .find((n) => n.childForFieldName('key')?.text === 'plugins')
        ?.childForFieldName('value');
      if (plugins?.type !== 'array') return state;
      plugin = plugins.namedChildren.some(
        (n) =>
          n.type === 'call_expression' &&
          names.has(n.childForFieldName('function')?.text ?? '') &&
          (n.childForFieldName('arguments')?.namedChildren.length === 0 ||
            (n.childForFieldName('arguments')?.namedChildren.length === 1 &&
              n.childForFieldName('arguments')?.namedChildren[0]?.type === 'object')),
      );
    } finally {
      tree.delete();
    }
  }
  if (!plugin) return state;
  for (const file of all) {
    if (!/\.[jt]s$/.test(file) || isAnalogPage(file)) continue;
    const content = context.readFile(file);
    if (!content?.includes('provideFileRouter')) continue;
    const tree = getParser(detectLanguage(file)!)?.parse(content);
    if (!tree) continue;
    try {
      const names = importedNames(tree.rootNode, '@analogjs/router', 'provideFileRouter');
      const visit = (node: SyntaxNode): boolean => {
        if (BODIES.has(node.type)) return false;
        if (
          node.type === 'call_expression' &&
          names.has(node.childForFieldName('function')?.text ?? '')
        )
          return node.childForFieldName('arguments')?.namedChildren.length === 0;
        return node.namedChildren.some(visit);
      };
      if (visit(tree.rootNode)) state.active = true;
    } finally {
      tree.delete();
    }
  }
  return state;
}

export function extractAnalogRoutes(
  filePath: string,
  content: string,
  context: ResolutionContext,
): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!isAnalogPage(filePath)) return result;
  const state = project(context);
  if (!state.active) return result;
  const raw = filePath.slice(ROOT.length, -'.page.ts'.length);
  // Hierarchy is computed from directories before dots become URL separators.
  if (state.files.some((file) => file.startsWith(ROOT + raw + '/'))) return result;
  if (raw.includes('[[...')) return result;
  const routePath =
    '/' +
    raw
      .split('/')
      .map((segment) =>
        segment
          .replace(/\[\.\.\.([^\]]+)\]/g, '*')
          .replace(/\[([^\]]+)\]/g, ':$1')
          .replace(/index|\(.*?\)/g, '')
          .replace(/\./g, '/'),
      )
      .join('/')
      .split('/')
      .filter(Boolean)
      .join('/');
  const tree = getParser('typescript')?.parse(content);
  if (!tree) return result;
  try {
    const exports = tree.rootNode.namedChildren.filter((n) => n.type === 'export_statement');
    if (
      exports.some((n) =>
        n
          .descendantsOfType(['variable_declarator', 'export_specifier'])
          .some(
            (item) =>
              item.childForFieldName('name')?.text === 'routeMeta' ||
              item.childForFieldName('alias')?.text === 'routeMeta',
          ),
      )
    )
      return result;
    const exported = exports.find((n) => n.children.some((c) => c.type === 'default'));
    let declaration = exported?.childForFieldName('declaration');
    const value = exported?.childForFieldName('value');
    if (!declaration && value?.type === 'identifier')
      declaration =
        tree.rootNode.namedChildren.find(
          (n) => n.type === 'class_declaration' && n.childForFieldName('name')?.text === value.text,
        ) ?? null;
    if (declaration?.type !== 'class_declaration') return result;
    const name = declaration.childForFieldName('name')?.text;
    if (!name) return result;
    const id = `route:analog:${filePath}`;
    result.nodes.push({
      id,
      kind: 'route',
      name: routePath,
      qualifiedName: `${filePath}::${routePath}`,
      filePath,
      language: 'typescript',
      startLine: declaration.startPosition.row + 1,
      endLine: declaration.endPosition.row + 1,
      startColumn: declaration.startPosition.column,
      endColumn: declaration.endPosition.column,
      updatedAt: Date.now(),
    });
    result.references.push({
      fromNodeId: id,
      referenceName: 'analog-component:' + name,
      referenceKind: 'references',
      filePath,
      language: 'typescript',
      line: declaration.startPosition.row + 1,
      column: declaration.startPosition.column,
    });
    return result;
  } finally {
    tree.delete();
  }
}

export const analogResolver: FrameworkResolver = {
  name: 'analog',
  languages: ['typescript'],
  detect: (context) => dependsOn(context, '@analogjs/router'),
  claimsReference: (name) => name.startsWith('analog-component:'),
  resolve(ref, context) {
    if (
      !ref.fromNodeId.startsWith('route:analog:') ||
      !ref.referenceName.startsWith('analog-component:')
    )
      return null;
    const candidates = context
      .getNodesInFile(ref.filePath)
      .filter(
        (n) =>
          n.name === ref.referenceName.slice('analog-component:'.length) &&
          ['class', 'component'].includes(n.kind),
      );
    return candidates.length === 1
      ? { original: ref, targetNodeId: candidates[0]!.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
