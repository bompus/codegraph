/**
 * Python binding rows from the kernel walker (Phase 3 of
 * docs/design/resolution-binding-model-plan.md).
 *
 * Every module-level definition is a `decl` row exported as itself (`public`;
 * a leading underscore is `storage = private`, the convention), a definition
 * inside a function or class is `local`, parameters are `param` rows, an
 * assignment inside a function is a nodeless `local`, and every import form
 * is an `import` row the resolver's import mappings are built from — the
 * Python import regex is gone.
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

const SOURCE = `import os
import os.path as osp
import json, sys
from utils import helper, Widget as W
from .models import User
from ..services import auth_service
from pkg import *

TIMEOUT = 30
_cache = {}

def load(path, *args, retries=3, **kw):
    from . import db
    data = json.loads(path)
    def inner(x):
        return x
    return inner(data)

class Service:
    def run(self, item: str) -> str:
        result = helper(item)
        return result
`;

const by = (rows: Binding[], name: string, kind?: Binding['kind']) =>
  rows.find((b) => b.name === name && (kind === undefined || b.kind === kind));

describe.skipIf(!kernelBuilt)('Python bindings', () => {
  const result = kernelBuilt ? tryKernelExtract('src/main.py', SOURCE, 'python') : null;
  const rows = result?.bindings ?? [];

  it('module-level definitions are public `decl` rows; underscore names are private by convention', () => {
    expect(by(rows, 'TIMEOUT')).toMatchObject({ kind: 'decl', exportedAs: 'TIMEOUT', exportForm: 'public', scopeStart: 1 });
    expect(by(rows, '_cache')).toMatchObject({ kind: 'decl', exportedAs: '_cache', storage: 'private' });
    expect(by(rows, 'load')).toMatchObject({ kind: 'decl', exportedAs: 'load' });
    expect(by(rows, 'Service')).toMatchObject({ kind: 'decl', exportedAs: 'Service' });
    expect(by(rows, 'load')!.nodeId).toBeDefined();
  });

  it('nested definitions and function-body assignments are `local`; parameters are `param`', () => {
    expect(by(rows, 'inner')).toMatchObject({ kind: 'local', scopeStart: 12, scopeEnd: 17 });
    expect(by(rows, 'data')).toMatchObject({ kind: 'local', scopeStart: 12, scopeEnd: 17 });
    expect(by(rows, 'data')!.nodeId).toBeUndefined();
    expect(by(rows, 'run')).toMatchObject({ kind: 'local', scopeStart: 19, scopeEnd: 22 });
    expect(by(rows, 'result')).toMatchObject({ kind: 'local', scopeStart: 20, scopeEnd: 22 });
    for (const p of ['path', 'args', 'retries', 'kw']) expect(by(rows, p), p).toMatchObject({ kind: 'param', scopeStart: 12, scopeEnd: 17 });
    expect(by(rows, 'x')).toMatchObject({ kind: 'param', scopeStart: 15, scopeEnd: 16 });
    expect(by(rows, 'item')).toMatchObject({ kind: 'param', scopeStart: 20, scopeEnd: 22 });
  });

  it('every import form is an `import` row', () => {
    expect(by(rows, 'os')).toMatchObject({ kind: 'import', targetSpec: 'os', targetName: '*' });
    expect(by(rows, 'osp')).toMatchObject({ kind: 'import', targetSpec: 'os.path', targetName: '*' });
    expect(by(rows, 'json')).toMatchObject({ kind: 'import', targetSpec: 'json' });
    expect(by(rows, 'sys')).toMatchObject({ kind: 'import', targetSpec: 'sys' });
    expect(by(rows, 'helper')).toMatchObject({ kind: 'import', targetSpec: 'utils', targetName: 'helper' });
    expect(by(rows, 'W')).toMatchObject({ kind: 'import', targetSpec: 'utils', targetName: 'Widget' });
    expect(by(rows, 'User')).toMatchObject({ kind: 'import', targetSpec: '.models', targetName: 'User' });
    expect(by(rows, 'auth_service')).toMatchObject({ kind: 'import', targetSpec: '..services' });
    expect(rows.some((b) => b.name === '*')).toBe(false);
    // An import inside a function body is a row scoped to that function.
    expect(by(rows, 'db')).toMatchObject({ kind: 'import', targetSpec: '.', targetName: 'db', scopeStart: 12, scopeEnd: 17 });
  });

  it('the import mappings the resolver reads come from the rows', () => {
    const mappings = importMappingsFromBindings(rows)!;
    expect(mappings.find((m) => m.localName === 'osp')).toMatchObject({ source: 'os.path', isNamespace: true });
    expect(mappings.find((m) => m.localName === 'W')).toMatchObject({ source: 'utils', exportedName: 'Widget', isNamespace: false });
  });
});
