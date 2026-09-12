/**
 * Embedded-language blocks ride the kernel (Phase 2 of
 * docs/design/kernel-only-extraction-plan.md).
 *
 * Vue, Svelte, Astro and Razor slice a file and hand each script block to a
 * real language extractor. Before Phase 2 that was always `new
 * TreeSitterExtractor(...)` — the wasm path, even on a host with a kernel.
 * Now `extractEmbeddedBlock` tries the kernel first. Two pins:
 *
 *   1. With a kernel staged and every language routed, extracting an SFC
 *      never instantiates a wasm parser (`getParser` is not called).
 *   2. The result is identical to the wasm path's for the same file, so the
 *      golden dumps (vue-sfc, sfc-mix) hold on both paths.
 *
 * Skips without a kernel binary; CODEGRAPH_KERNEL_EXPECT=1 makes that a
 * failure (kernel-scaffold.test.ts owns that assertion).
 */

import { describe, it, expect, beforeAll, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { extractFromSource } from '../src/extraction';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import * as grammars from '../src/extraction/grammars';
import { resetKernelForTests } from '../src/extraction/kernel';
import type { ExtractionResult, Language } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname,
  '..',
  'codegraph-kernel',
  'prebuilds',
  `${process.platform}-${process.arch}`,
  'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

const SFC_DIR = path.join(__dirname, 'fixtures', 'golden', 'sfc-mix');
const VUE_DIR = path.join(__dirname, 'fixtures', 'golden', 'vue-sfc');

const SAMPLES: Array<{ rel: string; lang: Language }> = [
  { rel: path.join(VUE_DIR, 'src/views/Home.vue'), lang: 'vue' },
  { rel: path.join(VUE_DIR, 'src/views/Profile.vue'), lang: 'vue' },
  { rel: path.join(SFC_DIR, 'src/lib/Counter.svelte'), lang: 'svelte' },
  { rel: path.join(SFC_DIR, 'src/lib/Panel.svelte'), lang: 'svelte' },
  { rel: path.join(SFC_DIR, 'src/pages/index.astro'), lang: 'astro' },
  { rel: path.join(SFC_DIR, 'Components/Greeter.razor'), lang: 'razor' },
];

function canon(result: ExtractionResult) {
  return {
    nodes: result.nodes.map(({ updatedAt: _u, ...n }) => JSON.stringify(n, Object.keys(n).sort())).sort(),
    edges: result.edges.map((e) => JSON.stringify(e, Object.keys(e).sort())).sort(),
    refs: result.unresolvedReferences.map((r) => JSON.stringify(r, Object.keys(r).sort())).sort(),
    errors: result.errors.map((e) => JSON.stringify(e, Object.keys(e).sort())).sort(),
  };
}

const ENV_KEYS = ['CODEGRAPH_KERNEL', 'CODEGRAPH_KERNEL_LANGS'] as const;
let savedEnv: Record<string, string | undefined>;

describe.skipIf(!kernelBuilt)('SFC blocks extract through the kernel', () => {
  beforeAll(async () => {
    await initGrammars();
    await loadGrammarsForLanguages(['typescript', 'javascript', 'csharp']);
  });
  beforeEach(() => {
    savedEnv = Object.fromEntries(ENV_KEYS.map((k) => [k, process.env[k]]));
    resetKernelForTests();
  });
  afterEach(() => {
    for (const k of ENV_KEYS) {
      if (savedEnv[k] === undefined) delete process.env[k];
      else process.env[k] = savedEnv[k];
    }
    vi.restoreAllMocks();
    resetKernelForTests();
  });

  for (const { rel, lang } of SAMPLES) {
    const name = path.relative(path.join(__dirname, 'fixtures', 'golden'), rel);

    it(`${name}: no wasm parser is instantiated`, () => {
      process.env.CODEGRAPH_KERNEL_LANGS = 'all';
      delete process.env.CODEGRAPH_KERNEL;
      const spy = vi.spyOn(grammars, 'getParser');
      const source = fs.readFileSync(rel, 'utf8');
      const result = extractFromSource(rel, source, lang);
      expect(result.nodes.some((n) => n.kind === 'component')).toBe(true);
      expect(spy, 'TreeSitterExtractor was constructed for a script block').not.toHaveBeenCalled();
    });

    it(`${name}: kernel and wasm block results are identical`, () => {
      const source = fs.readFileSync(rel, 'utf8');
      process.env.CODEGRAPH_KERNEL_LANGS = 'all';
      delete process.env.CODEGRAPH_KERNEL;
      const native = canon(extractFromSource(rel, source, lang));
      process.env.CODEGRAPH_KERNEL = '0';
      const wasm = canon(extractFromSource(rel, source, lang));
      expect(native).toEqual(wasm);
    });
  }
});
