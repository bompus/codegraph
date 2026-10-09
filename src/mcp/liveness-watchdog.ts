/**
 * Main-thread liveness watchdog — belt-and-suspenders for #850.
 *
 * The #850 fix removes the one *known* trigger (the uncaught-exception handler
 * no longer formats a raw Error's `.stack`). But ANY synchronous, non-yielding
 * loop on the main thread — a future V8 stack-format pathology, a runaway
 * regex, an accidental `while (true)` — wedges the event loop, and from JS you
 * cannot interrupt it: timers, signal handlers, and the PPID watchdog all run
 * *on* that blocked loop, so the process pins a core forever with no
 * self-recovery (the exact unrecoverable state #850 reported).
 *
 * **Why a separate PROCESS, not a worker thread.** A worker thread was the
 * obvious first choice and it works in a toy process — but it was validated to
 * FAIL in the real daemon (#850 live test). V8 isolates in one process
 * coordinate on global safepoints, so when one thread requests a GC every other
 * thread must reach a safepoint before it can proceed. A main thread wedged in
 * a tight, non-allocating loop never reaches one, which strands the watchdog
 * worker on its very next allocation/safepoint check — and the #850 hot loop
 * (`SourcePositionTableIterator::Advance`, a non-allocating C++ table walk) is
 * exactly that shape. A child process shares no isolate and no heap with the
 * parent, so the wedge cannot touch it; it kills via the kernel, which honours
 * SIGKILL regardless of what the parent's threads are doing.
 *
 * **How.** The parent writes a heartbeat byte to the child's stdin every
 * `checkMs` from a timer — firing at all means the event loop is turning. The
 * child resets a kill-timer on each byte; if none arrives for `timeoutMs` it
 * `SIGKILL`s the parent so a fresh daemon starts on the next connection. When
 * the parent exits normally the pipe closes and the child exits too (no
 * orphan).
 *
 * **Won't fire on real work.** Heavy parsing runs in the parse worker
 * (off-thread) and the daemon's indexing shells out to a child process, so the
 * daemon's main thread only ever does fast, bounded work. The default timeout
 * is ~300× the 5h #850 wedge shorter, yet far longer than any legitimate
 * main-thread block. Opt out with `CODEGRAPH_NO_WATCHDOG=1`; tune with
 * `CODEGRAPH_WATCHDOG_TIMEOUT_MS`.
 *
 * **Disk-progress deferral (`progressPaths`).** The CLI `index`/`init` path is
 * different: it runs the SQLite store on this thread, and one long synchronous
 * statement on severely degraded storage can block the loop past the timeout
 * with the process perfectly healthy (#1231: killed a valid index on a
 * 150-IOPS disk). Heartbeat silence alone cannot tell that apart from a wedge —
 * but the disk can: a wedged CPU loop makes no forward progress on the DB
 * files, while a slow store advances them. When the caller supplies
 * `progressPaths` (the SQLite DB + `-wal`), the child checks them at each
 * silent timeout: size/mtime advanced ⇒ defer the kill and keep watching;
 * unchanged ⇒ kill as before. Deferral is bounded by a hard cap
 * (`PROGRESS_CAP_MULTIPLIER` × timeout) of continuous silence, so a wedge
 * coinciding with unrelated file activity — or I/O hung beyond all reason —
 * still dies. A true wedge with no disk progress dies at the base timeout,
 * exactly as before.
 */
import * as fs from 'fs';
import * as os from 'os';
import { spawn, ChildProcess } from 'child_process';

/** Default: 60s — ~300× shorter than the 5h #850 wedge, far longer than any real main-thread block. */
export const DEFAULT_WATCHDOG_TIMEOUT_MS = 60_000;

/**
 * Hard cap on disk-progress deferral: after this many timeouts' worth of
 * CONTINUOUS heartbeat silence the process is killed even if the watched files
 * keep advancing (a wedge coinciding with unrelated file writes, or I/O hung
 * beyond any legitimate statement). 10× the 60s default ⇒ 10 minutes.
 */
export const PROGRESS_CAP_MULTIPLIER = 10;

/** `true` for `1/true/yes/on` (case-insensitive); `false` otherwise. */
function isEnvTruthy(raw: string | undefined): boolean {
  if (!raw) return false;
  return ['1', 'true', 'yes', 'on'].includes(raw.trim().toLowerCase());
}

/**
 * The longest delay a Node timer holds: anything above 2^31-1 ms is run after
 * 1 ms instead (with a TimeoutOverflowWarning), so a huge value meant as
 * "effectively never" would fire at once (#1966).
 */
const MAX_TIMER_DELAY_MS = 2_147_483_647;

/**
 * Parse the timeout env, falling back to the default for missing/invalid values
 * and capping at {@link MAX_TIMER_DELAY_MS}.
 */
export function parseWatchdogTimeoutMs(
  raw: string | undefined,
  fallback: number = DEFAULT_WATCHDOG_TIMEOUT_MS
): number {
  if (raw === undefined) return fallback;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.min(n, MAX_TIMER_DELAY_MS) : fallback;
}

/** Derive a heartbeat cadence that emits several beats inside the timeout window. */
export function deriveCheckIntervalMs(timeoutMs: number): number {
  return Math.min(2000, Math.max(50, Math.round(timeoutMs / 5)));
}

/** Arming/teardown diagnostics, gated on the existing MCP debug switch. */
function debug(msg: string): void {
  if (process.env.CODEGRAPH_MCP_DEBUG) {
    try { fs.writeSync(2, `[CodeGraph watchdog] ${msg}\n`); } catch { /* ignore */ }
  }
}

export interface WatchdogHandle {
  /** Stop heartbeating and shut the watchdog child down. Idempotent. */
  stop(): void;
}

/**
 * The watchdog child body, run via `node -e`. Inlined as a string (not a
 * shipped `.js`) so there is no dist-vs-src path to resolve — it runs
 * identically under `tsx` in tests and under the bundle in production. Reads its
 * target pid + timeout from argv; an MSG built once at startup (the child is
 * never wedged, so allocation here is fine).
 */
const CHILD_SOURCE = `
const fs = require('fs');
const parentPid = Number(process.argv[1]);
const timeoutMs = Number(process.argv[2]);
const capMs = Number(process.argv[3]);
const progressPaths = process.argv.slice(4);
const secs = Math.round(timeoutMs / 1000);
function kill(extra) {
  // Timestamped so daemon.log kills can be correlated with anything (#1431) —
  // computed here at kill time; this child process is never the wedged one.
  try { fs.writeSync(2, Buffer.from('[' + new Date().toISOString() + '] [CodeGraph] Main thread unresponsive for ~' + secs + 's' + (extra || '') + ' — killing the wedged process so a fresh one can start (#850). Disable with CODEGRAPH_NO_WATCHDOG=1.\\n')); } catch (e) {}
  try { process.kill(parentPid, 'SIGKILL'); } catch (e) {}
  process.exit(0);
}
// Fingerprint of the watched files (size + mtime). A change between checks is
// forward disk progress — a slow synchronous SQLite statement, not a wedge.
function snap() {
  let s = '';
  for (const p of progressPaths) {
    try { const st = fs.statSync(p); s += st.size + ':' + st.mtimeMs + ';'; } catch (e) { s += 'x;'; }
  }
  return s;
}
let lastSnap = progressPaths.length ? snap() : '';
let lastSnapAt = Date.now();
let silentSince = null; // start of the current continuous-silence episode
function onTimeout() {
  if (!progressPaths.length) return kill('');
  const now = Date.now();
  if (silentSince === null) silentSince = now - timeoutMs; // silence began ~one timeout ago
  const cur = snap();
  if (cur !== lastSnap && now - silentSince < capMs) {
    // The event loop is blocked but the DB files are advancing: a legitimate
    // long store on slow storage. Defer, re-baseline, keep watching.
    lastSnap = cur;
    timer = setTimeout(onTimeout, timeoutMs);
    return;
  }
  kill(cur !== lastSnap ? ' despite ongoing disk activity (hard cap ' + Math.round(capMs / 1000) + 's reached)' : '');
}
let timer = setTimeout(onTimeout, timeoutMs);
process.stdin.on('data', () => {
  silentSince = null;
  // Keep the baseline fresh while healthy (throttled — a stat per second).
  if (progressPaths.length) {
    const t = Date.now();
    if (t - lastSnapAt >= 1000) { lastSnap = snap(); lastSnapAt = t; }
  }
  clearTimeout(timer); timer = setTimeout(onTimeout, timeoutMs);
});
process.stdin.on('end', () => process.exit(0));   // parent closed the pipe (exited) -> no orphan
process.stdin.on('error', () => process.exit(0)); // pipe broke -> parent gone
process.stdin.resume();
`;

export interface WatchdogOptions {
  /**
   * Files whose size/mtime advancing counts as forward progress (the SQLite
   * DB + `-wal` for an in-process indexer). With paths supplied, a silent
   * timeout only kills when the files did NOT advance — see the header. Omit
   * for pure heartbeat behavior (the daemon, whose main thread never runs
   * long synchronous work).
   */
  progressPaths?: string[];
}

/**
 * The refresh launcher sets this to its own pid and opens a pipe on
 * {@link LAUNCHER_LIVENESS_FD} for each server it starts. That server sends its
 * heartbeat there instead of starting a watchdog process, and the launcher (a
 * separate process, so the wedge cannot reach it either) kills it on silence and
 * starts a fresh one. A process with any other parent that inherits the variable
 * (a daemon, a CLI command) starts its own watchdog process as before.
 */
export const LAUNCHER_LIVENESS_ENV = 'CODEGRAPH_LAUNCHER_LIVENESS';
export const LAUNCHER_LIVENESS_FD = 3;

/**
 * Lines on the launcher pipe: this object as JSON arms the watch, an empty line
 * is a heartbeat, and `disarm` ends the watch.
 */
interface LivenessArm {
  timeoutMs: number;
  capMs: number;
  progressPaths: string[];
}

/** Size + mtime of each watched file; a change is forward disk progress. */
function progressSnapshot(paths: string[]): string {
  let s = '';
  for (const p of paths) {
    try { const st = fs.statSync(p); s += st.size + ':' + st.mtimeMs + ';'; } catch { s += 'x;'; }
  }
  return s;
}

/**
 * The launcher's side of the watch: the same timeout, disk-progress deferral and
 * hard cap as the watchdog process ({@link CHILD_SOURCE}), driven by lines from
 * the server's pipe. `kill` gets the notice to log and must end the server.
 */
export class LivenessMonitor {
  private armed: LivenessArm | null = null;
  private timer: NodeJS.Timeout | null = null;
  private lastSnap = '';
  private lastSnapAt = 0;
  private silentSince: number | null = null;

  constructor(private readonly kill: (notice: string) => void) {}

  receive(line: string): void {
    if (line === '') return this.beat();
    if (line === 'disarm') return this.disarm();
    let arm: Partial<LivenessArm>;
    try { arm = JSON.parse(line) as Partial<LivenessArm>; } catch { return; }
    if (!(Number(arm.timeoutMs) > 0) || !(Number(arm.capMs) > 0)) return;
    const progressPaths = Array.isArray(arm.progressPaths) ? arm.progressPaths.filter((p) => typeof p === 'string') : [];
    this.armed = { timeoutMs: Math.min(Number(arm.timeoutMs), MAX_TIMER_DELAY_MS), capMs: Number(arm.capMs), progressPaths };
    this.lastSnap = progressSnapshot(progressPaths);
    this.lastSnapAt = Date.now();
    this.silentSince = null;
    this.schedule();
  }

  disarm(): void {
    this.armed = null;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  private beat(): void {
    if (!this.armed) return;
    this.silentSince = null;
    // Keep the baseline fresh while healthy (throttled: a stat per second).
    if (this.armed.progressPaths.length && Date.now() - this.lastSnapAt >= 1000) {
      this.lastSnap = progressSnapshot(this.armed.progressPaths);
      this.lastSnapAt = Date.now();
    }
    this.schedule();
  }

  private schedule(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => this.onTimeout(), this.armed!.timeoutMs);
    this.timer.unref?.();
  }

  private onTimeout(): void {
    const armed = this.armed;
    if (!armed) return;
    const notice = (extra: string): string =>
      `Main thread unresponsive for ~${Math.round(armed.timeoutMs / 1000)}s${extra}; killing the wedged process so a fresh one can start (#850). Disable with CODEGRAPH_NO_WATCHDOG=1.`;
    if (!armed.progressPaths.length) {
      this.disarm();
      return this.kill(notice(''));
    }
    const now = Date.now();
    if (this.silentSince === null) this.silentSince = now - armed.timeoutMs;
    const cur = progressSnapshot(armed.progressPaths);
    if (cur !== this.lastSnap && now - this.silentSince < armed.capMs) {
      // Blocked, but the DB files advance: a long store on slow storage.
      this.lastSnap = cur;
      this.schedule();
      return;
    }
    this.disarm();
    this.kill(notice(cur !== this.lastSnap ? ` despite ongoing disk activity (hard cap ${Math.round(armed.capMs / 1000)}s reached)` : ''));
  }
}

/**
 * Send heartbeats to the refresh launcher that started this process, or return
 * null when there is none. Writes go through `fs.write` on the descriptor: a
 * `net.Socket` opened on it delivers nothing under Bun 1.4.
 */
function launcherWatchdog(arm: LivenessArm, checkMs: number): WatchdogHandle | null {
  if (process.platform === 'win32' || process.env[LAUNCHER_LIVENESS_ENV] !== String(process.ppid)) return null;
  try {
    const st = fs.fstatSync(LAUNCHER_LIVENESS_FD);
    if (!st.isSocket() && !st.isFIFO()) return null;
  } catch {
    return null;
  }
  // One write at a time keeps the lines in order. A heartbeat is dropped while
  // another line is still being written; that line already shows the loop turns.
  const queue: string[] = [];
  let writing = false;
  let broken = false;
  const send = (line: string, droppable = false): void => {
    if (broken || (droppable && (writing || queue.length))) return;
    queue.push(line + '\n');
    if (!writing) flush();
  };
  const flush = (): void => {
    const next = queue.shift();
    if (next === undefined) { writing = false; return; }
    writing = true;
    fs.write(LAUNCHER_LIVENESS_FD, next, (err) => {
      // The launcher can no longer watch this process (or is gone). Exit so a
      // launcher that is still running replaces it, rather than leave a wedge unwatched.
      if (err && err.code !== 'EAGAIN') {
        broken = true;
        queue.length = 0;
        writing = false;
        process.kill(process.pid, 'SIGKILL');
        return;
      }
      flush();
    });
  };
  send(JSON.stringify(arm));
  const heartbeat = setInterval(() => send('', true), checkMs);
  heartbeat.unref();
  debug(`armed through launcher ${process.ppid}: timeoutMs=${arm.timeoutMs} checkMs=${checkMs} progressPaths=${arm.progressPaths.length}`);
  let stopped = false;
  return {
    stop(): void {
      if (stopped) return;
      stopped = true;
      clearInterval(heartbeat);
      send('disarm');
    },
  };
}

/**
 * Install the main-thread liveness watchdog for a long-lived process. Returns a
 * handle to stop it, or `null` when disabled or when the child can't be spawned
 * (degraded, never throws — a missing watchdog must never keep a process from
 * starting).
 */
export function installMainThreadWatchdog(options: WatchdogOptions = {}): WatchdogHandle | null {
  if (isEnvTruthy(process.env.CODEGRAPH_NO_WATCHDOG)) return null;

  const timeoutMs = parseWatchdogTimeoutMs(process.env.CODEGRAPH_WATCHDOG_TIMEOUT_MS);
  const checkMs = deriveCheckIntervalMs(timeoutMs);
  const capMs = timeoutMs * PROGRESS_CAP_MULTIPLIER;
  const progressPaths = options.progressPaths ?? [];

  const viaLauncher = launcherWatchdog({ timeoutMs, capMs, progressPaths }, checkMs);
  if (viaLauncher) return viaLauncher;

  let child: ChildProcess;
  try {
    // No execArgv inheritance (unlike Worker), so the child carries none of our
    // V8 flags — it runs no WASM and needs none. stderr inherits the parent's
    // fd 2 so the kill notice lands wherever the parent logs (daemon.log).
    child = spawn(
      process.execPath,
      ['-e', CHILD_SOURCE, String(process.pid), String(timeoutMs), String(capMs), ...progressPaths],
      {
        stdio: ['pipe', 'ignore', 'inherit'],
        windowsHide: true,
        // The watchdog touches no files; keep its cwd off the project/temp dir
        // so it can't hold one open (Windows EPERM-on-cleanup, mirrors the
        // parse-worker quirk).
        cwd: os.tmpdir(),
      }
    );
  } catch (err) {
    debug(`spawn failed: ${err instanceof Error ? err.message : String(err)}`);
    return null;
  }

  const stdin = child.stdin;
  if (!stdin) {
    debug('child has no stdin pipe; not arming');
    try { child.kill(); } catch { /* ignore */ }
    return null;
  }
  // Writing after the child exits surfaces EPIPE on the stream — swallow it so
  // it can't escalate to the global handler (which now exits, #850).
  stdin.on('error', () => { /* child gone; heartbeat writes are best-effort */ });
  child.on('error', (err) => debug(`child error: ${err.message}`));

  // Heartbeat: a byte per tick. When the main thread wedges, these stop and the
  // child's timeout fires. unref'd so it never keeps the process alive itself.
  const heartbeat = setInterval(() => {
    try { stdin.write('\n'); } catch { /* child gone */ }
  }, checkMs);
  heartbeat.unref();

  // Neither the child nor its pipe should keep the parent alive past its work.
  child.unref();
  try { (stdin as unknown as { unref?: () => void }).unref?.(); } catch { /* ignore */ }

  debug(`armed (child pid ${child.pid ?? '?'}): timeoutMs=${timeoutMs} checkMs=${checkMs} progressPaths=${progressPaths.length}`);

  let stopped = false;
  return {
    stop(): void {
      if (stopped) return;
      stopped = true;
      clearInterval(heartbeat);
      try { stdin.end(); } catch { /* ignore */ } // EOF -> child exits cleanly
      try { child.kill(); } catch { /* ignore */ } // belt-and-suspenders
    },
  };
}
