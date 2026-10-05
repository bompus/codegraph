import { afterEach, describe, expect, it } from 'vitest';
import { spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

const MODULE = path.resolve(__dirname, '../dist/mcp/writer-lock.js');
const ACTOR = path.resolve(__dirname, 'fixtures/writer-lock-ordering.cjs');

describe('writer handover ordering', () => {
  let root: string;
  afterEach(() => {
    if (root) fs.rmSync(root, { recursive: true, force: true });
  });

  function run(operation: string) {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-writer-order-'));
    fs.mkdirSync(path.join(root, '.codegraph'));
    const child = spawnSync(process.execPath, [ACTOR, MODULE, root, operation], { encoding: 'utf8', timeout: 15000 });
    expect(child.status, child.stderr).toBe(0);
    return JSON.parse(child.stdout);
  }

  it.each(['swap', 'ready', 'release', 'acquire'] as const)(
    'refuses a second handover while %s is between its ownership read and mutation',
    (operation) => {
      const outcome = run(operation);
      expect(outcome.paused, `${operation} barrier was not reached`).toBe(true);
      expect(outcome.competitor, `${operation} -> competing handover -> ${operation} mutation`).toEqual({ claimed: false, promoted: false });
      if (operation === 'swap') expect(outcome.info).toMatchObject({ pid: 222, mode: 'daemon' });
      else if (operation === 'ready') expect(outcome.info).toMatchObject({ pid: outcome.pid, ready: true });
      else if (operation === 'release') expect(outcome.info).toBeNull();
      else expect(outcome.info).toMatchObject({ pid: outcome.pid, mode: 'fallback' });
    },
  );

  it('eventually releases a retired writer after coordination contention', () => {
    const outcome = run('retire');
    expect(outcome.released).toBe(true);
    expect(outcome.acquired).toBe(true);
  });

  it('releases the coordination lock when its process exits without cleanup', () => {
    const outcome = run('crash');
    expect(outcome).toEqual({ held: true });
    const child = spawnSync(process.execPath, [ACTOR, MODULE, root, 'reacquire'], { encoding: 'utf8', timeout: 15000 });
    expect(child.status, child.stderr).toBe(0);
    expect(JSON.parse(child.stdout)).toEqual({ acquired: true });
    expect(fs.existsSync(path.join(root, '.codegraph/writer.pid.mutation.lock'))).toBe(true);
  });

  it('keeps release and fallback acquisition from invalidating a pending successor', () => {
    const outcome = run('successor');
    expect(outcome.result).toBe(true);
    expect(outcome.paused).toBe(true);
    expect(outcome.acquired, 'successor read -> launcher release -> fallback acquire -> successor rename').toBe('taken');
    expect(outcome.info).toMatchObject({ pid: 333, mode: 'daemon' });
  });
});
