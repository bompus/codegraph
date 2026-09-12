/**
 * PHP binding rows from the kernel walker (Phase 3 of
 * docs/design/resolution-binding-model-plan.md).
 *
 * Every file-level declaration is a `decl` row exported as itself (PHP has no
 * file-level visibility); members are node-backed `local` rows; parameters
 * are `param` rows; `$x = …` in a function body is a nodeless `local`; every
 * `use` clause — single, aliased, grouped, `use function` / `use const` — is
 * an `import` row the resolver's mappings come from. A trait `use` inside a
 * class body is not an import. The PHP import regex is gone.
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

const SOURCE = `<?php
namespace App\\Http;

use App\\Models\\User;
use App\\Services\\Mailer as Mail;
use App\\Support\\{Str, Arr as Arrays};
use function App\\Helpers\\slug;

class UserController
{
    use LogsActivity;

    private int $count = 0;

    public function show(User $user, string ...$rest): string
    {
        $name = slug($user->name);
        return $name;
    }
}

function helper(int $n): int
{
    return $n;
}
`;

const by = (rows: Binding[], name: string, kind?: Binding['kind']) =>
  rows.find((b) => b.name === name && (kind === undefined || b.kind === kind));

describe.skipIf(!kernelBuilt)('PHP bindings', () => {
  const rows = kernelBuilt ? tryKernelExtract('app/Http/UserController.php', SOURCE, 'php')?.bindings ?? [] : [];

  it('file-level declarations are exported `decl` rows; members are local to the class', () => {
    expect(by(rows, 'UserController')).toMatchObject({ kind: 'decl', exportedAs: 'UserController', exportForm: 'public' });
    expect(by(rows, 'helper')).toMatchObject({ kind: 'decl', exportedAs: 'helper' });
    expect(by(rows, 'show')).toMatchObject({ kind: 'local', scopeStart: 9, scopeEnd: 20 });
    expect(by(rows, 'count')?.kind ?? by(rows, '$count')?.kind).toBe('local');
  });

  it('parameters and body variables are scoped rows, as written', () => {
    expect(by(rows, '$user')).toMatchObject({ kind: 'param', scopeStart: 15, scopeEnd: 19 });
    expect(by(rows, '$rest')).toMatchObject({ kind: 'param' });
    expect(by(rows, '$name')).toMatchObject({ kind: 'local', scopeStart: 15, scopeEnd: 19 });
    expect(by(rows, '$name')!.nodeId).toBeUndefined();
    expect(by(rows, '$n')).toMatchObject({ kind: 'param', scopeStart: 22, scopeEnd: 25 });
  });

  it('every `use` clause is an `import` row; a trait use is not', () => {
    expect(by(rows, 'User')).toMatchObject({ kind: 'import', targetSpec: 'App\\Models\\User', targetName: 'User' });
    expect(by(rows, 'Mail')).toMatchObject({ kind: 'import', targetSpec: 'App\\Services\\Mailer' });
    expect(by(rows, 'Str')).toMatchObject({ kind: 'import', targetSpec: 'App\\Support\\Str' });
    expect(by(rows, 'Arrays')).toMatchObject({ kind: 'import', targetSpec: 'App\\Support\\Arr' });
    expect(by(rows, 'slug')).toMatchObject({ kind: 'import', targetSpec: 'App\\Helpers\\slug' });
    expect(by(rows, 'LogsActivity', 'import')).toBeUndefined();
    const mappings = importMappingsFromBindings(rows)!;
    expect(mappings.find((m) => m.localName === 'Mail')).toMatchObject({ source: 'App\\Services\\Mailer', exportedName: 'Mail' });
  });
});
