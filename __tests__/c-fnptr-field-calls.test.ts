/**
 * Callable struct/union members — `int (*fp)(int)` and function-pointer
 * typedef members — are `field` nodes qualified `Type::member`, and member
 * calls through them (`rtc->read(...)`, `x.fp(...)`) resolve to those
 * fields. Extraction lives in the kernel's C/C++ walker; resolution rides
 * the bound-receiver arm: a declared receiver type pins `Type::member`,
 * and an unrecoverable receiver falls back to a globally unique
 * same-language field name (never ambiguous).
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { tryKernelExtract } from '../src/extraction/kernel';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

let tempDir: string;
let cg: CodeGraph | null = null;

async function project(files: Record<string, string>): Promise<CodeGraph> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-fnptr-field-'));
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), content);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  cg.resolveReferences();
  return cg;
}

function callsFrom(graph: CodeGraph, caller: string): Array<{ name: string; kind: string; qname: string; by?: string }> {
  const from = graph.getNodesByKind('function').find((n) => n.name === caller)!;
  expect(from, caller).toBeDefined();
  return graph.getOutgoingEdges(from.id).filter((e) => e.kind === 'calls').map((e) => {
    const t = graph.getNode(e.target)!;
    return { name: t.name, kind: t.kind, qname: t.qualifiedName, by: (e.metadata as { resolvedBy?: string } | undefined)?.resolvedBy };
  });
}

function fieldNodes(graph: CodeGraph): Array<{ name: string; qname: string }> {
  return graph.getNodesByKind('field').map((n) => ({ name: n.name, qname: n.qualifiedName }));
}

afterEach(() => {
  cg?.close();
  cg = null;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
});

describe.skipIf(!kernelBuilt)('C fn-pointer field nodes', () => {
  const SRC = [
    'typedef int (*hook_func)(int);',
    'typedef void cb_t(void);',
    '',
    'struct ops {',
    '    int scalar, *data, arr[4];',
    '    unsigned flag:1;',
    '    ssize_t (*read)(struct ops *, char *, long);',
    '    int (*fp)(int);',
    '    hook_func hook;',
    '    cb_t *cbp;',
    '    void (*a_fp)(int), (*b_fp)(char);',
    '    int (*arr_fp[4])(int);',
    '};',
    '',
    'static int read_impl(struct ops *o, char *b, long n) { return 0; }',
    '',
    'int probe(struct ops *rtc) {',
    '    return rtc->read(rtc, 0, 0) + rtc->fp(1);',
    '}',
  ].join('\n');

  it('emits field nodes for direct, typedef, and multi-declarator fn-pointer members', () => {
    const nodes = tryKernelExtract('t.c', SRC, 'c')?.nodes ?? [];
    const fields = nodes.filter((n) => n.kind === 'field').map((n) => n.qualifiedName);
    expect(fields).toEqual(expect.arrayContaining([
      'ops::read', 'ops::fp', 'ops::hook', 'ops::cbp', 'ops::a_fp', 'ops::b_fp',
    ]));
  });

  it('does not emit scalar, pointer, array, or bitfield members as fields', () => {
    const nodes = tryKernelExtract('t.c', SRC, 'c')?.nodes ?? [];
    const fields = nodes.filter((n) => n.kind === 'field').map((n) => n.qualifiedName);
    for (const excluded of ['ops::scalar', 'ops::data', 'ops::arr', 'ops::flag', 'ops::arr_fp']) {
      expect(fields).not.toContain(excluded);
    }
  });

  it('resolves `recv->field(...)` on a declared parameter type to Type::field', async () => {
    const graph = await project({ 'main.c': SRC });
    const calls = callsFrom(graph, 'probe');
    expect(calls).toEqual(expect.arrayContaining([
      { name: 'read', kind: 'field', qname: 'ops::read', by: 'field-call' },
      { name: 'fp', kind: 'field', qname: 'ops::fp', by: 'field-call' },
    ]));
  });

  it('resolves `x.field(...)` dot syntax and local-variable receivers', async () => {
    const graph = await project({
      'main.c': [
        'struct disp { void (*draw)(int); };',
        'static void draw_impl(int x) {}',
        '',
        'void run(void) {',
        '    struct disp d;',
        '    struct disp *p = &d;',
        '    d.draw(1);',
        '    p->draw(2);',
        '}',
      ].join('\n'),
    });
    const calls = callsFrom(graph, 'run');
    expect(calls.filter((c) => c.name === 'draw')).toEqual([
      { name: 'draw', kind: 'field', qname: 'disp::draw', by: 'field-call' },
      { name: 'draw', kind: 'field', qname: 'disp::draw', by: 'field-call' },
    ]);
  });

  it('unique-field fallback resolves an un-typed receiver only when unambiguous', async () => {
    const graph = await project({
      'a.c': [
        'struct lone { int (*uncommon_op)(void); };',
        'struct other { int (*common_op)(void); };',
        'struct third { int (*common_op)(void); };',
        '',
        'void caller(void) {',
        '    unknown1->uncommon_op();',
        '    unknown2->common_op();',
        '}',
      ].join('\n'),
    });
    const calls = callsFrom(graph, 'caller');
    expect(calls).toEqual([
      { name: 'uncommon_op', kind: 'field', qname: 'lone::uncommon_op', by: 'field-call' },
    ]);
  });

  it('a bare call sharing a field name never resolves to the field', async () => {
    // Linux-scale regression guard: `bind(fd, …)` is libc, not
    // `sock_ops::bind` — a receiver-less call cannot name a member.
    const graph = await project({
      'main.c': [
        'struct sock_ops { int (*bind)(int); int (*close)(void); };',
        '',
        'void run(int fd) {',
        '    bind(fd);',
        '    close();',
        '}',
      ].join('\n'),
    });
    expect(fieldNodes(graph).map((f) => f.qname)).toEqual(
      expect.arrayContaining(['sock_ops::bind', 'sock_ops::close'])
    );
    expect(callsFrom(graph, 'run')).toEqual([]);
  });

  it('nested anonymous aggregates qualify under the outer struct', () => {
    const src = [
      'struct outer {',
      '    union { int a; void (*u_fp)(int); } u;',
      '    struct { int x; void (*n_fp)(int); } anon;',
      '};',
    ].join('\n');
    const nodes = tryKernelExtract('t.c', src, 'c')?.nodes ?? [];
    const fields = nodes.filter((n) => n.kind === 'field').map((n) => n.qualifiedName);
    expect(fields).toEqual(expect.arrayContaining([
      'outer::<anonymous>::u_fp', 'outer::<anonymous>::n_fp',
    ]));
  });
});

describe.skipIf(!kernelBuilt)('C++ fn-pointer field calls', () => {
  it('methods stay methods; fn-pointer members become fields and resolve', async () => {
    const graph = await project({
      'main.cpp': [
        'struct K {',
        '    virtual int m() = 0;',
        '    int (*fp)(int);',
        '    void method();',
        '};',
        'struct L { int (*cb)(int); };',
        '',
        'void K::method() {}',
        '',
        'void run() {',
        '    L l;',
        '    l.cb(1);',
        '}',
      ].join('\n'),
    });
    const fields = fieldNodes(graph).map((f) => f.qname);
    expect(fields).toContain('K::fp');
    expect(fields).toContain('L::cb');
    expect(fields).not.toContain('K::m');
    expect(fields).not.toContain('K::method');
    expect(callsFrom(graph, 'run')).toEqual([
      { name: 'cb', kind: 'field', qname: 'L::cb', by: 'field-call' },
    ]);
  });

  it('implicit-this member calls inside a method still reach the field', async () => {
    // The extractor drops `this`, so `this->fp(...)` / `fp(...)` inside a
    // member arrive as bare names — the one case a bare call may be a field.
    const graph = await project({
      'main.cpp': [
        'struct K { int (*fp)(int); void go(); };',
        'struct Other { int (*fp)(int); };',
        '',
        'void K::go() {',
        '    this->fp(1);',
        '    fp(2);',
        '}',
      ].join('\n'),
    });
    const go = graph.getNodesByKind('method').find((n) => n.name === 'go')!;
    expect(go).toBeDefined();
    const calls = graph
      .getOutgoingEdges(go.id)
      .filter((e) => e.kind === 'calls')
      .map((e) => graph.getNode(e.target)!)
      .filter((t) => t.name === 'fp');
    // Both calls land on K::fp (the enclosing type's own field), never
    // Other::fp — two edges when both refs resolve, one if `this->` is kept.
    expect(calls.length).toBeGreaterThanOrEqual(1);
    for (const t of calls) {
      expect(t.qualifiedName).toBe('K::fp');
      expect(t.kind).toBe('field');
    }
  });
});
