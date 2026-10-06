/** Supported daemon control and ordinary startup. Artifact selection and swaps belong to the caller. */
import * as fs from 'fs';
import * as net from 'net';
import * as path from 'path';
import { canonicalProjectRoot, isInitialized, getCodeGraphDir } from './directory';
import { getDaemonPidPath, decodeLockInfo, probeDaemonIdentity } from './mcp/daemon-paths';
import { isProcessAlive, stopDaemonAt, type StopResult } from './mcp/daemon-registry';
import { reserveWriterLock, updateWriterLock, readWriterLock, getWriterPidPath, type WriterLockInfo } from './mcp/writer-lock';
import { CodeGraphPackageVersion } from './mcp/version';

export const RUNTIME_CONTROL_PROTOCOL = 1;
export interface RuntimeIdentity { pid: number; version: string }
export interface RuntimeWatcher extends RuntimeIdentity { projectRoot: string; watching: true }
/** Opaque, JSON-serializable reservation. Retain the exact value until claim or release. */
export interface WriterReservation { pid: number; mode: string; startedAt: number }

export async function getRuntimeIdentity(projectRoot: string, options: { timeoutMs?: number; requireWatcher?: boolean } = {}): Promise<RuntimeIdentity | null> {
  projectRoot = canonicalProjectRoot(projectRoot);
  let raw: string;
  try { raw = fs.readFileSync(getDaemonPidPath(projectRoot), 'utf8'); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
  const info = decodeLockInfo(raw);
  if (!info || !await probeDaemonIdentity(info, options.timeoutMs ?? 1000,
    options.requireWatcher, options.requireWatcher ? projectRoot : undefined)) return null;
  // A probe awaits I/O; a successor may have replaced the record meanwhile.
  try {
    if (fs.readFileSync(getDaemonPidPath(projectRoot), 'utf8') !== raw) return null;
  } catch { return null; }
  return { pid: info.pid, version: info.version };
}

/** Refuse live uncoordinated writers unless the caller has explicitly quiesced legacy sessions. */
export async function stopRuntime(
  projectRoot: string,
  options: { legacyQuiescent?: boolean } = {},
): Promise<StopResult> {
  projectRoot = canonicalProjectRoot(projectRoot);
  return stopDaemonAt(projectRoot, {
    preserveUnverified: true, requireWriterProtocol: !options.legacyQuiescent,
  });
}

/** Claim a free or stale slot for a live updater, after terminal stop and legacy-session quiescence. */
export function reserveRuntimeWriter(projectRoot: string, holderPid: number): WriterReservation | null {
  projectRoot = canonicalProjectRoot(projectRoot);
  const result = reserveWriterLock(projectRoot, holderPid);
  if (result.kind !== 'acquired') return null;
  const { pid, mode, startedAt } = result.info;
  return { pid, mode, startedAt };
}

function validReservation(value: WriterReservation): boolean {
  return Number.isSafeInteger(value.pid) && value.pid > 0
    && typeof value.mode === 'string' && /^promotion:[0-9a-f-]{36}$/.test(value.mode)
    && Number.isSafeInteger(value.startedAt) && value.startedAt > 0;
}

/** Run in the replacement daemon bootstrap, then execute the selected CLI in this same process. */
export function claimRuntimeWriter(projectRoot: string, reservation: WriterReservation): boolean {
  projectRoot = canonicalProjectRoot(projectRoot);
  if (!validReservation(reservation) || !isProcessAlive(reservation.pid)) return false;
  const next: WriterLockInfo = { pid: process.pid, mode: 'daemon', startedAt: Date.now(), ready: false };
  return updateWriterLock(projectRoot, reservation, next);
}

/** Idempotently retire a reservation, preserving successors. Retry false before ending the updater. */
export function releaseRuntimeWriter(projectRoot: string, reservation: WriterReservation): boolean {
  projectRoot = canonicalProjectRoot(projectRoot);
  return validReservation(reservation) && updateWriterLock(projectRoot, reservation, null);
}

/** Verify the hello, MCP build and successful status response of an already discovered daemon. */
export async function checkRuntimeReady(
  projectRoot: string,
  expected: RuntimeIdentity,
  timeoutMs = 120_000,
  options: { requireWatcher?: boolean } = {},
): Promise<RuntimeIdentity> {
  projectRoot = canonicalProjectRoot(projectRoot);
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new Error('Readiness timeout must be positive.');
  const deadline = Date.now() + timeoutMs;
  const raw = fs.readFileSync(getDaemonPidPath(projectRoot), 'utf8');
  const writerRaw = options.requireWatcher ? fs.readFileSync(getWriterPidPath(projectRoot), 'utf8') : null;
  const info = decodeLockInfo(raw);
  if (!info || info.pid !== expected.pid || info.version !== expected.version) {
    throw new Error('Daemon identity changed before readiness.');
  }
  await new Promise<void>((resolve, reject) => {
    const socket = net.createConnection(info.socketPath);
    let buffer = '';
    let phase = 'hello';
    let done = false;
    const finish = (error?: Error): void => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error); else resolve();
    };
    const timer = setTimeout(() => finish(new Error('MCP readiness timed out.')), timeoutMs);
    socket.setEncoding('utf8');
    socket.on('error', finish);
    socket.on('close', () => finish(new Error('Daemon closed before readiness.')));
    socket.on('data', (chunk) => {
      buffer += String(chunk);
      if (buffer.length > 1024 * 1024) return finish(new Error('MCP readiness response exceeds the limit.'));
      let end: number;
      while (!done && (end = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        let msg;
        try { msg = JSON.parse(line); }
        catch { return finish(new Error('Invalid MCP readiness response.')); }
        if (!msg || typeof msg !== 'object') return finish(new Error('Invalid MCP readiness response.'));
        if (phase === 'hello') {
          if (msg.protocol !== 1 || msg.pid !== expected.pid || msg.codegraph !== expected.version) {
            return finish(new Error('Daemon identity changed during readiness.'));
          }
          if (options.requireWatcher && (msg.writerProtocol !== 1 ||
              msg.watcher?.projectRoot !== projectRoot || msg.watcher?.active !== true ||
              msg.watcher?.ready !== true)) {
            return finish(new Error('The exact project watcher is not ready.'));
          }
          phase = 'initialize';
          socket.write(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {
            protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'codegraph-runtime-control', version: '1' },
          } }) + '\n');
        } else if (phase === 'initialize' && msg.id === 1) {
          if (msg.error || msg.result?.serverInfo?.version !== expected.version) {
            return finish(new Error('MCP readiness build mismatch.'));
          }
          phase = 'status';
          socket.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
          socket.write(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: {
            name: 'codegraph_status', arguments: { projectPath: projectRoot },
          } }) + '\n');
        } else if (phase === 'status' && msg.id === 2) {
          const text = Array.isArray(msg.result?.content)
            ? msg.result.content.filter((item: { type?: string; text?: unknown }) => item?.type === 'text' && typeof item.text === 'string')
              .map((item: { text: string }) => item.text).join('\n') : '';
          // Expected-condition guidance is success-shaped too. Require the real
          // project status body, not merely a successful RPC envelope.
          const status = text.includes('**CodeGraph Status**')
            && text.includes(`**Server build:** ${expected.version}`)
            && /^\*\*Files indexed:\*\* \d+$/m.test(text)
            && /^\*\*Total nodes:\*\* \d+$/m.test(text)
            && /^\*\*Total edges:\*\* \d+$/m.test(text);
          finish(msg.error || msg.result?.isError || !status
            ? new Error('MCP status failed readiness.') : undefined);
        }
      }
    });
  });
  if (Date.now() >= deadline) throw new Error('MCP readiness timed out.');
  const after = await getRuntimeIdentity(projectRoot, { timeoutMs: deadline - Date.now(), requireWatcher: options.requireWatcher });
  if (after?.pid !== expected.pid || after.version !== expected.version) {
    throw new Error('Daemon identity changed after readiness.');
  }
  if (options.requireWatcher) {
    const writer = readWriterLock(projectRoot);
    if (writer?.pid !== expected.pid || writer.mode !== 'daemon' || writer.ready !== true ||
        fs.readFileSync(getDaemonPidPath(projectRoot), 'utf8') !== raw ||
        fs.readFileSync(getWriterPidPath(projectRoot), 'utf8') !== writerRaw) {
      throw new Error('Watcher ownership changed after readiness.');
    }
  }
  return expected;
}

/** Reuse or elect an exact-checkout watcher without replacement or promotion. */
export async function startRuntimeWatcher(projectRoot: string, options: {
  expectedVersion: string; cliPath: string; runtimePath?: string; timeoutMs?: number;
}): Promise<RuntimeWatcher> {
  projectRoot = canonicalProjectRoot(projectRoot);
  const timeoutMs = options.timeoutMs ?? 120_000;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new Error('Readiness timeout must be positive.');
  if (CodeGraphPackageVersion === '0.0.0-unknown' || options.expectedVersion !== CodeGraphPackageVersion) {
    throw new Error('Runtime-control build is unavailable or does not match the expected watcher build.');
  }
  if (!path.isAbsolute(options.cliPath) || !fs.statSync(options.cliPath).isFile() ||
      (options.runtimePath && !path.isAbsolute(options.runtimePath))) {
    throw new Error('Watcher startup requires an absolute CLI and runtime path.');
  }
  if (!isInitialized(projectRoot)) throw new Error('Watcher startup requires an index at the exact project root.');
  for (const file of [getCodeGraphDir(projectRoot), path.join(getCodeGraphDir(projectRoot), 'codegraph.db')]) {
    if (fs.lstatSync(file).isSymbolicLink()) throw new Error('Watcher startup refuses a linked project index.');
  }
  const deadline = Date.now() + timeoutMs;
  let launched = false;
  let launchError: Error | undefined;
  let lastError: unknown;
  do {
    const identity = await getRuntimeIdentity(projectRoot, { timeoutMs: Math.max(1, deadline - Date.now()) });
    if (identity) {
      if (identity.version !== options.expectedVersion) throw new Error('Existing daemon build is preserved; watcher startup is blocked.');
      try {
        await checkRuntimeReady(projectRoot, identity, Math.max(1, deadline - Date.now()), { requireWatcher: true });
        return { ...identity, projectRoot, watching: true };
      } catch (error) { lastError = error; }
    } else if (!launched) {
      // A failed hello is not an empty slot. Existing guards arbitrate the race
      // after this read; the candidate never clears live or uncertain owners.
      for (const file of [getDaemonPidPath(projectRoot), getWriterPidPath(projectRoot)]) {
        try {
          const raw = fs.readFileSync(file, 'utf8');
          const owner = file === getDaemonPidPath(projectRoot) ? decodeLockInfo(raw) : readWriterLock(projectRoot);
          if (!owner || !Number.isSafeInteger(owner.pid) || owner.pid <= 0 || isProcessAlive(owner.pid)) {
            throw new Error('Existing or uncertain writer is preserved; watcher startup is blocked.');
          }
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
        }
      }
      const { spawnDetachedDaemon } = await import('./mcp');
      const child = spawnDetachedDaemon(projectRoot, false, { preserveExisting: true,
        cliPath: options.cliPath, runtimePath: options.runtimePath });
      child.once('error', (error) => { launchError = error; });
      launched = true;
    }
    if (launchError) throw launchError;
    if (Date.now() >= deadline) break;
    await new Promise((resolve) => setTimeout(resolve, Math.min(25, deadline - Date.now())));
  } while (Date.now() < deadline);
  // An elected daemon can already serve other clients. Its idle lifecycle,
  // rather than this caller's timeout, owns retirement of an unused candidate.
  throw new Error(`Watcher startup did not become ready: ${lastError instanceof Error ? lastError.message : 'deadline exceeded'}`);
}
