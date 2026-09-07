import type { Node as SyntaxNode, Tree } from 'web-tree-sitter';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { resolveImportPath } from '../import-resolver';
import { dependsOn } from './package-deps';

const FUNCTIONS = new Set([
  'arrow_function',
  'function_expression',
  'function_declaration',
  'method_definition',
]);
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
type Location = { file: string; node: SyntaxNode };
type Module = {
  tree: Tree;
  locals: Map<string, SyntaxNode>;
  imports: Map<string, { source: string; name: string }>;
  exports: Map<string, string>;
  mutated: Set<string>;
};

export const isAngularRegistrationFile = (content: string): boolean =>
  content.includes('@angular/router') && /\b(?:provideRouter|forRoot)\b/.test(content);

/** Resolves source bindings without depending on the order files reach the database. */
class AngularSource {
  private modules = new Map<string, Module | null>();
  constructor(
    private context: ResolutionContext,
    private owner: string,
    private content: string,
  ) {}
  close(): void {
    for (const module of this.modules.values()) module?.tree.delete();
  }
  module(file: string): Module | null {
    if (this.modules.has(file)) return this.modules.get(file)!;
    const content = file === this.owner ? this.content : this.context.readFile(file);
    const parser = getParser(detectLanguage(file)!);
    const tree = content === null ? null : parser?.parse(content);
    if (!tree) {
      this.modules.set(file, null);
      return null;
    }
    const module: Module = {
      tree,
      locals: new Map(),
      imports: new Map(),
      exports: new Map(),
      mutated: new Set(),
    };
    this.modules.set(file, module);
    for (const statement of tree.rootNode.namedChildren) {
      if (
        statement.type === 'import_statement' &&
        !statement.children.some((n) => n.type === 'type')
      ) {
        const source = literal(statement.childForFieldName('source'));
        if (source === null) continue;
        const clause = statement.namedChildren.find((n) => n.type === 'import_clause');
        for (const spec of clause?.namedChildren ?? []) {
          if (spec.type === 'identifier')
            module.imports.set(spec.text, { source, name: 'default' });
          for (const item of spec.type === 'named_imports' ? spec.namedChildren : []) {
            if (item.type !== 'import_specifier' || item.children.some((n) => n.type === 'type'))
              continue;
            const name = item.childForFieldName('name')?.text;
            if (name)
              module.imports.set(item.childForFieldName('alias')?.text ?? name, { source, name });
          }
        }
      }
      const exported = statement.type === 'export_statement';
      const declaration = exported ? statement.childForFieldName('declaration') : statement;
      if (
        declaration?.type === 'lexical_declaration' &&
        declaration.children.some((n) => n.type === 'const')
      ) {
        for (const item of declaration.namedChildren) {
          const name = item.childForFieldName('name');
          const value = item.childForFieldName('value');
          if (name?.type === 'identifier' && value) {
            module.locals.set(name.text, value);
            if (exported) module.exports.set(name.text, name.text);
          }
        }
      } else if (
        declaration &&
        ['class_declaration', 'abstract_class_declaration'].includes(declaration.type)
      ) {
        const name = declaration.childForFieldName('name')?.text;
        if (name) {
          module.locals.set(name, declaration);
          if (exported)
            module.exports.set(
              statement.children.some((n) => n.type === 'default') ? 'default' : name,
              name,
            );
        }
      }
      if (
        exported &&
        !statement.childForFieldName('source') &&
        !statement.children.some((n) => n.type === 'type')
      ) {
        const value = statement.childForFieldName('value');
        if (statement.children.some((n) => n.type === 'default') && value?.type === 'identifier')
          module.exports.set('default', value.text);
        if (statement.children.some((n) => n.type === 'default') && value?.type === 'array') {
          module.locals.set('default', value);
          module.exports.set('default', 'default');
        }
        for (const spec of statement.descendantsOfType('export_specifier')) {
          const name = spec.childForFieldName('name')?.text;
          if (name && !spec.children.some((n) => n.type === 'type'))
            module.exports.set(spec.childForFieldName('alias')?.text ?? name, name);
        }
      }
    }
    for (const expression of tree.rootNode.descendantsOfType([
      'assignment_expression',
      'augmented_assignment_expression',
      'update_expression',
      'call_expression',
    ])) {
      let receiver =
        expression.type === 'call_expression'
          ? expression.childForFieldName('function')
          : (expression.childForFieldName('left') ?? expression.childForFieldName('argument'));
      if (
        expression.type === 'call_expression' &&
        !['member_expression', 'subscript_expression'].includes(receiver?.type ?? '')
      )
        continue;
      while (receiver && ['member_expression', 'subscript_expression'].includes(receiver.type))
        receiver = receiver.childForFieldName('object');
      if (
        receiver?.type === 'identifier' &&
        module.locals.get(receiver.text)?.type !== 'class_declaration'
      )
        module.mutated.add(receiver.text);
    }
    let changed = true;
    while (changed) {
      changed = false;
      for (const [name, raw] of module.locals) {
        const alias = unwrap(raw);
        if (
          alias?.type !== 'identifier' ||
          (!module.mutated.has(name) && !module.mutated.has(alias.text))
        )
          continue;
        for (const binding of [name, alias.text])
          if (!module.mutated.has(binding)) {
            module.mutated.add(binding);
            changed = true;
          }
      }
    }
    return module;
  }
  imported(file: string, source: string, name: string, depth: number): Location | null {
    if (depth > 32 || !source.startsWith('.')) return null;
    const target = resolveImportPath(source, file, detectLanguage(file)!, this.context);
    const local = target && this.module(target)?.exports.get(name);
    return target && local ? this.binding(target, local, depth + 1) : null;
  }
  binding(file: string, name: string, depth = 0): Location | null {
    if (depth > 32) return null;
    const module = this.module(file);
    if (module?.mutated.has(name)) return null;
    const local = module?.locals.get(name);
    if (local) return this.value({ file, node: local }, depth + 1);
    const imported = module?.imports.get(name);
    return imported ? this.imported(file, imported.source, imported.name, depth + 1) : null;
  }
  value(location: Location, depth = 0): Location | null {
    if (depth > 32) return null;
    const node = unwrap(location.node);
    if (!node) return null;
    return node.type === 'identifier'
      ? this.binding(location.file, node.text, depth + 1)
      : { file: location.file, node };
  }
  helper(file: string, node: SyntaxNode | null, name: string): boolean {
    const imported = node?.type === 'identifier' && this.module(file)?.imports.get(node.text);
    return !!imported && imported.source === '@angular/router' && imported.name === name;
  }
  lazy(file: string, raw: SyntaxNode): Location | null {
    const fn = unwrap(raw);
    if (!fn || !['arrow_function', 'function_expression'].includes(fn.type)) return null;
    let body = unwrap(fn.childForFieldName('body'));
    if (body?.type === 'statement_block') {
      const statements = body.namedChildren.filter((n) => n.type !== 'comment');
      body =
        statements.length === 1 && statements[0]?.type === 'return_statement'
          ? unwrap(statements[0].namedChildren[0])
          : null;
    }
    let name = 'default';
    if (
      body?.type === 'call_expression' &&
      body.childForFieldName('function')?.type === 'member_expression'
    ) {
      const member = body.childForFieldName('function')!;
      if (member.childForFieldName('property')?.text !== 'then') return null;
      const callback = body.childForFieldName('arguments')?.namedChildren[0];
      const parameter =
        callback?.childForFieldName('parameter') ??
        callback?.childForFieldName('parameters')?.namedChildren[0];
      const selected = unwrap(callback?.childForFieldName('body'));
      if (
        callback?.type !== 'arrow_function' ||
        selected?.type !== 'member_expression' ||
        parameter?.type !== 'identifier' ||
        selected.childForFieldName('object')?.text !== parameter.text
      )
        return null;
      name = selected.childForFieldName('property')?.text ?? '';
      body = unwrap(member.childForFieldName('object'));
    }
    if (body?.type !== 'call_expression' || body.childForFieldName('function')?.type !== 'import')
      return null;
    const source = literal(body.childForFieldName('arguments')?.namedChildren[0]);
    return source === null ? null : this.imported(file, source, name, 0);
  }
}

function properties(node: SyntaxNode): Map<string, SyntaxNode> | null {
  if (node.type !== 'object') return null;
  const found = new Map<string, SyntaxNode>();
  for (const item of node.namedChildren) {
    if (item.type === 'comment') continue;
    const key = item.childForFieldName('key');
    const name =
      item.type === 'shorthand_property_identifier'
        ? item.text
        : key?.type === 'property_identifier'
          ? key.text
          : literal(key);
    const value =
      item.type === 'shorthand_property_identifier' ? item : item.childForFieldName('value');
    if (!name || !value || found.has(name)) return null;
    found.set(name, value);
  }
  return found;
}

export function extractAngularRoutes(
  filePath: string,
  content: string,
  context: ResolutionContext,
): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!isAngularRegistrationFile(content)) return result;
  const source = new AngularSource(context, filePath, content);
  try {
    const module = source.module(filePath);
    if (!module) return result;
    const visit = (raw: Location, prefix: string, site: SyntaxNode, depth = 0): void => {
      if (depth > 32) return;
      const location = source.value(raw);
      if (!location) return;
      const { file, node } = location;
      if (node.type === 'array') {
        for (const child of node.namedChildren)
          if (child.type !== 'comment' && child.type !== 'spread_element')
            visit({ file, node: child }, prefix, site, depth + 1);
        return;
      }
      if (node.type === 'class_declaration') {
        const declaration = node.parent?.type === 'export_statement' ? node.parent : node;
        for (const decorator of declaration.namedChildren.filter((n) => n.type === 'decorator')) {
          const invocation = decorator.namedChildren[0];
          const imported = invocation?.childForFieldName('function');
          const binding =
            imported?.type === 'identifier' && source.module(file)?.imports.get(imported.text);
          if (!binding || binding.source !== '@angular/core' || binding.name !== 'NgModule')
            continue;
          const metadata = invocation?.childForFieldName('arguments')?.namedChildren[0];
          const imports = metadata && properties(metadata)?.get('imports');
          if (imports?.type !== 'array') continue;
          for (const call of imports.namedChildren) {
            if (call.type !== 'call_expression') continue;
            const fn = call.childForFieldName('function');
            if (
              fn?.type === 'member_expression' &&
              source.helper(file, fn.childForFieldName('object'), 'RouterModule') &&
              fn.childForFieldName('property')?.text === 'forChild'
            ) {
              const routes = call.childForFieldName('arguments')?.namedChildren[0];
              const table = routes && source.value({ file, node: routes });
              if (table?.node.type === 'array') visit(table, prefix, site, depth + 1);
            }
          }
        }
        return;
      }
      const fields = properties(node);
      if (!fields || ['redirectTo', 'matcher', 'outlet'].some((key) => fields.has(key))) return;
      const segment = literal(fields.get('path'));
      if (segment === null) return;
      const routePath =
        '/' +
        [prefix, segment === '**' ? '*' : segment].join('/').split('/').filter(Boolean).join('/');
      const children = fields.get('children');
      const lazyChildren = fields.get('loadChildren');
      const childStart = result.nodes.length;
      const table = children && source.value({ file, node: children });
      if (children && table?.node.type !== 'array') return;
      if (table?.node.type === 'array') visit(table, routePath, site, depth + 1);
      if (lazyChildren) {
        const loaded = source.lazy(file, lazyChildren);
        if (!loaded || !['array', 'class_declaration'].includes(loaded.node.type)) return;
        visit(loaded, routePath, site, depth + 1);
      }
      if (result.nodes.slice(childStart).some((n) => n.name === routePath)) return;
      const component = fields.get('component');
      const lazyComponent = fields.get('loadComponent');
      const target = component
        ? source.binding(file, component.text)
        : lazyComponent
          ? source.lazy(file, lazyComponent)
          : null;
      if (!target || target.node.type !== 'class_declaration') return;
      const name = target.node.childForFieldName('name')?.text;
      if (!name) return;
      const id = `route:angular:${filePath}:${site.startIndex}:${file}:${node.startIndex}:${routePath}`;
      if (result.nodes.some((n) => n.id === id)) return;
      result.nodes.push({
        id,
        kind: 'route',
        name: routePath,
        qualifiedName: `${filePath}::${routePath}`,
        filePath,
        language: detectLanguage(filePath)!,
        startLine: site.startPosition.row + 1,
        endLine: site.endPosition.row + 1,
        startColumn: site.startPosition.column,
        endColumn: site.endPosition.column,
        updatedAt: Date.now(),
      });
      result.references.push({
        fromNodeId: id,
        referenceName: 'angular-component:' + JSON.stringify([target.file, name]),
        referenceKind: 'references',
        filePath,
        language: detectLanguage(filePath)!,
        line: site.startPosition.row + 1,
        column: site.startPosition.column,
      });
    };
    const registrations = (node: SyntaxNode): void => {
      if (
        FUNCTIONS.has(node.type) ||
        [
          'statement_block',
          'binary_expression',
          'ternary_expression',
          'if_statement',
          'switch_statement',
          'for_statement',
          'while_statement',
        ].includes(node.type)
      )
        return;
      if (node.type === 'call_expression') {
        const fn = node.childForFieldName('function');
        if (
          source.helper(filePath, fn, 'provideRouter') ||
          (fn?.type === 'member_expression' &&
            source.helper(filePath, fn.childForFieldName('object'), 'RouterModule') &&
            fn.childForFieldName('property')?.text === 'forRoot')
        ) {
          const routes = node.childForFieldName('arguments')?.namedChildren[0];
          const table = routes && source.value({ file: filePath, node: routes });
          if (table?.node.type === 'array') visit(table, '', node);
          return;
        }
      }
      for (const child of node.namedChildren) registrations(child);
    };
    registrations(module.tree.rootNode);
    return result;
  } finally {
    source.close();
  }
}

export const angularResolver: FrameworkResolver = {
  name: 'angular',
  languages: ['typescript', 'javascript'],
  detect: (context) => dependsOn(context, '@angular/router'),
  claimsReference: (name) => name.startsWith('angular-component:'),
  resolve(ref, context) {
    if (
      !ref.fromNodeId.startsWith('route:angular:') ||
      !ref.referenceName.startsWith('angular-component:')
    )
      return null;
    let target: unknown;
    try {
      target = JSON.parse(ref.referenceName.slice('angular-component:'.length));
    } catch {
      return null;
    }
    if (
      !Array.isArray(target) ||
      target.length !== 2 ||
      !target.every((v) => typeof v === 'string')
    )
      return null;
    const candidates = context
      .getNodesInFile(target[0]!)
      .filter((n) => n.name === target[1] && ['class', 'component'].includes(n.kind));
    return candidates.length === 1
      ? { original: ref, targetNodeId: candidates[0]!.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
