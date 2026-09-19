/**
 * Rust binding rows from the kernel walker — `use` declarations emit one
 * `import` row per bound local name (resolution-binding-model-plan.md §2.4
 * enhancement leg). `targetSpec` is the full `::` path as written,
 * `targetName` its leaf; aliases rename the local; `use a::b::{self}` binds
 * the parent leaf `b`; a glob is recorded under the never-matching name `*`;
 * `pub use` carries `exportForm: public` + `exportedAs`. Scope is the use's
 * parent extent — the file for a top-level use, the `mod` body or `fn` block
 * otherwise.
 */
import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { tryKernelExtract } from '../src/extraction/kernel';
import { importMappingsFromBindings } from '../src/resolution/import-resolver';
import type { Binding } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

const RUST = `use crate::foo;
use crate::foo as bar;
use a::{b, c};
use a::{b::{c, d}};
use a::b::{self, c};
use std::io::{self as io, Write};
use a::*;
pub use crate::x as y;

mod m {
    use super::sup::S;
}

fn f() {
    use local::dep::Item;
    let z = Item::new();
}
`;

const by = (rows: Binding[], name: string, spec?: string) =>
  rows.find((b) => b.name === name && (spec === undefined || b.targetSpec === spec));

describe.skipIf(!kernelBuilt)('rust use bindings', () => {
  const rows = kernelBuilt ? tryKernelExtract('src/main.rs', RUST, 'rust')?.bindings ?? [] : [];

  it('a plain use binds the path leaf to the full spec', () => {
    expect(by(rows, 'foo')).toMatchObject({
      kind: 'import', targetSpec: 'crate::foo', targetName: 'foo',
    });
    expect(by(rows, 'foo')!.exportForm).toBeUndefined();
  });

  it('an alias binds the alias, not the leaf', () => {
    expect(by(rows, 'bar')).toMatchObject({ kind: 'import', targetSpec: 'crate::foo', targetName: 'foo' });
  });

  it('use lists flatten to one row per name', () => {
    expect(by(rows, 'b', 'a::b')).toBeDefined();
    expect(by(rows, 'c', 'a::c')).toBeDefined();
  });

  it('nested use lists carry the accumulated prefix', () => {
    expect(by(rows, 'c', 'a::b::c')).toBeDefined();
    expect(by(rows, 'd', 'a::b::d')).toBeDefined();
  });

  it('{self} inside a list binds the parent leaf', () => {
    expect(by(rows, 'b', 'a::b')).toMatchObject({ targetName: 'b' });
    expect(by(rows, 'c', 'a::b::c')).toBeDefined();
  });

  it('{self as x} binds the alias to the prefix path, not a `self` segment', () => {
    expect(by(rows, 'io')).toMatchObject({ kind: 'import', targetSpec: 'std::io', targetName: 'io' });
    expect(by(rows, 'Write', 'std::io::Write')).toBeDefined();
  });

  it('a glob records its module path under the never-matching name *', () => {
    expect(by(rows, '*')).toMatchObject({ kind: 'import', targetSpec: 'a::*', targetName: '*' });
  });

  it('pub use is a public re-export', () => {
    expect(by(rows, 'y')).toMatchObject({
      kind: 'import', targetSpec: 'crate::x', exportedAs: 'y', exportForm: 'public',
    });
  });

  it('mod- and fn-local uses are scoped to their enclosing extent', () => {
    const s = by(rows, 'S', 'super::sup::S');
    const item = by(rows, 'Item', 'local::dep::Item');
    expect(s).toBeDefined();
    expect(item).toBeDefined();
    // `mod m` opens on line 10 and `fn f` on line 14 — each scope sits inside
    // its parent extent and neither covers the other's use.
    expect(s!.scopeStart).toBe(10);
    expect(item!.scopeStart).toBe(14);
    expect(s!.scopeEnd).toBeLessThan(item!.scopeStart);
  });

  it('importMappingsFromBindings turns the rows into local-name mappings', () => {
    const mappings = importMappingsFromBindings(rows)!;
    expect(mappings.find((m) => m.localName === 'bar')).toMatchObject({
      source: 'crate::foo', exportedName: 'foo',
    });
    expect(mappings.find((m) => m.localName === '*')).toMatchObject({ isNamespace: true });
  });
});
