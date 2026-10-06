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
async function project(files: Record<string, string>) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-receiver-scope-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
  graph.resolveReferences();
  return graph;
}
function calls(caller: string) {
  const from = [...graph!.getNodesByKind('function'), ...graph!.getNodesByKind('method')].find(n => n.name === caller)!;
  expect(from).toBeDefined();
  return graph!.getOutgoingEdges(from.id).filter(e => e.kind === 'calls')
    .map(e => `${e.line}:${graph!.getNode(e.target)!.qualifiedName}`).sort();
}

describe('receiver types follow each language lookup and scope rules', () => {
  it.each([
    ['latest assignment', 'def run():\n    m = Other()\n    m = Model()\n    m.dump()\n', ['9:Model::dump']],
    ['each call position', 'def run():\n    m = Other()\n    m.dump()\n    m = Model()\n    m.dump()\n', ['10:Model::dump', '8:Other::dump']],
    ['reassigned parameter', 'def run(m: Other):\n    m = Model()\n    m.dump()\n', ['8:Model::dump']],
    ['unknown replacement', 'def run():\n    m = Model()\n    m = object()\n    m.dump()\n', []],
    ['future local binding', 'def run():\n    m.dump()\n    m = Model()\n', []],
    ['nested assignment isolation', 'def run():\n    m = Model()\n    def inner():\n        m = Other()\n    m.dump()\n', ['10:Model::dump']],
    ['annotation without replacement', 'def run():\n    m = Model()\n    m: object\n    m.dump()\n', ['9:Model::dump']],
    ['module annotation without replacement', 'm = Model()\nm: object\ndef run():\n    m.dump()\n', ['9:Model::dump']],
    ['continued annotation without replacement', String.raw`def run():
    m = Model()
    m \
        : object
    m.dump()
`, ['10:Model::dump']],
  ])('uses the visible Python receiver value for %s', async (_case, body, expected) => {
    await project({ 'app.py': `class Model:
    def dump(self): pass
class Other:
    def dump(self): pass

${body}` });
    expect(calls('run').filter(call => call.endsWith('::dump'))).toEqual(expected);
  });

  it('resolves a Python receiver through its latest local import alias', async () => {
    await project({
      'pkg/__init__.py': '',
      'pkg/sub.py': 'def run(): return 1\n',
      'other.py': 'def run(): return 2\n',
      'app.py': `import pkg.sub as alias
def known():
    alias = object()
    import other as alias
    return alias.run()
`,
    });
    const from = graph!.getNodesByKind('function').find(n => n.name === 'known')!;
    const targets = graph!.getOutgoingEdges(from.id).filter(e => e.kind === 'calls')
      .map(e => graph!.getNode(e.target)!).filter(n => n.name === 'run');
    expect(targets.map(n => n.filePath)).toEqual(['other.py']);
  });

  it.each([
    ['module scope', 'import pkg.sub\nimport pkg.other\ndef known():\n    return pkg.sub.run()\n'],
    ['function scope', 'import pkg.sub\ndef known():\n    import pkg.other\n    return pkg.sub.run()\n'],
  ])('retains earlier unaliased Python submodules when another is imported in %s', async (_scope, app) => {
    await project({
      'pkg/__init__.py': '',
      'pkg/sub.py': 'def run(): return 1\n',
      'pkg/other.py': 'def different(): return 2\n',
      'app.py': app,
    });
    expect(calls('known')).toEqual(['4:run']);
  });

  it.each([
    ['module imports', 'import pkg.sub, pkg.other\ndef known():\n    return pkg.sub.run()\n', ['3:run']],
    ['local imports', 'def known():\n    import pkg.sub, pkg.other\n    return pkg.sub.run()\n', ['3:run']],
    ['explicit alias', 'import pkg.sub, pkg.other as pkg\ndef known():\n    return pkg.sub.run()\n', []],
    ['value replacement', 'import pkg.sub; pkg = object()\ndef known():\n    return pkg.sub.run()\n', []],
  ])('preserves only compatible same-line Python package bindings for %s', async (_case, app, expected) => {
    await project({
      'pkg/__init__.py': '',
      'pkg/sub.py': 'def run(): return 1\n',
      'pkg/other.py': 'def different(): return 2\n',
      'app.py': app,
    });
    expect(calls('known')).toEqual(expected);
  });

  it('treats an explicit Python alias matching the package name as a replacement', async () => {
    await project({
      'pkg/__init__.py': '',
      'pkg/sub.py': 'def run(): return 1\n',
      'pkg/other.py': 'def different(): return 2\n',
      'app.py': `import pkg.other
import pkg.sub as pkg
def known():
    return pkg.other.different()
`,
    });
    expect(calls('known')).toEqual([]);
  });

  it('keeps a Python receiver initialized before its returned closure runs', async () => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
def outer():
    def known():
        return store.get()
    store = Store()
    return known
outer()()
` });
    expect(calls('known')).toEqual(['5:Store::get']);
  });

  it.each([
    ["direct-reassignment", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A()\n    store = B()\n    return store.get()\ndef exercise(): return known()\n", ["B::get"]],
    ["returned-future-init", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    def known():\n        return store.get()\n    store = A()\n    return known\ndef exercise(): return outer()()\n", ["A::get"]],
    ["reassigned-before-definition", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    store = A()\n    store = B()\n    def known():\n        return store.get()\n    return known\ndef exercise(): return outer()()\n", ["B::get"]],
    ["reassigned-after-definition", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    store = A()\n    def known():\n        return store.get()\n    store = B()\n    return known\ndef exercise(): return outer()()\n", []],
    ["two-invocation-values", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    store = A()\n    def known():\n        return store.get()\n    first = known()\n    store = B()\n    return [first, known()]\ndef exercise(): return outer()\n", []],
    ["future-init-then-reassigned", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    def known():\n        return store.get()\n    store = A()\n    store = B()\n    return known\ndef exercise(): return outer()()\n", []],
    ["unknown-after-definition", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    store = A()\n    def known():\n        return store.get()\n    store = object()\n    return known\ndef exercise(): return outer()()\n", []],
    ["module-reassigned-after-definition", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\nstore = A()\ndef known():\n    return store.get()\nstore = B()\ndef exercise(): return known()\n", []],
    ["captured annotation only", "class A:\n    def get(self): return 1\ndef outer():\n    store = A()\n    def known(): return store.get()\n    store: object\n    return known\n", ["A::get"]],
    ["sibling local mutation", "class A:\n    def get(self): return 1\nclass B:\n    def get(self): return 2\ndef outer():\n    store = A()\n    def known(): return store.get()\n    def unrelated(): store = B()\n    return known\n", ["A::get"]],
  ])('does not assert an invocation-time receiver type for %s', async (_case, source, expected) => {
    await project({ 'app.py': source });
    expect(calls('known').filter(call => call.endsWith('::get')).map(call => call.slice(call.indexOf(':') + 1))).toEqual(expected);
  });


  it.each([
    ["local-same-line", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A(); store = B()\n    return store.get()\ndef exercise(): return known()\n", "known", []],
    ["local-reverse", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = B(); store = A()\n    return store.get()\ndef exercise(): return known()\n", "known", []],
    ["module-same-line", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\nstore = A(); store = B()\ndef known(): return store.get()\ndef exercise(): return known()\n", "known", []],
    ["module-unknown", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\nstore = A(); store = object()\ndef known(): return store.get()\ndef exercise(): return known()\n", "known", []],
    ["lambda-same-line", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    store = A()\n    known = lambda: store.get(); store = B()\n    return known\ndef exercise(): return outer()()\n", "outer", []],
    ["lambda-two-values", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef outer():\n    store = A()\n    known = lambda: store.get(); first = known(); store = B(); return [first, known()]\ndef exercise(): return outer()\n", "outer", []],
    ["stable-same-line", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A(); return store.get()\n", "known", ["A::get"]],
    ["future-same-line", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    first = store.get(); store = A(); return first\n", "known", []],
    ["stable-parameter-same-line", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known(store: A): return store.get()\n", "known", ["A::get"]],
    ["deferred-single-same-line", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A(); run = lambda: store.get(); return run\n", "known", ["A::get"]],
    ["annotation-first replacement", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A()\n    store: B; store = B()\n    return store.get()\n", "known", []],
    ["other annotation before replacement", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A()\n    unused: object; store = B()\n    return store.get()\n", "known", ["B::get"]],
    ["other value before bare annotation", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A()\n    unused = object(); store: B\n    return store.get()\n", "known", ["A::get"]],
    ["same-line bare annotation", "class A:\n    def get(self): return 'A'\nclass B:\n    def get(self): return 'B'\n\ndef known():\n    store = A()\n    store: B; unused = object()\n    return store.get()\n", "known", ["A::get"]],
  ])('keeps same-line Python receiver uncertainty conservative for %s', async (_case, source, caller, expected) => {
    await project({ 'app.py': source });
    expect(calls(caller).filter(call => call.endsWith('::get')).map(call => call.slice(call.indexOf(':') + 1))).toEqual(expected);
  });

  it.each([
    ['replacement', 'store = A(); store = B(); return store.get', []],
    ['stable', 'store = A(); return store.get', ['A::get']],
  ])('keeps same-line Python method values conservative for %s', async (_case, body, expected) => {
    await project({ 'app.py': `class A:
    def get(self): return 1
class B:
    def get(self): return 2
def known():
    ${body}
` });
    const from = graph!.getNodesByKind('function').find(n => n.name === 'known')!;
    expect(graph!.getOutgoingEdges(from.id).filter(e => e.kind === 'references' && e.metadata?.fnRef === true)
      .map(e => graph!.getNode(e.target)!.qualifiedName)).toEqual(expected);
  });

  it.each(['self', 'cls'])('refuses a captured %s receiver after replacement', async receiver => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
    def outer(${receiver}):
        def known(): return ${receiver}.get()
        ${receiver} = object()
        return known
` });
    expect(calls('known')).toEqual([]);
  });

  it.each(['self', 'cls'])('retains a stable captured %s receiver', async receiver => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
    def outer(${receiver}):
        def known(): return ${receiver}.get()
        return known
` });
    expect(calls('known')).toEqual(['4:Store::get']);
  });

  it.each(['lambda: store.get()', 'lambda: [store.get]', '(store.get() for _ in [0])'])('refuses a deferred captured member %s after replacement', async expression => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
class Other:
    def get(self): return 2
def outer():
    store = Store()
    known = ${expression}
    store = Other()
    return known
` });
    const from = graph!.getNodesByKind('function').find(n => n.name === 'outer')!;
    expect(graph!.getOutgoingEdges(from.id).filter(e => ['calls', 'references'].includes(e.kind))
      .map(e => graph!.getNode(e.target)!.qualifiedName).filter(name => name.endsWith('::get'))).toEqual([]);
  });

  it('keeps an unsupported future lambda receiver unresolved', async () => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
def outer():
    known = lambda: store.get()
    store = Store()
    return known
` });
    expect(calls('outer').filter(call => call.endsWith('::get'))).toEqual([]);
  });

  it('keeps a lambda default evaluated before a later receiver replacement', async () => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
def outer():
    store = Store()
    known = lambda value=store.get(): value
    store = object()
    return known
` });
    expect(calls('outer').filter(call => call.endsWith('::get'))).toEqual(['5:Store::get']);
  });

  it('preserves a lambda receiver captured as a default parameter', async () => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
def outer():
    store = Store()
    known = lambda store=store: store.get()
    store = object()
    return known
` });
    expect(calls('outer').filter(call => call.endsWith('::get'))).toEqual(['5:Store::get']);
  });

  it('keeps the first generator iterable evaluated before receiver replacement', async () => {
    await project({ 'app.py': `class Store:
    def get(self): return [1]
def outer():
    store = Store()
    known = (value for value in store.get())
    store = object()
    return known
` });
    expect(calls('outer').filter(call => call.endsWith('::get'))).toEqual(['5:Store::get']);
  });

  it('refuses a lambda default evaluated inside a captured deferred function', async () => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
def outer():
    store = Store()
    def known(): return lambda value=store.get(): value
    store = object()
    return known
` });
    expect(calls('known')).toEqual([]);
  });

  it('refuses a captured method value after its receiver is replaced', async () => {
    await project({ 'app.py': `class Store:
    def get(self): return 1
def outer():
    store = Store()
    def known(): return store.get
    store = object()
    return known
` });
    const from = graph!.getNodesByKind('function').find(n => n.name === 'known')!;
    expect(graph!.getOutgoingEdges(from.id).filter(e => e.kind === 'references' && e.metadata?.fnRef === true)
      .map(e => graph!.getNode(e.target)!.qualifiedName)).toEqual([]);
  });

  it('retains an unaliased package receiver when another submodule is imported later', async () => {
    await project({
      'pkg/__init__.py': '',
      'pkg/sub.py': 'def run(): return 1\n',
      'pkg/other.py': 'def different(): return 2\n',
      'app.py': `import pkg.sub
def known(): return pkg.sub.run()
import pkg.other
`,
    });
    expect(calls('known')).toEqual(['2:run']);
  });

  it('refuses a module receiver when a later import alias replaces it', async () => {
    await project({
      'a.py': 'def run(): return 1\n',
      'b.py': 'def run(): return 2\n',
      'app.py': `import a as store
def known(): return store.run()
import b as store
`,
    });
    expect(calls('known')).toEqual([]);
  });

  it('refuses a package receiver when a later assignment replaces the package object', async () => {
    await project({
      'pkg/__init__.py': '',
      'pkg/sub.py': 'def run(): return 1\n',
      'app.py': `import pkg.sub
def known(): return pkg.sub.run()
pkg = object()
`,
    });
    expect(calls('known')).toEqual([]);
  });

  it('finds a Ruby included module before the superclass chain, last include first', async () => {
    await project({ 'app.rb': `class Base
  def m; end
  def n; end
end
module M
  def m; end
  def n; end
end
module N
  def n; end
end
class B < Base
end
class A < B
  include M
  include N
end
class C < Base
end
def run
  a = A.new
  a.m
  a.n
  c = C.new
  c.m
end
` });
    expect(calls('run')).toEqual(['22:M::m', '23:N::n', '25:Base::m']);
  });

  it('finds a PHP trait method before the parent class', async () => {
    await project({ 'app.php': `<?php
class Base { function get() { return 1; } function put() { return 1; } }
trait OverridesGet { function get() { return 2; } }
class Store extends Base { use OverridesGet; }
function known(Store $value) { $value->put(); return $value->get(); }
function built() { $store = new Store(); return $store->get(); }
` });
    expect(calls('known')).toEqual(['5:Base::put', '5:OverridesGet::get']);
    expect(calls('built')).toContain('6:OverridesGet::get');
    expect(calls('built')).not.toContain('6:Base::get');
  });

  it('types a Go range over the result slot the collection was assigned from', async () => {
    await project({ 'app.go': `package app

type Item struct{}
func (i Item) Run() {}
type Decoy struct{}
func (d Decoy) Run() {}
type Provider struct{}
func (p Provider) List() ([]Decoy, []Item) { return nil, nil }

func ranged(p Provider) {
    _, items := p.List()
    for _, item := range items {
        item.Run()
    }
}

func first(p Provider) {
    decoys, _ := p.List()
    for _, d := range decoys {
        d.Run()
    }
}
` });
    expect(calls('ranged')).toEqual(['11:Provider::List', '13:Item::Run']);
    expect(calls('first')).toEqual(['18:Provider::List', '20:Decoy::Run']);
  });

  it('does not revive an awaited binding type after reassignment', async () => {
    await project({ 'caller.ts': `export class Engine { run() {} }
export async function makeEngine(): Promise<Engine> { return new Engine(); }
class Other { run() {} }
export async function drive() {
  let value = await makeEngine();
  value = new Other();
  value.run();
}
export async function keep() {
  let value = await makeEngine();
  value.run();
}
export async function nested() {
  const value = await makeEngine();
  const hooks = { act(value: Other) { return value; } };
  value.run();
}
` });
    expect(calls('drive')).not.toContain('7:Engine::run');
    expect(calls('keep')).toEqual(['10:makeEngine', '11:Engine::run']);
    expect(calls('nested')).toContain('16:Engine::run');
  });

  it('leaves Kotlin it to the outer let when an inner run lambda takes no parameter', async () => {
    await project({ 'app.kt': `class Widget { fun render() {} }
fun known(w: Widget) {
    w.let { run { it.render() } }
}
fun direct(w: Widget) {
    w.let { it.render() }
}
` });
    expect(calls('known')).toEqual(['3:Widget::render']);
    expect(calls('direct')).toEqual(['6:Widget::render']);
  });

  it('matches a Scala class scan on the whole owner name, not a substring', async () => {
    await project({
      'app/Loud.scala': `class Loudspeaker {
  def shout(): Int = 2
}
object Loud {
  def shout(): Int = 1
}
`,
      'app/Main.scala': `object Main {
  def run(): Int = Loud.shout()
}
`,
    });
    expect(calls('run')).toEqual(['2:Loud::shout']);
  });

  it('does not type a Java lambda parameter from a same-named field', async () => {
    await project({ 'App.java': `class Store { int get() { return 1; } }
class Cache { int get() { return 2; } }
class App { Store value; int run(java.util.List<Cache> caches) { caches.forEach(value -> value.get()); return value.get(); } }
` });
    expect(calls('run')).toEqual(['3:Store::get']);
  });

  it('does not type a Kotlin lambda parameter from an outer same-named local', async () => {
    await project({ 'app.kt': `class Box { fun open(): Int { return 1 } }
class Other { fun close(): Int { return 2 } }
fun make(): Box { return Box() }
fun run(other: Other) {
    val v: Box = make()
    other.let { v -> v.open() }
    other.let { v -> v.close() }
    v.open()
}
` });
    expect(calls('run')).toEqual(['5:make', '7:Other::close', '8:Box::open']);
  });

  it('types each Go parameter of a grouped declaration', async () => {
    await project({ 'app.go': `package app
type Store struct{}
func (s Store) get() int { return 1 }
type Other struct{}
func (o Other) get() int { return 2 }
func known(value, other Store) int { return value.get() }
func mixed(value Other, other Store) int { return value.get() + other.get() }
` });
    expect(calls('known')).toEqual(['6:Store::get']);
    expect(calls('mixed')).toEqual(['7:Other::get', '7:Store::get']);
  });
});
