/**
 * An incremental sync refreshes synthesized edges: the ones a changed file
 * wires up come back (inline for a one-shot sync, after a debounce for the
 * watcher's), and the ones whose registration was removed are dropped even
 * when neither end's file changed.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { syncBuiltinESMExports } from 'node:module';
import CodeGraph from '../src/index';

describe('synthesized edges on incremental sync', () => {
  let dir: string;
  let cg: CodeGraph;
  const write = (rel: string, body: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), body);
  };
  const callees = (fn: string) => {
    const node = cg.getNodesByName(fn).find((n) => n.kind === 'function')!;
    return cg.getCallees(node.id).map((c) => c.node.name).sort();
  };

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-resynth-'));
    write('src/bus.ts', "import { EventEmitter } from 'events';\nexport const bus = new EventEmitter();\n");
    write('src/publish.ts', "import { bus } from './bus';\n\nexport function publish() {\n  bus.emit('saved', 1);\n}\n");
    write('src/handlers.ts', 'export function onSaved(x: number) {\n  console.log(x);\n}\n');
    write('src/wire.ts', "import { bus } from './bus';\nimport { onSaved } from './handlers';\n\nexport function wire() {\n  bus.on('saved', onSaved);\n}\n");
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    syncBuiltinESMExports();
    delete process.env.CODEGRAPH_SYNTH_REFRESH_MS;
    delete process.env.CODEGRAPH_SYNC_RESYNTHESIS;
    try { cg.close(); } catch { /* ignore */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('starts with the synthesized edges', () => {
    expect(callees('publish')).toContain('onSaved');
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });

  it('restores a changed file\'s synthesized edges on a one-shot sync', async () => {
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  // edited\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    await cg.sync();
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });

  it('drops an edge whose registration was removed in another file', async () => {
    write('src/wire.ts', "import { bus } from './bus';\n\nexport function wire() {\n  return bus;\n}\n");
    await cg.sync();
    expect(callees('publish')).not.toContain('onSaved');
  });

  it('drops an edge when the file holding its registration is deleted', async () => {
    fs.rmSync(path.join(dir, 'src/wire.ts'));
    await cg.sync();
    expect(callees('publish')).not.toContain('onSaved');
  });

  it('drops a removed registration\'s edge when the sync also re-resolves other files', async () => {
    // A second onSaved re-opens every edge to that name, so this sync runs the
    // orphan sweep, whose own synthesis pass has to leave the removed wiring out.
    write('src/use.ts', "import { onSaved } from './handlers';\n\nexport function use() {\n  onSaved(1);\n}\n");
    await cg.sync();
    write('src/wire.ts', "import { bus } from './bus';\n\nexport function wire() {\n  return bus;\n}\n");
    write('src/more.ts', 'export function onSaved(x: string) {\n  return x;\n}\n');
    await cg.sync();
    expect(callees('publish')).not.toContain('onSaved');
    expect(callees('use')).toContain('onSaved');
  });

  it('refreshes after a debounce when the sync defers it', async () => {
    process.env.CODEGRAPH_SYNTH_REFRESH_MS = '30';
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  // edited\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    await cg.sync({ deferSynthesis: true });
    expect(callees('loadRepo')).not.toContain('GET https://api.github.com/repos/${…}');
    await new Promise((r) => setTimeout(r, 400));
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });

  it('finishes queued watcher synthesis before a no-change complete reconciliation returns', async () => {
    process.env.CODEGRAPH_SYNTH_REFRESH_MS = '60000';
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  // deferred edit\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    await cg.sync({ deferSynthesis: true });
    expect(callees('loadRepo')).not.toContain('GET https://api.github.com/repos/${…}');
    const result = await cg.sync({ requireComplete: true });
    expect(result.filesAdded + result.filesModified + result.filesRemoved).toBe(0);
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });

  it('rejects incomplete synthesis and retries it on the next complete reconciliation', async () => {
    process.env.CODEGRAPH_SYNTH_REFRESH_MS = '60000';
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  // deferred edit\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    await cg.sync({ deferSynthesis: true });
    const resolver = (cg as unknown as { resolver: { resynthesize: (...args: unknown[]) => Promise<number> } }).resolver;
    const failure = vi.spyOn(resolver, 'resynthesize').mockRejectedValueOnce(new Error('injected synthesis failure'));
    await expect(cg.sync({ requireComplete: true })).rejects.toThrow('did not complete synthesized-edge refresh');
    expect(callees('loadRepo')).not.toContain('GET https://api.github.com/repos/${…}');
    failure.mockRestore();
    await cg.sync({ requireComplete: true });
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });

  it('rejects a failed pending-marker write and retains queued work for a no-change retry', async () => {
    process.env.CODEGRAPH_SYNTH_REFRESH_MS = '60000';
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  // pending marker failure\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    await cg.sync({ deferSynthesis: true });
    const queries = (cg as unknown as { queries: { setSynthesisPending: (pending: boolean) => void;
      isSynthesisPending: () => boolean } }).queries;
    expect(queries.isSynthesisPending()).toBe(false);
    const marker = vi.spyOn(queries, 'setSynthesisPending').mockImplementationOnce(() => {
      throw new Error('injected pending-marker failure');
    });
    await expect(cg.sync({ requireComplete: true })).rejects.toThrow('did not complete synthesized-edge refresh');
    expect(queries.isSynthesisPending()).toBe(false);
    marker.mockRestore();
    await cg.sync({ requireComplete: true });
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });

  it('serializes a complete refresh behind watcher indexing and flushes its deferred edges', async () => {
    process.env.CODEGRAPH_SYNTH_REFRESH_MS = '60000';
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  // queued watcher edit\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    const orchestrator = (cg as unknown as { orchestrator: { sync: (...args: unknown[]) => Promise<unknown> } }).orchestrator;
    const original = orchestrator.sync.bind(orchestrator);
    let release!: () => void;
    let entered!: () => void;
    const held = new Promise<void>(resolve => { release = resolve; });
    const started = new Promise<void>(resolve => { entered = resolve; });
    const sync = vi.spyOn(orchestrator, 'sync').mockImplementationOnce(async (...args) => {
      entered();
      await held;
      return original(...args);
    });
    const watcher = cg.sync({ deferSynthesis: true });
    await started;
    const refresh = cg.sync({ requireComplete: true });
    try { expect(sync).toHaveBeenCalledTimes(1); }
    finally { release(); await Promise.all([watcher, refresh]); }
    expect(sync).toHaveBeenCalledTimes(2);
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });

  it('rejects an unreadable changed file and indexes it when a later refresh can read it', async () => {
    const source = path.join(dir, 'src/handlers.ts');
    write('src/handlers.ts', 'export function readableAfterRetry() {}\n');
    const mutableFs = require('fs') as typeof fs;
    const original = mutableFs.openSync;
    const read = vi.spyOn(mutableFs, 'openSync').mockImplementation((file, ...args) => {
      if (String(file) === source) throw Object.assign(new Error('injected read failure'), { code: 'EACCES' });
      return Reflect.apply(original, fs, [file, ...args]);
    });
    syncBuiltinESMExports();
    await expect(cg.sync({ requireComplete: true })).rejects.toThrow('failed to index 1 file');
    expect(cg.getNodesByName('onSaved')).not.toHaveLength(0);
    read.mockRestore();
    syncBuiltinESMExports();
    await cg.sync({ requireComplete: true });
    expect(cg.getNodesByName('readableAfterRetry')).not.toHaveLength(0);
    expect(cg.getNodesByName('onSaved')).toHaveLength(0);
  });

  it('refuses complete reconciliation while synthesized-edge refresh is disabled', async () => {
    process.env.CODEGRAPH_SYNC_RESYNTHESIS = '0';
    await expect(cg.sync({ requireComplete: true })).rejects.toThrow('requires synthesized-edge refresh');
  });

  it('acknowledges current wiring across every refresh-ending sequence of up to three events', async () => {
    process.env.CODEGRAPH_SYNTH_REFRESH_MS = '60000';
    const attached = "import { bus } from './bus';\nimport { onSaved } from './handlers';\nexport function wire() { bus.on('saved', onSaved); }\n";
    const detached = "import { bus } from './bus';\nexport function wire() { return bus; }\n";
    const events = ['attach', 'detach', 'watcher', 'refresh'] as const;
    const failures: string[] = [];
    const prefixes = [[], ...events.map(a => [a]), ...events.flatMap(a => events.map(b => [a, b]))];
    for (const prefix of prefixes) {
      write('src/wire.ts', attached);
      await cg.sync({ requireComplete: true });
      let wired = true;
      const sequence = [...prefix, 'refresh'];
      for (const event of sequence) {
        if (event === 'attach' || event === 'detach') {
          wired = event === 'attach';
          write('src/wire.ts', wired ? attached : detached);
        } else if (event === 'watcher') {
          await cg.sync({ paths: ['src/wire.ts'], deferSynthesis: true });
        } else {
          await cg.sync({ requireComplete: true });
          if (callees('publish').includes('onSaved') !== wired) failures.push(sequence.join(' -> '));
        }
      }
    }
    expect(failures).toEqual([]);
  }, 20000);
});
