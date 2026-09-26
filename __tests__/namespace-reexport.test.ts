/**
 * `export * as ns from` exports one name (`ns`), not the module's members.
 * A barrel that namespaces one module and star-exports another must resolve a
 * name to the star-exported definition, even when the namespaced module
 * (listed first) has a member of the same name — zod's v4 barrel does exactly
 * this (`export * as core` before `export * from "./schemas"`), and treating the
 * namespace as a flat wildcard bound `z.string()` to a regex constant.
 */
import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let dir: string;
let graph: CodeGraph | undefined;
afterEach(() => { graph?.close(); graph = undefined; if (dir) fs.rmSync(dir, { recursive: true, force: true }); });

describe('namespace re-exports in a barrel', () => {
  it('resolves a name to the star-exported module, not a namespaced one', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-ns-reexport-'));
    const write = (rel: string, body: string) => {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), body);
    };
    write('src/core/regexes.ts', 'export function string() { return /x/; }\n');
    write('src/core/index.ts', 'export * as regexes from "./regexes";\n');
    write('src/schemas.ts', 'export function string() { return "schema"; }\n');
    write('src/barrel.ts', 'export * as core from "./core/index";\nexport * from "./schemas";\n');
    write('src/use.ts', 'import { string } from "./barrel";\nexport function build() { return string(); }\n');
    graph = await CodeGraph.init(dir, { index: true });
    const build = graph.getNodesByKind('function').find(n => n.name === 'build')!;
    const targets = graph.getOutgoingEdges(build.id).filter(e => e.kind === 'calls').map(e => graph!.getNode(e.target)!.filePath);
    expect(targets).toEqual(['src/schemas.ts']);
  });

  it('reaches a namespace member forwarded by a named re-export inside a star-exported module', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-ns-member-'));
    const write = (rel: string, body: string) => {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), body);
    };
    write('src/core/util.ts', 'export function clone(x: unknown) { return x; }\n');
    write('src/core/schemas.ts', 'export { clone } from "./util";\n');
    write('src/core/index.ts', 'export * as util from "./util";\nexport * from "./schemas";\n');
    write('src/use.ts', 'import * as core from "./core/index";\nexport function copy(v: unknown) { return core.clone(v); }\n');
    graph = await CodeGraph.init(dir, { index: true });
    const copy = graph.getNodesByKind('function').find(n => n.name === 'copy')!;
    const targets = graph.getOutgoingEdges(copy.id).filter(e => e.kind === 'calls').map(e => graph!.getNode(e.target)!.filePath);
    expect(targets).toEqual(['src/core/util.ts']);
  });
});
