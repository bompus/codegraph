/**
 * Java and Kotlin binding rows from the kernel walkers (Phase 3 of
 * docs/design/resolution-binding-model-plan.md).
 *
 * A file-level declaration is a `decl` row exported unless its modifier
 * narrows it (`private` / `internal` are not visible across files, and Java's
 * package-private default is `storage = package`); members are node-backed
 * `local` rows scoped to their class; parameters are `param` rows; a method
 * body's variables are nodeless `local` rows; every non-wildcard import is an
 * `import` row (the FQN as `targetSpec`, the alias or last segment as the
 * local name) — the JVM import regex is gone.
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

const JAVA = `package com.example.pets;

import java.util.List;
import static java.util.Collections.emptyList;
import com.example.util.*;

public class OwnerService {
    private final List<String> names = emptyList();
    public String greet(String who, int... extra) {
        String reply = "hi " + who;
        return reply;
    }
}

class Helper {
    void run() {}
}
`;

const KOTLIN = `package com.example.pets

import java.util.UUID
import com.example.util.Formatter as Fmt
import com.example.util.*

private const val SECRET = 1
internal val counter = 2
val shared = 3

class OwnerService(val repo: String) {
    fun greet(who: String, count: Int): String {
        val reply = "hi " + who
        return reply
    }
}

private fun helper() = UUID.randomUUID()
`;

const by = (rows: Binding[], name: string, kind?: Binding['kind']) =>
  rows.find((b) => b.name === name && (kind === undefined || b.kind === kind));

describe.skipIf(!kernelBuilt)('Java bindings', () => {
  const rows = kernelBuilt ? tryKernelExtract('src/main/java/com/example/pets/OwnerService.java', JAVA, 'java')?.bindings ?? [] : [];

  it('file-level classes carry their modifier; members are local to the class', () => {
    expect(by(rows, 'OwnerService')).toMatchObject({ kind: 'decl', exportedAs: 'OwnerService', exportForm: 'public' });
    expect(by(rows, 'Helper')).toMatchObject({ kind: 'decl', storage: 'package' });
    expect(by(rows, 'Helper')!.exportedAs).toBeUndefined();
    expect(by(rows, 'greet')).toMatchObject({ kind: 'local', scopeStart: 7, scopeEnd: 13 });
    expect(by(rows, 'names')).toMatchObject({ kind: 'local' });
  });

  it('parameters and method-body variables are scoped rows', () => {
    expect(by(rows, 'who')).toMatchObject({ kind: 'param', scopeStart: 9, scopeEnd: 12 });
    expect(by(rows, 'extra')).toMatchObject({ kind: 'param' });
    expect(by(rows, 'reply')).toMatchObject({ kind: 'local', scopeStart: 9, scopeEnd: 12 });
    expect(by(rows, 'reply')!.nodeId).toBeUndefined();
  });

  it('imports: plain and static bind the last segment; a wildcard binds nothing', () => {
    expect(by(rows, 'List')).toMatchObject({ kind: 'import', targetSpec: 'java.util.List', targetName: 'List' });
    expect(by(rows, 'emptyList')).toMatchObject({ kind: 'import', targetSpec: 'java.util.Collections.emptyList' });
    expect(rows.filter((b) => b.kind === 'import')).toHaveLength(2);
    expect(importMappingsFromBindings(rows)!.find((m) => m.localName === 'List')).toMatchObject({ source: 'java.util.List', exportedName: 'List', isNamespace: false });
  });
});

describe.skipIf(!kernelBuilt)('Kotlin bindings', () => {
  const rows = kernelBuilt ? tryKernelExtract('src/main/kotlin/com/example/pets/OwnerService.kt', KOTLIN, 'kotlin')?.bindings ?? [] : [];

  it('visibility modifiers decide the export: public by default, private and internal are not', () => {
    expect(by(rows, 'OwnerService')).toMatchObject({ kind: 'decl', exportedAs: 'OwnerService', exportForm: 'public' });
    expect(by(rows, 'shared')).toMatchObject({ kind: 'decl', exportedAs: 'shared' });
    expect(by(rows, 'SECRET')).toMatchObject({ kind: 'decl', storage: 'private' });
    expect(by(rows, 'SECRET')!.exportedAs).toBeUndefined();
    expect(by(rows, 'counter')).toMatchObject({ kind: 'decl', storage: 'internal' });
    expect(by(rows, 'helper')).toMatchObject({ kind: 'decl', storage: 'private' });
  });

  it('parameters and function-body variables are scoped rows', () => {
    expect(by(rows, 'who')).toMatchObject({ kind: 'param', scopeStart: 12, scopeEnd: 15 });
    expect(by(rows, 'count')).toMatchObject({ kind: 'param' });
    expect(by(rows, 'reply')).toMatchObject({ kind: 'local', scopeStart: 12, scopeEnd: 15 });
  });

  it('imports: the alias or last segment; a wildcard binds nothing', () => {
    expect(by(rows, 'UUID')).toMatchObject({ kind: 'import', targetSpec: 'java.util.UUID' });
    expect(by(rows, 'Fmt')).toMatchObject({ kind: 'import', targetSpec: 'com.example.util.Formatter' });
    expect(rows.filter((b) => b.kind === 'import')).toHaveLength(2);
  });
});
