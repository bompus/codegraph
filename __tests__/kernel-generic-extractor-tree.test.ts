/**
 * The generic TypeScript extractor on a native tree ≡ on a wasm tree
 * (Phase 4 of docs/design/kernel-only-extraction-plan.md).
 *
 * `TreeSitterExtractor` (7,000 lines of language-agnostic walking driven by
 * the per-language tables in `languages/`) parses through `parseSourceTreeSync`
 * now, so on a host with a kernel it walks the serialized native tree via
 * the `NativeNode` facade. That is the mechanism that puts every language
 * WITHOUT a bespoke Rust walker on the kernel. This suite is its gate: with
 * walker routing switched off (`CODEGRAPH_KERNEL_LANGS=none`) but native
 * trees on, extraction of every torture fixture must equal the wasm arm's
 * (`CODEGRAPH_KERNEL=0`, which disables both) as canonical multisets.
 *
 * Erroring trees (the C/C++ torture files) are compared loosely: same file
 * node, same non-empty symbol set size within 5% — recovery may differ.
 */

import { describe, it, expect, beforeAll, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { extractFromSource } from '../src/extraction';
import { getParser, initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import * as grammars from '../src/extraction/grammars';
import { resetKernelForTests } from '../src/extraction/kernel';
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

function canon(r: ExtractionResult) {
  return {
    nodes: r.nodes.map(({ updatedAt: _u, ...n }) => JSON.stringify(n, Object.keys(n).sort())).sort(),
    edges: r.edges.map((e) => JSON.stringify(e, Object.keys(e).sort())).sort(),
    refs: r.unresolvedReferences.map((x) => JSON.stringify(x, Object.keys(x).sort())).sort(),
  };
}

/**
 * The five tail languages have no walker at all; the kernel carries their
 * grammars (Cargo.toml, Phase 4) purely for parse_tree. Extracting them on a
 * kernel host must never instantiate a wasm parser.
 */
const TAIL: Array<{ rel: string; lang: Language }> = [
  { rel: 'objc/Greeter.m', lang: 'objc' },
  { rel: 'erlang/counter.erl', lang: 'erlang' },
  { rel: 'nix/default.nix', lang: 'nix' },
  { rel: 'pascal/Greeter.pas', lang: 'pascal' },
  { rel: 'solidity/Greeter.sol', lang: 'solidity' },
  // Phase 4b: vendored-C grammars for the languages once slated for dropping,
  // plus CFML (whose tag-based extractor walks the facade directly).
  { rel: 'arkts/Greeter.ets', lang: 'arkts' },
  { rel: 'terraform/main.tf', lang: 'terraform' },
  { rel: 'vbnet/Greeter.vb', lang: 'vbnet' },
  { rel: 'cobol/GREETER.cbl', lang: 'cobol' },
  { rel: 'cfml/Greeter.cfc', lang: 'cfml' },
  { rel: 'cfml/greet.cfm', lang: 'cfml' },
];

const ENV_KEYS = ['CODEGRAPH_KERNEL', 'CODEGRAPH_KERNEL_LANGS'] as const;
let saved: Record<string, string | undefined>;

describe.skipIf(!kernelBuilt)('generic extractor on native trees', () => {
  const fixtures = fs
    .readdirSync(FIXTURE_DIR)
    .filter((f) => EXT_LANG[path.extname(f)])
    .map((f) => ({ file: path.join(FIXTURE_DIR, f), lang: EXT_LANG[path.extname(f)]! }));

  beforeAll(async () => {
    await initGrammars();
    await loadGrammarsForLanguages([...new Set(fixtures.map((f) => f.lang)), ...TAIL.map((t) => t.lang), 'cfscript', 'cfquery']);
  });
  beforeEach(() => {
    saved = Object.fromEntries(ENV_KEYS.map((k) => [k, process.env[k]]));
    resetKernelForTests();
  });
  afterEach(() => {
    for (const k of ENV_KEYS) {
      if (saved[k] === undefined) delete process.env[k];
      else process.env[k] = saved[k];
    }
    resetKernelForTests();
  });

  it('has fixtures for every walker language', () => {
    expect(fixtures.length).toBeGreaterThan(15);
  });

  for (const { rel, lang } of TAIL) {
    it(`${rel}: extracts natively with no wasm parser`, () => {
      const file = path.join(__dirname, 'fixtures', 'golden', 'tail-langs', rel);
      const source = fs.readFileSync(file, 'utf8');
      delete process.env.CODEGRAPH_KERNEL;
      const spy = vi.spyOn(grammars, 'getParser');
      try {
        const result = extractFromSource(file, source, lang);
        // A .cfm page holds one <cfscript> function; a .cfc/.pas/.m file holds several.
        expect(result.nodes.filter((n) => n.kind !== 'file').length).toBeGreaterThanOrEqual(1);
        expect(spy, 'wasm parser instantiated for a kernel-parsed language').not.toHaveBeenCalled();
      } finally {
        spy.mockRestore();
      }
    });
  }

  for (const { file, lang } of fixtures) {
    it(`${path.basename(file)} (${lang}): generic extractor gives the same graph on the native tree`, () => {
      const source = fs.readFileSync(file, 'utf8');
      const wasmTree = getParser(lang)!.parse(source)!;
      const erroring = wasmTree.rootNode.hasError;
      wasmTree.delete();

      // Native tree, no walker: the generic extractor over the facade.
      process.env.CODEGRAPH_KERNEL_LANGS = 'none';
      delete process.env.CODEGRAPH_KERNEL;
      const native = canon(extractFromSource(file, source, lang));
      // Everything wasm.
      process.env.CODEGRAPH_KERNEL = '0';
      const wasm = canon(extractFromSource(file, source, lang));

      if (!erroring) {
        expect(native).toEqual(wasm);
      } else {
        expect(native.nodes.length).toBeGreaterThan(1);
        const ratio = native.nodes.length / wasm.nodes.length;
        expect(ratio, `symbol count ratio ${ratio}`).toBeGreaterThan(0.95);
        expect(ratio).toBeLessThan(1.05);
      }
    });
  }
});
