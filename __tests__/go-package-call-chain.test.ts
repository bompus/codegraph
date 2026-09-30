/**
 * Go `pkg.New().Bar()` calls `Bar` on the result of a function the file
 * reaches through an import. The method belongs to `New`'s result type in
 * that package, so a same-named method in another package is never the
 * target. An import from outside the module gives no edge, and chains whose
 * receiver is a local value keep their existing handling.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

async function callees(files: Record<string, string>, fromName: string): Promise<string[]> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-go-pkg-chain-'));
  for (const [rel, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), source);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  cg.resolveReferences();
  const from = cg.getNodesByKind('function').find((n) => n.name === fromName)!;
  expect(from).toBeDefined();
  return cg
    .getOutgoingEdges(from.id)
    .filter((e) => e.kind === 'calls')
    .map((e) => cg!.getNode(e.target))
    .filter((n): n is NonNullable<typeof n> => !!n)
    .map((n) => `${n.kind}:${n.qualifiedName}@${n.filePath}`);
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

const PACKAGES = {
  'go.mod': 'module example.com/m\n\ngo 1.21\n',
  'bbb/bbb.go': 'package bbb\n\ntype Foo struct{}\n\nfunc (f *Foo) Bar() {}\n\nfunc New() *Foo { return &Foo{} }\n',
  'aaa/aaa.go': 'package aaa\n\ntype Widget struct{}\n\nfunc (w *Widget) Bar() {}\n\nfunc New() *Widget { return &Widget{} }\n',
};

const methods = (out: string[]) => out.filter((c) => c.startsWith('method:'));

describe('Go pkg.New().Method() follows the imported function result type', () => {
  it('links to the method of the imported package, not a same-named one elsewhere', async () => {
    const out = await callees(
      { ...PACKAGES, 'cmd/main.go': 'package main\n\nimport "example.com/m/bbb"\n\nfunc caller() { bbb.New().Bar() }\n' },
      'caller'
    );
    expect(methods(out)).toEqual(['method:Foo::Bar@bbb/bbb.go']);
  });

  it('links to the other package when that is the one imported', async () => {
    const out = await callees(
      { ...PACKAGES, 'cmd/main.go': 'package main\n\nimport "example.com/m/aaa"\n\nfunc caller() { aaa.New().Bar() }\n' },
      'caller'
    );
    expect(methods(out)).toEqual(['method:Widget::Bar@aaa/aaa.go']);
  });

  it('follows an import alias', async () => {
    const out = await callees(
      { ...PACKAGES, 'cmd/main.go': 'package main\n\nimport w "example.com/m/aaa"\n\nfunc caller() { w.New().Bar() }\n' },
      'caller'
    );
    expect(methods(out)).toEqual(['method:Widget::Bar@aaa/aaa.go']);
  });

  it('makes no edge through a package outside the module', async () => {
    const out = await callees(
      { ...PACKAGES, 'cmd/main.go': 'package main\n\nimport "github.com/other/bbb"\n\nfunc caller() { bbb.New().Bar() }\n' },
      'caller'
    );
    expect(methods(out)).toEqual([]);
  });
});
