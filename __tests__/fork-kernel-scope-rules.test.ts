/**
 * Fork additions to the upstream scope rules (#2126, #2128), ported to the kernel:
 * an anonymous Java class reaches the methods of the type it instantiates, and a
 * JS/TS file's own declaration of a built-in's name (`function fetch`) is that
 * declaration, not the built-in.
 */
import { describe, it, expect, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
afterAll(() => {
  for (const r of roots.splice(0)) fs.rmSync(r, { recursive: true, force: true });
});

async function index(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-fork-scope-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

function callsFrom(cg: CodeGraph, name: string, kind: string): string[] {
  const from = cg.getNodesByName(name).find((n) => n.kind === kind)!;
  return cg
    .getOutgoingEdgesFrom([from.id], ['calls'])
    .map((e) => {
      const t = cg.getNode(e.target)!;
      return `${t.qualifiedName}@${t.filePath}`;
    })
    .sort();
}

describe('fork scope rules', () => {
  it('Java: a bare call in an anonymous class reaches the instantiated type', async () => {
    const cg = await index({
      'src/main/java/app/Base.java': `package app;
public class Base {
  protected void helper() {}
}
`,
      'src/main/java/app/Plain.java': `package app;
public class Plain {
  public Object make() {
    return new Base() {
      void go() {
        helper();
      }
    };
  }
}
`,
    });
    try {
      expect(callsFrom(cg, 'go', 'method')).toEqual(['app::Base::helper@src/main/java/app/Base.java']);
    } finally {
      cg.close();
    }
  });

  it('TS: a declared function named like a JS built-in takes its same-file calls', async () => {
    const cg = await index({
      'src/net.ts': `export function fetch(url: string): string {
  return url;
}

export function load(): string {
  return fetch('/api');
}
`,
    });
    try {
      expect(callsFrom(cg, 'load', 'function')).toEqual(['fetch@src/net.ts']);
    } finally {
      cg.close();
    }
  });
});
