/**
 * Parse Worker
 *
 * Runs kernel extraction in a separate thread so the main thread
 * stays unblocked and the UI animation renders smoothly.
 */

// Compile cache FIRST: the worker's boot cost is dominated by re-requiring
// the extraction module graph; the persistent V8 cache (Node ≥22.8) makes
// that a bytecode load instead of a recompile. Safe no-op when unavailable.
try {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  (require('node:module') as { enableCompileCache?: () => void }).enableCompileCache?.();
} catch { /* cache is best-effort */ }

import { parentPort } from 'worker_threads';
import { extractFromSource } from './tree-sitter';
import { detectLanguage } from './grammars';
import { tryKernelExtractRaw } from './kernel';
import { getAllFrameworkResolvers, getApplicableFrameworks } from '../resolution/frameworks';
import type { Language, ExtractionResult } from '../types';

parentPort!.on('message', async (msg: { type: string; id?: number; filePath?: string; content?: string; languages?: Language[]; frameworkNames?: string[]; language?: Language }) => {
  if (msg.type === 'load-grammars') {
    // Nothing to load since the wasm path was removed: grammars live in the
    // kernel binary. The handshake stays so the pool's readiness model
    // (spawned → ready → idle) is unchanged.
    parentPort!.postMessage({ type: 'grammars-loaded' });
  } else if (msg.type === 'parse') {
    const { id, filePath, content, frameworkNames } = msg;
    // Worker-side parse clock: reported back with the result so the pool can
    // tell a genuinely slow parse from a result whose delivery was delayed by
    // a stalled main thread (issue #1231 false timeouts).
    const t0 = performance.now();
    try {
      // The main thread resolves the language (it holds the project's
      // codegraph.json extension overrides) and sends it; fall back to detection
      // for older callers / safety.
      const language = msg.language ?? detectLanguage(filePath!, content);

      // Kernel deferred-decode fast path: ship the file's tables as flat
      // buffers and decode at the STORE boundary, so the main thread never
      // materializes per-node objects (nor pays their structured-clone cost —
      // buffer clone is a flat memcpy). Only when no applicable framework has
      // an extract() hook: those merge extra nodes/refs into the DECODED
      // result inside extractFromSource, so such files keep the decoded path.
      let result: ExtractionResult | undefined;
      const frameworksNeedDecode =
        frameworkNames && frameworkNames.length > 0
          ? getApplicableFrameworks(
              getAllFrameworkResolvers().filter((r) => frameworkNames.includes(r.name)),
              language
            ).some((fw) => !!fw.extract)
          : false;
      if (!frameworksNeedDecode) {
        const raw = tryKernelExtractRaw(filePath!, content!, language);
        if (raw) {
          result = {
            nodes: [],
            edges: [],
            unresolvedReferences: [],
            errors: raw.errors,
            durationMs: 0,
            kernelBuffers: raw.buffers,
            kernelCounts: raw.counts,
          };
        }
      }
      result ??= extractFromSource(filePath!, content!, language, frameworkNames);

      parentPort!.postMessage({ type: 'parse-result', id, result, parseMs: performance.now() - t0 });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);

      parentPort!.postMessage({
        type: 'parse-result',
        id,
        parseMs: performance.now() - t0,
        result: {
          nodes: [],
          edges: [],
          unresolvedReferences: [],
          errors: [{ message: `Parse worker error: ${message}`, filePath: filePath!, severity: 'error', code: 'parse_error' }],
          durationMs: 0,
        } satisfies ExtractionResult,
      });
    }
  } else if (msg.type === 'shutdown') {
    parentPort!.postMessage({ type: 'shutdown-ack' });
  }
});
