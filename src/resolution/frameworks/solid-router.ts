import type { TreeNode as SyntaxNode } from '../../extraction/parse-tree';
import type { Node } from '../../types';
import type { FrameworkResolver, FrameworkExtractionResult, ResolutionContext } from '../types';
import { detectLanguage } from '../../extraction/grammars';
import { parseSourceTreeSync } from '../../extraction/parse-tree';
import { resolveImportPath } from '../import-resolver';
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
/**
 * Marks, in `qualifiedName`, a route from an exported `RouteDefinition[]`
 * table: `solid-table:<export names>:<path in the table>`. Where another file
 * registers the table decides its prefix, which `postExtract` applies.
 */
const TABLE_MARKER = 'solid-table:';
/** A relative or project-alias module (`./page`, `@/routes/home`, `~/page`), not a package. */
const LOCAL_MODULE = /^(?:\.|\/|[@~#]\/)/;

/** Local name → the names a module exports it under (`default` included). */
const exportedBindings = (program: SyntaxNode): Map<string, string[]> => {
  const exported = new Map<string, string[]>();
  const exportAs = (local: string, name: string) =>
    exported.set(local, [...(exported.get(local) ?? []), name]);
  for (const statement of program.namedChildren) {
    if (statement.type !== 'export_statement' || statement.childForFieldName('source')) continue;
    const value = statement.childForFieldName('value');
    if (statement.children.some((n) => n.type === 'default') && value?.type === 'identifier')
      exportAs(value.text, 'default');
    for (const spec of statement.descendantsOfType('export_specifier')) {
      const name = spec.childForFieldName('name')?.text;
      if (name) exportAs(name, spec.childForFieldName('alias')?.text ?? name);
    }
    for (const d of statement.childForFieldName('declaration')?.namedChildren ?? []) {
      const name = d.childForFieldName('name');
      if (d.type === 'variable_declarator' && name?.type === 'identifier') exportAs(name.text, name.text);
    }
  }
  return exported;
};
/** Route/Router names this file imports from `@solidjs/router`, keyed by local name. */
const routerHelpers = (program: SyntaxNode): Map<string, string> => {
  const helpers = new Map<string, string>();
  for (const statement of program.namedChildren)
    if (
      statement.type === 'import_statement' &&
      !statement.children.some((n) => n.type === 'type') &&
      literal(statement.childForFieldName('source')) === '@solidjs/router'
    )
      for (const spec of statement.descendantsOfType('import_specifier')) {
        const name = spec.childForFieldName('name')?.text;
        if (name && ['Route', 'Router'].includes(name) && !spec.children.some((n) => n.type === 'type'))
          helpers.set(spec.childForFieldName('alias')?.text ?? name, name);
      }
  return helpers;
};

/** Only children of an imported Router are route declarations. */
export function extractSolidRoutes(filePath: string, content: string): FrameworkExtractionResult {
  const result: FrameworkExtractionResult = { nodes: [], references: [] };
  if (!content.includes('@solidjs/router')) return result;
  const language = detectLanguage(filePath)!;
  const tree = parseSourceTreeSync(content, language);
  if (!tree) return result;
  try {
    const helpers = new Map<string, string>();
    const bindings = new Map<string, SyntaxNode>();
    const routeTypes = new Set<string>();
    for (const statement of tree.rootNode.namedChildren) {
      if (statement.type === 'import_statement' && literal(statement.childForFieldName('source')) === '@solidjs/router')
        for (const spec of statement.descendantsOfType('import_specifier'))
          if (spec.childForFieldName('name')?.text === 'RouteDefinition')
            routeTypes.add(spec.childForFieldName('alias')?.text ?? 'RouteDefinition');
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
    const registered = new Set<number>();
    let table: string | null = null;
    const visit = (raw: SyntaxNode | null | undefined, base: string, depth = 0): void => {
      if (depth > 32) return;
      const node = resolve(raw);
      if (!node) return;
      if (node.type === 'array') registered.add(node.startIndex);
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
          let body = unwrap(callback?.childForFieldName('body'));
          if (body?.type === 'await_expression') body = unwrap(body.namedChildren[0]);
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
            source &&
            LOCAL_MODULE.test(source)
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
          qualifiedName: `${filePath}::${table ? `${TABLE_MARKER}${table}:` : ''}${routePath}`,
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
    // An exported table typed as RouteDefinition[] is read here even though
    // another file registers it; one this file registers was read above.
    const typed = (type: SyntaxNode | null | undefined): boolean => {
      const text = type?.text.replace(/^:\s*/, '').replace(/\s+/g, '') ?? '';
      return [...routeTypes].some((t) => text === `${t}[]` || text === `Array<${t}>`);
    };
    for (const [local, names] of exportedBindings(tree.rootNode)) {
      const raw = bindings.get(local);
      if (!raw || mutated.has(local)) continue;
      const declarator = raw.parent;
      const value = unwrap(raw);
      if (
        value?.type !== 'array' ||
        registered.has(value.startIndex) ||
        !(
          typed(declarator?.childForFieldName('type')) ||
          (['satisfies_expression', 'as_expression'].includes(raw.type) && typed(raw.namedChildren[1]))
        )
      )
        continue;
      table = names.join(',');
      visit(value, '');
      table = null;
    }
    return result;
  } finally {
    tree.delete();
  }
}

/**
 * Prefixes each exported route table's routes by where other files register
 * it: `<Router base>{routes}</Router>`, a `<Route path>` child, or
 * `children: table` in a route object, through local const tables and tables
 * that are themselves registered elsewhere. A table registered under one
 * prefix takes it; none or several leave the paths as written in the table.
 */
function solidTableRoutes(context: ResolutionContext): Node[] {
  const marker = '::' + TABLE_MARKER;
  const marked: { node: Node; table: string; path: string }[] = [];
  for (const node of context.iterateNodesByKind?.('route') ?? context.getNodesByKind('route')) {
    const at = node.qualifiedName.indexOf(marker);
    if (at < 0 || !node.id.startsWith('route:solid:')) continue;
    const rest = node.qualifiedName.slice(at + marker.length);
    const colon = rest.indexOf(':');
    marked.push({
      node,
      table: `${node.filePath}\0${rest.slice(0, colon)}`,
      path: rest.slice(colon + 1),
    });
  }
  if (!marked.length) return [];
  const tables = new Set(marked.map((m) => m.table));

  interface Parsed {
    program: SyntaxNode;
    helpers: Map<string, string>;
    exported: Map<string, string[]>;
    /** Local binding → the table it imports, as `file\0names`. */
    imports: Map<string, string>;
  }
  const parsed = new Map<string, Parsed | null>();
  const trees: { delete(): void }[] = [];
  const tableFor = (file: string, name: string): string | undefined =>
    [...tables].find((t) => {
      const [f, names] = t.split('\0');
      return f === file && names!.split(',').includes(name);
    });
  const parse = (file: string): Parsed | null => {
    if (parsed.has(file)) return parsed.get(file)!;
    const content = context.readFile(file);
    const language = detectLanguage(file);
    const tree = content?.includes('@solidjs/router') ? parseSourceTreeSync(content, language) : null;
    let entry: Parsed | null = null;
    if (tree) {
      trees.push(tree);
      const program = tree.rootNode;
      const imports = new Map<string, string>();
      for (const statement of program.namedChildren) {
        if (statement.type !== 'import_statement' || statement.children.some((n) => n.type === 'type'))
          continue;
        const source = literal(statement.childForFieldName('source'));
        const target = source && LOCAL_MODULE.test(source)
          ? resolveImportPath(source, file, language, context)
          : null;
        if (!target) continue;
        const clause = statement.namedChildren.find((n) => n.type === 'import_clause');
        const bound: [string, string][] = [];
        for (const child of clause?.namedChildren ?? []) {
          if (child.type === 'identifier') bound.push([child.text, 'default']);
          if (child.type === 'named_imports')
            for (const spec of child.namedChildren) {
              const name = spec.childForFieldName('name')?.text;
              if (spec.type === 'import_specifier' && name)
                bound.push([spec.childForFieldName('alias')?.text ?? name, name]);
            }
        }
        for (const [local, name] of bound) {
          const table = tableFor(target, name);
          if (table) imports.set(local, table);
        }
      }
      entry = { program, helpers: routerHelpers(program), exported: exportedBindings(program), imports };
    }
    parsed.set(file, entry);
    return entry;
  };

  // Every prefix a table is registered under; null when one is not a literal.
  const memo = new Map<string, Set<string> | null>();
  const tablePrefixes = (table: string): Set<string> | null => {
    if (memo.has(table)) return memo.get(table)!;
    memo.set(table, null); // a registration cycle is ambiguous
    let found: Set<string> | null = new Set();
    for (const file of context.getAllFiles()) {
      const entry = parse(file);
      if (!entry || ![...entry.imports.values()].includes(table)) continue;
      for (const [local, imported] of entry.imports)
        if (imported === table)
          for (const base of uses(file, entry, local, 0) ?? [null]) {
            if (base === null) found = null;
            found?.add(base as string);
          }
      if (!found) break;
    }
    memo.set(table, found);
    return found;
  };
  const cross = (outer: (string | null)[] | null, paths: (string | null)[]) =>
    outer?.flatMap((o) => paths.map((p) => (o === null || p === null ? null : join(o, p)))) ?? null;
  const pathsOf = (value: SyntaxNode | null | undefined): (string | null)[] => {
    const node = unwrap(value);
    return !node ? [''] : node.type === 'array' ? node.namedChildren.map(literal) : [literal(node)];
  };
  const uses = (file: string, entry: Parsed, local: string, depth: number): (string | null)[] | null =>
    entry.program
      .descendantsOfType('identifier')
      .filter((n) => n.text === local && n.parent?.type !== 'import_specifier' &&
        n.parent?.type !== 'import_clause' && n.parent?.childForFieldName('name')?.id !== n.id)
      .flatMap((n) => bases(file, entry, n, depth + 1) ?? [null]);
  // Where the table expression `node` is registered in this file.
  const bases = (file: string, entry: Parsed, node: SyntaxNode, depth: number): (string | null)[] | null => {
    if (depth > 16) return null;
    let current = node;
    let parent = current.parent;
    while (parent && ['parenthesized_expression', 'as_expression', 'satisfies_expression',
      'spread_element', 'jsx_fragment'].includes(parent.type)) {
      current = parent;
      parent = current.parent;
    }
    if (!parent) return [];
    if (parent.type === 'array') return bases(file, entry, parent, depth + 1);
    if (parent.type === 'pair') {
      const key = parent.childForFieldName('key');
      if ((key?.type === 'property_identifier' ? key.text : literal(key)) !== 'children') return [];
      const route = parent.parent!;
      const pair = route.namedChildren.find((n) => {
        const k = n.childForFieldName('key');
        return n.type === 'pair' && (k?.type === 'property_identifier' ? k.text : literal(k)) === 'path';
      });
      return cross(bases(file, entry, route, depth + 1), pathsOf(pair?.childForFieldName('value')));
    }
    if (parent.type === 'jsx_expression' || parent.type === 'jsx_element') {
      const element = parent.type === 'jsx_element' ? parent : parent.parent;
      const opening = element?.namedChildren.find((n) => n.type === 'jsx_opening_element');
      const helper = entry.helpers.get(opening?.childForFieldName('name')?.text ?? '');
      if (!element || !opening || !helper) return [];
      const attribute = (name: string) =>
        opening.namedChildren.find((n) => n.type === 'jsx_attribute' && n.namedChildren[0]?.text === name)
          ?.namedChildren[1];
      if (helper === 'Router') return pathsOf(attribute('base')).map((b) => (b === null ? null : join('', b)));
      return cross(bases(file, entry, element, depth + 1), pathsOf(attribute('path')));
    }
    if (parent.type === 'variable_declarator' && parent.childForFieldName('value')?.id === current.id) {
      const name = parent.childForFieldName('name');
      if (name?.type !== 'identifier') return [];
      const found = uses(file, entry, name.text, depth);
      const exported = entry.exported.get(name.text);
      const table = exported && exported.map((e) => tableFor(file, e)).find(Boolean);
      const outer = table ? tablePrefixes(table) : new Set<string>();
      return found && outer ? [...found, ...outer] : null;
    }
    return [];
  };

  try {
    const renamed: Node[] = [];
    for (const { node, table, path } of marked) {
      const prefixes = tablePrefixes(table);
      const name = prefixes?.size === 1 ? join([...prefixes][0]!, path) : path;
      if (node.name !== name) renamed.push({ ...node, name });
    }
    return renamed;
  } finally {
    for (const tree of trees) tree.delete();
  }
}

export const solidRouterResolver: FrameworkResolver = {
  name: 'solid-router',
  languages: ['typescript', 'javascript', 'tsx', 'jsx'],
  detect: (context) => dependsOn(context, '@solidjs/router'),
  extract: extractSolidRoutes,
  postExtract: solidTableRoutes,
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
        file && content !== null ? parseSourceTreeSync(content, detectLanguage(file)) : null;
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
    const imported = context.resolveImport?.({ ...ref, referenceName: name }) ?? null;
    if (imported) return { ...imported, original: ref };
    const targets = context
      .getNodesInFile(ref.filePath)
      .filter((n) => n.name === name && ['function', 'component'].includes(n.kind));
    return targets.length === 1
      ? { original: ref, targetNodeId: targets[0]!.id, confidence: 1, resolvedBy: 'framework' }
      : null;
  },
};
