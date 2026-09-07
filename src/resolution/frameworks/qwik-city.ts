import type { Node as SyntaxNode } from 'web-tree-sitter';
import type { Node, ExtractionResult } from '../../types';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { generateNodeId } from '../../extraction/tree-sitter-helpers';
import { dependsOn } from './package-deps';

const ROOT = 'src/routes/';
const FUNCTIONS = new Set(['function_declaration', 'function_expression', 'arrow_function']);
const CONDITIONAL = new Set([
  'binary_expression',
  'ternary_expression',
  'if_statement',
  'switch_statement',
  'for_statement',
  'for_in_statement',
  'while_statement',
  'do_statement',
]);
export const isQwikCityRoute = (file: string): boolean =>
  file.startsWith(ROOT) && /(?:^|\/)index\.[jt]sx?$/.test(file);
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

function imports(root: SyntaxNode, source: string, imported: string): Set<string> {
  const names = new Set<string>();
  for (const statement of root.namedChildren) {
    if (
      statement.type !== 'import_statement' ||
      literal(statement.childForFieldName('source')) !== source ||
      statement.children.some((n) => n.type === 'type')
    )
      continue;
    for (const spec of statement.descendantsOfType('import_specifier'))
      if (
        spec.childForFieldName('name')?.text === imported &&
        !spec.children.some((n) => n.type === 'type')
      )
        names.add(spec.childForFieldName('alias')?.text ?? imported);
  }
  return names;
}

function shadowed(root: SyntaxNode, names: Set<string>): boolean {
  return root
    .descendantsOfType([
      'required_parameter',
      'optional_parameter',
      'variable_declarator',
      'function_declaration',
      'arrow_function',
      'assignment_expression',
    ])
    .some((node) => {
      const pattern =
        node.childForFieldName('name') ??
        node.childForFieldName('pattern') ??
        node.childForFieldName('parameter') ??
        node.childForFieldName('left');
      return (
        !!pattern &&
        (names.has(pattern.text) ||
          pattern
            .descendantsOfType(['identifier', 'shorthand_property_identifier_pattern'])
            .some((n) => names.has(n.text)))
      );
    });
}

function fields(node: SyntaxNode | null | undefined): Map<string, SyntaxNode> | null {
  node = unwrap(node);
  if (node?.type !== 'object') return null;
  const result = new Map<string, SyntaxNode>();
  for (const entry of node.namedChildren) {
    if (entry.type === 'comment') continue;
    const key = entry.childForFieldName('key');
    const name = literal(key) ?? (key?.type === 'property_identifier' ? key.text : null);
    const value = entry.childForFieldName('value');
    if (entry.type !== 'pair' || !name || !value || result.has(name)) return null;
    result.set(name, value);
  }
  return result;
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
      const helpers = imports(root, '@builder.io/qwik-city/vite', 'qwikCity');
      if (shadowed(root, helpers)) return false;
      const exported = root.namedChildren.find(
        (n) => n.type === 'export_statement' && n.children.some((c) => c.type === 'default'),
      );
      let config = unwrap(exported?.childForFieldName('value'));
      if (config?.type === 'call_expression') {
        const define = imports(root, 'vite', 'defineConfig');
        if (!define.has(config.childForFieldName('function')?.text ?? '') || shadowed(root, define))
          return false;
        config = unwrap(config.childForFieldName('arguments')?.namedChildren[0]);
      }
      if (config && FUNCTIONS.has(config.type)) {
        let body = unwrap(config.childForFieldName('body'));
        if (body?.type === 'statement_block') {
          const statements = body.namedChildren.filter((n) => n.type !== 'comment');
          if (
            statements.some((n) => CONDITIONAL.has(n.type)) ||
            statements.filter((n) => n.type === 'return_statement').length !== 1
          )
            return false;
          body = unwrap(statements.find((n) => n.type === 'return_statement')?.namedChildren[0]);
        }
        config = body;
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
        if (
          call.type !== 'call_expression' ||
          !helpers.has(call.childForFieldName('function')?.text ?? '')
        )
          return false;
        const args =
          call.childForFieldName('arguments')?.namedChildren.filter((n) => n.type !== 'comment') ??
          [];
        return args.length === 0 || (args.length === 1 && fields(args[0])?.size === 0);
      });
    } finally {
      tree.delete();
    }
  }
  return false;
}

type Binding = { node: SyntaxNode; value: SyntaxNode };
function exportsIn(root: SyntaxNode): Map<string, Binding | null> {
  const locals = new Map<string, Binding>();
  for (const statement of root.namedChildren) {
    const node =
      statement.type === 'export_statement'
        ? statement.childForFieldName('declaration')
        : statement;
    const name = node?.childForFieldName('name');
    if (node?.type === 'function_declaration' && name) locals.set(name.text, { node, value: node });
    if (node?.type === 'lexical_declaration' && node.children.some((n) => n.type === 'const'))
      for (const binding of node.namedChildren) {
        const name = binding.childForFieldName('name');
        const value = unwrap(binding.childForFieldName('value'));
        if (name?.type === 'identifier' && value) locals.set(name.text, { node: binding, value });
      }
  }
  for (const mutation of root.descendantsOfType([
    'assignment_expression',
    'augmented_assignment_expression',
    'update_expression',
  ])) {
    const target = mutation.childForFieldName('left') ?? mutation.childForFieldName('argument');
    if (target?.type === 'identifier') locals.delete(target.text);
    for (const name of target?.descendantsOfType([
      'identifier',
      'shorthand_property_identifier_pattern',
    ]) ?? [])
      locals.delete(name.text);
  }
  const result = new Map<string, Binding | null>();
  for (const statement of root.namedChildren) {
    if (statement.type !== 'export_statement' || statement.children.some((n) => n.type === 'type'))
      continue;
    const declaration = statement.childForFieldName('declaration');
    if (statement.children.some((n) => n.type === 'default')) {
      const value = unwrap(statement.childForFieldName('value') ?? declaration);
      const name =
        declaration?.childForFieldName('name')?.text ??
        (value?.type === 'identifier' ? value.text : null);
      result.set(
        'default',
        name ? (locals.get(name) ?? null) : value ? { node: value, value } : null,
      );
    } else if (declaration?.type === 'function_declaration') {
      const name = declaration.childForFieldName('name')?.text;
      if (name) result.set(name, locals.get(name) ?? null);
    } else if (declaration?.type === 'lexical_declaration') {
      for (const variable of declaration.namedChildren) {
        const name = variable.childForFieldName('name')?.text;
        if (name) result.set(name, locals.get(name) ?? null);
      }
    }
    for (const spec of statement.descendantsOfType('export_specifier')) {
      if (spec.children.some((n) => n.type === 'type')) continue;
      const name = spec.childForFieldName('name')?.text ?? '';
      result.set(
        spec.childForFieldName('alias')?.text ?? name,
        statement.childForFieldName('source') ? null : (locals.get(name) ?? null),
      );
    }
  }
  return result;
}

function componentBody(value: SyntaxNode, root: SyntaxNode): SyntaxNode | null {
  if (FUNCTIONS.has(value.type)) return value;
  const helpers = imports(root, '@builder.io/qwik', 'component$');
  if (
    value.type !== 'call_expression' ||
    !helpers.has(value.childForFieldName('function')?.text ?? '') ||
    shadowed(root, helpers)
  )
    return null;
  const args =
    value.childForFieldName('arguments')?.namedChildren.filter((n) => n.type !== 'comment') ?? [];
  return args.length === 1 && FUNCTIONS.has(args[0]!.type) ? args[0]! : null;
}

function pages(context: ResolutionContext): boolean {
  for (const ext of ['tsx', 'jsx']) {
    const file = `src/root.${ext}`;
    const content = context.readFile(file);
    if (content === null) continue;
    const tree = getParser(detectLanguage(file))?.parse(content);
    if (!tree) return false;
    try {
      const root = tree.rootNode;
      const value = exportsIn(root).get('default')?.value;
      const component = value && componentBody(value, root);
      if (!component) return false;
      const providers = imports(root, '@builder.io/qwik-city', 'QwikCityProvider');
      const outlets = imports(root, '@builder.io/qwik-city', 'RouterOutlet');
      if (shadowed(root, new Set([...providers, ...outlets]))) return false;
      for (const outlet of component.descendantsOfType('jsx_self_closing_element')) {
        if (
          !outlets.has(outlet.childForFieldName('name')?.text ?? '') ||
          outlet.namedChildren.length !== 1
        )
          continue;
        let parent = outlet.parent;
        let provider = false;
        while (parent && parent.id !== component.id) {
          if (FUNCTIONS.has(parent.type) || CONDITIONAL.has(parent.type)) break;
          if (
            parent.type === 'jsx_element' &&
            providers.has(parent.namedChildren[0]?.childForFieldName('name')?.text ?? '') &&
            parent.namedChildren[0]?.namedChildren.length === 1
          )
            provider = true;
          parent = parent.parent;
        }
        if (provider && parent?.id === component.id) return true;
      }
    } finally {
      tree.delete();
    }
  }
  return false;
}

const states = new WeakMap<ResolutionContext, { active: boolean; pages: boolean }>();
function project(context: ResolutionContext) {
  const cached = states.get(context);
  if (cached) return cached;
  const active = plugin(context);
  const state = { active, pages: active && pages(context) };
  states.set(context, state);
  return state;
}

/** Factory components own their callback's file-level references, whether named or anonymous. */
export function extractQwikCityRoutes(
  filePath: string,
  content: string,
  context: ResolutionContext,
  existing: ExtractionResult,
): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!isQwikCityRoute(filePath)) return result;
  const state = project(context);
  if (!state.active) return result;
  const segments = filePath.slice(ROOT.length).split('/').slice(0, -1);
  if (segments.some((segment) => segment.includes('[[') || segment.includes(']]'))) return result;
  const routePath =
    '/' +
    segments
      .filter((segment) => !/^\(.*\)$/.test(segment) && !segment.startsWith('__'))
      .map((segment) =>
        segment.replace(
          /\[(\.\.\.)?(\w+)\]/g,
          (_, rest: string, name: string) => (rest ? '*' : ':') + name,
        ),
      )
      .join('/') +
    (segments.some((segment) => !/^\(.*\)$/.test(segment) && !segment.startsWith('__')) ? '/' : '');
  const language = detectLanguage(filePath);
  const tree = getParser(language)?.parse(content);
  if (!tree) return result;
  try {
    const root = tree.rootNode;
    const exported = exportsIn(root);
    const add = (method: string, binding: Binding | null | undefined) => {
      if (!binding) return;
      const callback = method
        ? FUNCTIONS.has(binding.value.type)
          ? binding.value
          : null
        : componentBody(binding.value, root);
      if (!callback) return;
      const name = binding.node.childForFieldName('name')?.text;
      if (method && !name) return;
      let targetId: string | undefined;
      if (!name) {
        // Factory-call defaults have no generic extractor symbol; this is their exported component.
        if (binding.value.type !== 'call_expression') return;
        targetId = generateNodeId(
          filePath,
          'component',
          'default',
          binding.node.startPosition.row + 1,
        );
        const component: Node = {
          id: targetId,
          kind: 'component',
          name: 'default',
          qualifiedName: `${filePath}::default`,
          filePath,
          language,
          startLine: binding.node.startPosition.row + 1,
          startColumn: binding.node.startPosition.column,
          endLine: binding.node.endPosition.row + 1,
          endColumn: binding.node.endPosition.column,
          updatedAt: Date.now(),
        };
        result.nodes.push(component);
      }
      if (binding.value.type === 'call_expression') {
        const named = existing.nodes.filter(
          (node) =>
            node.name === name &&
            node.kind === 'constant' &&
            node.startLine === binding.node.startPosition.row + 1,
        );
        const owner = targetId ?? (named.length === 1 ? named[0]!.id : undefined);
        const body = callback.childForFieldName('body')!;
        for (const ref of existing.unresolvedReferences) {
          const row = ref.line - 1;
          if (
            !owner ||
            ref.fromNodeId !== `file:${filePath}` ||
            row < body.startPosition.row ||
            row > body.endPosition.row ||
            (row === body.startPosition.row && ref.column < body.startPosition.column) ||
            (row === body.endPosition.row && ref.column >= body.endPosition.column)
          )
            continue;
          ref.fromNodeId = owner;
        }
      }
      const routeName = method ? `${method} ${routePath}` : routePath;
      const id = `route:qwik-city:${filePath}:${method || 'page'}`;
      result.nodes.push({
        id,
        kind: 'route',
        name: routeName,
        qualifiedName: `${filePath}::${routeName}`,
        filePath,
        language,
        startLine: binding.node.startPosition.row + 1,
        startColumn: binding.node.startPosition.column,
        endLine: binding.node.endPosition.row + 1,
        endColumn: binding.node.endPosition.column,
        updatedAt: Date.now(),
      });
      result.references.push({
        fromNodeId: id,
        referenceName: targetId ? `qwik-default:${targetId}` : `qwik-target:${name}`,
        referenceKind: 'references',
        filePath,
        language,
        line: binding.node.startPosition.row + 1,
        column: binding.node.startPosition.column,
      });
    };
    if (state.pages) add('', exported.get('default'));
    for (const method of ['Get', 'Post', 'Put', 'Patch', 'Delete', 'Options', 'Head'])
      add(method.toUpperCase(), exported.get('on' + method));
    return result;
  } finally {
    tree.delete();
  }
}

export const qwikCityResolver: FrameworkResolver = {
  name: 'qwik-city',
  languages: ['typescript', 'javascript', 'tsx', 'jsx'],
  detect: (context) => dependsOn(context, '@builder.io/qwik-city'),
  claimsReference: (name) => name.startsWith('qwik-target:') || name.startsWith('qwik-default:'),
  resolve(ref, context) {
    if (!ref.fromNodeId.startsWith('route:qwik-city:')) return null;
    const candidates = context
      .getNodesInFile(ref.filePath)
      .filter((node) =>
        ref.referenceName.startsWith('qwik-default:')
          ? node.id === ref.referenceName.slice('qwik-default:'.length)
          : ref.referenceName.startsWith('qwik-target:') &&
            node.name === ref.referenceName.slice('qwik-target:'.length) &&
            ['function', 'component', 'constant'].includes(node.kind) &&
            node.startLine === ref.line,
      );
    return candidates.length === 1
      ? { original: ref, targetNodeId: candidates[0]!.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
