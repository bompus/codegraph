import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { extractFromSource } from '../src/extraction';
const createTempDir = () => fs.mkdtempSync(path.join(os.tmpdir(), 'cg-awaited-'));
const cleanupTempDir = (dir: string) => fs.rmSync(dir, { recursive: true, force: true });
describe('awaited generic calls', () => {
  // tree-sitter-typescript parses `await f<T>(x)` as a call whose callee is
  // `await f`, so the ref was named `await f` and matched nothing (#2586).
  it.each([['user.ts'], ['user.tsx']])('an awaited call with type arguments names its callee: %s (#2586)', (file) => {
    const code = [
      'export async function awaited() { return await fetchApi<User>("/me"); }',
      'export async function assigned() { const u = await fetchApi<User>("/me"); return u; }',
      'export async function viaNamespace() { return await api.fetchApi<User>("/me"); }',
      'export async function viaThis() { return await this.load<User>("/me"); }',
      '',
    ].join('\n');
    const result = extractFromSource(file, code);
    const calls = result.unresolvedReferences.filter((r) => r.referenceKind === 'calls');
    const byFn = (name: string) => {
      const fn = result.nodes.find((n) => n.name === name)!;
      return calls.filter((c) => c.fromNodeId === fn.id).map((c) => c.referenceName);
    };
    expect(byFn('awaited')).toEqual(['fetchApi']);
    expect(byFn('assigned')).toEqual(['fetchApi']);
    expect(byFn('viaNamespace')).toEqual(['api.fetchApi']);
    expect(byFn('viaThis')).toEqual(['load']);
    expect(calls.some((c) => c.referenceName.startsWith('await'))).toBe(false);
    // The reference sits where the callee starts, as for `await f(x)` — not on
    // the `await`, which the call node starts at.
    const lines = code.split('\n');
    const columnOf = (name: string) => {
      const fn = result.nodes.find((n) => n.name === name)!;
      return calls.find((c) => c.fromNodeId === fn.id)!.column;
    };
    expect(columnOf('awaited')).toBe(lines[0]!.indexOf('fetchApi'));
    expect(columnOf('viaNamespace')).toBe(lines[2]!.indexOf('api.fetchApi'));
  });

  it('an awaited generic call to a name a package exports links to nothing in the project (#2586)', async () => {
    const dir = createTempDir();
    try {
      fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ name: 'app', dependencies: { 'idb-keyval': '6.2.1' } }));
      fs.mkdirSync(path.join(dir, 'src'));
      // excalidraw's `await get<SavedChats>(key, store)` from idb-keyval, beside
      // a project class whose static `get` the name alone would match.
      fs.writeFileSync(path.join(dir, 'src', 'storage.ts'),
        'import { get } from "idb-keyval";\nexport async function load() { return await get<string>("k"); }\n');
      fs.writeFileSync(path.join(dir, 'src', 'cache.ts'), 'export class Cache {\n  static get = (s: string) => s;\n}\n');
      const cg = await CodeGraph.init(dir, { index: true });
      try {
        const load = cg.getNodesByKind('function').find((n) => n.name === 'load')!;
        expect(cg.getOutgoingEdges(load.id).filter((e) => e.kind === 'calls')).toEqual([]);
      } finally {
        cg.close();
      }
    } finally {
      cleanupTempDir(dir);
    }
  }, 60_000);

  it.each([['user.ts'], ['user.tsx'], ['user.ets']])('an awaited generic call peels every await and skips comments: %s (#2586)', (file) => {
    const code = [
      'export async function chainedParen() { return (await fetchApi<User>("/me")).json(); }',
      'export async function chainedThen() { return await api.get<User>("/me").then(r => r); }',
      'export async function commented() { return await /* typed */ fetchApi<User>("/me"); }',
      'export async function lineCommented() { return await // typed\n fetchApi<User>("/me"); }',
      'export async function doubled() { return await await fetchApi<User>("/me"); }',
      '',
    ].join('\n');
    const result = extractFromSource(file, code);
    const calls = result.unresolvedReferences.filter((r) => r.referenceKind === 'calls');
    const byFn = (name: string) => {
      const fn = result.nodes.find((n) => n.name === name)!;
      return calls.filter((c) => c.fromNodeId === fn.id).map((c) => c.referenceName).sort();
    };
    expect(byFn('commented')).toEqual(['fetchApi']);
    expect(byFn('lineCommented')).toEqual(['fetchApi']);
    expect(byFn('doubled')).toEqual(['fetchApi']);
    expect(calls.some((c) => /^await|^\/[*/]/.test(c.referenceName))).toBe(false);
    // ArkTS keeps its own chain encoding; TS/TSX name the inner callee.
    if (file !== 'user.ets') {
      expect(byFn('chainedParen')).toEqual(['fetchApi', 'fetchApi().json']);
      expect(byFn('chainedThen')).toEqual(['api.get', 'api.get().then']);
    }
  });
});
