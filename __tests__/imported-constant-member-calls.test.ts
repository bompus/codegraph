/**
 * A member call on an imported constant or variable (`LIST.includes(x)` after
 * `import { LIST } from './constants'`) links to that constant. The callee is
 * a member of the constant, which the graph has no node for, so the constant
 * is the most precise target.
 *
 * Two calls that only look alike stay unlinked: a name a parameter or local
 * hides (`function f(LIST) { LIST.includes(x) }`), and a deeper chain
 * (`config.api.fetch()`), whose callee hangs off a member rather than the
 * constant itself.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

const FILES: Record<string, string> = {
  'package.json': JSON.stringify({ name: 'app' }),
  'src/constants.ts': [
    "export const LIST = ['a', 'b'];",
    'export const config = { api: { fetch() { return 1; } } };',
    'function makeState() { return { inc() { return 1; } }; }',
    'export let state = makeState();',
    '',
  ].join('\n'),
  'src/use.ts': [
    "import { LIST, config, state } from './constants';",
    'function makeThing() { return { inc() { return 2; } }; }',
    'export function direct(x: string) { return LIST.includes(x); }',
    'export function viaVariable() { return state.inc(); }',
    'export function deep() { return config.api.fetch(); }',
    'export function shadowedByParameter(LIST: string[]) { return LIST.includes("a"); }',
    'export function shadowedByLocal() { const state = makeThing(); return state.inc(); }',
    '',
  ].join('\n'),
};

let roots: string[] = [];
let graphs: CodeGraph[] = [];

afterEach(() => {
  for (const graph of graphs) graph.close();
  graphs = [];
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  roots = [];
});

/** `<caller> -> <target kind> <target>` for every call made through a member reference. */
function memberCalls(graph: CodeGraph): string[] {
  const nodes = graph.getFiles().flatMap((file) => graph.getNodesInFile(file.path));
  const links: string[] = [];
  for (const edge of graph.getOutgoingEdgesFrom(nodes.map((n) => n.id))) {
    const ref = edge.metadata?.refName;
    if (edge.kind !== 'calls' || typeof ref !== 'string' || !ref.includes('.')) continue;
    const source = graph.getNode(edge.source)!;
    const target = graph.getNode(edge.target)!;
    links.push(`${source.name}: ${ref} -> ${target.kind} ${target.name}`);
  }
  return links.sort();
}

describe('a member call on an imported constant', () => {
  it('links to the constant, and not through a shadowed name or a deeper chain', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-const-member-'));
    roots.push(root);
    for (const [file, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
      fs.writeFileSync(path.join(root, file), text);
    }
    const graph = await CodeGraph.init(root, { index: true });
    graphs.push(graph);

    expect(memberCalls(graph)).toEqual([
      'direct: LIST.includes -> constant LIST',
      'viaVariable: state.inc -> variable state',
    ]);
  }, 120_000);
});
