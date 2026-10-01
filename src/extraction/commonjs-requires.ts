/** File dependencies from literal CommonJS calls that use the module's require. */
import type { Language, UnresolvedReference } from '../types';
import { parseSourceTreeSync, type TreeNode } from './parse-tree';

const COMMONJS_LANGUAGES: ReadonlySet<Language> = new Set(['javascript', 'jsx', 'typescript', 'tsx']);
const MODULE_SPECIFIER = /^(?:\.{1,2}(?:\/|$)|#)|\//;
const FUNCTIONS = new Set(['function_declaration', 'function_expression', 'generator_function', 'generator_function_declaration', 'arrow_function', 'method_definition']);
const BLOCKS = new Set(['program', 'statement_block', 'for_statement', 'for_in_statement', 'switch_body']);

/** Only binding positions count: object keys, types and default values do not bind a name. */
function bindsRequire(pattern: TreeNode | null): boolean {
  if (!pattern) return false;
  if (['identifier', 'shorthand_property_identifier_pattern'].includes(pattern.type)) return pattern.text === 'require';
  if (['assignment_pattern', 'object_assignment_pattern'].includes(pattern.type)) return bindsRequire(pattern.childForFieldName('left'));
  if (pattern.type === 'pair_pattern') return bindsRequire(pattern.childForFieldName('value'));
  if (['required_parameter', 'optional_parameter'].includes(pattern.type)) return bindsRequire(pattern.childForFieldName('pattern'));
  if (['formal_parameters', 'rest_pattern', 'object_pattern', 'array_pattern'].includes(pattern.type)) return pattern.namedChildren.some(bindsRequire);
  return false;
}

/** Cook a literal without evaluating source; package paths may contain escaped characters. */
function literalValue(text: string): string | null {
  const escapes: Record<string, string> = { n: '\n', r: '\r', t: '\t', b: '\b', f: '\f', v: '\v' };
  try {
    return text.slice(1, -1).replace(/\\(?:u\{([\da-fA-F]+)\}|u([\da-fA-F]{4})|x([\da-fA-F]{2})|([0-3][0-7]{2}|[0-7]{1,2})|(\r\n|[\s\S]))/g,
      (_match, point: string | undefined, unicode: string | undefined, hex: string | undefined, octal: string | undefined, character: string | undefined) => {
        if (point || unicode || hex) return String.fromCodePoint(parseInt((point ?? unicode ?? hex)!, 16));
        if (octal) return String.fromCharCode(parseInt(octal, 8));
        if (character === '\n' || character === '\r' || character === '\r\n') return '';
        return escapes[character!] ?? character!;
      });
  } catch { return null; }
}

function scopeFor(node: TreeNode, functionScoped = false): TreeNode {
  for (let owner = node.parent; owner; owner = owner.parent) {
    if (FUNCTIONS.has(owner.type)) return functionScoped ? (owner.childForFieldName('body') ?? owner) : owner;
    if (owner.type === 'program' || owner.type === 'class_static_block' || (!functionScoped && BLOCKS.has(owner.type))) return owner;
  }
  return node;
}

export function commonJsRequireRefs(filePath: string, source: string, language: Language): UnresolvedReference[] {
  if (!COMMONJS_LANGUAGES.has(language) || !source.includes('require')) return [];
  const tree = parseSourceTreeSync(source, language);
  if (!tree) return [];
  const scopes: TreeNode[] = [];
  const calls: TreeNode[] = [];
  const stack = [tree.rootNode];
  try {
    while (stack.length) {
      const node = stack.pop()!;
      const children = node.namedChildren;
      for (let i = children.length - 1; i >= 0; i--) stack.push(children[i]!);
      if ((node.type === 'call_expression' && node.childForFieldName('function')?.text === 'require') || node.type === 'import_require_clause') calls.push(node);
      if (FUNCTIONS.has(node.type)) {
        if (bindsRequire(node.childForFieldName('parameters')) || bindsRequire(node.childForFieldName('parameter'))) scopes.push(node);
        if (node.type !== 'method_definition' && node.childForFieldName('name')?.text === 'require') scopes.push(node.type.endsWith('declaration') ? scopeFor(node) : node);
      } else if (node.type === 'variable_declarator' && bindsRequire(node.childForFieldName('name'))) {
        scopes.push(scopeFor(node, node.parent?.type === 'variable_declaration'));
      } else if (node.type === 'for_in_statement' && bindsRequire(node.childForFieldName('left'))) {
        scopes.push(node.children.some((child) => child.type === 'var') ? scopeFor(node, true) : node);
      } else if (node.type === 'catch_clause' && bindsRequire(node.childForFieldName('parameter'))) {
        scopes.push(node);
      } else if (['class_declaration', 'class'].includes(node.type) && node.childForFieldName('name')?.text === 'require') {
        scopes.push(node.type === 'class_declaration' ? scopeFor(node) : node);
      } else if (node.type === 'import_require_clause') {
        if (node.namedChildren[0]?.text === 'require') scopes.push(tree.rootNode);
      } else if (node.type === 'import_clause') {
        if (!node.parent?.children.some((child) => child.type === 'type') && node.namedChildren.some((child) => child.type === 'identifier' && child.text === 'require')) scopes.push(tree.rootNode);
      } else if (node.type === 'import_specifier' || node.type === 'namespace_import') {
        const local = node.childForFieldName('alias') ?? node.childForFieldName('name') ?? node.namedChildren.at(-1);
        let typeOnly = node.children.some((child) => child.type === 'type');
        for (let owner = node.parent; owner && owner.type !== 'program'; owner = owner.parent) {
          if (owner.type === 'import_statement') typeOnly ||= owner.children.some((child) => child.type === 'type');
        }
        if (!typeOnly && local?.text === 'require') scopes.push(tree.rootNode);
      }
    }
    const out: UnresolvedReference[] = [];
    const seen = new Set<string>();
    for (const node of calls) {
      const importEquals = node.type === 'import_require_clause';
      const callee = importEquals ? node : node.childForFieldName('function')!;
      if (!importEquals && (callee.type !== 'identifier' || scopes.some((scope) => scope.startIndex <= callee.startIndex && scope.endIndex > callee.startIndex))) continue;
      const args = importEquals ? node.namedChildren.filter((child) => child.type === 'string')
        : node.childForFieldName('arguments')?.namedChildren.filter((child) => child.type !== 'comment');
      if (args?.length !== 1 || !['string', 'template_string'].includes(args[0]!.type)) continue;
      const text = args[0]!.text;
      if (args[0]!.namedChildren.some((child) => child.type === 'template_substitution')) continue;
      const specifier = literalValue(text);
      if (!specifier || !MODULE_SPECIFIER.test(specifier) || seen.has(specifier)) continue;
      seen.add(specifier);
      out.push({ fromNodeId: `file:${filePath}`, referenceName: specifier, referenceKind: 'imports',
        line: callee.startPosition.row + 1, column: callee.startPosition.column, filePath, language });
    }
    return out;
  } finally {
    tree.delete();
  }
}
