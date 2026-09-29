/**
 * A `paths` pattern whose literals overlap the import (`foo*oo` against
 * `foo`) does not match, as in TypeScript's isPatternMatch. The kernel
 * sliced the capture anyway and panicked, which aborted the process; the
 * next pattern must get the import instead.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';
import { loadProjectAliases, applyAliases } from '../src/resolution/path-aliases';

describe('overlapping paths literals', () => {
  let cg: CodeGraph | undefined;
  let dir: string;

  afterEach(() => {
    cg?.destroy();
    cg = undefined;
    fs.rmSync(dir, { recursive: true, force: true });
  });

  const write = (files: Record<string, string>): void => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-alias-overlap-'));
    for (const [name, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
      fs.writeFileSync(path.join(dir, name), content);
    }
  };

  const files = {
    'tsconfig.json': JSON.stringify({
      compilerOptions: { baseUrl: '.', paths: { 'foo*oo': ['src/wrong/*'], 'f*': ['src/*'] } },
    }),
    'src/oo.ts': 'export function target(): number { return 1; }\n',
    'src/app.ts': "import { target } from 'foo';\nexport function caller(): number { return target(); }\n",
  };

  it('skips the overlapping pattern when rewriting', () => {
    write(files);
    expect(applyAliases('foo', loadProjectAliases(dir)!, dir)).toEqual(['src/oo']);
  });

  it('resolves the import through the next pattern', async () => {
    write(files);
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
    const target = cg.getNodesByKind('function').find((n) => n.name === 'target');
    expect(target).toBeDefined();
    const callers = cg.getCallers(target!.id).filter((c) => c.edge.kind === 'calls');
    expect(callers.map((c) => c.node.name)).toContain('caller');
  });
});
