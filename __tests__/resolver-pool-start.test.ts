/**
 * A run that starts the pool up front waits for it before the first batch: the
 * first batch is the prerequisites, and in C++ the first qualified base class
 * reads every source file for the project's namespace aliases, one synchronous
 * stretch on the thread the liveness watchdog watches (#1091). The wait is
 * bounded (POOL_START_WAIT_MS) and a failure ends it, so a pool that doesn't
 * come up leaves the run on its own thread, as it would be with no pool. A
 * worker that fails to open the index has to fail ready(), or the wait never
 * ends.
 */
import { afterEach, describe, it, expect, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { spawnSync } from 'child_process';
import { POOL_START_WAIT_MS, poolStartSettled } from '../src/resolution/resolver-pool';

const repo = path.resolve(__dirname, '..');
const poolScript = path.join(repo, 'dist/resolution/resolver-pool.js');

afterEach(() => {
  vi.useRealTimers();
});

describe('waiting for a pool start', () => {
  it('returns as soon as the start settles, however it goes', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    await expect(poolStartSettled(Promise.resolve())).resolves.toBeUndefined();
    await expect(poolStartSettled(Promise.reject(new Error('no workers')))).resolves.toBeUndefined();
  });

  it('stops waiting at POOL_START_WAIT_MS for a pool that never comes up', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    let settled = false;
    const waiting = poolStartSettled(new Promise<void>(() => undefined)).then(() => { settled = true; });
    await vi.advanceTimersByTimeAsync(POOL_START_WAIT_MS - 1);
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await waiting;
    expect(settled).toBe(true);
  });
});

describe('a worker that cannot open the index', () => {
  // The compiled worker is absent when running from source before a build.
  it.runIf(fs.existsSync(poolScript) && fs.existsSync(path.join(repo, 'dist/resolution/resolver-worker.js')))(
    'fails its ready() rather than leaving the wait stuck',
    () => {
      const script = `
const { ResolverPool } = require(process.argv[1]);
const pool = ResolverPool.tryCreate(process.argv[2], process.argv[3]);
if (!pool) { console.log('NO_POOL'); process.exit(0); }
const stuck = setTimeout(() => { console.log('STUCK'); process.exit(0); }, 10000);
pool.ready().then(() => console.log('READY'), (err) => console.log('FAILED ' + err.message)).finally(() => {
  clearTimeout(stuck);
  pool.destroy().finally(() => process.exit(0));
});
`;
      const result = spawnSync(
        process.execPath,
        ['--disable-warning=ExperimentalWarning', '-e', script, poolScript, path.join(repo, 'missing.db'), repo],
        { encoding: 'utf8', env: { ...process.env, CODEGRAPH_RESOLVE_WORKERS: '2' }, timeout: 30_000 }
      );
      expect(result.stdout).toMatch(/^FAILED /m);
    },
    60_000
  );
});
