import type { Node as SyntaxNode } from 'web-tree-sitter';
import type { FrameworkResolver, FrameworkExtractionResult } from '../types';
import { detectLanguage, getParser } from '../../extraction/grammars';
import { resolveImportPath, resolveViaImport } from '../import-resolver';
import { dependsOn } from './package-deps';

const unwrap = (raw: SyntaxNode | null | undefined): SyntaxNode | null => {
  let node = raw ?? null;
  while (
    node &&
    [
      'parenthesized_expression',
      'as_expression',
      'satisfies_expression',
      'jsx_expression',
    ].includes(node.type)
  )
    node = node.namedChildren[0] ?? null;
  return node;
};
const literal = (node: SyntaxNode | null | undefined): string | null =>
  node?.type === 'string' && !node.text.includes('\\') ? node.text.slice(1, -1) : null;
const join = (base: string, path: string): string =>
  ('/' + base.replace(/\*.*$/, '').replace(/^\/+|\/+$/g, '') + '/' + path.replace(/^\/+|\/+$/g, ''))
    .replace(/\/+/g, '/')
    .replace(/\/$/, '') || '/';

/** Only children of an imported Router are route declarations. */
export function extractSolidRoutes(filePath: string, content: string): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!content.includes('@solidjs/router')) return result;
  const language = detectLanguage(filePath)!;
  const parser = getParser(language);
  if (!parser) throw new Error(`Solid Router extraction requires the ${language} grammar`);
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
          const name = spec.childForFieldName('name')?.text;
          if (
            name &&
            ((source === '@solidjs/router' && ['Route', 'Router'].includes(name)) ||
              (source === 'solid-js' && name === 'lazy')) &&
            !spec.children.some((n) => n.type === 'type')
          )
            helpers.set(spec.childForFieldName('alias')?.text ?? name, name);
        }
      }
      const declaration =
        statement.type === 'export_statement'
          ? statement.childForFieldName('declaration')
          : statement;
      if (
        declaration?.type === 'lexical_declaration' &&
        declaration.children.some((n) => n.type === 'const')
      )
        for (const variable of declaration.namedChildren) {
          const name = variable.childForFieldName('name');
          const value = variable.childForFieldName('value');
          if (name?.type === 'identifier' && value) bindings.set(name.text, value);
        }
    }
    const mutated = new Set<string>();
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
      if (receiver?.type === 'identifier') mutated.add(receiver.text);
    }
    for (let changed = true; changed;) {
      changed = false;
      for (const [name, raw] of bindings) {
        const value = unwrap(raw);
        if (value?.type === 'identifier' && (mutated.has(name) || mutated.has(value.text)))
          for (const alias of [name, value.text])
            if (!mutated.has(alias)) {
              mutated.add(alias);
              changed = true;
            }
      }
    }
    const shadowed = (name: string, site: SyntaxNode): boolean => {
      for (let scope = site.parent; scope && scope.type !== 'program'; scope = scope.parent) {
        const params =
          scope.childForFieldName('parameters') ?? scope.childForFieldName('parameter');
        if (
          params &&
          [
            params,
            ...params.descendantsOfType(['identifier', 'shorthand_property_identifier_pattern']),
          ].some((n) => n.text === name)
        )
          return true;
        if (scope.type === 'statement_block')
          for (const child of scope.namedChildren) {
            if (
              ['function_declaration', 'class_declaration'].includes(child.type) &&
              child.childForFieldName('name')?.text === name
            )
              return true;
            if (
              ['lexical_declaration', 'variable_declaration'].includes(child.type) &&
              child.descendantsOfType('variable_declarator').some((n) => {
                const pattern = n.childForFieldName('name');
                return (
                  pattern &&
                  [
                    pattern,
                    ...pattern.descendantsOfType([
                      'identifier',
                      'shorthand_property_identifier_pattern',
                    ]),
                  ].some((binding) => binding.text === name)
                );
              })
            )
              return true;
          }
      }
      return false;
    };
    const resolve = (
      raw: SyntaxNode | null | undefined,
      seen = new Set<string>(),
    ): SyntaxNode | null => {
      const node = unwrap(raw);
      if (node?.type !== 'identifier' || !bindings.has(node.text)) return node;
      if (seen.has(node.text) || mutated.has(node.text) || shadowed(node.text, node)) return null;
      seen.add(node.text);
      return resolve(bindings.get(node.text), seen);
    };
    const fields = (node: SyntaxNode): Map<string, SyntaxNode> | null => {
      const map = new Map<string, SyntaxNode>();
      for (const child of node.namedChildren) {
        if (child.type === 'comment') continue;
        const key = child.childForFieldName('key');
        const name = key?.type === 'property_identifier' ? key.text : literal(key);
        const value = child.childForFieldName('value');
        if (!name || !value || map.has(name)) return null;
        map.set(name, value);
      }
      return map;
    };
    const tag = (node: SyntaxNode): SyntaxNode | undefined =>
      node.type === 'jsx_element'
        ? node.namedChildren.find((n) => n.type === 'jsx_opening_element')
        : node.type === 'jsx_self_closing_element'
          ? node
          : undefined;
    const attributes = (opening: SyntaxNode): Map<string, SyntaxNode> | null => {
      const map = new Map<string, SyntaxNode>();
      for (const child of opening.namedChildren.slice(1)) {
        if (child.type !== 'jsx_attribute') return null;
        const name = child.namedChildren[0]?.text;
        const value = child.namedChildren[1];
        if (!name || !value || map.has(name)) return null;
        map.set(name, value);
      }
      return map;
    };
    const children = (node: SyntaxNode): SyntaxNode[] =>
      node.type === 'jsx_self_closing_element'
        ? []
        : node.namedChildren.filter(
            (n) =>
              !['jsx_opening_element', 'jsx_closing_element', 'jsx_text', 'comment'].includes(
                n.type,
              ),
          );
    const visit = (raw: SyntaxNode | null | undefined, base: string, depth = 0): void => {
      if (depth > 32) return;
      const node = resolve(raw);
      if (!node) return;
      if (node.type === 'array' || node.type === 'jsx_fragment') {
        for (const child of node.type === 'array' ? node.namedChildren : children(node))
          visit(child, base, depth + 1);
        return;
      }
      const opening = tag(node);
      const name = opening?.childForFieldName('name')?.text;
      if (opening && (!name || helpers.get(name) !== 'Route' || shadowed(name, opening))) return;
      const props = opening ? attributes(opening) : node.type === 'object' ? fields(node) : null;
      if (!props) return;
      const pathNode = props.has('path') ? resolve(props.get('path')) : null;
      const paths = !props.has('path')
        ? ['']
        : pathNode?.type === 'array'
          ? pathNode.namedChildren.map(literal)
          : [literal(pathNode)];
      if (paths.some((p) => p === null)) return;
      const descendants = opening
        ? children(node)
        : props.has('children')
          ? [props.get('children')!]
          : [];
      const nested = descendants.filter((n) => {
        const value = resolve(n);
        return !(value?.type === 'array' && value.namedChildren.length === 0);
      });
      for (const part of paths) {
        const routePath = join(base, part!);
        if (nested.length) {
          for (const child of nested) visit(child, routePath, depth + 1);
          continue;
        }
        const component = unwrap(props.get('component'));
        if (!component) continue;
        let referenceName: string | null =
          component.type === 'identifier' &&
          !shadowed(component.text, component) &&
          !mutated.has(component.text)
            ? 'solid-component:' + component.text
            : null;
        const value = resolve(component);
        if (value?.type === 'call_expression') {
          referenceName = null;
          const fn = value.childForFieldName('function');
          const args = value.childForFieldName('arguments')?.namedChildren;
          const callback = args?.length === 1 ? args[0] : null;
          const body = unwrap(callback?.childForFieldName('body'));
          const target =
            body?.type === 'call_expression' &&
            body.childForFieldName('function')?.type === 'import'
              ? body.childForFieldName('arguments')?.namedChildren
              : null;
          const source = target?.length === 1 ? literal(target[0]) : null;
          if (
            fn?.type === 'identifier' &&
            helpers.get(fn.text) === 'lazy' &&
            !shadowed(fn.text, fn) &&
            callback?.type === 'arrow_function' &&
            source?.startsWith('.')
          )
            referenceName = 'solid-lazy:' + source;
        }
        if (!referenceName) continue;
        const id = `route:solid:${filePath}:${node.startIndex}:${routePath}`;
        if (result.nodes.some((n) => n.id === id)) continue;
        result.nodes.push({
          id,
          kind: 'route',
          name: routePath,
          qualifiedName: `${filePath}::${routePath}`,
          filePath,
          language,
          startLine: node.startPosition.row + 1,
          endLine: node.endPosition.row + 1,
          startColumn: node.startPosition.column,
          endColumn: node.endPosition.column,
          updatedAt: Date.now(),
        });
        result.references.push({
          fromNodeId: id,
          referenceName,
          referenceKind: 'references',
          filePath,
          language,
          line: component.startPosition.row + 1,
          column: component.startPosition.column,
        });
      }
    };
    for (const node of tree.rootNode.descendantsOfType('jsx_element')) {
      const opening = tag(node)!;
      const name = opening.childForFieldName('name')?.text;
      if (!name || helpers.get(name) !== 'Router' || shadowed(name, opening)) continue;
      const props = attributes(opening);
      if (!props) continue;
      const base = props.has('base') ? literal(resolve(props.get('base'))) : '';
      if (base === null) continue;
      for (const child of children(node)) visit(child, base);
    }
    return result;
  } finally {
    tree.delete();
  }
}

export const solidRouterResolver: FrameworkResolver = {
  name: 'solid-router',
  languages: ['typescript', 'javascript', 'tsx', 'jsx'],
  detect: (context) => dependsOn(context, '@solidjs/router'),
  extract: extractSolidRoutes,
  claimsReference: (name) => name.startsWith('solid-component:') || name.startsWith('solid-lazy:'),
  resolve(ref, context) {
    if (ref.referenceName.startsWith('solid-lazy:')) {
      const file = resolveImportPath(
        ref.referenceName.slice('solid-lazy:'.length),
        ref.filePath,
        ref.language,
        context,
      );
      const content = file ? context.readFile(file) : null;
      const tree =
        file && content !== null ? getParser(detectLanguage(file))?.parse(content) : null;
      if (!tree || !file) return null;
      try {
        const exported = tree.rootNode.namedChildren.find(
          (n) => n.type === 'export_statement' && n.children.some((c) => c.type === 'default'),
        );
        const declaration = exported?.childForFieldName('declaration');
        const value = exported?.childForFieldName('value');
        const name =
          declaration?.childForFieldName('name')?.text ??
          (value?.type === 'identifier' ? value.text : null);
        if (!name) return null;
        const targets = context
          .getNodesInFile(file)
          .filter((n) => ['function', 'component'].includes(n.kind) && n.name === name);
        return targets.length === 1
          ? { original: ref, targetNodeId: targets[0]!.id, confidence: 1, resolvedBy: 'framework' }
          : null;
      } finally {
        tree.delete();
      }
    }
    if (!ref.referenceName.startsWith('solid-component:')) return null;
    const name = ref.referenceName.slice('solid-component:'.length);
    const imported = resolveViaImport({ ...ref, referenceName: name }, context);
    if (imported) return { ...imported, original: ref };
    const targets = context
      .getNodesInFile(ref.filePath)
      .filter((n) => n.name === name && ['function', 'component'].includes(n.kind));
    return targets.length === 1
      ? { original: ref, targetNodeId: targets[0]!.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
