/**
 * An incremental sync refreshes synthesized edges: the ones a changed file
 * wires up come back (inline for a one-shot sync, after a debounce for the
 * watcher's), and the ones whose registration was removed are dropped even
 * when neither end's file changed.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
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
    delete process.env.CODEGRAPH_SYNTH_REFRESH_MS;
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

  it('refreshes after a debounce when the sync defers it', async () => {
    process.env.CODEGRAPH_SYNTH_REFRESH_MS = '30';
    write('src/api.ts', 'export async function loadRepo(owner: string) {\n  // edited\n  return fetch(`https://api.github.com/repos/${owner}`);\n}\n');
    await cg.sync({ deferSynthesis: true });
    expect(callees('loadRepo')).not.toContain('GET https://api.github.com/repos/${…}');
    await new Promise((r) => setTimeout(r, 400));
    expect(callees('loadRepo')).toContain('GET https://api.github.com/repos/${…}');
  });
});
