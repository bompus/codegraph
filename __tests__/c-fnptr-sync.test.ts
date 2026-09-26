/**
 * Incremental sync keeps C function-pointer dispatch edges equal to a fresh
 * index. The pass reads every C file's registrations and dispatch sites, so a
 * change in one file can add or remove edges between two others: this pins
 * that a git-scoped sync lands exactly the edges a full index would, after
 * each kind of edit (a table, a header's macro, a new registering file, a
 * removed one, a renamed handler).
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { execFileSync } from 'node:child_process';
import { CodeGraph } from '../src';

let dir: string;
const write = (rel: string, body: string) => {
  fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
  fs.writeFileSync(path.join(dir, rel), body);
};
const git = (...args: string[]) => execFileSync('git', args, { cwd: dir, stdio: 'pipe' });

const edgesOf = (cg: CodeGraph): string[] => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
  return (db.prepare(
    `SELECT s.qualified_name AS src, t.qualified_name AS tgt, e.kind AS kind,
            json_extract(e.metadata, '$.via') AS via, e.line AS line
       FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
      WHERE json_extract(e.metadata, '$.synthesizedBy') = 'fn-pointer-dispatch'
      ORDER BY 1, 2, 3, 4, 5`,
  ).all() as Array<Record<string, unknown>>).map((r) => JSON.stringify(r));
};

async function fresh(): Promise<string[]> {
  const copy = fs.mkdtempSync(path.join(os.tmpdir(), 'cfp-sync-fresh-'));
  try {
    fs.cpSync(dir, copy, { recursive: true, filter: (p) => !p.includes(`${path.sep}.codegraph`) && !p.endsWith(`${path.sep}.git`) && !p.includes(`${path.sep}.git${path.sep}`) });
    const cg = await CodeGraph.init(copy, { silent: true });
    await cg.indexAll();
    const out = edgesOf(cg);
    cg.close();
    return out;
  } finally {
    fs.rmSync(copy, { recursive: true, force: true });
  }
}

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cfp-sync-'));
  write('ops.h', [
    'struct file_ops { int (*read)(int); int (*write)(int); };',
    '#define OP(name) { name##_read, name##_write }',
    '',
  ].join('\n'));
  write('disk.c', [
    '#include "ops.h"',
    'static int disk_read(int n) { return n; }',
    'static int disk_write(int n) { return n + 1; }',
    'struct file_ops disk_ops = OP(disk);',
    '',
  ].join('\n'));
  write('net.c', [
    '#include "ops.h"',
    'static int net_read(int n) { return n * 2; }',
    'static int net_write(int n) { return n * 3; }',
    'static struct file_ops net_ops = { .read = net_read, .write = net_write };',
    '',
  ].join('\n'));
  write('vfs.c', [
    '#include "ops.h"',
    'static int mem_read(int n) { return n - 1; }',
    'void wire(struct file_ops *o) { o->read = mem_read; }',
    'int do_read(struct file_ops *f, int n) { return f->read(n); }',
    'int do_write(struct file_ops *f, int n) { return f->write(n); }',
    '',
  ].join('\n'));
  git('init', '-q');
  git('add', '-A');
  git('-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-qm', 'init');
});

afterEach(() => {
  fs.rmSync(dir, { recursive: true, force: true });
});

describe('C function-pointer edges after an incremental sync', () => {
  it('match a fresh index after each kind of edit', async () => {
    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const initial = edgesOf(cg);
    expect(initial.some((e) => e.includes('"tgt":"net_read"'))).toBe(true);
    expect(initial).toEqual(await fresh());

    const edits: Array<[string, () => void]> = [
      ['a registration table', () => write('net.c', fs.readFileSync(path.join(dir, 'net.c'), 'utf-8').replace('.write = net_write', '.write = net_read'))],
      ['a header macro', () => write('ops.h', fs.readFileSync(path.join(dir, 'ops.h'), 'utf-8').replace('name##_write }', 'name##_read }'))],
      ['a new registering file', () => write('pipe.c', '#include "ops.h"\nstatic int pipe_read(int n) { return n; }\nstatic int pipe_write(int n) { return n; }\nstruct file_ops pipe_ops = OP(pipe);\n')],
      ['a removed file', () => fs.rmSync(path.join(dir, 'disk.c'))],
      ['a renamed handler', () => write('vfs.c', fs.readFileSync(path.join(dir, 'vfs.c'), 'utf-8').replaceAll('mem_read', 'ram_read'))],
    ];
    for (const [what, edit] of edits) {
      edit();
      await cg.sync();
      expect(edgesOf(cg), `after ${what}`).toEqual(await fresh());
    }
    cg.close();
  }, 60_000);
});
