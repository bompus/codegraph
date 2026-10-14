/**
 * Edge cases of how a C++ declaration's type is read for a call's receiver, and
 * which overloads a call reaches:
 *
 * - `Foo* __restrict p` is a `Foo` pointer, as `Foo* const p` is.
 * - An annotation macro without arguments after a declared name
 *   (`Box* b ABSL_ATTRIBUTE_LIFETIME_BOUND;`) does not hide the declaration.
 * - A `const` pointer to a class template (`const Box<int>* b`) is read as a
 *   `Box`.
 * - A deleted overload is none a call reaches, however many lines its
 *   declaration runs over before the `= delete;`.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function indexed(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-declarator-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

const lines = (...text: string[]): string => [...text, ''].join('\n');

/** What the calls at `line` of the function or method `qualifiedName` reach, as `target:its first line @confidence`. */
function callsAt(cg: CodeGraph, qualifiedName: string, line: number): string[] {
  const from = [...cg.getNodesByKind('method'), ...cg.getNodesByKind('function')].find((n) => n.qualifiedName === qualifiedName)!;
  return cg.getOutgoingEdgesFrom([from.id], ['calls'])
    .filter((e) => e.line === line)
    .map((e) => {
      const t = cg.getNode(e.target)!;
      return `${t.qualifiedName}:${t.startLine} @${(e.metadata as { confidence?: number } | undefined)?.confidence}`;
    })
    .sort();
}

// `Open` is also a namesake class's, so a guess by the name can miss.
const BOXES = lines(
  'template <typename T>',
  'class Box {',
  ' public:',
  '  void Open() {}',
  '};',
  'class Cask {',
  ' public:',
  '  void Open() {}',
  '};',
);

describe('C++ declarations read for a call receiver', () => {
  it('reads `Box* __restrict` as a Box pointer', async () => {
    const cg = await indexed({
      'box.h': BOXES,
      'use.cc': lines('#include "box.h"', 'void Use(Cask* __restrict c) {', '  c->Open();', '}'),
    });
    try {
      expect(callsAt(cg, 'Use', 3)).toEqual(['Cask::Open:8 @0.9']);
    } finally {
      cg.close();
    }
  });

  it('reads a member followed by an annotation macro that takes no arguments', async () => {
    const cg = await indexed({
      'box.h': BOXES,
      'owner.h': lines(
        '#include "box.h"',
        '#define LIFETIME_BOUND_ANNOTATION',
        'class Owner {',
        ' public:',
        '  void Run() { cask_->Open(); }',
        ' private:',
        '  Cask* const cask_ LIFETIME_BOUND_ANNOTATION;',
        '};',
      ),
    });
    try {
      expect(callsAt(cg, 'Owner::Run', 5)).toEqual(['Cask::Open:8 @0.9']);
    } finally {
      cg.close();
    }
  });

  it('reads a const pointer to a class template as the template', async () => {
    const cg = await indexed({
      'box.h': BOXES,
      'use.cc': lines('#include "box.h"', 'void Use(const Box<int>* b) {', '  b->Open();', '}'),
    });
    try {
      expect(callsAt(cg, 'Use', 3)).toEqual(['Box::Open:4 @0.9']);
    } finally {
      cg.close();
    }
  });
});

describe('C++ deleted overloads', () => {
  it('takes no deleted overload whose declaration runs over many lines', async () => {
    const comments = Array.from({ length: 14 }, (_, i) => `      // reason ${i + 1}`);
    const cg = await indexed({
      'meter.h': lines(
        'class Meter {',
        ' public:',
        '  void Scale(',
        ...comments,
        '      double factor) = delete;',
        '  void Scale(int factor) {}',
        '  void Run() { Scale(2); }',
        '};',
      ),
    });
    try {
      expect(callsAt(cg, 'Meter::Run', 20)).toEqual(['Meter::Scale:19 @0.9']);
    } finally {
      cg.close();
    }
  });
});
