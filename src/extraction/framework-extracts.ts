import { getAllFrameworkResolvers, getApplicableFrameworks } from '../resolution/frameworks';
import type { ExtractionResult, Language } from '../types';

/**
 * Append every applicable framework's `extract()` output to `result` —
 * nodes then refs, extractor output first, a failed hook degrading to a
 * warning. Runs inside extractFromSource for the decoded path, and on the
 * store side of the kernel-buffer path, where the merge must land
 * post-decode so framework projects keep the flat-buffer fast path.
 */
export function applyFrameworkExtracts(
  result: ExtractionResult,
  filePath: string,
  source: string,
  language: Language,
  frameworkNames: string[] | undefined
): void {
  if (!frameworkNames || frameworkNames.length === 0) return;
  const applicable = getApplicableFrameworks(
    getAllFrameworkResolvers().filter((r) => frameworkNames.includes(r.name)),
    language
  );
  for (const fw of applicable) {
    if (!fw.extract) continue;
    try {
      const fwResult = fw.extract(filePath, source);
      result.nodes.push(...fwResult.nodes);
      result.unresolvedReferences.push(...fwResult.references);
    } catch (err) {
      result.errors.push({
        message: `Framework extractor '${fw.name}' failed: ${
          err instanceof Error ? err.message : String(err)
        }`,
        filePath,
        severity: 'warning',
      });
    }
  }
}
