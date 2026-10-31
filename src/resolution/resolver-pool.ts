/**
 * ResolverPool — main-thread client for the parallel-resolution workers.
 *
 * resolveBatch() splits a rowid-ordered batch into ordered chunks, fans the
 * chunks across the pool, and reassembles the results IN CHUNK ORDER, so the
 * caller's admission (edge inserts, row cleanup, failure parking, deferred
 * post-pass queues) is byte-for-byte the sequence the single-threaded loop
 * would have produced. Any worker failure fails the batch — the caller falls
 * back to the sequential path. Kill switch: CODEGRAPH_NO_PARALLEL_RESOLVE=1.
 */

import { Worker } from 'worker_threads';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import type { Edge, UnresolvedReference } from '../types';
import type { NearDupBody, NearDupSignature } from '../graph/near-duplicates';
import type { KernelResolveStats, ResolvedRef, UnresolvedRef } from './types';
import { memoryBudgetBytes } from './memory-budget';
import { terminateOnceStarted, workerStarted } from '../worker-teardown';

/** One synthesis pass's output: its edge list + worker-measured wall clock. */
export interface SynthPassResult {
  edges: Edge[];
  ms: number;
  /** Content-only skips the pass recorded, as [pass, path, content hash]. */
  skips?: Array<[string, string, string]>;
}

export interface ChunkResult {
  resolved: ResolvedRef[];
  unresolved: UnresolvedRef[];
  deferredChain: UnresolvedRef[];
  deferredThisMember: UnresolvedRef[];
  byMethod: Record<string, number>;
  kernel?: KernelResolveStats;
}

interface PoolWorker {
  worker: Worker;
  ready: Promise<void>;
  /** Settles on the worker's first message or its end — see worker-teardown.ts. */
  started: Promise<void>;
  busy: number;
}

/** How long destroy() waits for a worker to exit on 'close' before terminating it. */
const CLOSE_TIMEOUT_MS = 5000;

// 250 refs per chunk — measured on pretix's vendor-JS ref cluster: dense,
// expensive refs sit contiguously in rowid order, so at 500 a single chunk
// held ~2.3s of work and owned the batch tail; at 250 the cluster spreads
// ~2x finer across the steal queue (7.9s→7.1s index wall). 125 bought ~1%
// more for +300MB RSS.
const CHUNK_SIZE = 250;

/**
 * Minimum TOTAL pending refs before the pool is created at all. Pool boot
 * (module load + worker start + caches) costs ~90ms and real CPU, so it pays
 * only when the refs themselves are expensive. gin's 14k Go refs resolve
 * sequentially in 464ms and the pool takes its 1.17s index to 0.88s; an 8k
 * cheap C fixture resolves in 71ms and the pool lost 49ms on it. 12k keeps
 * the Go win and stays above the measured cheap-resolution counterexample.
 * Override: CODEGRAPH_PARALLEL_RESOLVE_MIN=<refs> (0 forces the pool on).
 */
export function minRefsForPool(): number {
  const raw = process.env.CODEGRAPH_PARALLEL_RESOLVE_MIN;
  if (raw !== undefined) {
    const parsed = Number.parseInt(raw, 10);
    if (Number.isFinite(parsed) && parsed >= 0) return parsed;
  }
  return 12_000;
}

/** Minimum refs before the loop drops and rebuilds the non-unique edge and
 *  unresolved-ref indexes over the whole existing graph. That window is a
 *  full-table scan on a large index, so it stays at the measured 150k floor;
 *  the pool threshold above is separate. */
export function minRefsForBulkLoad(): number {
  return 150_000;
}

/**
 * Fewest refs still to resolve for the batch loop to boot the pool mid-run
 * from its observed per-ref rate. Booting costs worker CPU and memory (the
 * floor above), so a small project stays on the main-thread kernel.
 */
export const ADAPTIVE_ENGAGE_MIN_REFS = 20_000;

/** Projected main-thread kernel time for the remaining refs that justifies booting the pool. */
export const ADAPTIVE_ENGAGE_RESOLVE_MS = 400;

/**
 * Whether the resolve loop should boot the pool mid-run. The fixed ref-count
 * gate (minRefsForPool) cannot see per-ref cost; this projects the remaining
 * main-thread kernel time from the rate the loop has measured so far.
 */
export function shouldEngageAdaptively(projectedMs: number, remainingRefs: number): boolean {
  return remainingRefs >= ADAPTIVE_ENGAGE_MIN_REFS && projectedMs >= ADAPTIVE_ENGAGE_RESOLVE_MS;
}

/**
 * The mid-run start is the default policy only. An explicit
 * CODEGRAPH_PARALLEL_RESOLVE_MIN is the user's own pool threshold (a higher
 * value keeps the pool off below it, 0 forces it on), so it turns the
 * projection off.
 */
export function adaptiveEngageEnabled(): boolean {
  return process.env.CODEGRAPH_PARALLEL_RESOLVE_MIN === undefined;
}

/** The compiled worker entry. In production it is the sibling tsc emits; under
 *  Vitest, which loads src/ while the global setup builds dist/, use that copy
 *  so pool-mode tests exercise the pool instead of silently declining it. */
function resolverWorkerScript(): string | null {
  const sibling = path.join(__dirname, 'resolver-worker.js');
  if (fs.existsSync(sibling)) return sibling;
  const built = path.resolve(__dirname, '../../dist/resolution/resolver-worker.js');
  return fs.existsSync(built) ? built : null;
}

export class ResolverPool {
  private static poolSeq = 0;
  private workers: PoolWorker[] = [];
  private nextId = 0;
  private waiters = new Map<number, { resolve: (r: ChunkResult) => void; reject: (e: Error) => void }>();
  private synthWaiters = new Map<number, { resolve: (r: SynthPassResult) => void; reject: (e: Error) => void }>();
  private signWaiters = new Map<number, { resolve: (r: NearDupSignature[]) => void; reject: (e: Error) => void }>();
  private recycleWaiters = new Map<number, { arm: () => void; done: () => void }>();
  // Dispatch hold for the WAL backpressure window: in-flight chunks finish
  // and release their read marks, no new ones start, so the valve's
  // checkpoint isn't pinned by continuous pool reads.
  private paused = false;
  private failed: Error | null = null;
  private opened = false;

  /**
   * Pool size from CPU headroom, memory headroom, and the explicit override.
   * Pure — every input injected — so the whole matrix is unit-testable.
   *
   * CPU term: `availableParallelism` (cpuset/affinity-honest — `os.cpus()`
   * enumerates the host's CPUs and sized SIX workers inside a 2-CPU cpuset,
   * §7a.1's false-premise finding), minus one for the persisting main thread,
   * floored at 2 so a true 2-core box keeps the pool's ~2× on synthesis,
   * capped at 12 — measured the sweet spot on a 24-vCPU host (2026-11):
   * 12→16 workers bought ~0% on the supabase corpus for +30% peak RSS.
   *
   * Memory term: workers hold real heap at scale (~1GB each against a 4.6GB
   * kernel-scale DB — six of them OOM-killed a 7GB container once real
   * 8-core concurrency let them peak simultaneously). Estimate per-worker
   * cost from the DB size, keep 30% of the budget for the main thread, and
   * let the smaller term win. Below 2 workers the pool isn't worth its boot
   * cost — callers get null and stay sequential.
   */
  static resolvePoolSize(opts: {
    explicit?: string;
    availableParallelism: number;
    memoryBudget: number;
    dbSizeBytes: number;
    pendingRefs?: number;
  }): number | null {
    if (opts.explicit !== undefined && opts.explicit !== '') {
      const n = Number.parseInt(opts.explicit, 10);
      if (Number.isFinite(n)) {
        if (n <= 0) return null;
        return Math.min(n, 16);
      }
    }
    // No floor: at ap=2 the pool LOSES to sequential outright — measured on
    // the kernel-scale 2-cpuset envelope: resolution 853s sequential vs
    // 1,150s pooled-6-on-2 (§7a.1), and synthesis is Amdahl-bound by its
    // dominant pass (cFnPtrEdges 306s of 358s) so pooling it bought nothing.
    // ap−1 < 2 ⇒ sequential is the fast path, not a fallback.
    const cpuCap = Math.min(opts.availableParallelism - 1, 12);
    const perWorker = Math.min(Math.max(opts.dbSizeBytes * 0.2, 256 * 1024 * 1024), 1.5 * 1024 * 1024 * 1024);
    const memCap = Math.floor((opts.memoryBudget * 0.7) / perWorker);
    // Past ~25k pending refs per worker the extra workers stop paying for
    // themselves: on a 207k-ref corpus 8 of 12 matched the wall time while
    // each idle worker still carried ~180MB of isolate+kernel overhead, and
    // only a deep queue (1.85M refs) made the 12th worker earn it (−13%
    // resolve wall there). Size to the work, not just the machine.
    // Small runs get a worker per ~2k refs: gin's 14k refs resolve
    // sequentially in 464ms against a ~90ms pool boot, and 7-8 workers bring
    // a 1.17s index to 0.88s. The cap rises smoothly with the work (refs/12)
    // up to the measured ~25k refs per worker, so 100k refs are not a cliff.
    const perWorkerRefs = opts.pendingRefs === undefined
      ? 25_000
      : Math.min(25_000, Math.max(2_000, Math.ceil(opts.pendingRefs / 12)));
    const workCap =
      opts.pendingRefs === undefined ? cpuCap : Math.max(2, Math.ceil(opts.pendingRefs / perWorkerRefs));
    const size = Math.min(cpuCap, memCap, workCap);
    return size >= 2 ? size : null;
  }

  /**
   * Create a pool when the compiled worker exists (absent when running from
   * source in tests → callers use the sequential path), the kill switch is
   * off, and the machine has the cores AND memory to carry it. Returns null
   * otherwise. `CODEGRAPH_RESOLVE_WORKERS` overrides the computed size
   * (0 disables the pool; values are capped at 16).
   */
  /**
   * The decline checks of {@link tryCreate} without construction — kill
   * switch, compiled worker presence, and host sizing. Callers that pay a
   * one-time setup cost for the pool (the kernel-reader snapshot copy in
   * resolveAndPersistBatched) can preflight before spending it.
   */
  static preflight(dbPath: string, pendingRefs?: number): number | null {
    if (process.env.CODEGRAPH_NO_PARALLEL_RESOLVE === '1') return null;
    if (resolverWorkerScript() === null) return null;
    let dbSizeBytes = 0;
    try {
      dbSizeBytes = fs.statSync(dbPath).size;
    } catch { /* fresh/missing file — the 256MB per-worker floor applies */ }
    const ap = os.availableParallelism();
    const budget = memoryBudgetBytes();
    const size = ResolverPool.resolvePoolSize({
      explicit: process.env.CODEGRAPH_RESOLVE_WORKERS,
      availableParallelism: ap,
      memoryBudget: budget,
      dbSizeBytes,
      pendingRefs,
    });
    // Both outcomes log under SYNTH_TIMINGS — a silent null is how §7a.1's
    // diagnostic run hid the memory-term misfire for a whole 25-minute cycle.
    if (process.env.CODEGRAPH_SYNTH_TIMINGS) {
      console.error(
        `[pool-timing] pool ${size === null ? 'disabled' : `size=${size}`} (ap=${ap} budget=${Math.round(budget / 1024 / 1024)}MB db=${Math.round(dbSizeBytes / 1024 / 1024)}MB)`
      );
    }
    return size;
  }

  /** Spawn the workers without opening them. Modules load while the caller
   *  (re)builds the kernel snapshot copy, so the ~60ms module load hides
   *  under that work instead of adding to pool boot. Call {@link open} once
   *  the snapshot path is known; {@link destroy} is safe in between. */
  static spawn(size: number): ResolverPool | null {
    const workerScript = resolverWorkerScript();
    if (workerScript === null) return null;
    try {
      const pool = new ResolverPool(workerScript, size);
      // A worker can fail its boot before openPool observes readiness. Without
      // a handler here that rejection is unhandled, and Node terminates the
      // indexing process before the main-thread fallback can run. The later
      // ready() await still sees the rejection and opens the fallback path.
      void pool.ready().catch(() => undefined);
      return pool;
    } catch {
      return null;
    }
  }

  static tryCreate(
    dbPath: string,
    projectRoot: string,
    kernelDbPath: string | null = null,
    supertypesComplete = false,
    pendingRefs?: number,
  ): ResolverPool | null {
    const size = ResolverPool.preflight(dbPath, pendingRefs);
    if (size === null) return null;
    const pool = ResolverPool.spawn(size);
    if (pool === null) return null;
    pool.open(dbPath, projectRoot, kernelDbPath, supertypesComplete);
    return pool;
  }

  /** Send the open message to the workers spawned by {@link spawn}. */
  open(
    dbPath: string,
    projectRoot: string,
    kernelDbPath: string | null,
    supertypesComplete: boolean,
  ): void {
    if (this.opened) return;
    this.opened = true;
    // One token per pool: the workers' kernel resolvers key their shared
    // node table on it, so a later pool over a rewritten snapshot at the
    // same path can never pick up this run's table.
    const kernelGeneration = kernelDbPath ? `${process.pid}-${Date.now().toString(36)}-${++ResolverPool.poolSeq}` : undefined;
    for (const pw of this.workers) {
      pw.worker.postMessage({ type: 'open', dbPath, projectRoot, kernelDbPath, kernelGeneration, supertypesComplete });
    }
  }

  private constructor(workerScript: string, size: number) {
    for (let i = 0; i < size; i++) {
      const worker = new Worker(workerScript, { workerData: { spawnedAt: Date.now() } });
      const started = workerStarted(worker);
      let readyResolve!: () => void;
      let readyReject!: (e: Error) => void;
      const ready = new Promise<void>((resolve, reject) => {
        readyResolve = resolve;
        readyReject = reject;
      });
      const pw: PoolWorker = { worker, ready, started, busy: 0 };
      worker.on('message', (msg: { type: string; id?: number; message?: string; edges?: Edge[]; ms?: number; skips?: Array<[string, string, string]> } & Partial<ChunkResult>) => {
        if (msg.type === 'ready') {
          readyResolve();
        } else if (msg.type === 'loaded') {
          if (process.env.CODEGRAPH_SYNTH_TIMINGS && (msg as { loadedMs?: number }).loadedMs !== undefined) {
            console.error(`[pool-timing] worker modules loaded after ${(msg as { loadedMs?: number }).loadedMs}ms`);
          }
        } else if (msg.type === 'result' && msg.id !== undefined) {
          pw.busy--;
          const waiter = this.waiters.get(msg.id);
          this.waiters.delete(msg.id);
          this.pumpDispatch();
          waiter?.resolve({
            resolved: msg.resolved!,
            unresolved: msg.unresolved!,
            deferredChain: msg.deferredChain!,
            deferredThisMember: msg.deferredThisMember!,
            byMethod: msg.byMethod!,
            kernel: msg.kernel,
          });
        } else if (msg.type === 'synth-result' && msg.id !== undefined) {
          pw.busy--;
          const waiter = this.synthWaiters.get(msg.id);
          this.synthWaiters.delete(msg.id);
          waiter?.resolve({ edges: msg.edges ?? [], ms: msg.ms ?? 0, skips: msg.skips });
        } else if (msg.type === 'near-dup-signed' && msg.id !== undefined) {
          pw.busy--;
          const waiter = this.signWaiters.get(msg.id);
          this.signWaiters.delete(msg.id);
          waiter?.resolve((msg as { results?: NearDupSignature[] }).results ?? []);
        } else if (msg.type === 'recycling' && msg.id !== undefined) {
          this.recycleWaiters.get(msg.id)?.arm();
        } else if (msg.type === 'recycled' && msg.id !== undefined) {
          const waiter = this.recycleWaiters.get(msg.id);
          this.recycleWaiters.delete(msg.id);
          waiter?.done();
        } else if (msg.type === 'error') {
          pw.busy--;
          this.pumpDispatch();
          const err = new Error(`resolver worker: ${msg.message}`);
          if (msg.id !== undefined && this.waiters.has(msg.id)) {
            const waiter = this.waiters.get(msg.id)!;
            this.waiters.delete(msg.id);
            waiter.reject(err);
          } else if (msg.id !== undefined && this.synthWaiters.has(msg.id)) {
            const waiter = this.synthWaiters.get(msg.id)!;
            this.synthWaiters.delete(msg.id);
            waiter.reject(err);
          } else if (msg.id !== undefined && this.signWaiters.has(msg.id)) {
            const waiter = this.signWaiters.get(msg.id)!;
            this.signWaiters.delete(msg.id);
            waiter.reject(err);
          } else {
            this.fail(err);
          }
        }
      });
      worker.on('error', (err) => {
        this.fail(err instanceof Error ? err : new Error(String(err)));
        readyReject(this.failed!);
      });
      worker.on('exit', (code) => {
        if (code !== 0) {
          this.fail(new Error(`resolver worker exited with code ${code}`));
          readyReject(this.failed!);
        }
      });
      this.workers.push(pw);
    }
  }

  private fail(err: Error): void {
    if (!this.failed) this.failed = err;
    for (const [, waiter] of this.waiters) waiter.reject(this.failed);
    this.waiters.clear();
    for (const item of this.pendingChunks) item.reject(this.failed);
    this.pendingChunks = [];
    for (const [, waiter] of this.synthWaiters) waiter.reject(this.failed);
    this.synthWaiters.clear();
    for (const [, waiter] of this.signWaiters) waiter.reject(this.failed);
    this.signWaiters.clear();
    // Pending recycles resolve rather than reject: their per-call timeout
    // owns rejection, and the recycle caller checks this.failed next round.
    for (const [, w] of this.recycleWaiters) w.done();
    this.recycleWaiters.clear();
  }

  /**
   * Stop the pump from posting new chunks (WAL backpressure window). Chunks
   * already posted finish normally; queued chunks wait for resumeDispatch.
   */
  pauseDispatch(): void {
    this.paused = true;
  }

  resumeDispatch(): void {
    this.paused = false;
    this.pumpDispatch();
  }

  /** Number of workers — how many passes the synthesis fan-out keeps in flight. */
  get size(): number {
    return this.workers.length;
  }

  async ready(): Promise<void> {
    await Promise.all(this.workers.map((w) => w.ready));
  }

  /**
   * Chunks queued but not yet posted, shared across in-flight batches: the
   * next batch fans out while the previous one is still resolving, and the
   * workers pull from both in post order. Each entry carries its batch's
   * reassembly slot.
   */
  private pendingChunks: Array<{
    id: number;
    refs: UnresolvedReference[];
    resolve: (r: ChunkResult) => void;
    reject: (e: Error) => void;
  }> = [];

  /** Post queued chunks while any worker is under its in-flight bound —
   *  real work-stealing: a worker that drew a slow chunk traps only the few
   *  queued chunks behind it instead of the deep pre-assigned queues the old
   *  post-everything-up-front dispatch stacked up (~4s of parked work at a
   *  12-worker, 500-ref-chunk scale on pretix's vendor-JS cluster). Called
   *  from resolveBatch and after every result/error ack. */
  private pumpDispatch(): void {
    if (this.paused) return;
    while (this.pendingChunks.length > 0) {
      const pw = this.workers.reduce((a, b) => (b.busy < a.busy ? b : a));
      // Depth 4 per worker: 2 starved the pool on ack turnaround (workers
      // idle between a chunk's ack and the pump's refill — settle ~4.1s);
      // 8 re-deepened queues and re-trapped chunks behind stragglers (8.3s).
      if (pw.busy >= 4) return;
      const item = this.pendingChunks.shift()!;
      pw.busy++;
      this.waiters.set(item.id, { resolve: item.resolve, reject: item.reject });
      pw.worker.postMessage({ type: 'resolve', id: item.id, refs: item.refs });
    }
  }

  /**
   * Resolve `refs` across the pool. Chunks preserve input order; the returned
   * arrays are the in-order concatenation of the chunk results.
   */
  async resolveBatch(refs: UnresolvedReference[]): Promise<ChunkResult> {
    if (this.failed) throw this.failed;
    const chunkPromises: Promise<ChunkResult>[] = [];
    for (let i = 0; i < refs.length; i += CHUNK_SIZE) {
      const chunk = refs.slice(i, i + CHUNK_SIZE);
      const id = this.nextId++;
      // Queue, don't blast: pumpDispatch keeps each worker under its
      // in-flight bound and refills on each ack. Result order is fixed by
      // the promise array, not by completion order.
      chunkPromises.push(
        new Promise<ChunkResult>((resolve, reject) => {
          this.pendingChunks.push({ id, refs: chunk, resolve, reject });
        })
      );
    }
    this.pumpDispatch();
    const chunks = await Promise.all(chunkPromises);
    const out: ChunkResult = { resolved: [], unresolved: [], deferredChain: [], deferredThisMember: [], byMethod: {} };
    for (const c of chunks) {
      out.resolved.push(...c.resolved);
      out.unresolved.push(...c.unresolved);
      out.deferredChain.push(...c.deferredChain);
      out.deferredThisMember.push(...c.deferredThisMember);
      for (const [k, v] of Object.entries(c.byMethod)) out.byMethod[k] = (out.byMethod[k] || 0) + v;
      if (c.kernel) {
        out.kernel ??= { handled: 0 };
        out.kernel.handled += c.kernel.handled;
        out.kernel.frameworkMerge = (out.kernel.frameworkMerge ?? 0) + (c.kernel.frameworkMerge ?? 0);
        out.kernel.frameworkMergeWithCands =
          (out.kernel.frameworkMergeWithCands ?? 0) + (c.kernel.frameworkMergeWithCands ?? 0);
      }
    }
    return out;
  }

  /**
   * Run one synthesis pass (by SYNTH_PASSES name) on the least-busy worker.
   * The worker reads the committed graph on its own connection and returns
   * the pass's edge list; the caller merges in canonical order. Rejects on
   * worker failure — the caller retries the pass on the main thread.
   */
  async runSynthPass(passName: string, shard?: { index: number; count: number; from?: number; to?: number }): Promise<SynthPassResult> {
    if (this.failed) throw this.failed;
    const id = this.nextId++;
    const pw = this.workers.reduce((a, b) => (b.busy < a.busy ? b : a));
    pw.busy++;
    return new Promise<SynthPassResult>((resolve, reject) => {
      this.synthWaiters.set(id, { resolve, reject });
      pw.worker.postMessage({ type: 'synth', id, pass: passName, shard });
    });
  }

  /**
   * Sign near-duplicate bodies across the idle pool (the full index calls
   * this after synthesis, before the pool is destroyed). Workers read the
   * bodies from disk and return the MinHash bytes plus the shingle set the
   * pair pass reuses; the main thread writes the rows.
   */
  async signNearDuplicates(bodies: NearDupBody[], projectRoot: string): Promise<NearDupSignature[]> {
    if (this.failed) throw this.failed;
    if (bodies.length === 0) return [];
    const lanes = Math.min(this.workers.length, 8, Math.max(1, Math.ceil(bodies.length / 256)));
    const chunks: NearDupBody[][] = Array.from({ length: lanes }, () => []);
    const per = Math.ceil(bodies.length / lanes);
    for (let i = 0; i < bodies.length; i++) chunks[Math.min(lanes - 1, Math.floor(i / per))]!.push(bodies[i]!);
    const parts = await Promise.all(chunks.filter((c) => c.length > 0).map((chunk) => new Promise<NearDupSignature[]>((resolve, reject) => {
      const id = this.nextId++;
      const pw = this.workers.reduce((a, b) => (b.busy < a.busy ? b : a));
      pw.busy++;
      this.signWaiters.set(id, { resolve, reject });
      pw.worker.postMessage({ type: 'near-dup-sign', id, bodies: chunk, projectRoot });
    })));
    return parts.flat();
  }

  /**
   * Ask every worker to close and reopen its read-only connection, and wait
   * for all acks. MUST be called only at the pool-idle boundary (all fanned
   * chunks settled, next batch not yet dispatched) — the workers close their
   * connections in place. Why: a long-lived reader pins WAL checkpoint
   * progress, and the deep WAL behind it taxes every main-thread B-tree
   * page operation (writes-under-readers, plan §7a.6 — deletes 42.6→118.8s
   * from 0 to 4 attached readers). Releasing the read marks periodically
   * lets the existing checkpoints advance, keeping the WAL shallow WITHOUT
   * the full-park folds an aggressive valve pays (+129s measured at 64MB).
   * A recycle failure fails the pool — the caller's sequential fallback
   * covers the rest of the run.
   */
  async recycleWorkers(kernelDbPath?: string): Promise<void> {
    if (this.failed) throw this.failed;
    // A refreshed kernel snapshot gets its own generation: the shared node
    // table is keyed on it, and the old snapshot's table must not be reused.
    const kernel = kernelDbPath
      ? { dbPath: kernelDbPath, generation: `${process.pid}-${Date.now().toString(36)}-${++ResolverPool.poolSeq}` }
      : undefined;
    await Promise.all(
      this.workers.map(
        (pw) =>
          new Promise<void>((resolve, reject) => {
            const id = this.nextId++;
            // Arm only once the worker dequeues the request and posts its
            // 'recycling' ack — the request can sit behind posted chunks for
            // longer than the timeout, so timing from post would fail a
            // healthy pool. A worker that never dequeues is wedged on a
            // chunk; its failure/exit paths settle the waiter instead.
            let t: NodeJS.Timeout | undefined;
            this.recycleWaiters.set(id, {
              arm: () => {
                t = setTimeout(() => {
                  if (this.recycleWaiters.delete(id)) {
                    const err = new Error('resolver worker recycle timed out');
                    this.fail(err);
                    reject(err);
                  }
                }, 10_000);
              },
              done: () => {
                clearTimeout(t);
                resolve();
              },
            });
            pw.worker.postMessage({ type: 'recycle', id, kernel });
          })
      )
    );
  }

  /**
   * Ask every worker to close; each collects garbage and exits by itself (see
   * worker-teardown.ts). One that hasn't by `closeTimeoutMs` is terminated —
   * but never while it is still starting up: a pool torn down soon after it
   * booted, on a busy machine, can have a worker still loading its modules.
   */
  async destroy(closeTimeoutMs = CLOSE_TIMEOUT_MS): Promise<void> {
    // Abandon pending recycles first: their acks can't arrive once the
    // workers are closing, and the per-recycle timeout would otherwise hold
    // the event loop for up to 10s after teardown. Completing the waiters
    // clears those timers; callers of recycleWorkers see a resolved promise
    // and check this.failed next round, as on a pool failure.
    for (const [, w] of this.recycleWaiters) w.done();
    this.recycleWaiters.clear();
    for (const item of this.pendingChunks) item.reject(new Error('resolver pool destroyed'));
    this.pendingChunks = [];
    await Promise.all(
      this.workers.map(
        (pw) =>
          new Promise<void>((resolve) => {
            const t = setTimeout(() => {
              void terminateOnceStarted(pw.worker, pw.started).then(() => resolve());
            }, closeTimeoutMs);
            pw.worker.once('exit', () => {
              clearTimeout(t);
              resolve();
            });
            pw.worker.postMessage({ type: 'close' });
          })
      )
    );
  }
}
