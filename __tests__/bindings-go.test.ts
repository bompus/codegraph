/**
 * Go binding rows from the kernel walker (Phase 3 of
 * docs/design/resolution-binding-model-plan.md).
 *
 * Go exports by case: a capitalized package-level name is a `decl` row
 * exported as itself (`public`), a lowercase one has `storage = package`.
 * Parameters and receivers are `param` rows; `x := …`, `var x` and `for i, v
 * := range` inside a function are nodeless `local` rows; every import spec
 * is an `import` row the resolver's mappings are built from — the Go import
 * regex is gone.
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

const SOURCE = `package payroll

import "fmt"
import (
	"strings"
	str "strconv"
	_ "embed"
	"example.com/proj/internal/bytesconv"
)

const MaxRate = 3
var counter int

type Employee struct {
	Name string
}

func (e *Employee) Pay(rate int, bonus ...int) int {
	total := rate * 2
	var note string
	for i, b := range bonus {
		total += i + b
	}
	_ = note
	return total
}

func helper(x int) int {
	return x
}
`;

const by = (rows: Binding[], name: string, kind?: Binding['kind']) =>
  rows.find((b) => b.name === name && (kind === undefined || b.kind === kind));

describe.skipIf(!kernelBuilt)('Go bindings', () => {
  const result = kernelBuilt ? tryKernelExtract('internal/payroll/pay.go', SOURCE, 'go') : null;
  const rows = result?.bindings ?? [];

  it('package-level names are `decl` rows; case decides the export', () => {
    expect(by(rows, 'MaxRate')).toMatchObject({ kind: 'decl', exportedAs: 'MaxRate', exportForm: 'public' });
    expect(by(rows, 'Employee')).toMatchObject({ kind: 'decl', exportedAs: 'Employee', exportForm: 'public' });
    expect(by(rows, 'Pay')).toMatchObject({ kind: 'decl', exportedAs: 'Pay' });
    expect(by(rows, 'counter')).toMatchObject({ kind: 'decl', storage: 'package' });
    expect(by(rows, 'counter')!.exportedAs).toBeUndefined();
    expect(by(rows, 'helper')).toMatchObject({ kind: 'decl', storage: 'package' });
    expect(by(rows, 'helper')!.nodeId).toBeDefined();
  });

  it('parameters, receivers and function-body names are scoped rows', () => {
    expect(by(rows, 'e')).toMatchObject({ kind: 'param', scopeStart: 18, scopeEnd: 26 });
    expect(by(rows, 'rate')).toMatchObject({ kind: 'param', scopeStart: 18, scopeEnd: 26 });
    expect(by(rows, 'bonus')).toMatchObject({ kind: 'param' });
    expect(by(rows, 'x')).toMatchObject({ kind: 'param', scopeStart: 28, scopeEnd: 30 });
    expect(by(rows, 'total')).toMatchObject({ kind: 'local', scopeStart: 18, scopeEnd: 26 });
    expect(by(rows, 'total')!.nodeId).toBeUndefined();
    expect(by(rows, 'note')).toMatchObject({ kind: 'local' });
    expect(by(rows, 'i')).toMatchObject({ kind: 'local' });
    expect(by(rows, 'b')).toMatchObject({ kind: 'local' });
  });

  it('every import spec is an `import` row: alias, dot/blank as spelled, else the last path segment', () => {
    expect(by(rows, 'fmt')).toMatchObject({ kind: 'import', targetSpec: 'fmt', targetName: '*' });
    expect(by(rows, 'strings')).toMatchObject({ kind: 'import', targetSpec: 'strings' });
    expect(by(rows, 'str')).toMatchObject({ kind: 'import', targetSpec: 'strconv' });
    expect(by(rows, '_')).toMatchObject({ kind: 'import', targetSpec: 'embed' });
    expect(by(rows, 'bytesconv')).toMatchObject({ kind: 'import', targetSpec: 'example.com/proj/internal/bytesconv' });
  });

  it('the import mappings the resolver reads come from the rows', () => {
    const mappings = importMappingsFromBindings(rows)!;
    expect(mappings.find((m) => m.localName === 'str')).toMatchObject({ source: 'strconv', isNamespace: true });
    expect(mappings.find((m) => m.localName === 'bytesconv')).toMatchObject({ source: 'example.com/proj/internal/bytesconv', isNamespace: true });
  });
});
