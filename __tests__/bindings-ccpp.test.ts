/**
 * C and C++ binding rows from the kernel walker (Phase 3 of
 * docs/design/resolution-binding-model-plan.md).
 *
 * A file-level definition is a `decl` row exported as itself; `static` is
 * recorded as `storage = static` and left to the resolver, which exempts
 * headers (a `static inline` there exists in every includer). Parameters are
 * `param` rows, a function body's declarations are nodeless `local` rows,
 * and every `#include` is an `import` row whose local name is the header's
 * basename without its extension — the include regex and the static-function
 * source check are gone.
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

const C = `#include <stdio.h>
#include "util/helpers.h"

static int counter = 0;
int shared = 1;

static void
helper(const char *name, int *out)
{
    int local = 3;
    char *copy = NULL;
    *out = local;
}

int run(int argc, char **argv)
{
    helper(argv[0], &counter);
    return argc;
}
`;

const by = (rows: Binding[], name: string, kind?: Binding['kind']) =>
  rows.find((b) => b.name === name && (kind === undefined || b.kind === kind));

describe.skipIf(!kernelBuilt)('C/C++ bindings', () => {
  const rows = kernelBuilt ? tryKernelExtract('src/main.c', C, 'c')?.bindings ?? [] : [];

  it('file-level definitions are exported `decl` rows; `static` is recorded as storage', () => {
    expect(by(rows, 'run')).toMatchObject({ kind: 'decl', exportedAs: 'run', exportForm: 'public' });
    expect(by(rows, 'run')!.storage).toBeUndefined();
    expect(by(rows, 'helper')).toMatchObject({ kind: 'decl', exportedAs: 'helper', storage: 'static' });
    expect(by(rows, 'shared')).toMatchObject({ kind: 'decl', exportedAs: 'shared' });
    expect(by(rows, 'counter')).toMatchObject({ kind: 'decl', storage: 'static' });
    expect(by(rows, 'helper')!.nodeId).toBeDefined();
  });

  it('parameters and body declarations are scoped rows', () => {
    expect(by(rows, 'name')).toMatchObject({ kind: 'param', scopeStart: 7, scopeEnd: 13 });
    expect(by(rows, 'out')).toMatchObject({ kind: 'param' });
    expect(by(rows, 'argv')).toMatchObject({ kind: 'param', scopeStart: 15, scopeEnd: 19 });
    expect(by(rows, 'local')).toMatchObject({ kind: 'local', scopeStart: 7, scopeEnd: 13 });
    expect(by(rows, 'copy')).toMatchObject({ kind: 'local' });
    expect(by(rows, 'local')!.nodeId).toBeUndefined();
  });

  it('every #include is an `import` row named by the header basename', () => {
    expect(by(rows, 'stdio')).toMatchObject({ kind: 'import', targetSpec: 'stdio.h', targetName: '*' });
    expect(by(rows, 'helpers')).toMatchObject({ kind: 'import', targetSpec: 'util/helpers.h' });
    const mappings = importMappingsFromBindings(rows)!;
    expect(mappings.find((m) => m.localName === 'helpers')).toMatchObject({ source: 'util/helpers.h', isNamespace: true });
  });
});
