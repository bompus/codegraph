import type { Node as SyntaxNode } from 'web-tree-sitter';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { dependsOn } from './package-deps';

const EXT = '(?:[cm]?[jt]s|[jt]sx)';
const namedFile = (name: string): RegExp => new RegExp(`(?:^|/)\\+${name}\\.${EXT}$`);
const PAGE = namedFile('Page');
const ROUTE = namedFile('route');
const CONFIG = namedFile('config');
const BEFORE_ROUTE = namedFile('onBeforeRoute');
const ROOT_OVERRIDE = namedFile('filesystemRoutingRoot');
const META = namedFile('meta');
const EXTENDS = namedFile('extends');
export const isVikePage = (file: string): boolean => PAGE.test(file);
const directory = (file: string): string =>
  file.includes('/') ? file.slice(0, file.lastIndexOf('/')) : '';
const location = (file: string): string =>
  directory(file)
    .split('/')
    .filter((part) => part !== 'pages' && part !== 'renderer')
    .join('/');
const within = (file: string, parent: string): boolean =>
  !parent || file === parent || file.startsWith(parent + '/');
const literal = (node: SyntaxNode | null | undefined): string | null =>
  node?.type === 'string' && !node.text.includes('\\') ? node.text.slice(1, -1) : null;
const unwrap = (node: SyntaxNode | null | undefined): SyntaxNode | null => {
  while (
    node &&
    ['parenthesized_expression', 'as_expression', 'satisfies_expression'].includes(node.type)
  )
    node = node.namedChildren[0];
  return node ?? null;
};

function imported(root: SyntaxNode, source: string, name = 'default'): Set<string> {
  const result = new Set<string>();
  for (const statement of root.namedChildren) {
    if (
      statement.type !== 'import_statement' ||
      literal(statement.childForFieldName('source')) !== source ||
      statement.children.some((n) => n.type === 'type')
    )
      continue;
    if (name === 'default')
      for (const item of statement.namedChildren.find((n) => n.type === 'import_clause')
        ?.namedChildren ?? [])
        if (item.type === 'identifier') result.add(item.text);
    for (const spec of statement.descendantsOfType('import_specifier'))
      if (
        spec.childForFieldName('name')?.text === name &&
        !spec.children.some((n) => n.type === 'type')
      )
        result.add(spec.childForFieldName('alias')?.text ?? name);
  }
  return result;
}

function mutated(root: SyntaxNode, name: string): boolean {
  return root
    .descendantsOfType([
      'assignment_expression',
      'augmented_assignment_expression',
      'update_expression',
      'call_expression',
    ])
    .some((node) => {
      if (node.type === 'call_expression') {
        const callee = node.childForFieldName('function');
        const args = node.childForFieldName('arguments');
        return (
          !!args?.namedChildren.some((n) => n.type === 'identifier' && n.text === name) ||
          (callee?.type === 'member_expression' &&
            callee.childForFieldName('object')?.text === name)
        );
      }
      const target = node.childForFieldName('left') ?? node.childForFieldName('argument');
      return (
        target?.text === name ||
        !!target
          ?.descendantsOfType(['identifier', 'shorthand_property_identifier_pattern'])
          .some((n) => n.text === name)
      );
    });
}

function fields(node: SyntaxNode | null | undefined): Map<string, SyntaxNode> | null {
  node = unwrap(node);
  if (node?.type !== 'object') return null;
  const result = new Map<string, SyntaxNode>();
  for (const item of node.namedChildren) {
    if (item.type === 'comment') continue;
    const key = item.childForFieldName('key');
    const name = literal(key) ?? (key?.type === 'property_identifier' ? key.text : null);
    const value = item.childForFieldName('value');
    if (item.type !== 'pair' || !name || !value || result.has(name)) return null;
    result.set(name, value);
  }
  return result;
}

type Binding = { node: SyntaxNode; value: SyntaxNode };
function exported(root: SyntaxNode, exportName: string): Binding | null {
  const locals = new Map<string, Binding>();
  for (const statement of root.namedChildren) {
    const node =
      statement.type === 'export_statement'
        ? statement.childForFieldName('declaration')
        : statement;
    const name = node?.childForFieldName('name');
    if (node && ['function_declaration', 'class_declaration'].includes(node.type) && name)
      locals.set(name.text, { node, value: node });
    if (node?.type === 'lexical_declaration' && node.children.some((n) => n.type === 'const'))
      for (const variable of node.namedChildren) {
        const name = variable.childForFieldName('name');
        const value = unwrap(variable.childForFieldName('value'));
        if (name?.type === 'identifier' && value) locals.set(name.text, { node: variable, value });
      }
  }
  const candidates: (Binding | null)[] = [];
  for (const statement of root.namedChildren) {
    if (statement.type !== 'export_statement' || statement.children.some((n) => n.type === 'type'))
      continue;
    const declaration = statement.childForFieldName('declaration');
    if (statement.children.some((n) => n.type === 'default')) {
      const value = unwrap(statement.childForFieldName('value') ?? declaration);
      const name =
        declaration?.childForFieldName('name')?.text ??
        (value?.type === 'identifier' ? value.text : null);
      candidates.push(name ? (locals.get(name) ?? null) : value ? { node: value, value } : null);
    } else if (declaration?.childForFieldName('name')?.text === exportName)
      candidates.push(locals.get(exportName) ?? null);
    else if (declaration?.type === 'lexical_declaration')
      for (const variable of declaration.namedChildren)
        if (variable.childForFieldName('name')?.text === exportName)
          candidates.push(locals.get(exportName) ?? null);
    for (const spec of statement.descendantsOfType('export_specifier')) {
      if (spec.children.some((n) => n.type === 'type')) continue;
      const name = spec.childForFieldName('name')?.text ?? '';
      const exportedName = spec.childForFieldName('alias')?.text ?? name;
      if (exportedName === exportName || exportedName === 'default')
        candidates.push(statement.childForFieldName('source') ? null : (locals.get(name) ?? null));
    }
  }
  const selected = candidates.length === 1 ? candidates[0] : null;
  const name = selected?.node.childForFieldName('name')?.text;
  return selected && (!name || !mutated(root, name)) ? selected : null;
}

function plugin(context: ResolutionContext): boolean {
  for (const ext of ['ts', 'js', 'mts', 'mjs']) {
    const file = `vite.config.${ext}`;
    const content = context.readFile(file);
    if (content === null) continue;
    const tree = getParser(detectLanguage(file))?.parse(content);
    if (!tree) return false;
    try {
      const root = tree.rootNode;
      const helpers = imported(root, 'vike/plugin');
      let config = exported(root, 'config')?.value;
      if (config?.type === 'call_expression') {
        if (
          !imported(root, 'vite', 'defineConfig').has(
            config.childForFieldName('function')?.text ?? '',
          )
        )
          return false;
        config = config.childForFieldName('arguments')?.namedChildren[0];
      }
      const options = fields(config);
      if (!options || options.has('root') || options.has('base')) return false;
      const plugins = options.get('plugins');
      if (
        plugins?.type !== 'array' ||
        plugins.namedChildren.some((n) => n.type === 'spread_element')
      )
        return false;
      return plugins.namedChildren.some((call) => {
        const name = call.childForFieldName('function')?.text ?? '';
        if (call.type !== 'call_expression' || !helpers.has(name) || mutated(root, name))
          return false;
        const args =
          call.childForFieldName('arguments')?.namedChildren.filter((n) => n.type !== 'comment') ??
          [];
        return !args.length || (args.length === 1 && fields(args[0])?.size === 0);
      });
    } finally {
      tree.delete();
    }
  }
  return false;
}

function configEffect(content: string, file: string): 'safe' | 'local' | 'global' {
  const tree = getParser(detectLanguage(file))?.parse(content);
  if (!tree) return 'global';
  try {
    const root = tree.rootNode;
    const options = fields(exported(root, 'config')?.value);
    if (!options) return 'global';
    if (options.has('onBeforeRoute') || options.has('meta')) return 'global';
    const extension = unwrap(options.get('extends'));
    if (extension) {
      const helpers = imported(root, 'vike-react/config');
      const values =
        extension.type === 'array'
          ? extension.namedChildren.filter((n) => n.type !== 'comment')
          : [extension];
      if (
        !values.every(
          (node) =>
            node.type === 'identifier' && helpers.has(node.text) && !mutated(root, node.text),
        )
      )
        return 'global';
    }
    return ['route', 'filesystemRoutingRoot', 'Page'].some((name) => options.has(name))
      ? 'local'
      : 'safe';
  } finally {
    tree.delete();
  }
}

const states = new WeakMap<
  ResolutionContext,
  { active: boolean; files: string[]; blocked: string[] }
>();
function project(context: ResolutionContext) {
  const cached = states.get(context);
  if (cached) return cached;
  const state = {
    active: plugin(context),
    files: context.getAllFiles().filter((file) => context.fileExists(file)),
    blocked: [] as string[],
  };
  if (state.active)
    for (const file of state.files) {
      if (BEFORE_ROUTE.test(file) || META.test(file) || EXTENDS.test(file)) state.active = false;
      if (ROOT_OVERRIDE.test(file)) state.blocked.push(location(file));
      if (CONFIG.test(file)) {
        const content = context.readFile(file);
        const effect = content === null ? 'global' : configEffect(content, file);
        if (effect === 'global') state.active = false;
        if (effect === 'local') state.blocked.push(location(file));
      }
    }
  states.set(context, state);
  return state;
}

export function extractVikeRoutes(
  filePath: string,
  content: string,
  context: ResolutionContext,
): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!isVikePage(filePath)) return result;
  const state = project(context);
  if (!state.active || state.blocked.some((parent) => within(location(filePath), parent)))
    return result;
  if (
    state.files.filter((file) => isVikePage(file) && location(file) === location(filePath))
      .length !== 1
  )
    return result;
  let routePath =
    '/' +
    directory(filePath)
      .split('/')
      .filter(
        (part) =>
          part && !['pages', 'src', 'index', 'renderer'].includes(part) && !/^\(.*\)$/.test(part),
      )
      .join('/');
  const overrides = state.files
    .filter((file) => ROUTE.test(file) && within(location(filePath), location(file)))
    .sort((a, b) => location(b).length - location(a).length);
  if (overrides.length > 1 && location(overrides[0]!) === location(overrides[1]!)) return result;
  if (overrides.length) {
    const file = overrides[0]!;
    const source = context.readFile(file);
    if (source === null) return result;
    const tree = getParser(detectLanguage(file))?.parse(source);
    if (!tree) return result;
    try {
      const path = literal(exported(tree.rootNode, 'route')?.value);
      if (path === null || !path.startsWith('/')) return result;
      routePath = path;
    } finally {
      tree.delete();
    }
  }
  routePath = routePath.replace(/(^|\/)@([^/]+)/g, '$1:$2');
  const language = detectLanguage(filePath);
  const tree = getParser(language)?.parse(content);
  if (!tree) return result;
  try {
    const binding = exported(tree.rootNode, 'Page');
    const name = binding?.node.childForFieldName('name')?.text;
    if (
      !binding ||
      !name ||
      ![
        'function_declaration',
        'function_expression',
        'arrow_function',
        'class_declaration',
      ].includes(binding.value.type)
    )
      return result;
    const node = binding.value;
    const id = `route:vike:${filePath}`;
    result.nodes.push({
      id,
      kind: 'route',
      name: routePath,
      qualifiedName: `${filePath}::${routePath}`,
      filePath,
      language,
      startLine: node.startPosition.row + 1,
      startColumn: node.startPosition.column,
      endLine: node.endPosition.row + 1,
      endColumn: node.endPosition.column,
      updatedAt: Date.now(),
    });
    result.references.push({
      fromNodeId: id,
      referenceName: `vike-page:${name}`,
      referenceKind: 'references',
      filePath,
      language,
      line: node.startPosition.row + 1,
      column: node.startPosition.column,
    });
    return result;
  } finally {
    tree.delete();
  }
}

export const vikeResolver: FrameworkResolver = {
  name: 'vike',
  languages: ['typescript', 'javascript', 'tsx', 'jsx'],
  detect: (context) => dependsOn(context, 'vike'),
  claimsReference: (name) => name.startsWith('vike-page:'),
  resolve(ref, context) {
    if (!ref.fromNodeId.startsWith('route:vike:') || !ref.referenceName.startsWith('vike-page:'))
      return null;
    const candidates = context
      .getNodesInFile(ref.filePath)
      .filter(
        (node) =>
          node.name === ref.referenceName.slice('vike-page:'.length) &&
          ['function', 'component', 'class'].includes(node.kind) &&
          node.startLine === ref.line,
      );
    return candidates.length === 1
      ? { original: ref, targetNodeId: candidates[0]!.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
