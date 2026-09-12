/**
 * The precision scorer (__tests__/evaluation/scoring.ts, scoreEdgeCase) on a
 * synthetic project that reproduces the shapes the resolution PR chain
 * removed: a bare npm import that must not fuzzy-bind to a same-named
 * project symbol (#1713) and a sealed module whose locals must not be
 * cross-file candidates (#1746), plus a relative import as the `present`
 * control. This pins that the scorer can SEE a false edge — the recall
 * scorers cannot — before the binding-model plan changes any resolver.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { scoreEdgeCase } from './evaluation/scoring';
import type { EdgeCase } from './evaluation/types';

let dir: string;
let cg: CodeGraph;

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-precision-'));
  const write = (rel: string, body: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), body);
  };
  write('package.json', JSON.stringify({ name: 'precision-fixture', dependencies: { 'magic-string': '1' } }));
  // #1713 shape: a bare import whose name collides with a project function.
  write('src/consumer.ts', "import { bundle } from 'magic-string';\nimport { helper } from './util';\nexport function run() { return bundle(helper()); }\n");
  write('scripts/build.ts', 'export function bundle(): string { return "x"; }\n');
  write('src/util.ts', 'export function helper(): number { return 1; }\n');
  // #1746 shape: a module that imports but exports nothing.
  write('sealed.ts', "import { helper } from './src/util';\nconst widget = helper();\n");
  write('src/other.ts', "import { helper } from './util';\nexport function use() { return widget + helper(); }\n");
  cg = await CodeGraph.init(dir, { index: true });
  cg.resolveReferences();
});
afterAll(() => {
  cg.close();
  fs.rmSync(dir, { recursive: true, force: true });
});

const base = { corpus: 'synthetic', source: 'test', why: '' } as const;

describe('scoreEdgeCase', () => {
  it('holds an absent case when the bare import does not bind to the project symbol', () => {
    const c: EdgeCase = { ...base, id: 'bare', kind: 'imports', from: { file: 'src/consumer.ts', name: 'consumer.ts' }, to: { file: 'scripts/build.ts', name: 'bundle' }, expect: 'absent' };
    const r = scoreEdgeCase(c, cg);
    expect(r.found).toEqual([]);
    expect(r.pass).toBe(true);
  });

  it('holds an absent case when a sealed module local is not reached cross-file', () => {
    const c: EdgeCase = { ...base, id: 'sealed', kind: 'references', from: { file: 'src/other.ts', name: 'use' }, to: { file: 'sealed.ts', name: 'widget' }, expect: 'absent' };
    const r = scoreEdgeCase(c, cg);
    expect(r.pass).toBe(true);
  });

  it('holds a present control for a relative import', () => {
    const c: EdgeCase = { ...base, id: 'control', kind: 'imports', from: { file: 'src/consumer.ts', name: 'consumer.ts' }, to: { file: 'src/util.ts', name: 'helper' }, expect: 'present' };
    const r = scoreEdgeCase(c, cg);
    expect(r.found.length).toBeGreaterThan(0);
    expect(r.pass).toBe(true);
  });

  it('reports a violated absent case when the edge exists', () => {
    // Use the control edge as if it were known-wrong: the scorer must flag it.
    const c: EdgeCase = { ...base, id: 'violated', kind: 'imports', from: { file: 'src/consumer.ts', name: 'consumer.ts' }, to: { file: 'src/util.ts', name: 'helper' }, expect: 'absent' };
    const r = scoreEdgeCase(c, cg);
    expect(r.pass).toBe(false);
    expect(r.found[0]?.resolvedBy).toBeDefined();
  });

  it('fails a present control whose endpoint is not in the graph, and says which', () => {
    const c: EdgeCase = { ...base, id: 'missing', kind: 'imports', from: { file: 'src/consumer.ts', name: 'consumer.ts' }, to: { file: 'src/nowhere.ts', name: 'nothing' }, expect: 'present' };
    const r = scoreEdgeCase(c, cg);
    expect(r.pass).toBe(false);
    expect(r.missingEndpoints).toEqual(['to src/nowhere.ts:nothing']);
  });
});
