/**
 * A call through a field (`s.store.Upsert()`, `self.inner.reset()`,
 * `this.mailer.send()`) resolves on the type the field declares, and only
 * there: not on a same-named type elsewhere picked by file order, not on a
 * parameter or object key that shares the field's name, and not on the
 * element type of an array field.
 */
import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let dir: string | undefined;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
  dir = undefined;
});

async function project(files: Record<string, string>) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-field-chain-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
  graph.resolveReferences();
}

/** `Qualified::name@file` of every call target of the method `caller`. */
function calls(caller: string): string[] {
  const from = graph!.getNodesByKind('method').find(n => n.qualifiedName === caller);
  expect(from, caller).toBeDefined();
  return graph!
    .getOutgoingEdges(from!.id)
    .filter(e => e.kind === 'calls')
    .map(e => graph!.getNode(e.target)!)
    .map(n => `${n.qualifiedName}@${n.filePath}`)
    .sort();
}

const goStore = (pkg: string) => `package ${pkg}\n\ntype Store struct{}\n\nfunc (s *Store) Upsert() {}\n`;
const goFiles = (decoyDir: string) => ({
  'go.mod': 'module example.com/app\n\ngo 1.22\n',
  [`internal/${decoyDir}/fkit/store.go`]: goStore('fkit'),
  'internal/store/payslipstore/store.go': goStore('payslipstore'),
  'internal/usecase/cycle.go': `package usecase

import "example.com/app/internal/store/payslipstore"

type Service struct {
	store *payslipstore.Store
}

func (s *Service) Run() {
	s.store.Upsert()
}
`,
});

describe('Go field chain', () => {
  it.each(['gen', 'zgen'])('binds the imported package type, whatever the decoy dir sorts as (%s)', async decoy => {
    await project(goFiles(decoy));
    expect(calls('Service::Run')).toEqual(['Store::Upsert@internal/store/payslipstore/store.go']);
  });
});

describe('Rust self.field chain', () => {
  const inner = 'pub struct Inner;\nimpl Inner { pub fn reset(&self) {} }\n';
  const files = (useLine: string, decoy: string) => ({
    'lib.rs': `pub mod caller; pub mod inner; pub mod ${decoy};\n`,
    'caller.rs': `${useLine}pub struct Target { inner: Inner }\nimpl Target { pub fn run(&self) { self.inner.reset(); } }\n`,
    'inner.rs': inner,
    [`${decoy}.rs`]: inner,
  });

  it.each(['decoy', 'zdecoy'])('follows the use that names the field type (%s)', async decoy => {
    await project(files('use crate::inner::Inner;\n', decoy));
    expect(calls('Target::run')).toEqual(['Inner::reset@inner.rs']);
  });

  it('declines between same-named types when nothing says which one the field means', async () => {
    await project(files('', 'decoy'));
    expect(calls('Target::run')).toEqual([]);
  });
});

describe('Rust Self::Assoc path', () => {
  const lib = (attr: string) => `pub struct Back;
impl Back { pub fn init() {} }
pub struct Target;
pub trait Tr { type A; fn step(&self); }
impl Tr for Target {
    type A = Back;
    #[doc = "${attr}"]
    fn step(&self) { Self::A::init(); }
}
`;
  it.each(['{', 'x'])('binds the impl associated type past an attribute string %j', async attr => {
    await project({ 'lib.rs': lib(attr) });
    expect(calls('Target::step')).toEqual(['Back::init@lib.rs']);
  });
});

describe('TypeScript this.field chain', () => {
  const mailer = 'export class Mailer {\n  send(msg: string): string { return msg; }\n  push(msg: string): void {}\n}\n';
  const notifier = (extra: string, items: string) => `import { Mailer } from './mailer';
export class Notifier {
${extra}  constructor(private readonly mailer: Mailer, private items: ${items}) {}
  send(msg: string): string { return this.mailer.send(msg); }
  push(msg: string): void { this.items.push(msg); }
}
`;

  it.each([
    ['a method parameter', '  noop(mailer: Notifier) {}\n'],
    ['an object key', '  static defaults(overrides: any) { return { mailer: overrides.mailer }; }\n'],
    ['nothing', '  noop(other: Notifier) {}\n'],
  ])('reads the field type, not %s of the same name', async (_label, extra) => {
    await project({ 'src/mailer.ts': mailer, 'src/notifier.ts': notifier(extra, 'Mailer') });
    expect(calls('Notifier::send')).toEqual(['Mailer::send@src/mailer.ts']);
  });

  it('reads an inherited field from a plain constructor parameter handed to super', async () => {
    await project({
      'src/mailer.ts': mailer,
      'src/notifier.ts': `import { Mailer } from './mailer';
export class Base { constructor(public mailer: Mailer) {} }
export class Notifier extends Base {
  constructor(mailer: Mailer, options: { mailer: string }) { super(mailer); }
  send(msg: string): string { return this.mailer.send(msg); }
}
`,
    });
    expect(calls('Notifier::send')).toEqual(['Mailer::send@src/mailer.ts']);
  });

  it('does not bind an array method to the element type', async () => {
    await project({ 'src/mailer.ts': mailer, 'src/notifier.ts': notifier('', 'Mailer[]') });
    expect(calls('Notifier::push')).toEqual([]);
  });

  it.each(['Mailer', 'Mailer | null'])('binds a %s field to Mailer', async items => {
    await project({ 'src/mailer.ts': mailer, 'src/notifier.ts': notifier('', items) });
    expect(calls('Notifier::push')).toEqual(['Mailer::push@src/mailer.ts']);
  });
});
