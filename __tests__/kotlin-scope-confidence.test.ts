/**
 * Fork rules on top of upstream's Kotlin top-level visibility (#2146).
 *
 * - Removing invisible top-level functions is no evidence for what is left.
 *   A bare `render(1)` in an unrelated class must not settle on the one
 *   other class's `render` at trusted confidence because a top-level
 *   `render` in a package the file never imports dropped out. A member of
 *   the calling class's own hierarchy is still bound.
 * - A call written after a qualifier (`app.config.Key<String>("x")`) reaches
 *   the resolver as its bare name; the qualifier, not the file's imports,
 *   names it.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-scope-confidence-'));
  const files: Record<string, string> = {
    'a/Fmt.kt': `package a

fun render(x: Int) {}

fun paint(x: Int) {}
`,
    'b/Report.kt': `package b

class Report {
  fun render(x: Int) {}
}
`,
    'b/Base.kt': `package b

open class Base {
  fun paint(x: Int) {}
}
`,
    'app/config/Key.kt': `package app.config

class Key<T>(val name: String)
`,
    'c/Main.kt': `package c

class Main {
  fun run() {
    render(1)
  }
}
`,
    'c/Child.kt': `package c

import b.Base

class Child : Base() {
  fun run() {
    paint(1)
  }
}
`,
    'c/Use.kt': `package c

class Use {
  fun build() = app.config.Key<String>("x")
}
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

function edgesFrom(file: string): Array<{ target: string; confidence: number }> {
  const ids = cg.getNodesInFile(file).map((n) => n.id);
  return cg
    .getOutgoingEdgesFrom(ids)
    .filter((e) => e.kind === 'calls' || e.kind === 'instantiates')
    .map((e) => ({
      target: cg.getNode(e.target)!.qualifiedName,
      confidence: Number((e.metadata as { confidence?: number } | undefined)?.confidence ?? 0),
    }));
}

describe('Kotlin names left over after the visibility rule', () => {
  it('a class member outside the call site’s hierarchy stays below trusted confidence', () => {
    const report = edgesFrom('c/Main.kt').filter((e) => e.target.endsWith('Report::render'));
    expect(report.every((e) => e.confidence < 0.8)).toBe(true);
  });

  it('a member of the calling class’s supertype keeps trusted confidence', () => {
    const base = edgesFrom('c/Child.kt').filter((e) => e.target.endsWith('Base::paint'));
    expect(base).toHaveLength(1);
    expect(base[0]!.confidence).toBeGreaterThanOrEqual(0.9);
  });

  it('a qualified call reaches the class its qualifier names without an import', () => {
    expect(edgesFrom('c/Use.kt').map((e) => e.target)).toContain('app.config::Key');
  });
});
