/**
 * A built-in method name called on a receiver whose type is unknown —
 * `this.#items.add(x)`, `(list ??= []).push(x)` — is the built-in, so it
 * must not be matched by name to a project method that shares it. The
 * extractor keeps only the method name for these receivers, and a Set's
 * `add` used to link to whatever project class had an `add`. `this.add()`
 * and domain-named methods keep their name match.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

async function callsFrom(files: Record<string, string>, from: string): Promise<string[]> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-recv-'));
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), content);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  const source = cg.getNodesByKind('method').concat(cg.getNodesByKind('function')).find((n) => n.name === from)!;
  expect(source).toBeDefined();
  return cg
    .getOutgoingEdges(source.id)
    .filter((e) => e.kind === 'calls')
    .map((e) => cg!.getNode(e.target))
    .filter((n): n is NonNullable<typeof n> => !!n)
    .map((n) => `${n.filePath}:${n.qualifiedName}`);
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

const REGISTRY = {
  'src/registry.ts': 'export class Registry {\n  add(name: string): void {\n    void name;\n  }\n}\n',
  'src/svc.ts': 'export class Svc {\n  refresh(): void {}\n}\n',
};

describe('member calls on a receiver of unknown type', () => {
  it('a built-in method name does not match a project method', async () => {
    const calls = await callsFrom({
      ...REGISTRY,
      'src/store.ts': 'export class Store {\n  #items = new Set<number>();\n  track(n: number): void {\n    this.#items.add(n);\n  }\n}\n',
    }, 'track');
    expect(calls.some((c) => c.endsWith('Registry::add'))).toBe(false);
  });

  it('this.add() still reaches the class\'s own add', async () => {
    const calls = await callsFrom({
      ...REGISTRY,
      'src/store.ts': 'export class Store {\n  add(n: number): void {\n    void n;\n  }\n  track(n: number): void {\n    this.add(n);\n  }\n}\n',
    }, 'track');
    expect(calls).toContain('src/store.ts:Store::add');
  });

  it('a domain-named method keeps its name match', async () => {
    const calls = await callsFrom({
      ...REGISTRY,
      'src/store.ts': "import { Svc } from './svc';\n\nexport class Store {\n  #svc = new Svc();\n  sync(): void {\n    this.#svc.refresh();\n  }\n}\n",
    }, 'sync');
    expect(calls.some((c) => c.endsWith('Svc::refresh'))).toBe(true);
  });
});
