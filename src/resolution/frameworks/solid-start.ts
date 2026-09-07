import type { Node as SyntaxNode } from 'web-tree-sitter';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { dependsOn } from './package-deps';

const ROOT = 'src/routes/';
const METHODS = ['HEAD', 'GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'OPTIONS'];
export const isSolidStartRoute = (file: string): boolean =>
  file.startsWith(ROOT) &&
  /\.[jt]sx?$/.test(file) &&
  !file.endsWith('.d.ts') &&
  !file
    .slice(ROOT.length)
    .split('/')
    .some((part) => part.startsWith('.'));
const literal = (node: SyntaxNode | null | undefined): string | null =>
  node?.type === 'string' && !node.text.includes('\\') ? node.text.slice(1, -1) : null;
const rawPath = (file: string): string => file.slice(ROOT.length).replace(/\.[jt]sx?$/, '');

function imports(root: SyntaxNode, source: string, name: string): Set<string> {
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
        spec.childForFieldName('name')?.text === name &&
        !spec.children.some((n) => n.type === 'type')
      )
        names.add(spec.childForFieldName('alias')?.text ?? name);
  }
  return names;
}

function object(node: SyntaxNode | null | undefined): Map<string, SyntaxNode> | null {
  if (node?.type !== 'object') return null;
  const values = new Map<string, SyntaxNode>();
  for (const field of node.namedChildren) {
    if (field.type === 'comment') continue;
    const key = field.childForFieldName('key');
    const name = literal(key) ?? (key?.type === 'property_identifier' ? key.text : null);
    const value = field.childForFieldName('value');
    if (field.type !== 'pair' || !name || !value || values.has(name)) return null;
    values.set(name, value);
  }
  return values;
}

function plugin(context: ResolutionContext): boolean {
  for (const extension of ['ts', 'js', 'mts', 'mjs']) {
    const file = `vite.config.${extension}`;
    const content = context.readFile(file);
    if (content === null) continue;
    const tree = getParser(detectLanguage(file)!)?.parse(content);
    if (!tree) return false;
    try {
      const root = tree.rootNode;
      const exported = root.namedChildren.find(
        (n) => n.type === 'export_statement' && n.children.some((c) => c.type === 'default'),
      );
      let config = exported?.childForFieldName('value');
      if (config?.type === 'call_expression') {
        if (
          !imports(root, 'vite', 'defineConfig').has(
            config.childForFieldName('function')?.text ?? '',
          )
        )
          return false;
        config = config.childForFieldName('arguments')?.namedChildren[0];
      }
      const fields = object(config);
      if (!fields || fields.has('root') || fields.has('base')) return false;
      const plugins = fields.get('plugins');
      if (plugins?.type !== 'array') return false;
      const names = imports(root, '@solidjs/start/config', 'solidStart');
      return plugins.namedChildren.some((call) => {
        if (
          call.type !== 'call_expression' ||
          !names.has(call.childForFieldName('function')?.text ?? '')
        )
          return false;
        const args =
          call.childForFieldName('arguments')?.namedChildren.filter((n) => n.type !== 'comment') ??
          [];
        if (!args.length) return true;
        const options = object(args[0]);
        return args.length === 1 && options !== null && options.size === 0;
      });
    } finally {
      tree.delete();
    }
  }
  return false;
}

function fileRoutes(context: ResolutionContext): boolean {
  for (const extension of ['tsx', 'jsx']) {
    const file = `src/app.${extension}`;
    const content = context.readFile(file);
    if (content === null) continue;
    const tree = getParser(detectLanguage(file)!)?.parse(content);
    if (!tree) return false;
    try {
      const root = tree.rootNode;
      const routers = imports(root, '@solidjs/router', 'Router');
      const routes = imports(root, '@solidjs/start/router', 'FileRoutes');
      const names = new Set([...routers, ...routes]);
      const exported = root.namedChildren.find(
        (n) => n.type === 'export_statement' && n.children.some((c) => c.type === 'default'),
      );
      let app = exported?.childForFieldName('declaration') ?? exported?.childForFieldName('value');
      if (app?.type === 'identifier') {
        const name = app.text;
        app =
          root.namedChildren.find(
            (n) => n.type === 'function_declaration' && n.childForFieldName('name')?.text === name,
          ) ??
          root.namedChildren
            .filter((n) => n.type === 'lexical_declaration')
            .flatMap((n) => n.namedChildren)
            .find((n) => n.childForFieldName('name')?.text === name)
            ?.childForFieldName('value');
      }
      if (
        !app ||
        !['function_declaration', 'function_expression', 'arrow_function'].includes(app.type)
      )
        return false;
      // A local binding with an imported name makes the JSX registration ambiguous.
      if (
        root
          .descendantsOfType([
            'required_parameter',
            'optional_parameter',
            'variable_declarator',
            'function_declaration',
            'arrow_function',
          ])
          .some((n) =>
            [...names].some(
              (name) =>
                (
                  n.childForFieldName('name') ??
                  n.childForFieldName('pattern') ??
                  n.childForFieldName('parameter')
                )?.text === name ||
                (n.childForFieldName('name') ?? n.childForFieldName('pattern'))
                  ?.descendantsOfType(['identifier', 'shorthand_property_identifier_pattern'])
                  .some((binding) => binding.text === name),
            ),
          )
      )
        return false;
      for (const element of app.descendantsOfType('jsx_element')) {
        let parent = element.parent;
        while (
          parent &&
          parent.id !== app.id &&
          ![
            'function_declaration',
            'function_expression',
            'arrow_function',
            'binary_expression',
            'ternary_expression',
            'if_statement',
            'switch_statement',
            'for_statement',
            'while_statement',
          ].includes(parent.type)
        )
          parent = parent.parent;
        if (parent?.id !== app.id) continue;
        const opening = element.namedChildren[0];
        if (
          !routers.has(opening?.childForFieldName('name')?.text ?? '') ||
          opening?.namedChildren.some(
            (n) =>
              n.type === 'jsx_expression' ||
              (n.type === 'jsx_attribute' && n.namedChildren[0]?.text !== 'root'),
          )
        )
          continue;
        if (
          element.namedChildren.some(
            (child) =>
              child.type === 'jsx_self_closing_element' &&
              routes.has(child.childForFieldName('name')?.text ?? '') &&
              child.namedChildren.length === 1,
          )
        )
          return true;
      }
    } finally {
      tree.delete();
    }
  }
  return false;
}

type Target = { name: string; line: number; column: number; endLine: number; endColumn: number };
type Module = { exports: Map<string, Target | null> };
function moduleExports(file: string, content: string): Module {
  const result: Module = { exports: new Map() };
  const tree = getParser(detectLanguage(file)!)?.parse(content);
  if (!tree) return result;
  try {
    const bindings = new Map<string, SyntaxNode>();
    const declaration = (node: SyntaxNode) =>
      node.type === 'export_statement' ? node.childForFieldName('declaration') : node;
    for (const statement of tree.rootNode.namedChildren) {
      const node = declaration(statement);
      if (node?.type === 'function_declaration' && node.childForFieldName('name'))
        bindings.set(node.childForFieldName('name')!.text, node);
      if (node?.type === 'lexical_declaration' && node.children.some((n) => n.type === 'const'))
        for (const variable of node.namedChildren) {
          const name = variable.childForFieldName('name');
          const value = variable.childForFieldName('value');
          if (
            name?.type === 'identifier' &&
            ['arrow_function', 'function_expression'].includes(value?.type ?? '')
          )
            bindings.set(name.text, variable);
        }
    }
    for (const mutation of tree.rootNode.descendantsOfType([
      'assignment_expression',
      'augmented_assignment_expression',
      'update_expression',
    ])) {
      const assigned = mutation.childForFieldName('left') ?? mutation.childForFieldName('argument');
      if (assigned?.type === 'identifier') bindings.delete(assigned.text);
      for (const name of assigned?.descendantsOfType([
        'identifier',
        'shorthand_property_identifier_pattern',
      ]) ?? [])
        bindings.delete(name.text);
    }
    const target = (name: string): Target | null => {
      const binding = bindings.get(name);
      const node =
        binding?.type === 'variable_declarator' ? binding.childForFieldName('value') : binding;
      return node
        ? {
            name,
            line: node.startPosition.row + 1,
            column: node.startPosition.column,
            endLine: node.endPosition.row + 1,
            endColumn: node.endPosition.column,
          }
        : null;
    };
    for (const statement of tree.rootNode.namedChildren) {
      if (
        statement.type !== 'export_statement' ||
        statement.children.some((n) => n.type === 'type')
      )
        continue;
      const node = declaration(statement);
      if (statement.children.some((n) => n.type === 'default')) {
        const value = statement.childForFieldName('value');
        const name =
          node?.childForFieldName('name')?.text ?? (value?.type === 'identifier' ? value.text : '');
        result.exports.set('default', target(name));
      } else if (node?.type === 'function_declaration') {
        const name = node.childForFieldName('name')?.text;
        if (name) result.exports.set(name, target(name));
      } else if (node?.type === 'lexical_declaration') {
        for (const variable of node.namedChildren) {
          const name = variable.childForFieldName('name')?.text;
          if (name) result.exports.set(name, target(name));
        }
      }
      for (const spec of statement.descendantsOfType('export_specifier')) {
        if (spec.children.some((n) => n.type === 'type')) continue;
        const name = spec.childForFieldName('name')?.text ?? '';
        result.exports.set(
          spec.childForFieldName('alias')?.text ?? name,
          statement.childForFieldName('source') || spec.children.some((n) => n.type === 'type')
            ? null
            : target(name),
        );
      }
    }
    return result;
  } finally {
    tree.delete();
  }
}

const states = new WeakMap<
  ResolutionContext,
  { active: boolean; pages: string[]; overrides: string[] }
>();
function project(context: ResolutionContext) {
  const cached = states.get(context);
  if (cached) return cached;
  const state = { active: plugin(context), pages: [] as string[], overrides: [] as string[] };
  if (state.active && fileRoutes(context))
    for (const file of context.getAllFiles().filter(isSolidStartRoute)) {
      const content = context.readFile(file);
      if (content !== null) {
        const exported = moduleExports(file, content).exports;
        if (exported.has('default')) {
          state.pages.push(file);
          if (exported.has('route')) state.overrides.push(rawPath(file));
        }
      }
    }
  states.set(context, state);
  return state;
}

export function extractSolidStartRoutes(
  filePath: string,
  content: string,
  context: ResolutionContext,
): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!isSolidStartRoute(filePath)) return result;
  const state = project(context);
  if (!state.active) return result;
  const exported = moduleExports(filePath, content).exports;
  const raw = rawPath(filePath);
  const path =
    (
      '/' +
      raw
        .replace(/index$/, '')
        .replace(/\[([^/]+)\]/g, (_, name: string) =>
          name.startsWith('...')
            ? '*' + name.slice(3)
            : name.startsWith('[') && name.endsWith(']')
              ? ':' + name.slice(1, -1) + '?'
              : ':' + name,
        )
        .replace(/\([^)]*\)/g, '')
    )
      .replace(/\/+/g, '/')
      .replace(/\/$/, '') || '/';
  const add = (method: string, target: Target | null | undefined) => {
    if (!target) return;
    const name = method ? `${method} ${path}` : path;
    const id = `route:solid-start:${filePath}:${method || 'page'}`;
    const language = detectLanguage(filePath)!;
    result.nodes.push({
      id,
      kind: 'route',
      name,
      qualifiedName: `${filePath}::${name}`,
      filePath,
      language,
      startLine: target.line,
      startColumn: target.column,
      endLine: target.endLine,
      endColumn: target.endColumn,
      updatedAt: Date.now(),
    });
    result.references.push({
      fromNodeId: id,
      referenceName: `solid-start-target:${target.name}`,
      referenceKind: 'references',
      filePath,
      language,
      line: target.line,
      column: target.column,
    });
  };
  if (
    !exported.has('route') &&
    !state.overrides.some((parent) => raw.startsWith(parent + '/')) &&
    state.pages.includes(filePath) &&
    !state.pages.some((file) => rawPath(file).startsWith(raw + '/'))
  )
    add('', exported.get('default'));
  if (
    !raw.includes('[[') &&
    METHODS.some((method) => method !== 'OPTIONS' && exported.has(method))
  ) {
    for (const method of METHODS) add(method, exported.get(method));
    if (exported.has('GET') && !exported.has('HEAD')) add('HEAD', exported.get('GET'));
  }
  return result;
}

export const solidStartResolver: FrameworkResolver = {
  name: 'solid-start',
  languages: ['typescript', 'javascript'],
  detect: (context) => dependsOn(context, '@solidjs/start'),
  claimsReference: (name) => name.startsWith('solid-start-target:'),
  resolve(ref, context) {
    if (
      !ref.fromNodeId.startsWith('route:solid-start:') ||
      !ref.referenceName.startsWith('solid-start-target:')
    )
      return null;
    const candidates = context
      .getNodesInFile(ref.filePath)
      .filter(
        (n) =>
          n.name === ref.referenceName.slice('solid-start-target:'.length) &&
          ['function', 'component'].includes(n.kind) &&
          n.startLine === ref.line,
      );
    return candidates.length === 1
      ? { original: ref, targetNodeId: candidates[0]!.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
