/**
 * Phase 4 kernel resolver (docs/design/resolution-binding-model-plan.md §4):
 * the binding-backed bare-name slice resolves natively in the Rust kernel —
 * readPendingBatch + resolveChunk over bindings/nodes/unresolved_refs — while
 * the TypeScript ReferenceResolver stays the orchestrator for passthrough
 * refs, framework merging, and persistence.
 *
 * These tests pin the contract:
 *   - the native KernelResolver class exists and answers the chunk contract;
 *   - a bare imported call resolves, a builtin misses (unresolved — never a
 *     fabricated edge), and a receiver-shaped name passes through to TS;
 *   - a whole project resolves byte-identically with the kernel on and with
 *     CODEGRAPH_KERNEL_RESOLVE=0 (the TypeScript fallback).
 */
import { describe, it, expect, afterEach } from 'vitest';
import { builtinModules } from 'module';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { getKernel } from '../src/extraction/kernel/loader';
import { getAllFrameworkResolvers } from '../src/resolution/frameworks';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

const FIXTURE: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: { baseUrl: '.', paths: { '@lib/*': ['src/*'] } },
  }),
  'src/util.ts': [
    'export function helper() { return 1; }',
    "export const VERSION = '1';",
  ].join('\n'),
  'src/barrel.ts': "export { helper as renamed } from './util';",
  'src/main.ts': [
    "import { helper, VERSION } from './util';",
    "import { renamed } from './barrel';",
    "import { helper as aliased } from '@lib/util';",
    'export function run() {',
    '  helper();',
    '  renamed();',
    '  aliased();',
    '  const v = VERSION;',
    '  const d = new Date();',
    '  console.log(d, v);',
    '}',
  ].join('\n'),
  'tool.py': 'def pyhelper():\n    return 1\n',
  'main.py': 'from tool import pyhelper\n\ndef go():\n    pyhelper()\n',
};

let tempDir: string | null = null;
let cg: CodeGraph | null = null;
const env = { kernelResolve: process.env.CODEGRAPH_KERNEL_RESOLVE };

async function project(kernelResolve: boolean): Promise<CodeGraph> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-kresolve-'));
  for (const [rel, content] of Object.entries(FIXTURE)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), content);
  }
  process.env.CODEGRAPH_KERNEL_RESOLVE = kernelResolve ? undefined : '0';
  cg = await CodeGraph.init(tempDir, { index: true });
  await cg.resolveReferencesBatched();
  return cg;
}

interface EdgeRow { source: string; target: string; kind: string; metadata: string | null }
interface RefRow {
  from_node_id: string; reference_name: string; reference_kind: string;
  status: string; failure_reason: string | null;
}

function dump(graph: CodeGraph): { edges: EdgeRow[]; refs: RefRow[] } {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const db = (graph as any).db.db as import('node:sqlite').DatabaseSync;
  const edges = (db.prepare(
    'SELECT source, target, kind, metadata FROM edges ORDER BY source, target, kind, metadata',
  ).all() as unknown as EdgeRow[]);
  const refs = (db.prepare(
    'SELECT from_node_id, reference_name, reference_kind, status, failure_reason FROM unresolved_refs ORDER BY from_node_id, reference_name, reference_kind, line, col',
  ).all() as unknown as RefRow[]);
  return { edges, refs };
}

afterEach(() => {
  if (env.kernelResolve === undefined) delete process.env.CODEGRAPH_KERNEL_RESOLVE;
  else process.env.CODEGRAPH_KERNEL_RESOLVE = env.kernelResolve;
  cg?.close();
  cg = null;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  tempDir = null;
});

describe.skipIf(!kernelBuilt)('kernel resolver (Phase 4)', () => {
  it('exports KernelResolver and resolves a bare imported call natively', async () => {
    const kernel = getKernel();
    expect(kernel?.KernelResolver).toBeDefined();
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-kresolve-'));
    for (const [rel, content] of Object.entries(FIXTURE)) {
      fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tempDir, rel), content);
    }
    cg = await CodeGraph.init(tempDir, { index: true });

    // init resolves inline — seed synthetic pending rows in the exact shape
    // extraction leaves them, from the already-indexed call sites.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
    const runFn = cg.getNodesByKind('function').find((n) => n.name === 'run')!.id;
    const goFn = cg.getNodesByKind('function').find((n) => n.name === 'go')!.id;
    const ins = db.prepare(
      "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, file_path, language, status) VALUES (?,?,?,?,?,?,?,'pending')",
    );
    ins.run(runFn, 'helper', 'calls', 5, 2, 'src/main.ts', 'typescript');
    ins.run(runFn, 'console.log', 'calls', 9, 2, 'src/main.ts', 'typescript');
    ins.run(runFn, 'Date', 'instantiates', 8, 14, 'src/main.ts', 'typescript');
    ins.run(goFn, 'pyhelper', 'calls', 4, 4, 'main.py', 'python');

    const resolver = new kernel!.KernelResolver!({
      dbPath: path.join(tempDir, '.codegraph', 'codegraph.db'),
      projectRoot: tempDir,
      aliases: {
        baseUrl: '.',
        patterns: [{ prefix: '@lib/', suffix: '', hasWildcard: true, replacements: ['src/*'] }],
      },
      cppIncludeDirs: [],
      nodeBuiltinSpecifiers: [...builtinModules],
      frameworksActive: false,
    });
    const batch = resolver.readPendingBatch(0, 200, false);
    expect(batch.length).toBeGreaterThanOrEqual(4);
    const outcomes = resolver.resolveChunk(batch);
    expect(outcomes.length).toBe(batch.length);

    const byName = new Map(batch.map((r, i) => [`${r.referenceName}@${r.referenceKind}`, i]));
    const helper = outcomes[byName.get('helper@calls')!]!;
    expect(helper.status).toBe('resolved');
    expect(helper.targetNodeId).toBe(cg.getNodesByKind('function').find((n) => n.name === 'helper')!.id);
    // console.log is receiver-shaped — the kernel declines it for the TS pipeline.
    expect(outcomes[byName.get('console.log@calls')!]!.status).toBe('passthrough');
    // Date is a JS builtin — terminal unresolved, never a fabricated edge.
    expect(outcomes[byName.get('Date@instantiates')!]!.status).toBe('unresolved');
    // pyhelper resolves through its Python import binding.
    const py = outcomes[byName.get('pyhelper@calls')!]!;
    expect(py.status).toBe('resolved');
  });

  it('resolves byte-identically to the TypeScript fallback', async () => {
    const withKernel = dump(await project(true));
    await cg!.close();
    cg = null;
    const withoutKernel = dump(await project(false));
    expect(withKernel).toEqual(withoutKernel);
    // Guard against a trivially-passing comparison — the fixture must actually
    // resolve edges through the ported strategies.
    const resolvedBy = withKernel.edges
      .map((e) => (e.metadata ? (JSON.parse(e.metadata) as { resolvedBy?: string }).resolvedBy : undefined))
      .filter(Boolean);
    expect(resolvedBy).toContain('import');
    expect(resolvedBy).toContain('exact-match');
  });

  it('leaves genuine misses failed rather than fabricating edges', async () => {
    const graph = await project(true);
    const failed = dump(graph).refs.filter((r) => r.status === 'failed');
    // console.log (external builtin member) and Date (builtin) must stay failed.
    const names = new Set(failed.map((r) => r.reference_name));
    expect(names.has('console.log')).toBe(true);
    expect(names.has('Date')).toBe(true);
  });

  it('replicates every registered resolver\'s claimsReference predicate', async () => {
    const kernel = getKernel();
    expect(kernel?.frameworkClaimsName).toBeDefined();
    const names = [
      // bare names the NAV/verb predicates can claim
      'navigate', 'redirect', 'goto', 'push', 'replace', 'prefetch', 'navigateTo',
      'dismissTo', 'permanentRedirect', 'xPush', 'fooNavigate', 'hook_init',
      '_iterable_class', 'fooController@bar', 'helper', 'useState', 'mypush',
      // qualified shapes only some predicates reach
      'foo::bar', 'foo:bar', 'history.push', 'router.navigate', 'Route.navigate',
      'module.vpc:output.id', 'module.x:file', 'App\\Ctrl', 'a.b', 'x#y',
      'cics-transid:AB12', 'name:prefix', 'x.urls', 'NextResponse.redirect',
      'this.foo', 'foo.push',
      // edge shapes — near-misses and the empty string
      '', 'push2', 'navigates', 'goto2', 'hook_', 'Controller@', '@bar',
    ];
    for (const resolver of getAllFrameworkResolvers()) {
      for (const name of names) {
        const expected = resolver.claimsReference?.(name) ?? false;
        expect(
          kernel!.frameworkClaimsName!(resolver.name, name),
          `${resolver.name}.claimsReference(${JSON.stringify(name)})`,
        ).toBe(expected);
      }
    }
    // An unregistered name (custom registerFrameworkResolver) claims
    // conservatively — a passthrough is always safe.
    expect(kernel!.frameworkClaimsName!('not-a-framework', 'helper')).toBe(true);
  });

  it('settles unclaimed prefilter misses natively under active frameworks', async () => {
    const kernel = getKernel();
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-kresolve-'));
    for (const [rel, content] of Object.entries(FIXTURE)) {
      fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tempDir, rel), content);
    }
    cg = await CodeGraph.init(tempDir, { index: true });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
    const runFn = cg.getNodesByKind('function').find((n) => n.name === 'run')!.id;
    db.prepare(
      "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, file_path, language, status) VALUES (?,?,?,?,?,?,?,'pending')",
    ).run(runFn, 'neverDeclared', 'calls', 5, 2, 'src/main.ts', 'typescript');
    db.prepare(
      "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, file_path, language, status) VALUES (?,?,?,?,?,?,?,'pending')",
    ).run(runFn, 'navigate', 'calls', 6, 2, 'src/main.ts', 'typescript');

    const config = {
      dbPath: path.join(tempDir, '.codegraph', 'codegraph.db'),
      projectRoot: tempDir,
      cppIncludeDirs: [],
      nodeBuiltinSpecifiers: [...builtinModules],
      frameworksActive: true,
    };
    const readAndSettle = (extra: Record<string, unknown>) => {
      const resolver = new kernel!.KernelResolver!({ ...config, ...extra });
      const batch = resolver.readPendingBatch(0, 200, false);
      const out = new Map(
        resolver.resolveChunk(batch).map((o, i) => [batch[i]!.referenceName, o.status]),
      );
      resolver.close();
      return out;
    };

    // express claims nothing — an unclaimed miss settles as a terminal
    // unresolved instead of riding the full TS dispatch.
    expect(readAndSettle({ frameworkNames: ['express'] }).get('neverDeclared')).toBe('unresolved');
    // react-router's NAV_CALL claims `navigate` — it must still passthrough.
    expect(readAndSettle({ frameworkNames: ['react-router'] }).get('navigate')).toBe('passthrough');
    // A config without frameworkNames keeps the legacy claim-everything gate.
    expect(readAndSettle({}).get('neverDeclared')).toBe('passthrough');
  });
});
