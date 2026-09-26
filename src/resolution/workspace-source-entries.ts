/** Map explicit package exports through static Rollup/Rolldown entry declarations.
 * Configs are parsed, never imported or executed. Dynamic or ambiguous entries
 * stay unresolved; neither filenames nor symbol names establish an export.
 */
import * as fs from 'node:fs';
import * as path from 'node:path';
import { parseSourceTreeSync, TreeNode } from '../extraction/parse-tree';

type Value = string | Value[] | { [key: string]: Value | undefined } | undefined;
type ObjectValue = { [key: string]: Value | undefined };
function object(value: Value): ObjectValue | undefined {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function bundleEntries(source: string, directory: string): Map<string, string> {
  const tree = parseSourceTreeSync(source, 'typescript');
  const entries = new Map<string, string>();
  if (!tree) return entries;
  try {
    if (tree.rootNode.hasError || tree.rootNode.namedChildren.some(n =>
      !['import_statement', 'lexical_declaration', 'export_statement', 'function_declaration',
        'interface_declaration', 'type_alias_declaration', 'comment'].includes(n.type))) return entries;
    const declarations = new Map<string, TreeNode>();
    const configHelpers = new Set<string>();
    const pathModules = new Set<string>();
    let exported: TreeNode | null = null;
    for (const node of tree.rootNode.namedChildren) {
      if (node.type === 'import_statement') {
        const spec = node.childForFieldName('source')?.text.slice(1, -1);
        if (spec === 'rolldown' || spec === 'rollup') {
          for (const imp of node.descendantsOfType('import_specifier')) {
            if (imp.childForFieldName('name')?.text === 'defineConfig') {
              configHelpers.add((imp.childForFieldName('alias') ?? imp.childForFieldName('name'))!.text);
            }
          }
        }
        if (spec === 'node:path' || spec === 'path') {
          const clause = node.namedChildren.find(n => n.type === 'import_clause');
          const name = clause?.namedChildren.find(n => n.type === 'identifier' || n.type === 'namespace_import');
          if (name) pathModules.add(name.type === 'identifier' ? name.text : name.lastChild!.text);
        }
      }
      if (node.type === 'lexical_declaration' && node.firstChild?.text === 'const') {
        for (const decl of node.namedChildren) {
          const name = decl.childForFieldName('name');
          const value = decl.childForFieldName('value');
          if (name?.type === 'identifier' && value) declarations.set(name.text, value);
        }
      }
      if (node.type === 'export_statement' && node.children.some(n => n.text === 'default')) {
        exported = node.childForFieldName('value');
      }
    }
    let visits = 0;
    function read(node: TreeNode | null | undefined, depth = 0): Value {
      if (!node || depth > 32 || ++visits > 10000) return undefined;
      const next = (n: TreeNode | null | undefined) => read(n, depth + 1);
      if (['string', 'template_string'].includes(node.type)) {
        // Only literal paths: no interpolation or escaped text to misinterpret.
        return /[\\]|\$\{/.test(node.text) ? undefined : node.text.slice(1, -1);
      }
      if (node.type === 'identifier') return next(declarations.get(node.text));
      if (node.type === 'parenthesized_expression') return next(node.namedChildren[0]);
      if (node.type === 'array') return node.namedChildren.map(next);
      if (node.type === 'object') {
        const result: ObjectValue = Object.create(null);
        for (const field of node.namedChildren) {
          if (field.type === 'comment') continue;
          if (field.type === 'spread_element') {
            const spread = object(next(field.namedChildren[0]));
            if (!spread) return undefined;
            Object.assign(result, spread);
          } else if (field.type === 'pair') {
            const key = field.childForFieldName('key');
            if (!key || !['property_identifier', 'string'].includes(key.type)) return undefined;
            result[key.type === 'string' ? key.text.slice(1, -1) : key.text] = next(field.childForFieldName('value'));
          } else if (field.type === 'shorthand_property_identifier') {
            result[field.text] = next(declarations.get(field.text));
          } else return undefined;
        }
        return result;
      }
      if (node.type === 'member_expression') {
        if (node.text === 'import.meta.dirname') return directory;
        return object(next(node.childForFieldName('object')))?.[node.childForFieldName('property')?.text ?? ''];
      }
      if (node.type === 'call_expression') {
        const callee = node.childForFieldName('function');
        const args = node.childForFieldName('arguments')?.namedChildren.filter(n => n.type !== 'comment') ?? [];
        if (callee?.type === 'identifier' && configHelpers.has(callee.text) && args.length === 1) return next(args[0]);
        if (callee?.type === 'member_expression' && pathModules.has(callee.childForFieldName('object')?.text ?? '') &&
            callee.childForFieldName('property')?.text === 'resolve') {
          const values = args.map(next);
          if (values.length && values.every((v): v is string => typeof v === 'string')) {
            return path.resolve(directory, ...values);
          }
        }
      }
      return undefined;
    }
    const config = read(exported);
    const functions = tree.rootNode.namedChildren.filter(n => n.type === 'function_declaration');
    const capturesMutable = new Set<string>();
    for (const fn of functions) {
      if (fn.descendantsOfType('identifier').some(n => {
        const value = read(n);
        return value !== undefined && typeof value === 'object';
      })) capturesMutable.add(fn.childForFieldName('name')!.text);
    }
    // Include local helper chains and aliases without executing their bodies.
    let changed = true;
    while (changed) {
      changed = false;
      for (const [name, node] of [...declarations, ...functions.map(fn => [fn.childForFieldName('name')!.text, fn] as const)]) {
        if (!capturesMutable.has(name) && (capturesMutable.has(node.text) ||
            node.descendantsOfType('identifier').some(n => capturesMutable.has(n.text)))) {
          capturesMutable.add(name);
          changed = true;
        }
      }
    }
    // A const binding does not make its object immutable. Do not use static
    // initializers when evaluated top-level code can mutate those objects.
    for (const statement of tree.rootNode.namedChildren.filter(n =>
      n.type === 'lexical_declaration' || n.type === 'export_statement')) {
      if (statement.descendantsOfType(['assignment_expression', 'augmented_assignment_expression', 'update_expression']).length) return entries;
      for (const call of statement.descendantsOfType('call_expression')) {
        const callee = call.childForFieldName('function');
        if (callee?.type === 'identifier' && configHelpers.has(callee.text)) continue;
        if (callee?.type === 'identifier' && capturesMutable.has(callee.text)) return entries;
        const receiver = callee?.type === 'member_expression' ? read(callee.childForFieldName('object')) : undefined;
        if (receiver !== undefined && typeof receiver === 'object') return entries;
        const args = call.childForFieldName('arguments');
        if (args?.descendantsOfType('identifier').some(n => {
          const value = read(n);
          return capturesMutable.has(n.text) || value !== undefined && typeof value === 'object';
        })) return entries;
      }
    }
    if (visits > 10000) return entries;
    const ambiguous = new Set<string>();
    for (const item of Array.isArray(config) ? config : [config]) {
      const options = object(item);
      const input = options?.input;
      const inputs = typeof input === 'string' ? { [path.parse(input).name]: input } : object(input);
      if (!inputs || !Object.keys(inputs).length || Object.values(inputs).some(v => typeof v !== 'string')) return new Map();
      for (const output of Array.isArray(options?.output) ? options.output : [options?.output]) {
        const out = object(output);
        if (!out || ['preserveModules', 'preserveModulesRoot'].some(k => Object.hasOwn(out, k))) return new Map();
        for (const [name, entry] of Object.entries(inputs)) {
          if (typeof entry !== 'string') continue;
          const pattern = Object.hasOwn(out, 'entryFileNames') ? out.entryFileNames : '[name].js';
          const filename = typeof out.file === 'string' && Object.keys(inputs).length === 1 ? out.file
            : typeof out.dir === 'string' && typeof pattern === 'string' && !/\[(?!name\])/.test(pattern)
              ? path.join(out.dir, pattern.replaceAll('[name]', name)) : undefined;
          if (!filename) return new Map();
          const target = path.resolve(directory, filename);
          const original = path.resolve(directory, entry);
          if (entries.has(target) && entries.get(target) !== original) ambiguous.add(target);
          entries.set(target, original);
        }
      }
    }
    for (const target of ambiguous) entries.delete(target);
    return entries;
  } finally { tree.delete(); }
}

/** A resolvable source file (not a declaration file). */
const SOURCE_FILE = /\.(?:[cm]?[jt]sx?|vue|svelte)$/;

/** Every string target under an `exports` value, conditions included. */
function allTargets(value: unknown): string[] {
  if (typeof value === 'string') return [value];
  if (!value || typeof value !== 'object') return [];
  return Object.values(value).flatMap(allTargets);
}

/**
 * Exact subpaths whose conditions name exactly one source file that exists in
 * the checkout (`"@zod/source": "./src/v4/index.ts"` beside build targets that
 * are not committed). Build output present locally makes it ambiguous, so the
 * subpath is skipped rather than guessed.
 */
function existingSourceEntries(projectRoot: string, member: string, name: string, exports: unknown): Map<string, string> {
  const result = new Map<string, string>();
  const directory = path.join(projectRoot, member);
  const rootExports = typeof exports === 'object' && exports !== null && !Array.isArray(exports) &&
    Object.keys(exports).some(k => k.startsWith('.')) ? exports : { '.': exports };
  for (const [subpath, value] of Object.entries(rootExports as Record<string, unknown>)) {
    if (subpath !== '.' && !subpath.startsWith('./') || subpath.includes('*')) continue;
    const existing = new Set(allTargets(value)
      .filter(t => t.startsWith('./') && SOURCE_FILE.test(t) && !/\.d\.[cm]?ts$/.test(t))
      .map(t => path.resolve(directory, t))
      .filter(abs => fs.existsSync(abs)));
    if (existing.size !== 1) continue;
    const relative = path.relative(projectRoot, [...existing][0]!).replace(/\\/g, '/');
    if (relative.startsWith('../') || !relative.startsWith(member + '/')) continue;
    result.set(name + (subpath === '.' ? '' : subpath.slice(1)), relative);
  }
  return result;
}

/** Exact public specifier → source file, relative to the indexed project. */
export function loadWorkspaceSourceEntries(projectRoot: string, member: string, name: string): Map<string, string> {
  let result = new Map<string, string>();
  const directory = path.join(projectRoot, member);
  let exports: unknown;
  let scripts: Record<string, unknown>;
  try {
    const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8'));
    exports = manifest.exports;
    scripts = manifest.scripts ?? {};
  }
  catch { return result; }
  if (!exports) return result;
  result = existingSourceEntries(projectRoot, member, name, exports);
  const configs = ['rolldown', 'rollup'].flatMap(tool => ['ts', 'mts', 'js', 'mjs'].map(ext => `${tool}.config.${ext}`))
    .filter(file => fs.existsSync(path.join(directory, file)));
  if (configs.length !== 1) return result;
  // npm/pnpm package scripts run in the package directory. Requiring a direct
  // config command establishes the base for relative input/output paths; CLI
  // overrides and shell wrappers are deliberately outside this static subset.
  const tool = configs[0]!.split('.')[0]!;
  if (!Object.values(scripts).some(command => typeof command === 'string' &&
      [ `${tool} --config ${configs[0]}`, `${tool} -c ${configs[0]}` ].includes(command.trim()))) return result;
  let bundles: Map<string, string>;
  try { bundles = bundleEntries(fs.readFileSync(path.join(directory, configs[0]!), 'utf8'), directory); }
  catch { return result; }
  const rootExports = typeof exports === 'object' && exports !== null && !Array.isArray(exports) &&
    Object.keys(exports).some(k => k.startsWith('.')) ? exports : { '.': exports };
  function targets(value: unknown): string[] | undefined {
    if (typeof value === 'string') return [value];
    if (!value || typeof value !== 'object') return undefined;
    const children = Object.values(value).map(targets);
    return children.every((v): v is string[] => !!v) ? children.flat() : undefined;
  }
  for (const [subpath, value] of Object.entries(rootExports)) {
    if (subpath !== '.' && !subpath.startsWith('./') || subpath.includes('*')) continue;
    const paths = targets(value);
    if (!paths?.length || paths.some(p => !p.startsWith('./'))) continue;
    const sources = paths.map(p => bundles.get(path.resolve(directory, p)));
    if (!sources.every((s): s is string => !!s) || new Set(sources).size !== 1) continue;
    const relative = path.relative(projectRoot, sources[0]!).replace(/\\/g, '/');
    if (relative.startsWith('../') || !relative.startsWith(member + '/')) continue;
    result.set(name + (subpath === '.' ? '' : subpath.slice(1)), relative);
  }
  return result;
}
