/**
 * A query worker the pool retires closes what it holds, collects garbage, then
 * exits — in that order (#3126, #3060's teardown rule).
 *
 * On Windows a worker thread that ends while V8's concurrent marker is marking
 * its heap can crash the whole process (see src/worker-teardown.ts), so the
 * full collection has to land before the exit. The pool half of the contract
 * lives in query-pool.test.ts; this file pins the worker's own order and that a
 * real worker answers `close` by exiting 0 rather than sitting until the pool
 * terminates it.
 */
import { describe, expect, it } from 'vitest';
import * as path from 'path';
import { Worker } from 'worker_threads';
import { closeWorker } from '../src/mcp/query-worker';

describe('a query worker asked to close', () => {
  it('closes its projects, then its graph, collects, and only then exits 0', () => {
    const steps: string[] = [];
    closeWorker({
      closeProjects: () => { steps.push('closeProjects'); },
      closeGraph: () => { steps.push('closeGraph'); },
      collect: () => { steps.push('collect'); return true; },
      exit: (code) => { steps.push(`exit:${code}`); },
    });
    expect(steps).toEqual(['closeProjects', 'closeGraph', 'collect', 'exit:0']);
  });

  it('still collects and exits when a connection was already closed', () => {
    const steps: string[] = [];
    closeWorker({
      closeProjects: () => { throw new Error('already closed'); },
      closeGraph: () => { throw new Error('already closed'); },
      collect: () => { steps.push('collect'); return true; },
      exit: (code) => { steps.push(`exit:${code}`); },
    });
    expect(steps).toEqual(['collect', 'exit:0']);
  });

  it('a real worker exits 0 on close instead of waiting to be terminated', async () => {
    const worker = new Worker(path.resolve(__dirname, '../dist/mcp/query-worker.js'), {
      workerData: { root: null },
    });
    try {
      await new Promise<void>((resolve, reject) => {
        worker.once('message', () => resolve());
        worker.once('error', reject);
      });
      const exit = new Promise<number>((resolve) => worker.once('exit', (code) => resolve(code)));
      worker.postMessage({ type: 'close' });
      await expect(exit).resolves.toBe(0);
    } finally {
      void worker.terminate();
    }
  }, 30_000);
});
