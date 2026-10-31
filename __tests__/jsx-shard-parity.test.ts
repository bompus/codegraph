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
  delete process.env.CODEGRAPH_NO_PARALLEL_RESOLVE;
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
    'src/Widget.tsx':
      'export class Widget extends Component {\n' +
      '  update() { this.setState({ ready: true }); }\n' +
      '  render() { return <div />; }\n' +
      '}\n',
    'src/main.tsx':
      "import { createRoot } from 'react-dom/client';\nimport App from './App';\n" +
      "createRoot(document.getElementById('root')!).render(<App />);\n",
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

async function indexAndDump(mode: { shards?: string; sequential?: boolean }): Promise<string> {
  if (mode.sequential) {
    process.env.CODEGRAPH_NO_PARALLEL_RESOLVE = '1';
    delete process.env.CODEGRAPH_PARALLEL_RESOLVE_MIN;
    delete process.env.CODEGRAPH_RESOLVE_WORKERS;
    delete process.env.CODEGRAPH_SYNTH_SHARDS;
  } else {
    delete process.env.CODEGRAPH_NO_PARALLEL_RESOLVE;
    process.env.CODEGRAPH_SYNTH_SHARDS = mode.shards ?? '1';
    process.env.CODEGRAPH_PARALLEL_RESOLVE_MIN = '0';
    process.env.CODEGRAPH_RESOLVE_WORKERS = '4';
  }
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
    // The sequential run is the reference: a shard-slot bug (jsx written to
    // another pass's index, a dropped gated pass, a doubled pass) changes the
    // edge table or its order against it, which two sharded runs cannot see.
    const sequential = await indexAndDump({ sequential: true });
    const single = await indexAndDump({ shards: '1' });
    const sharded = await indexAndDump({ shards: '4' });
    expect(sequential).toContain('jsx-render');
    expect(sequential).toContain('object-registry');
    expect(sequential).toContain('react-render');
    expect(single).toBe(sequential);
    expect(sharded).toBe(sequential);
  });
});
