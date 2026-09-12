/**
 * Resolution reads the bindings table (Phase 2 of
 * docs/design/resolution-binding-model-plan.md).
 *
 * For a TS/JS file the walker emitted rows for, the bare-import, local-shadow
 * and sealed-module questions and the import / re-export mappings are
 * answered from the table, not from source regexes. Each case here is a
 * shape the regexes could not see and the rows can: a parameter that shadows
 * only inside its function, a `require` destructuring, a default export
 * that binds no declaration, a barrel forwarding a default, and an imported
 * name re-exported by a later clause.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

let tempDir: string;
let cg: CodeGraph | null = null;

async function project(files: Record<string, string>): Promise<CodeGraph> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-bindres-'));
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), content);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  cg.resolveReferences();
  return cg;
}

function callsFrom(graph: CodeGraph, caller: string): Array<{ file: string; name: string; by?: string }> {
  const from = graph.getNodesByKind('function').find((n) => n.name === caller)!;
  expect(from, caller).toBeDefined();
  return graph.getOutgoingEdges(from.id).filter((e) => e.kind === 'calls').map((e) => {
    const t = graph.getNode(e.target)!;
    return { file: t.filePath, name: t.name, by: (e.metadata as { resolvedBy?: string } | undefined)?.resolvedBy };
  });
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe.skipIf(!kernelBuilt)('resolution reads bindings', () => {
  it('a parameter shadows a cross-file name only inside its own function', async () => {
    const graph = await project({
      'lib.ts': 'export function notify() { return 1; }',
      'app.ts': [
        "import './lib';",
        'export function inside(notify: () => void) { notify(); }',
        'export function outside() { notify(); }',
      ].join('\n'),
    });
    expect(callsFrom(graph, 'inside')).toEqual([]);
    expect(callsFrom(graph, 'outside')).toMatchObject([{ file: 'lib.ts', name: 'notify' }]);
  });

  it('a `require` destructuring is an import the resolver follows', async () => {
    const graph = await project({
      'util.js': 'export function slugify(s) { return s; }\nexport function other() {}',
      'main.js': "const { slugify: slug } = require('./util');\nexport function run() { return slug('x'); }",
    });
    expect(callsFrom(graph, 'run')).toMatchObject([{ file: 'util.js', name: 'slugify', by: 'import' }]);
  });

  it('a file whose only export is `export default <expression>` is not sealed', async () => {
    const graph = await project({
      'shared.js': "import { defineConfig } from 'vite';\nexport default defineConfig({ plugins: [] });",
      'a.config.js': "import shared from './shared.js';\nexport default shared;",
    });
    const shared = graph.getNodesByKind('file').find((n) => n.filePath === 'shared.js')!;
    expect(graph.getIncomingEdges(shared.id).some((e) => e.kind === 'imports')).toBe(true);
  });

  it('a barrel forwarding a default export (`export { default as X } from`) resolves the import', async () => {
    const graph = await project({
      'cards/card.js': 'export default function card() { return 1; }',
      'cards/index.js': "export { default as Card } from './card.js';",
      'use.js': "import { Card } from './cards/index.js';\nexport function draw() { return Card(); }",
    });
    expect(callsFrom(graph, 'draw')).toMatchObject([{ file: 'cards/card.js', name: 'card', by: 'import' }]);
  });

  it('an imported name re-exported by a later clause keeps the file reachable', async () => {
    const graph = await project({
      'inner.js': 'export default function Inner() { return 1; }',
      'bridge.js': "import Inner from './inner.js';\nexport { Inner };",
      'user.js': "import { Inner } from './bridge.js';\nexport function use() { return Inner(); }",
    });
    const bridge = graph.getNodesByKind('file').find((n) => n.filePath === 'bridge.js')!;
    expect(graph.getIncomingEdges(bridge.id).some((e) => e.kind === 'imports')).toBe(true);
  });

  it('a bare call never lands on a property, field or enum member, and the rejection manufactures no other match', async () => {
    const graph = await project({
      'types.ts': 'export interface Options { trace: () => void }\nexport enum E { a, b }',
      'other.ts': 'export const holder = { trace: 1 };',
      'app.ts': 'export function run() { trace(); a(); }',
    });
    expect(callsFrom(graph, 'run')).toEqual([]);
  });

  it('a dynamic bare import stays external', async () => {
    const graph = await project({
      'server.ts': 'export function createServer() { return 1; }',
      'http.ts': "export async function boot() { const { createServer } = await import('node:http'); return createServer(); }",
    });
    expect(callsFrom(graph, 'boot')).toEqual([]);
  });
});
