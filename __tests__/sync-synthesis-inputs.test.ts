/**
 * A sync whose lazy value loads a workspace package commits (#2565).
 *
 * A lazy value that loads a workspace package, `dynamic(() =>
 * import('icons').then((m) => m.EdgeFunctions))`, names the package's
 * directory as the module it reads. Upstream recorded that directory as a
 * synthesis input and the foreign key into `files(path)` rolled every sync
 * back. This fork keeps no synthesis-inputs table (it retries a failed refresh
 * through a pending marker), so only the sync-versus-fresh-index outcome is
 * checked here; upstream's two tests of that table's writes do not apply.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

type Files = Record<string, string>;

/** A pnpm workspace whose app lazy-loads a component from a workspace package (supabase studio's shape). */
const WORKSPACE: Files = {
  'pnpm-workspace.yaml': "packages:\n  - 'packages/*'\n",
  'package.json': JSON.stringify({ name: 'app', private: true, workspaces: ['packages/*'], dependencies: { react: '^18.0.0', next: '^14.0.0' } }),
  'packages/icons/package.json': JSON.stringify({ name: 'icons', main: './src/icons/index.ts' }),
  'packages/icons/src/icons/index.ts': 'export function EdgeFunctions() {\n  return null;\n}\n',
  'src/App.tsx':
    "import dynamic from 'next/dynamic';\n" +
    "const Chart = dynamic(() => import('icons').then((mod) => mod.EdgeFunctions));\n" +
    'export function App() {\n  return <Chart />;\n}\n',
};

let roots: string[] = [];
let graphs: CodeGraph[] = [];

afterEach(() => {
  for (const graph of graphs) graph.close();
  graphs = [];
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  roots = [];
});

function write(root: string, files: Files): void {
  for (const [file, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), text);
  }
}

async function indexed(files: Files): Promise<{ root: string; graph: CodeGraph }> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-sync-synth-inputs-'));
  roots.push(root);
  write(root, files);
  const graph = await CodeGraph.init(root, { index: true });
  graphs.push(graph);
  return { root, graph };
}

/** Every edge, by its ends' natural keys. */
function edgesOf(graph: CodeGraph): string[] {
  const keys = new Map(
    graph.getFiles()
      .flatMap((file) => graph.getNodesInFile(file.path))
      .map((n) => [n.id, `${n.kind} ${n.filePath}:${n.qualifiedName}:${n.startLine}`] as const)
  );
  return graph.getOutgoingEdgesFrom([...keys.keys()])
    .map((e) => `${e.kind} ${keys.get(e.source)} -> ${keys.get(e.target) ?? e.target} @${e.line}:${e.column} ${JSON.stringify(e.metadata ?? {})}`)
    .sort();
}

describe('a sync whose lazy value loads a workspace package', () => {
  it('commits instead of failing on the package directory, and matches a fresh index', async () => {
    const { root, graph } = await indexed(WORKSPACE);
    const appended = { 'src/App.tsx': `${WORKSPACE['src/App.tsx']}// touched\n` };
    write(root, appended);
    const result = await graph.sync();
    expect(result.filesModified).toBe(1);

    // A second sync after another edit still works: nothing was left half-done.
    write(root, { 'src/App.tsx': `${appended['src/App.tsx']}// touched again\n` });
    expect((await graph.sync()).filesModified).toBe(1);

    const fresh = await indexed({ ...WORKSPACE, 'src/App.tsx': `${appended['src/App.tsx']}// touched again\n` });
    expect(edgesOf(graph)).toEqual(edgesOf(fresh.graph));
  }, 120_000);
});
