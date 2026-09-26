/**
 * A bare name never binds across a code boundary: languages that cannot name
 * each other's symbols (Rust and Scala, Python and TypeScript) share a name by
 * coincidence, so the match is declined. Languages that do interoperate keep
 * resolving across files: Kotlin calling Java, Objective-C calling C. Each
 * case pairs the declined shape with an interop shape of the same form.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

function project(files: Record<string, string>): void {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cross-language-'));
  for (const [rel, content] of Object.entries(files)) {
    const abs = path.join(tempDir, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, content);
  }
}

/** Outgoing `kind` edge targets of the function/method named `from`, as `file:name`. */
async function targetsOf(from: string, kind = 'calls'): Promise<string[]> {
  cg = await CodeGraph.init(tempDir, { index: true });
  cg.resolveReferences();
  const source = cg.getNodesByKind('function').concat(cg.getNodesByKind('method')).find((n) => n.name === from)!;
  expect(source).toBeDefined();
  return cg
    .getOutgoingEdges(source.id)
    .filter((e) => e.kind === kind)
    .map((e) => cg!.getNode(e.target))
    .filter((n): n is NonNullable<typeof n> => !!n)
    .map((n) => `${n.filePath}:${n.name}`);
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe('cross-language name matches', () => {
  it('declines a Rust call onto a same-named Scala method', async () => {
    project({
      'src/lib.rs': 'pub fn run() -> u32 {\n    shout()\n}\n',
      'app/Loud.scala': 'object Loud {\n  def shout(): Int = 1\n}\n',
    });
    expect(await targetsOf('run')).not.toContain('app/Loud.scala:shout');
  });

  it('declines a Python call onto a same-named TypeScript function', async () => {
    project({
      'tools/gen.py': 'def glyph():\n    return round(1.5)\n',
      'ui/svg.ts': 'export function round(n: number): number {\n  return n;\n}\n',
    });
    expect(await targetsOf('glyph')).not.toContain('ui/svg.ts:round');
  });

  it('keeps a Kotlin call onto a Java method (one JVM)', async () => {
    project({
      'src/Main.kt': 'fun main() {\n    shout()\n}\n',
      'src/Loud.java': 'public class Loud {\n    public static int shout() { return 1; }\n}\n',
    });
    expect(await targetsOf('main')).toContain('src/Loud.java:shout');
  });

  it('keeps an Objective-C call onto a C function (ObjC is a C superset)', async () => {
    project({
      'app/View.m': '#import "util.h"\nvoid render(void) {\n    clampValue(3);\n}\n',
      'app/util.h': 'int clampValue(int v);\n',
      'app/util.c': 'int clampValue(int v) {\n    return v;\n}\n',
    });
    const targets = await targetsOf('render');
    expect(targets.some((t) => t.endsWith(':clampValue'))).toBe(true);
  });
});
