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
  dir = undefined;
});
async function project(files: Record<string, string>) {
  graph?.close();
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-receiver-scope-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
  graph.resolveReferences();
  return graph;
}
function edges(caller: string, kind: 'calls' | 'references') {
  const from = [...graph!.getNodesByKind('function'), ...graph!.getNodesByKind('method')].find(n => n.name === caller)!;
  expect(from).toBeDefined();
  return graph!.getOutgoingEdges(from.id).filter(e => e.kind === kind)
    .map(e => `${e.line}:${graph!.getNode(e.target)!.qualifiedName}`).sort();
}

describe('receiver inference stays in scope', () => {
  it('does not bind a Go f().m to a same-named method when f holds a function value', async () => {
    const source = (decl: string) => `package service
type Table struct{}
func (t *Table) Reload() error { return nil }
${decl}
type Cache struct{}
func (c *Cache) Reload() error { return nil }
func Handle() error { return SysTable().Reload() }
`;
    await project({ 'service/table.go': source('var SysTable = func() *Table { return &Table{} }') });
    expect(edges('Handle', 'calls')).toContain('7:Table::Reload');
    expect(edges('Handle', 'calls')).not.toContain('7:Cache::Reload');
    await project({ 'service/table.go': source('var SysTable = pick()') + 'func pick() func() *Table { return nil }\n' });
    expect(edges('Handle', 'calls').filter(e => e.endsWith('::Reload'))).toEqual([]);
    await project({ 'service/table.go': source('func SysTable() *Table { return &Table{} }') });
    expect(edges('Handle', 'calls')).toContain('7:Table::Reload');
  });

  it('types a Go sync.OnceValue function value from its literal, through the import', async () => {
    await project({
      'go.mod': 'module example.com/m\n\ngo 1.21\n',
      'core/engine.go': `package core
type Engine struct{}
func (e *Engine) Serve() {}
`,
      'wrap/wrap.go': `package wrap
import (
	"sync"
	"example.com/m/core"
)
var engine = sync.OnceValue(func() *core.Engine {
	return &core.Engine{}
})
func Serve() { engine().Serve() }
`,
    });
    const serve = graph!.getNodesByKind('function').find(n => n.name === 'Serve')!;
    const targets = graph!.getOutgoingEdges(serve.id).filter(e => e.kind === 'calls')
      .map(e => graph!.getNode(e.target)!).filter(n => n.name === 'Serve')
      .map(n => `${n.filePath}:${n.qualifiedName}`);
    expect(targets).toEqual(['core/engine.go:Engine::Serve']);
  });

  it('types a Go bare New() from the calling package, not the first same-named function', async () => {
    await project({
      'aaa/aaa.go': `package aaa
type Widget struct{}
func (w *Widget) Bar() {}
func New() *Widget { return &Widget{} }
`,
      'main.go': `package main
type Foo struct{}
func New() *Foo { return &Foo{} }
func (f *Foo) Bar() {}
func caller() { New().Bar() }
`,
    });
    expect(edges('caller', 'calls')).toContain('5:Foo::Bar');
    expect(edges('caller', 'calls')).not.toContain('5:Widget::Bar');
  });

  it('does not bind a Go method value on an external package type to a same-named project type', async () => {
    const source = (param: string) => `package demo
import "database/sql"
type DB struct{}
func (d *DB) Ping() error { return nil }
func Typed(s ${param}) { cb := s.Ping; _ = cb }
`;
    await project({ 'main.go': source('*sql.DB') });
    expect(edges('Typed', 'references')).not.toContain('5:DB::Ping');
    await project({ 'main.go': source('*DB') });
    expect(edges('Typed', 'references')).toContain('5:DB::Ping');
  });

  it('keeps a Go method value on an imported project package type', async () => {
    await project({
      'go.mod': 'module example.com/m\n\ngo 1.21\n',
      'store/db.go': `package store
type DB struct{}
func (d *DB) Ping() error { return nil }
`,
      'main.go': `package main
import "example.com/m/store"
type DB struct{}
func (d *DB) Ping() error { return nil }
func Typed(s *store.DB) { cb := s.Ping; _ = cb }
`,
    });
    const typed = graph!.getNodesByKind('function').find(n => n.name === 'Typed')!;
    const pings = graph!.getOutgoingEdges(typed.id)
      .map(e => graph!.getNode(e.target)!)
      .filter(n => n.name === 'Ping')
      .map(n => n.filePath);
    expect(pings).toEqual(['store/db.go']);
  });

  it('types a CFML variables.x receiver from the component scope, not a function-local var', async () => {
    const handler = (local: string) => `component {
  function handle() {
    var ${local} = new OrderService();
    return variables.svc.save(1);
  }
  function init() {
    variables.svc = new UserService();
    return this;
  }
}
`;
    const services = {
      'svc/OrderService.cfc': 'component {\n  function save(any o) { return o; }\n}\n',
      'svc/UserService.cfc': 'component {\n  function save(any u) { return u; }\n}\n',
    };
    await project({ ...services, 'handlers/Fielded.cfc': handler('svc') });
    expect(edges('handle', 'calls')).toContain('4:UserService::save');
    expect(edges('handle', 'calls')).not.toContain('4:OrderService::save');
    await project({ ...services, 'handlers/Fielded.cfc': handler('other') });
    expect(edges('handle', 'calls')).toContain('4:UserService::save');
  });

  it('does not resolve a store accessor chain whose accessor a parameter shadows', async () => {
    const store = (params: string) => `import { create } from 'zustand';
export const useStore = create((set, get) => ({
  reset() { set({}); },
  bump(${params}) { get().reset(); },
}));
`;
    const pkg = '{"name":"z","dependencies":{"zustand":"^4.0.0"}}';
    await project({ 'package.json': pkg, 'src/zstore.ts': store('get: any') });
    expect(edges('bump', 'calls')).not.toContain('4:reset');
    await project({ 'package.json': pkg, 'src/zstore.ts': store('') });
    expect(edges('bump', 'calls')).toContain('4:reset');
  });

  it('follows the Python MRO for a method value under multiple inheritance', async () => {
    const source = (bases: string) => `class Store:
    def fetch(self):
        return 1

class Other:
    def fetch(self):
        return 2

class Child(${bases}):
    def inherited(self, pool):
        pool.submit(self.fetch)
`;
    await project({ 'main.py': source('Store, Other') });
    expect(edges('inherited', 'references')).toEqual(['11:Store::fetch']);
    await project({ 'main.py': source('Other, Store') });
    expect(edges('inherited', 'references')).toEqual(['11:Other::fetch']);
    await project({ 'main.py': source('Store') });
    expect(edges('inherited', 'references')).toEqual(['11:Store::fetch']);
  });
});
