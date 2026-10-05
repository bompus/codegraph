/** Supported daemon control for installers. Process launch and artifact swaps belong to the caller. */
import * as fs from 'fs';
import * as net from 'net';
import { canonicalProjectRoot } from './directory';
import { getDaemonPidPath, decodeLockInfo, probeDaemonIdentity } from './mcp/daemon-paths';
import { isProcessAlive, stopDaemonAt, type StopResult } from './mcp/daemon-registry';
import { reserveWriterLock, updateWriterLock, type WriterLockInfo } from './mcp/writer-lock';

export const RUNTIME_CONTROL_PROTOCOL = 1;
export interface RuntimeIdentity { pid: number; version: string }
/** Opaque, JSON-serializable reservation. Retain the exact value until claim or release. */
export interface WriterReservation { pid: number; mode: string; startedAt: number }

export async function getRuntimeIdentity(projectRoot: string): Promise<RuntimeIdentity | null> {
  projectRoot = canonicalProjectRoot(projectRoot);
  let raw: string;
  try { raw = fs.readFileSync(getDaemonPidPath(projectRoot), 'utf8'); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
  const info = decodeLockInfo(raw);
  if (!info || !await probeDaemonIdentity(info)) return null;
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
): Promise<RuntimeIdentity> {
  projectRoot = canonicalProjectRoot(projectRoot);
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new Error('Readiness timeout must be positive.');
  const raw = fs.readFileSync(getDaemonPidPath(projectRoot), 'utf8');
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
  const after = await getRuntimeIdentity(projectRoot);
  if (after?.pid !== expected.pid || after.version !== expected.version) {
    throw new Error('Daemon identity changed after readiness.');
  }
  return expected;
}
