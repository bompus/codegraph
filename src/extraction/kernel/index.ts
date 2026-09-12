/**
 * Kernel extraction — the single entry point the extraction path calls.
 *
 * Every language with a bespoke Rust walker (`LANGUAGES` in langs.rs, reported
 * by contractInfo) is extracted here. A language the kernel only PARSES (no
 * walker) returns null and the caller runs the generic TypeScript extractor
 * over the kernel's serialized tree instead (tree-sitter.ts → parse-tree.ts).
 * The only other null is the stack-overflow guard's `defer:` (stack.rs), which
 * takes the same generic path — parse_tree is iterative, so it survives the
 * nesting the recursive walker would not.
 *
 * The per-language routing table, the CODEGRAPH_KERNEL kill switch and the
 * old fallback parser were removed in Phase 5 of kernel-only-extraction-plan.md.
 */

import type { Binding, ExtractionResult, Language, Node } from '../../types';
import { EXTRACTORS } from '../languages';
import { getKernel, kernelSupports } from './loader';
import { decodeExtractBuffers } from './decode';
import { captureLiterals } from '../literal-capture';
import {
  KERNEL_ABI_VERSION as LAYOUT_ABI,
  META as LAYOUT_META,
  NONE as LAYOUT_NONE,
} from './layout';

export { getKernel, kernelSupports, resetKernelForTests } from './loader';
export { decodeExtractBuffers } from './decode';

/**
 * Per-language TS post-pass over the decoded result — the escape hatch for
 * logic `.scm` queries can't express (macro salvage, dialect sniffing,
 * wrapper-based component recognition). Runs synchronously after decode,
 * before the framework extract() hooks the caller applies. Keep these SMALL:
 * anything heavy belongs in the Rust emitter.
 */
export type KernelPostPass = (result: ExtractionResult, source: string) => void;
const POST_PASSES: Partial<Record<Language, KernelPostPass>> = {
  // (none yet — R2+)
};

/**
 * The preParse hoist (checklist §arch-1): languages with an offset-preserving
 * `preParse` hook (c/cpp macro blanking, csharp #237, metal #1121, cuda #1172)
 * apply it HERE, before the kernel call, so both arms parse identical blanked
 * bytes and none of the blanking logic needs a Rust port. The generic
 * extractor applies the same hook itself on the RAW source it receives, so a
 * walker-less language or a stack-guard defer still extracts identically.
 * Every blank is an equal-length-space replacement, so offsets, lines, and
 * columns survive; `filePath` rides along for the extension-gated dialect
 * blanks (`.metal` attributes; `.cu`/`.cuh` + content-gated CUDA).
 */
function preParsedSource(filePath: string, source: string, language: Language): string {
  const pre = EXTRACTORS[language]?.preParse;
  return pre ? pre(source, filePath) : source;
}

/** True when `language` has a bespoke kernel walker (contractInfo.languages). */
export function kernelRoutes(language: Language): boolean {
  return kernelSupports(language);
}

/** Warned-once registry so a broken language logs a single line, not one per file. */
const warned = new Set<string>();

/** The raw table buffers + the cheap facts the orchestrator needs pre-decode. */
export interface KernelRawResult {
  buffers: NonNullable<ExtractionResult['kernelBuffers']>;
  counts: { nodes: number; edges: number; refs: number };
  errors: ExtractionResult['errors'];
}

/**
 * Extract via the kernel WITHOUT decoding — the bulk-index fast path. The
 * tables ride to the store boundary as buffers (decoded on the store worker),
 * so the main thread never materializes per-node objects. Returns null under
 * exactly the conditions tryKernelExtract does, PLUS when the language has a
 * registered post() pass (post passes operate on decoded results, so those
 * languages keep the decoded path).
 */
export function tryKernelExtractRaw(
  filePath: string,
  source: string,
  language: Language
): KernelRawResult | null {
  if (!kernelRoutes(language) || POST_PASSES[language]) return null;
  const kernel = getKernel();
  if (!kernel) return null;
  const pre = preParsedSource(filePath, source, language);
  try {
    const buffers = kernel.extractFile(filePath, pre, language);
    const meta = buffers.meta;
    if (meta.readUInt8(LAYOUT_META.version) !== LAYOUT_ABI) {
      throw new Error(`kernel buffer ABI ${meta.readUInt8(0)} != expected ${LAYOUT_ABI}`);
    }
    const counts = {
      nodes: meta.readUInt32LE(LAYOUT_META.nodeCount),
      edges: meta.readUInt32LE(LAYOUT_META.edgeCount),
      refs: meta.readUInt32LE(LAYOUT_META.refCount),
    };
    let errors: ExtractionResult['errors'] = [];
    const errorsOff = meta.readUInt32LE(LAYOUT_META.errorsOff);
    if (errorsOff !== LAYOUT_NONE) {
      const errorsLen = meta.readUInt32LE(LAYOUT_META.errorsLen);
      errors = JSON.parse(
        buffers.arena.toString('utf8', errorsOff, errorsOff + errorsLen)
      ) as ExtractionResult['errors'];
    }
    return { buffers: { ...buffers, literalSource: pre }, counts, errors };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // `defer:` is the stack guard's signal; the generic extractor takes the
    // file (parse_tree is iterative). Silent by design.
    if (message.includes('defer:')) return null;
    if (!warned.has(language)) {
      warned.add(language);
      process.stderr.write(
        `[codegraph-kernel] ${language} walker failed (${message}) — using the generic extractor for this language\n`
      );
    }
    return null;
  }
}

/**
 * Decode a buffer-carrying result (see ExtractionResult.kernelBuffers) into a
 * plain, fully-materialized ExtractionResult — the fallback for store paths
 * that need objects (main-thread store, tests).
 */
export function materializeKernelResult(
  result: ExtractionResult,
  filePath: string,
  language: Language
): ExtractionResult {
  if (!result.kernelBuffers) return result;
  const b = result.kernelBuffers;
  const asBuf = (u: Uint8Array) => Buffer.from(u.buffer, u.byteOffset, u.byteLength);
  const decoded = decodeExtractBuffers(
    { meta: asBuf(b.meta), nodes: asBuf(b.nodes), edges: asBuf(b.edges), refs: asBuf(b.refs), bindings: asBuf(b.bindings), arena: asBuf(b.arena) },
    filePath,
    language
  );
  if (b.literalSource !== undefined) captureLiterals(b.literalSource, decoded.nodes);
  decoded.durationMs = result.durationMs;
  return decoded;
}

/**
 * Extract via the native kernel. Returns null when the kernel doesn't apply
 * (not routed / not available / kill switch) — the caller falls back to the
 * generic TreeSitterExtractor. A kernel ERROR on a routed file also returns
 * null: per-file fallback keeps indexing correct while a kernel bug costs
 * only that file's speedup.
 */
export function tryKernelExtract(
  filePath: string,
  source: string,
  language: Language
): ExtractionResult | null {
  if (!kernelRoutes(language)) return null;
  const kernel = getKernel();
  if (!kernel) return null;
  const t0 = Date.now();
  const pre = preParsedSource(filePath, source, language);
  try {
    const buffers = kernel.extractFile(filePath, pre, language);
    const result = decodeExtractBuffers(buffers, filePath, language);
    POST_PASSES[language]?.(result, source);
    // Literal seeds are a pass over source text and the node list, never over
    // the tree (literal-capture.ts), so they need no Rust mirror — the same
    // pass the generic extractor runs at the end of extract() applies to the
    // kernel's nodes here, and the two paths stay node-for-node identical.
    // `pre`, not `source`: the generic extractor captures from its own preParsed
    // text, and a preParse blanks bytes a literal could otherwise be read from.
    // Without this every routed language loses its seeds while markdown and the
    // unrouted ones keep theirs, and a quoted-key explore query silently stops
    // finding holders.
    captureLiterals(pre, result.nodes);
    result.durationMs = Date.now() - t0;
    return result;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // `defer:` is the stack guard's signal; the generic extractor takes the
    // file (parse_tree is iterative). Silent by design.
    if (message.includes('defer:')) return null;
    if (!warned.has(language)) {
      warned.add(language);
      process.stderr.write(
        `[codegraph-kernel] ${language} walker failed (${message}) — using the generic extractor for this language\n`
      );
    }
    return null;
  }
}

/** Languages `bindingsFile` handles: every walker language with rows: the TS/JS family, ArkTS, Python, Go, Java, Kotlin, PHP, C and C++. */
const BINDINGS_LANGUAGES = new Set<string>(['typescript', 'tsx', 'javascript', 'jsx', 'arkts', 'python', 'go', 'java', 'kotlin', 'php', 'c', 'cpp']);

/**
 * Binding rows for a TS/JS-family file from the kernel's AST-only emitter,
 * for the generic extractor's path (a stack-guard defer, ArkTS). `decl` rows
 * come back without node ids; the caller attaches them by name and line to
 * the nodes it created. Null when the kernel does not apply.
 */
export function tryKernelBindings(filePath: string, source: string, language: Language): Binding[] | null {
  if (!BINDINGS_LANGUAGES.has(language)) return null;
  const kernel = getKernel();
  if (!kernel || typeof kernel.bindingsFile !== 'function') return null;
  try {
    // The same offset-preserving pre-parse the walker sees (C/C++ macro
    // blanking, C# directives), so both paths read one tree.
    const buffers = kernel.bindingsFile(filePath, preParsedSource(filePath, source, language), language);
    return decodeExtractBuffers(buffers, filePath, language).bindings ?? [];
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (!warned.has(`bindings:${language}`)) {
      warned.add(`bindings:${language}`);
      process.stderr.write(`[codegraph-kernel] ${language} bindings failed (${message}) — this file has no binding rows\n`);
    }
    return null;
  }
}

/**
 * Attach node ids to rows the AST-only emitter left nodeless: a `decl` (or a
 * node-backed `import`) row names the node the extractor created at the same
 * line. Rows that match nothing stay nodeless; they still answer the
 * shadowing, sealed-module and export questions.
 */
export function attachBindingNodeIds(bindings: Binding[], nodes: Node[]): Binding[] {
  const byNameLine = new Map<string, string>();
  for (const n of nodes) {
    if (n.kind === 'file' || n.kind === 'import') continue;
    const key = `${n.name}\0${n.startLine}`;
    if (!byNameLine.has(key)) byNameLine.set(key, n.id);
  }
  for (const b of bindings) {
    if (b.nodeId !== undefined || (b.kind !== 'decl' && b.kind !== 'import' && b.kind !== 'local')) continue;
    const id = byNameLine.get(`${b.name}\0${b.line}`);
    if (id) b.nodeId = id;
  }
  return bindings;
}
