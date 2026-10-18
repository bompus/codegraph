/**
 * A call in Svelte or Astro markup on a name an arrow function declares
 * (`rows.map((count) => count.set(1))`) is a call on that parameter. The
 * template scan has no binding rows, so without this the script's import of the
 * same name is the only binding the resolver sees and the call links to it.
 * A call on a name the arrow does not declare still links.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let roots: string[] = [];
let graphs: CodeGraph[] = [];

afterEach(() => {
  for (const graph of graphs) graph.close();
  graphs = [];
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  roots = [];
});

async function index(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-markup-arrow-'));
  roots.push(root);
  for (const [file, text] of Object.entries({ 'package.json': '{"name":"app"}', ...files })) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), text);
  }
  const graph = await CodeGraph.init(root, { index: true });
  graphs.push(graph);
  return graph;
}

/** `<target kind> <target>` for each call made from `file` to another file (Astro also links a page to itself). */
function callsFrom(graph: CodeGraph, file: string): string[] {
  const ids = graph.getNodesInFile(file).map((n) => n.id);
  return graph
    .getOutgoingEdgesFrom(ids, ['calls'])
    .map((e) => graph.getNode(e.target)!)
    .filter((t) => t.filePath !== file)
    .map((t) => `${t.kind} ${t.name}`)
    .sort();
}

const STORES = {
  'src/stores.ts': 'export const count = writable(0);\nexport function bump(n) { return n + 1; }\n',
};

// One markup line per arrow shape; each declares `count` (or `bump`) itself.
const SHADOWING = [
  '{rows.map((count) => count.set(1))}',
  '{rows.map(count => count.set(1))}',
  '{rows.map((row, count) => count.set(row))}',
  '{rows.map(({ count }) => count.set(1))}',
  '{rows.map((bump) => bump(1))}',
];

describe('a markup call on a name an arrow function declares', () => {
  it('does not link to a same-named import in Svelte', async () => {
    const script = "<script>\n  import { count, bump } from './stores';\n  let rows = [];\n</script>\n";
    const files: Record<string, string> = { ...STORES };
    SHADOWING.forEach((line, i) => { files[`src/Shadow${i}.svelte`] = `${script}${line}\n`; });
    files['src/Plain.svelte'] = `${script}{rows.map((row) => count.set(row))}\n{bump(1)}\n`;
    const graph = await index(files);

    const shadowed = SHADOWING.map((_, i) => callsFrom(graph, `src/Shadow${i}.svelte`));
    expect(shadowed).toEqual(SHADOWING.map(() => []));
    expect(callsFrom(graph, 'src/Plain.svelte')).toEqual(['constant count', 'function bump']);
  }, 120_000);

  it('does not link to a same-named import in Astro', async () => {
    const front = "---\nimport { count, bump } from '../stores';\nconst rows = [];\n---\n";
    const files: Record<string, string> = { ...STORES };
    SHADOWING.forEach((line, i) => { files[`src/pages/shadow${i}.astro`] = `${front}<ul>${line}</ul>\n`; });
    files['src/pages/plain.astro'] = `${front}<ul>{rows.map((row) => count.set(row))}</ul>\n{bump(1)}\n`;
    const graph = await index(files);

    const shadowed = SHADOWING.map((_, i) => callsFrom(graph, `src/pages/shadow${i}.astro`));
    expect(shadowed).toEqual(SHADOWING.map(() => []));
    expect(callsFrom(graph, 'src/pages/plain.astro')).toEqual(['constant count', 'function bump']);
  }, 120_000);
});
