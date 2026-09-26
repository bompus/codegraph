/**
 * A git worktree starts from a sibling worktree's index and syncs the files
 * that differ. The result must be the graph a from-scratch index of the same
 * files builds: committed edits, added and deleted files, and an uncommitted
 * edit all land. A sibling whose index this engine did not build is not used.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { findSeedSource } from '../src/sync/worktree-seed';
import { DatabaseConnection, getDatabasePath } from '../src/db';

const BIN = path.resolve(__dirname, '../dist/bin/codegraph.js');

function init(root: string, ...flags: string[]): string {
  return execFileSync(process.execPath, [BIN, 'init', '--yes', ...flags, root], {
    encoding: 'utf8',
    env: { ...process.env, CODEGRAPH_NO_DAEMON: '1', CODEGRAPH_TELEMETRY: '0', NO_COLOR: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

let tempDir: string;
const open: CodeGraph[] = [];

function git(cwd: string, ...args: string[]): string {
  return execFileSync('git', ['-c', 'user.email=t@example.com', '-c', 'user.name=t', ...args], {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

function write(root: string, files: Record<string, string>): void {
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
}

/** A main checkout indexed at its first commit, plus a linked worktree on a branch. */
async function repoWithWorktree(): Promise<{ main: string; wt: string }> {
  tempDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-seed-')));
  const main = path.join(tempDir, 'main');
  fs.mkdirSync(main);
  git(main, 'init', '-q', '-b', 'main');
  write(main, {
    'src/math.ts': 'export function add(a: number, b: number) { return a + b; }\n',
    'src/app.ts': "import { add } from './math';\nexport function run() { return add(1, 2); }\n",
    'src/old.ts': "import { add } from './math';\nexport function legacy() { return add(0, 0); }\n",
  });
  git(main, 'add', '.');
  git(main, 'commit', '-qm', 'base');
  const cg = await CodeGraph.init(main, { index: true });
  cg.close();

  const wt = path.join(tempDir, 'wt');
  git(main, 'worktree', 'add', '-q', '-b', 'feature', wt);
  write(wt, {
    'src/math.ts': 'export function add(a: number, b: number) { return a + b; }\nexport function mul(a: number, b: number) { return a * b; }\n',
    'src/extra.ts': "import { mul } from './math';\nexport function area() { return mul(2, 3); }\n",
  });
  fs.rmSync(path.join(wt, 'src/old.ts'));
  git(wt, 'add', '-A');
  git(wt, 'commit', '-qm', 'feature');
  // Uncommitted: run() now also calls mul().
  write(wt, { 'src/app.ts': "import { add, mul } from './math';\nexport function run() { return add(1, mul(2, 2)); }\n" });
  return { main, wt };
}

/** Nodes and edges as comparable strings. */
function graphOf(cg: CodeGraph): { nodes: string[]; edges: string[] } {
  const nodes = cg.getFiles().flatMap((f) => cg.getNodesInFile(f.path));
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const label = (id: string) => {
    const n = byId.get(id);
    return n ? `${n.filePath}:${n.kind}:${n.name}` : id;
  };
  const edges = nodes.flatMap((n) =>
    cg.getOutgoingEdges(n.id).map((e) => `${label(e.source)} -${e.kind}-> ${label(e.target)}`)
  );
  return {
    nodes: nodes.map((n) => `${n.filePath}:${n.kind}:${n.name}:${n.startLine}`).sort(),
    edges: [...new Set(edges)].sort(),
  };
}

afterEach(() => {
  for (const cg of open.splice(0)) cg.close();
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe('seeding a worktree index from a sibling', () => {
  it('builds the same graph as a from-scratch index of the worktree', async () => {
    const { main, wt } = await repoWithWorktree();

    const seeded = await CodeGraph.initFromSibling(wt);
    expect(seeded).not.toBeNull();
    open.push(seeded!.codegraph);
    expect(seeded!.source.root).toBe(main);
    expect(seeded!.source.changedFiles).toBe(3);

    // The same files, indexed from nothing and outside git.
    const scratch = path.join(tempDir, 'scratch');
    fs.cpSync(path.join(wt, 'src'), path.join(scratch, 'src'), { recursive: true });
    const fresh = await CodeGraph.init(scratch, { index: true });
    open.push(fresh);

    const got = graphOf(seeded!.codegraph);
    expect(got).toEqual(graphOf(fresh));
    expect(got.edges).toContain('src/app.ts:function:run -calls-> src/math.ts:function:mul');
    expect(got.nodes.some((n) => n.startsWith('src/old.ts:'))).toBe(false);
  });

  it('does not seed from an index another extraction version built', async () => {
    const { main, wt } = await repoWithWorktree();
    const conn = DatabaseConnection.open(getDatabasePath(main));
    conn.getDb().prepare("UPDATE project_metadata SET value = '1' WHERE key = 'indexed_with_extraction_version'").run();
    conn.close();

    expect(findSeedSource(wt)).toBeNull();
    expect(await CodeGraph.initFromSibling(wt)).toBeNull();
    expect(fs.existsSync(getDatabasePath(wt))).toBe(false);
  });

  it('codegraph init seeds a worktree, and --no-seed indexes it from scratch', async () => {
    const { main, wt } = await repoWithWorktree();
    expect(init(wt)).toContain(`from the index in ${main}`);
    fs.rmSync(path.join(wt, '.codegraph'), { recursive: true, force: true });
    const scratch = init(wt, '--no-seed');
    expect(scratch).not.toContain('from the index in');
    expect(fs.existsSync(getDatabasePath(wt))).toBe(true);
  });

  it('has nothing to seed from outside a git worktree', async () => {
    tempDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-seed-')));
    write(tempDir, { 'a.ts': 'export const a = 1;\n' });
    expect(await CodeGraph.initFromSibling(tempDir)).toBeNull();
  });
});
