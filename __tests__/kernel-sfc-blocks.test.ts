/**
 * Embedded-language blocks ride the kernel (Phase 2 of
 * docs/design/kernel-only-extraction-plan.md; re-based in Phase 5 when the
 * wasm comparison arm was removed).
 *
 * Vue, Svelte, Astro and Razor slice a file and hand each script block to
 * `extractEmbeddedBlock`, which tries the kernel walker for the block's
 * language and otherwise runs the generic extractor on the kernel's tree.
 * Two pins:
 *
 *   1. Every sample SFC extracts a component node plus its script symbols.
 *   2. For a block, the walker path and the generic-extractor path give the
 *      same result — the same agreement the whole-file gate checks, at the
 *      block seam the SFC extractors actually use.
 */

import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { extractFromSource } from '../src/extraction';
import { extractEmbeddedBlock } from '../src/extraction/block-extract';
import { TreeSitterExtractor } from '../src/extraction/tree-sitter';
import type { ExtractionResult, Language } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
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

describe.skipIf(!kernelBuilt)('SFC blocks extract through the kernel', () => {
  for (const { rel, lang } of SAMPLES) {
    const name = path.relative(path.join(__dirname, 'fixtures', 'golden'), rel);
    it(`${name}: extracts a component and its script symbols`, () => {
      const source = fs.readFileSync(rel, 'utf8');
      const result = extractFromSource(rel, source, lang);
      expect(result.nodes.some((n) => n.kind === 'component')).toBe(true);
      expect(result.errors.filter((e) => e.severity === 'error')).toEqual([]);
    });
  }

  const BLOCKS: Array<{ name: string; content: string; lang: Language }> = [
    {
      name: 'vue script setup (ts)',
      lang: 'typescript',
      content: 'import { ref } from "vue";\nconst count = ref(0);\nfunction bump(): void { count.value += 1; }\n',
    },
    { name: 'svelte plain script', lang: 'javascript', content: 'export let title;\nfunction describe() { return title + " panel"; }\n' },
    {
      name: 'razor @code wrapper',
      lang: 'csharp',
      content: 'class __RazorCode__ {\n    private string message = "";\n    private void SayHello() { message = Greeter.Greet(new GreetRequest("x")); }\n}\n',
    },
  ];
  for (const { name, content, lang } of BLOCKS) {
    it(`${name}: block seam agrees with the generic extractor`, () => {
      const viaSeam = canon(extractEmbeddedBlock('src/Block.x', content, lang));
      const viaGeneric = canon(new TreeSitterExtractor('src/Block.x', content, lang).extract());
      expect(viaSeam).toEqual(viaGeneric);
      expect(viaSeam.nodes.length).toBeGreaterThan(1);
    });
  }
});
