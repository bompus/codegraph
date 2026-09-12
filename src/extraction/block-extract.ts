/**
 * Embedded-block extraction — the one seam the SFC extractors (Vue, Svelte,
 * Astro, Razor) use to hand a sliced `<script>` / frontmatter / `@code`
 * block to a real language extractor.
 *
 * Phase 2 of docs/design/kernel-only-extraction-plan.md: the block goes to
 * the kernel walker for the block's language (same `(filePath, content,
 * language)` contract the whole-file path uses), and to the generic
 * extractor over the kernel's serialized tree when there is no walker or the
 * stack guard deferred.
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
