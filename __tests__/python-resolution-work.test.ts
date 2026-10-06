import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';


// Native graph regressions from upstream #2332/#2334.
// TypeScript spies do not observe the Rust resolver; these make no cost claim.
describe('Python resolution graph (#2332)', () => {
  let tmpDir: string | undefined;
  let cg: CodeGraph | undefined;

  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (tmpDir) fs.rmSync(tmpDir, { recursive: true, force: true, maxRetries: 5 });
    tmpDir = undefined;
  });

  async function index(files: Record<string, string>) {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-py-work-'));
    for (const [file, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(tmpDir, file)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, file), content);
    }
    cg = CodeGraph.initSync(tmpDir);
    await cg.indexAll();
    return cg;
  }

  it('resolves repeated global method values and excludes a locally rebound helper', async () => {
    const SCALE = 16;
    const conns = Array.from({ length: SCALE }, (_, i) => `conn${i}`);
    const files: Record<string, string> = {
      // Globals typed by their own module's writes: each one's type also
      // depends on what every other module writes to it.
      'store.py': 'class Store:\n    def fetch(self, ids):\n        return ids\n',
      'settings.py': `from store import Store\n${conns.map(c => `${c} = None\n`).join('')}\n` +
        `def init():\n    global ${conns.join(', ')}\n${conns.map(c => `    ${c} = Store()\n`).join('')}`,
      'consumer.py': `import settings\n${conns.map((c, i) => `\ndef cb${i}(pool):\n    pool.submit(settings.${c}.fetch)\n`).join('')}`,
      // A bare call in every function: each asks whether its function binds the name.
      'helpers.py': 'def helper():\n    return 1\n',
      'callers.py': Array.from({ length: SCALE }, (_, i) => `def f${i}():\n    return helper()\n\n`).join('') +
        'def shadowed(make):\n    helper = make()\n    return helper()\n',
    };
    // Files no reference touches.
    const fillers = Array.from({ length: 20 }, (_, i) => `pkg/filler${i}.py`);
    fillers.forEach((file, i) => { files[file] = `def filler${i}(x):\n    return x + ${i}\n`; });
    const graph = await index(files);

    // Both paths ran: every global's method value reached Store.fetch, and the
    // function that binds `helper` itself calls its own value.
    const fetch = graph.getNodesByName('fetch').find(n => n.qualifiedName === 'Store::fetch')!;
    expect(graph.getIncomingEdges(fetch.id).filter(e => e.metadata?.fnRef === true)
      .map(e => graph.getNode(e.source)?.name).sort()).toEqual(conns.map((_, i) => `cb${i}`).sort());
    const helper = graph.getNodesByName('helper').find(n => n.kind === 'function')!;
    expect(graph.getIncomingEdges(helper.id).filter(e => e.kind === 'calls')
      .map(e => graph.getNode(e.source)?.name).sort()).toEqual(Array.from({ length: SCALE }, (_, i) => `f${i}`).sort());
  });

});
