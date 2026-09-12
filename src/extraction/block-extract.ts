/**
 * Embedded-block extraction — the one seam the SFC extractors (Vue, Svelte,
 * Astro, Razor) use to hand a sliced `<script>` / frontmatter / `@code`
 * block to a real language extractor.
 *
 * Phase 2 of docs/design/kernel-only-extraction-plan.md: the block goes to
 * the native kernel first (same `(filePath, content, language)` contract the
 * whole-file path uses, so the block's nodes, edges, refs, literals and the
 * parse-collapse warning come out identical to the wasm extractor's), and
 * only falls back to `TreeSitterExtractor` when the kernel declines — the
 * language is not routed, no binary is staged, or the stack guard deferred.
 * The fallback goes away with the wasm path (Phase 5).
 *
 * Callers keep their own position rebasing: results are block-relative, the
 * same as `new TreeSitterExtractor(...).extract()` returned.
 */

import type { ExtractionResult, Language } from '../types';
import { tryKernelExtract } from './kernel';
import { TreeSitterExtractor } from './tree-sitter';

export function extractEmbeddedBlock(
  filePath: string,
  content: string,
  language: Language
): ExtractionResult {
  const native = tryKernelExtract(filePath, content, language);
  if (native) return native;
  return new TreeSitterExtractor(filePath, content, language).extract();
}
