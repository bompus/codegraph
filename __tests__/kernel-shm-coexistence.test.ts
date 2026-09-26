/**
 * The kernel links its own SQLite build next to node:sqlite. POSIX locks never
 * conflict within one process, so a kernel connection cannot see node:sqlite's
 * locks on the same database. Opened as an ordinary reader it took itself for
 * the first connection and truncated the `-shm` wal-index that node:sqlite
 * still had mapped; node:sqlite's next write past the first 32 KB region was
 * a SIGBUS. A sync after a large checkout hit it in 3 runs of 4.
 *
 * The scenario runs in a child process so a crash is an exit status, not a
 * dead test runner.
 */

import { describe, it, expect } from 'vitest';
import { spawnSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const SCHEMA_PATH = path.join(__dirname, '..', 'src', 'db', 'schema.sql');

const SCENARIO = `
const { DatabaseSync } = require('node:sqlite');
const fs = require('fs');
const path = require('path');
const [kernelPath, dir, schemaPath] = process.argv.slice(1);
const kernel = require(kernelPath);
const dbPath = path.join(dir, 'codegraph.db');
const db = new DatabaseSync(dbPath);
db.exec('PRAGMA page_size = 1024; PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0');
db.exec(fs.readFileSync(schemaPath, 'utf8'));
db.exec('CREATE TABLE filler (b BLOB)');
const fill = () => {
  const ins = db.prepare('INSERT INTO filler VALUES (randomblob(900))');
  db.exec('BEGIN');
  for (let i = 0; i < 20000; i++) ins.run();
  db.exec('COMMIT');
};
fill(); // ~20k WAL frames: the -shm spans several 32 KB regions
db.prepare('PRAGMA wal_checkpoint(TRUNCATE)').get();
const kr = new kernel.KernelResolver({ dbPath, projectRoot: dir, nodeBuiltinSpecifiers: [], frameworksActive: false });
fill(); // node:sqlite writes past the first region again
kr.close();
db.close();
`;

describe('kernel connection next to node:sqlite', () => {
  it.runIf(process.platform !== 'win32' && fs.existsSync(KERNEL_PATH))(
    'leaves the -shm node:sqlite has mapped intact',
    () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-shm-'));
      try {
        const run = spawnSync(process.execPath, ['-e', SCENARIO, KERNEL_PATH, dir, SCHEMA_PATH], {
          encoding: 'utf8',
          timeout: 60_000,
        });
        expect(run.signal).toBeNull();
        expect(run.status, run.stderr).toBe(0);
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    },
  );
});
