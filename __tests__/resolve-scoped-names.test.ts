/**
 * A name the resolver reads at a call site means what it means there:
 *
 * - PHP, TypeScript: the supertype walk for a receiver typed `Sub` follows
 *   the `Sub` the file names, not a same-named class in another namespace or
 *   module.
 * - Kotlin: `w.let { it.x() }` types `it` as `w` only for the stdlib `let`;
 *   a member `let` on `w`'s class decides what its lambda receives.
 * - C: `#define FLAG BACKING` reads `BACKING` where `#if FLAG` is evaluated,
 *   not where `FLAG` was defined.
 * - Python: an assignment inside a nested `def` binds that function's local,
 *   not the enclosing function's receiver.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

async function edgesFrom(
  files: Record<string, string>,
  fromName: string,
  kind: 'calls' | 'references'
): Promise<string[]> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-scoped-names-'));
  for (const [rel, source] of Object.entries(files)) {
    fs.writeFileSync(path.join(tempDir, rel), source);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  cg.resolveReferences();
  const from = [...cg.getNodesByKind('function'), ...cg.getNodesByKind('method')].find(
    (n) => n.name === fromName
  )!;
  expect(from).toBeDefined();
  return cg
    .getOutgoingEdges(from.id)
    .filter((e) => e.kind === kind)
    .map((e) => cg!.getNode(e.target))
    .filter((n): n is NonNullable<typeof n> => !!n)
    .map((n) => `${n.kind}:${n.qualifiedName}@${n.filePath}`)
    .sort();
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe('PHP supertype walk follows the class the site names', () => {
  const app =
    '<?php\n' +
    '%USE%class App {\n' +
    '  public function __construct(private Sub $s) {}\n' +
    '  public function run() { return $this->s->baseMethod(); }\n' +
    '}\n';
  const files = (decoy: string, use = '') => ({
    'Decoy.php':
      '<?php\nnamespace Other;\nclass DecoyBase { public function baseMethod() { return 1; } }\n' +
      `class ${decoy} extends DecoyBase {}\n`,
    'Sub.php': '<?php\nclass Sub { public function other() { return 2; } }\n',
    'App.php': app.replace('%USE%', use),
  });

  it('does not borrow the parent of a same-named class in another namespace', async () => {
    expect(await edgesFrom(files('Sub'), 'run', 'calls')).toEqual([]);
  });

  it('control: no edge when the other class has a different name', async () => {
    expect(await edgesFrom(files('Sub2'), 'run', 'calls')).toEqual([]);
  });

  it('follows the namespaced class when the file imports it', async () => {
    expect(await edgesFrom(files('Sub', 'use Other\\Sub;\n'), 'run', 'calls')).toEqual([
      'method:Other::DecoyBase::baseMethod@Decoy.php',
    ]);
  });
});

describe('TypeScript supertype walk follows the class the module declares', () => {
  const files = (sub: string) => ({
    'decoy.ts': `export class DecoyBase { baseMethod() {} }\nexport class ${sub} extends DecoyBase {}\n`,
    'app.ts':
      'class Sub { other() {} }\n' +
      'export class App {\n  constructor(private s: Sub) {}\n  run() { this.s.baseMethod(); }\n}\n',
  });

  it("does not borrow the parent of another module's same-named class", async () => {
    expect(await edgesFrom(files('Sub'), 'run', 'calls')).toEqual([]);
  });

  it('control: no edge when the other class has a different name', async () => {
    expect(await edgesFrom(files('Sub2'), 'run', 'calls')).toEqual([]);
  });
});

describe('Kotlin let/also lambda receiver', () => {
  const source = (member: string) =>
    'class Other { fun render() {} }\n' +
    'class Widget {\n' +
    '    fun render() {}\n' +
    `    fun ${member}(block: (Other) -> Unit) { block(Other()) }\n` +
    '}\n' +
    'fun known(w: Widget) { w.let { it.render() } }\n';

  it("does not type `it` as the receiver when the receiver's class declares `let`", async () => {
    const out = await edgesFrom({ 'app.kt': source('let') }, 'known', 'calls');
    expect(out).not.toContain('method:Widget::render@app.kt');
    expect(out).toContain('method:Widget::let@app.kt');
  });

  it('control: the stdlib `let` passes the receiver as `it`', async () => {
    expect(await edgesFrom({ 'app.kt': source('lat') }, 'known', 'calls')).toEqual([
      'method:Widget::render@app.kt',
    ]);
  });
});

describe('C object-macro alias in #if', () => {
  const conditional = '#if ENABLE_TRACE\n#define CONDITIONAL(v) ((void)(v))\n#endif\n';
  const run = (unit: string) =>
    edgesFrom(
      { 'unit.c': unit, 'decoy.c': 'void CONDITIONAL(int x) {}\n', 'conditional.h': conditional },
      'changed_flag',
      'calls'
    );

  it('reads the aliased macro when the condition is evaluated', async () => {
    const unit =
      '#define BACKING 0\n#define ENABLE_TRACE BACKING\n#include "conditional.h"\n' +
      '#undef BACKING\n#define BACKING 1\n#include "conditional.h"\n' +
      'void changed_flag() { CONDITIONAL(1); }\n';
    expect(await run(unit)).toEqual([]);
  });

  it('an alias whose target turns false leaves the macro undefined', async () => {
    const unit =
      '#define BACKING 1\n#define ENABLE_TRACE BACKING\n#undef BACKING\n#define BACKING 0\n' +
      '#include "conditional.h"\nvoid changed_flag() { CONDITIONAL(1); }\n';
    expect(await run(unit)).toEqual(['function:CONDITIONAL@decoy.c']);
  });

  it('control: a macro redefined directly', async () => {
    const unit =
      '#define BACKING 0\n#define ENABLE_TRACE 0\n#include "conditional.h"\n' +
      '#undef ENABLE_TRACE\n#define ENABLE_TRACE 1\n#include "conditional.h"\n' +
      'void changed_flag() { CONDITIONAL(1); }\n';
    expect(await run(unit)).toEqual([]);
  });

  it('control: an alias defined after its target is true', async () => {
    const unit =
      '#define BACKING 1\n#define ENABLE_TRACE BACKING\n#include "conditional.h"\n' +
      'void changed_flag() { CONDITIONAL(1); }\n';
    expect(await run(unit)).toEqual([]);
  });
});

describe('Python receiver type ignores nested def bodies', () => {
  const source = (local: string) =>
    'class Store:\n    def fetch(self):\n        return 1\n' +
    'class Data:\n    def __init__(self):\n        self.fetch = 42\n' +
    'def data(obj: Data, pool):\n' +
    '    def unrelated():\n' +
    `        ${local} = Store()\n` +
    '    pool.submit(obj.fetch)\n';

  it('keeps the parameter annotation over a nested function local', async () => {
    expect(await edgesFrom({ 'main.py': source('obj') }, 'data', 'references')).toEqual([]);
  });

  it("still reads an assignment in the function's own body", async () => {
    const own = source('oth').replace('    pool.submit', '    obj = Store()\n    pool.submit');
    expect(await edgesFrom({ 'main.py': own }, 'data', 'references')).toEqual([
      'method:Store::fetch@main.py',
    ]);
  });

  it('control: no edge when the nested local has another name', async () => {
    expect(await edgesFrom({ 'main.py': source('oth') }, 'data', 'references')).toEqual([]);
  });
});
