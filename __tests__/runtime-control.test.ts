import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as net from 'node:net';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import {
  getRuntimeIdentity, checkRuntimeReady, reserveRuntimeWriter, claimRuntimeWriter, releaseRuntimeWriter,
  stopRuntime, type WriterReservation,
} from '../src/runtime-control';
import { readWriterLock, releaseWriterLock, markWriterReady, tryAcquireWriterLock } from '../src/mcp/writer-lock';
import { ToolHandler } from '../src/mcp/tools';
import { getDaemonPidPath, getDaemonSocketPath, encodeLockInfo } from '../src/mcp/daemon-paths';

let root: string;
let server: net.Server | null = null;
let statusProjectPath: unknown;
const actors: ChildProcess[] = [];
beforeEach(() => {
  statusProjectPath = null;
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-runtime-'));
  fs.mkdirSync(path.join(root, '.codegraph'));
});
afterEach(async () => {
  for (const actor of actors.splice(0)) {
    if (actor.exitCode === null && actor.signalCode === null) {
      const exited = once(actor, 'exit');
      actor.kill('SIGKILL');
      await exited;
    }
  }
  if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  server = null;
  releaseWriterLock(root);
  fs.rmSync(root, { recursive: true, force: true });
});

describe('runtime writer reservations', () => {
  it('hands a serialized updater reservation to a bootstrap process without freeing the slot', () => {
    const reservation = reserveRuntimeWriter(root, process.pid)!;
    expect(reservation).not.toBeNull();
    expect(reserveRuntimeWriter(root, process.pid)).toBeNull();
    const actor = spawnSync(process.execPath, ['-e', `
      const api = require(process.argv[1]);
      const writer = require(process.argv[2]);
      const root = process.argv[3], lease = JSON.parse(process.argv[4]);
      if (!api.claimRuntimeWriter(root, lease)) process.exit(2);
      // Both the deployed legacy CLI and current daemon accept an existing own-PID record.
      const acquired = writer.tryAcquireWriterLock(root, 'daemon');
      console.log(JSON.stringify({pid:process.pid, acquired:acquired.kind, info:writer.readWriterLock(root)}));
    `, path.resolve(__dirname, '../dist/runtime-control.js'), path.resolve(__dirname, '../dist/mcp/writer-lock.js'), root, JSON.stringify(reservation)], { encoding: 'utf8', timeout: 10000 });
    expect(actor.status, actor.stderr).toBe(0);
    const result = JSON.parse(actor.stdout);
    expect(result.acquired).toBe('acquired');
    expect(result.info).toMatchObject({ pid: result.pid, mode: 'daemon', ready: false });
    expect(releaseRuntimeWriter(root, reservation)).toBe(true);
    expect(readWriterLock(root)?.pid).toBe(result.pid);
  });

  it('does not let a released lease claim or remove another reservation by the same PID', () => {
    const previous = reserveRuntimeWriter(root, process.pid)!;
    expect(releaseRuntimeWriter(root, previous)).toBe(true);
    const next = reserveRuntimeWriter(root, process.pid)!;
    expect(claimRuntimeWriter(root, previous)).toBe(false);
    expect(releaseRuntimeWriter(root, previous)).toBe(true);
    expect(readWriterLock(root)).toMatchObject(next);
    expect(claimRuntimeWriter(root, next)).toBe(true);
    markWriterReady(root);
    expect(readWriterLock(root)).toMatchObject({ pid: process.pid, mode: 'daemon', ready: true });
  });

  it('reports an unreadable reservation as incomplete release, preserving the record', () => {
    const actor = spawnSync(process.execPath, ['-e', `
      const fs = require('node:fs'), path = require('node:path');
      const api = require(process.argv[1]), root = process.argv[2];
      const lease = api.reserveRuntimeWriter(root, process.pid);
      const lock = path.join(root, '.codegraph', 'writer.pid');
      const original = fs.readFileSync;
      const before = original(lock, 'utf8');
      fs.readFileSync = function(file, ...args) {
        if (file === lock) throw Object.assign(new Error('injected read failure'), {code:'EIO'});
        return original.call(this, file, ...args);
      };
      const released = api.releaseRuntimeWriter(root, lease);
      fs.readFileSync = original;
      console.log(JSON.stringify({released, unchanged:original(lock, 'utf8') === before}));
    `, path.resolve(__dirname, '../dist/runtime-control.js'), root], { encoding: 'utf8', timeout: 10000 });
    expect(actor.status, actor.stderr).toBe(0);
    expect(JSON.parse(actor.stdout)).toEqual({ released: false, unchanged: true });
  });

  it('rejects a dead updater before creating ownership', () => {
    const actor = spawnSync(process.execPath, ['-e', 'console.log(process.pid)'], { encoding: 'utf8' });
    expect(actor.status).toBe(0);
    expect(() => reserveRuntimeWriter(root, Number(actor.stdout))).toThrow('live holder PID');
    expect(readWriterLock(root)).toBeNull();
  });

  it('preserves ownership across every four-event ordering of stale release, claim, reservation and readiness', () => {
    const events = ['reserve', 'claim', 'release', 'old-release', 'ready', 'fallback'] as const;
    const failures: string[] = [];
    let protectedSuccessors = 0;
    function check(sequence: typeof events[number][]): void {
      releaseWriterLock(root);
      let lease: WriterReservation | null = null;
      let old: WriterReservation | null = null;
      for (const event of sequence) {
        const before = readWriterLock(root);
        if (event === 'reserve') {
          const candidate = reserveRuntimeWriter(root, process.pid);
          if (candidate) { if (lease) old = lease; lease = candidate; }
        } else if (event === 'claim' && lease) claimRuntimeWriter(root, lease);
        else if (event === 'release' && lease) { releaseRuntimeWriter(root, lease); old = lease; lease = null; }
        else if (event === 'old-release' && old) releaseRuntimeWriter(root, old);
        else if (event === 'ready') markWriterReady(root);
        else if (event === 'fallback') tryAcquireWriterLock(root, 'fallback');
        const after = readWriterLock(root);
        if (event === 'old-release' && old && before && before.mode !== old.mode) {
          protectedSuccessors++;
          if (JSON.stringify(before) !== JSON.stringify(after)) failures.push(sequence.join(' -> '));
        }
        if (event === 'fallback' && before && after?.mode !== before.mode) failures.push(sequence.join(' -> '));
      }
    }
    for (const a of events) for (const b of events) for (const c of events) for (const d of events) check([a, b, c, d]);
    expect(protectedSuccessors).toBeGreaterThan(0);
    expect(failures).toEqual([]);
  });
});

async function fakeDaemon(options: { wrongHello?: boolean; wrongBuild?: boolean; statusError?: boolean; guidance?: boolean; closeEarly?: boolean; legacy?: boolean; changeRecord?: boolean } = {}): Promise<void> {
  const guidance = options.guidance ? await new ToolHandler(null).execute('codegraph_status', {}) : null;
  const socketPath = getDaemonSocketPath(root);
  const info = { pid: process.pid, version: 'test-build', socketPath, startedAt: Date.now() };
  fs.writeFileSync(getDaemonPidPath(root), encodeLockInfo(info));
  server = net.createServer((socket) => {
    socket.setEncoding('utf8');
    socket.write(JSON.stringify({ protocol: 1, pid: process.pid, codegraph: options.wrongHello ? 'other' : 'test-build', ...(options.legacy ? {} : { writerProtocol: 1 }) }) + '\n');
    let buffer = '';
    socket.on('data', (chunk) => {
      buffer += chunk;
      let end: number;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const msg = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
        if (msg.method === 'initialize') {
          if (options.closeEarly) { socket.end(); return; }
          socket.write(JSON.stringify({ jsonrpc: '2.0', id: 1, result: { serverInfo: { version: options.wrongBuild ? 'other' : 'test-build' } } }) + '\n');
        } else if (msg.id === 2) {
          statusProjectPath = msg.params?.arguments?.projectPath;
          if (options.changeRecord) fs.writeFileSync(getDaemonPidPath(root), encodeLockInfo({ ...info, version: 'other' }));
          socket.write(JSON.stringify({ jsonrpc: '2.0', id: 2, result: { isError: !!options.statusError, content: [{ type: 'text', text: options.guidance ? guidance!.content[0].text : '**CodeGraph Status**\n**Server build:** test-build\n**Files indexed:** 1\n**Total nodes:** 2\n**Total edges:** 1' }] } }) + '\n');
        }
      }
    });
  });
  await new Promise<void>((resolve, reject) => { server!.once('error', reject); server!.listen(socketPath, resolve); });
}

describe('runtime identity and readiness', () => {
  it('verifies a daemon build through hello, MCP initialization and status', async () => {
    await fakeDaemon();
    expect(await getRuntimeIdentity(root)).toEqual({ pid: process.pid, version: 'test-build' });
    expect(await checkRuntimeReady(root, { pid: process.pid, version: 'test-build' }, 1000)).toEqual({ pid: process.pid, version: 'test-build' });
  });

  it('resolves a relative caller root before sending status to a daemon with another working directory', async () => {
    await fakeDaemon();
    await checkRuntimeReady(path.relative(process.cwd(), root), { pid: process.pid, version: 'test-build' }, 1000);
    expect(path.isAbsolute(statusProjectPath as string)).toBe(true);
    expect(fs.realpathSync.native(statusProjectPath as string)).toBe(fs.realpathSync.native(root));
  });

  it.each(['wrongHello', 'wrongBuild', 'statusError', 'guidance', 'closeEarly', 'changeRecord'] as const)(
    'refuses readiness when %s invalidates the expected daemon', async (failure) => {
      await fakeDaemon({ [failure]: true });
      await expect(checkRuntimeReady(root, { pid: process.pid, version: 'test-build' }, 1000)).rejects.toThrow();
    },
  );

  it('preserves a legacy daemon by default, then stops it only after explicit quiescence', async () => {
    const actor = spawn(process.execPath, [path.resolve(__dirname, 'fixtures/runtime-control-daemon.cjs'),
      root, getDaemonSocketPath(root), 'legacy', path.resolve(__dirname, '../dist/mcp/writer-lock.js')],
      { stdio: ['ignore', 'ignore', 'pipe', 'ipc'] });
    actors.push(actor);
    await once(actor, 'message');
    const before = fs.readFileSync(getDaemonPidPath(root), 'utf8');
    expect(await stopRuntime(root)).toMatchObject({ pid: actor.pid, outcome: 'legacy-writer' });
    expect(actor.exitCode).toBeNull();
    expect(fs.readFileSync(getDaemonPidPath(root), 'utf8')).toBe(before);
    const exited = once(actor, 'exit');
    expect(await stopRuntime(root, { legacyQuiescent: true })).toMatchObject({ pid: actor.pid, outcome: 'term' });
    await exited;
    const lease = reserveRuntimeWriter(root, process.pid);
    expect(lease).not.toBeNull();
    expect(readWriterLock(root)).toMatchObject(lease!);
  });

  it('preserves an unverified live daemon rather than authorizing an artifact swap', async () => {
    await fakeDaemon({ wrongHello: true });
    const before = fs.readFileSync(getDaemonPidPath(root), 'utf8');
    expect(await stopRuntime(root)).toMatchObject({ pid: process.pid, outcome: 'unverified' });
    expect(fs.readFileSync(getDaemonPidPath(root), 'utf8')).toBe(before);
  });
});
