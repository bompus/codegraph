/**
 * Kernel parse-tree service and the NativeNode facade (Phase 3 of
 * docs/design/kernel-only-extraction-plan.md; the wasm comparison arm went
 * with the wasm path in Phase 5 — the facade is now checked against its own
 * invariants and against the source text it indexes).
 *
 * For every torture fixture plus three of this repo's own sources:
 *   - the tree parses, its root spans the whole file, and `hasError` is
 *     false on the fixtures known to be clean;
 *   - every node's text slice equals `source.slice(startIndex, endIndex)`,
 *     children lie inside their parent in order, parent links round-trip,
 *     and the sibling/field accessors agree with the child table;
 *   - the two read-time derivations built on the tree — syntax spans and
 *     branch guards — run and are deterministic across two parses.
 *
 * Skips without a kernel binary; CODEGRAPH_KERNEL_EXPECT=1 turns that into a
 * failure (kernel-scaffold.test.ts owns that assertion).
 */

import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { parseNativeTree, NativeNode } from '../src/extraction/kernel/tree';
import { parseSourceTree, type TreeNode } from '../src/extraction/parse-tree';
import { classifyTree } from '../src/extraction/syntax-tokens';
import { guardsInSource, supportsBranchGuards } from '../src/graph/branch-guards';
import type { Language } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);
const FIXTURE_DIR = path.join(__dirname, 'fixtures', 'kernel-parity');

/** One fixture per routed language (+ a second grammar variant where one exists). */
const FIXTURES: Array<{ file: string; lang: Language; clean: boolean }> = [
  { file: 'torture.js', lang: 'javascript', clean: true },
  { file: 'torture.js', lang: 'jsx', clean: true },
  { file: 'Torture.java', lang: 'java', clean: true },
  { file: 'torture.py', lang: 'python', clean: true },
  { file: 'torture.go', lang: 'go', clean: true },
  // The macro-heavy C/C++ torture files carry parse errors by design.
  { file: 'torture.c', lang: 'c', clean: false },
  { file: 'torture.cpp', lang: 'cpp', clean: false },
  { file: 'torture.hpp', lang: 'cpp', clean: false },
  { file: 'torture.rs', lang: 'rust', clean: true },
  { file: 'Torture.cs', lang: 'csharp', clean: true },
  { file: 'torture.rb', lang: 'ruby', clean: true },
  { file: 'torture.php', lang: 'php', clean: true },
  { file: 'torture.swift', lang: 'swift', clean: true },
  { file: 'torture.kt', lang: 'kotlin', clean: true },
  { file: 'torture.R', lang: 'r', clean: true },
  { file: 'torture.lua', lang: 'lua', clean: true },
  { file: 'torture.luau', lang: 'luau', clean: true },
  { file: 'torture.scala', lang: 'scala', clean: true },
  { file: 'torture.dart', lang: 'dart', clean: true },
];
const REAL: Array<{ file: string; lang: Language; clean: boolean }> = [
  { file: 'src/extraction/kernel/tree.ts', lang: 'typescript', clean: true },
  { file: 'src/graph/branch-guards.ts', lang: 'typescript', clean: true },
  { file: 'src/extraction/syntax-tokens.ts', lang: 'tsx', clean: true },
];

/** Walk the whole tree checking the structural invariants; return the first violation. */
function firstViolation(root: TreeNode, source: string): string | null {
  let count = 0;
  const stack: TreeNode[] = [root];
  while (stack.length) {
    const n = stack.pop()!;
    count++;
    const here = `<${n.type} ${n.startPosition.row}:${n.startPosition.column}>`;
    if (n.text !== source.slice(n.startIndex, n.endIndex)) return `${here}: text != source slice`;
    if (n.endIndex < n.startIndex) return `${here}: negative extent`;
    const kids = n.children;
    if (kids.length !== n.childCount) return `${here}: children.length != childCount`;
    if (n.namedChildren.length !== n.namedChildCount) return `${here}: namedChildren.length != namedChildCount`;
    let prevEnd = n.startIndex;
    for (let i = 0; i < kids.length; i++) {
      const c = kids[i]!;
      if (c.parent?.id !== n.id) return `${here}: child ${i} parent link`;
      if (c.startIndex < prevEnd || c.endIndex > n.endIndex) return `${here}: child ${i} outside parent or out of order`;
      prevEnd = c.endIndex;
      if (n.child(i)?.id !== c.id) return `${here}: child(${i}) != children[${i}]`;
      if (i > 0 && c.previousSibling?.id !== kids[i - 1]!.id) return `${here}: previousSibling of child ${i}`;
      if (i < kids.length - 1 && c.nextSibling?.id !== kids[i + 1]!.id) return `${here}: nextSibling of child ${i}`;
      const f = n.fieldNameForChild(i);
      if (f && n.childForFieldName(f) === null) return `${here}: field ${f} not resolvable`;
      stack.push(c);
    }
  }
  return count > 0 ? null : 'empty tree';
}

describe.skipIf(!kernelBuilt)('kernel parse-tree service', () => {
  for (const { file, lang, clean } of [...FIXTURES, ...REAL]) {
    const abs = file.startsWith('src/') ? path.join(__dirname, '..', file) : path.join(FIXTURE_DIR, file);

    it(`${file} as ${lang}: facade invariants hold over the whole tree`, () => {
      const source = fs.readFileSync(abs, 'utf8');
      const tree = parseNativeTree(source, lang);
      expect(tree, 'kernel declined to parse').not.toBeNull();
      const root = tree!.rootNode;
      expect(root.startIndex).toBe(0);
      expect(root.endIndex).toBe(source.length);
      expect(root.hasError).toBe(!clean);
      expect(firstViolation(root, source)).toBeNull();
    });

    it(`${file} as ${lang}: syntax spans cover the file and are deterministic`, () => {
      const source = fs.readFileSync(abs, 'utf8');
      const a = classifyTree(parseNativeTree(source, lang)!.rootNode, source, lang);
      const b = classifyTree(parseNativeTree(source, lang)!.rootNode, source, lang);
      expect(a.length).toBeGreaterThan(10);
      expect(a).toEqual(b);
      let prev = 0;
      for (const span of a) {
        expect(span.start).toBeGreaterThanOrEqual(prev);
        expect(span.end).toBeGreaterThan(span.start);
        prev = span.end;
      }
    });

    if (supportsBranchGuards(lang) && clean) {
      it(`${file} as ${lang}: branch guards are deterministic at every call site`, async () => {
        const source = fs.readFileSync(abs, 'utf8');
        const lines = source.split('\n');
        const sites: Array<{ line: number; column: number }> = [];
        for (let i = 0; i < lines.length && sites.length < 60; i++) {
          const m = /[A-Za-z_][\w.]*\(/.exec(lines[i]!);
          if (m) sites.push({ line: i + 1, column: m.index });
        }
        expect(sites.length).toBeGreaterThan(0);
        for (const site of sites) {
          const a = await guardsInSource(source, lang, site.line, site.column);
          const b = await guardsInSource(source, lang, site.line, site.column);
          expect(a, `site ${site.line}:${site.column}`).toEqual(b);
        }
      });
    }
  }

  it('the shared seam returns the kernel facade', async () => {
    const tree = await parseSourceTree('const x = f(1);\n', 'typescript');
    expect(tree).not.toBeNull();
    expect(tree!.rootNode).toBeInstanceOf(NativeNode);
    tree!.delete();
  });

  it('a language the kernel has no grammar for parses to null, not a throw', async () => {
    expect(await parseSourceTree('anything', 'markdown')).toBeNull();
  });

  it('UTF-16 indexes are exact for non-ASCII source', () => {
    const source = 'const 名前 = "héllo 😀";\nfunction f(x) { return 名前 + x }\n';
    const root = parseNativeTree(source, 'typescript')!.rootNode;
    expect(firstViolation(root, source)).toBeNull();
    const ident = root.descendantForIndex(source.indexOf('名前'))!;
    expect(ident.text).toBe('名前');
    const str = root.descendantForIndex(source.indexOf('😀'))!;
    expect(str.text.includes('😀')).toBe(true);
    expect(root.endPosition.row).toBe(2);
  });

  it('hidden-rule fields resolve through childForFieldName but not fieldNameForChild', () => {
    // Swift attaches `return_type` through a hidden intermediate rule.
    const root = parseNativeTree('func g() -> Session? { nil }\n', 'swift')!.rootNode;
    const fn = root.namedChildren.find((n) => n.type === 'function_declaration')!;
    const rt = fn.childForFieldName('return_type');
    expect(rt).not.toBeNull();
    expect(rt!.type).toBe('optional_type');
  });
});
