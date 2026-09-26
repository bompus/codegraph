/**
 * One temp directory per test file, removed when the file's tests finish.
 *
 * `run-tmpdir.global-setup.ts` already collects a whole run's temp files in
 * one directory. That still let a long run hold every file's leftovers until
 * the end, and some suites never delete what they `mkdtemp` (helpers without
 * a matching cleanup, early returns, subprocesses that write after a test).
 * Scoping TMPDIR to the test file removes each file's leftovers as soon as it
 * is done, for existing suites and new ones alike.
 *
 * Each file gets its own directory, so parallel workers never remove each
 * other's files. TMPDIR is restored afterwards because a worker process runs
 * several files in turn.
 */
import { afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

const saved = { TMPDIR: process.env.TMPDIR, TMP: process.env.TMP, TEMP: process.env.TEMP };
const fileDir = fs.mkdtempSync(path.join(os.tmpdir(), 'f-'));
// os.tmpdir() reads TMPDIR on POSIX and TMP/TEMP on Windows, on every call.
process.env.TMPDIR = process.env.TMP = process.env.TEMP = fileDir;

afterAll(() => {
  for (const [k, v] of Object.entries(saved)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
  fs.rmSync(fileDir, { recursive: true, force: true });
});
