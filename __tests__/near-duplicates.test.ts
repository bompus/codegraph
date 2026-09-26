/**
 * Near-duplicate function bodies: copies of a function are found at index
 * time, kept current by sync, and named wherever an agent reads that function
 * (explore's blast radius, codegraph_node's trail).
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';
import { signature, similarity } from '../src/graph/near-duplicates';

const urlOf = (name: string, label: string) => [
  `export function ${name}(input: string | URL): string {`,
  `  if (typeof input === "string") {`,
  `    console.debug("${label}", input.length);`,
  `    return input;`,
  `  }`,
  `  if (input instanceof URL) {`,
  `    return input.href;`,
  `  }`,
  `  return String(input);`,
  `}`,
  ``,
].join('\n');

const unrelated = [
  'export function totalPoints(rows: Array<{ pts: number; bonus?: number }>): number {',
  '  let sum = 0;',
  '  for (const row of rows) {',
  '    sum += row.pts * 2 + (row.bonus ?? 0);',
  '    if (sum > 1000) break;',
  '  }',
  '  return Math.round(sum / rows.length);',
  '}',
  '',
].join('\n');

describe('signature', () => {
  it('matches copies that differ only in names and literals, not unrelated code', () => {
    const a = signature(urlOf('fpUrlOf', 'fp'), 'fpUrlOf')!;
    const b = signature(urlOf('ttdUrlOf', 'ttd'), 'ttdUrlOf')!;
    expect(similarity(a, b)).toBeGreaterThanOrEqual(0.8);
    expect(similarity(a, signature(unrelated)!)).toBeLessThan(0.3);
  });

  it('declines bodies too small to compare', () => {
    expect(signature('function f() { return 1; }')).toBeNull();
  });
});

describe('near-duplicates in the index', () => {
  let dir: string;
  let cg: CodeGraph;
  const write = (rel: string, body: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), body);
  };
  const tool = async (name: string, args: Record<string, unknown>) => {
    const result = await new ToolHandler(cg).execute(name, args);
    return result.content?.[0]?.type === 'text' ? result.content[0].text : '';
  };
  const dupsOf = (name: string) => {
    const node = cg.getNodesByName(name).find((n) => n.kind === 'function')!;
    return cg.getNearDuplicates(node.id).map((d) => d.node.name).sort();
  };

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-near-dups-'));
    write('src/fp.ts', urlOf('fpUrlOf', 'fp'));
    write('src/ttd.ts', urlOf('ttdUrlOf', 'ttd'));
    write('src/ds.ts', urlOf('dsUrlOf', 'ds'));
    write('src/points.ts', unrelated);
    // Copies in tests and vendored code are expected and stay out.
    write('test/helpers.ts', urlOf('testUrlOf', 'test'));
    write('vendor/lib/url.ts', urlOf('vendoredUrlOf', 'vendor'));
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
  });

  afterEach(() => {
    try { cg.close(); } catch { /* ignore */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('pairs the copies and leaves out tests, vendored code and unrelated functions', () => {
    expect(dupsOf('fpUrlOf')).toEqual(['dsUrlOf', 'ttdUrlOf']);
    expect(dupsOf('totalPoints')).toEqual([]);
  });

  it('names the copies in explore and codegraph_node', async () => {
    const explore = await tool('codegraph_explore', { query: 'fpUrlOf' });
    expect(explore).toMatch(/- `fpUrlOf` \(src\/fp\.ts:1\) — no callers; near-duplicates \(update together\): .*`dsUrlOf` \(src\/ds\.ts:1, \d+% similar\)/);
    const node = await tool('codegraph_node', { symbol: 'fpUrlOf', includeCode: false });
    expect(node).toMatch(/\*\*Near-duplicates \(update together\) ≈\*\* .*`ttdUrlOf` \(src\/ttd\.ts:1/);
  });

  it('follows edits and deletions on sync', async () => {
    write('src/ds.ts', unrelated.replace('totalPoints', 'dsTotals'));
    fs.rmSync(path.join(dir, 'src/ttd.ts'));
    await cg.sync();
    expect(dupsOf('fpUrlOf')).toEqual([]);
    expect(dupsOf('totalPoints')).toEqual(['dsTotals']);
  });
});
