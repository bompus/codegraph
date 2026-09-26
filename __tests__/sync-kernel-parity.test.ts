/**
 * Incremental sync resolves through the kernel first, as a full index does,
 * and lands the same graph as the TypeScript path (CODEGRAPH_KERNEL_RESOLVE=0).
 *
 * The git fast path resolves the changed files' refs, then retries failed refs
 * the changed files may now satisfy (#1240). Both used to run the TypeScript
 * pipeline only.
 */
import { describe, it, expect, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import CodeGraph from '../src/index';
import { getKernel } from '../src/extraction/kernel/loader';

const kernelBuilt = !!getKernel()?.KernelResolver;

const BEFORE: Record<string, string> = {
  'src/base.ts': 'export class Base {\n  greet() { return 1; }\n}\n',
  'src/svc.ts': [
    "import { Base } from './base';",
    'export class Svc extends Base {',
    '  run() { return 2; }',
    '}',
  ].join('\n'),
  'src/util.ts': 'export function helper() { return 3; }\n',
  'src/main.ts': [
    "import { Svc } from './svc';",
    "import { helper } from './util';",
    'export function main() {',
    '  const s = new Svc();',
    '  s.run();',
    '  helper();',
    '  later();',
    '}',
  ].join('\n'),
};

// The edit: main.ts gains an inherited call and an import of `later`, which
// util.ts now exports — the old `later()` ref failed and is retried.
const AFTER: Record<string, string> = {
  'src/util.ts': 'export function helper() { return 3; }\nexport function later() { return 4; }\n',
  'src/main.ts': [
    "import { Svc } from './svc';",
    "import { helper, later } from './util';",
    'export function main() {',
    '  const s = new Svc();',
    '  s.run();',
    '  s.greet();',
    '  helper();',
    '  later();',
    '}',
  ].join('\n'),
};

let tempDir: string | null = null;
let cg: CodeGraph | null = null;
const envKernel = process.env.CODEGRAPH_KERNEL_RESOLVE;

afterEach(() => {
  vi.restoreAllMocks();
  if (envKernel === undefined) delete process.env.CODEGRAPH_KERNEL_RESOLVE;
  else process.env.CODEGRAPH_KERNEL_RESOLVE = envKernel;
  cg?.close();
  cg = null;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  tempDir = null;
});

function write(files: Record<string, string>): void {
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir!, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir!, rel), content);
  }
}

async function syncedGraph(kernelResolve: boolean): Promise<{ edges: string[]; syncChunks: number }> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-sync-kernel-'));
  const git = (...args: string[]) => execFileSync('git', args, { cwd: tempDir!, stdio: 'pipe' });
  write(BEFORE);
  git('init');
  git('config', 'user.email', 'test@test.com');
  git('config', 'user.name', 'Test');
  git('add', '-A');
  git('commit', '-m', 'initial');
  if (kernelResolve) delete process.env.CODEGRAPH_KERNEL_RESOLVE;
  else process.env.CODEGRAPH_KERNEL_RESOLVE = '0';
  cg = await CodeGraph.init(tempDir, { index: true });
  write(AFTER);
  // Counted from here: only the sync's own native chunks.
  const proto = getKernel()!.KernelResolver!.prototype as { resolveChunk: (...a: unknown[]) => unknown };
  const chunk = vi.spyOn(proto, 'resolveChunk');
  await cg.sync();
  const syncChunks = chunk.mock.calls.length;
  chunk.mockRestore();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
  const rows = db.prepare(
    `SELECT s.qualified_name AS src, t.qualified_name AS tgt, e.kind AS kind, e.metadata AS metadata
       FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
      ORDER BY src, tgt, kind, metadata`,
  ).all() as unknown as { src: string; tgt: string; kind: string; metadata: string | null }[];
  return { edges: rows.map((r) => `${r.kind} ${r.src} -> ${r.tgt} ${r.metadata ?? ''}`), syncChunks };
}

describe.skipIf(!kernelBuilt)('incremental sync through the kernel', () => {
  it('resolves the changed refs natively and matches the TypeScript path', async () => {
    const viaKernel = await syncedGraph(true);
    expect(viaKernel.syncChunks).toBeGreaterThan(0);
    cg?.close();
    cg = null;
    fs.rmSync(tempDir!, { recursive: true, force: true });
    tempDir = null;

    const viaTs = await syncedGraph(false);
    expect(viaTs.syncChunks).toBe(0);
    expect(viaKernel.edges).toEqual(viaTs.edges);
    // The edit's new edges exist: the inherited call and the retried import.
    expect(viaKernel.edges.some((e) => e.startsWith('calls main -> Base::greet'))).toBe(true);
    expect(viaKernel.edges.some((e) => e.startsWith('calls main -> later'))).toBe(true);
  });
});
