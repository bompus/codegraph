/**
 * Inside a function, a parameter or local of the same name hides an import:
 * `toStore(get)` calling `get()` calls its parameter. The import arm used to
 * answer before any scope check, so the call linked to the imported function.
 * A class method of the same name does not hide the import — a bare call
 * never reaches a class member.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

async function callsFrom(files: Record<string, string>, from: string): Promise<string[]> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-shadow-'));
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), content);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  const source = cg.getNodesByKind('function').concat(cg.getNodesByKind('method')).find((n) => n.name === from)!;
  expect(source).toBeDefined();
  return cg
    .getOutgoingEdges(source.id)
    .filter((e) => e.kind === 'calls')
    .map((e) => cg!.getNode(e.target))
    .filter((n): n is NonNullable<typeof n> => !!n)
    .map((n) => `${n.filePath}:${n.name}`);
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

const LIB = { 'src/lib.ts': 'export function get(): number {\n  return 1;\n}\n' };

describe('a parameter or local hides a same-named import', () => {
  it('a parameter', async () => {
    const calls = await callsFrom({
      ...LIB,
      'src/store.ts': "import { get } from './lib';\n\nexport function toStore(get: () => number) {\n  return get();\n}\n",
    }, 'toStore');
    expect(calls).not.toContain('src/lib.ts:get');
  });

  it('a destructured parameter', async () => {
    const calls = await callsFrom({
      ...LIB,
      'src/store.ts': "import { get } from './lib';\n\nexport function run({ get }: { get: () => number }) {\n  return get();\n}\n",
    }, 'run');
    expect(calls).not.toContain('src/lib.ts:get');
  });

  it('a local declared in the function', async () => {
    const calls = await callsFrom({
      ...LIB,
      'src/store.ts': "import { get } from './lib';\n\nexport function run() {\n  const get = () => 2;\n  return get();\n}\n",
    }, 'run');
    expect(calls).not.toContain('src/lib.ts:get');
  });

  it('a parameter called after an if condition, with another file exporting the name', async () => {
    const calls = await callsFrom({
      ...LIB,
      'src/store.ts': "export function derive(cb: (n: number, get: (n: number) => void) => void) {\n  cb(1, (n) => n);\n}\nexport function evens() {\n  derive((n, get) => {\n    if (n % 2 === 0) get(n);\n  });\n}\n",
    }, 'evens');
    expect(calls).not.toContain('src/lib.ts:get');
  });

  it('still links a call the import really answers', async () => {
    const calls = await callsFrom({
      ...LIB,
      'src/store.ts': "import { get } from './lib';\n\nexport function use(n: number) {\n  return get() + n;\n}\n",
    }, 'use');
    expect(calls).toContain('src/lib.ts:get');
  });

  it('still links the import from inside a class with a same-named method', async () => {
    const calls = await callsFrom({
      ...LIB,
      'src/store.ts': "import { get } from './lib';\n\nexport class Box {\n  get(): number {\n    return 0;\n  }\n  read(): number {\n    return get();\n  }\n}\n",
    }, 'read');
    expect(calls).toContain('src/lib.ts:get');
  });
});

describe('a member call is never an import', () => {
  it('this.x() links to the class method, not a same-named import', async () => {
    const calls = await callsFrom({
      'src/lib.ts': 'export function findAll(): number[] {\n  return [];\n}\n',
      'src/handler.ts': "import { findAll } from './lib';\n\nexport class Handler {\n  findAll(): number[] {\n    return findAll();\n  }\n  impact(): number {\n    return this.findAll().length;\n  }\n}\n",
    }, 'impact');
    expect(calls).toContain('src/handler.ts:findAll');
    expect(calls).not.toContain('src/lib.ts:findAll');
  });

  it('a parameter does not hide the method a member call names', async () => {
    const calls = await callsFrom({
      ...LIB,
      'src/box.ts': "import { get } from './lib';\n\nexport class Box {\n  get(): number {\n    return 0;\n  }\n  read(get: number): number {\n    return this.get() + get;\n  }\n}\n",
    }, 'read');
    expect(calls).toContain('src/box.ts:get');
    expect(calls).not.toContain('src/lib.ts:get');
  });
});
