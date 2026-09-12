import { afterEach, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let graph: CodeGraph | undefined;
let dir: string;
afterEach(() => { graph?.close(); graph = undefined; if (dir) fs.rmSync(dir, { recursive: true, force: true }); });
async function project(files: Record<string, string>) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-phase2b-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
}
function calls(name: string) {
  const caller = [...graph!.getNodesByKind('function'), ...graph!.getNodesByKind('method')].find(n => n.name === name)!;
  expect(caller).toBeDefined();
  return graph!.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => graph!.getNode(e.target)!.name);
}
it.each([
  ['app.py', 'class Store:\n    def get(self): return 1\ndef known(value: Store): return value.get()\ndef unknown(value): return value.get()\n'],
  ['app.go', 'package app\ntype Store struct{}\nfunc (s Store) get() int { return 1 }\nfunc known(value Store) int { return value.get() }\nfunc unknown(value any) { value.get() }\n'],
  ['App.java', 'class Store { int get() { return 1; } }\nclass App { int known(Store value) { return value.get(); }\nint unknown(Object value) { return value.get(); } }\n'],
  ['app.kt', 'class Store { fun get(): Int { return 1 } }\nfun known(value: Store): Int { return value.get() }\nfun unknown(value: Any) { value.get() }\n'],
  ['app.php', '<?php\nclass Store { function get() { return 1; } }\nfunction known(Store $value) { return $value->get(); }\nfunction unknown($value) { return $value->get(); }\n'],
  ['app.cpp', 'class Store { public: int get() { return 1; } };\nint known(Store value) { return value.get(); }\nvoid unknown(auto value) { value.get(); }\n'],
])('keeps typed calls and diagnoses unknown receivers in %s', async (name, source) => {
  await project({ [name]: source });
  expect(calls('known')).toContain('get');
  expect(calls('unknown')).not.toContain('get');
  expect(graph!.getUnresolvedReferencesInFile(name).filter(r => r.referenceName === 'value.get')).toEqual([
    expect.objectContaining({ failureReason: 'unknown-receiver' }),
  ]);
});
it('follows Python dotted imports and aliases without binding the last segment globally', async () => {
  await project({
    'pkg/__init__.py': '', 'pkg/sub.py': 'def run(): return 1\n',
    'app.py': 'import pkg.sub\nimport pkg.sub as alias\ndef known(): return pkg.sub.run()\ndef aliased(): return alias.run()\ndef unknown(): return sub.run()\ndef shadowed(pkg): return pkg.sub.run()\n',
  });
  expect(calls('known')).toContain('run');
  expect(calls('aliased')).toContain('run');
  expect(calls('unknown')).toEqual([]);
  expect(calls('shadowed')).toEqual([]);
});
it('retains typed Kotlin lambda and Go range receivers', async () => {
  await project({
    'app.kt': 'class Widget { fun render() {} }\nfun known(w: Widget) { w.let { it.render() } }\nfun unknown(w: Any) { w.let { it.render() } }\n',
    'app.go': 'package app\ntype Item struct{}\nfunc (i Item) Run() {}\nfunc ranged(items []Item) { for _, item := range items { item.Run() } }\n',
  });
  expect(calls('known')).toContain('render');
  expect(calls('unknown')).not.toContain('render');
  expect(calls('ranged')).toContain('Run');
});
it('does not borrow a same-named type for an external Python annotation', async () => {
  await project({ 'other.py': 'class Store:\n    def get(self): return 1\n', 'app.py': 'from external import Store\ndef unknown(value: Store): return value.get()\n' });
  expect(calls('unknown')).toEqual([]);
});
it('allows project values that shadow host globals', async () => {
  await project({ 'app.ts': 'class Console { log() {} }\nconst console = new Console();\nfunction known() { console.log(); }\nfunction unknown(console: any) { console.log(); }\n' });
  expect(calls('known')).toContain('log');
  expect(calls('unknown')).toEqual([]);
});
it('does not let an unknown inner lambda or a range index inherit the outer value type', async () => {
  await project({
    'app.kt': 'class Widget { fun render() {} }\nfun unknown(w: Widget, other: Any) { w.let { other.let { it.render() } } }\n',
    'app.go': 'package app\ntype Item struct{}\nfunc (i Item) Run() {}\nfunc indexOnly(items []Item) { for item := range items { item.Run() } }\n',
  });
  expect(calls('unknown')).not.toContain('render');
  expect(calls('indexOnly')).not.toContain('Run');
});
it('keeps C++ namespace identity on explicitly typed receivers', async () => {
  await project({ 'app.cpp': 'class Store { public: void get() {} };\nnamespace inner { class Store { public: void run() {} }; }\nvoid unknown(external::Store value) { value.get(); }\nvoid known(inner::Store value) { value.run(); }\n' });
  expect(calls('unknown')).toEqual([]);
  expect(calls('known')).toContain('run');
});
it('does not invent a Python type binding or a PHP class import', async () => {
  await project({
    'other.py': 'class Store:\n    def get(self): return 1\n',
    'app.py': 'def unknown(value: Store): return value.get()\n',
    'Other.php': '<?php\nnamespace Other;\nclass Service { function run() {} }\n',
    'App.php': '<?php\nnamespace App;\nfunction phpUnknown() { $service = new Service(); $service->run(); }\n',
  });
  expect(calls('unknown')).toEqual([]);
  expect(calls('phpUnknown')).not.toContain('run');
});

it.each(['ts', 'tsx'])('resolves imported JavaScript receiver methods from %s', async extension => {
  await project({
    'store.js': 'export class Store { get() { return 1; } }',
    [`app.${extension}`]: "import { Store } from './store.js';\nfunction known(value: Store) { return value.get(); }",
  });
  expect(calls('known')).toContain('get');
});

it('follows Python public class aliases through package imports', async () => {
  await project({
    'pkg/__init__.py': 'from .store import Store as PublicStore\n',
    'pkg/store.py': 'class Store:\n    def get(self): return 1\n',
    'app.py': 'from pkg import PublicStore\ndef known(value: PublicStore): return value.get()\n',
  });
  expect(calls('known')).toContain('get');
});

it.each([
  'from external import Store as PublicStore\n',
  'def private_scope():\n    from .store import Store as PublicStore\n',
  'from .cycle import PublicStore\n',
])('does not guess Python re-export targets for %s', async barrel => {
  await project({
    'pkg/__init__.py': barrel,
    'pkg/cycle.py': 'from . import PublicStore\n',
    'pkg/store.py': 'class Store:\n    def get(self): return 1\n',
    'app.py': 'from pkg import PublicStore\ndef unknown(value: PublicStore): return value.get()\n',
  });
  expect(calls('unknown')).not.toContain('get');
});

it('follows Go factory result positions and named receiver aliases', async () => {
  await project({ 'app.go': `package app
type item struct{}
func (i item) Run() {}
func New() *item { return &item{} }
func Pair() (i *item, ok bool) { return New(), true }
func known() { value := New(); value.Run() }
func paired() { value, _ := Pair(); value.Run() }
func unknown() { _, value := Pair(); value.Run() }
` });
  expect(calls('known')).toContain('Run');
  expect(calls('paired')).toContain('Run');
  expect(calls('unknown')).not.toContain('Run');
});

it('uses Go package value declarations and lowercase parameter types', async () => {
  await project({
    'types.go': 'package app\ntype store struct{}\nfunc (s store) Run() {}\nvar Global = store{}\n',
    'app.go': 'package app\nfunc known(value store) { value.Run() }\nfunc global() { Global.Run() }\nfunc unknown(Global any) { Global.Run() }\n',
  });
  expect(calls('known')).toContain('Run');
  expect(calls('global')).toContain('Run');
  expect(calls('unknown')).not.toContain('Run');
});

it('uses Java enhanced-for element types', async () => {
  await project({ 'App.java': `class Item { void run() {} }
class App {
  void known(Item[] items) { for (Item item : items) { item.run(); } }
  void unknown(Object[] items) { for (Object item : items) { item.run(); } }
}` });
  expect(calls('known')).toContain('run');
  expect(calls('unknown')).not.toContain('run');
});

it('uses a captured Python receiver declaration outside the calling function', async () => {
  await project({ 'app.py': 'class Store:\n    def get(self): return 1\nstore = Store()\ndef known(): return store.get()\ndef unknown(store): return store.get()\n' });
  expect(calls('known')).toContain('get');
  expect(calls('unknown')).not.toContain('get');
});

it('uses a Python parameter captured by a nested method', async () => {
  await project({ 'app.py': 'class Store:\n    def get(self): return 1\ndef outer(store: Store):\n    class Inner:\n        def known(self): return store.get()\n' });
  expect(calls('known')).toContain('get');
});

it('prefers a Python source package over an unrelated test module of the same name', async () => {
  await project({
    'src/pkg/__init__.py': 'from .store import Store\n',
    'src/pkg/store.py': 'class Store:\n    def get(self): return 1\n',
    'tests/fixture/pkg.py': 'class Unrelated: pass\n',
    'app.py': 'from pkg import Store\ndef known(value: Store): return value.get()\n',
  });
  expect(calls('known')).toContain('get');
});

it('resolves inherited Python package receivers during the initial index', async () => {
  await project({
    'src/pkg/__init__.py': 'from .store import Store\n',
    'src/pkg/store.py': 'from .base import Base\nclass Store(Base): pass\n',
    'src/pkg/base.py': 'class Base:\n    def get(self): return 1\n',
    'app.py': 'from pkg import Store\nstore = Store()\ndef known(): return store.get()\n',
  });
  expect(calls('known')).toContain('get');
});

it('follows a Java type parameter with an explicit class bound', async () => {
  await project({ 'App.java': `class Item { void run() {} }
class App {
  <T extends Item> void known(T value) { value.run(); }
  <T> void unknown(T value) { value.run(); }
}` });
  expect(calls('known')).toContain('run');
  expect(calls('unknown')).not.toContain('run');
});

it('resolves Kotlin wildcard-imported types without borrowing another package', async () => {
  await project({
    'first/Store.kt': 'package first\nobject Store { fun run() {} }\n',
    'second/Store.kt': 'package second\nobject Store { fun other() {} }\n',
    'app.kt': 'package app\nimport first.*\nfun known() { Store.run() }\nfun unknown() { Store.other() }\n',
  });
  expect(calls('known')).toContain('run');
  expect(calls('unknown')).not.toContain('other');
});

it('uses PHP instanceof evidence only in an unchanged positive branch', async () => {
  await project({ 'app.php': `<?php
class Store { function run() {} }
function known($value) { if ($value instanceof Store) { $value->run(); } }
function outside($value) { if ($value instanceof Store) {} else { $value->run(); } }
function reassigned($value, $other) { if ($value instanceof Store) { $value = $other; $value->run(); } }
function shadowed($value) { if ($value instanceof Store) { $f = function($value) { $value->run(); }; } }
` });
  expect(calls('known')).toContain('run');
  expect(calls('outside')).not.toContain('run');
  expect(calls('reassigned')).not.toContain('run');
  expect(calls('shadowed')).not.toContain('run');
  const run = graph!.getNodesByKind('method').find(n => n.name === 'run')!;
  expect(graph!.getIncomingEdges(run.id).filter(e => e.kind === 'calls')).toHaveLength(1);
});

it('allows Go values to shadow standard-library package names', async () => {
  await project({ 'app.go': 'package app\ntype Printer struct{}\nfunc (p Printer) Println() {}\nfunc known(fmt Printer) { fmt.Println() }\nfunc unknown(fmt any) { fmt.Println() }\n' });
  expect(calls('known')).toContain('Println');
  expect(calls('unknown')).not.toContain('Println');
});

it('does not borrow another class member from a JVM import target file', async () => {
  await project({
    'pkg/Known.java': 'package pkg;\npublic class Known { public static void own() {} }\nclass Decoy { public static void other() {} }\n',
    'App.java': 'import pkg.Known;\nclass App { void known() { Known.own(); } void unknown() { Known.other(); } }\n',
  });
  expect(calls('known')).toContain('own');
  expect(calls('unknown')).not.toContain('other');
});
