/**
 * Kernel parse-tree service ↔ wasm tree parity (Phase 3 of
 * docs/design/kernel-only-extraction-plan.md).
 *
 * `parseNativeTree` returns the whole CST as flat buffers wrapped in a facade
 * with the web-tree-sitter node surface. For every kernel-routed language's
 * torture fixture, walk the native facade and the wasm tree in lockstep and
 * assert node-for-node identity: type, named-ness, UTF-16 start/end index,
 * start/end position, child count, and the field each child fills. Then
 * assert the two read-time derivations built on the tree — the viewer's
 * syntax spans and the branch-guard walker — agree on both trees.
 *
 * This is a facade gate, not an error-recovery one: a fixture whose tree has
 * errors (the macro-heavy C/C++ torture files) is only checked for agreeing
 * on `hasError`, because recovery may legitimately differ (Phase 1).
 *
 * Skips without a kernel binary; CODEGRAPH_KERNEL_EXPECT=1 turns that into a
 * failure (kernel-scaffold.test.ts owns that assertion).
 */

import { describe, it, expect, beforeAll, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { getParser, initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { parseNativeTree } from '../src/extraction/kernel/tree';
import { resetKernelForTests } from '../src/extraction/kernel';
import { parseSourceTree, type TreeNode } from '../src/extraction/parse-tree';
import { classifyTree } from '../src/extraction/syntax-tokens';
import { guardsInSource, supportsBranchGuards } from '../src/graph/branch-guards';
import type { Language } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname,
  '..',
  'codegraph-kernel',
  'prebuilds',
  `${process.platform}-${process.arch}`,
  'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

const FIXTURE_DIR = path.join(__dirname, 'fixtures', 'kernel-parity');

/** One fixture per routed language (+ a second grammar variant where one exists). */
const FIXTURES: Array<{ file: string; lang: Language }> = [
  { file: 'torture.js', lang: 'javascript' },
  { file: 'torture.js', lang: 'jsx' },
  { file: 'Torture.java', lang: 'java' },
  { file: 'torture.py', lang: 'python' },
  { file: 'torture.go', lang: 'go' },
  { file: 'torture.c', lang: 'c' },
  { file: 'torture.cpp', lang: 'cpp' },
  { file: 'torture.hpp', lang: 'cpp' },
  { file: 'torture.rs', lang: 'rust' },
  { file: 'Torture.cs', lang: 'csharp' },
  { file: 'torture.rb', lang: 'ruby' },
  { file: 'torture.php', lang: 'php' },
  { file: 'torture.swift', lang: 'swift' },
  { file: 'torture.kt', lang: 'kotlin' },
  { file: 'torture.R', lang: 'r' },
  { file: 'torture.lua', lang: 'lua' },
  { file: 'torture.luau', lang: 'luau' },
  { file: 'torture.scala', lang: 'scala' },
  { file: 'torture.dart', lang: 'dart' },
];
/** This repo's own sources, parsed as typescript / tsx. */
const REAL: Array<{ file: string; lang: Language }> = [
  { file: 'src/extraction/kernel/tree.ts', lang: 'typescript' },
  { file: 'src/graph/branch-guards.ts', lang: 'typescript' },
  { file: 'src/extraction/syntax-tokens.ts', lang: 'tsx' },
];

interface WasmLike {
  type: string;
  isNamed: boolean;
  startIndex: number;
  endIndex: number;
  startPosition: { row: number; column: number };
  endPosition: { row: number; column: number };
  childCount: number;
  child(i: number): WasmLike | null;
  fieldNameForChild?(i: number): string | null;
  parent: WasmLike | null;
}

/** Walk both trees in lockstep; return the first divergence as text, or null. */
function firstDivergence(a: TreeNode, b: WasmLike, pathDesc = 'root'): string | null {
  const here = `${pathDesc} <${b.type} ${b.startPosition.row}:${b.startPosition.column}>`;
  if (a.type !== b.type) return `${here}: type ${a.type} != ${b.type}`;
  if (a.isNamed !== b.isNamed) return `${here}: isNamed`;
  if (a.startIndex !== b.startIndex || a.endIndex !== b.endIndex)
    return `${here}: index ${a.startIndex}-${a.endIndex} != ${b.startIndex}-${b.endIndex}`;
  if (a.startPosition.row !== b.startPosition.row || a.startPosition.column !== b.startPosition.column)
    return `${here}: startPosition ${JSON.stringify(a.startPosition)} != ${JSON.stringify(b.startPosition)}`;
  if (a.endPosition.row !== b.endPosition.row || a.endPosition.column !== b.endPosition.column)
    return `${here}: endPosition ${JSON.stringify(a.endPosition)} != ${JSON.stringify(b.endPosition)}`;
  if (a.childCount !== b.childCount) return `${here}: childCount ${a.childCount} != ${b.childCount}`;
  if ((a.parent === null) !== (b.parent === null)) return `${here}: parent presence`;
  for (let i = 0; i < a.childCount; i++) {
    const ca = a.child(i)!;
    const cb = b.child(i)!;
    if (b.fieldNameForChild) {
      const fa = (ca as unknown as { fieldName: string | null }).fieldName;
      const fb = b.fieldNameForChild(i);
      if ((fa ?? null) !== (fb ?? null)) return `${here}: child ${i} field ${fa} != ${fb}`;
    }
    const d = firstDivergence(ca, cb, `${pathDesc}/${i}`);
    if (d) return d;
  }
  return null;
}

let savedKernelEnv: string | undefined;

describe.skipIf(!kernelBuilt)('kernel parse-tree service', () => {
  beforeAll(async () => {
    await initGrammars();
    await loadGrammarsForLanguages([...new Set([...FIXTURES, ...REAL].map((f) => f.lang))]);
  });
  beforeEach(() => {
    savedKernelEnv = process.env.CODEGRAPH_KERNEL;
    delete process.env.CODEGRAPH_KERNEL;
    resetKernelForTests();
  });
  afterEach(() => {
    if (savedKernelEnv === undefined) delete process.env.CODEGRAPH_KERNEL;
    else process.env.CODEGRAPH_KERNEL = savedKernelEnv;
    resetKernelForTests();
  });

  for (const { file, lang } of [...FIXTURES, ...REAL]) {
    const abs = file.startsWith('src/') ? path.join(__dirname, '..', file) : path.join(FIXTURE_DIR, file);

    it(`${file} as ${lang}: native facade matches the wasm tree node for node`, () => {
      const source = fs.readFileSync(abs, 'utf8');
      const native = parseNativeTree(source, lang);
      expect(native, 'kernel declined to parse').not.toBeNull();
      const wasm = getParser(lang)!.parse(source)!;
      try {
        expect(native!.rootNode.hasError).toBe(wasm.rootNode.hasError);
        // A tree with errors may legitimately differ between the parsers
        // (UTF-8 vs UTF-16 error recovery — Phase 1); the facade gate is
        // for clean trees. The macro-heavy C/C++ torture files are the
        // erroring ones.
        if (wasm.rootNode.hasError) return;
        expect(firstDivergence(native!.rootNode, wasm.rootNode as unknown as WasmLike)).toBeNull();
      } finally {
        wasm.delete();
      }
    });

    it(`${file} as ${lang}: syntax spans agree`, () => {
      const source = fs.readFileSync(abs, 'utf8');
      const native = parseNativeTree(source, lang)!;
      const wasm = getParser(lang)!.parse(source)!;
      try {
        const a = classifyTree(native.rootNode, source, lang);
        expect(a.length).toBeGreaterThan(10);
        if (wasm.rootNode.hasError) return;
        const b = classifyTree(wasm.rootNode as unknown as TreeNode, source, lang);
        expect(a).toEqual(b);
      } finally {
        wasm.delete();
      }
    });

    if (supportsBranchGuards(lang)) {
      it(`${file} as ${lang}: branch guards agree at every call site`, async () => {
        const source = fs.readFileSync(abs, 'utf8');
        // Every line that looks like a call site, so the walker's rules get
        // exercised broadly rather than at one hand-picked position.
        const lines = source.split('\n');
        const sites: Array<{ line: number; column: number }> = [];
        for (let i = 0; i < lines.length && sites.length < 60; i++) {
          const m = /[A-Za-z_][\w.]*\(/.exec(lines[i]!);
          if (m) sites.push({ line: i + 1, column: m.index });
        }
        expect(sites.length).toBeGreaterThan(0);
        const wasmTree = getParser(lang)!.parse(source)!;
        const erroring = wasmTree.rootNode.hasError;
        wasmTree.delete();
        if (erroring) return;
        for (const site of sites) {
          delete process.env.CODEGRAPH_KERNEL;
          const a = await guardsInSource(source, lang, site.line, site.column);
          process.env.CODEGRAPH_KERNEL = '0';
          const b = await guardsInSource(source, lang, site.line, site.column);
          delete process.env.CODEGRAPH_KERNEL;
          expect(a, `site ${site.line}:${site.column}`).toEqual(b);
        }
      });
    }
  }

  it('the shared seam prefers the kernel and honours the kill switch', async () => {
    const source = 'const x = f(1);\n';
    delete process.env.CODEGRAPH_KERNEL;
    const native = await parseSourceTree(source, 'typescript');
    expect(native).not.toBeNull();
    expect(native!.rootNode.constructor.name).toBe('NativeNode');
    process.env.CODEGRAPH_KERNEL = '0';
    const wasm = await parseSourceTree(source, 'typescript');
    expect(wasm).not.toBeNull();
    expect(wasm!.rootNode.constructor.name).not.toBe('NativeNode');
    wasm!.delete();
  });

  it('UTF-16 indexes are exact for non-ASCII source', () => {
    const source = 'const 名前 = "héllo 😀";\nfunction f(x) { return 名前 + x }\n';
    const native = parseNativeTree(source, 'typescript')!;
    const wasm = getParser('typescript')!.parse(source)!;
    try {
      expect(firstDivergence(native.rootNode, wasm.rootNode as unknown as WasmLike)).toBeNull();
      // And text slices round-trip through the JS string.
      const ident = native.rootNode.descendantForIndex(source.indexOf('名前'));
      expect(ident.text).toBe('名前');
    } finally {
      wasm.delete();
    }
  });
});
