/**
 * Project writer lock (#1740) — unit coverage for acquire / re-entrant /
 * stale-dead-pid / live-holder refusal.
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import { spawn, type ChildProcess } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import filesystem from 'fs';
import { syncBuiltinESMExports } from 'node:module';
import { MCPEngine } from '../src/mcp/engine';
import {
  decodeWriterLockInfo,
  getWriterPidPath,
  releaseWriterLock,
  tryAcquireWriterLock,
  writerLockHeldMessage,
} from '../src/mcp/writer-lock';
import { builtinPatchingReachesImporters } from './helpers/runtime-capabilities';

describe('writer lock (#1740)', () => {
  let dir: string;
  const foreignHolders: ChildProcess[] = [];
  let holder: ChildProcess | null = null;

  afterEach(() => {
    vi.restoreAllMocks();
    syncBuiltinESMExports();
    try { holder?.kill('SIGKILL'); } catch { /* already gone */ }
    holder = null;
    for (const child of foreignHolders) {
      try { child.kill('SIGKILL'); } catch { /* already gone */ }
    }
    foreignHolders.length = 0;
    if (dir) {
      releaseWriterLock(dir);
      try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ }
    }
  });

  function makeProject(): string {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg1740-lock-'));
    fs.mkdirSync(path.join(dir, '.codegraph'), { recursive: true });
    return dir;
  }

  it('acquires and releases writer.pid', () => {
    const root = makeProject();
    const r = tryAcquireWriterLock(root, 'direct');
    expect(r.kind).toBe('acquired');
    expect(fs.existsSync(getWriterPidPath(root))).toBe(true);
    const info = decodeWriterLockInfo(fs.readFileSync(getWriterPidPath(root), 'utf8'));
    expect(info?.pid).toBe(process.pid);
    expect(info?.mode).toBe('direct');
    releaseWriterLock(root);
    expect(fs.existsSync(getWriterPidPath(root))).toBe(false);
  });

  it('is re-entrant for the same pid', () => {
    const root = makeProject();
    expect(tryAcquireWriterLock(root, 'daemon').kind).toBe('acquired');
    const again = tryAcquireWriterLock(root, 'fallback');
    expect(again.kind).toBe('acquired');
    releaseWriterLock(root);
  });

  it('preserves an uncertain writer record at ordinary candidate acquisition', () => {
    const root = makeProject();
    const file = getWriterPidPath(root);
    fs.writeFileSync(file, 'uncertain');
    expect(tryAcquireWriterLock(root, 'daemon', 'writer.pid', { preserveUncertain: true }).kind).toBe('taken');
    expect(fs.readFileSync(file, 'utf8')).toBe('uncertain');
  });

  it.skipIf(!builtinPatchingReachesImporters).each(['uncertain', 'invalid-pid', 'same-pid-successor'])(
    'preserves a %s record arriving during stale-writer cleanup', (successor) => {
      const root = makeProject();
      const file = getWriterPidPath(root);
      const deadPid = 2147483646;
      fs.writeFileSync(file, JSON.stringify({ pid: deadPid, mode: 'daemon', startedAt: 1 }));
      const next = successor === 'uncertain' ? 'uncertain' : JSON.stringify({
        pid: successor === 'invalid-pid' ? 0 : deadPid, mode: 'direct', startedAt: 2,
      });
      const original = filesystem.readFileSync;
      let reads = 0;
      vi.spyOn(filesystem, 'readFileSync').mockImplementation(((target: fs.PathOrFileDescriptor, ...args: unknown[]) => {
        if (target === file && ++reads === 2) fs.writeFileSync(file, next);
        return Reflect.apply(original, filesystem, [target, ...args]);
      }) as typeof filesystem.readFileSync);
      syncBuiltinESMExports();
      expect(tryAcquireWriterLock(root, 'daemon', 'writer.pid', { preserveUncertain: true }).kind).toBe('taken');
      expect(fs.readFileSync(file, 'utf8')).toBe(next);
      expect(reads).toBeGreaterThanOrEqual(2);
    },
  );

  it('reports taken when a live foreign pid holds the lock', () => {
    const root = makeProject();
    // A foreign process that is genuinely alive, rather than a pid assumed to
    // be: PID 1 is init on Linux but does not exist on Windows, where the lock
    // then reads the holder as dead and correctly acquires — the assertion was
    // failing on the fixture, not on the lock. A parked child is alive
    // everywhere, and it is what the lock actually promises not to steal from.
    const holder = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 60000)'], {
      stdio: 'ignore',
    });
    foreignHolders.push(holder);
    expect(holder.pid).toBeGreaterThan(0);
    fs.writeFileSync(
      getWriterPidPath(root),
      JSON.stringify({ pid: holder.pid, mode: 'direct', startedAt: Date.now() }) + '\n',
      { flag: 'wx' },
    );
    const r = tryAcquireWriterLock(root, 'direct');
    expect(r.kind).toBe('taken');
    if (r.kind === 'taken') {
      expect(r.existing?.pid).toBe(holder.pid);
      const msg = writerLockHeldMessage(r.existing, r.pidPath);
      expect(msg).toMatch(/writer lock held/i);
      expect(msg).toMatch(/CODEGRAPH_NO_DAEMON/);
      expect(msg).toMatch(/daemon stop/);
    }
  });

  it('clears a stale dead-pid lock and acquires', () => {
    const root = makeProject();
    // Pick a pid that is extremely unlikely to be alive.
    const deadPid = 2147483646;
    fs.writeFileSync(
      getWriterPidPath(root),
      JSON.stringify({ pid: deadPid, mode: 'direct', startedAt: Date.now() }) + '\n',
    );
    const r = tryAcquireWriterLock(root, 'direct');
    expect(r.kind).toBe('acquired');
    releaseWriterLock(root);
  });

  it('lets a fallback engine atomically claim and release writer ownership', () => {
    const root = makeProject();
    const engine = new MCPEngine({ writerLockRoot: root });

    expect(decodeWriterLockInfo(fs.readFileSync(getWriterPidPath(root), 'utf8'))).toMatchObject({
      pid: process.pid,
      mode: 'fallback',
    });

    engine.stop();
    expect(fs.existsSync(getWriterPidPath(root))).toBe(false);
  });

  it('rejects a fallback engine before opening when another process owns writer.pid', () => {
    const root = makeProject();
    holder = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
    if (!holder.pid) throw new Error('Failed to spawn writer-lock holder');
    fs.writeFileSync(
      getWriterPidPath(root),
      JSON.stringify({ pid: holder.pid, mode: 'daemon', startedAt: Date.now() }) + '\n',
    );

    expect(() => new MCPEngine({ writerLockRoot: root })).toThrow(/writer lock held/i);
  });
});
