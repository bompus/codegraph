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
  graph.resolveReferences();
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
