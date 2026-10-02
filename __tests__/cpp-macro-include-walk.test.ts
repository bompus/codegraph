/**
 * #2127 — the C/C++ macro-visibility walk must stay bounded on deep include
 * graphs.
 *
 * A header whose guard the walk cannot decide, such as any header reached
 * under an unknown `#if`, would be re-scanned on every inclusion path: a
 * layered include graph where each header includes the two of the next layer
 * costs 2^depth header scans (upstream ran out of heap on osgEarth, whose
 * `#define X_H 1` guards its reader does not take for guards; the kernel's
 * does). The answers must not change: a macro definitely visible at the call
 * site is still a macro, and an unknown one still is not.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

type Guard = 'value' | 'bare' | 'unknown-flag';

/** `depth` layers of two headers; each includes both headers of the next layer. */
function layeredHeaders(depth: number, guard: Guard): Record<string, string> {
  const files: Record<string, string> = {};
  for (let d = 0; d < depth; d++) {
    for (const side of ['a', 'b']) {
      const g = `H_${d}_${side}`;
      const open =
        guard === 'value' ? `#ifndef ${g}\n#define ${g} 1\n` :
        guard === 'bare' ? `#ifndef ${g}\n#define ${g}\n` :
        `#ifdef USE_${d}\n`;
      const next = d + 1 < depth ? `#include "h${d + 1}_a.h"\n#include "h${d + 1}_b.h"\n` : '';
      files[`h${d}_${side}.h`] = `${open}${next}#define M_${d}_${side}(x) (x)\nint f_${d}_${side}(int);\n#endif\n`;
    }
  }
  return files;
}

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

/** Index `files` and return, per caller, the files its `calls` edges land in. */
async function calleeFiles(files: Record<string, string>, callers: string[]): Promise<Record<string, string[]>> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-walk-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), content);
  const cg = await CodeGraph.init(root, { index: true });
  try {
    const fn = (name: string) => cg.getNodesByKind('function').find((n) => n.name === name && n.filePath !== 'decoy.cpp')!;
    return Object.fromEntries(callers.map((name) => [
      name,
      cg.getCallees(fn(name).id).filter((r) => r.edge.kind === 'calls').map((r) => `${r.node.name}@${r.node.filePath}`).sort(),
    ]));
  } finally {
    cg.close();
  }
}

describe('#2127 — the macro-visibility include walk is bounded', () => {
  it.each(['value', 'bare', 'unknown-flag'] as const)(
    'a 40-layer include graph (2^40 inclusion paths) indexes (%s guards)',
    async (guard) => {
      const got = await calleeFiles({
        ...layeredHeaders(40, guard),
        'trace.h': '#define TRACE(v) ((void)(v))\n',
        'unit.cpp': '#include "h0_a.h"\n#include "h0_b.h"\nvoid early() { TRACE(0); }\n#include "trace.h"\nvoid unit() { TRACE(1); M_39_a(2); }\n',
        'decoy.cpp': 'void TRACE(int v) {}\nint M_39_a(int v) { return v; }\n',
      }, ['early', 'unit']);
      // A call above the include that defines TRACE is still a call; a header
      // macro under an undecidable `#ifdef` is not definitely visible. The
      // kernel reads `#define X_H 1` as a guard, so a value guard decides.
      expect(got).toEqual({
        early: ['TRACE@decoy.cpp'],
        unit: guard === 'unknown-flag' ? ['M_39_a@decoy.cpp'] : [],
      });
    },
    60_000,
  );

  it('a walk that cannot be shortened stops at its budget and suppresses nothing past it', async () => {
    // No guards and a known flag toggled on every inclusion: each visit
    // changes the state, so no re-entry repeats an earlier one and the real
    // preprocessor would expand all 2^22 paths too. The walk gives up instead:
    // before the exploding include the macro is still known, after it nothing is.
    const files: Record<string, string> = {
      'toggle.h': '#ifdef T\n#undef T\n#else\n#define T\n#endif\n',
      'unit.cpp': '#define T\n#define TRACE(v) ((void)(v))\nvoid before() { TRACE(1); }\n#include "h0.h"\nvoid after() { TRACE(2); }\n',
      'decoy.cpp': 'void TRACE(int v) {}\n',
    };
    for (let d = 0; d < 22; d++) {
      files[`h${d}.h`] = d + 1 < 22 ? `#include "toggle.h"\n#include "h${d + 1}.h"\n#include "h${d + 1}.h"\n` : '#include "toggle.h"\n';
    }
    expect(await calleeFiles(files, ['before', 'after'])).toEqual({
      before: [],
      after: ['TRACE@decoy.cpp'],
    });
  }, 60_000);
});

describe('#2127 — a walk cut off at its budget reads no type aliases', () => {
  it('an Objective-C superclass alias redefined past the budget is not the earlier one', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-walk-'));
    roots.push(root);
    const files: Record<string, string> = {
      'toggle.h': '#ifdef T\n#undef T\n#else\n#define T\n#endif\n',
      'Images.h': '@interface DesktopImage : NSObject\n- (id)initWithBytes:(id)bytes;\n@end\n@interface MobileImage : NSObject\n@end\n',
      'Images.m': '#import "Images.h"\n@implementation DesktopImage\n- (id)initWithBytes:(id)bytes { return self; }\n@end\n',
      // The walk runs out inside h0.h, so it never reads the redefinition.
      'Aliased.h': '#import "Images.h"\n#define T\n#define Image DesktopImage\n#include "h0.h"\n#undef Image\n#define Image MobileImage\n@interface Aliased : Image\n@end\n',
      'Aliased.m': '#import "Aliased.h"\n@implementation Aliased\n- (id)initWithBytes:(id)bytes { return [super initWithBytes:bytes]; }\n@end\n',
    };
    for (let d = 0; d < 22; d++) {
      files[`h${d}.h`] = d + 1 < 22 ? `#include "toggle.h"\n#include "h${d + 1}.h"\n#include "h${d + 1}.h"\n` : '#include "toggle.h"\n';
    }
    for (const [rel, content] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), content);
    const cg = await CodeGraph.init(root, { index: true });
    try {
      const ids = cg.getNodesInFile('Aliased.m').map((n) => n.id);
      const sends = cg.getOutgoingEdgesFrom(ids).filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!.qualifiedName);
      expect(sends).toEqual([]);
    } finally {
      cg.close();
    }
  }, 60_000);
});

describe('#2127 — indexing a deep include graph keeps #1838 macro suppression', () => {
  it('the macro reached through value-guarded headers still does not bind to the decoy function', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-walk-'));
    roots.push(root);
    const files = {
      ...layeredHeaders(24, 'value'),
      'trace.h': '#define TRACE(v) ((void)(v))\n',
      'unit.cpp': '#include "h0_a.h"\n#include "h0_b.h"\n#include "trace.h"\nvoid unit() { TRACE(1); }\n',
      'decoy.cpp': 'void TRACE(int v) {}\nvoid caller() { TRACE(2); }\n',
    };
    for (const [rel, content] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), content);
    const cg = await CodeGraph.init(root, { index: true });
    try {
      const fn = (name: string) => cg.getNodesByKind('function').find((n) => n.name === name)!;
      const callees = (name: string) =>
        cg.getCallees(fn(name).id).filter((r) => r.edge.kind === 'calls').map((r) => r.node.filePath);
      expect(callees('unit')).toEqual([]);
      expect(callees('caller')).toEqual(['decoy.cpp']);
    } finally {
      cg.close();
    }
  }, 60_000);
});
