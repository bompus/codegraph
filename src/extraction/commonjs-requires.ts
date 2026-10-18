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

/**
 * Index past the whitespace and comments the grammar skips between tokens, or -1
 * where the text could be something this reader does not follow (non-ASCII
 * whitespace, an HTML-like comment, an unclosed block comment).
 */
function skipExtras(source: string, i: number): number {
  for (;;) {
    const c = source.charCodeAt(i);
    if (c === 0x20 || (c >= 0x09 && c <= 0x0d)) { i++; continue; }
    if (c === 0x2f && source.charCodeAt(i + 1) === 0x2f) {
      for (i += 2; i < source.length && !'\n\r\u2028\u2029'.includes(source[i]!); i++);
      continue;
    }
    if (c === 0x2f && source.charCodeAt(i + 1) === 0x2a) {
      const close = source.indexOf('*/', i + 2);
      if (close < 0) return -1;
      i = close + 2;
      continue;
    }
    if (c > 0x7e || c < 0x20 || source.startsWith('<!--', i) || source.startsWith('-->', i)) return -1;
    return i;
  }
}

/**
 * Whether an error-free parse of the text could hold a call the tree walk
 * reads: `require`, an optional `?.`, `(`, then a quoted module path. It only
 * rules files out: anything it cannot read (type arguments, escapes, odd
 * spacing) counts as a call and the walk decides. Most files that say
 * "require" have no such call (`typeof require`, prose, `required`), and their
 * re-parse was the cost. A tagged template (require`…`) is no call here: the
 * walk never reads one. A tree with errors is out of its reach: recovery can
 * make a call of `require@('./x')`.
 */
export function mayRequireModulePath(source: string, language: Language): boolean {
  const typed = language === 'typescript' || language === 'tsx';
  for (let at = source.indexOf('require'); at !== -1; at = source.indexOf('require', at + 7)) {
    let i = skipExtras(source, at + 7);
    if (i >= 0 && source.startsWith('?.', i)) i = skipExtras(source, i + 2);
    if (i < 0 || (typed && source[i] === '<')) return true;
    if (source[i] !== '(') continue;
    // The grammar allows empty argument slots: `require(, './x')` is clean.
    do i = skipExtras(source, i + 1); while (i >= 0 && source[i] === ',');
    if (i < 0) return true;
    const quote = source[i];
    if (quote !== "'" && quote !== '"' && quote !== '`') continue;
    let end = i + 1;
    for (; end < source.length && source[end] !== quote; end++) {
      if (source[end] === '\\' || (quote === '`' ? source[end] === '$' : '\n\r\u2028\u2029'.includes(source[end]!))) return true;
    }
    if (end >= source.length || MODULE_SPECIFIER.test(source.slice(i + 1, end))) return true;
  }
  return false;
}

/** `parsedClean`: the caller's parser read `source` without a syntax error (the kernel's flag), so the text check may skip the re-parse. */
export function commonJsRequireRefs(filePath: string, source: string, language: Language, parsedClean = false): UnresolvedReference[] {
  if (!COMMONJS_LANGUAGES.has(language) || !source.includes('require')) return [];
  if (parsedClean && !mayRequireModulePath(source, language)) return [];
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
