/**
 * Go and Python receivers are typed where the code binds them.
 *
 * - Go `if ready()` / `for ready()` / `switch ready()` with a `ready func()`
 *   parameter is a bare call, never the same-named method.
 * - A Go range over `o.Items` types `o` at the range, not at a call in the
 *   loop body that sits after a shadowing `o := …`.
 * - A Python parameter that shadows an import scopes `obj.fetch` by its
 *   annotation, not by the import.
 * - A Python field reassigned from an untyped value no longer has the type
 *   of its first constructor assignment.
 * - `self . fetch` and `c . Fetch` are the same member values as their
 *   unspaced forms.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
const graphs: CodeGraph[] = [];
afterEach(() => {
  for (const cg of graphs.splice(0)) cg.close();
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function indexed(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-gopy-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), content);
  const cg = await CodeGraph.init(root, { index: true });
  graphs.push(cg);
  return cg;
}

/**
 * Edges out of the function or method named `from`, as `Owner::name`:
 * `calls`, or the function-value `references` (fnRef) edges.
 */
function targets(cg: CodeGraph, from: string, kind: 'calls' | 'references'): string[] {
  const source = cg.getNodesByName(from).find((n) => n.kind === 'function' || n.kind === 'method');
  if (!source) throw new Error(`no function ${from}`);
  return cg
    .getOutgoingEdges(source.id)
    .filter((e) => e.kind === kind && (kind === 'calls' || e.metadata?.fnRef === true))
    .map((e) => cg.getNode(e.target)!.qualifiedName)
    .sort();
}

describe('Go and Python receiver scope', () => {
  it('Go: a func parameter called after if/for/switch is not a method call', async () => {
    const cg = await indexed({
      'main.go': [
        'package main',
        'type Loop struct{}',
        'func (l *Loop) ready() bool { return true }',
        'func waitIf(ready func() bool) {\n\tif ready() {\n\t}\n}',
        'func waitFor(ready func() bool) {\n\tfor ready() {\n\t}\n}',
        'func waitSwitch(ready func() bool) {\n\tswitch ready() {\n\t}\n}',
        '',
      ].join('\n'),
    });
    for (const fn of ['waitIf', 'waitFor', 'waitSwitch']) expect(targets(cg, fn, 'calls')).toEqual([]);
  });

  it('Go: a range owner is typed at the range, not after a shadowing declaration', async () => {
    const cg = await indexed({
      'main.go': [
        'package main',
        'type A struct{}',
        'func (A) Run() {}',
        'type B struct{}',
        'func (B) Run() {}',
        'type OuterA struct{ Items []A }',
        'type OuterB struct{ Items []B }',
        'func loop() {',
        '\to := OuterA{}',
        '\tfor _, v := range o.Items {',
        '\t\to := OuterB{}',
        '\t\t_ = o',
        '\t\tv.Run()',
        '\t}',
        '}',
        '',
      ].join('\n'),
    });
    expect(targets(cg, 'loop', 'calls')).toEqual(['A::Run']);
  });

  it('Python: a parameter that shadows an import scopes the member by its annotation', async () => {
    const cg = await indexed({
      'lib.py': 'class Store:\n    def fetch(self):\n        pass\n',
      'other.py': 'class Other:\n    def fetch(self):\n        pass\n',
      'main.py': [
        'from lib import Store as obj',
        'from other import Other',
        '',
        'def work(pool, obj: Other):',
        '    pool.submit(obj.fetch)',
        '',
        'def control(pool):',
        '    pool.submit(obj.fetch)',
        '',
      ].join('\n'),
    });
    expect(targets(cg, 'work', 'references')).toEqual(['Other::fetch']);
    expect(targets(cg, 'control', 'references')).toEqual(['Store::fetch']);
  });

  it('Python: an untyped reassignment leaves a field without a type', async () => {
    const svc = (reassign: string) => [
      'class Store:',
      '    def fetch(self):',
      '        pass',
      '',
      'class Cache:',
      '    def fetch(self):',
      '        pass',
      '',
      'class Svc:',
      '    def __init__(self, replacement):',
      '        self.store = None',
      '        self.store = Store()',
      reassign,
      '',
      '    def go(self, pool):',
      '        pool.submit(self.store.fetch)',
      '',
    ].join('\n');
    const reassigned = await indexed({ 'svc.py': svc('        self.store = replacement') });
    expect(targets(reassigned, 'go', 'references')).toEqual([]);
    // `None` is no competing type.
    const kept = await indexed({ 'svc.py': svc('        pass') });
    expect(targets(kept, 'go', 'references')).toEqual(['Store::fetch']);
  });

  it('spaces around the dot do not hide a member value', async () => {
    const py = await indexed({
      'm.py': [
        'def register(cb):',
        '    pass',
        '',
        'class Store:',
        '    def fetch(self):',
        '        pass',
        '    def wire(self):',
        '        register(self . fetch)',
        '',
      ].join('\n'),
    });
    expect(targets(py, 'wire', 'references')).toEqual(['Store::fetch']);
    const go = await indexed({
      'm.go': [
        'package m',
        'type Client struct{}',
        'func (c *Client) Fetch() {}',
        'func Submit(f func()) {}',
        'func Wire(c *Client) {',
        '\tSubmit(c . Fetch)',
        '}',
        '',
      ].join('\n'),
    });
    expect(targets(go, 'Wire', 'references')).toEqual(['Client::Fetch']);
  });
});
