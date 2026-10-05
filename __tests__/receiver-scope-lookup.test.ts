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
