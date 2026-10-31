/**
 * JSX synthesis shard parity: splitting the jsxEdges pass across the resolver
 * pool must produce exactly the single-pass edge set, in the same insertion
 * order. The pass shards by contiguous candidate ranges; the merge
 * concatenates shards in index order, so edge order is preserved.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

const { DatabaseSync } = require('node:sqlite') as typeof import('node:sqlite');

let tmpDir: string;

beforeEach(() => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-jsx-shard-'));
});

afterEach(() => {
  delete process.env.CODEGRAPH_SYNTH_SHARDS;
  delete process.env.CODEGRAPH_PARALLEL_RESOLVE_MIN;
  delete process.env.CODEGRAPH_RESOLVE_WORKERS;
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

function writeProject(): void {
  const files: Record<string, string> = {
    'src/Button.tsx': 'export function Button() { return <button>ok</button>; }\n',
    'src/Label.tsx': 'export function Label() { return <span>label</span>; }\n',
    'src/Card.tsx':
      "import { Button } from './Button';\nimport { Label } from './Label';\n" +
      'export function Card() { return <div><Button /><Label /></div>; }\n',
    'src/List.tsx':
      "import { Card } from './Card';\n" +
      'export function List() { return <ul><li><Card /></li><li><Card /></li></ul>; }\n',
    'src/App.tsx':
      "import { List } from './List';\n" +
      'export default function App() { return <main><List /><List /></main>; }\n',
    'src/handlers.ts':
      'function saveHandler() { return 1; }\n' +
      'function loadHandler() { return 2; }\n' +
      'const handlers = { save: saveHandler, load: loadHandler };\n' +
      'export function run(kind: string) { return handlers[kind](); }\n',
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, rel), content);
  }
}

async function indexAndDump(shards: string): Promise<string> {
  process.env.CODEGRAPH_SYNTH_SHARDS = shards;
  process.env.CODEGRAPH_PARALLEL_RESOLVE_MIN = '0';
  process.env.CODEGRAPH_RESOLVE_WORKERS = '4';
  const cg = await CodeGraph.init(tmpDir, { index: true });
  await cg.close();
  const db = new DatabaseSync(path.join(tmpDir, '.codegraph', 'codegraph.db'), { readOnly: true });
  const rows = db
    .prepare("SELECT id, source, target, kind, ifnull(line, '') AS line, ifnull(metadata, '') AS metadata FROM edges ORDER BY id")
    .all();
  db.close();
  fs.rmSync(path.join(tmpDir, '.codegraph'), { recursive: true, force: true });
  return JSON.stringify(rows);
}

describe('JSX synthesis sharding', () => {
  it('produces the single-pass edges in the same order', async () => {
    writeProject();
    const single = await indexAndDump('1');
    const sharded = await indexAndDump('4');
    expect(single).toContain('jsx-render');
    expect(single).toContain('object-registry');
    if (single !== sharded) {
      const a = JSON.parse(single) as Array<Record<string, unknown>>;
      const b = JSON.parse(sharded) as Array<Record<string, unknown>>;
      for (let i = 0; i < Math.max(a.length, b.length); i++) {
        if (JSON.stringify(a[i]) !== JSON.stringify(b[i])) {
          console.log('first diff at', i, 'single=', a[i], 'sharded=', b[i], 'lens', a.length, b.length);
          break;
        }
      }
    }
    expect(sharded).toBe(single);
  });
});
