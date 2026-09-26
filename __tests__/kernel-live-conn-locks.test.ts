/**
 * Closing a kernel connection on the live db must not drop this process's
 * SQLite locks. POSIX `fcntl` locks belong to the process, and closing any
 * descriptor on a file releases all of them — node:sqlite's included, which
 * the kernel's separate SQLite build cannot see. Once they were gone, another
 * process took itself for the only connection: on close it checkpointed and
 * deleted the live WAL while this process kept writing into the unlinked
 * file, and a worktree index was later reported malformed. The kernel now
 * parks its live-db connection instead of closing it.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import CodeGraph from '../src/index';
import { getKernel } from '../src/extraction/kernel/loader';

const kernel = getKernel();
// Windows locks belong to the handle; Bun has no node:sqlite for the child.
const runs = !!kernel?.KernelResolver && process.platform !== 'win32' && !process.versions.bun;

let tempDir: string | null = null;
let cg: CodeGraph | null = null;

afterEach(() => {
  cg?.close();
  cg = null;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  tempDir = null;
});

describe.skipIf(!runs)('kernel connection on the live db', () => {
  it('keeps the WAL alive for another process after the kernel closes', async () => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-live-conn-'));
    fs.mkdirSync(path.join(tempDir, 'src'));
    fs.writeFileSync(path.join(tempDir, 'src', 'a.ts'), 'export function a() { return b(); }\nexport function b() { return 1; }\n');
    cg = await CodeGraph.init(tempDir, { index: true });
    const dbPath = path.join(tempDir, '.codegraph', 'codegraph.db');
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
    db.exec('CREATE TABLE IF NOT EXISTS live_conn_probe(x TEXT)');
    db.exec("INSERT INTO live_conn_probe VALUES ('a')");
    expect(fs.existsSync(`${dbPath}-wal`)).toBe(true);

    const resolver = new kernel!.KernelResolver!({
      dbPath,
      projectRoot: tempDir,
      cppIncludeDirs: [],
      nodeBuiltinSpecifiers: [],
      frameworksActive: false,
      queryLookups: true,
    });
    resolver.readPendingBatch(0, 10, false);
    resolver.close();

    // Another process opens, writes and closes. If this process's locks were
    // dropped, it believes it is the last connection and deletes the WAL.
    execFileSync(process.execPath, ['-e', `
      const { DatabaseSync } = require('node:sqlite');
      const b = new DatabaseSync(${JSON.stringify(dbPath)});
      b.exec("INSERT INTO live_conn_probe VALUES ('b')");
      b.close();
    `], { stdio: 'pipe' });
    expect(fs.existsSync(`${dbPath}-wal`)).toBe(true);

    db.exec("INSERT INTO live_conn_probe VALUES ('c')");
    expect(db.prepare('SELECT count(*) AS n FROM live_conn_probe').get()).toEqual({ n: 3 });
  });
});
