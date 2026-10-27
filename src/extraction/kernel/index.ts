/**
 * Kernel extraction — the single entry point the extraction path calls.
 *
 * Every language with a bespoke Rust walker (`LANGUAGES` in langs.rs, reported
 * by contractInfo) is extracted here. A language the kernel only PARSES (no
 * walker) returns null and the caller runs the generic TypeScript extractor
 * over the kernel's serialized tree instead (tree-sitter.ts → parse-tree.ts).
 * Walker failures, including the stack guard's `defer:` (stack.rs), return null
 * and take the same generic path. Native tree serialization is iterative.
 *
 * CODEGRAPH_KERNEL=0 disables bespoke walkers for debugging; parsing still
 * requires the native kernel.
 */

import type { Binding, ExtractionResult, Language, Node } from '../../types';
import { commonJsRequireRefs } from '../commonjs-requires';
import { EXTRACTORS } from '../languages';
import { getKernel, kernelSupports } from './loader';
import { decodeExtractBuffers } from './decode';
import { captureLiterals } from '../literal-capture';
import {
  KERNEL_ABI_VERSION as LAYOUT_ABI,
  META as LAYOUT_META,
  META_PARSED_CLEAN,
  NONE as LAYOUT_NONE,
} from './layout';

export { getKernel, kernelSupports, resetKernelForTests } from './loader';
export { decodeExtractBuffers } from './decode';

/**
 * Languages routed to the kernel by default (gate-passed only — see the
 * per-language tracker in docs/design/rust-kernel-migration-plan.md §4).
 * Per-file safety valve regardless of routing: a file whose parse tree
 * contains ERRORS defers to the wasm extractor (error recovery differs
 * between UTF-8 and UTF-16 parsing — wasm's recovery is canonical).
 */
const DEFAULT_ROUTED: ReadonlySet<Language> = new Set<Language>([
  'typescript',
  'tsx',
  'javascript',
  'jsx',
  'java',
  'python',
  'go',
  // R7a (2026-07-17): parity swept 0-diff on redis/git/fmt/protobuf/ALS
  // (2,389 files compared) + full-init dump-diffs byte-identical; erroring
  // files defer per-file to wasm (routine for macro-heavy C/C++ — see
  // scripts/kernel-parity.mjs --max-deferral).
  'c',
  'cpp',
  // R7b (2026-07-20): parity swept 0-diff on ripgrep/tokio/rust-analyzer
  // (2,108 files byte-parity) + full-init dump-diffs byte-identical. Rust
  // deferral is ~0% on normal repos; token-macro-table sources (rust-analyzer's
  // parser crates, 18%) error on BOTH arms — grammar-inherent, not a walker
  // signal.
  'rust',
  // R7b (2026-07-20): parity swept 0-diff on serilog/Newtonsoft.Json/jellyfin
  // (3,229 files byte-parity) + full-init dump-diffs byte-identical ×3.
  // Deferral 0.05–3.3% — both-branches-kept `#if` damage that errors on BOTH
  // arms (the preParse blanking hoist keeps the kernel's input identical).
  'csharp',
  // R7b (2026-07-20): parity swept 0-diff on sinatra/jekyll/rails (3,763
  // files byte-parity, 0 deferrals — ruby error incidence is 0.00%) +
  // full-init dump-diffs byte-identical ×3. Any deferral on a ruby sweep is
  // a walker-bug signal, not grammar reality.
  'ruby',
  // R7b (2026-07-20): parity swept 0-diff on monolog/laravel-framework/
  // symfony (13,950 files byte-parity) + full-init dump-diffs byte-identical
  // ×3. Deferral ≈0–0.1% (genuinely-broken fixtures) — default sweep guard.
  'php',
  // R7b (2026-07-20): parity swept 0-diff on Alamofire/vapor/swift-nio (720
  // clean files byte-parity; Alamofire's 348 #1020 property nodes reproduced
  // exactly) + full-init dump-diffs byte-identical ×3. Swift error incidence
  // is structurally 9–27% on BOTH arms (heavy #if conditionalization) —
  // sweeps run --max-deferral 0.3; a deferral-rate JUMP is the bug signal.
  'swift',
  // R7b (2026-07-20): parity swept 0-diff on okio/okhttp/kotlinx.coroutines
  // (1,861 clean files byte-parity; KMP expect/actual synthesis identical —
  // 412 edges both arms on kotlinx.coroutines) + full-init dump-diffs
  // byte-identical ×3. Deferral 4.7–8.5% both-arm grammar reality
  // (fun-interface misparses + PHANTOM hasError files) — default sweep guard
  // holds; a JUMP past ~10% is the bug signal.
  'kotlin',
  // R7b batch 4 (2026-07-20): parity swept 0-diff on dplyr/ggplot2/shiny +
  // AnomalyDetection + full-init dump-diffs byte-identical ×3. R error
  // incidence is ~0% (deferrals 0/0/0/1 across the gate repos — the 1 is a
  // moustache-template pseudo-R file, both-arm): any deferral on an R sweep
  // is a walker-bug signal, not grammar reality.
  'r',
  // R7b batch 4 (2026-07-20): one walker, two dialects. Parity swept 0-diff
  // on kong/lazy.nvim/lua-resty-core (lua) + lune/Fusion (luau) + full-init
  // dump-diffs byte-identical. Lua error incidence ~0% (any deferral is a
  // walker-bug signal); luau 1.4–7.1% both-arm grammar reality (generic type
  // packs, default type params) — a JUMP past ~10% is the bug signal.
  'lua',
  'luau',
  // R7b batch 4 (2026-07-20): parity swept 0-diff on os-lib/cats + scala3
  // compiler/src + library/src + full-init dump-diffs byte-identical.
  // Error incidence is bimodal: mainstream Scala-2-style repos ~0-2%
  // (os-lib 0%, cats 1.8%) but scala-3-frontier code runs 10-18% with
  // PHANTOM-dominated error sets (hasError=true, zero ERROR nodes —
  // capture-checking `^` types; defer on the FLAG). Sweeps over
  // scala3-style repos use --max-deferral 0.3 (swift precedent); a
  // deferral JUMP on cats/os-lib-style code is the bug signal.
  'scala',
  // R7b batch 4 (2026-07-20): parity swept 0-diff on shelf/bloc/flutter +
  // full-init dump-diffs byte-identical. Error incidence 3.4-20.7% both-arm
  // grammar reality (empty object patterns — the sealed-class idiom — and
  // unnamed `library;` dominate; flutter sits at ~21%) — sweeps use
  // --max-deferral 0.3; the RATE, not the count, is the signal (flutter
  // HEAD drifts).
  'dart',
]);

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

/**
 * Whether the file's re-exports keep their statements as written while the
 * kernel reads the preParsed bytes (`reExportAsWritten`: TypeScript's
 * `export type *`, #2474). The decoded result then takes those import nodes'
 * signatures from `source`, as TreeSitterExtractor does, and the undecoded
 * fast path steps aside for it.
 */
function keepsReExportsAsWritten(source: string, pre: string, language: Language): boolean {
  return pre !== source && EXTRACTORS[language]?.reExportAsWritten?.(source) === true;
}

/** Each re-export import node's signature, read from `source` by the node's span. */
function restoreReExportSignatures(result: ExtractionResult, source: string): void {
  let lineStarts: number[] | null = null;
  for (const node of result.nodes) {
    if (node.kind !== 'import' || !node.signature?.startsWith('export')) continue;
    if (!lineStarts) {
      lineStarts = [0];
      for (let i = source.indexOf('\n'); i !== -1; i = source.indexOf('\n', i + 1)) lineStarts.push(i + 1);
    }
    const start = lineStarts[node.startLine - 1]! + node.startColumn;
    const end = lineStarts[node.endLine - 1]! + node.endColumn;
    node.signature = source.slice(start, end).trim();
  }
}

function isRouted(language: Language): boolean {
  const env = process.env.CODEGRAPH_KERNEL_LANGS;
  if (env === undefined || env === '') return DEFAULT_ROUTED.has(language);
  if (env === 'all') return true;
  return env
    .split(',')
    .map((s) => s.trim())
    .includes(language);
}

/** True when `language` would be extracted by the kernel right now. */
export function kernelRoutes(language: Language): boolean {
  return isRouted(language) && kernelSupports(language);
}

/** Warned-once registry so a broken language logs a single line, not one per file. */
const warned = new Set<string>();

/**
 * One-slot defer memo. A file the kernel defers (parse errors → the generic
 * extractor) used to pay the full pipeline again at every seam: the worker's
 * raw try blanked + native-parsed it, extractFromSource's kernel try blanked +
 * native-parsed it AGAIN, and the generic extractor then re-applied preParse a
 * third time. On a high-deferral tree (the Linux kernel defers ~79% of files)
 * that waste dominated the arm's parse phase. The slot remembers the LAST
 * deferred (file, source, language) so (a) a repeat kernel attempt for the
 * same file short-circuits to null, and (b) the generic extractor can reuse
 * the already-blanked source instead of re-running preParse. Source is matched
 * by string identity — the worker passes the same string through every seam.
 */
let deferSlot: { filePath: string; source: string; language: Language; pre: string } | null = null;

/** The hoisted preParse output for a just-deferred file, if it matches. */
export function takeDeferredPreParse(
  filePath: string,
  source: string,
  language: Language
): string | null {
  if (
    deferSlot &&
    deferSlot.filePath === filePath &&
    deferSlot.source === source &&
    deferSlot.language === language
  ) {
    return deferSlot.pre;
  }
  return null;
}

/** The raw table buffers + the cheap facts the orchestrator needs pre-decode. */
export interface KernelRawResult {
  buffers: NonNullable<ExtractionResult['kernelBuffers']>;
  counts: { nodes: number; edges: number; refs: number };
  errors: ExtractionResult['errors'];
  /** The kernel parsed `source` itself, not a pre-parsed rewrite, without a syntax error. */
  sourceParsedClean: boolean;
}

/** Whether the walker's tree is `source`'s own and has no ERROR or MISSING node. */
function sourceParsedClean(meta: Buffer, pre: string, source: string): boolean {
  return (meta.readUInt8(LAYOUT_META.flags) & META_PARSED_CLEAN) !== 0 && pre === source;
}

/**
 * Extract via the kernel WITHOUT decoding — the bulk-index fast path. The
 * tables ride to the store boundary as buffers (decoded on the store worker),
 * so the main thread never materializes per-node objects. Returns null under
 * exactly the conditions tryKernelExtract does, PLUS when the language has a
 * registered post() pass (post passes operate on decoded results, so those
 * languages keep the decoded path), and for a file whose re-exports keep their
 * statements as written (keepsReExportsAsWritten), which the decoded path
 * restores.
 */
export function tryKernelExtractRaw(
  filePath: string,
  source: string,
  language: Language
): KernelRawResult | null {
  if (!kernelRoutes(language) || POST_PASSES[language]) return null;
  const kernel = getKernel();
  if (!kernel) return null;
  if (takeDeferredPreParse(filePath, source, language) !== null) return null; // already deferred
  const pre = preParsedSource(filePath, source, language);
  // Its signatures come from the source as written, which needs the decoded path.
  if (keepsReExportsAsWritten(source, pre, language)) return null;
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
    return { buffers: { ...buffers, literalSource: pre }, counts, errors, sourceParsedClean: sourceParsedClean(meta, pre, source) };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // `defer:` is the stack guard's signal; the generic extractor takes the
    // file (parse_tree is iterative). Silent by design.
    if (message.includes('defer:')) {
      deferSlot = { filePath, source, language, pre };
      return null;
    }
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
  // References read beside the tables (a CommonJS `require`) ride on the transport.
  if (result.unresolvedReferences.length > 0) decoded.unresolvedReferences.push(...result.unresolvedReferences);
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
  if (takeDeferredPreParse(filePath, source, language) !== null) return null; // already deferred
  const t0 = Date.now();
  const pre = preParsedSource(filePath, source, language);
  try {
    const buffers = kernel.extractFile(filePath, pre, language);
    const result = decodeExtractBuffers(buffers, filePath, language);
    if (keepsReExportsAsWritten(source, pre, language)) restoreReExportSignatures(result, source);
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
    // Read beside the tables, as TreeSitterExtractor does (a CommonJS `require`).
    result.unresolvedReferences.push(...commonJsRequireRefs(filePath, source, language, sourceParsedClean(buffers.meta, pre, source)));
    result.durationMs = Date.now() - t0;
    return result;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // `defer:` is the stack guard's signal; the generic extractor takes the
    // file (parse_tree is iterative). Silent by design.
    if (message.includes('defer:')) {
      deferSlot = { filePath, source, language, pre };
      return null;
    }
    if (!warned.has(language)) {
      warned.add(language);
      process.stderr.write(
        `[codegraph-kernel] ${language} walker failed (${message}) — using the generic extractor for this language\n`
      );
    }
    return null;
  }
}

/** Languages `bindingsFile` handles: every walker language with rows: the TS/JS family, ArkTS, Python, Go, Java, Kotlin, PHP, C, C++ and Rust. */
const BINDINGS_LANGUAGES = new Set<string>(['typescript', 'tsx', 'javascript', 'jsx', 'arkts', 'python', 'go', 'java', 'kotlin', 'php', 'c', 'cpp', 'rust']);

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
  // Same-line namesakes (`function y() {}; function y(n) {}`) pair up in
  // source order; surplus declarations take the first node. Surplus locals
  // remain nodeless: an anonymous aggregate and its variable can share a name.
  const byNameLine = new Map<string, { ids: string[]; next: number }>();
  for (const n of nodes) {
    if (n.kind === 'file' || n.kind === 'import') continue;
    const key = `${n.name}\0${n.startLine}`;
    const slot = byNameLine.get(key);
    if (slot) slot.ids.push(n.id);
    else byNameLine.set(key, { ids: [n.id], next: 0 });
  }
  for (const b of bindings) {
    if (b.nodeId !== undefined || (b.kind !== 'decl' && b.kind !== 'import' && b.kind !== 'local')) continue;
    const slot = byNameLine.get(`${b.name}\0${b.line}`);
    if (slot) b.nodeId = slot.ids[slot.next++] ?? (b.kind === 'local' ? undefined : slot.ids[0]);
  }
  return bindings;
}
