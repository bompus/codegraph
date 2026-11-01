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
import { execFileSync } from 'child_process';
import { ToolHandler } from '../src/mcp/tools';
import { forEachBandBucket, refreshNearDuplicates, shingles, signature, signatureFromSet, similarity } from '../src/graph/near-duplicates';

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
    const b = signature(urlOf('webUrlOf', 'web'), 'webUrlOf')!;
    expect(similarity(a, b)).toBeGreaterThanOrEqual(0.8);
    expect(similarity(a, signature(unrelated)!)).toBeLessThan(0.3);
  });

  it('declines bodies too small to compare', () => {
    expect(signature('function f() { return 1; }')).toBeNull();
  });
});

describe('band buckets', () => {
  const HASHES = 64;
  const BAND_WORDS = 4;
  // FNV-1a's multiplier is odd, so it has an inverse modulo 2^32.
  const P = 0x01000193;
  let P_INV = 1;
  for (let i = 0; i < 5; i++) P_INV = Math.imul(P_INV, 2 - Math.imul(P, P_INV));
  const bandHash = (w: Uint32Array) => {
    let h = 0x811c9dc5;
    for (const x of w) h = Math.imul(h ^ x, P);
    return h >>> 0;
  };
  /** The last word that gives a band of `first` three words the hash `target`. */
  const forceHash = (first: number[], target: number) => {
    let h = 0x811c9dc5;
    for (const x of first) h = Math.imul(h ^ x, P);
    return ((Math.imul(target, P_INV) ^ h) >>> 0);
  };

  it('groups exactly the rows whose band bytes are equal, even when two bands share a hash', () => {
    let seed = 7;
    const rand = () => (seed = (Math.imul(seed, 1103515245) + 12345) >>> 0);
    const count = 400;
    const sigs = Buffer.alloc(count * HASHES * 4);
    for (let i = 0; i < count; i++) for (let k = 0; k < HASHES; k++) sigs.writeUInt32LE(rand(), (i * HASHES + k) * 4);
    const band0 = (i: number) => new Uint32Array(sigs.buffer, sigs.byteOffset + i * HASHES * 4, BAND_WORDS);
    // Rows 10-14 copy row 3's first band; rows 20 and 21 get different band bytes with row 3's hash.
    for (const i of [10, 11, 12, 13, 14]) sigs.copy(sigs, i * HASHES * 4, 3 * HASHES * 4, 3 * HASHES * 4 + 16);
    for (const i of [20, 21]) {
      const first = [rand(), rand(), rand()];
      first.forEach((x, k) => sigs.writeUInt32LE(x, (i * HASHES + k) * 4));
      sigs.writeUInt32LE(forceHash(first, bandHash(band0(3))), (i * HASHES + 3) * 4);
    }
    expect(bandHash(band0(20))).toBe(bandHash(band0(3)));
    expect(Buffer.compare(Buffer.from(band0(20).buffer, band0(20).byteOffset, 16), Buffer.from(band0(3).buffer, band0(3).byteOffset, 16))).not.toBe(0);

    for (const band of [0, 5]) {
      const expected = new Map<string, number[]>();
      for (let i = 0; i < count; i++) {
        const at = i * HASHES * 4 + band * BAND_WORDS * 4;
        const key = sigs.toString('latin1', at, at + BAND_WORDS * 4);
        expected.set(key, [...(expected.get(key) ?? []), i]);
      }
      const order = new Uint32Array(count);
      const hash = new Uint32Array(count);
      const got: number[][] = [];
      forEachBandBucket(sigs, count, band, order, hash, (start, end) => got.push([...order.subarray(start, end)]));
      const norm = (groups: number[][]) => groups.map((g) => g.join(',')).sort();
      expect(norm(got)).toEqual(norm([...expected.values()]));
    }
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
    write('src/web.ts', urlOf('webUrlOf', 'web'));
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
    expect(dupsOf('fpUrlOf')).toEqual(['dsUrlOf', 'webUrlOf']);
    expect(dupsOf('totalPoints')).toEqual([]);
  });

  it('names the copies in explore and codegraph_node', async () => {
    const explore = await tool('codegraph_explore', { query: 'fpUrlOf' });
    expect(explore).toMatch(/- `fpUrlOf` \(src\/fp\.ts:1\) — no callers; near-duplicates \(update together\): .*`dsUrlOf` \(src\/ds\.ts:1, \d+% similar\)/);
    const node = await tool('codegraph_node', { symbol: 'fpUrlOf', includeCode: false });
    expect(node).toMatch(/\*\*Near-duplicates \(update together\) ≈\*\* .*`webUrlOf` \(src\/web\.ts:1/);
  });

  it('re-scores only the pairs a changed body is in, and matches a full recompute', async () => {
    write('src/points2.ts', unrelated.replace('totalPoints', 'totalPoints2'));
    await cg.sync();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const queries = (cg as any).queries;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
    const reads: string[] = [];
    const deps = {
      queries,
      readFile: (p: string) => {
        reads.push(p);
        return fs.readFileSync(path.join(dir, p), 'utf-8');
      },
      isExcluded: (p: string) => p.startsWith('test/') || p.startsWith('vendor/'),
    };
    const snapshot = () => ['fpUrlOf', 'webUrlOf', 'dsUrlOf', 'totalPoints', 'totalPoints2'].map((n) => [n, dupsOf(n)]);
    const before = snapshot();
    expect(dupsOf('totalPoints')).toEqual(['totalPoints2']);

    // dsUrlOf changes: its two pairs are re-scored, the points pair is not read.
    db.prepare("UPDATE nodes SET updated_at = updated_at + 1 WHERE name = 'dsUrlOf'").run();
    const r = await refreshNearDuplicates(deps);
    expect(r.signed).toBe(1);
    expect(r.rescored).toBe(2);
    expect(reads.some((p) => p.includes('points'))).toBe(false);
    expect(snapshot()).toEqual(before);

    // Every body re-signed: a full recompute stores the same pairs.
    db.prepare("UPDATE nodes SET updated_at = updated_at + 1 WHERE kind = 'function'").run();
    await refreshNearDuplicates(deps);
    expect(snapshot()).toEqual(before);

    // The off-thread signer path stores the same signatures and pairs as the
    // in-process loop (the pool worker computes shingles/signatureFromSet).
    db.prepare("UPDATE nodes SET updated_at = updated_at + 1 WHERE kind = 'function'").run();
    const sign = async (bodies: Array<{ id: string; name: string; filePath: string; startLine: number; endLine: number; updatedAt: number }>) =>
      bodies.map((b) => {
        const set = shingles(deps.readFile(b.filePath)!.split(/\r?\n/).slice(b.startLine - 1, b.endLine).join('\n'), b.name);
        const sig = set ? signatureFromSet(set) : null;
        return {
          id: b.id,
          sig: sig ? new Uint8Array(sig.buffer, sig.byteOffset, sig.byteLength).slice() : new Uint8Array(0),
          set: set ? Uint32Array.from(set) : new Uint32Array(0),
        };
      });
    const viaSigner = await refreshNearDuplicates({ ...deps, sign });
    expect(viaSigner.signed).toBeGreaterThan(0);
    expect(snapshot()).toEqual(before);

    // A worker/pool rejection falls back to the in-process loop; the rows and
    // pairs must be the same, not stale or empty.
    db.prepare("UPDATE nodes SET updated_at = updated_at + 1 WHERE kind = 'function'").run();
    const viaFallback = await refreshNearDuplicates({ ...deps, sign: async () => { throw new Error('pool down'); } });
    expect(viaFallback.signed).toBeGreaterThan(0);
    expect(snapshot()).toEqual(before);
  });

  it('a git-scoped sync that edits and removes files stores the same pairs as a fresh index', async () => {
    const git = (...args: string[]) => execFileSync('git', args, { cwd: dir, stdio: 'pipe' });
    const pairsOf = (graph: CodeGraph) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const db = (graph as any).db.db as import('node:sqlite').DatabaseSync;
      return db.prepare(
        `SELECT a.name AS a, b.name AS b, round(d.score, 6) AS score FROM near_duplicates d
           JOIN nodes a ON a.id = d.node_id JOIN nodes b ON b.id = d.other_id ORDER BY 1, 2`,
      ).all();
    };
    write('src/points2.ts', unrelated.replace('totalPoints', 'totalPoints2'));
    git('init', '-q');
    git('add', '-A');
    git('-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-qm', 'init');
    cg.close();
    fs.rmSync(path.join(dir, '.codegraph'), { recursive: true, force: true });
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();

    write('src/ds.ts', unrelated.replace('totalPoints', 'dsTotals'));
    fs.rmSync(path.join(dir, 'src/points2.ts'));
    await cg.sync();
    const synced = pairsOf(cg);
    expect(synced.length).toBeGreaterThan(0);

    cg.close();
    fs.rmSync(path.join(dir, '.codegraph'), { recursive: true, force: true });
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
    expect(synced).toEqual(pairsOf(cg));
  });

  it('an index created before the pair-score table gets it on open and keeps refreshing', async () => {
    const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
    db.exec("DROP TABLE near_dup_scores; DELETE FROM schema_versions WHERE version >= 14; INSERT OR IGNORE INTO schema_versions VALUES (13, 0, 'v13');");
    cg.close();
    cg = await CodeGraph.open(dir);
    write('src/ds.ts', unrelated.replace('totalPoints', 'dsTotals'));
    await cg.sync();
    expect(dupsOf('fpUrlOf')).toEqual(['webUrlOf']);
    expect(dupsOf('totalPoints')).toEqual(['dsTotals']);
  });

  it('follows edits and deletions on sync', async () => {
    write('src/ds.ts', unrelated.replace('totalPoints', 'dsTotals'));
    fs.rmSync(path.join(dir, 'src/web.ts'));
    await cg.sync();
    expect(dupsOf('fpUrlOf')).toEqual([]);
    expect(dupsOf('totalPoints')).toEqual(['dsTotals']);
  });
});
