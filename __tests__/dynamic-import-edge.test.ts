/**
 * Dynamic `import('x')` is an import boundary: the specifier names a module
 * exactly like a static `import … from 'x'`, but the call-expression shape
 * never emitted an `imports` ref — so lazy registries (`load: () =>
 * import('./handler')`, React.lazy, route split points) left their targets
 * graph-isolated and retrieval could not walk to them.
 *
 * The extractors now emit the specifier as an `imports` ref from the
 * enclosing symbol, resolved like a static specifier. The `.then(v => v.m)`
 * unwrap is a member access on the module namespace — untyped, so no
 * symbol-level edge is claimed; the file edge is the honest deliverable.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

function project(files: Record<string, string>): void {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-dynimport-'));
  for (const [rel, content] of Object.entries(files)) {
    const abs = path.join(tempDir, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, content);
  }
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

function importEdgesTo(fileSuffix: string): { from: string; to: string }[] {
  const targets = cg!.getNodesByKind('file').filter((n) => n.filePath.endsWith(fileSuffix));
  return targets.flatMap((t) =>
    cg!.getIncomingEdges(t.id)
      .filter((e) => e.kind === 'imports')
      .map((e) => ({
        from: cg!.getNode(e.source)?.name ?? e.source,
        to: t.filePath,
      })),
  );
}

describe('dynamic import() emits an imports edge to the specifier file', () => {
  it('links a lazy-registry `load: () => import(\'./x\')` to the target file', async () => {
    project({
      'registry.ts': [
        'export const COMMANDS = {',
        "  about: { load: () => import('./commands/about.ts').then((v) => v.about) },",
        '};',
      ].join('\n'),
      'commands/about.ts': 'export function about() { return 1; }',
    });
    cg = await CodeGraph.init(tempDir, { index: true });
    cg.resolveReferences();

    const edges = importEdgesTo('commands/about.ts');
    expect(edges.length).toBeGreaterThan(0);
    expect(edges.every((e) => e.to === 'commands/about.ts')).toBe(true);
  });

  it('links an `await import(\'./x\')` call site as well', async () => {
    project({
      'main.ts': [
        'export async function run() {',
        "  const mod = await import('./lazy/mod.ts');",
        '  return mod;',
        '}',
      ].join('\n'),
      'lazy/mod.ts': 'export const mod = 1;',
    });
    cg = await CodeGraph.init(tempDir, { index: true });
    cg.resolveReferences();

    expect(importEdgesTo('lazy/mod.ts').length).toBeGreaterThan(0);
  });

  it('declines interpolated specifiers and non-string args', async () => {
    project({
      'main.ts': [
        'export function load(name: string) {',
        '  const a = import(`./lazy/${name}.ts`);',
        '  const b = import(name);',
        '  return [a, b];',
        '}',
      ].join('\n'),
      'lazy/only.ts': 'export const only = 1;',
    });
    cg = await CodeGraph.init(tempDir, { index: true });
    cg.resolveReferences();

    expect(importEdgesTo('lazy/only.ts')).toEqual([]);
  });
});
