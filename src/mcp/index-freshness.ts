import { existsSync } from 'fs';
import * as path from 'path';
import { Worker } from 'worker_threads';
import { terminateOnceStarted, workerStarted, WORKER_START_SETTLE_MS } from '../worker-teardown';

export interface PendingChangeCounts {
  added: number;
  modified: number;
  removed: number;
}

/** `getChangedFiles()` may fall back to a huge filesystem scan (#1959). */
const MEASURE_TIMEOUT_MS = 8_000;
let liveMeasurements = 0;
const active = new Map<string, Promise<PendingChangeCounts | null>>();
/** Every measurement worker not yet terminated, with its load — for {@link endFreshnessMeasurements}. */
const live = new Map<Worker, Promise<void>>();

export function measurePendingChanges(root: string, timeoutMs = MEASURE_TIMEOUT_MS): Promise<PendingChangeCounts | null> {
  const key = path.resolve(root);
  const existing = active.get(key);
  if (existing) return existing;
  // Status probes for many projects must not exhaust the shared daemon.
  if (liveMeasurements >= 2) return Promise.resolve(null);

  const pending = runMeasurement(key, timeoutMs).finally(() => active.delete(key));
  active.set(key, pending);
  return pending;
}

/**
 * Terminate every measurement still running, each once it has loaded its
 * modules (worker-teardown.ts) — for a server that is about to exit, since
 * exiting tears the workers down the same way terminating them does.
 *
 * The wait ends by `capMs`. A worker inside a synchronous Git call terminates
 * only when that call returns, which the fallback scan lets run for 30 s; past
 * the cap the server goes on stopping with that worker loaded but not yet gone,
 * rather than outliving the deadline its stopper gives it (`stopDaemonAt()`,
 * the proxy fallback's backstop). A closing tool handler starts no new
 * measurement, so the workers live at the call are all there are.
 */
export async function endFreshnessMeasurements(opts: { capMs?: number } = {}): Promise<void> {
  const capMs = opts.capMs ?? WORKER_START_SETTLE_MS;
  let cap: NodeJS.Timeout | undefined;
  await Promise.race([
    Promise.all([...live].map(([worker, loaded]) => terminateOnceStarted(worker, loaded, capMs))),
    new Promise<void>((resolve) => {
      cap = setTimeout(resolve, capMs);
      cap.unref?.();
    }),
  ]);
  clearTimeout(cap);
}

function runMeasurement(root: string, timeoutMs: number): Promise<PendingChangeCounts | null> {
  // The compiled sibling is beside us in production; Vitest loads src/ but
  // builds dist/ before the tests, so use that copy for the worker there.
  const sibling = path.join(__dirname, 'index-freshness-worker.js');
  const workerFile = existsSync(sibling)
    ? sibling
    : path.resolve(__dirname, '../../dist/mcp/index-freshness-worker.js');
  if (!existsSync(workerFile)) return Promise.resolve(null);

  let worker: Worker;
  try {
    worker = new Worker(workerFile, { workerData: { root } });
  } catch {
    return Promise.resolve(null);
  }

  // Its first message is 'loaded', posted once its modules are in. Terminating
  // it before then can crash the whole process on Windows (worker-teardown.ts).
  const loaded = workerStarted(worker);
  live.set(worker, loaded);
  liveMeasurements++;
  return new Promise(resolve => {
    let settled = false;
    const finish = (counts: PendingChangeCounts | null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      // A worker inside a synchronous Git call may take time to terminate, and
      // one still loading its modules must not be terminated yet. Return
      // unknown on deadline, but keep its concurrency slot until it is gone.
      void terminateOnceStarted(worker, loaded).finally(() => {
        live.delete(worker);
        liveMeasurements--;
      });
      resolve(counts);
    };
    const timer = setTimeout(() => finish(null), timeoutMs);
    worker.on('message', (msg: { type?: string; counts?: PendingChangeCounts | null }) => {
      if (msg?.type !== 'counts') return;
      const value = msg.counts;
      const valid = value && typeof value === 'object' && ['added', 'modified', 'removed'].every(
        key => Number.isSafeInteger(value[key as keyof PendingChangeCounts]) && value[key as keyof PendingChangeCounts] >= 0,
      );
      finish(valid ? value : null);
    });
    worker.once('error', () => finish(null));
    worker.once('exit', () => finish(null));
  });
}
