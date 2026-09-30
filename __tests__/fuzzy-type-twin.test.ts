/**
 * Fuzzy matching settles a name only one callable definition carries. A class
 * that shares its exact name with an interface or type alias is not such a
 * name: vitest's browser tests import `type { Locator }` from the public
 * `vitest/browser` entry, which is the interface `context.d.ts` declares, and
 * fuzzy used to land every one of those references on the tester's abstract
 * `class Locator` because an interface is not a callable kind. An interface
 * in the class's own file is a TypeScript declaration merge, the same symbol,
 * and does not count.
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

async function project(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-fuzzy-type-twin-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

const targets = (cg: CodeGraph, file: string, resolvedBy?: string): string[] =>
  cg
    .getOutgoingEdgesFrom(cg.getNodesInFile(file).map((n) => n.id))
    .filter((e) => e.kind !== 'contains')
    .filter((e) => !resolvedBy || (e.metadata as { resolvedBy?: string } | undefined)?.resolvedBy === resolvedBy)
    .map((e) => cg.getNode(e.target)!)
    .map((n) => `${n.kind}:${n.name}@${n.filePath}`)
    .sort();

describe('fuzzy matching and a class with a same-named type', () => {
  it('does not settle a reference on the class when an interface shares its name', async () => {
    const cg = await project({
      'package.json': '{"name":"root","private":true,"workspaces":["packages/*"]}',
      'packages/browser/package.json': '{"name":"@vitest/browser","exports":{"./context":{"types":"./context.d.ts"}}}',
      'packages/browser/context.d.ts': 'export interface Locator {\n  click(): Promise<void>\n}\n',
      'packages/browser/src/client/tester/locators.ts': 'export abstract class Locator {\n  abstract click(): Promise<void>\n}\n',
      'packages/vitest/package.json': '{"name":"vitest","exports":{"./browser":{"types":"./browser/context.d.ts"}}}',
      'packages/vitest/browser/context.d.ts': "export * from '@vitest/browser/context'\n",
      'test/browser/fixtures/basic.test.tsx': `import type { Locator } from 'vitest/browser';

export function first(all: Locator[]): Locator {
  return all[0]
}
`,
    });
    try {
      expect(targets(cg, 'test/browser/fixtures/basic.test.tsx', 'fuzzy')).toEqual([]);
    } finally {
      cg.close();
    }
  });

  it('keeps the class when its twin is a declaration merge in the same file', async () => {
    const cg = await project({
      'package.json': '{"name":"root","private":true,"workspaces":["packages/*"]}',
      'packages/browser/package.json': '{"name":"@vitest/browser","exports":{"./context":{"types":"./context.d.ts"}}}',
      'packages/browser/context.d.ts': 'export interface BrowserPage {\n  reload(): Promise<void>\n}\n',
      'packages/browser/src/client/tester/locators.ts':
        'export interface Locator {\n  selector: string\n}\nexport abstract class Locator {\n  abstract click(): Promise<void>\n}\n',
      'packages/vitest/package.json': '{"name":"vitest","exports":{"./browser":{"types":"./browser/context.d.ts"}}}',
      'packages/vitest/browser/context.d.ts': "export * from '@vitest/browser/context'\n",
      'test/browser/fixtures/basic.test.tsx': `import type { Locator } from 'vitest/browser';

export function first(all: Locator[]): Locator {
  return all[0]
}
`,
    });
    try {
      expect(targets(cg, 'test/browser/fixtures/basic.test.tsx', 'fuzzy')).toContain(
        'class:Locator@packages/browser/src/client/tester/locators.ts',
      );
    } finally {
      cg.close();
    }
  });

  it('still reaches the class when no type shares its name', async () => {
    const cg = await project({
      'package.json': '{"name":"root","private":true,"workspaces":["packages/*"]}',
      'packages/browser/package.json': '{"name":"@vitest/browser","exports":{"./context":{"types":"./context.d.ts"}}}',
      'packages/browser/context.d.ts': 'export interface BrowserPage {\n  reload(): Promise<void>\n}\n',
      'packages/browser/src/client/tester/locators.ts': 'export abstract class Locator {\n  abstract click(): Promise<void>\n}\n',
      'packages/vitest/package.json': '{"name":"vitest","exports":{"./browser":{"types":"./browser/context.d.ts"}}}',
      'packages/vitest/browser/context.d.ts': "export * from '@vitest/browser/context'\n",
      'test/browser/fixtures/basic.test.tsx': `import type { Locator } from 'vitest/browser';

export function first(all: Locator[]): Locator {
  return all[0]
}
`,
    });
    try {
      expect(targets(cg, 'test/browser/fixtures/basic.test.tsx')).toContain(
        'class:Locator@packages/browser/src/client/tester/locators.ts',
      );
    } finally {
      cg.close();
    }
  });
});
