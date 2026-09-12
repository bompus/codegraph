import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';
import { tryKernelExtract } from '../src/extraction/kernel';
import { TreeSitterExtractor } from '../src/extraction/tree-sitter';

let dir: string | undefined;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});
async function project(files: Record<string, string>) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-receiver-binding-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
  graph.resolveReferences();
  return graph;
}
function targets(caller: string) {
  const from = graph!.getNodesByKind('function').find(n => n.name === caller)!;
  expect(from).toBeDefined();
  return graph!.getOutgoingEdges(from.id).filter(e => e.kind === 'calls')
    .map(e => graph!.getNode(e.target)!.qualifiedName);
}

describe('receiver binding evidence', () => {
  it('does not guess a method for an untyped parameter or a shadowed project value', async () => {
    await project({ 'app.ts': `
export class Store { get() { return 1; } }
const store = new Store();
export function known() { return store.get(); }
export function unknown(values: any) { return values.get(); }
export function shadowed(store: any) { return store.get(); }
` });
    expect(targets('known')).toContain('Store::get');
    expect(targets('unknown')).toEqual([]);
    expect(targets('shadowed')).toEqual([]);
  });

  it('follows a typed nested receiver without guessing an unknown nested member', async () => {
    await project({ 'app.ts': `
export class Store { get() { return 1; } }
export class Holder { values: Store; constructor() { this.values = new Store(); } }
export function known(holder: Holder) { return holder.values.get(); }
export function unknown(holder: any) { return holder.values.get(); }
` });
    expect(targets('known')).toEqual(['Store::get']);
    expect(targets('unknown')).toEqual([]);
  });

  it('does not treat an arbitrary window namespace as proof of a project function', async () => {
    await project({ 'app.ts': `
declare const window: any;
export function ping() { return 1; }
export function unknown() { return window.MyNs.ping(); }
` });
    expect(targets('unknown')).toEqual([]);
  });

  it('keeps imported namespace calls while respecting a parameter that shadows the import', async () => {
    await project({
      'lib.ts': 'export function run() { return 1; }',
      'app.ts': `import * as service from './lib';
export function known() { return service.run(); }
export function unknown(service: any) { return service.run(); }
`,
    });
    expect(targets('known')).toEqual(['run']);
    expect(targets('unknown')).toEqual([]);
  });

  it('does not borrow a project class for an externally imported receiver type', async () => {
    await project({
      'other.ts': 'export class Store { get() { return 1; } }',
      'app.ts': "import { Store } from 'external-lib';\nexport function unknown(value: Store) { return value.get(); }",
    });
    expect(targets('unknown')).toEqual([]);
  });

  it('keeps member lookup on the bound declaration and its actual supertypes', async () => {
    await project({
      'other.ts': 'export class Store { get() { return 1; } }',
      'app.ts': `class Store {}
class StoreOther { static get() {} }
class Base { run() {} }
class Child extends Base {}
export function unknown(value: Store) { return value.get(); }
export function staticUnknown() { return Store.get(); }
export function inherited(value: Child) { return value.run(); }`,
    });
    expect(targets('unknown')).toEqual([]);
    expect(targets('staticUnknown')).toEqual([]);
    expect(targets('inherited')).toEqual(['Base::run']);
  });

  it('does not guess nested array or externally imported typeof fields', async () => {
    await project({
      'other.ts': 'export const storage = { get() { return 1; } };',
      'app.ts': `import { storage } from 'external-lib';
class Store { get() { return 1; } }
class Holder { array: Store[]; foreign: typeof storage; }
export function array(value: Holder) { return value.array.get(); }
export function foreign(value: Holder) { return value.foreign.get(); }`,
    });
    expect(targets('array')).toEqual([]);
    expect(targets('foreign')).toEqual([]);
  });

  it('does not treat an array or union annotation as its first named type', async () => {
    await project({ 'app.ts': `export class Store { get() { return 1; } }
export function array(values: Store[]) { return values.get(); }
export function union(values: Store | { get(): number }) { return values.get(); }` });
    expect(targets('array')).toEqual([]);
    expect(targets('union')).toEqual([]);
  });

  it('uses a bound factory return type without guessing an unknown factory result', async () => {
    await project({
      'lib.ts': `export class Store { get() { return 1; } }
export function createStore(): Store { return new Store(); }
export function unknownFactory(): any { return {}; }`,
      'app.ts': `import { createStore, unknownFactory } from './lib';
const store = createStore();
const untyped = unknownFactory();
export function known() { return store.get(); }
export function local() { const value = createStore(); return value.get(); }
export function unknown() { return untyped.get(); }`,
    });
    expect(targets('known')).toEqual(['Store::get']);
    expect(targets('local').sort()).toEqual(['Store::get', 'createStore'].sort());
    expect(targets('unknown')).toEqual([]);
  });

  it('does not borrow a namespace-qualified type or a later shadow declaration', async () => {
    await project({
      'app.ts': `import * as External from 'external-lib';
class Store { get() { return 1; } }
class Holder { values: External.Store; }
function createStore(): Store { return new Store(); }
export function qualified(value: External.Store) { return value.get(); }
export function nested(value: Holder) { return value.values.get(); }
export function shadow(store: any) {
  { const store = createStore(); }
  return store.get();
}`,
    });
    expect(targets('qualified')).toEqual([]);
    expect(targets('nested')).toEqual([]);
    expect(targets('shadow')).not.toContain('Store::get');
  });

  it('does not equate an internal module export with an unresolved workspace package export', async () => {
    await project({
      'package.json': JSON.stringify({ workspaces: ['packages/*'] }),
      'packages/lib/package.json': JSON.stringify({ name: 'lib', exports: './dist/index.js' }),
      'packages/lib/src/factory.ts': `export class Store { get() {} }
export function createStore(): Store { return new Store(); }
export function duplicate(): Store { return new Store(); }`,
      'packages/lib/src/other.ts': 'export function duplicate(): unknown { return null; }',
      'outside.ts': 'export function createStore(): unknown { return null; }',
      'app.ts': `import { createStore as make, duplicate } from 'lib';
import { createStore as external } from 'external-lib';
export function known() { const value = make(); return value.get(); }
export function unknown() { const value = external(); return value.get(); }
export function ambiguous() { const value = duplicate(); return value.get(); }`,
    });
    expect(targets('known')).not.toContain('Store::get');
    expect(targets('unknown')).not.toContain('Store::get');
    expect(targets('ambiguous')).not.toContain('Store::get');
  });

  it('retains Go composite-literal receiver text on both extraction paths', () => {
    const source = 'package app\nfunc known() { jsonBinding{}.BindBody(nil, nil) }';
    for (const result of [tryKernelExtract('app.go', source, 'go'), new TreeSitterExtractor('app.go', source, 'go').extract()]) {
      expect(result?.unresolvedReferences.filter(r => r.referenceKind === 'calls').map(r => r.referenceName))
        .toContain('jsonBinding.BindBody');
    }
  });

  it('resolves a Go composite literal to its own method, not a same-named sibling', async () => {
    await project({ 'app.go': `package app
type Wrong struct{}
func (w Wrong) Run() {}
type Right struct{}
func (r Right) Run() {}
func known() { Right{}.Run() }
` });
    expect(targets('known')).toContain('Right::Run');
    expect(targets('known')).not.toContain('Wrong::Run');
  });
});
