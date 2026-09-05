import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import CodeGraph from '../src';
import { ToolHandler } from '../src/mcp/tools';

/**
 * A file spawned by path, run as a worker, or imported by a string has no edge
 * to the file that names it. Explore treats a file-name literal in a named-tier
 * file as that edge: the referenced file renders with source next to the named
 * one. An ambiguous basename resolves to nothing, and at most three files join.
 */
describe('path-literal references — explore renders the spawned file', () => {
  let dir: string;
  let cg: CodeGraph;
  let handler: ToolHandler;

  const sourced = (text: string) =>
    [...text.matchAll(/^\*\*`(.+?)`\*\* —/gm)].map((m) => m[1]);

  beforeAll(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-pathrefs-'));
    fs.mkdirSync(path.join(dir, 'lib'), { recursive: true });
    fs.mkdirSync(path.join(dir, 'a', 'dup'), { recursive: true });
    fs.mkdirSync(path.join(dir, 'b', 'dup'), { recursive: true });
    fs.writeFileSync(
      path.join(dir, 'lib', 'boot-room.ts'),
      `import { spawn } from 'node:child_process';
const ARMED_WATCHER_SCRIPT = 'armed-watcher.ts';
const AMBIGUOUS = 'helper.ts';
export function bootRoom(argv: string[]) {
  if (argv.includes('--start')) spawn('bun', [ARMED_WATCHER_SCRIPT, AMBIGUOUS]);
  return 0;
}
`,
    );
    fs.writeFileSync(
      path.join(dir, 'lib', 'armed-watcher.ts'),
      `const POLL_TARGET = 'json/list';
function pollOnce() { return fetch(POLL_TARGET); }
function armLoop() { setInterval(pollOnce, 250); }
armLoop();
`,
    );
    fs.writeFileSync(path.join(dir, 'a', 'dup', 'helper.ts'), `export function helperA() { return 1; }\n`);
    fs.writeFileSync(path.join(dir, 'b', 'dup', 'helper.ts'), `export function helperB() { return 2; }\n`);
    // Holders that together name more files than the cap, two per symbol (a
    // symbol naming three or more is a path list, not a hop, and is skipped).
    const many = ['worker1.ts', 'worker2.ts', 'worker3.ts', 'worker4.ts', 'worker5.ts', 'worker6.ts'];
    for (const w of many) fs.writeFileSync(path.join(dir, 'lib', w), `export function ${w.slice(0, 7)}() { return 0; }\n`);
    fs.writeFileSync(
      path.join(dir, 'lib', 'fanOut.ts'),
      `export function fanOut() { return spawnA().concat(spawnB(), spawnC()); }
function spawnA() { return ['worker1.ts', 'worker2.ts']; }
function spawnB() { return ['worker3.ts', 'worker4.ts']; }
function spawnC() { return ['worker5.ts', 'worker6.ts']; }
`,
    );
    fs.writeFileSync(
      path.join(dir, 'lib', 'audit-table.ts'),
      `export function pathList() { return ['worker1.ts', 'worker2.ts', 'worker3.ts', 'worker4.ts']; }\n`,
    );
    cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    handler = new ToolHandler(cg);
  });

  afterAll(() => {
    cg.destroy();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('the spawned script renders with source beside the file that names it', async () => {
    const res = await handler.execute('codegraph_explore', { query: 'bootRoom' });
    const text = res.content[0].text as string;
    const files = sourced(text);
    expect(files).toContain('lib/boot-room.ts');
    expect(files).toContain('lib/armed-watcher.ts');
    expect(text).toContain('json/list');
  });

  it('an ambiguous basename is not a reference', async () => {
    const res = await handler.execute('codegraph_explore', { query: 'bootRoom' });
    const files = sourced(res.content[0].text as string);
    expect(files.filter((f) => f.endsWith('helper.ts'))).toEqual([]);
  });

  it('at most three referenced files join', async () => {
    const res = await handler.execute('codegraph_explore', { query: 'fanOut' });
    const files = sourced(res.content[0].text as string);
    expect(files.filter((f) => /^lib\/worker\d\.ts$/.test(f)).length).toBe(3);
  });

  it('a symbol naming a list of paths is data, not a hop', async () => {
    const res = await handler.execute('codegraph_explore', { query: 'pathList' });
    const files = sourced(res.content[0].text as string);
    expect(files.filter((f) => /^lib\/worker\d\.ts$/.test(f))).toEqual([]);
  });

  it('CODEGRAPH_LITERAL_SEEDS=0 turns the pass off', async () => {
    process.env.CODEGRAPH_LITERAL_SEEDS = '0';
    try {
      const res = await handler.execute('codegraph_explore', { query: 'bootRoom' });
      expect(sourced(res.content[0].text as string)).not.toContain('lib/armed-watcher.ts');
    } finally {
      delete process.env.CODEGRAPH_LITERAL_SEEDS;
    }
  });
});
