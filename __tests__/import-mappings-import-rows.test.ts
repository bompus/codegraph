/**
 * A file's import mappings are built from its `import` binding rows only. The
 * resolver reads those rows with their own query instead of loading every
 * binding of the file (declarations, locals, parameters) and filtering, so the
 * narrow query must return exactly the rows the filter kept, in the same order.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { importMappingsFromBindings } from '../src/resolution/import-resolver';
import type { QueryBuilder } from '../src/db/queries';
import type { ResolutionContext } from '../src/resolution/types';

const FILES: Record<string, string> = {
  'src/app.ts': [
    "import Default, { named, other as alias } from './lib';",
    "import * as ns from './ns';",
    "import type { Shape } from './types';",
    "const req = require('./legacy');",
    "export * from './reexported';",
    "export { picked } from './picked';",
    'export function run(param: Shape, second: number): number {',
    '  const local = named(param) + alias(second);',
    '  let other = ns.value + req.count;',
    '  return Default(local, other);',
    '}',
  ].join('\n'),
  'src/lib.ts': 'export default function main() { return 1; }\nexport const named = () => 1;\nexport const other = () => 2;\n',
  'src/ns.ts': 'export const value = 1;\n',
  'src/types.ts': 'export interface Shape { x: number }\n',
  'src/legacy.js': 'module.exports = { count: 1 };\n',
  'src/reexported.ts': 'export const fromStar = 1;\n',
  'src/picked.ts': 'export const picked = 1;\n',
  'src/plain.ts': 'export function alone(a: number) { const b = a * 2; return b; }\n',
  'tools/script.py': 'import os\nfrom .helpers import helper as h\n\ndef go(arg):\n    value = h(arg)\n    return os.path.join(value)\n',
  'tools/helpers.py': 'def helper(x):\n    return x\n',
};

describe('import mappings read only the import rows', () => {
  let dir: string;
  let cg: CodeGraph;
  let queries: QueryBuilder;
  let context: ResolutionContext;

  beforeAll(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-import-rows-'));
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), text);
    }
    cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    queries = (cg as unknown as { queries: QueryBuilder }).queries;
    context = (cg as unknown as { resolver: { context: ResolutionContext } }).resolver.context;
  });

  afterAll(() => {
    cg.close();
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5 });
  });

  it('returns the same rows, in order, as filtering every binding of the file', () => {
    let sawOthers = false;
    for (const file of Object.keys(FILES)) {
      const all = queries.getBindingsByFile(file);
      if (all.some((b) => b.kind !== 'import')) sawOthers = true;
      expect(queries.getImportBindingsByFile(file)).toEqual(all.filter((b) => b.kind === 'import' && b.targetSpec));
    }
    // The fixture must exercise the rows the narrow query skips.
    expect(sawOthers).toBe(true);
  });

  it('gives every file the import mappings its full binding rows give', () => {
    for (const file of Object.keys(FILES)) {
      const expected = importMappingsFromBindings(queries.getBindingsByFile(file)) ?? [];
      expect(context.getImportMappings(file, file.endsWith('.py') ? 'python' : 'typescript')).toEqual(expected);
    }
    const app = context.getImportMappings('src/app.ts', 'typescript');
    expect(app.map((m) => m.localName)).toEqual(expect.arrayContaining(['Default', 'named', 'alias', 'ns', 'req']));
    expect(context.getImportMappings('src/plain.ts', 'typescript')).toEqual([]);
    expect(context.getImportMappings('tools/script.py', 'python').map((m) => m.localName)).toEqual(
      expect.arrayContaining(['os', 'h'])
    );
  });
});
