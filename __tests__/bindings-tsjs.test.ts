/**
 * Bindings emitted by the TS/JS kernel walker (Phases 1 and 2 of
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
import { extractFromSource } from '../src/extraction';
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

/**
 * Phase 2 rows: the names that shadow a cross-file symbol without becoming a
 * node (parameters, function-body locals), the CommonJS import and export
 * forms, wildcard and default re-exports, and a default export that binds no
 * declaration. These are what let the resolver drop its source regexes.
 */
const PHASE2 = `import Widget from './widget';
const util = require('./util');
const { readSync, statSync: stat } = require('node:fs');
export { Widget };
export * from './all';
export * as ns from './space';
export { default as Card } from './card';
export default defineConfig({ plugins: [] });

export async function main(first, { second, third = 1 }, ...rest) {
  const loaded = await load();
  const dyn = await import('./dyn');
  const { a: renamedA } = await import('./ab');
  const picked = useStore((s) => s.picked);
  const { fetchUser } = useStore.getState();
  try { loaded(); } catch (err) { console.log(err); }
  return [first, second, third, rest, dyn, renamedA, picked, fetchUser];
}
function helper() { return 1; }
function bracketed() { return 2; }
const text = \`outer \${\`inner \${module.exports = { helper, renamed: bracketed }}\`}\`;
exports['bracketed'] = bracketed;
`;

describe.skipIf(!kernelBuilt)('TS/JS bindings: scoped rows, CommonJS and default forms', () => {
  const result = kernelBuilt ? tryKernelExtract('src/p2.js', PHASE2, 'javascript') : null;
  const bindings = result?.bindings ?? [];
  const rows = (name: string) => bindings.filter((b) => b.name === name);

  it('parameters are `param` rows scoped to their function, including destructured, defaulted and rest names', () => {
    for (const name of ['first', 'second', 'third', 'rest']) {
      expect(by(bindings, name), name).toMatchObject({ kind: 'param', scopeStart: 10, scopeEnd: 18 });
    }
    expect(by(bindings, 's')).toMatchObject({ kind: 'param', scopeStart: 14, scopeEnd: 14 });
    expect(by(bindings, 'err')).toMatchObject({ kind: 'param' });
  });

  it('a function-body `const x = call()` is a nodeless `local` row', () => {
    expect(by(bindings, 'loaded')).toMatchObject({ kind: 'local', scopeStart: 10, scopeEnd: 18 });
    expect(by(bindings, 'loaded')!.nodeId).toBeUndefined();
  });

  it('a store selector and a destructured member are not local bindings', () => {
    expect(rows('picked')).toEqual([]);
    expect(rows('fetchUser')).toEqual([]);
  });

  it('`require` and `await import()` are `import` rows, at module scope and inside a function', () => {
    expect(by(bindings, 'util')).toMatchObject({ kind: 'import', targetSpec: './util', targetName: 'default' });
    expect(by(bindings, 'util')!.nodeId).toBeDefined();
    expect(by(bindings, 'readSync')).toMatchObject({ kind: 'import', targetSpec: 'node:fs', targetName: 'readSync' });
    expect(by(bindings, 'stat')).toMatchObject({ kind: 'import', targetSpec: 'node:fs', targetName: 'statSync' });
    expect(by(bindings, 'dyn')).toMatchObject({ kind: 'import', targetSpec: './dyn', scopeStart: 10, scopeEnd: 18 });
    expect(by(bindings, 'renamedA')).toMatchObject({ kind: 'import', targetSpec: './ab', targetName: 'a' });
  });

  it('an imported name a later clause exports carries the export on its import row', () => {
    expect(by(bindings, 'Widget')).toMatchObject({ kind: 'import', exportedAs: 'Widget', exportForm: 'esm-later' });
  });

  it('wildcard and default re-exports are rows', () => {
    const wild = bindings.filter((b) => b.kind === 'reexport' && b.name === '*');
    expect(wild.map((b) => [b.targetSpec, b.exportedAs])).toEqual([['./all', '*'], ['./space', 'ns']]);
    expect(bindings.find((b) => b.kind === 'reexport' && b.name === 'default')).toMatchObject({ targetSpec: './card', exportedAs: 'Card' });
  });

  it('`export default <expression>` is a nodeless `default` row', () => {
    const def = by(bindings, 'default', 'decl');
    expect(def).toMatchObject({ exportedAs: 'default', exportForm: 'esm-default' });
    expect(def!.nodeId).toBeUndefined();
  });

  it('CommonJS object and bracket exports, wherever the assignment sits', () => {
    expect(by(bindings, 'helper')).toMatchObject({ kind: 'decl', exportedAs: 'helper', exportForm: 'cjs-object' });
    expect(by(bindings, 'bracketed')).toMatchObject({ kind: 'decl', exportedAs: 'renamed', exportForm: 'cjs-object' });
  });
});

/**
 * Rows for the files the walker does not extract: the generic extractor's
 * path (ArkTS here; a stack-guard defer takes the same path) gets rows from
 * the kernel's AST-only emitter with node ids attached by name and line, and
 * a Vue / Svelte / Astro file gets its script block's rows rebased to file
 * positions.
 */
describe.skipIf(!kernelBuilt)('TS/JS bindings: generic-extractor and SFC paths', () => {
  it('an ArkTS file has rows with node ids attached', () => {
    const src = "import { Repo } from 'data';\nexport function load(id: string) { const r = new Repo(); return r.get(id); }\nfunction helper() { return 1; }\nexport { helper as util };\n";
    const result = extractFromSource('entry/src/main/ets/Page.ets', src, 'arkts');
    const rows = result.bindings ?? [];
    expect(by(rows, 'Repo')).toMatchObject({ kind: 'import', targetSpec: 'data', targetName: 'Repo' });
    const load = by(rows, 'load');
    expect(load).toMatchObject({ kind: 'decl', exportedAs: 'load', exportForm: 'esm' });
    expect(load!.nodeId).toBe(result.nodes.find((n) => n.name === 'load')!.id);
    expect(by(rows, 'id')).toMatchObject({ kind: 'param', scopeStart: 2, scopeEnd: 2 });
    expect(by(rows, 'r')).toMatchObject({ kind: 'local' });
    expect(by(rows, 'helper')).toMatchObject({ kind: 'decl', exportedAs: 'util', exportForm: 'esm-later' });
  });

  it('a Vue file carries its script block rows at file positions', () => {
    const src = "<template><div @click=\"go()\" /></template>\n<script setup lang=\"ts\">\nimport { useRouter } from 'vue-router';\nconst router = useRouter();\nfunction go(to: string) { router.push(to); }\n</script>\n";
    const result = extractFromSource('src/views/Home.vue', src, 'vue');
    const rows = result.bindings ?? [];
    expect(rows.every((b) => b.filePath === 'src/views/Home.vue')).toBe(true);
    expect(by(rows, 'useRouter')).toMatchObject({ kind: 'import', targetSpec: 'vue-router', line: 3 });
    expect(by(rows, 'to')).toMatchObject({ kind: 'param', scopeStart: 5, scopeEnd: 5, line: 5 });
    const go = by(rows, 'go');
    expect(go).toMatchObject({ kind: 'decl', line: 5 });
    expect(go!.nodeId).toBe(result.nodes.find((n) => n.name === 'go')!.id);
  });
});
