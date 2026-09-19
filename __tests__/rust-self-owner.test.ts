import { afterEach, beforeEach, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let root: string;
let cg: CodeGraph | undefined;
beforeEach(() => { root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rust-self-owner-')); });
afterEach(() => { cg?.close(); cg = undefined; fs.rmSync(root, { recursive: true, force: true }); });
async function index(files: Record<string, string>) {
  fs.mkdirSync(path.join(root, 'src'));
  fs.writeFileSync(path.join(root, 'Cargo.toml'), '[package]\nname="owners"\nversion="0.1.0"\nedition="2021"\n');
  for (const [file, text] of Object.entries(files)) fs.writeFileSync(path.join(root, 'src', file), text);
  cg = await CodeGraph.init(root, { index: true });
}
function targets(file: string) {
  const caller = cg!.getNodesByKind('method').find(n => n.filePath === `src/${file}` && n.qualifiedName === 'Target::run');
  expect(caller).toBeDefined();
  return cg!.getOutgoingEdges(caller!.id).filter(e => e.kind === 'calls').map(e => {
    const target = cg!.getNode(e.target)!;
    return `${target.filePath}:${target.qualifiedName}`;
  });
}

it('does not borrow a missing method from a same-named type in another module (#1861)', async () => {
  await index({
    'lib.rs': 'pub mod caller; pub mod decoy;',
    'caller.rs': 'pub struct Target;\nimpl Target { pub fn run(&self) { self.reset(); } }',
    'decoy.rs': 'pub struct Target;\nimpl Target { pub fn reset(&self) {} }',
  });
  expect(targets('caller.rs')).toEqual([]);
});

it('keeps a proven local owner despite a same-named type and method in another module (#1861)', async () => {
  await index({
    'lib.rs': 'pub mod caller; pub mod decoy;',
    'caller.rs': 'pub struct Target;\nimpl Target { pub fn reset(&self) {} }\nimpl Target { pub fn run(&self) { self.reset(); } }',
    'decoy.rs': 'pub struct Target;\nimpl Target { pub fn reset(&self) {} }',
  });
  expect(targets('caller.rs')).toEqual(['src/caller.rs:Target::reset']);
  fs.writeFileSync(path.join(root, 'src/caller.rs'), 'pub struct Target;\nimpl Target { pub fn run(&self) { self.reset(); } }');
  await cg!.sync();
  expect(targets('caller.rs')).toEqual([]);
  fs.writeFileSync(path.join(root, 'src/caller.rs'), 'pub struct Target;\nimpl Target { pub fn reset(&self) {} }\nimpl Target { pub fn run(&self) { self.reset(); } }');
  await cg!.sync();
  expect(targets('caller.rs')).toEqual(['src/caller.rs:Target::reset']);
});

it('keeps a unique owner whose impl is split across files (#1861)', async () => {
  await index({
    'lib.rs': 'pub mod caller;\npub struct Target;\nimpl Target { pub fn reset(&self) {} }',
    'caller.rs': 'use crate::Target;\nimpl Target { pub fn run(&self) { self.reset(); } }',
  });
  expect(targets('caller.rs')).toEqual(['src/lib.rs:Target::reset']);
});

it('declines indistinguishable inline-module owners instead of claiming one (#1861)', async () => {
  await index({ 'lib.rs': `mod a {
    pub struct Target;
    impl Target { pub fn reset(&self) {} }
  }
  mod b {
    pub struct Target;
    impl Target { pub fn run(&self) { self.reset(); } }
  }` });
  expect(targets('lib.rs')).toEqual([]);
});

it('resolves Self::item associated paths to the enclosing impl type', async () => {
  await index({
    'lib.rs': 'pub struct Target;\nimpl Target { pub fn helper(&self) {} pub fn run(&self) { Self::helper(); } }',
  });
  expect(targets('lib.rs')).toEqual(['src/lib.rs:Target::helper']);
});

it('resolves Self::Variant(..) constructor to the enum member', async () => {
  await index({
    'lib.rs': 'pub enum Target { On(u8), Off }\nimpl Target { pub fn run(&self) -> Target { Self::On(1) } }',
  });
  expect(targets('lib.rs')).toEqual(['src/lib.rs:Target::On']);
});

it('binds Self through a trait impl (impl Tr for T)', async () => {
  await index({
    'lib.rs': 'pub struct Target;\npub trait Tr { fn step(&self) -> Self; }\nimpl Tr for Target { fn step(&self) -> Self { Self::helper() } }\nimpl Target { pub fn helper(&self) {} }',
  });
  const caller = cg!.getNodesByKind('method').find(n => n.qualifiedName === 'Target::step');
  const calls = cg!.getOutgoingEdges(caller!.id).filter(e => e.kind === 'calls')
    .map(e => cg!.getNode(e.target)!.qualifiedName);
  expect(calls).toEqual(['Target::helper']);
});

it('resolves Self::AssocType::member through the impl assoc-type decl', async () => {
  await index({
    'lib.rs': 'pub struct Back;\nimpl Back { pub fn init(&self) {} }\npub struct Target;\npub trait Tr { type A; fn step(&self); }\nimpl Tr for Target { type A = Back; fn step(&self) { Self::A::init(); } }',
  });
  const caller = cg!.getNodesByKind('method').find(n => n.qualifiedName === 'Target::step');
  const calls = cg!.getOutgoingEdges(caller!.id).filter(e => e.kind === 'calls')
    .map(e => cg!.getNode(e.target)!.qualifiedName);
  expect(calls).toEqual(['Back::init']);
});

it('declines Self::AssocType paths with no impl decl, and absent members', async () => {
  await index({
    'lib.rs': 'pub struct Target;\nimpl Target { pub fn run(&self) { Self::Assoc::new(); Self::nonexistent_zz(); } }',
  });
  expect(targets('lib.rs')).toEqual([]);
});

it('resolves Self::f().tail through the receiver return type', async () => {
  await index({
    'lib.rs':
      'pub struct Chain;\nimpl Chain { pub fn next(&self) {} }\n' +
      'pub struct Target;\nimpl Target {\n' +
      '  pub fn make() -> Self { Self }\n' +
      '  pub fn step(&self) {}\n' +
      '  pub fn to_chain() -> Chain { Chain }\n' +
      '  pub fn run(&self) { Self::make().step(); Self::to_chain().next(); }\n}',
  });
  const caller = cg!.getNodesByKind('method').find(n => n.qualifiedName === 'Target::run');
  const calls = cg!.getOutgoingEdges(caller!.id).filter(e => e.kind === 'calls')
    .map(e => cg!.getNode(e.target)!.qualifiedName);
  // `Self::make`/`Self::to_chain` inner calls resolve via the 2-seg arm;
  // the `.step`/`.next` tails ride the receiver return type.
  expect(calls.sort()).toEqual(['Chain::next', 'Target::make', 'Target::step', 'Target::to_chain']);
});

it('binds the tail of Self::Assoc::f().tail through the impl type decl', async () => {
  await index({
    'lib.rs':
      'pub struct Out;\nimpl Out { pub fn finish(&self) {} }\n' +
      'pub struct Back;\nimpl Back { pub fn make() -> Out { Out } }\n' +
      'pub struct Target;\npub trait Tr { type A; fn step(&self); }\n' +
      'impl Tr for Target { type A = Back; fn step(&self) { Self::A::make().finish(); } }',
  });
  const caller = cg!.getNodesByKind('method').find(n => n.qualifiedName === 'Target::step');
  const calls = cg!.getOutgoingEdges(caller!.id).filter(e => e.kind === 'calls')
    .map(e => cg!.getNode(e.target)!.qualifiedName);
  // `Self::A::make` inner call via the assoc-type arm; `.finish` tail via
  // make's `-> Out`.
  expect(calls.sort()).toEqual(['Back::make', 'Out::finish']);
});

it('declines Self::f().tail when the return type or tail is unresolvable', async () => {
  await index({
    'lib.rs':
      'pub struct Target;\nimpl Target {\n' +
      '  pub fn opt() -> Option<Self> { None }\n' +
      '  pub fn run(&self) { Self::opt().unwrap(); Self::opt().missing_zz(); Self::nope().x(); }\n}',
  });
  const caller = cg!.getNodesByKind('method').find(n => n.qualifiedName === 'Target::run');
  const calls = cg!.getOutgoingEdges(caller!.id).filter(e => e.kind === 'calls')
    .map(e => cg!.getNode(e.target)!.qualifiedName);
  // `Self::opt` inner calls resolve (two edges, one per call site); every
  // `.tail` declines (Option is not an in-graph owner; `nope` receiver misses).
  expect(calls).toEqual(['Target::opt', 'Target::opt']);
});
