/**
 * A synthesis pass may skip a file on a later run when an earlier run dropped
 * it for reasons that depend only on its bytes (src/resolution/synth-skips.ts).
 * These pin that a sync using those skips lands exactly the synthesized edges
 * a fresh index would, after each kind of edit that could make a stale skip
 * wrong: a skipped file gaining a registry, reverting to its skipped bytes,
 * being deleted, and a build-version change.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let dir: string;
const write = (rel: string, body: string) => {
  fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
  fs.writeFileSync(path.join(dir, rel), body);
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const dbOf = (cg: CodeGraph) => (cg as any).db.db as import('node:sqlite').DatabaseSync;

const synthesized = (cg: CodeGraph): string[] =>
  (dbOf(cg).prepare(
    `SELECT s.qualified_name AS src, t.qualified_name AS tgt, e.kind AS kind, e.line AS line, e.metadata AS meta
       FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
      WHERE e.provenance = 'heuristic'
      ORDER BY 1, 2, 3, 4, 5`,
  ).all() as Array<Record<string, unknown>>).map((r) => JSON.stringify(r));

// The skips that apply now: a row whose hash no longer matches its file stays in the table but never applies.
const skipped = (cg: CodeGraph, pass: string): string[] =>
  (dbOf(cg).prepare(
    `SELECT s.path AS path FROM synth_skips s JOIN files f ON f.path = s.path AND f.content_hash = s.content_hash
      WHERE s.pass = ? ORDER BY s.path`,
  ).all(pass) as Array<{ path: string }>).map((r) => r.path);
const storedRows = (cg: CodeGraph, file: string): number =>
  (dbOf(cg).prepare('SELECT COUNT(*) AS n FROM synth_skips WHERE path = ?').get(file) as { n: number }).n;

async function fresh(): Promise<string[]> {
  const copy = fs.mkdtempSync(path.join(os.tmpdir(), 'synth-skips-fresh-'));
  try {
    fs.cpSync(dir, copy, { recursive: true, filter: (p) => !p.includes(`${path.sep}.codegraph`) });
    const cg = await CodeGraph.init(copy, { silent: true });
    await cg.indexAll();
    const out = synthesized(cg);
    cg.close();
    return out;
  } finally {
    fs.rmSync(copy, { recursive: true, force: true });
  }
}

const registry = (a: string, b: string, run: string): string =>
  [
    `function ${a}() { return 1; }`,
    `function ${b}() { return 2; }`,
    `const handlers = { first: ${a}, second: ${b} };`,
    `export function ${run}(name: string) {`,
    '  return handlers[name]();',
    '}',
    '',
  ].join('\n');
const REGISTRY = registry('saveItem', 'loadItem', 'runIt');
const LATER = registry('storeItem', 'fetchItem', 'goIt');
const PLAIN = 'export function helper(x: number) {\n  return x + 1;\n}\n';

describe('content-only synthesis skips', () => {
  let cg: CodeGraph;

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'synth-skips-'));
    write('src/registry.ts', REGISTRY);
    write('src/plain.ts', PLAIN);
    write('src/other.ts', 'export const values = [1, 2, 3];\nexport const first = values[0];\n');
    cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
  });

  afterEach(() => {
    try { cg.close(); } catch { /* already closed */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('records files the registry pass dropped for their content alone', () => {
    const rows = skipped(cg, 'registryEdges');
    expect(rows).toContain('src/plain.ts');
    expect(rows).toContain('src/other.ts');
    expect(rows).not.toContain('src/registry.ts');
    expect(synthesized(cg).some((e) => e.includes('object-registry'))).toBe(true);
  });

  it('scans a skipped file again once it gains a registry, and matches a fresh index', async () => {
    write('src/plain.ts', LATER);
    await cg.sync();
    expect(synthesized(cg).filter((e) => e.includes('plain.ts') && e.includes('object-registry')).length).toBe(2);
    expect(synthesized(cg)).toEqual(await fresh());
  });

  it('matches a fresh index after reverting to the skipped bytes and after a delete', async () => {
    write('src/plain.ts', LATER);
    await cg.sync();
    write('src/plain.ts', PLAIN);
    await cg.sync();
    expect(synthesized(cg)).toEqual(await fresh());
    fs.rmSync(path.join(dir, 'src/other.ts'));
    await cg.sync();
    expect(storedRows(cg, 'src/other.ts')).toBe(0);
    expect(synthesized(cg)).toEqual(await fresh());
  });

  it('ignores skips written by a different build', async () => {
    const db = dbOf(cg);
    db.prepare("UPDATE project_metadata SET value = 'older-build' WHERE key = 'synth_skips_version'").run();
    // A wrong skip for the registry file: a different build must not apply it.
    db.prepare("INSERT OR REPLACE INTO synth_skips (pass, path, content_hash) SELECT 'registryEdges', path, content_hash FROM files WHERE path = 'src/registry.ts'").run();
    write('src/other.ts', 'export const values = [1, 2];\n');
    await cg.sync();
    expect(synthesized(cg)).toEqual(await fresh());
    expect(skipped(cg, 'registryEdges')).not.toContain('src/registry.ts');
  });
});

describe('content-only framework detection skips', () => {
  let cg: CodeGraph;
  const routes = (g: CodeGraph): string[] =>
    (dbOf(g).prepare(
      `SELECT n.id AS id, t.qualified_name AS target FROM nodes n
         LEFT JOIN edges e ON e.source = n.id LEFT JOIN nodes t ON t.id = e.target
        WHERE n.kind = 'route' ORDER BY 1, 2`,
    ).all() as Array<Record<string, unknown>>).map((r) => JSON.stringify(r));
  const freshRoutes = async (): Promise<string[]> => {
    const copy = fs.mkdtempSync(path.join(os.tmpdir(), 'detect-skips-fresh-'));
    try {
      fs.cpSync(dir, copy, { recursive: true, filter: (p) => !p.includes(`${path.sep}.codegraph`) });
      const g = await CodeGraph.init(copy, { silent: true });
      await g.indexAll();
      const out = routes(g);
      g.close();
      return out;
    } finally {
      fs.rmSync(copy, { recursive: true, force: true });
    }
  };

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'detect-skips-'));
    write('package.json', '{ "name": "app" }\n');
    write('src/app.ts', PLAIN);
    write('src/users.ts', 'export function listUsers() { return []; }\n');
    cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
  });

  afterEach(() => {
    try { cg.close(); } catch { /* already closed */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('detects a framework a skipped file gains on sync, and matches a fresh index', async () => {
    expect(skipped(cg, 'detect:http-routing')).toContain('src/app.ts');
    expect(routes(cg)).toEqual([]);
    write('src/app.ts', [
      "import { Hono } from 'hono';",
      "import { listUsers } from './users';",
      'const app = new Hono();',
      "app.get('/users', listUsers);",
      'export default app;',
      '',
    ].join('\n'));
    await cg.sync();
    expect(routes(cg).some((r) => r.includes('/users'))).toBe(true);
    expect(routes(cg)).toEqual(await freshRoutes());
    expect(skipped(cg, 'detect:http-routing')).not.toContain('src/app.ts');
  });
});
