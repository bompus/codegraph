/**
 * Project writer lock (#1740).
 *
 * At most one long-lived MCP *writer* (shared daemon OR direct-mode /
 * in-process engine that owns the FileWatcher) may serve a given project.
 * The shared daemon already multiplexes N stdio proxies onto one writer; this
 * lock closes the same-OS gap where two direct-mode `serve --mcp` processes
 * (via `CODEGRAPH_NO_DAEMON=1` or proxy→in-process fallback) each start a
 * watcher, contend on `codegraph.lock`, and degrade auto-sync.
 *
 * Deliberately separate from `daemon.pid`: proxies probe the daemon socket
 * and may clear a live pid that has no socket. A direct-mode holder must not
 * look like a daemon. `writer.pid` is only about "who owns live auto-sync".
 */

import * as fs from 'fs';
import * as path from 'path';
import { randomUUID } from 'crypto';
import { getCodeGraphDir } from '../directory';
import { requireKernel } from '../extraction/kernel/loader';
/** Signal-0 liveness (EPERM ⇒ alive). Local copy to avoid a daemon↔writer cycle. */
function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err: unknown) {
    const e = err as NodeJS.ErrnoException;
    if (e.code === 'EPERM') return true;
    return false;
  }
}


/** Absolute path to the writer pid lockfile for `projectRoot`. */
export function getWriterPidPath(projectRoot: string, lockName: 'writer.pid' | 'rebuild.pid' = 'writer.pid'): string {
  let root = projectRoot;
  try { root = fs.realpathSync(projectRoot); } catch { /* keep lexical */ }
  return path.join(getCodeGraphDir(root), lockName);
}

/** Structured contents of the writer pidfile. */
/**
 * Set by the refresh launcher on every child it owns. Such a child may find
 * the lock held by the sibling it is replacing, which is not the second
 * independent writer #1740 refuses — it is the successor, and the holder exits
 * at the switch. Marked children wait for that release instead of exiting.
 */
export const WRITER_LOCK_DEFER_ENV = 'CODEGRAPH_WRITER_LOCK_DEFER';

export interface WriterLockInfo {
  pid: number;
  /** `direct` | `daemon` | `fallback` — for actionable error text only. */
  mode: string;
  startedAt: number;
  /** False until the MCP owner has finished its initial catch-up. */
  ready?: boolean;
}

export type WriterAcquireResult =
  | { kind: 'acquired'; pidPath: string; info: WriterLockInfo }
  | { kind: 'taken'; existing: WriterLockInfo | null; pidPath: string };

const pendingReleases = new Map<string, NodeJS.Timeout>();
const pendingReadiness = new Map<string, NodeJS.Timeout>();

function cancelPendingReadiness(pidPath: string): void {
  const timer = pendingReadiness.get(pidPath);
  if (timer) clearTimeout(timer);
  pendingReadiness.delete(pidPath);
}

function cancelPendingRelease(pidPath: string): void {
  const timer = pendingReleases.get(pidPath);
  if (timer) clearTimeout(timer);
  pendingReleases.delete(pidPath);
}

/** Serialize every ownership comparison and mutation, including stale cleanup.
 * Keep the guard file permanently; the OS releases its lock on process exit. */
function lockWriterMutation(pidPath: string, waitMs = 1000): { release(): void } | null {
  const acquire = requireKernel().tryWriterMutationLock;
  if (!acquire) throw new Error('Native kernel lacks writer coordination; rebuild or reinstall CodeGraph.');
  const deadline = Date.now() + waitMs;
  for (;;) {
    const lock = acquire(`${pidPath}.mutation.lock`);
    if (lock) return lock;
    if (Date.now() >= deadline) return null;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5);
  }
}

function encode(info: WriterLockInfo): string {
  return JSON.stringify(info) + '\n';
}

export function decodeWriterLockInfo(raw: string): WriterLockInfo | null {
  try {
    const parsed = JSON.parse(raw.trim()) as Partial<WriterLockInfo>;
    if (typeof parsed.pid !== 'number' || typeof parsed.mode !== 'string') return null;
    return {
      pid: parsed.pid,
      mode: parsed.mode,
      startedAt: typeof parsed.startedAt === 'number' ? parsed.startedAt : 0,
      ...(typeof parsed.ready === 'boolean' ? { ready: parsed.ready } : {}),
    };
  } catch {
    return null;
  }
}

/**
 * Atomically create `writer.pid` (link-into-place, O_EXCL fallback). If held
 * by a dead PID, clear and retry once. Does not steal from a live holder.
 */
export function tryAcquireWriterLock(
  projectRoot: string,
  mode: string,
  lockName: 'writer.pid' | 'rebuild.pid' = 'writer.pid',
  options: { preserveUncertain?: boolean } = {},
): WriterAcquireResult {
  return acquireWriterRecord(projectRoot, {
    pid: process.pid, mode, startedAt: Date.now(), ready: false,
  }, lockName, true, options.preserveUncertain);
}

/** Reserve for a live external updater. A unique mode distinguishes repeated leases by one PID. */
export function reserveWriterLock(projectRoot: string, holderPid: number): WriterAcquireResult {
  if (!Number.isSafeInteger(holderPid) || holderPid <= 0 || !isProcessAlive(holderPid)) {
    throw new Error('Writer reservation requires a live holder PID.');
  }
  return acquireWriterRecord(projectRoot, {
    pid: holderPid, mode: `promotion:${randomUUID()}`, startedAt: Date.now(), ready: false,
  }, 'writer.pid', false);
}

function acquireWriterRecord(
  projectRoot: string,
  info: WriterLockInfo,
  lockName: 'writer.pid' | 'rebuild.pid',
  reuseOwnRecord = true,
  preserveUncertain = false,
): WriterAcquireResult {
  const pidPath = getWriterPidPath(projectRoot, lockName);
  fs.mkdirSync(path.dirname(pidPath), { recursive: true });

  const mutation = lockWriterMutation(pidPath);
  if (!mutation) return { kind: 'taken', existing: readWriterLock(projectRoot, lockName), pidPath };
  try {
    if (!isProcessAlive(info.pid)) return { kind: 'taken', existing: readWriterLock(projectRoot, lockName), pidPath };

    let observedRaw: string | null = null;
    const attempt = (): WriterAcquireResult => {
      const tmp = `${pidPath}.${process.pid}.tmp`;
      let acquired = false;
      try {
        fs.writeFileSync(tmp, encode(info), { mode: 0o600 });
        try {
          fs.linkSync(tmp, pidPath);
          acquired = true;
        } catch (err: unknown) {
          if ((err as NodeJS.ErrnoException).code === 'EEXIST') {
            // taken
          } else {
            // No hard links — O_EXCL create.
            try {
              const fd = fs.openSync(pidPath, 'wx', 0o600);
              try {
                fs.writeSync(fd, encode(info));
                acquired = true;
              } finally {
                fs.closeSync(fd);
              }
            } catch (e2: unknown) {
              if ((e2 as NodeJS.ErrnoException).code !== 'EEXIST') throw e2;
            }
          }
        }
      } finally {
        try { fs.unlinkSync(tmp); } catch { /* ignore */ }
      }

      if (acquired) {
        cancelPendingReadiness(pidPath);
        cancelPendingRelease(pidPath);
        return { kind: 'acquired', pidPath, info };
      }

      let existing: WriterLockInfo | null = null;
      try {
        observedRaw = fs.readFileSync(pidPath, 'utf8');
        existing = decodeWriterLockInfo(observedRaw);
      } catch { /* unreadable */ }
      return { kind: 'taken', existing, pidPath };
    };

    let result = attempt();
    if (reuseOwnRecord && result.kind === 'taken' && result.existing && result.existing.pid === process.pid) {
      // Same process already holds it (daemon acquired before engine watch).
      cancelPendingRelease(pidPath);
      return { kind: 'acquired', pidPath: result.pidPath, info: result.existing };
    }
    if (result.kind === 'taken') {
      const existing = result.existing;
      if (preserveUncertain && (!existing || !Number.isSafeInteger(existing.pid) || existing.pid <= 0)) return result;
      if (!existing || existing.pid <= 0 || !isProcessAlive(existing.pid)) {
        // Stale — clear (pid-verified) and retry once.
        try {
          const raw = fs.readFileSync(pidPath, 'utf8');
          const cur = decodeWriterLockInfo(raw);
          if (preserveUncertain && (raw !== observedRaw || !cur ||
              !Number.isSafeInteger(cur.pid) || cur.pid <= 0)) {
            return { kind: 'taken', existing: cur, pidPath };
          }
          if (!cur || cur.pid === existing?.pid) {
            if (!cur || cur.pid <= 0 || !isProcessAlive(cur.pid)) {
              fs.unlinkSync(pidPath);
            }
          }
        } catch { /* ENOENT ok */ }
        result = attempt();
      }
    }
    return result;
  } finally {
    mutation.release();
  }
}

/** Publish catch-up readiness without exposing a partially-written pidfile.
 * Contention retains a retry until publication or an ownership change. */
export function markWriterReady(projectRoot: string): void {
  const pidPath = getWriterPidPath(projectRoot);
  const retry = (): void => {
    if (pendingReadiness.has(pidPath)) return;
    const timer = setTimeout(() => {
      pendingReadiness.delete(pidPath);
      try {
        publish(0);
      } catch (error) {
        // Permanent failures stay local to publication, like the lifecycle's
        // synchronous catch boundary; an unguarded timer would exit the server.
        process.stderr.write(`[CodeGraph MCP] Writer readiness publication failed: ${error instanceof Error ? error.message : String(error)}\n`);
      }
    }, 100);
    timer.unref();
    pendingReadiness.set(pidPath, timer);
  };
  const publish = (waitMs: number): void => {
    let mutation: { release(): void } | null = null;
    try {
      if (!fs.existsSync(pidPath)) {
        cancelPendingReadiness(pidPath);
        return;
      }
      mutation = lockWriterMutation(pidPath, waitMs);
      if (!mutation) {
        retry();
        return;
      }
      cancelPendingReadiness(pidPath);
      const info = readWriterLock(projectRoot);
      if (!info || info.pid !== process.pid) return;
      const tmp = `${pidPath}.${process.pid}.ready.tmp`;
      try {
        fs.writeFileSync(tmp, encode({ ...info, ready: true }), { mode: 0o600 });
        fs.renameSync(tmp, pidPath);
      } finally {
        try { fs.unlinkSync(tmp); } catch { /* best-effort */ }
      }
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (['EPERM', 'EACCES', 'EBUSY', 'EINTR', 'EAGAIN'].includes(code ?? '')) retry();
      else throw error;
    } finally {
      mutation?.release();
    }
  };
  publish(1000);
}

/** Waits between retries of a writer-record swap refused by Windows (another process has the file open). */
const SWAP_RETRY_DELAYS_MS = [25, 50, 100, 200];

/**
 * Replace the writer record of `fromPid` with `next`, and only that record, so
 * the slot passes between two processes without ever falling free (#2335). A
 * stopped daemon's sessions serve themselves in-process the moment it goes, and
 * one that found the slot free or stale would claim it as their own writer.
 * The OS coordination lock keeps comparison and rename in one critical
 * section. Callers check whether the record was replaced. Never throws.
 */
export function swapWriterLock(projectRoot: string, fromPid: number, next: WriterLockInfo): boolean {
  return updateWriterLock(projectRoot, fromPid, next);
}

/** Compare a reservation generation before transfer or release; old leases cannot affect successors. */
export function updateWriterLock(
  projectRoot: string,
  expected: number | WriterLockInfo,
  next: WriterLockInfo | null,
): boolean {
  const pidPath = getWriterPidPath(projectRoot);
  const tmp = `${pidPath}.${process.pid}.swap.tmp`;
  let mutation: { release(): void } | null = null;
  try {
    mutation = lockWriterMutation(pidPath);
    if (!mutation) return false;
    const matches = (): boolean => {
      let current: WriterLockInfo | null;
      try {
        current = decodeWriterLockInfo(fs.readFileSync(pidPath, 'utf8'));
        if (!current) throw new Error('Writer ownership record is invalid.');
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
        throw error;
      }
      return typeof expected === 'number'
        ? current?.pid === expected
        : current?.pid === expected.pid && current.mode === expected.mode && current.startedAt === expected.startedAt;
    };
    if (!matches()) return next === null;
    if (typeof expected !== 'number' && next && !isProcessAlive(expected.pid)) return false;
    if (!next) {
      fs.unlinkSync(pidPath);
      cancelPendingReadiness(pidPath);
      cancelPendingRelease(pidPath);
      return true;
    }
    fs.writeFileSync(tmp, encode(next), { mode: 0o600 });
    for (let attempt = 0; ; attempt++) {
      try {
        fs.renameSync(tmp, pidPath);
        cancelPendingReadiness(pidPath);
        if (next.pid === process.pid) cancelPendingRelease(pidPath);
        return true;
      } catch (err) {
        const code = (err as NodeJS.ErrnoException).code;
        const delay = SWAP_RETRY_DELAYS_MS[attempt];
        if (process.platform !== 'win32' || !['EPERM', 'EACCES', 'EBUSY'].includes(code ?? '') || delay === undefined) {
          return false;
        }
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, delay);
        if (!matches()) return false;
      }
    }
  } catch {
    return false;
  } finally {
    if (mutation) {
      try { fs.unlinkSync(tmp); } catch { /* renamed, or never written */ }
    }
    mutation?.release();
  }
}

/** Release if we still own the lock. Contention keeps a retry obligation;
 * callers may retire the engine while this process remains alive. */
export function releaseWriterLock(projectRoot: string, lockName: 'writer.pid' | 'rebuild.pid' = 'writer.pid'): void {
  const pidPath = getWriterPidPath(projectRoot, lockName);
  cancelPendingReadiness(pidPath);
  const release = (waitMs: number): void => {
    let mutation: { release(): void } | null = null;
    try {
      if (!fs.existsSync(path.dirname(pidPath))) return;
      mutation = lockWriterMutation(pidPath, waitMs);
      if (!mutation) {
        // One unref'd retry per record. Reacquisition by this process cancels it,
        // so an old retirement cannot release a newly started watcher.
        if (!pendingReleases.has(pidPath)) {
          const timer = setTimeout(() => {
            pendingReleases.delete(pidPath);
            release(0);
          }, 100);
          timer.unref();
          pendingReleases.set(pidPath, timer);
        }
        return;
      }
      cancelPendingRelease(pidPath);
      const info = readWriterLock(projectRoot, lockName);
      if (info?.pid === process.pid) fs.unlinkSync(pidPath);
    } catch { /* best-effort for unavailable files or kernel */ } finally {
      mutation?.release();
    }
  };
  release(1000);
}

/** Read current lock without acquiring. */
export function readWriterLock(projectRoot: string, lockName: 'writer.pid' | 'rebuild.pid' = 'writer.pid'): WriterLockInfo | null {
  const pidPath = getWriterPidPath(projectRoot, lockName);
  try {
    return decodeWriterLockInfo(fs.readFileSync(pidPath, 'utf8'));
  } catch {
    return null;
  }
}

/**
 * Actionable message when another live process owns the writer lock (#1740).
 */
export function writerLockHeldMessage(
  existing: WriterLockInfo | null,
  pidPath: string,
): string {
  const who = existing && existing.pid > 0
    ? `PID ${existing.pid} (${existing.mode || 'unknown'} mode)`
    : 'another process';
  return (
    'CodeGraph writer lock held by ' + who + '. ' +
    'Only one live MCP writer may serve a project (auto-sync / index). ' +
    'Stop the other server (codegraph daemon stop if a shared daemon, or end the other MCP session), ' +
    'or unset CODEGRAPH_NO_DAEMON so additional clients proxy to the shared daemon. ' +
    'If this is stale, delete ' + pidPath
  );
}

/** Rebuild intent is separate from writer ownership: acquire BEFORE stopping
 * the daemon, so its disconnected proxies cannot reopen SQLite in the gap. */
/**
 * An index rebuild (`codegraph index`) owns the database. Expected and brief,
 * so the MCP layer answers it as guidance, never as a tool error (#1325).
 */
export class RebuildInProgressError extends Error {
  constructor() {
    super('CodeGraph index rebuild is in progress; retry when it finishes.');
    this.name = 'RebuildInProgressError';
  }
}

export function assertNoRebuild(root: string): void {
  const lock = readWriterLock(root, 'rebuild.pid');
  if (lock && lock.pid !== process.pid && isProcessAlive(lock.pid)) {
    throw new RebuildInProgressError();
  }
}
