import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';
import { loadWorkspaceSourceEntries } from '../src/resolution/workspace-source-entries';

let dir: string;
let graph: CodeGraph | undefined;
afterEach(() => { graph?.close(); graph = undefined; if (dir) fs.rmSync(dir, { recursive: true, force: true }); });
function fixture(config: string, extra: Record<string, unknown> = {}) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-workspace-source-'));
  fs.mkdirSync(path.join(dir, 'packages/lib'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ workspaces: ['packages/*'] }));
  fs.writeFileSync(path.join(dir, 'packages/lib/package.json'), JSON.stringify({
    name: 'lib', exports: { '.': './dist/node/index.js', './runner': './dist/node/runner.js' },
    scripts: { build: 'rolldown --config rolldown.config.ts' }, ...extra,
  }));
  fs.writeFileSync(path.join(dir, 'packages/lib/rolldown.config.ts'), config);
}
const config = `import path from 'node:path';
import { defineConfig as config } from 'rolldown';
const dirname = import.meta.dirname;
const shared = config({ output: { dir: './dist', entryFileNames: 'node/[name].js' } });
const main = config({ ...shared, input: { index: path.resolve(dirname, 'src/index.ts') } });
const runner = config({ ...shared, input: { runner: path.resolve(dirname, 'src/runner/index.ts') },
  output: { ...shared.output, format: 'esm' } });
export default config([main, runner]);`;

describe('workspace bundle source entries', () => {
  it('connects a factory receiver through two public entries and re-export aliases', async () => {
    fixture(config);
    const files = {
      'packages/lib/src/index.ts': "export { make as createRunner } from './factory';",
      'packages/lib/src/factory.ts': "import { Runner } from 'lib/runner';\nexport function make(): Runner { return new Runner(); }",
      'packages/lib/src/runner/index.ts': "export { Runner } from './runner';",
      'packages/lib/src/runner/runner.ts': 'export class Runner { run() { return 1; } }',
      'packages/lib/src/internal.ts': 'export class Wrong { run() {} }\nexport function createRunner(): Wrong { return new Wrong(); }',
      'app.ts': "import { createRunner } from 'lib';\nexport function consumer() { const runner = createRunner(); return runner.run(); }",
    };
    for (const [file, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
      fs.writeFileSync(path.join(dir, file), content);
    }
    expect([...loadWorkspaceSourceEntries(dir, 'packages/lib', 'lib')]).toEqual([
      ['lib', 'packages/lib/src/index.ts'], ['lib/runner', 'packages/lib/src/runner/index.ts'],
    ]);
    graph = await CodeGraph.init(dir, { index: true });
    graph.resolveReferences();
    const caller = graph.getNodesByKind('function').find(n => n.name === 'consumer')!;
    const targets = graph.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => graph!.getNode(e.target)!.qualifiedName);
    expect(targets).toContain('Runner::run');
    expect(targets).not.toContain('Wrong::run');
  });

  it.each([
    ['mutated config', config.replace('export default', "main.input = { index: 'src/other.ts' };\nexport default")],
    ['mutation through an alias', config.replace('export default', "const alias = main; const changed = Object.assign(alias, {input: 'src/other.ts'});\nexport default")],
    ['mutation through a local helper', config.replace('export default', "function mutate() { main.input = {index: 'src/other.ts'}; } const ignored = mutate();\nexport default")],
    ['mutation through an array receiver', config.replace('export default config([main, runner]);', 'const configs = [main, runner]; const removed = configs.pop(); export default configs;')],
    ['dynamic config alternative', config.replace('[main, runner]', '[main, chooseConfig()]')],
    ['dynamic output name', config.replace("'node/[name].js'", 'computeFilename()')],
    ['preserved modules', config.replace("dir: './dist'", "preserveModules: true, dir: './dist'")],
    ['conflicting entries', config.replace("runner: path.resolve(dirname, 'src/runner/index.ts')", "index: path.resolve(dirname, 'src/other.ts')")],
    ['unknown spread', config.replace('...shared, input:', '...getOptions(), input:')],
  ])('rejects %s without executing configuration code', (_label, source) => {
    fixture(source);
    expect([...loadWorkspaceSourceEntries(dir, 'packages/lib', 'lib')]).toEqual([]);
  });

  it('requires an explicit package-local build command', () => {
    fixture(config, { scripts: { build: 'cd ../.. && rolldown --config packages/lib/rolldown.config.ts' } });
    expect([...loadWorkspaceSourceEntries(dir, 'packages/lib', 'lib')]).toEqual([]);
  });

  it('does not infer wildcard or conditionally different public exports', () => {
    fixture(config, { exports: { './*': './dist/node/*.js', '.': { import: './dist/node/index.js', require: './dist/node/runner.js' } } });
    expect([...loadWorkspaceSourceEntries(dir, 'packages/lib', 'lib')]).toEqual([]);
  });
});

describe('workspace exports conditions naming committed source', () => {
  function sourceFixture(files: string[], exports: unknown) {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-workspace-exports-'));
    fs.mkdirSync(path.join(dir, 'packages/zod'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ workspaces: ['packages/*'] }));
    fs.writeFileSync(path.join(dir, 'packages/zod/package.json'), JSON.stringify({ name: 'zod', exports }));
    for (const f of files) {
      fs.mkdirSync(path.dirname(path.join(dir, 'packages/zod', f)), { recursive: true });
      fs.writeFileSync(path.join(dir, 'packages/zod', f), 'export const x = 1;\n');
    }
  }
  const zodExports = {
    '.': { '@zod/source': './src/index.ts', types: './index.d.cts', import: './index.js' },
    './v4': { '@zod/source': './src/v4/index.ts', types: './v4/index.d.cts', import: './v4/index.js' },
  };

  it('maps each subpath to the one condition target present in the checkout', () => {
    sourceFixture(['src/index.ts', 'src/v4/index.ts', 'v4/index.d.cts'], zodExports);
    expect(new Map(loadWorkspaceSourceEntries(dir, 'packages/zod', 'zod'))).toEqual(new Map([
      ['zod', 'packages/zod/src/index.ts'],
      ['zod/v4', 'packages/zod/src/v4/index.ts'],
    ]));
  });

  it('skips a subpath whose build output is also present, rather than guess', () => {
    sourceFixture(['src/v4/index.ts', 'v4/index.js'], { './v4': zodExports['./v4'] });
    expect([...loadWorkspaceSourceEntries(dir, 'packages/zod', 'zod')]).toEqual([]);
  });

  it('resolves a namespace import through the mapped subpath', async () => {
    sourceFixture(['src/v4/index.ts'], { './v4': zodExports['./v4'] });
    fs.writeFileSync(path.join(dir, 'packages/zod/src/v4/index.ts'), 'export function string() { return 1; }\n');
    fs.mkdirSync(path.join(dir, 'packages/zod/tests'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'packages/zod/tests/use.test.ts'), 'import * as z from "zod/v4";\nexport function check() { return z.string(); }\n');
    graph = await CodeGraph.init(dir, { index: true });
    const check = graph.getNodesByKind('function').find(n => n.name === 'check')!;
    const targets = graph.getOutgoingEdges(check.id).filter(e => e.kind === 'calls').map(e => graph!.getNode(e.target)!.filePath);
    expect(targets).toContain('packages/zod/src/v4/index.ts');
  });
});
