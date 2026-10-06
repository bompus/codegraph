import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as fs from 'node:fs';
import * as net from 'node:net';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { syncBuiltinESMExports } from 'node:module';
import {
  getRuntimeIdentity, checkRuntimeReady, reserveRuntimeWriter, claimRuntimeWriter, releaseRuntimeWriter,
  stopRuntime, startRuntimeWatcher, type WriterReservation,
} from '../src/runtime-control';
import { readWriterLock, releaseWriterLock, markWriterReady, tryAcquireWriterLock } from '../src/mcp/writer-lock';
import { ToolHandler } from '../src/mcp/tools';
import { getDaemonPidPath, getDaemonSocketPath, encodeLockInfo } from '../src/mcp/daemon-paths';
import { CodeGraph } from '../src';
import { CodeGraphPackageVersion } from '../src/mcp/version';
import childProcess from 'child_process';
import { MCPServer } from '../src/mcp';
import * as proxy from '../src/mcp/proxy';
import * as daemonRegistry from '../src/mcp/daemon-registry';
import { getTelemetry } from '../src/telemetry';
import * as updateCheck from '../src/upgrade/update-check';
import { getWriterPidPath } from '../src/mcp/writer-lock';

let root: string;
let server: net.Server | null = null;
let statusProjectPath: unknown;
const actors: ChildProcess[] = [];
const detachedActors: number[] = [];
function spySpawn() {
  const spy = vi.spyOn(childProcess, 'spawn');
  syncBuiltinESMExports();
  return spy;
}
beforeEach(() => {
  statusProjectPath = null;
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-runtime-'));
  fs.mkdirSync(path.join(root, '.codegraph'));
});
afterEach(async () => {
  vi.restoreAllMocks();
  syncBuiltinESMExports();
  vi.unstubAllEnvs();
  for (const pid of detachedActors.splice(0)) {
    if (!daemonRegistry.isProcessAlive(pid)) continue;
    process.kill(pid, 'SIGKILL');
    const deadline = Date.now() + 5000;
    while (daemonRegistry.isProcessAlive(pid) && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    expect(daemonRegistry.isProcessAlive(pid)).toBe(false);
  }
  const winner = readWriterLock(root)?.pid;
  for (const actor of actors.splice(0).sort((a, b) => Number(a.pid === winner) - Number(b.pid === winner))) {
    if (actor.exitCode === null && actor.signalCode === null) {
      const exited = once(actor, 'exit');
      actor.kill('SIGKILL');
      await exited;
    }
  }
  if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  server = null;
  releaseWriterLock(root);
  fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
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
    const alias = path.join(root, 'alias');
    fs.symlinkSync(root, alias, 'junction');
    const actor = spawnSync(process.execPath, ['-e', `
      const fs = require('node:fs'), path = require('node:path');
      const api = require(process.argv[1]), root = process.argv[2];
      const lease = api.reserveRuntimeWriter(root, process.pid);
      const lock = fs.realpathSync.native(path.join(root, '.codegraph', 'writer.pid'));
      const original = fs.readFileSync;
      const before = original(lock, 'utf8');
      let injectedReads = 0;
      fs.readFileSync = function(file, ...args) {
        if (file === lock) {
          injectedReads++;
          throw Object.assign(new Error('injected read failure'), {code:'EIO'});
        }
        return original.call(this, file, ...args);
      };
      const released = api.releaseRuntimeWriter(root, lease);
      fs.readFileSync = original;
      console.log(JSON.stringify({released, injectedReads, unchanged:original(lock, 'utf8') === before}));
    `, path.resolve(__dirname, '../dist/runtime-control.js'), alias], { encoding: 'utf8', timeout: 10000 });
    expect(actor.status, actor.stderr).toBe(0);
    const { injectedReads, ...result } = JSON.parse(actor.stdout);
    expect(result).toEqual({ released: false, unchanged: true });
    expect(injectedReads).toBe(1);
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

async function fakeDaemon(options: { wrongHello?: boolean; wrongBuild?: boolean; statusError?: boolean; guidance?: boolean; closeEarly?: boolean; legacy?: boolean; changeRecord?: boolean;
  version?: string; watcherRoot?: string; inactive?: boolean; initializing?: boolean; stopWatchingDuringStatus?: boolean;
  changeWriterGeneration?: boolean;
} = {}): Promise<void> {
  const guidance = options.guidance ? await new ToolHandler(null).execute('codegraph_status', {}) : null;
  const socketPath = getDaemonSocketPath(root);
  const version = options.version ?? 'test-build';
  const info = { pid: process.pid, version, socketPath, startedAt: Date.now() };
  let watching = !options.inactive;
  fs.writeFileSync(getDaemonPidPath(root), encodeLockInfo(info));
  server = net.createServer((socket) => {
    socket.setEncoding('utf8');
    socket.write(JSON.stringify({ protocol: 1, pid: process.pid, codegraph: options.wrongHello ? 'other' : version,
      ...(options.legacy ? {} : { writerProtocol: 1 }),
      watcher: { projectRoot: options.watcherRoot ?? fs.realpathSync.native(root), active: watching, ready: !options.initializing },
    }) + '\n');
    let buffer = '';
    socket.on('data', (chunk) => {
      buffer += chunk;
      let end: number;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const msg = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
        if (msg.method === 'initialize') {
          if (options.closeEarly) { socket.end(); return; }
          socket.write(JSON.stringify({ jsonrpc: '2.0', id: 1, result: { serverInfo: { version: options.wrongBuild ? 'other' : version } } }) + '\n');
        } else if (msg.id === 2) {
          statusProjectPath = msg.params?.arguments?.projectPath;
          if (options.changeRecord) fs.writeFileSync(getDaemonPidPath(root), encodeLockInfo({ ...info, version: 'other' }));
          if (options.stopWatchingDuringStatus) watching = false;
          if (options.changeWriterGeneration) fs.writeFileSync(getWriterPidPath(root), JSON.stringify({
            pid: process.pid, mode: 'daemon', ready: true, startedAt: 999 }));
          socket.write(JSON.stringify({ jsonrpc: '2.0', id: 2, result: { isError: !!options.statusError, content: [{ type: 'text', text: options.guidance ? guidance!.content[0].text : `**CodeGraph Status**\n**Server build:** ${version}\n**Files indexed:** 1\n**Total nodes:** 2\n**Total edges:** 1` }] } }) + '\n');
        }
      }
    });
  });
  await new Promise<void>((resolve, reject) => { server!.once('error', reject); server!.listen(socketPath, resolve); });
}

describe('runtime identity and readiness', () => {
  it.each(['inactive', 'initializing', 'legacy', 'stopWatchingDuringStatus', 'changeRecord', 'changeWriterGeneration'] as const)(
    'never calls a readable daemon watcher-ready when %s', async (failure) => {
      tryAcquireWriterLock(root, 'daemon');
      markWriterReady(root);
      await fakeDaemon({ [failure]: true });
      await expect(checkRuntimeReady(root, { pid: process.pid, version: 'test-build' }, 1000,
        { requireWatcher: true })).rejects.toThrow();
    },
  );

  it('rejects a watcher from another checkout and preserves both ownership records', async () => {
    tryAcquireWriterLock(root, 'daemon');
    markWriterReady(root);
    await fakeDaemon({ watcherRoot: path.dirname(root) });
    const before = fs.readFileSync(getDaemonPidPath(root), 'utf8');
    const writer = readWriterLock(root);
    await expect(checkRuntimeReady(root, { pid: process.pid, version: 'test-build' }, 1000,
      { requireWatcher: true })).rejects.toThrow('exact project watcher');
    expect(fs.readFileSync(getDaemonPidPath(root), 'utf8')).toBe(before);
    expect(readWriterLock(root)).toEqual(writer);
  });
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

describe('ordinary watcher startup', () => {
  const options = () => ({ expectedVersion: CodeGraphPackageVersion,
    cliPath: path.resolve(__dirname, '../dist/bin/codegraph.js'), timeoutMs: 10000 });

  async function initialize(): Promise<void> {
    const cg = await CodeGraph.init(root, { index: false });
    cg.close();
  }

  it('reuses a verified watcher without spawning or changing ownership', async () => {
    await initialize();
    tryAcquireWriterLock(root, 'daemon');
    markWriterReady(root);
    await fakeDaemon({ version: CodeGraphPackageVersion });
    const spawnProbe = spySpawn();
    const before = fs.readFileSync(getDaemonPidPath(root), 'utf8');
    expect(await startRuntimeWatcher(root, options())).toEqual({ pid: process.pid,
      version: CodeGraphPackageVersion, projectRoot: fs.realpathSync.native(root), watching: true });
    expect(spawnProbe).not.toHaveBeenCalled();
    expect(fs.readFileSync(getDaemonPidPath(root), 'utf8')).toBe(before);
  });

  it.each(['legacy', 'direct', 'fallback', 'promotion', 'invalid', 'wrong-build'])(
    'preserves a %s holder rather than launching a second writer', async (holder) => {
      await initialize();
      if (holder === 'legacy') fs.writeFileSync(getDaemonPidPath(root), String(process.pid));
      else if (holder === 'invalid') fs.writeFileSync(getDaemonPidPath(root), 'uncertain');
      else if (holder === 'wrong-build') await fakeDaemon();
      else if (holder === 'promotion') reserveRuntimeWriter(root, process.pid);
      else tryAcquireWriterLock(root, holder);
      const spawnProbe = spySpawn();
      const writer = readWriterLock(root);
      await expect(startRuntimeWatcher(root, options())).rejects.toThrow(/preserved/);
      expect(spawnProbe).not.toHaveBeenCalled();
      expect(readWriterLock(root)).toEqual(writer);
    },
  );

  it('fails compatibility and exact-root validation before any launch', async () => {
    const spawnProbe = spySpawn();
    await expect(startRuntimeWatcher(root, { ...options(), expectedVersion: 'different' })).rejects.toThrow('build');
    await expect(startRuntimeWatcher(root, options())).rejects.toThrow('exact project root');
    expect(spawnProbe).not.toHaveBeenCalled();
    expect(readWriterLock(root)).toBeNull();
  });

  it('does not borrow a parent or linked index for an exact checkout', async () => {
    await initialize();
    const childRoot = path.join(root, 'child');
    fs.mkdirSync(childRoot);
    const launch = spySpawn();
    await expect(startRuntimeWatcher(childRoot, options())).rejects.toThrow('exact project root');
    fs.symlinkSync(path.join(root, '.codegraph'), path.join(childRoot, '.codegraph'), 'junction');
    await expect(startRuntimeWatcher(childRoot, options())).rejects.toThrow('linked project index');
    expect(launch).not.toHaveBeenCalled();
    expect(readWriterLock(root)).toBeNull();
  });

  it('refuses unknown loaded and expected build identities before candidate launch', () => {
    const result = spawnSync(process.execPath, ['-e', `
      require(process.argv[1]).CodeGraphPackageVersion = '0.0.0-unknown';
      require('node:child_process').spawn = () => { throw new Error('candidate launch reached'); };
      require(process.argv[2]).startRuntimeWatcher(process.argv[3], JSON.parse(process.argv[4]))
        .then(() => { console.error('unexpected readiness'); process.exit(1); })
        .catch(error => { console.log(error.message); });
    `, path.resolve(__dirname, '../dist/mcp/version.js'), path.resolve(__dirname, '../dist/runtime-control.js'),
    root, JSON.stringify({ ...options(), expectedVersion: '0.0.0-unknown' })], { encoding: 'utf8', timeout: 10000 });
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain('build is unavailable');
    expect(readWriterLock(root)).toBeNull();
  });

  it('keeps a detached ordinary candidate usable when its initiating caller exits before readiness', async () => {
    await initialize();
    const caller = spawn(process.execPath, ['-e', `
      const childProcess = require('node:child_process');
      const original = childProcess.spawn;
      childProcess.spawn = (...args) => {
        const candidate = original(...args);
        process.send({pid:candidate.pid}, () => process.exit(0));
        return candidate;
      };
      require(process.argv[1]).startRuntimeWatcher(process.argv[2], JSON.parse(process.argv[3]))
        .catch(error => { console.error(error); process.exit(1); });
    `, path.resolve(__dirname, '../dist/runtime-control.js'), root, JSON.stringify(options())],
    { stdio: ['ignore', 'ignore', 'pipe', 'ipc'] });
    actors.push(caller);
    const exited = once(caller, 'exit');
    const [message] = await once(caller, 'message') as [{ pid: number }];
    detachedActors.push(message.pid);
    await exited;
    expect(caller.exitCode).toBe(0);
    const deadline = Date.now() + 10000;
    let identity;
    do {
      identity = await getRuntimeIdentity(root, { requireWatcher: true, timeoutMs: 500 });
      if (identity) break;
      await new Promise((resolve) => setTimeout(resolve, 25));
    } while (Date.now() < deadline);
    expect(identity?.pid).toBe(message.pid);
    const launch = spySpawn();
    expect(await startRuntimeWatcher(root, options())).toMatchObject({ pid: message.pid, watching: true });
    expect(launch).not.toHaveBeenCalled();
  });

  it('preserves an uncertain writer appearing after daemon election but before project activation', async () => {
    await initialize();
    const candidate = spawn(process.execPath, ['-e', `
      const fs = require('node:fs');
      const { MCPEngine } = require(process.argv[1] + '/engine');
      const { Daemon, tryAcquireDaemonLock } = require(process.argv[1] + '/daemon');
      const root = process.argv[2], file = root + '/.codegraph/writer.pid';
      const initialize = MCPEngine.prototype.ensureInitialized;
      MCPEngine.prototype.ensureInitialized = async function(...args) {
        fs.writeFileSync(file, 'uncertain');
        await initialize.apply(this, args);
        await this.getToolHandler().execute('codegraph_status', {});
        process.send({writer:fs.readFileSync(file, 'utf8'), watcher:this.getWatcherState()});
      };
      tryAcquireDaemonLock(root);
      new Daemon(root, {preserveExisting:true, idleTimeoutMs:0}).start()
        .catch(error => { console.error(error); process.exit(1); });
    `, path.resolve(__dirname, '../dist/mcp'), root],
    { stdio: ['ignore', 'ignore', 'pipe', 'ipc'], env: { ...process.env, CODEGRAPH_QUERY_POOL_SIZE: '0' } });
    actors.push(candidate);
    const [state] = await once(candidate, 'message');
    expect(state).toMatchObject({ writer: 'uncertain', watcher: { active: false } });
    expect(fs.readFileSync(getWriterPidPath(root), 'utf8')).toBe('uncertain');
  });

  it('converges racing ordinary starters on one real watcher without a promotion lease', async () => {
    await initialize();
    const original = childProcess.spawn;
    const spawned: ChildProcess[] = [];
    spySpawn().mockImplementation(((...args: Parameters<typeof spawn>) => {
      const child = original(...args);
      spawned.push(child);
      actors.push(child);
      return child;
    }) as typeof spawn);
    const results = await Promise.allSettled([startRuntimeWatcher(root, options()), startRuntimeWatcher(root, options())]);
    const successes = results.flatMap((r) => r.status === 'fulfilled' ? [r.value] : []);
    expect(successes.length).toBeGreaterThan(0);
    expect(new Set(successes.map((r) => r.pid)).size).toBe(1);
    const winner = successes[0]!;
    expect(spawned.some((child) => child.pid === winner.pid)).toBe(true);
    expect(readWriterLock(root)).toMatchObject({ pid: winner.pid, mode: 'daemon', ready: true });
    expect(reserveRuntimeWriter(root, process.pid)).toBeNull();
    await checkRuntimeReady(root, winner, 1000, { requireWatcher: true });
  });

  it.each(['promotion', 'older', 'invalid-writer'])(
    'preserves a %s owner appearing after the empty-slot precheck, at real candidate election', async (event) => {
      await initialize();
      const original = childProcess.spawn;
      let before: string;
      let protectedFile: string;
      spySpawn().mockImplementation(((...args: Parameters<typeof spawn>) => {
        if (event === 'promotion') {
          reserveRuntimeWriter(root, process.pid);
          protectedFile = getWriterPidPath(root);
        } else if (event === 'older') {
          protectedFile = getDaemonPidPath(root);
          fs.writeFileSync(protectedFile, encodeLockInfo({ pid: process.pid, version: '0.0.1',
            socketPath: getDaemonSocketPath(root), startedAt: 1 }));
        } else {
          protectedFile = getWriterPidPath(root);
          fs.writeFileSync(protectedFile, 'uncertain');
        }
        before = fs.readFileSync(protectedFile, 'utf8');
        const child = original(...args);
        actors.push(child);
        return child;
      }) as typeof spawn);
      await expect(startRuntimeWatcher(root, { ...options(), timeoutMs: 1000 })).rejects.toThrow('did not become ready');
      expect(actors).toHaveLength(1);
      expect(fs.readFileSync(protectedFile!, 'utf8')).toBe(before!);
    },
  );

  it('times out with a disabled real watcher without terminating a possibly shared daemon', async () => {
    await initialize();
    const original = childProcess.spawn;
    spySpawn().mockImplementation(((command: string, args: string[], spawnOptions: childProcess.SpawnOptions) => {
      const child = original(command, args, { ...spawnOptions,
        env: { ...spawnOptions.env, CODEGRAPH_NO_WATCH: '1' } });
      actors.push(child);
      return child;
    }) as typeof spawn);
    await expect(startRuntimeWatcher(root, { ...options(), timeoutMs: 1000 })).rejects.toThrow('did not become ready');
    expect(actors).toHaveLength(1);
    expect(actors[0]!.exitCode).toBeNull();
    expect(readWriterLock(root)?.pid).toBe(actors[0]!.pid);
  });
});

describe('preserving MCP launch policy', () => {
  it('preserves owners across all three-attempt orderings of older, unreachable, uncertain and promotion holders', async () => {
    const cg = await CodeGraph.init(root, { index: false });
    cg.close();
    vi.stubEnv('CODEGRAPH_NO_DAEMON', '0');
    vi.stubEnv('CODEGRAPH_DAEMON_INTERNAL', '0');
    vi.spyOn(getTelemetry(), 'startInterval').mockImplementation(() => {});
    vi.spyOn(updateCheck, 'checkForUpdateInBackground').mockImplementation(() => {});
    const connect = vi.spyOn(proxy, 'connectWithHello');
    const stop = vi.spyOn(daemonRegistry, 'stopOlderDaemon');
    const launch = spySpawn();
    vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    const events = ['older', 'unreachable', 'uncertain', 'promotion'] as const;
    const failures: string[] = [];
    let attempts = 0;
    for (const a of events) for (const b of events) for (const c of events) {
      const sequence = [a, b, c];
      vi.spyOn(proxy, 'runLocalHandshakeProxy').mockImplementation(async (deps) => {
        for (const event of sequence) {
          connect.mockResolvedValue(event === 'older' ? 'older-version' : null);
          fs.writeFileSync(getDaemonPidPath(root), event === 'uncertain' ? 'uncertain' : encodeLockInfo({
            pid: process.pid, version: event === 'older' ? '0.0.1' : CodeGraphPackageVersion,
            socketPath: getDaemonSocketPath(root), startedAt: Date.now(),
          }));
          fs.writeFileSync(getWriterPidPath(root), JSON.stringify({ pid: process.pid,
            mode: event === 'promotion' ? 'promotion:held' : 'daemon', startedAt: 1, ready: false }));
          const before = fs.readFileSync(getDaemonPidPath(root), 'utf8');
          const writer = fs.readFileSync(getWriterPidPath(root), 'utf8');
          const socket = await deps.getDaemonSocket();
          attempts++;
          if (socket !== null || fs.readFileSync(getDaemonPidPath(root), 'utf8') !== before ||
              fs.readFileSync(getWriterPidPath(root), 'utf8') !== writer) failures.push(sequence.join(' -> '));
        }
        const fallback = deps.makeEngine();
        expect(fallback.isReadOnly()).toBe(true);
        await fallback.stop();
      });
      await new MCPServer(root, { preserveExisting: true }).start();
    }
    expect(attempts).toBe(192);
    expect(failures).toEqual([]);
    expect(stop).not.toHaveBeenCalled();
    expect(launch).not.toHaveBeenCalled();
  });
});
