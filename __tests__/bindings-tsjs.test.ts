/**
 * Bindings emitted by the TS/JS kernel walker (Phase 1 of
 * docs/design/resolution-binding-model-plan.md).
 *
 * The `bindings` table is the single answer to "is X exported" and "what
 * does N bind to in F". Two levels:
 *   - extraction: the walker's rows for every export form (esm at the
 *     declaration, later `export { }`, `export default NAME`, CommonJS
 *     assignment), for imports (default / named / aliased / namespace), for
 *     re-exports, and for a nested (`local`) declaration;
 *   - storage: an indexed project persists the rows, and a `decl` row with
 *     `exportedAs` marks its node exported even when the declaration itself
 *     is not inside an export statement — the shape the old regex
 *     `isExportedLater` existed for.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
// eslint-disable-next-line @typescript-eslint/no-require-imports
const { DatabaseSync } = require('node:sqlite') as typeof import('node:sqlite');
import CodeGraph from '../src/index';
import { tryKernelExtract } from '../src/extraction/kernel';
import type { Binding } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

const SOURCE = `import React from 'react';
import { readFile, join as pathJoin } from './fs-utils';
import * as helpers from '../helpers';
export { widget as default } from './widget';
export { helperA, helperB as renamedB } from './helpers';

export function exportedAtDecl() { return 1; }
function laterNamed() { return 2; }
function laterAliased() { return 3; }
const useStore = { actions: { load() { return 4; } } };
function neverExported() {
  function inner() { return 5; }
  return inner();
}
exports.cjsFn = function () { return 6; };
export { laterNamed, laterAliased as aliasedOut };
export default useStore;
`;

function by(bindings: Binding[], name: string, kind?: Binding['kind']): Binding | undefined {
  return bindings.find((b) => b.name === name && (kind === undefined || b.kind === kind));
}

describe.skipIf(!kernelBuilt)('TS/JS bindings: extraction', () => {
  const result = kernelBuilt ? tryKernelExtract('src/mod.ts', SOURCE, 'typescript') : null;
  const bindings = result?.bindings ?? [];

  it('emits rows for every declaration, import and re-export', () => {
    expect(bindings.length).toBeGreaterThanOrEqual(12);
  });

  it('esm at the declaration', () => {
    const b = by(bindings, 'exportedAtDecl', 'decl')!;
    expect(b).toMatchObject({ exportedAs: 'exportedAtDecl', exportForm: 'esm', scopeStart: 1 });
    expect(b.nodeId).toBeDefined();
  });

  it('later `export { name }` and `export { name as alias }`', () => {
    expect(by(bindings, 'laterNamed', 'decl')).toMatchObject({ exportedAs: 'laterNamed', exportForm: 'esm-later' });
    expect(by(bindings, 'laterAliased', 'decl')).toMatchObject({ exportedAs: 'aliasedOut', exportForm: 'esm-later' });
  });

  it('`export default NAME`', () => {
    expect(by(bindings, 'useStore', 'decl')).toMatchObject({ exportedAs: 'default', exportForm: 'esm-default' });
  });

  it('CommonJS `exports.name = function () {}` (the anonymous form both engines name, #1675)', () => {
    expect(by(bindings, 'cjsFn', 'decl')).toMatchObject({ exportedAs: 'cjsFn', exportForm: 'cjs' });
  });

  it('an unexported declaration has no exportedAs', () => {
    const b = by(bindings, 'neverExported', 'decl')!;
    expect(b.exportedAs).toBeUndefined();
    expect(b.exportForm).toBeUndefined();
  });

  it('a nested declaration is a `local` row scoped to its enclosing function', () => {
    const inner = by(bindings, 'inner', 'local')!;
    const outer = by(bindings, 'neverExported', 'decl')!;
    expect(inner).toBeDefined();
    expect(inner.scopeStart).toBeGreaterThan(1);
    expect(inner.scopeEnd).toBeLessThan(SOURCE.split('\n').length);
    expect(outer.scopeEnd).toBe(SOURCE.split('\n').length);
  });

  it('imports: default, named, aliased, namespace', () => {
    expect(by(bindings, 'React', 'import')).toMatchObject({ targetSpec: 'react', targetName: 'default' });
    expect(by(bindings, 'readFile', 'import')).toMatchObject({ targetSpec: './fs-utils', targetName: 'readFile' });
    expect(by(bindings, 'pathJoin', 'import')).toMatchObject({ targetSpec: './fs-utils', targetName: 'join' });
    expect(by(bindings, 'helpers', 'import')).toMatchObject({ targetSpec: '../helpers', targetName: '*' });
  });

  it('re-exports carry the source and the exported name', () => {
    expect(by(bindings, 'helperA', 'reexport')).toMatchObject({ targetSpec: './helpers', exportedAs: 'helperA', exportForm: 'esm' });
    expect(by(bindings, 'helperB', 'reexport')).toMatchObject({ targetSpec: './helpers', exportedAs: 'renamedB' });
    expect(by(bindings, 'widget', 'reexport')).toMatchObject({ targetSpec: './widget', exportedAs: 'default' });
  });
});

describe.skipIf(!kernelBuilt)('TS/JS bindings: storage', () => {
  let dir: string;
  let cg: CodeGraph;
  beforeAll(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-bindings-'));
    fs.mkdirSync(path.join(dir, 'src'));
    fs.writeFileSync(path.join(dir, 'src', 'mod.ts'), SOURCE);
    cg = await CodeGraph.init(dir, { index: true });
  });
  afterAll(() => {
    cg.close();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('persists the rows for the file', () => {
    const db = new DatabaseSync(path.join(dir, '.codegraph', 'codegraph.db'), { readOnly: true });
    try {
      const rows = db.prepare('SELECT name, kind, exported_as, export_form, target_spec FROM bindings WHERE file_path = ? ORDER BY line, name').all('src/mod.ts') as Array<Record<string, unknown>>;
      expect(rows.length).toBeGreaterThanOrEqual(12);
      expect(rows.find((r) => r.name === 'laterAliased')).toMatchObject({ kind: 'decl', exported_as: 'aliasedOut', export_form: 'esm-later' });
      expect(rows.find((r) => r.name === 'pathJoin')).toMatchObject({ kind: 'import', target_spec: './fs-utils' });
    } finally {
      db.close();
    }
  });

  it('a later-exported declaration is stored as exported, from the bindings table', () => {
    const later = cg.getNodesByName('laterNamed').find((n) => n.kind === 'function')!;
    expect(later.isExported).toBe(true);
    const store = cg.getNodesByName('useStore')[0]!;
    expect(store.isExported).toBe(true);
    const never = cg.getNodesByName('neverExported')[0]!;
    expect(never.isExported).toBe(false);
  });
});
