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
  // A supertype the kernel's walk can reach once an `extends` edge names it.
  'src/base.ts': 'export class BaseSvc {\n  call() { return 2; }\n}\n',
  // Import-member arms that read the exporting file: an object literal whose
  // members alias bindings (one local, one imported), and a shared instance
  // typed from its own declaration.
  'src/facade.ts': [
    "import { helper } from './util';",
    "import { Service } from './svc';",
    'export function local1() { return 3; }',
    'export const Facade = { helper, go: local1 };',
    'export const shared = new Service();',
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
    // A receiver that STARTS with a non-ASCII letter: JS's `\b` is ASCII, so
    // TS's `\büber\b\s*=\s*new` never matches and inference misses — the
    // kernel's regexes must miss the same way (`(?-u:\b)`), or it would
    // infer `Service` and resolve `run` at 0.9 where TS lands at 0.7.
    '  const über = new Service();',
    '  über.run();',
    '  Facade.helper();',
    '  Facade.go();',
    '  shared.run();',
    '  shared.unrelated();',
    '}',
    // Last, so the seeded lines above stay put; ESM hoists imports anyway.
    "import { Facade, shared } from './facade';",
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
  // `this.<member>` function refs: the class's own member, or — inherited —
  // deferred to the `this.<member>` pass, which walks the extends edge.
  'src/widget.ts': [
    'export class BaseW { onBase() {} }',
    'export class Widget extends BaseW {',
    '  onClick() {}',
    '  wire(el: any) { el.on("a", this.onClick); el.on("b", this.onBase); }',
    '}',
  ].join('\n'),
  // Zustand store actions: destructured, selected, chained off the accessor,
  // and `get()` inside the store — each lands on the store's own `reset`.
  'src/zstore.ts': [
    "import { create } from 'zustand';",
    'export const useStore = create((set, get) => ({',
    '  reset() { set({}); },',
    '  bump() { get().reset(); },',
    '}));',
  ].join('\n'),
  'src/zapp.ts': [
    "import { useStore } from './zstore';",
    'export function zapp() {',
    '  const { reset } = useStore.getState();',
    '  reset();',
    '  const r2 = useStore((s) => s.reset);',
    '  r2();',
    '  useStore.getState().reset();',
    '}',
  ].join('\n'),
  // Pascal: RTL built-ins are external; `lg: TLogger` and
  // `lg2 := TLogger.Create` type their receivers.
  'src/lg.pas': [
    'unit lg;',
    'interface',
    'type',
    '  TLogger = class',
    '    procedure Log;',
    '  end;',
    'implementation',
    'procedure TLogger.Log; begin end;',
    'procedure Run;',
    'var lg: TLogger;',
    'begin',
    '  lg.Log;',
    '  WriteLn(IntToStr(1));',
    'end;',
    'procedure Run2;',
    'var lg2: TObject;',
    'begin',
    '  lg2 := TLogger.Create;',
    '  lg2.Log;',
    'end;',
    'end.',
  ].join('\n'),
  // Erlang: arity is part of a function's identity (`em::f/2`); behaviours
  // and `.app.src` entries name modules only.
  'src/em.erl': [
    '-module(em).',
    '-behaviour(gen_server).',
    '-export([f/1, f/2, g/1, run/0]).',
    'f(X) -> X.',
    'f(X, Y) -> {X, Y}.',
    'g(X) -> X.',
    'run() -> f(1), em:f(1, 2), em:g(3), em:h(4), lists:map(fun g/1, []).',
  ].join('\n'),
  'src/em_user.erl': [
    '-module(em_user).',
    '-behaviour(em).',
    '-export([go/0]).',
    'go() -> em:f(1), em:g(2), apply(em, g, [3]).',
  ].join('\n'),
  'src/em.app.src': '{application, em, [{mod, {em, []}}, {applications, [kernel, em_user]}]}.\n',
  // Import-only languages: a Nix path import, a COBOL copybook and Terraform
  // refs resolve through the import arm (and frameworks) or not at all.
  'nix/mod.nix': '{ imports = [ ./part.nix ./missing.nix ]; y = nixonly 1; }\n',
  'nix/part.nix': '{ z = 2; }\n',
  'cob/PROG.cbl': [
    '       IDENTIFICATION DIVISION.',
    '       PROGRAM-ID. PROG.',
    '       DATA DIVISION.',
    '       WORKING-STORAGE SECTION.',
    '       COPY CUSTREC.',
    '       COPY SQLCA.',
    '       PROCEDURE DIVISION.',
    '           STOP RUN.',
  ].join('\n'),
  'cob/CUSTREC.cpy': '       01 CUST-REC.\n          05 CUST-ID PIC 9(5).\n',
  'tf/main.tf': [
    'variable "region" { default = "us-east-1" }',
    'module "net" { source = "./net" }',
    'resource "aws_s3_bucket" "b" { bucket = var.region }',
    'output "o" { value = module.net.id }',
  ].join('\n'),
  'tf/net/main.tf': 'output "id" { value = "x" }\n',
  // Markdown links resolve to the linked file by path.
  'README.md': '# Fixture\n\nSee [util](src/util.ts).\n',
  // PHP include paths resolve to files only: relative to the including file,
  // `.php` optional, and a miss never name-matches a same-named file.
  'src/inc/db.php': '<?php\nfunction dbconnect() {}\n',
  'src/useinc.php': [
    '<?php',
    "require 'inc/db.php';",
    "require 'inc/db';",
    "require 'nowhere/g.php';",
  ].join('\n'),
  // A PHP `instanceof` branch narrows `$x` inside its body only.
  'src/g.php': [
    '<?php',
    'class Cat { function meow() {} }',
    'function pet($x) {',
    '    if ($x instanceof Cat) {',
    '        $x->meow();',
    '    }',
    '    if ($x instanceof Nope) {',
    '        $x->meow();',
    '    }',
    '}',
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
    // A range variable typed from the collection's declared slice.
    'func loop(xs []*Service) {',
    '\tfor _, x := range xs {',
    '\t\tx.Run()',
    '\t}',
    '}',
  ].join('\n'),
  // Kotlin scope functions: `it` and a named lambda parameter take the
  // receiver's type from the `let`/`also` call's root.
  'src/L.kt': [
    'class Box { fun open() {} }',
    'fun make(): Box = Box()',
    'fun user() {',
    '    val b: Box = make()',
    '    b.let { it.open() }',
    '    b.also { v -> v.open() }',
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
    'export async function makeEngine(): Promise<Engine> { return new Engine(); }',
    'export async function top2(): Promise<void> {',
    '  const eng = await makeEngine();',
    '  eng.start();',
    '}',
  ].join('\n'),
  'tool.py': 'def pyhelper():\n    return 1\n\n\nclass Widget:\n    pass\n',
  // `import tool` + `tool.pyhelper()` exercises the python module-member arm.
  'main.py': 'import tool\nfrom tool import pyhelper\n\ndef go():\n    pyhelper()\n    tool.pyhelper()\n',
  // A name match may never land on a Nix binding: no other language can call
  // one symbolically (TS drops the match; the kernel must too).
  'nix/lib.nix': '{ nixonly = x: x; }\n',
  'nixcaller.py': 'def callsnix():\n    nixonly(1)\n    obj.nixonly(2)\n',
  // Rust module-path refs — pure `::` names take the native
  // resolveRustPathReference arm ahead of the ineligible:lang punt. `lib.rs`
  // marks the crate root; `deep/mod.rs` exercises the `<seg>/mod.rs` form;
  // `Widget` is a struct (not a module) so `Widget::new` misses the arm.
  'src/lib.rs': 'fn libuser() {}\n',
  'src/sub.rs': [
    'pub fn leaf_fn() {}',
    'pub struct Widget;',
    'impl Widget {',
    '    pub fn new() {}',
    '    pub fn again(&self) { self.new(); }',
    '}',
    'pub struct Holder { inner: Widget }',
    'impl Holder {',
    '    pub fn go(&self) { self.inner.new(); }',
    '    pub fn miss(&self) { self.unknown.new(); }',
    '}',
    // `x.y` local receiver inference (R5): `: T` annotations (let or
    // param) type the receiver for rmot. `let w = Widget::new()` (spaced
    // `=`) is an inference MISS even in TS — the pattern's `=` must
    // follow the optional `:T` group — so it falls to the strat arms
    // (unique-method @0.7) exactly like undeclared receivers.
    'pub struct Ctx;',
    'impl Ctx {',
    '    pub fn run(&self) {}',
    '}',
    'pub fn useit() {',
    '    let w = Widget::new();',
    '    w.again();',
    '    let mut v: Widget = Widget::new();',
    '    v.new();',
    '    w.inner.again();',
    '}',
    'pub fn with_param(p: &Widget) { p.new(); }',
    'pub fn worker() { let ctx: Ctx = Ctx; ctx.run(); }',
    'pub fn unbound() { w.new(); z.again(); z.nomethod(); }',
    'pub fn chains() { Widget::new().again(); make().run(); }',
    // `Self::item` associated-item paths — `Self` binds the enclosing impl
    // type (Mode for both the inherent impl and `impl Step for Mode`), then
    // the leaf resolves by `owner::leaf` qualified name: enum_member `On`,
    // method `flip`. `Self::Assoc::init` in `impl Step` binds the middle
    // segment through that impl's `type Assoc = Back` decl → `Back::init`;
    // the same 3-seg shape inside the inherent impl (no `type Assoc` decl)
    // and the free-fn `Self::new` (no impl owner) decline to the name
    // strategies.
    'pub enum Mode { On, Off }',
    'pub struct Back;',
    'impl Back { pub fn init(&self) {} }',
    'impl Mode {',
    '    pub fn flip(&self) -> Mode { Self::Off }',
    '    pub fn make(&self) { Self::flip(); Self::Assoc::new(); }',
    '}',
    'pub trait Step { type Assoc; fn step(&self) -> Self; }',
    'impl Step for Mode { type Assoc = Back; fn step(&self) -> Self { Self::Assoc::init(); Self::Off } }',
    'pub fn free_self() { Self::new(); }',
  ].join('\n'),
  // Rust inheritance locality — `Error` is bound to a stdlib-rooted `use`,
  // so the same-named local type_alias must NOT adopt the implements ref.
  'src/inh.rs': [
    'use std::error::Error;',
    'pub type Error = String;',
    'pub enum MapperError { Missing }',
    'impl Error for MapperError {}',
    'pub trait Local {}',
    'impl Local for MapperError {}',
  ].join('\n'),
  'src/deep/mod.rs': 'pub mod inner;\npub mod sib;\n',
  'src/deep/inner.rs': 'pub fn deep_fn() {}\nfn deepuser() {}\n',
  'src/deep/sib.rs': 'pub fn sib_fn() {}\n',
  'src/user.rs': 'fn user() {}\n',
  // Kernel-module-style crate: no lib.rs/main.rs — `mymod_main.rs` is the
  // root (nothing declares it), discovered by climbing the `mod` decl
  // chain. `node.rs` + `node/inner.rs` exercise the 2018 nested-module
  // declarant (`<dir>/<dirname>.rs`).
  'kmod/mymod_main.rs': 'mod sub;\nmod user;\nmod node;\nfn root_fn() {}\n',
  'kmod/sub.rs': 'pub fn mod_leaf_fn() {}\n',
  'kmod/user.rs': 'fn kmod_user() {}\n',
  'kmod/node.rs': 'mod inner;\n',
  'kmod/node/inner.rs': 'pub fn nested_inner_fn() {}\n',
  // Orphan file: no declarant, no `sub.rs` sibling — `crate::sub` must miss.
  'other/lonely.rs': 'fn lonely_fn() {}\n',
  // A module file at the project root: its submodules live in `rootmod/`,
  // a project-relative directory (TS joins against the absolute root; a
  // naive `${dir}/${stem}` join made it `/rootmod` and missed).
  'rootmod.rs': 'mod child;\nfn rootmod_user() {}\n',
  'rootmod/child.rs': 'pub fn root_child_fn() {}\n',
  // Bindings-free walker languages (csharp, ruby, swift, scala, dart,
  // lua/luau, r): the kernel gate admits them; their language-specific arms
  // are the receiver-type patterns (mc-infer-local → rmot), the lua `:` /
  // r `$` receiver shapes, and lua `require`. One `lg = <Type>…;
  // lg.<method>()` per language pins each pattern natively. Method names are
  // per-language so the name-strategy pools never overlap across files.
  'src/Lg.cs': [
    'class LoggerCs { public void LogCs() {} }',
    'class AppCs { void RunCs() { var lg = new LoggerCs(); lg.LogCs(); } }',
  ].join('\n'),
  'src/lg.swift': [
    'class LoggerSw { func logSw() {} }',
    'func runSw() { let lg = LoggerSw(); lg.logSw() }',
  ].join('\n'),
  'src/lg.rb': [
    'class LoggerRb',
    '  def log_rb; end',
    'end',
    'def run_rb',
    '  lg = LoggerRb.new',
    '  lg.log_rb',
    'end',
  ].join('\n'),
  'src/Lg.scala': [
    'class LoggerSc { def logSc(): Unit = {} }',
    'object AppSc { def runSc(): Unit = { val lg = new LoggerSc(); lg.logSc() } }',
  ].join('\n'),
  'src/lg.dart': [
    'class LoggerDt { void logDt() {} }',
    'void runDt() { var lg = LoggerDt(); lg.logDt(); }',
  ].join('\n'),
  'src/lg.lua': [
    'local LoggerLua = {}',
    'LoggerLua.__index = LoggerLua',
    'function LoggerLua.new() return setmetatable({}, LoggerLua) end',
    'function LoggerLua:logLua() end',
    'local sub = require("mod.sub")',
    'local function runLua()',
    '  local lg = LoggerLua.new()',
    '  lg:logLua()',
    'end',
  ].join('\n'),
  'src/mod/sub.lua': 'local M = {}\nreturn M\n',
  'src/lg.R': [
    'LoggerR <- R6::R6Class("LoggerR", public = list(logR = function() {}))',
    'runR <- function() {',
    '  lg <- LoggerR$new()',
    '  lg$logR()',
    '}',
  ].join('\n'),
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

  it('reads a pool worker snapshot, a copy with no -wal or -shm', async () => {
    // Pool workers resolve over a checkpointed copy of the db file. A copy
    // has no -shm, and a `readonly_shm=1` open fails at its first query
    // there — every worker then fell back to TypeScript for the whole run.
    const kernel = getKernel();
    const graph = await project(true);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const db = (graph as any).db.db as import('node:sqlite').DatabaseSync;
    db.exec('PRAGMA wal_checkpoint(TRUNCATE)');
    const live = path.join(tempDir!, '.codegraph', 'codegraph.db');
    const snap = `${live}.kr-snapshot`;
    fs.copyFileSync(live, snap);
    const resolver = new kernel!.KernelResolver!({
      dbPath: snap,
      projectRoot: tempDir!,
      cppIncludeDirs: [],
      nodeBuiltinSpecifiers: [...builtinModules],
      frameworksActive: false,
      snapshot: true,
    });
    try {
      expect(Array.isArray(resolver.readPendingBatch(0, 10, false))).toBe(true);
    } finally {
      resolver.close();
    }
    // Read as-is: nothing is created next to the copy.
    expect(fs.existsSync(`${snap}-shm`)).toBe(false);
    expect(fs.existsSync(`${snap}-wal`)).toBe(false);
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
    // TS/JS/Python call chains skip the import arm: a store accessor's action
    // stays in TS, any other chain is a native miss.
    seed(runFn, 'api.prepare().all', 'src/main.ts', 'typescript', 'calls', 13);
    seed(runFn, 'api.getState().reset', 'src/main.ts', 'typescript', 'calls', 13);
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
    seed(runFn, 'über.run', 'src/main.ts', 'typescript', 'calls', 21);
    seed(runFn, 'Facade.helper', 'src/main.ts', 'typescript', 'calls', 22);
    seed(runFn, 'Facade.go', 'src/main.ts', 'typescript', 'calls', 23);
    seed(runFn, 'shared.run', 'src/main.ts', 'typescript', 'calls', 24);
    seed(runFn, 'shared.unrelated', 'src/main.ts', 'typescript', 'calls', 25);
    // Java field receiver — `private K k = new K()` inside class J.
    seed(nodeId('user', 'K.java', 'method'), 'k.mymethod', 'src/K.java', 'java', 'calls', 2);
    // Go factory receiver + two-hop field chain.
    const goRun = nodeId('run', 'main.go');
    seed(goRun, 'svc.Run', 'main.go', 'go', 'calls', 15);
    ins.run(nodeId('loop', 'main.go'), 'x.Run', 'calls', 19, 2, 'main.go', 'go');
    ins.run(nodeId('user', 'L.kt'), 'it.open', 'calls', 5, 12, 'src/L.kt', 'kotlin');
    ins.run(nodeId('user', 'L.kt'), 'v.open', 'calls', 6, 18, 'src/L.kt', 'kotlin');
    seed(nodeId('use', 'main.go'), 'o.in.Do', 'main.go', 'go', 'calls', 12);
    // Chain arms — `<inner>().<method>` refs whose receiver's type is the
    // inner call's declared return type.
    seed(nodeId('pooluser', 'w.cpp'), 'Pool::instance().drain', 'src/w.cpp', 'cpp', 'calls', 5);
    seed(nodeId('pooluser', 'w.cpp'), 'Pool::instance().nope', 'src/w.cpp', 'cpp', 'calls', 5);
    seed(nodeId('reguser', 'reg.php'), 'Registry::make().name', 'src/reg.php', 'php', 'calls', 6);
    ins.run(nodeId('pet', 'g.php'), 'x.meow', 'calls', 5, 8, 'src/g.php', 'php');
    const wireFn = byName('wire', 'method').find((n) => n.qualifiedName === 'Widget::wire')!.id;
    ins.run(wireFn, 'this.onClick', 'function_ref', 4, 38, 'src/widget.ts', 'typescript');
    ins.run(wireFn, 'this.onBase', 'function_ref', 4, 67, 'src/widget.ts', 'typescript');
    const zappFn = nodeId('zapp', 'zapp.ts');
    ins.run(zappFn, 'reset', 'calls', 4, 2, 'src/zapp.ts', 'typescript');
    ins.run(zappFn, 'r2', 'calls', 6, 2, 'src/zapp.ts', 'typescript');
    ins.run(zappFn, 'useStore.getState().reset', 'calls', 7, 2, 'src/zapp.ts', 'typescript');
    const bumpFn = [...byName('bump', 'method'), ...byName('bump', 'function')].find((n) => n.filePath.endsWith('zstore.ts'))!.id;
    ins.run(bumpFn, 'get().reset', 'calls', 4, 11, 'src/zstore.ts', 'typescript');
    const readme = cg!.getNodesByKind('file').find((n) => n.filePath === 'README.md')!.id;
    ins.run(readme, 'src/util.ts', 'references', 3, 4, 'README.md', 'markdown');
    for (const [name, line] of [['inc/db.php', 2], ['inc/db', 3], ['nowhere/g.php', 4]] as const) {
      ins.run('file:src/useinc.php', name, 'imports', line, 0, 'src/useinc.php', 'php');
    }
    ins.run(nodeId('pet', 'g.php'), 'x.meow', 'calls', 8, 8, 'src/g.php', 'php');
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
    seed(nodeId('top', 'notify.ts'), 'adat2.run', 'src/notify.ts', 'typescript', 'references', 20);
    seed(nodeId('top2', 'notify.ts'), 'eng.start', 'src/notify.ts', 'typescript', 'references', 25);
    seed(nodeId('top2', 'notify.ts'), 'eng.start', 'src/notify.ts', 'typescript', 'calls', 25);
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
    // Kernel-module crate roots (no lib.rs/main.rs): the `mod`-chain
    // fallback climbs `user.rs → mymod_main.rs`, then walks `crate::` down
    // from `kmod/`. Nested `node.rs → node/inner.rs` climbs two levels.
    const kmodUserFn = nodeId('kmod_user', 'kmod/user.rs');
    const kmodInnerFn = nodeId('nested_inner_fn', 'kmod/node/inner.rs');
    const lonelyFn = nodeId('lonely_fn', 'other/lonely.rs');
    seed(kmodUserFn, 'crate::sub::mod_leaf_fn', 'kmod/user.rs', 'rust', 'calls', 2);
    seed(kmodUserFn, 'sub::mod_leaf_fn', 'kmod/user.rs', 'rust', 'calls', 3);
    seed(kmodUserFn, 'crate::node::inner::nested_inner_fn', 'kmod/user.rs', 'rust', 'calls', 4);
    seed(kmodInnerFn, 'crate::sub::mod_leaf_fn', 'kmod/node/inner.rs', 'rust', 'calls', 2);
    seed(lonelyFn, 'crate::sub::mod_leaf_fn', 'other/lonely.rs', 'rust', 'calls', 2);
    seed(nodeId('rootmod_user', 'rootmod.rs'), 'self::child::root_child_fn', 'rootmod.rs', 'rust', 'calls', 2);
    // Rust `self.` receiver arms — `self.m` (enclosing impl via caller
    // qname), `self.f.m` (field type off the struct decl), and a decline.
    seed(nodeId('again', 'sub.rs', 'method'), 'self.new', 'src/sub.rs', 'rust', 'calls', 5);
    seed(nodeId('go', 'sub.rs', 'method'), 'self.inner.new', 'src/sub.rs', 'rust', 'calls', 7);
    seed(nodeId('miss', 'sub.rs', 'method'), 'self.unknown.new', 'src/sub.rs', 'rust', 'calls', 8);
    // Rust `x.y` local receiver inference (R5) — `let v: Widget`, a
    // typed param `p: &Widget`, and `ctx: Ctx` (the §5.25 shape) feed
    // inferLocalReceiverType → rmot @0.9. `let w = Widget::new()` (spaced
    // `=`), `w.new` scope-bounded out of `useit` into `unbound`, and the
    // undeclared `w.inner`/`z` receivers all miss → the unique-method
    // strat arm @0.7, exactly like TS. `z.nomethod` misses both.
    const useitFn = nodeId('useit', 'sub.rs');
    seed(useitFn, 'w.again', 'src/sub.rs', 'rust', 'calls', 18);
    seed(useitFn, 'v.new', 'src/sub.rs', 'rust', 'calls', 20);
    seed(useitFn, 'w.inner.again', 'src/sub.rs', 'rust', 'calls', 21);
    seed(nodeId('with_param', 'sub.rs'), 'p.new', 'src/sub.rs', 'rust', 'calls', 23);
    seed(nodeId('worker', 'sub.rs'), 'ctx.run', 'src/sub.rs', 'rust', 'calls', 24);
    const unboundFn = nodeId('unbound', 'sub.rs');
    seed(unboundFn, 'w.new', 'src/sub.rs', 'rust', 'calls', 25);
    seed(unboundFn, 'z.again', 'src/sub.rs', 'rust', 'calls', 25);
    seed(unboundFn, 'z.nomethod', 'src/sub.rs', 'rust', 'calls', 25);
    // Rust `Self::item` associated-item path — `Self` binds the caller's
    // impl owner (Widget for `again`, Mode for `make`); the leaf resolves by
    // `owner::leaf` qualified name across method/enum_member kinds. A free
    // fn has no `Self` and the 3-seg associated-type path declines — both
    // fall through to the strats exactly like TS.
    seed(nodeId('again', 'sub.rs', 'method'), 'Self::new', 'src/sub.rs', 'rust', 'calls', 5);
    seed(nodeId('make', 'sub.rs', 'method'), 'Self::On', 'src/sub.rs', 'rust', 'calls', 6);
    seed(nodeId('make', 'sub.rs', 'method'), 'Self::Assoc::new', 'src/sub.rs', 'rust', 'calls', 7);
    // `step` exists twice — the trait declaration `Step::step` and the impl
    // method `Mode::step`; the ref lives inside the impl, so select by QN.
    const implStep = byName('step', 'method').find((n) => n.qualifiedName === 'Mode::step')!.id;
    seed(implStep, 'Self::Assoc::init', 'src/sub.rs', 'rust', 'calls', 8);
    seed(useitFn, 'Self::flip', 'src/sub.rs', 'rust', 'calls', 26);
    // Gate exclusions — `::`+`.` (boundReceiver/scopedChain territory)
    // and `()` chain shapes stay punted to the TS spine. The `chains` fn
    // also exercises them through real extraction (byte-compare leg).
    const chainsFn = nodeId('chains', 'sub.rs');
    seed(chainsFn, 'Widget::new().again', 'src/sub.rs', 'rust', 'calls', 26);
    seed(chainsFn, 'make().run', 'src/sub.rs', 'rust', 'calls', 26);
    // Rust inheritance refs — the stdlib-bound `Error` must not adopt the
    // local type_alias; the in-repo `Local` trait resolves.
    const mapperId = nodeId('MapperError', 'inh.rs', 'enum');
    seed(mapperId, 'Error', 'src/inh.rs', 'rust', 'implements', 4);
    seed(mapperId, 'Local', 'src/inh.rs', 'rust', 'implements', 6);

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
    const atPre = (name: string, file: string, kind = 'imports') =>
      preOutcomes[preIdx.get(`${name}@${kind}@${file}`)!]!;

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
    // name strategy, exact and fuzzy included — a native miss.
    expect(at('W::nope', 'src/w.cpp', 'calls').status).toBe('unresolved');

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
    // A chain says nothing about what its inner call returns — no guess.
    expect(at('api.prepare().all', 'src/main.ts', 'calls').status).toBe('unresolved');
    // A store accessor chain resolves inside the identified store; `api`'s
    // literal has no `reset`, so it is a native miss.
    expect(at('api.getState().reset', 'src/main.ts', 'calls').status).toBe('unresolved');
    // `svc.call` infers Service then misses `Service::call` — the supertype
    // walk reads live edges, so the kernel punts for TS to decide.
    expect(at('svc.call', 'src/main.ts', 'calls').status).toBe('passthrough');
    // Over a db holding every supertype edge (the live db, or a snapshot taken
    // after the prerequisite phase) the kernel walks supertypes itself:
    // `Service extends BaseSvc`, written here as the prerequisite pass would,
    // proves `svc.call` on BaseSvc.
    db.prepare("INSERT INTO edges (source, target, kind) VALUES (?, ?, 'extends')").run(
      nodeId('Service', 'svc.ts', 'class'),
      nodeId('BaseSvc', 'base.ts', 'class'),
    );
    const walking = new kernel!.KernelResolver!({
      dbPath: path.join(tempDir, '.codegraph', 'codegraph.db'),
      projectRoot: tempDir,
      aliases: {
        baseUrl: '.',
        patterns: [{ prefix: '@lib/', suffix: '', hasWildcard: true, replacements: ['src/*'] }],
      },
      cppIncludeDirs: [],
      nodeBuiltinSpecifiers: [...builtinModules],
      frameworksActive: false,
      supertypesComplete: true,
    });
    const walkedAll = walking.resolveChunk(batch);
    walking.close();
    const walked = walkedAll[idx.get('svc.call@calls@src/main.ts')!]!;
    expect(walked.status).toBe('resolved');
    expect(walked.targetNodeId).toBe(nodeId('call', 'base.ts', 'method'));
    // A member neither the shared instance's type nor its supertype declares
    // (`unrelated` is only a free function elsewhere) settles natively once
    // the walk may run, the same way as TS — the byte-compare leg over this
    // fixture pins the TS side.
    const nope = walkedAll[idx.get('shared.unrelated@calls@src/main.ts')!]!;
    expect(nope.status).toBe('unresolved');
    // Object-literal aliases: `Facade.helper` follows the literal's shorthand
    // into facade.ts's own import; `Facade.go` names a local function.
    const facadeHelper = at('Facade.helper', 'src/main.ts', 'calls');
    expect(facadeHelper.status).toBe('resolved');
    expect(facadeHelper.resolvedBy).toBe('import');
    expect(facadeHelper.targetNodeId).toBe(nodeId('helper', 'util.ts'));
    const facadeGo = at('Facade.go', 'src/main.ts', 'calls');
    expect(facadeGo.status).toBe('resolved');
    expect(facadeGo.targetNodeId).toBe(nodeId('local1', 'facade.ts'));
    // `shared = new Service()` types the imported value → `Service::run` @0.85.
    const sharedRun = at('shared.run', 'src/main.ts', 'calls');
    expect(sharedRun.status).toBe('resolved');
    expect(sharedRun.resolvedBy).toBe('instance-method');
    expect(sharedRun.confidence).toBe(0.85);
    expect(sharedRun.targetNodeId).toBe(
      byName('run', 'method').find((n) => n.qualifiedName === 'Service::run')!.id,
    );
    // A miss on the inferred type needs the supertype walk — a punt here.
    expect(at('shared.unrelated', 'src/main.ts', 'calls').status).toBe('passthrough');
    // `über.run`: ASCII word boundaries miss the non-ASCII receiver in both
    // engines, and the receiver's `local` row makes the bound-receiver claim
    // exclusive — a refused claim is terminal. With Unicode boundaries the
    // kernel would have inferred `Service` and resolved `run` at 0.9.
    expect(at('über.run', 'src/main.ts', 'calls').status).toBe('unresolved');
    // Java field receiver — `private K k = new K()` infers K → `K::mymethod`.
    const km = at('k.mymethod', 'src/K.java', 'calls');
    expect(km.status).toBe('resolved');
    expect(km.resolvedBy).toBe('instance-method');
    expect(km.targetNodeId).toBe(
      byName('mymethod', 'method').find((n) => n.qualifiedName === 'K::mymethod')!.id,
    );
    // Iteration receivers (inferIterationReceiver, a tree walk): a Go range
    // variable over `xs []*Service`, and Kotlin `it` inside `b.let` where
    // `b: Box`.
    const loopRun = at('x.Run', 'main.go', 'calls');
    expect(loopRun.status).toBe('resolved');
    expect(loopRun.targetNodeId).toBe(nodeId('Run', 'main.go', 'method'));
    const itOpen = at('it.open', 'src/L.kt', 'calls');
    expect(itOpen.status).toBe('resolved');
    expect(itOpen.targetNodeId).toBe(nodeId('open', 'L.kt', 'method'));
    // A named lambda parameter has no binding row, so its declaration can't
    // be read and the walk never starts — unresolved in both engines.
    expect(at('v.open', 'src/L.kt', 'calls').status).toBe('unresolved');
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
    // `this.onClick` is Widget's own member @0.95; `this.onBase` is not on
    // Widget, so it is deferred (terminal now, retried after supertypes).
    const onClick = at('this.onClick', 'src/widget.ts', 'function_ref');
    expect(onClick.status).toBe('resolved');
    expect(onClick.confidence).toBe(0.95);
    expect(onClick.targetNodeId).toBe(byName('onClick', 'method').find((n) => n.qualifiedName === 'Widget::onClick')!.id);
    const onBase = at('this.onBase', 'src/widget.ts', 'function_ref');
    expect(onBase.status).toBe('unresolved');
    expect(onBase.reason).toBe('defer-this');
    // The deferred pass walks Widget → BaseW (the index wrote that edge).
    const onBaseRow = batch[idx.get('this.onBase@function_ref@src/widget.ts')!]!;
    const [inherited] = resolver.resolveDeferredThisMembers([onBaseRow]);
    expect(inherited!.status).toBe('resolved');
    expect(inherited!.confidence).toBe(0.85);
    expect(inherited!.targetNodeId).toBe(byName('onBase', 'method').find((n) => n.qualifiedName === 'BaseW::onBase')!.id);
    // Store actions resolve natively to the store's `reset`.
    const storeReset = [...byName('reset', 'method'), ...byName('reset', 'function')].find((n) => n.filePath.endsWith('zstore.ts'))!.id;
    for (const [name, file] of [['reset', 'src/zapp.ts'], ['r2', 'src/zapp.ts'], ['useStore.getState().reset', 'src/zapp.ts'], ['get().reset', 'src/zstore.ts']] as const) {
      const hit = at(name, file, 'calls');
      expect(hit.status, name).toBe('resolved');
      expect(hit.targetNodeId, name).toBe(storeReset);
    }
    // A markdown link is answered natively by the file-path arm.
    const mdLink = at('src/util.ts', 'README.md', 'references');
    expect(mdLink.status).toBe('resolved');
    expect(mdLink.resolvedBy).toBe('file-path');
    expect(mdLink.targetNodeId).toBe(cg!.getNodesByKind('file').find((n) => n.filePath === 'src/util.ts')!.id);
    // PHP include paths (prerequisite-phase `imports` rows).
    const dbFile = cg!.getNodesByKind('file').find((n) => n.filePath === 'src/inc/db.php')!.id;
    for (const name of ['inc/db.php', 'inc/db']) {
      const inc = atPre(name, 'src/useinc.php');
      expect(inc.status, name).toBe('resolved');
      expect(inc.targetNodeId, name).toBe(dbFile);
    }
    expect(atPre('nowhere/g.php', 'src/useinc.php').status).toBe('unresolved');
    // mc-guarded — `$x instanceof Cat` types `$x` inside its body; the
    // second branch names an undeclared type, so nothing is proven there.
    const guardedIdx = batch
      .map((row, i) => ({ row, i }))
      .filter(({ row }) => row.referenceName === 'x.meow' && row.filePath === 'src/g.php');
    const guarded = new Map(guardedIdx.map(({ row, i }) => [row.line, outcomes[i]!]));
    expect(guarded.get(5)!.status).toBe('resolved');
    expect(guarded.get(5)!.targetNodeId).toBe(nodeId('meow', 'g.php', 'method'));
    expect(guarded.get(8)!.status).toBe('unresolved');
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
    // mc-await — `const adat2 = await unknownFactory()` is an awaited
    // receiver of unknown type: a terminal miss, never a name guess.
    expect(at('adat2.run', 'src/notify.ts', 'references').status).toBe('unresolved');
    // `const eng = await makeEngine()` where makeEngine returns
    // `Promise<Engine>` types the receiver on both the free and bound arms.
    const engineStart = byName('start', 'method').find((n) => n.qualifiedName === 'Engine::start')!.id;
    for (const kind of ['references', 'calls']) {
      const eng = at('eng.start', 'src/notify.ts', kind);
      expect(eng.status, kind).toBe('resolved');
      expect(eng.targetNodeId, kind).toBe(engineStart);
    }
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
    // `Pool::missing` — no `missing` member anywhere: matchFunctionRef's `::`
    // arm misses and a `::` name tries nothing else → a native miss.
    expect(at('Pool::missing', 'src/w.cpp', 'function_ref').status).toBe('unresolved');
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
    // External crate — both anchors miss, and so does every name strategy.
    expect(at('ext::module::leaf_fn', 'src/lib.rs', 'calls').status).toBe('unresolved');
    // `Widget` is a struct, not a module — path-arm miss falls through to
    // the qualified-name arm, matching TS's downstream resolution.
    const widgetNew = at('Widget::new', 'src/lib.rs', 'calls');
    expect(widgetNew.status).toBe('resolved');
    expect(widgetNew.resolvedBy).toBe('qualified-name');
    expect(widgetNew.targetNodeId).toBe(nodeId('new', 'sub.rs', 'method'));
    // Leaf unknown → prefilter miss → terminal unresolved (store-binding is
    // JS-gated dead for rust).
    expect(at('crate::sub::missing', 'src/lib.rs', 'calls').status).toBe('unresolved');
    // `::`+`.` names are receiver-shaped — dot-gated punt back to TS
    // (rust's non-self receiver inference is source-reading, unported).
    expect(at('a::b.c', 'src/lib.rs', 'calls').status).toBe('passthrough');
    // Root-level module file: `self::child` resolves under `rootmod/`.
    const rootChild = at('self::child::root_child_fn', 'rootmod.rs', 'calls');
    expect(rootChild.status).toBe('resolved');
    expect(rootChild.targetNodeId).toBe(nodeId('root_child_fn', 'rootmod/child.rs'));
    // `mod`-chain crate roots — `kmod/` has no lib.rs/main.rs; the fallback
    // climbs `user.rs`'s `mod user;` decl to `mymod_main.rs` (the root) and
    // resolves `crate::` under `kmod/`.
    const kmodHit = at('crate::sub::mod_leaf_fn', 'kmod/user.rs', 'calls');
    expect(kmodHit.status).toBe('resolved');
    expect(kmodHit.targetNodeId).toBe(nodeId('mod_leaf_fn', 'kmod/sub.rs'));
    // Bare path: self-relative `kmod/user/sub.rs` misses → crate-relative hit.
    expect(at('sub::mod_leaf_fn', 'kmod/user.rs', 'calls').targetNodeId).toBe(
      nodeId('mod_leaf_fn', 'kmod/sub.rs'),
    );
    // Two-level descent from the chain root: `node` → node.rs, `inner` →
    // node/inner.rs (the `mod inner;` decl lives in node.rs).
    const nestedHit = at('crate::node::inner::nested_inner_fn', 'kmod/user.rs', 'calls');
    expect(nestedHit.status).toBe('resolved');
    expect(nestedHit.targetNodeId).toBe(
      nodeId('nested_inner_fn', 'kmod/node/inner.rs'),
    );
    // Climbing THROUGH a nested-module declarant: inner.rs's chain is
    // inner.rs → node.rs → mymod_main.rs → root dir kmod/.
    expect(
      at('crate::sub::mod_leaf_fn', 'kmod/node/inner.rs', 'calls').targetNodeId,
    ).toBe(nodeId('mod_leaf_fn', 'kmod/sub.rs'));
    // Orphan file: `other/lonely.rs` has no declarant and no `sub.rs`
    // sibling — `crate::sub` misses (root dir = `other/` itself); the arm
    // falls through and every name strategy misses too.
    expect(
      at('crate::sub::mod_leaf_fn', 'other/lonely.rs', 'calls').status,
    ).toBe('unresolved');
    // `self.m` → match_rust_self_call — enclosing impl type via caller qname.
    const selfNew = at('self.new', 'src/sub.rs', 'calls');
    expect(selfNew.status).toBe('resolved');
    expect(selfNew.resolvedBy).toBe('qualified-name');
    expect(selfNew.confidence).toBe(0.9);
    expect(selfNew.targetNodeId).toBe(nodeId('new', 'sub.rs', 'method'));
    // `self.f.m` → match_rust_self_field_call — field type off the struct decl.
    const fieldNew = at('self.inner.new', 'src/sub.rs', 'calls');
    expect(fieldNew.status).toBe('resolved');
    expect(fieldNew.resolvedBy).toBe('instance-method');
    expect(fieldNew.confidence).toBe(0.85);
    expect(fieldNew.targetNodeId).toBe(nodeId('new', 'sub.rs', 'method'));
    // `self.unknown.m` — field not declared on Holder → exclusive
    // decline, and the exact/fuzzy tail misses too — the same failed verdict
    // TS produces.
    expect(at('self.unknown.new', 'src/sub.rs', 'calls').status).toBe('unresolved');
    // `Self::item` → match_rust_self_path — `Self` binds the caller's impl
    // owner and the leaf resolves by `owner::leaf` qualified name.
    const selfPath = at('Self::new', 'src/sub.rs', 'calls');
    expect(selfPath.status).toBe('resolved');
    expect(selfPath.resolvedBy).toBe('qualified-name');
    expect(selfPath.confidence).toBe(0.9);
    expect(selfPath.targetNodeId).toBe(nodeId('new', 'sub.rs', 'method'));
    // `Self::On` → enum_member `Mode::On` on the same binding.
    const selfVariant = at('Self::On', 'src/sub.rs', 'calls');
    expect(selfVariant.status).toBe('resolved');
    expect(selfVariant.resolvedBy).toBe('qualified-name');
    expect(selfVariant.targetNodeId).toBe(nodeId('On', 'sub.rs', 'enum_member'));
    // `Self::Assoc::init` — 3-seg associated-type path: `Assoc` binds
    // through the enclosing `impl Step for Mode` block's `type Assoc = Back`
    // decl, then `Back::init` resolves by qualified name.
    const selfAssoc = at('Self::Assoc::init', 'src/sub.rs', 'calls');
    expect(selfAssoc.status).toBe('resolved');
    expect(selfAssoc.resolvedBy).toBe('qualified-name');
    expect(selfAssoc.confidence).toBe(0.9);
    expect(selfAssoc.targetNodeId).toBe(nodeId('init', 'sub.rs', 'method'));
    // `Self::Assoc::new` inside the inherent `impl Mode` — no `type Assoc`
    // decl there, so the arm declines and the exact/fuzzy tail misses the
    // `::` name like any other deep path.
    expect(at('Self::Assoc::new', 'src/sub.rs', 'calls').status).toBe('unresolved');
    // `Self::flip` from the free fn `useit` — no impl owner → falls through
    // to strat3, where `flip` is the unique rust method → @0.7, same as TS.
    const freeSelf = at('Self::flip', 'src/sub.rs', 'calls');
    expect(freeSelf.status).toBe('resolved');
    expect(freeSelf.resolvedBy).toBe('instance-method');
    expect(freeSelf.confidence).toBe(0.7);
    expect(freeSelf.targetNodeId).toBe(nodeId('flip', 'sub.rs', 'method'));
    // `x.y` local receiver inference (R5). `let mut v: Widget`, a typed
    // param `p: &Widget`, and `ctx: Ctx` (the exact §5.25 drift shape)
    // donate their annotation → rmot @0.9 instance-method.
    expect(at('v.new', 'src/sub.rs', 'calls').targetNodeId).toBe(nodeId('new', 'sub.rs', 'method'));
    expect(at('p.new', 'src/sub.rs', 'calls').targetNodeId).toBe(nodeId('new', 'sub.rs', 'method'));
    const ctxRun = at('ctx.run', 'src/sub.rs', 'calls');
    expect(ctxRun.status).toBe('resolved');
    expect(ctxRun.resolvedBy).toBe('instance-method');
    expect(ctxRun.confidence).toBe(0.9);
    expect(ctxRun.targetNodeId).toBe(nodeId('run', 'sub.rs', 'method'));
    // Inference misses land on the strat arms exactly like TS: `new` and
    // `again` are unique rust methods → strat3 single-candidate @0.7.
    // `let w = Widget::new()` is a miss in TS too — the pattern needs `=`
    // right after the optional `:T` group, so only `w=T`/`w: T` donate.
    // `w.new` in `unbound` is scope-bounded out — w's decl is in `useit`.
    for (const [n, want] of [
      ['w.again', 'again'],
      ['w.new', 'new'],
      ['z.again', 'again'],
      ['w.inner.again', 'again'],
    ] as const) {
      const h = at(n, 'src/sub.rs', 'calls');
      expect(h.status).toBe('resolved');
      expect(h.resolvedBy).toBe('instance-method');
      expect(h.confidence).toBe(0.7);
      expect(h.targetNodeId).toBe(nodeId(want, 'sub.rs', 'method'));
    }
    // `z.nomethod` — inference and strat both miss, and no `nomethod`
    // node exists anywhere → prefilter terminal unresolved (native
    // verdict, same as TS — not a punt).
    expect(at('z.nomethod', 'src/sub.rs', 'calls').status).toBe('unresolved');
    // Gate exclusions: `::`+`.` and `()` shapes stay punted to the TS
    // spine (boundReceiver/scopedChain territory, unchanged by R5).
    expect(at('Widget::new().again', 'src/sub.rs', 'calls').status).toBe('passthrough');
    expect(at('make().run', 'src/sub.rs', 'calls').status).toBe('passthrough');
    // `impl Error for X` bound to `use std::error::Error` — the locality
    // gate drops the same-named local type_alias → stays failed.
    // (implements refs are prerequisite-kind rows.)
    expect(atPre('Error', 'src/inh.rs', 'implements').status).toBe('unresolved');
    // In-repo trait impl → resolved via the bare-name path.
    const localInh = atPre('Local', 'src/inh.rs', 'implements');
    expect(localInh.status).toBe('resolved');
    expect(localInh.targetNodeId).toBe(nodeId('Local', 'inh.rs', 'trait'));
  });

  it('admits the bindings-free walker languages and ports their receiver arms', async () => {
    const kernel = getKernel();
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-kresolve-'));
    for (const [rel, content] of Object.entries(FIXTURE)) {
      fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tempDir, rel), content);
    }
    cg = await CodeGraph.init(tempDir, { index: true });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const db = (cg as any).db.db as import('node:sqlite').DatabaseSync;
    const nodeId = (name: string, file: string, kind = 'function') =>
      cg!.getNodesByKind(kind).find((n) => n.name === name && n.filePath.endsWith(file))!.id;
    const ins = db.prepare(
      "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, file_path, language, status) VALUES (?,?,?,?,?,?,?,'pending')",
    );
    const seed = (from: string, name: string, file: string, lang: string, kind: string, line: number) =>
      ins.run(from, name, kind, line, 0, file, lang);

    seed(nodeId('RunCs', 'Lg.cs', 'method'), 'lg.LogCs', 'src/Lg.cs', 'csharp', 'calls', 2);
    seed(nodeId('runSw', 'lg.swift'), 'lg.logSw', 'src/lg.swift', 'swift', 'calls', 2);
    seed(nodeId('run_rb', 'lg.rb'), 'lg.log_rb', 'src/lg.rb', 'ruby', 'calls', 6);
    seed(nodeId('runSc', 'Lg.scala', 'method'), 'lg.logSc', 'src/Lg.scala', 'scala', 'calls', 2);
    seed(nodeId('runDt', 'lg.dart'), 'lg.logDt', 'src/lg.dart', 'dart', 'calls', 2);
    const luaRun = nodeId('runLua', 'lg.lua');
    seed(luaRun, 'lg:logLua', 'src/lg.lua', 'lua', 'calls', 8);
    seed(luaRun, 'LoggerLua.new', 'src/lg.lua', 'lua', 'calls', 7);
    seed(nodeId('lg.lua', 'lg.lua', 'file'), 'mod.sub', 'src/lg.lua', 'lua', 'imports', 5);
    const rRun = nodeId('runR', 'lg.R');
    seed(rRun, 'lg$logR', 'src/lg.R', 'r', 'calls', 4);
    seed(rRun, 'LoggerR$new', 'src/lg.R', 'r', 'calls', 3);

    const resolver = new kernel!.KernelResolver!({
      dbPath: path.join(tempDir, '.codegraph', 'codegraph.db'),
      projectRoot: tempDir,
      cppIncludeDirs: [],
      nodeBuiltinSpecifiers: [...builtinModules],
      frameworksActive: false,
    });
    const batch = resolver.readPendingBatch(0, 200, false);
    const outcomes = resolver.resolveChunk(batch);
    const idx = new Map(batch.map((r, i) => [`${r.referenceName}@${r.filePath}`, i]));
    const at = (name: string, file: string) => outcomes[idx.get(`${name}@${file}`)!]!;
    const method = (name: string, file: string) => nodeId(name, file, 'method');

    // Each language's receiver-type pattern names the declared type; rmot
    // then proves the method → instance-method @0.9, the TS verdict.
    const pins: Array<[string, string, string]> = [
      ['lg.LogCs', 'src/Lg.cs', method('LogCs', 'Lg.cs')],
      ['lg.logSw', 'src/lg.swift', method('logSw', 'lg.swift')],
      ['lg.log_rb', 'src/lg.rb', method('log_rb', 'lg.rb')],
      ['lg.logSc', 'src/Lg.scala', method('logSc', 'Lg.scala')],
      ['lg.logDt', 'src/lg.dart', method('logDt', 'lg.dart')],
      ['lg:logLua', 'src/lg.lua', method('logLua', 'lg.lua')],
      ['lg$logR', 'src/lg.R', method('logR', 'lg.R')],
    ];
    for (const [name, file, target] of pins) {
      const o = at(name, file);
      expect(o.status, name).toBe('resolved');
      expect(o.resolvedBy, name).toBe('instance-method');
      expect(o.confidence, name).toBe(0.9);
      expect(o.targetNodeId, name).toBe(target);
    }
    // `LoggerLua.new`: no receiver pattern fires on the table itself, so
    // strategy 3's unique same-language method resolves it @0.7 like TS.
    const luaNew = at('LoggerLua.new', 'src/lg.lua');
    expect(luaNew.status).toBe('resolved');
    expect(luaNew.confidence).toBe(0.7);
    expect(luaNew.targetNodeId).toBe(method('new', 'lg.lua'));
    // `LoggerR$new`: the R6 class declares no `new`; every name strategy,
    // exact and fuzzy included, declines natively.
    expect(at('LoggerR$new', 'src/lg.R').status).toBe('unresolved');
    // lua `require("mod.sub")` is an `imports` ref (prerequisite batch):
    // the suffix match links the module file node @0.9.
    const pre = resolver.readPendingBatch(0, 200, true);
    const preOut = resolver.resolveChunk(pre);
    const req = preOut[pre.findIndex((r) => r.referenceName === 'mod.sub' && r.filePath === 'src/lg.lua')]!;
    expect(req.status).toBe('resolved');
    expect(req.resolvedBy).toBe('import');
    expect(req.confidence).toBe(0.9);
    expect(req.targetNodeId).toBe(nodeId('sub.lua', 'mod/sub.lua', 'file'));
    resolver.close();
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
