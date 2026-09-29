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

describe('block-scoped locals', () => {
  function callsByLine(caller: string) {
    const from = graph!.getNodesByKind('function').find(n => n.name === caller)!;
    expect(from).toBeDefined();
    return graph!.getOutgoingEdges(from.id).filter(e => e.kind === 'calls')
      .map(e => `${e.line}:${graph!.getNode(e.target)!.qualifiedName}`).sort();
  }

  it('types same-named Go locals in sibling blocks, case clauses and an if header separately', async () => {
    await project({ 'main.go': `package main

type Service struct{}

func NewService() *Service { return &Service{} }
func (s *Service) Run() {}

type Other struct{}

func NewOther() *Other { return &Other{} }
func (o *Other) Run() {}

func run(flag bool) {
	if flag {
		svc := NewService()
		svc.Run()
	} else {
		svc := NewOther()
		svc.Run()
	}
}

func cases(k int, svc *Service) {
	switch k {
	case 1:
		v := NewService()
		v.Run()
	case 2:
		v := NewOther()
		v.Run()
	}
	if svc := NewOther(); svc != nil {
		svc.Run()
	}
	svc.Run()
}
` });
    expect(callsByLine('run')).toEqual(['15:NewService', '16:Service::Run', '18:NewOther', '19:Other::Run']);
    expect(callsByLine('cases')).toEqual([
      '26:NewService', '27:Service::Run', '29:NewOther', '30:Other::Run',
      '32:NewOther', '33:Other::Run', '35:Service::Run',
    ]);
  });

  it('binds a Go function literal parameter and types a conversion to a package type', async () => {
    await project({
      'w.go': `package main

type Writer interface{ Written() bool }

type impl struct{}

func (i *impl) Written() bool { return true }
`,
      'main.go': `package main

type Context struct{}

func (c *Context) String(s string) {}

type Engine struct{}

func (e *Engine) GET(p string, h func(c *Context)) {}

type Conn struct{}

func Dial() (*Conn, error) { return &Conn{}, nil }

func serve(router *Engine) {
	go func() {
		router.GET("/", func(c *Context) { c.String("ok") })
	}()
	c, err := Dial()
	_, _ = c, err
}

func convert() {
	w := Writer(&impl{})
	w.Written()
}
`,
    });
    expect(callsByLine('serve')).toEqual(['17:Context::String', '17:Engine::GET', '19:Dial']);
    expect(callsByLine('convert')).toContain('25:Writer::Written');
  });

  it('types a TS block-local const apart from a sibling block, the parameter it shadows and a hoisted var', async () => {
    await project({ 'a.ts': `class A { run() {} }
class B { run() {} }
function make(): B { return new B(); }
declare const c: boolean;
export function go(k: string) {
  switch (k) {
    case 'a': { const h = new A(); h.run(); break; }
    case 'b': { const h = new B(); h.run(); break; }
  }
}
export function shadow(x: A) {
  if (c) {
    const x = make();
    x.run();
  }
  x.run();
}
export function hoisted() {
  if (c) {
    var y = new B();
  }
  y.run();
}
` });
    expect(callsByLine('go')).toEqual(['7:A::run', '8:B::run']);
    expect(callsByLine('shadow')).toEqual(['13:make', '14:B::run', '16:A::run']);
    expect(callsByLine('hoisted')).toEqual(['22:B::run']);
  });

  it('does not type a local from an outer declaration its own untypeable declaration shadows', async () => {
    await project({ 'shadow.ts': `export interface Tree { label(): string }
export function keep(w: Tree | undefined) { return w?.label(); }
declare function suite(fn: () => void): void;
export class Leaf<T> { label() { return 'leaf'; } }
function load() { return new Leaf<string>(); }
function loadTyped(): Leaf<string> { return new Leaf<string>(); }
suite(() => {
  const w = load();
  w.label();
});
suite(() => {
  const w = loadTyped();
  w.label();
});
` });
    const file = graph!.getNodesByKind('file')[0]!;
    const calls = graph!.getOutgoingEdges(file.id).filter(e => e.kind === 'calls')
      .map(e => `${e.line}:${graph!.getNode(e.target)!.qualifiedName}`).sort();
    expect(calls).toEqual(['12:loadTyped', '13:Leaf::label', '8:load']);
  });
});
