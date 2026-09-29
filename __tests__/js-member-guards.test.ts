import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let dir: string | undefined;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});
async function project(files: Record<string, string | Buffer>) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-js-guards-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
  graph.resolveReferences();
  return graph;
}
function edges(caller: string, kind = 'calls') {
  const from = [...graph!.getNodesByKind('function'), ...graph!.getNodesByKind('method')].find(n => n.qualifiedName === caller || n.name === caller)!;
  expect(from).toBeDefined();
  return graph!.getOutgoingEdges(from.id).filter(e => e.kind === kind)
    .map(e => `${e.line}:${graph!.getNode(e.target)!.qualifiedName}`).sort();
}

const lib = 'export function findAll(): number[] {\n  return [];\n}\n';
const handler = (impact: string, line2 = '') => `import { findAll } from './lib';
${line2}
export class Handler {
  findAll(): number[] {
    return findAll();
  }
  impact(): number {
    ${impact}
  }
}
`;

describe('JS/TS member-call guards', () => {
  it('reads a this. chain wrapped onto the next line', async () => {
    await project({ 'src/lib.ts': lib, 'src/handler.ts': handler('return this\n      .findAll().length;') });
    expect(edges('impact')).toEqual(['8:Handler::findAll']);
  });

  it('keeps a wrapped chain of unknown type away from a same-named project method', async () => {
    await project({
      'src/registry.ts': 'export class Registry {\n  add(name: string): void {\n    void name;\n  }\n}\n',
      'src/store.ts': 'export class Store {\n  #items = { list: new Set<number>() };\n  track(n: number): void {\n    this.#items.list\n      .add(n);\n  }\n}\n',
      'calls.ts': 'export class View { state = { items: [] as number[] }; render() { return this.state.items\n      .filter(Boolean); } }\n',
      'decoys.ts': 'export class Query { filter() {} }\n',
    });
    expect(edges('track')).toEqual([]);
    expect(edges('render')).toEqual([]);
  });

  it('reads past a comment between the member name and its (', async () => {
    await project({ 'src/lib.ts': lib, 'src/handler.ts': handler('return this.findAll /*x*/().length;') });
    expect(edges('impact')).toEqual(['8:Handler::findAll']);
  });

  it('reads the source of a file that is not valid UTF-8', async () => {
    await project({
      'src/lib.ts': lib,
      'src/handler.ts': Buffer.from(handler('return this.findAll().length;', '// caf\xe9'), 'latin1'),
    });
    expect(edges('impact')).toEqual(['8:Handler::findAll']);
  });

  it('keeps the one-line forms', async () => {
    await project({ 'src/lib.ts': lib, 'src/handler.ts': handler('return this.findAll().length;') });
    expect(edges('impact')).toEqual(['8:Handler::findAll']);
    expect(edges('Handler::findAll')).toEqual(['5:findAll']);
  });
});

describe('JS/TS export and literal readings', () => {
  it('seeds an inherited this.member from the enclosing class, not a nested namesake', async () => {
    const login = (decoy: string) => `import { FormBase } from './base';
import { Unrelated } from './unrelated';
declare const bus: { on(ev: string, cb: () => void): void };
function decoy() { class ${decoy} extends Unrelated {} }
export class LoginForm extends FormBase {
  wire(): void { bus.on("submit", this.handleSubmit); }
}
`;
    const files = {
      'base.ts': 'export class FormBase { handleSubmit(): void {} }\n',
      'unrelated.ts': 'export class Unrelated { handleSubmit(): void {} }\n',
    };
    await project({ ...files, 'login.ts': login('LoginForm') });
    expect(edges('wire', 'references')).toEqual(['6:FormBase::handleSubmit']);
    graph!.close();
    graph = undefined;
    fs.rmSync(dir!, { recursive: true, force: true });
    await project({ ...files, 'login.ts': login('OtherForm') });
    expect(edges('wire', 'references')).toEqual(['6:FormBase::handleSubmit']);
  });

  it.each([
    ['an import re-exported under a new name', "import { signIn } from './auth'; export { signIn as login };\n"],
    ['an export-from with a rename', "export { signIn as login } from './auth';\n"],
  ])('follows %s', async (_label, index) => {
    await project({
      'src/auth.ts': 'export function signIn(): void {}\n',
      'src/index.ts': index,
      'src/main.ts': "import { login } from './index';\nexport function go(): void { login(); }\n",
    });
    expect(edges('go')).toEqual(['2:signIn']);
  });

  it.each([
    ['an escaped slash before the closing slash', '/^\\/api\\//'],
    ['an escaped paren', '/\\(/'],
    ['a plain regex', '/x/'],
  ])('splits object-literal properties around %s', async (_label, regex) => {
    await project({
      'impl.ts': `function wrong() { return 1; }
function right() { return 2; }
export const api = { re: ${regex}, run: right };
export function sameCaller() { return api.run(); }
`,
      'consumer.ts': "import { api } from './impl';\nexport function crossCaller() { return api.run(); }\n",
    });
    expect(edges('sameCaller')).toEqual(['4:right']);
    expect(edges('crossCaller')).toEqual(['2:right']);
  });
});
