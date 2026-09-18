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
  // Phase 5 member arms: a class import whose members resolve through
  // br:import → resolveViaImport member descent (staticMember for `create`),
  // a const carrying an object literal (objectLiteralMember for `getState`),
  // and an unbound receiver the boundReceiver arm refuses terminally.
  'src/svc.ts': [
    'export class Service {',
    '  static create(): Service { return new Service(); }',
    '  run() { return 1; }',
    '  split(s: string): string { return s; }',
    '}',
    'export const api = { getState() { return {}; }, call() { return 1; } };',
  ].join('\n'),
  'src/main.ts': [
    "import { helper, VERSION } from './util';",
    "import { renamed } from './barrel';",
    "import { helper as aliased } from '@lib/util';",
    "import { Service, api } from './svc';",
    'export function run() {',
    '  helper();',
    '  renamed();',
    '  aliased();',
    '  const v = VERSION;',
    '  const d = new Date();',
    '  console.log(d, v);',
    '  Service.create();',
    '  api.getState();',
    '  unbound.doThing();',
    '  const svc = new Service();',
    '  svc.run();',
    '  const made = Service.create();',
    '  made.run();',
    '  svc.call();',
    '}',
  ].join('\n'),
  'src/other.ts': 'export function unrelated() { return 0; }\n',
  // Two same-named definitions in one file: the same-file overload arm.
  'src/dup.c': 'void dup(void) {}\nvoid dup(void) {}\nvoid registrar(void) {}\n',
  // C include-path refs (the Phase-5 non-bare `imports` arm): sibling hit,
  // subdir hit, in-repo miss, and a stdlib header that must stay failed.
  'src/hdr/util.h': 'int shared_util(int x);\n',
  'src/local.h': 'int local_fn(void);\n',
  'src/user.c': [
    '#include "local.h"',
    '#include "hdr/util.h"',
    '#include "missing/none.h"',
    '#include <stdio.h>',
    'int caller(void) { return local_fn() + shared_util(1); }',
  ].join('\n'),
  'src/K.java': [
    'class K { void mymethod() {} }',
    'class J { private K k = new K(); void user() { k.mymethod(); } }',
    // dottedChain: `J2.getK().mymethod` — the inner factory's declared return
    // type K is the receiver's type.
    'class J2 { static K getK() { return new K(); } void user2() { J2.getK().mymethod(); } }',
  ].join('\n'),
  // scopedChain: `Registry::make().name` — `: self` marks the factory's own
  // class as the receiver's type.
  'src/reg.php': [
    '<?php',
    'class Registry {',
    '    public static function make(): self { return new self(); }',
    '    public function name() { return "x"; }',
    '}',
    'function reguser() { Registry::make()->name(); }',
  ].join('\n'),
  // Stage-2 member inference: `svc := NewService()` exercises the Go factory
  // arm (callee return type → owner → member); `o.in.Do()` the two-hop field
  // chain (param type → field type → method).
  'main.go': [
    'package main',
    '',
    'type Service struct{}',
    '',
    'func NewService() *Service { return &Service{} }',
    'func (s *Service) Run() {}',
    '',
    'type Inner struct{}',
    'func (i *Inner) Do() {}',
    'type Outer struct{ in *Inner }',
    '',
    'func use(o *Outer) { o.in.Do() }',
    'func run() {',
    '\tsvc := NewService()',
    '\tsvc.Run()',
    '}',
  ].join('\n'),
  // C++ is bareFnOnly: a bare identifier there is never a method value.
  // `Outer::Sub::m` gives the qualified-name partial arm a suffix target.
  // `Pool::instance().drain` exercises cppChain — the `::` callee's recorded
  // return type is the receiver's type.
  'src/w.cpp': [
    'struct W { static void m() {} };',
    'struct Outer { struct Sub { static void m() {} }; };',
    'void wuser() { W::m(); }',
    'struct Pool { static Pool* instance() { static Pool p; return &p; } void drain() {} };',
    'void pooluser() { Pool::instance()->drain(); }',
  ].join('\n'),
  // Unbound member-call arms (matchMethodCall requireReceiverEvidence=false):
  // `this.mailer` skips the claim gate (`this.` root) → mc-thisfield → the
  // field's declared type off the enclosing class → rmot. The remaining arms
  // are pinned through `references`-kind seeds (kind-gated out of the claim,
  // still reaching matchReference's methodCall arm in TS) or through names
  // with no binding at all.
  'src/notify.ts': [
    "import { Service } from './svc';",
    'export class Mailer { send(): void {} }',
    'export class Notifier {',
    '  private mailer: Mailer = new Mailer();',
    '  ping(): void { this.mailer.send(); }',
    '}',
    'export const api2 = { localcall() { return 2; } };',
    'export class Engine { start(): void {} }',
    'export class Alpha { handle(): void {} }',
    'export class Beta { handle(): void {} }',
    'export class Sigma { frobnicate(): void {} }',
    'export function use4(): void {',
    '  const svc4 = new Service();',
    '  svc4.run();',
    '  const arr = new Array<string>();',
    "  arr.split('');",
    '}',
    'export async function top(): Promise<void> {',
    '  const adat2 = await unknownFactory();',
    '  adat2.run();',
    '}',
  ].join('\n'),
  'tool.py': 'def pyhelper():\n    return 1\n\n\nclass Widget:\n    pass\n',
  // `import tool` + `tool.pyhelper()` exercises the python module-member arm.
  'main.py': 'import tool\nfrom tool import pyhelper\n\ndef go():\n    pyhelper()\n    tool.pyhelper()\n',
  // Rust module-path refs — pure `::` names take the native
  // resolveRustPathReference arm ahead of the ineligible:lang punt. `lib.rs`
  // marks the crate root; `deep/mod.rs` exercises the `<seg>/mod.rs` form;
  // `Widget` is a struct (not a module) so `Widget::new` misses the arm.
  'src/lib.rs': 'fn libuser() {}\n',
  'src/sub.rs': [
    'pub fn leaf_fn() {}',
    'pub struct Widget;',
    'impl Widget { pub fn new() {} }',
  ].join('\n'),
  'src/deep/mod.rs': 'pub mod inner;\npub mod sib;\n',
  'src/deep/inner.rs': 'pub fn deep_fn() {}\nfn deepuser() {}\n',
  'src/deep/sib.rs': 'pub fn sib_fn() {}\n',
  'src/user.rs': 'fn user() {}\n',
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
    // console.log is receiver-shaped but nothing declares/imports `console`
    // or `log` — the prefilter misses and the store-binding arm is dead for
    // dotted names, so the kernel verdicts terminal unresolved (TS's
    // applyResolveTail stamps unknown-receiver the same way).
    expect(outcomes[byName.get('console.log@calls')!]!.status).toBe('unresolved');
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

  it('resolves bare function_ref refs on the dedicated native arm', async () => {
    const kernel = getKernel();
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-kresolve-'));
    for (const [rel, content] of Object.entries(FIXTURE)) {
      fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tempDir, rel), content);
    }
    cg = await CodeGraph.init(tempDir, { index: true });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
    const byName = (name: string, kind = 'function') =>
      cg!.getNodesByKind(kind).filter((n) => n.name === name);
    const nodeId = (name: string, file: string, kind = 'function') =>
      byName(name, kind).find((n) => n.filePath.endsWith(file))!.id;
    const ins = db.prepare(
      "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, file_path, language, status) VALUES (?,?,?,?,?,?,?,'pending')",
    );
    const seed = (from: string, name: string, file: string, lang: string, kind = 'function_ref', line = 1) =>
      ins.run(from, name, kind, line, 0, file, lang);

    const runFn = nodeId('run', 'main.ts');
    const goFn = nodeId('go', 'main.py');
    seed(nodeId('unrelated', 'other.ts'), 'helper', 'src/other.ts', 'typescript');
    seed(runFn, 'helper', 'src/main.ts', 'typescript');
    seed(runFn, 'VERSION', 'src/main.ts', 'typescript');
    seed(nodeId('registrar', 'dup.c'), 'dup', 'src/dup.c', 'c');
    seed(nodeId('user', 'K.java', 'method'), 'mymethod', 'src/K.java', 'java');
    seed(nodeId('wuser', 'w.cpp'), 'm', 'src/w.cpp', 'cpp');
    seed(goFn, 'Widget', 'main.py', 'python');
    seed(goFn, 'pyhelper', 'main.py', 'python');
    seed(runFn, 'this.cb', 'src/main.ts', 'typescript');
    seed(nodeId('wuser', 'w.cpp'), 'W::m', 'src/w.cpp', 'cpp');
    seed(runFn, 'neverDeclared', 'src/main.ts', 'typescript');
    // Phase 5 non-bare c/cpp `imports` arm — the #include-path slice.
    const cUser = nodeId('caller', 'user.c');
    seed(cUser, 'local.h', 'src/user.c', 'c', 'imports');
    seed(cUser, 'hdr/util.h', 'src/user.c', 'c', 'imports');
    seed(cUser, 'util.h', 'src/user.c', 'c', 'imports');
    seed(cUser, 'missing/none.h', 'src/user.c', 'c', 'imports');
    seed(cUser, 'stdio.h', 'src/user.c', 'c', 'imports');
    seed(nodeId('wuser', 'w.cpp'), 'hdr/util.h', 'src/w.cpp', 'cpp', 'imports');
    // Phase 5 member arms — calls-kind refs through the native spine. Lines
    // sit inside run()/go() so the import bindings' scopes cover them.
    seed(runFn, 'Service.create', 'src/main.ts', 'typescript', 'calls', 10);
    seed(runFn, 'api.getState', 'src/main.ts', 'typescript', 'calls', 11);
    seed(runFn, 'Service.deep.create', 'src/main.ts', 'typescript', 'calls', 12);
    seed(runFn, 'unbound.doThing', 'src/main.ts', 'typescript', 'calls', 13);
    seed(goFn, 'tool.pyhelper', 'main.py', 'python', 'calls', 5);
    seed(goFn, 'tool.missing', 'main.py', 'python', 'calls', 6);
    seed(nodeId('wuser', 'w.cpp'), 'W::m', 'src/w.cpp', 'cpp', 'calls');
    seed(nodeId('wuser', 'w.cpp'), 'Sub::m', 'src/w.cpp', 'cpp', 'calls');
    seed(nodeId('wuser', 'w.cpp'), 'W::nope', 'src/w.cpp', 'cpp', 'calls');
    // Stage 2 — source-backed member inference. `svc = new Service()` hits
    // mc-infer → bound-type-method; `made = Service.create()` falls through
    // inference to the ESM factory tail (which bails on the NULL return_type
    // like TS); `svc.call` passes the prefilter (`call` is a known node) then
    // misses the member on the inferred owner — the btm supers path reads
    // live edges, so the kernel punts to TS.
    seed(runFn, 'svc.run', 'src/main.ts', 'typescript', 'calls', 16);
    seed(runFn, 'made.run', 'src/main.ts', 'typescript', 'calls', 18);
    seed(runFn, 'svc.call', 'src/main.ts', 'typescript', 'calls', 19);
    // Java field receiver — `private K k = new K()` inside class J.
    seed(nodeId('user', 'K.java', 'method'), 'k.mymethod', 'src/K.java', 'java', 'calls', 2);
    // Go factory receiver + two-hop field chain.
    const goRun = nodeId('run', 'main.go');
    seed(goRun, 'svc.Run', 'main.go', 'go', 'calls', 15);
    seed(nodeId('use', 'main.go'), 'o.in.Do', 'main.go', 'go', 'calls', 12);
    // Chain arms — `<inner>().<method>` refs whose receiver's type is the
    // inner call's declared return type.
    seed(nodeId('pooluser', 'w.cpp'), 'Pool::instance().drain', 'src/w.cpp', 'cpp', 'calls', 5);
    seed(nodeId('pooluser', 'w.cpp'), 'Pool::instance().nope', 'src/w.cpp', 'cpp', 'calls', 5);
    seed(nodeId('reguser', 'reg.php'), 'Registry::make().name', 'src/reg.php', 'php', 'calls', 6);
    seed(goRun, 'NewService().Run', 'main.go', 'go', 'calls', 16);
    seed(goRun, 'nosuch().Run', 'main.go', 'go', 'calls', 17);
    seed(nodeId('user2', 'K.java', 'method'), 'J2.getK().mymethod', 'src/K.java', 'java', 'calls', 3);
    // Unbound member-call arm (requireReceiverEvidence=false). `this.` roots
    // skip the claim gate entirely; `references`-kind refs are kind-gated out
    // of it. A calls-kind `x.y` name with no binding is claimed-and-refused
    // terminally in TS (never reaching methodCall) — the strat arms are
    // pinned through `references` seeds, the only shapes that reach them.
    seed(nodeId('ping', 'notify.ts', 'method'), 'this.mailer.send', 'src/notify.ts', 'typescript', 'calls', 5);
    const use4Fn = nodeId('use4', 'notify.ts');
    seed(use4Fn, 'svc4.run', 'src/notify.ts', 'typescript', 'references', 14);
    seed(use4Fn, 'api2.localcall', 'src/notify.ts', 'typescript', 'references', 16);
    seed(use4Fn, 'Engine.start', 'src/notify.ts', 'typescript', 'references', 16);
    seed(use4Fn, 'engine.start', 'src/notify.ts', 'typescript', 'references', 16);
    seed(use4Fn, 'betaThing.handle', 'src/notify.ts', 'typescript', 'references', 16);
    seed(use4Fn, 'mystery.frobnicate', 'src/notify.ts', 'typescript', 'references', 16);
    seed(use4Fn, 'arr.split', 'src/notify.ts', 'typescript', 'references', 16);
    seed(nodeId('top', 'notify.ts'), 'adat2.run', 'src/notify.ts', 'typescript', 'references', 18);
    // Non-bare function_ref: TS's block runs viaImport (member-descent can
    // claim `a.b`) then the `::` member-pointer arm — the only non-bare
    // shape matchFunctionRef resolves.
    const wuserFn = nodeId('wuser', 'w.cpp');
    seed(wuserFn, 'W::m', 'src/w.cpp', 'cpp', 'function_ref', 6);
    seed(wuserFn, 'Pool::missing', 'src/w.cpp', 'cpp', 'function_ref', 6);
    seed(runFn, 'api.call', 'src/main.ts', 'typescript', 'function_ref', 10);
    // Rust `::` module-path refs — pure colon names take the native
    // resolveRustPathReference arm ahead of the ineligible:lang punt:
    // crate/self/super anchors, bare paths (self-relative then crate), and
    // the <seg>/mod.rs module form.
    const libuserFn = nodeId('libuser', 'lib.rs');
    const rustUserFn = nodeId('user', 'user.rs');
    const deepuserFn = nodeId('deepuser', 'inner.rs');
    seed(libuserFn, 'crate::sub::leaf_fn', 'src/lib.rs', 'rust', 'calls', 2);
    seed(libuserFn, 'self::sub::leaf_fn', 'src/lib.rs', 'rust', 'calls', 3);
    seed(libuserFn, 'crate::deep::inner::deep_fn', 'src/lib.rs', 'rust', 'calls', 4);
    seed(rustUserFn, 'sub::leaf_fn', 'src/user.rs', 'rust', 'calls', 2);
    seed(rustUserFn, 'crate::sub::leaf_fn', 'src/user.rs', 'rust', 'calls', 3);
    seed(deepuserFn, 'super::sib::sib_fn', 'src/deep/inner.rs', 'rust', 'calls', 2);
    seed(libuserFn, 'ext::module::leaf_fn', 'src/lib.rs', 'rust', 'calls', 5);
    seed(libuserFn, 'Widget::new', 'src/lib.rs', 'rust', 'calls', 6);
    seed(libuserFn, 'crate::sub::missing', 'src/lib.rs', 'rust', 'calls', 7);
    // `::` AND `.` — the boundReceiver claim can own an `a::b.c` receiver in
    // TS, so the arm leaves it punted.
    seed(libuserFn, 'a::b.c', 'src/lib.rs', 'rust', 'calls', 8);

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
    const outcomes = resolver.resolveChunk(batch);
    const idx = new Map(
      batch.map((r, i) => [`${r.referenceName}@${r.referenceKind}@${r.filePath}`, i]),
    );
    const at = (name: string, file: string, kind = 'function_ref') =>
      outcomes[idx.get(`${name}@${kind}@${file}`)!]!;
    // `imports` refs are prerequisite-kind rows — the orchestrator reads them
    // in the first pass (`prerequisites: true`).
    const preBatch = resolver.readPendingBatch(0, 200, true);
    const preOutcomes = resolver.resolveChunk(preBatch);
    const preIdx = new Map(
      preBatch.map((r, i) => [`${r.referenceName}@${r.referenceKind}@${r.filePath}`, i]),
    );
    const atPre = (name: string, file: string) =>
      preOutcomes[preIdx.get(`${name}@imports@${file}`)!]!;

    // Cross-file unique match → 'function-ref' at 0.8 (no import in other.ts).
    const cross = at('helper', 'src/other.ts');
    expect(cross.status).toBe('resolved');
    expect(cross.resolvedBy).toBe('function-ref');
    expect(cross.confidence).toBe(0.8);
    expect(cross.targetNodeId).toBe(nodeId('helper', 'util.ts'));
    // The import binding wins first — 'import' at 0.9, matching the TS spine.
    const viaImport = at('helper', 'src/main.ts');
    expect(viaImport.status).toBe('resolved');
    expect(viaImport.resolvedBy).toBe('import');
    // An import resolving to a non-callable is kind-gated out and discarded —
    // the name matcher then has no function/method candidates either.
    expect(at('VERSION', 'src/main.ts').status).toBe('unresolved');
    // Same-file overloads: earliest definition, 0.9.
    const dup = at('dup', 'src/dup.c');
    expect(dup.status).toBe('resolved');
    expect(dup.confidence).toBe(0.9);
    expect(dup.targetNodeId).toBe(byName('dup').sort((a, b) => a.startLine - b.startLine)[0]!.id);
    // Java is not bareFnOnly — a bare name may be a method value.
    expect(at('mymethod', 'src/K.java').status).toBe('resolved');
    // C++ is bareFnOnly — the only 'm' is a method, so no candidate.
    expect(at('m', 'src/w.cpp').status).toBe('unresolved');
    // Python bareClassOk — a class is a value.
    const widget = at('Widget', 'main.py');
    expect(widget.status).toBe('resolved');
    expect(widget.targetNodeId).toBe(nodeId('Widget', 'tool.py', 'class'));
    // Imported callback → 'import'.
    expect(at('pyhelper', 'main.py').resolvedBy).toBe('import');
    // Non-bare shapes with no declared segment die at the prefilter — the
    // store-binding arm is dead for dotted names, so TS fails it identically.
    expect(at('this.cb', 'src/main.ts').status).toBe('unresolved');
    // `W::m` — the `::` member-pointer arm resolves it natively (pinned in
    // detail below with the rest of the non-bare function_ref block).
    expect(at('W::m', 'src/w.cpp').status).toBe('resolved');
    // No node, no import — terminal miss.
    expect(at('neverDeclared', 'src/main.ts').status).toBe('unresolved');
    // Quoted include with a same-dir sibling → 'import' at 0.92.
    const localInc = atPre('local.h', 'src/user.c');
    expect(localInc.status).toBe('resolved');
    expect(localInc.resolvedBy).toBe('import');
    expect(localInc.confidence).toBe(0.92);
    expect(localInc.targetNodeId).toBe(nodeId('local.h', 'src/local.h', 'file'));
    // Subdir sibling and the cpp variant take the same arm.
    const hdrInc = atPre('hdr/util.h', 'src/user.c');
    expect(hdrInc.status).toBe('resolved');
    expect(hdrInc.resolvedBy).toBe('import');
    expect(hdrInc.targetNodeId).toBe(nodeId('util.h', 'src/hdr/util.h', 'file'));
    const cppInc = atPre('hdr/util.h', 'src/w.cpp');
    expect(cppInc.status).toBe('resolved');
    expect(cppInc.resolvedBy).toBe('import');
    expect(cppInc.targetNodeId).toBe(hdrInc.targetNodeId);
    // No sibling and no include dirs → matchByFilePath suffix hit at 0.85.
    const pathInc = atPre('util.h', 'src/user.c');
    expect(pathInc.status).toBe('resolved');
    expect(pathInc.resolvedBy).toBe('file-path');
    expect(pathInc.confidence).toBe(0.85);
    expect(pathInc.targetNodeId).toBe(hdrInc.targetNodeId);
    // A genuinely missing include can't resolve to a file — but its name IS
    // the qualified name of its own import node, so the qualifiedName arm
    // (matchReference's filePath→qualifiedName order, now mirrored in the
    // kernel's include arm) resolves it there at 0.95.
    const noneInc = atPre('missing/none.h', 'src/user.c');
    expect(noneInc.status).toBe('resolved');
    expect(noneInc.resolvedBy).toBe('qualified-name');
    expect(noneInc.confidence).toBe(0.95);
    expect(noneInc.targetNodeId).toBe(nodeId('missing/none.h', 'user.c', 'import'));
    // A stdlib header: isExternalImport sits inside resolveImportPath, so the
    // viaImport arm misses — qualifiedName then binds the ref to the `stdio.h`
    // import node, exactly as TS does.
    const stdioInc = atPre('stdio.h', 'src/user.c');
    expect(stdioInc.status).toBe('resolved');
    expect(stdioInc.resolvedBy).toBe('qualified-name');
    expect(stdioInc.targetNodeId).toBe(nodeId('stdio.h', 'user.c', 'import'));

    // ---- Phase 5 member arms (non-bare `calls` refs) ----
    // br:import — `Service.create` descends the import binding's member:
    // findExportedSymbol(Service class) → staticMember → `Service::create`.
    const svcCreate = at('Service.create', 'src/main.ts', 'calls');
    expect(svcCreate.status).toBe('resolved');
    expect(svcCreate.resolvedBy).toBe('import');
    expect(svcCreate.confidence).toBe(0.9);
    expect(svcCreate.targetNodeId).toBe(
      byName('create', 'method').find((n) => n.qualifiedName === 'Service::create')!.id,
    );
    // `api.getState` — objectLiteralMember inside the const's extent.
    const apiGet = at('api.getState', 'src/main.ts', 'calls');
    expect(apiGet.status).toBe('resolved');
    expect(apiGet.resolvedBy).toBe('import');
    // A deeper receiver on an import binding is refused — never name-guessed.
    expect(at('Service.deep.create', 'src/main.ts', 'calls').status).toBe('unresolved');
    // No binding at all → the same terminal refusal.
    expect(at('unbound.doThing', 'src/main.ts', 'calls').status).toBe('unresolved');
    // Python module-member: `tool.pyhelper` → tool.py's pyhelper @0.85.
    const toolPy = at('tool.pyhelper', 'main.py', 'calls');
    expect(toolPy.status).toBe('resolved');
    expect(toolPy.resolvedBy).toBe('import');
    expect(toolPy.confidence).toBe(0.85);
    expect(toolPy.targetNodeId).toBe(nodeId('pyhelper', 'tool.py'));
    // A member miss on the module → refused, not name-matched.
    expect(at('tool.missing', 'main.py', 'calls').status).toBe('unresolved');
    // qualifiedName: exact `W::m` @0.95; `Sub::m` partial-matches the
    // `Outer::Sub::m` qualified name at 0.85.
    const wm = at('W::m', 'src/w.cpp', 'calls');
    expect(wm.status).toBe('resolved');
    expect(wm.resolvedBy).toBe('qualified-name');
    expect(wm.confidence).toBe(0.95);
    expect(wm.targetNodeId).toBe(
      byName('m', 'method').find((n) => n.qualifiedName === 'W::m')!.id,
    );
    const subM = at('Sub::m', 'src/w.cpp', 'calls');
    expect(subM.status).toBe('resolved');
    expect(subM.resolvedBy).toBe('qualified-name');
    expect(subM.confidence).toBe(0.85);
    expect(subM.targetNodeId).toBe(
      byName('m', 'method').find((n) => n.qualifiedName === 'Outer::Sub::m')!.id,
    );
    // `W::nope` passes the prefilter (`W` is a known segment) but misses every
    // ported arm — the member tail goes back to TS.
    expect(at('W::nope', 'src/w.cpp', 'calls').status).toBe('passthrough');

    // ---- Stage 2 — source-backed member inference ----
    // mc-infer-local → matchBoundTypeMember: `svc = new Service()` then
    // `svc.run()` → `Service::run` @0.9.
    const svcRun = at('svc.run', 'src/main.ts', 'calls');
    expect(svcRun.status).toBe('resolved');
    expect(svcRun.resolvedBy).toBe('instance-method');
    expect(svcRun.confidence).toBe(0.9);
    expect(svcRun.targetNodeId).toBe(
      byName('run', 'method').find((n) => n.qualifiedName === 'Service::run')!.id,
    );
    // br:factory — `made = Service.create()` ends in a factory call, but the
    // TS extractor leaves `return_type` NULL (the `: Service` lives only in
    // `signature`), so the tail bails exactly like TS and the ref stays
    // unresolved.
    expect(at('made.run', 'src/main.ts', 'calls').status).toBe('unresolved');
    // `svc.call` infers Service then misses `Service::call` — the supertype
    // walk reads live edges, so the kernel punts for TS to decide.
    expect(at('svc.call', 'src/main.ts', 'calls').status).toBe('passthrough');
    // Java field receiver — `private K k = new K()` infers K → `K::mymethod`.
    const km = at('k.mymethod', 'src/K.java', 'calls');
    expect(km.status).toBe('resolved');
    expect(km.resolvedBy).toBe('instance-method');
    expect(km.targetNodeId).toBe(
      byName('mymethod', 'method').find((n) => n.qualifiedName === 'K::mymethod')!.id,
    );
    // Go factory — `svc := NewService()` → callee return type `*Service` →
    // matchBoundTypeMember on `Service::Run` @0.9.
    const goSvc = at('svc.Run', 'main.go', 'calls');
    expect(goSvc.status).toBe('resolved');
    expect(goSvc.resolvedBy).toBe('instance-method');
    expect(goSvc.confidence).toBe(0.9);
    // Go two-hop field chain — `o *Outer` → `in *Inner` → `Inner::Do`.
    const goChain = at('o.in.Do', 'main.go', 'calls');
    expect(goChain.status).toBe('resolved');
    expect(goChain.resolvedBy).toBe('instance-method');
    expect(goChain.confidence).toBe(0.85);

    // ---- Chain arms — `<inner>().<method>` receiver-type chains ----
    // cppChain: `Pool::instance` returns `Pool` (ptr stripped at extraction)
    // → `Pool::drain` @0.85.
    const cppChain = at('Pool::instance().drain', 'src/w.cpp', 'calls');
    expect(cppChain.status).toBe('resolved');
    expect(cppChain.resolvedBy).toBe('instance-method');
    expect(cppChain.confidence).toBe(0.85);
    expect(cppChain.targetNodeId).toBe(
      byName('drain', 'method').find((n) => n.qualifiedName === 'Pool::drain')!.id,
    );
    // `Pool::instance().nope` — the owner resolves but no `Pool::nope`
    // exists; rmot's supertype walk reads live edges, so the kernel punts.
    expect(at('Pool::instance().nope', 'src/w.cpp', 'calls').status).toBe('passthrough');
    // scopedChain: `Registry::make` returns `self` → the factory's own class
    // → `Registry::name` @0.85.
    const phpChain = at('Registry::make().name', 'src/reg.php', 'calls');
    expect(phpChain.status).toBe('resolved');
    expect(phpChain.resolvedBy).toBe('instance-method');
    expect(phpChain.confidence).toBe(0.85);
    expect(phpChain.targetNodeId).toBe(
      byName('name', 'method').find((n) => n.qualifiedName === 'Registry::name')!.id,
    );
    // dottedChain (go): bare `NewService()` returns `Service` → `Service::Run`.
    const goDot = at('NewService().Run', 'main.go', 'calls');
    expect(goDot.status).toBe('resolved');
    expect(goDot.resolvedBy).toBe('instance-method');
    expect(goDot.confidence).toBe(0.85);
    // dottedChain go bare-fallback (exactName/fuzzy on the method) is
    // unported — the member-tail punt hands it to TS.
    expect(at('nosuch().Run', 'main.go', 'calls').status).toBe('passthrough');
    // dottedChain (java): `J2.getK` returns `K` → `K::mymethod`.
    const javaDot = at('J2.getK().mymethod', 'src/K.java', 'calls');
    expect(javaDot.status).toBe('resolved');
    expect(javaDot.resolvedBy).toBe('instance-method');
    expect(javaDot.confidence).toBe(0.85);
    expect(javaDot.targetNodeId).toBe(
      byName('mymethod', 'method').find((n) => n.qualifiedName === 'K::mymethod')!.id,
    );

    // ---- Unbound member-call arm (requireReceiverEvidence=false) ----
    // mc-thisfield: `this.mailer.send` skips the claim (`this.` root) → the
    // field's declared `Mailer` type off class Notifier → `Mailer::send` @0.85.
    const thisField = at('this.mailer.send', 'src/notify.ts', 'calls');
    expect(thisField.status).toBe('resolved');
    expect(thisField.resolvedBy).toBe('instance-method');
    expect(thisField.confidence).toBe(0.85);
    expect(thisField.targetNodeId).toBe(
      byName('send', 'method').find((n) => n.qualifiedName === 'Mailer::send')!.id,
    );
    // Unbound infer → rmot: `const svc4 = new Service()` → `Service::run` @0.9
    // (references-kind keeps the ref out of the bound claim but inside
    // matchReference's methodCall arm, exactly like TS).
    const freeInfer = at('svc4.run', 'src/notify.ts', 'references');
    expect(freeInfer.status).toBe('resolved');
    expect(freeInfer.resolvedBy).toBe('instance-method');
    expect(freeInfer.confidence).toBe(0.9);
    expect(freeInfer.targetNodeId).toBe(
      byName('run', 'method').find((n) => n.qualifiedName === 'Service::run')!.id,
    );
    // mc-literal unbound: every same-file const/variable holder gets the
    // object-literal member scan (no binding filter) → `api2.localcall` @0.85.
    const freeLit = at('api2.localcall', 'src/notify.ts', 'references');
    expect(freeLit.status).toBe('resolved');
    expect(freeLit.resolvedBy).toBe('instance-method');
    expect(freeLit.confidence).toBe(0.85);
    // strat1 — `Engine` names the class; `Engine::start` found by file scan.
    const strat1 = at('Engine.start', 'src/notify.ts', 'references');
    expect(strat1.status).toBe('resolved');
    expect(strat1.resolvedBy).toBe('qualified-name');
    expect(strat1.confidence).toBe(0.85);
    expect(strat1.targetNodeId).toBe(
      byName('start', 'method').find((n) => n.qualifiedName === 'Engine::start')!.id,
    );
    // strat2 — capitalized receiver `engine` → `Engine` class scan @0.8.
    const strat2 = at('engine.start', 'src/notify.ts', 'references');
    expect(strat2.status).toBe('resolved');
    expect(strat2.resolvedBy).toBe('instance-method');
    expect(strat2.confidence).toBe(0.8);
    // strat3 single — the only `frobnicate` method wins outright @0.7.
    const strat3a = at('mystery.frobnicate', 'src/notify.ts', 'references');
    expect(strat3a.status).toBe('resolved');
    expect(strat3a.resolvedBy).toBe('instance-method');
    expect(strat3a.confidence).toBe(0.7);
    // strat3 overlap — `betaThing` shares a word with `Beta` → `Beta::handle`
    // @0.65 (receiver-word overlap 1 + same-language bonus 1 ≥ 2).
    const strat3b = at('betaThing.handle', 'src/notify.ts', 'references');
    expect(strat3b.status).toBe('resolved');
    expect(strat3b.resolvedBy).toBe('instance-method');
    expect(strat3b.confidence).toBe(0.65);
    expect(strat3b.targetNodeId).toBe(
      byName('handle', 'method').find((n) => n.qualifiedName === 'Beta::handle')!.id,
    );
    // mc-await — `const adat2 = await unknownFactory()` may narrow the
    // receiver through inferEsmAwaitedCallType, which the snapshot doesn't
    // run — the gate punts rather than guesses.
    expect(at('adat2.run', 'src/notify.ts', 'references').status).toBe('passthrough');
    // builtin bail — `arr` infers to `Array` (a JS_BUILT_INS member), whose
    // rmot miss returns null in TS rather than letting Strategy 3 guess the
    // unrelated `Service::split` (the bait — @0.7 if the bail is missing).
    expect(at('arr.split', 'src/notify.ts', 'references').status).toBe('passthrough');

    // ---- Non-bare function_ref (`::` member-pointer arm) ----
    // `W::m` — the only scoped match (`Outer::Sub::m` fails the `::W::m`
    // suffix) → same-file earliest-line @0.9.
    const scopedFr = at('W::m', 'src/w.cpp', 'function_ref');
    expect(scopedFr.status).toBe('resolved');
    expect(scopedFr.resolvedBy).toBe('function-ref');
    expect(scopedFr.confidence).toBe(0.9);
    expect(scopedFr.targetNodeId).toBe(
      byName('m', 'method').find((n) => n.qualifiedName === 'W::m')!.id,
    );
    // `Pool::missing` — no `missing` member anywhere → member-tail punt.
    expect(at('Pool::missing', 'src/w.cpp', 'function_ref').status).toBe('passthrough');
    // `api.call` — viaImport's member-descent claims it before the scoped
    // arm: import `api` → svc's `api` const → `call` member @0.9.
    const frImport = at('api.call', 'src/main.ts', 'function_ref');
    expect(frImport.status).toBe('resolved');
    expect(frImport.resolvedBy).toBe('import');
    expect(frImport.confidence).toBe(0.9);

    // ---- Rust `::` module-path refs (ineligible-language arm) ----
    // `crate::sub::leaf_fn` — crate anchor → src/sub.rs → leaf_fn @0.9.
    const crateHit = at('crate::sub::leaf_fn', 'src/lib.rs', 'calls');
    expect(crateHit.status).toBe('resolved');
    expect(crateHit.resolvedBy).toBe('import');
    expect(crateHit.confidence).toBe(0.9);
    expect(crateHit.targetNodeId).toBe(nodeId('leaf_fn', 'sub.rs'));
    // `self::` — lib.rs owns src/ as its module dir → same target.
    expect(at('self::sub::leaf_fn', 'src/lib.rs', 'calls').targetNodeId).toBe(
      nodeId('leaf_fn', 'sub.rs'),
    );
    // Bare path — self-relative misses (src/user/sub.rs) → crate-relative hit.
    expect(at('sub::leaf_fn', 'src/user.rs', 'calls').targetNodeId).toBe(
      nodeId('leaf_fn', 'sub.rs'),
    );
    expect(at('crate::sub::leaf_fn', 'src/user.rs', 'calls').targetNodeId).toBe(
      nodeId('leaf_fn', 'sub.rs'),
    );
    // Multi-segment — `deep` maps to deep/mod.rs, `inner` to deep/inner.rs.
    const deepHit = at('crate::deep::inner::deep_fn', 'src/lib.rs', 'calls');
    expect(deepHit.status).toBe('resolved');
    expect(deepHit.targetNodeId).toBe(nodeId('deep_fn', 'inner.rs'));
    // `super::` — inner.rs's module dir is src/deep/inner; one super →
    // src/deep → sib.rs → sib_fn.
    expect(at('super::sib::sib_fn', 'src/deep/inner.rs', 'calls').targetNodeId).toBe(
      nodeId('sib_fn', 'sib.rs'),
    );
    // External crate — both anchors miss → ineligible:lang punt.
    expect(at('ext::module::leaf_fn', 'src/lib.rs', 'calls').status).toBe('passthrough');
    // `Widget` is a struct, not a module — arm miss → punt (TS's
    // qualifiedName arm owns the same ref downstream).
    expect(at('Widget::new', 'src/lib.rs', 'calls').status).toBe('passthrough');
    // Leaf unknown → prefilter miss → terminal unresolved (store-binding is
    // JS-gated dead for rust).
    expect(at('crate::sub::missing', 'src/lib.rs', 'calls').status).toBe('unresolved');
    // `::`+`.` names stay punted — boundReceiver-claim territory.
    expect(at('a::b.c', 'src/lib.rs', 'calls').status).toBe('passthrough');
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
