/**
 * The duration `indexAll` and `sync` report covers the whole run.
 *
 * The orchestrator times extraction only. `codegraph init` printed that
 * figure beside edge totals that include resolution and synthesis, so a
 * 19.9 s index on pretix read "81,229 edges in 1.7s" and a slowdown in
 * resolution never showed in the CLI output.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';

const DELAY_MS = 300;

/** Make `method` on `target` wait DELAY_MS before running. */
function delay(target: object, method: string): void {
  const record = target as Record<string, (...args: unknown[]) => unknown>;
  const original = record[method]!.bind(target);
  record[method] = (...args: unknown[]) => {
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, DELAY_MS);
    return original(...args);
  };
}

describe('index and sync durations', () => {
  let cg: CodeGraph | undefined;
  let dir: string | undefined;

  afterEach(() => {
    cg?.destroy();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
    dir = undefined;
  });

  const open = (): CodeGraph => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-duration-'));
    fs.writeFileSync(path.join(dir, 'a.ts'), 'export function a() { return b(); }\nexport function b() { return 1; }\n');
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    return cg;
  };

  it('indexAll counts the work after extraction', async () => {
    const graph = open();
    delay((graph as unknown as { resolver: object }).resolver, 'resolveDeferredThisMemberRefs');
    const result = await graph.indexAll();
    expect(result.success).toBe(true);
    expect(result.durationMs).toBeGreaterThanOrEqual(DELAY_MS);
  });

  it('sync counts the work after extraction', async () => {
    const graph = open();
    await graph.indexAll();
    fs.appendFileSync(path.join(dir!, 'a.ts'), 'export function c() { return a(); }\n');
    delay((graph as unknown as { resolver: object }).resolver, 'countOrphanedReferences');
    const result = await graph.sync();
    expect(result.filesModified).toBe(1);
    expect(result.durationMs).toBeGreaterThanOrEqual(DELAY_MS);
  });
});
