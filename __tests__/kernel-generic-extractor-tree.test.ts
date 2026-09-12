/**
 * Walker ≡ generic extractor on the kernel's tree (Phase 4 of
 * docs/design/kernel-only-extraction-plan.md; re-based in Phase 5 when the
 * wasm comparison arm was removed).
 *
 * Two extractors exist for a walker language: the bespoke Rust walker
 * (`tryKernelExtract`) and the generic TypeScript `TreeSitterExtractor`
 * walking the same tree through the NativeNode facade. Every walker shipped
 * only after proving byte-parity with the generic extractor, and the
 * generic extractor is the path for every language WITHOUT a walker, for a
 * stack-guard defer, and for SFC blocks. So the two must agree on every
 * clean torture fixture, as canonical multisets. Erroring C/C++ fixtures are
 * compared loosely.
 *
 * The tail languages (no walker) are checked for extracting real symbols.
 */

import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { extractFromSource } from '../src/extraction';
import { TreeSitterExtractor } from '../src/extraction/tree-sitter';
import { tryKernelExtract } from '../src/extraction/kernel';
import type { ExtractionResult, Language } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);
const FIXTURE_DIR = path.join(__dirname, 'fixtures', 'kernel-parity');

const EXT_LANG: Record<string, Language> = {
  '.js': 'javascript', '.java': 'java', '.py': 'python', '.go': 'go', '.c': 'c', '.cpp': 'cpp', '.hpp': 'cpp',
  '.rs': 'rust', '.cs': 'csharp', '.rb': 'ruby', '.php': 'php', '.swift': 'swift', '.kt': 'kotlin', '.kts': 'kotlin',
  '.R': 'r', '.lua': 'lua', '.luau': 'luau', '.scala': 'scala', '.sc': 'scala', '.dart': 'dart',
};
const ERRORING = new Set(['torture.c', 'torture.cpp']);

function canon(r: ExtractionResult) {
  return {
    nodes: r.nodes.map(({ updatedAt: _u, ...n }) => JSON.stringify(n, Object.keys(n).sort())).sort(),
    edges: r.edges.map((e) => JSON.stringify(e, Object.keys(e).sort())).sort(),
    refs: r.unresolvedReferences.map((x) => JSON.stringify(x, Object.keys(x).sort())).sort(),
  };
}

/** Languages the kernel only parses: the generic extractor is their only path. */
const TAIL: Array<{ rel: string; lang: Language; minSymbols: number }> = [
  { rel: 'objc/Greeter.m', lang: 'objc', minSymbols: 3 },
  { rel: 'erlang/counter.erl', lang: 'erlang', minSymbols: 3 },
  { rel: 'nix/default.nix', lang: 'nix', minSymbols: 3 },
  { rel: 'pascal/Greeter.pas', lang: 'pascal', minSymbols: 3 },
  { rel: 'solidity/Greeter.sol', lang: 'solidity', minSymbols: 3 },
  { rel: 'arkts/Greeter.ets', lang: 'arkts', minSymbols: 3 },
  { rel: 'terraform/main.tf', lang: 'terraform', minSymbols: 3 },
  { rel: 'vbnet/Greeter.vb', lang: 'vbnet', minSymbols: 3 },
  { rel: 'cobol/GREETER.cbl', lang: 'cobol', minSymbols: 3 },
  { rel: 'cfml/Greeter.cfc', lang: 'cfml', minSymbols: 3 },
  { rel: 'cfml/greet.cfm', lang: 'cfml', minSymbols: 1 },
];

describe.skipIf(!kernelBuilt)('walker and generic extractor agree on the kernel tree', () => {
  const fixtures = fs
    .readdirSync(FIXTURE_DIR)
    .filter((f) => EXT_LANG[path.extname(f)])
    .map((f) => ({ file: path.join(FIXTURE_DIR, f), lang: EXT_LANG[path.extname(f)]! }));

  it('has fixtures for every walker language', () => {
    expect(fixtures.length).toBeGreaterThan(15);
  });

  for (const { rel, lang, minSymbols } of TAIL) {
    it(`${rel} (${lang}): the generic extractor yields symbols`, () => {
      const file = path.join(__dirname, 'fixtures', 'golden', 'tail-langs', rel);
      const result = extractFromSource(file, fs.readFileSync(file, 'utf8'), lang);
      expect(result.errors.filter((e) => e.severity === 'error')).toEqual([]);
      expect(result.nodes.filter((n) => n.kind !== 'file').length).toBeGreaterThanOrEqual(minSymbols);
    });
  }

  for (const { file, lang } of fixtures) {
    it(`${path.basename(file)} (${lang}): walker output equals the generic extractor's`, () => {
      const source = fs.readFileSync(file, 'utf8');
      const walker = tryKernelExtract(file, source, lang);
      expect(walker, 'walker declined').not.toBeNull();
      const generic = new TreeSitterExtractor(file, source, lang).extract();
      const a = canon(walker!);
      const b = canon(generic);
      if (!ERRORING.has(path.basename(file))) {
        expect(a).toEqual(b);
      } else {
        const ratio = a.nodes.length / b.nodes.length;
        expect(ratio, `symbol count ratio ${ratio}`).toBeGreaterThan(0.95);
        expect(ratio).toBeLessThan(1.05);
      }
    });
  }
});
