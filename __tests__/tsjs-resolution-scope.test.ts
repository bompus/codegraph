/**
 * TS/JS receivers and bindings resolve from the code that declares them.
 *
 * - A parameter named like a module-level factory hides it: neither the call
 *   nor the member call on its result reaches the factory's type.
 * - An awaited or imported value's type is looked up in the file that
 *   declares it, not in the caller's file.
 * - A regex literal's braces do not end an object literal early, and a
 *   `return` inside a nested callback does not type the outer factory.
 * - With a framework active, the object-literal rules still decide a
 *   member alias (`{ getState: wrong, ...override }` stays unresolved).
 * - Same-line namesakes keep their own binding node ids.
 * - A point lookup at the first character of a node that abuts the previous
 *   one (`if(ok)target()`) returns that node.
 * - `window.` chains are normalized like other identifier chains.
 */
import { describe, it, expect, afterEach, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { extractFromSource } from '../src/extraction';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { attachBindingNodeIds } from '../src/extraction/kernel';
import { parseSourceTreeSync } from '../src/extraction/parse-tree';
import { guardsInTree } from '../src/graph/branch-guards';
import type { Binding, Node } from '../src/types';

beforeAll(async () => {
  await initGrammars();
  await loadGrammarsForLanguages(['typescript']);
});

const roots: string[] = [];
const graphs: CodeGraph[] = [];
afterEach(() => {
  for (const cg of graphs.splice(0)) cg.close();
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function indexed(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-tsjs-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), content);
  const cg = await CodeGraph.init(root, { index: true });
  graphs.push(cg);
  return cg;
}

/** `calls` edges out of the function named `from`, as `qualifiedName@file`. */
function calls(cg: CodeGraph, from: string): string[] {
  const source = cg.getNodesByName(from).find((n) => n.kind === 'function' || n.kind === 'method');
  if (!source) throw new Error(`no function ${from}`);
  return cg
    .getOutgoingEdges(source.id)
    .filter((e) => e.kind === 'calls')
    .map((e) => {
      const t = cg.getNode(e.target)!;
      return `${t.qualifiedName}@${path.basename(t.filePath)}`;
    })
    .sort();
}

const TWO_CLASSES = 'export class Engine { run(): void {} }\nexport class Other { run(): void {} }\n';

describe('TS/JS resolution scope', () => {
  it('a parameter hides a same-named factory', async () => {
    const cg = await indexed({
      'use.ts':
        TWO_CLASSES +
        'export function make(): Engine { return new Engine(); }\n' +
        'export function useIt(make: () => Other): void {\n  const value = make();\n  value.run();\n}\n',
    });
    expect(calls(cg, 'useIt')).toEqual([]);
  });

  it('a local factory still types its result', async () => {
    const cg = await indexed({
      'engine.ts': TWO_CLASSES + 'export function make(): Engine { return new Engine(); }\n',
      'use.ts':
        "import { Other } from './engine';\n" +
        'export function useIt(): void {\n  const make = (): Other => new Other();\n  const value = make();\n  value.run();\n}\n',
    });
    expect(calls(cg, 'useIt')).toEqual(['Other::run@engine.ts', 'useIt::make@use.ts']);
  });

  it('an awaited value is typed in the file that declares its type', async () => {
    const cg = await indexed({
      'factory.ts':
        'export class Engine { run(): void {} }\nexport async function load(): Promise<Engine> { return new Engine(); }\n',
      'caller.ts':
        "import { load } from './factory';\nclass Engine { run(): void {} }\n" +
        'export async function main(): Promise<void> {\n  const value = await load();\n  value.run();\n}\n' +
        'export const keep = Engine;\n',
    });
    expect(calls(cg, 'main')).toEqual(['Engine::run@factory.ts', 'load@factory.ts']);
  });

  it('an imported instance is typed in the file that declares it', async () => {
    const cg = await indexed({
      'store.ts': 'export class Store { notify(): void {} }\nexport const store = new Store();\n',
      'consumer.ts':
        "import { store } from './store';\nclass Store { notify(): void {} }\n" +
        'export function go(): void { store.notify(); }\nexport const keep = Store;\n',
    });
    expect(calls(cg, 'go')).toEqual(['Store::notify@store.ts']);
  });

  it('a regex literal brace does not end the object literal', async () => {
    const cg = await indexed({
      'impl.ts':
        'export function wrong(): number { return 0; }\nexport function right(): number { return 1; }\n' +
        'export const api = { run: wrong, pattern: /\\}/, run: right };\n',
      'consumer.ts': "import { api } from './impl';\nexport function consumerFn(): number { return api.run(); }\n",
    });
    expect(calls(cg, 'consumerFn')).toEqual(['right@impl.ts']);
  });

  it("a nested callback's return does not type the factory", async () => {
    const cg = await indexed({
      'i18n.ts':
        "class I18nClass { t(): string { return ''; } }\nconst i18n: I18nClass = new I18nClass();\n" +
        'function onMount(cb: () => unknown): void { cb(); }\n' +
        'export function useI18n() {\n  onMount(() => {\n    return i18n;\n  });\n}\n',
      'consumer.ts': "import { useI18n } from './i18n';\nexport function go(): void {\n  const x = useI18n();\n  x.t();\n}\n",
    });
    expect(calls(cg, 'go')).toEqual(['useI18n@i18n.ts']);
  });

  it('a framework project keeps the object-literal verdict on a spread', async () => {
    const cg = await indexed({
      'package.json': JSON.stringify({ name: 'fx', dependencies: { react: '^18.2.0' } }),
      'impl.ts':
        'export function wrong(): number { return 0; }\nexport function right(): number { return 1; }\n' +
        'const override = { getState: right };\nexport const api = { getState: wrong, ...override };\n',
      'consumer.ts': "import { api } from './impl';\nexport function consumerFn(): number { return api.getState(); }\n",
    });
    expect(calls(cg, 'consumerFn')).toEqual(['api@impl.ts']);
  });
});

describe('TS/JS extraction details', () => {
  it('same-line namesakes keep their own binding node ids', () => {
    const node = (id: string, startColumn: number) =>
      ({ id, kind: 'function', name: 'y', startLine: 1, startColumn }) as unknown as Node;
    const binding = () => ({ name: 'y', kind: 'decl', line: 1 }) as unknown as Binding;
    const rows = attachBindingNodeIds([binding(), binding()], [node('first', 0), node('second', 17)]);
    expect(rows.map((b) => b.nodeId)).toEqual(['first', 'second']);
  });

  it('a point lookup finds a node that abuts the previous one', () => {
    const src = 'function f(ok: boolean) {\n  if(ok)target();\n}\n';
    const root = parseSourceTreeSync(src, 'typescript')!.rootNode;
    expect(root.descendantForPosition({ row: 1, column: 8 }).text).toBe('target');
    expect(root.descendantForIndex(src.indexOf('target')).text).toBe('target');
    expect(guardsInTree(root, src, 'typescript', 2, 8).map((g) => g.text)).toEqual(['ok']);
  });

  it('window chains are normalized like other member chains', () => {
    const src =
      'export function f(): void {\n  window\n    .app\n    .stop();\n  window.apps[0].go();\n  window.app?.pause();\n}\n';
    const names = extractFromSource('f.ts', src, 'typescript')
      .unresolvedReferences.filter((r) => r.referenceKind === 'calls')
      .map((r) => r.referenceName);
    expect(names).toEqual(['window.app.stop', 'window.app.pause']);
  });
});
