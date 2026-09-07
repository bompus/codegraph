import type { Node as SyntaxNode } from 'web-tree-sitter';
import type { ExtractionResult } from '../../types';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { generateNodeId } from '../../extraction/tree-sitter-helpers';
import { dependsOn } from './package-deps';

const ROOT = 'src/pages/';
const EXTENSIONS = ['js', 'ts', 'tsx', 'jsx', 'mjs', 'cjs'];
const FUNCTIONS = new Set(['function_declaration', 'function_expression', 'arrow_function']);
export const isWakuPage = (file: string): boolean =>
  file.startsWith(ROOT) && /\.(?:[cm]?[jt]s|[jt]sx)$/.test(file);
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
function mutated(root: SyntaxNode, name: string): boolean {
  const aliases = new Set([name]);
  const declarations = root.descendantsOfType('variable_declarator');
  let previous = 0;
  while (previous !== aliases.size) {
    previous = aliases.size;
    for (const declaration of declarations) {
      const local = declaration.childForFieldName('name');
      const value = unwrap(declaration.childForFieldName('value'));
      if (
        local?.type === 'identifier' &&
        value?.type === 'identifier' &&
        (aliases.has(local.text) || aliases.has(value.text))
      ) {
        aliases.add(local.text);
        aliases.add(value.text);
      }
    }
  }
  return root
    .descendantsOfType([
      'assignment_expression',
      'augmented_assignment_expression',
      'update_expression',
      'call_expression',
    ])
    .some((node) => {
      if (node.type === 'call_expression')
        return (
          node
            .childForFieldName('arguments')
            ?.namedChildren.some((n) => n.type === 'identifier' && aliases.has(n.text)) ||
          (node.childForFieldName('function')?.type === 'member_expression' &&
            aliases.has(
              node.childForFieldName('function')?.childForFieldName('object')?.text ?? '',
            ))
        );
      const target = node.childForFieldName('left') ?? node.childForFieldName('argument');
      return (
        aliases.has(target?.text ?? '') ||
        !!target
          ?.descendantsOfType(['identifier', 'shorthand_property_identifier_pattern'])
          .some((n) => aliases.has(n.text))
      );
    });
}
type Binding = { node: SyntaxNode; value: SyntaxNode };
function exportsIn(root: SyntaxNode): Map<string, Binding | null> {
  const locals = new Map<string, Binding>();
  const exported = new Map<string, Binding | null>();
  for (const statement of root.namedChildren) {
    const node =
      statement.type === 'export_statement'
        ? statement.childForFieldName('declaration')
        : statement;
    const name = node?.childForFieldName('name');
    if (node && FUNCTIONS.has(node.type) && name) locals.set(name.text, { node, value: node });
    if (node?.type === 'lexical_declaration' && node.children.some((n) => n.type === 'const'))
      for (const variable of node.namedChildren) {
        const name = variable.childForFieldName('name');
        const value = unwrap(variable.childForFieldName('value'));
        if (name?.type === 'identifier' && value) locals.set(name.text, { node: variable, value });
      }
  }
  const add = (name: string, binding: Binding | null) => {
    const local = binding?.node.childForFieldName('name')?.text;
    exported.set(name, exported.has(name) || (local && mutated(root, local)) ? null : binding);
  };
  for (const statement of root.namedChildren) {
    if (statement.type !== 'export_statement' || statement.children.some((n) => n.type === 'type'))
      continue;
    const declaration = statement.childForFieldName('declaration');
    if (statement.children.some((n) => n.type === 'default')) {
      const value = unwrap(statement.childForFieldName('value') ?? declaration);
      const name =
        declaration?.childForFieldName('name')?.text ??
        (value?.type === 'identifier' ? value.text : null);
      add('default', name ? (locals.get(name) ?? null) : value ? { node: value, value } : null);
    } else if (declaration?.childForFieldName('name')) {
      const name = declaration.childForFieldName('name')!.text;
      add(name, locals.get(name) ?? null);
    } else if (declaration?.type === 'lexical_declaration')
      for (const variable of declaration.namedChildren) {
        const name = variable.childForFieldName('name')?.text;
        if (name) add(name, locals.get(name) ?? null);
      }
    for (const spec of statement.descendantsOfType('export_specifier')) {
      if (spec.children.some((n) => n.type === 'type')) continue;
      const name = spec.childForFieldName('name')?.text ?? '';
      add(
        spec.childForFieldName('alias')?.text ?? name,
        statement.childForFieldName('source') ? null : (locals.get(name) ?? null),
      );
    }
    if (
      statement.childForFieldName('source') &&
      !statement.descendantsOfType('export_specifier').length
    )
      add('getConfig', null);
  }
  return exported;
}
function imports(root: SyntaxNode, source: string, name: string): Set<string> {
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
function returned(node: SyntaxNode): SyntaxNode | null {
  const body = unwrap(node.childForFieldName('body'));
  if (body?.type !== 'statement_block') return body;
  const statements = body.namedChildren.filter((n) => n.type !== 'comment');
  return statements.length === 1 && statements[0]?.type === 'return_statement'
    ? unwrap(statements[0].namedChildren[0])
    : null;
}
const states = new WeakMap<ResolutionContext, Set<string> | null>();
function project(context: ResolutionContext): Set<string> | null {
  if (states.has(context)) return states.get(context)!;
  let extensions: Set<string> | null = new Set(EXTENSIONS);
  for (const ext of EXTENSIONS) {
    const file = `waku.config.${ext}`;
    const source = context.readFile(file);
    if (source === null) continue;
    const tree = getParser(detectLanguage(file))?.parse(source);
    if (!tree) {
      extensions = null;
      break;
    }
    try {
      const root = tree.rootNode;
      let config = exportsIn(root).get('default')?.value;
      if (
        config?.type === 'call_expression' &&
        imports(root, 'waku/config', 'defineConfig').has(
          config.childForFieldName('function')?.text ?? '',
        )
      )
        config = config.childForFieldName('arguments')?.namedChildren[0];
      const options = fields(config);
      if (!options || ['srcDir', 'basePath', 'vite'].some((key) => options.has(key)))
        extensions = null;
    } finally {
      tree.delete();
    }
  }
  const servers = EXTENSIONS.filter((ext) => context.readFile(`src/waku.server.${ext}`) !== null);
  if (servers.length > 1) extensions = null;
  if (extensions && servers.length === 1) {
    const file = `src/waku.server.${servers[0]}`;
    const tree = getParser(detectLanguage(file))?.parse(context.readFile(file)!);
    extensions = null;
    if (tree)
      try {
        const root = tree.rootNode;
        const adapter = new Set([
          ...imports(root, 'waku/adapters/default', 'default'),
          ...imports(root, 'waku/adapters/cloudflare', 'default'),
        ]);
        const routers = imports(root, 'waku', 'fsRouter');
        const entry = exportsIn(root).get('default')?.value;
        const router = entry?.childForFieldName('arguments')?.namedChildren[0];
        const args = router?.childForFieldName('arguments')?.namedChildren ?? [];
        const glob = args[0];
        const globArgs = glob?.childForFieldName('arguments')?.namedChildren ?? [];
        const pattern = literal(globArgs[0]);
        const match = pattern?.match(/^\.\/pages\/\*\*\/\*\.\{([a-z,]+)\}$/);
        if (
          entry?.type === 'call_expression' &&
          adapter.has(entry.childForFieldName('function')?.text ?? '') &&
          router?.type === 'call_expression' &&
          routers.has(router.childForFieldName('function')?.text ?? '') &&
          args.length === 1 &&
          glob?.type === 'call_expression' &&
          glob.childForFieldName('function')?.text === 'import.meta.glob' &&
          globArgs.length === 1 &&
          match &&
          match[1]!.split(',').every((ext) => EXTENSIONS.includes(ext)) &&
          ![...adapter, ...routers].some((name) => mutated(root, name))
        )
          extensions = new Set(match[1]!.split(','));
      } finally {
        tree.delete();
      }
  }
  states.set(context, extensions);
  return extensions;
}

/** Waku 1.0.0-rc.0 registers static parameter pages only for their declared static paths. */
function paths(path: string, options: Map<string, SyntaxNode>): string[] {
  if ([...options.keys()].some((key) => !['render', 'staticPaths'].includes(key))) return [];
  const render = options.has('render') ? literal(options.get('render')) : 'static';
  if (render !== 'static' && render !== 'dynamic') return [];
  const segments = path.split('/').filter((segment) => !segment.startsWith('('));
  if (segments.at(-1) === 'index.html') segments.pop();
  const params: { index: number; prefix: string; suffix: string; name: string; rest: boolean }[] =
    [];
  for (let index = 0; index < segments.length; index++) {
    const segment = segments[index]!;
    if (!/[\[\]]/.test(segment)) continue;
    const match = segment.match(/^([^\[\]]*)\[(\.\.\.)?([A-Za-z_$][\w$]*)\]([^\[\]]*)$/);
    if (!match || (match[2] && (index !== segments.length - 1 || match[1] || match[4]))) return [];
    params.push({ index, prefix: match[1]!, suffix: match[4]!, name: match[3]!, rest: !!match[2] });
  }
  const join = (parts: string[]) => parts.join('/').replace(/\/$/, '') || '/';
  if (!params.length) return [join(segments)];
  if (render === 'dynamic') {
    for (const param of params)
      segments[param.index] =
        `${param.prefix}${param.rest ? '*' : ':'}${param.name}${param.suffix}`;
    return [join(segments)];
  }
  const staticPaths = options.get('staticPaths');
  if (staticPaths?.type !== 'array') return [];
  const result: string[] = [];
  for (const item of staticPaths.namedChildren) {
    if (item.type === 'comment') continue;
    const values =
      item.type === 'array'
        ? item.namedChildren.filter((n) => n.type !== 'comment').map(literal)
        : [literal(item)];
    if (
      values.some((value) => value === null || /[/?#]/.test(value)) ||
      (params.at(-1)!.rest ? values.length < params.length : values.length !== params.length)
    )
      return [];
    const expanded = [...segments];
    params.forEach((param, index) => {
      const value = (param.rest ? values.slice(index).join('/') : values[index]!).replace(
        / /g,
        '-',
      );
      expanded[param.index] = param.prefix + value + param.suffix;
    });
    result.push(join(expanded));
  }
  return [...new Set(result)];
}
export function extractWakuRoutes(
  filePath: string,
  content: string,
  context: ResolutionContext,
  existing: ExtractionResult,
): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!isWakuPage(filePath) || !project(context)?.has(filePath.split('.').at(-1)!)) return result;
  const segments = filePath
    .slice(ROOT.length)
    .replace(/\.[^.]+$/, '')
    .split('/');
  if (
    segments.some((part) => ['_actions', '_components', '_hooks'].includes(part)) ||
    ['_api', '_slices', '_interceptors'].includes(segments[0]!) ||
    ['_layout', '_root', '[path]'].includes(segments.at(-1)!)
  )
    return result;
  if (segments.at(-1) === 'index') segments.pop();
  const language = detectLanguage(filePath);
  const tree = getParser(language)?.parse(content);
  if (!tree) return result;
  try {
    const exported = exportsIn(tree.rootNode);
    const binding = exported.get('default');
    if (!binding || !FUNCTIONS.has(binding.value.type)) return result;
    let options = new Map<string, SyntaxNode>();
    if (exported.has('getConfig')) {
      const config = exported.get('getConfig');
      const object =
        config && FUNCTIONS.has(config.value.type) ? fields(returned(config.value)) : null;
      if (!object) return result;
      options = object;
    }
    const routes = paths('/' + segments.join('/'), options);
    if (!routes.length) return result;
    const anchor = binding.value;
    const name = binding.node.childForFieldName('name')?.text;
    let target = name
      ? existing.nodes.find(
          (node) =>
            node.name === name &&
            ['function', 'component'].includes(node.kind) &&
            node.startLine === anchor.startPosition.row + 1 &&
            node.startColumn === anchor.startPosition.column,
        )
      : undefined;
    if (!name) {
      const id = generateNodeId(filePath, 'component', 'default', anchor.startPosition.row + 1);
      target = {
        id,
        kind: 'component',
        name: 'default',
        qualifiedName: `${filePath}::default`,
        filePath,
        language,
        startLine: anchor.startPosition.row + 1,
        startColumn: anchor.startPosition.column,
        endLine: anchor.endPosition.row + 1,
        endColumn: anchor.endPosition.column,
        updatedAt: Date.now(),
      };
      result.nodes.push(target);
      const body = anchor.childForFieldName('body')!;
      for (const ref of existing.unresolvedReferences) {
        const row = ref.line - 1;
        if (
          ref.fromNodeId === `file:${filePath}` &&
          row >= body.startPosition.row &&
          row <= body.endPosition.row &&
          (row !== body.startPosition.row || ref.column >= body.startPosition.column) &&
          (row !== body.endPosition.row || ref.column < body.endPosition.column)
        )
          ref.fromNodeId = id;
      }
    }
    if (!target) return result;
    for (const routePath of routes) {
      const id = `route:waku:${filePath}:${routePath}`;
      result.nodes.push({
        ...target,
        id,
        kind: 'route',
        name: routePath,
        qualifiedName: `${filePath}::${routePath}`,
      });
      result.references.push({
        fromNodeId: id,
        referenceName: `waku-target:${target.id}`,
        referenceKind: 'references',
        filePath,
        language,
        line: target.startLine,
        column: target.startColumn,
      });
    }
    return result;
  } finally {
    tree.delete();
  }
}
export const wakuResolver: FrameworkResolver = {
  name: 'waku',
  languages: ['typescript', 'javascript', 'tsx', 'jsx'],
  detect: (context) => dependsOn(context, 'waku'),
  claimsReference: (name) => name.startsWith('waku-target:'),
  resolve(ref, context) {
    if (!ref.fromNodeId.startsWith('route:waku:')) return null;
    const target = context
      .getNodesInFile(ref.filePath)
      .find((node) => node.id === ref.referenceName.slice('waku-target:'.length));
    return target
      ? { original: ref, targetNodeId: target.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
