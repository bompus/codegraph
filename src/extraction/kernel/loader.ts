/**
 * Native-kernel loader — finds, loads, and contract-verifies the
 * codegraph-kernel .node addon.
 *
 * The kernel is OPTIONAL everywhere. Every failure mode here (no binary for
 * this platform, dlopen error, ABI/kind-table mismatch) resolves to `null`
 * and the extraction path silently keeps using the wasm pipeline — a missing
 * or stale kernel must never break indexing, only skip the speedup. Set
 * CODEGRAPH_KERNEL_DEBUG=1 to see why a kernel didn't load.
 *
 * Kill switch: CODEGRAPH_KERNEL=0 disables the kernel entirely (checked per
 * call so tests and embedders can flip it at runtime).
 *
 * Search order:
 *   1. CODEGRAPH_KERNEL_PATH — explicit .node path (dev/testing override)
 *   2. <up3>/kernel/codegraph-kernel.node — the release bundle layout
 *      (lib/dist/** next to lib/kernel/; see scripts/build-bundle.sh)
 *   3. <up3>/codegraph-kernel/prebuilds/<platform>-<arch>/codegraph-kernel.node
 *      — from-source runs and tests (staged by scripts/build-kernel.mjs)
 *
 * "up3" = three directories above this file, which is the package root both
 * from src/extraction/kernel/ and from dist/extraction/kernel/.
 */

import * as fs from 'fs';
import * as path from 'path';
import { createRequire } from 'module';
import { NODE_KINDS, EDGE_KINDS } from '../../types';
import { KERNEL_ABI_VERSION } from './layout';

/** Raw buffer tables for one file — see layout.ts for the byte layout. */
export interface KernelBuffers {
  meta: Buffer;
  nodes: Buffer;
  edges: Buffer;
  refs: Buffer;
  /** v3: per-file binding rows (resolution-binding-model-plan.md). */
  bindings: Buffer;
  arena: Buffer;
}

export interface KernelContractInfo {
  abiVersion: number;
  kernelVersion: string;
  nodeKinds: string[];
  edgeKinds: string[];
  languages: string[];
}

export interface KernelGrammarInfo {
  abiVersion: number;
  nodeKindCount: number;
  fieldCount: number;
  nodeKinds: string[];
  fieldNames: string[];
}

/** Input to the cFnPtr extraction sweep: one file's raw text + its struct
 *  node extents (`endLine ?? startLine` applied by the caller). */
export interface CfnptrFileIn {
  text: string;
  structs: { id: string; startLine: number; endLine: number }[];
}

/** Path-driven sweep input: the kernel reads the file itself and fans the
 *  batch across threads, so the corpus's ~1.5GB of text never crosses the
 *  boundary. */
export interface CfnptrPathIn {
  path: string;
  structs: { id: string; startLine: number; endLine: number }[];
}

/** Per-file facts from the native cFnPtr extraction sweep — mirror of the
 *  Rust `CfnptrFacts` (see codegraph-kernel/src/cfnptr.rs); semantics match
 *  the JS sweep in src/resolution/c-fnptr-synthesizer.ts. */
export interface CfnptrFactsOut {
  fnPtrTypedefs: string[];
  fnTypeTypedefs: string[];
  structs: { id: string; parsed: boolean; fields: { name: string; index: number; ptr: boolean; type: string }[] }[];
  inlinePtr: boolean;
  inlineTypes: string[];
  inlineTags: string[];
  initTokens: string[];
  arrayElems: string[];
  aliasNames: string[];
  dPairs: string[];
  /** Distinct LHS field names of `x->f = fn;` / `(*x)->f = fn;` (the
   *  bare-function-assignment registration filter). OPTIONAL — absent on
   *  binaries that predate it; callers treat absence as empty. */
  assignFields?: string[];
  dispatchFields: string[];
  arrayDispatchNames: string[];
  includes: string[];
}

/** Per-file `buildEnv` inputs from the native stage-C env extraction —
 *  mirror of Rust `CfnptrFileEnv`. `includes` are the raw `#include "…"`
 *  captures; extension filtering and path resolution stay caller-side.
 *  `stripped` is the JS `src(file)` result — push it into `srcCache` so
 *  `processUnit` doesn't re-read+strip (the old `src()`-backed extractors
 *  warmed that cache as a side effect). */
export interface CfnptrFileEnvOut {
  fnMacros: { name: string; params: string[]; expansion: string }[];
  objMacros: { name: string; value: string }[];
  defined: string[];
  includes: string[];
  stripped: string;
}

/** `{name, type, isFnPtr}` — the FieldInfo members stages D/E consult; `type`
 *  stays absent for fn-pointer fields (napi `Option` rejects explicit null). */
export interface CfnptrLinkFieldIn {
  name: string;
  type?: string;
  isFnPtr: boolean;
}

export interface CfnptrLinkFileIn {
  /** Project-relative path (the node filePath — `registeredAt` uses it). */
  rel: string;
  /** Absolute path the kernel reads. */
  abs: string;
  /** Stage-D survivor flag (facts pre-gate). */
  prop: boolean;
  /** Stage-E survivor flag. */
  dispatch: boolean;
  /** The file's function/method extents, in `getNodesInFile` order. */
  fns: { id: string; startLine: number; endLine: number }[];
}

/** The registration tables verbatim — mirror of Rust `CfnptrLinkTables`. */
export interface CfnptrLinkTablesIn {
  fieldToStructs: { field: string; structs: string[] }[];
  structLayout: { name: string; fields: CfnptrLinkFieldIn[] }[];
  allStructFields: { name: string; variants: CfnptrLinkFieldIn[][] }[];
  globalVarType: { var: string; type: string }[];
  reg: { key: string; ids: string[] }[];
  arrayReg: { name: string; entries: { file: string; ids: string[] }[] }[];
}

export interface CfnptrLinkOut {
  edges: { source: string; target: string; line: number; via: string; registeredAt: string }[];
}

/** Whole-CST buffers (codegraph-kernel/src/tree.rs); decoded by kernel/tree.ts. */
export interface KernelTreeBuffers {
  meta: Buffer;
  nodes: Buffer;
  children: Buffer;
  fields: Buffer;
}
export interface KernelTreeNames {
  kindCount: number;
  fieldCount: number;
  names: Buffer;
}

// ---------------------------------------------------------------------------
// Phase 4 kernel resolver — native read+settle over bindings/nodes/
// unresolved_refs (resolution-binding-model-plan.md §4).
// ---------------------------------------------------------------------------

export interface KernelKv {
  key: string;
  value: string;
}

export interface KernelAliasPatternIn {
  prefix: string;
  suffix: string;
  hasWildcard: boolean;
  replacements: string[];
}

export interface KernelAliasMapIn {
  baseUrl?: string;
  patterns: KernelAliasPatternIn[];
}

export interface KernelWorkspaceIn {
  sourceEntries: KernelKv[];
  byName: KernelKv[];
  entryByName?: KernelKv[];
  localLinkNames?: string[];
}

export interface KernelResolverConfig {
  dbPath: string;
  projectRoot: string;
  aliases?: KernelAliasMapIn;
  workspaces?: KernelWorkspaceIn;
  goModulePath?: string;
  cppIncludeDirs?: string[];
  nodeBuiltinSpecifiers: string[];
  frameworksActive: boolean;
  /** Names (`f.name`) of detected framework resolvers — lets the kernel
   *  evaluate `claimsReference` natively. Omit to keep the conservative
   *  "every prefilter miss is claimed" behavior. */
  frameworkNames?: string[];
  ambiguousNameCeiling?: number;
  /** Run token shared by every resolver of one resolution run — the pool hands
   *  it to its workers. Resolvers with the same dbPath and generation share
   *  one in-memory node table; without it the table is private to the
   *  instance. */
  generation?: string;
  /** This connection sees every implements/extends edge the run will read
   *  (the live db, or a snapshot taken after the prerequisite phase), so the
   *  kernel may walk supertypes itself. */
  supertypesComplete?: boolean;
  /** `dbPath` is a private checkpointed copy nothing writes (a pool
   *  worker's snapshot): open it `immutable=1`, with no `-wal`/`-shm`. */
  snapshot?: boolean;
  /** Answer node lookups with indexed queries instead of loading the node
   *  table — cheaper for small batches such as an incremental sync. */
  queryLookups?: boolean;
}

/** One pending unresolved_refs row, kernel-read. */
export interface ResolveRefIn {
  rowId?: number;
  fromNodeId: string;
  referenceName: string;
  referenceKind: string;
  line: number;
  column: number;
  /** Raw candidates JSON as stored — callers that need it parse it. */
  candidates?: string;
  filePath: string;
  language: string;
  failureReason?: string;
}

export interface KernelCandidateOut {
  targetNodeId: string;
  confidence: number;
  resolvedBy: string;
}

/**
 * One verdict per input ref. `status`:
 *   resolved    — verdict (gates + alias forwarding applied)
 *   unresolved  — terminal miss
 *   passthrough — kernel declined; run the full TS pipeline
 * `candidates` is populated only when frameworks are active and the kernel
 * produced a non-final verdict — the raw [import?, name?] list for the TS
 * first-max merge with framework candidates.
 */
export interface ResolveOutcome {
  status: 'resolved' | 'unresolved' | 'passthrough' | string;
  targetNodeId?: string;
  confidence?: number;
  resolvedBy?: string;
  isFinal: boolean;
  candidates?: KernelCandidateOut[];
  /** Passthrough: the gate that declined (diagnostics; absent on older
   *  binaries — tally those under 'unknown'). On an `unresolved` outcome,
   *  `'defer'` marks a chain call the conformance pass retries when no
   *  framework claims it, and `'defer-this'` a `this.<member>` function ref
   *  the `this.<member>` pass retries. */
  reason?: string;
  /** From an arm resolveOneInner runs before the framework loop: the verdict
   *  stands without the framework merge. */
  preFramework?: boolean;
}

export interface KernelResolverLike {
  readPendingBatch(afterRowId: number, limit: number, prerequisites: boolean): ResolveRefIn[];
  resolveChunk(refs: ResolveRefIn[]): ResolveOutcome[];
  /** The conformance pass's per-ref chain match (resolveChainedCallsViaConformance). */
  resolveDeferredChains(refs: ResolveRefIn[]): ResolveOutcome[];
  /** The `this.<member>` pass's per-ref match (resolveDeferredThisMemberRefs). */
  resolveDeferredThisMembers(refs: ResolveRefIn[]): ResolveOutcome[];
  /** Deterministic conn teardown — must run while no other-build conn can do
   *  shm work (before pool workers spawn / after they die). Without it the
   *  rusqlite conn closes at GC time, whose shm teardown races node:sqlite
   *  conns (cross-build wal-index locks can't see each other). */
  close(): void;
}

export interface KernelModule {
  extractFile(filePath: string, content: string, language: string): KernelBuffers;
  /** Binding rows only, from the AST, for a TS/JS-family or ArkTS file the
   *  generic extractor extracts (resolution-binding-model-plan.md §2.4).
   *  OPTIONAL: absent on older binaries; the caller then emits no rows. */
  bindingsFile?(filePath: string, content: string, language: string): KernelBuffers;
  /** Native batch resolver over persisted bindings (Phase 4). OPTIONAL:
   *  absent on older binaries — the resolution loop keeps its TS path. */
  KernelResolver?: new (config: KernelResolverConfig) => KernelResolverLike;
  /** Parse-tree service for read-time consumers (Phase 3). OPTIONAL: absent
   *  on older binaries — kernel/tree.ts feature-detects and the consumers
   *  keep the wasm parser. */
  parseTree?(content: string, language: string): KernelTreeBuffers;
  /** Kind and field name tables for a grammar, fetched once per language. */
  treeNames?(language: string): KernelTreeNames | null;
  contractInfo(): KernelContractInfo;
  grammarInfo(language: string): KernelGrammarInfo | null;
  /** Batched cFnPtr extraction sweep (task #5 step 2). OPTIONAL: absent on
   *  older binaries — callers feature-detect and keep their JS path. */
  cfnptrScanFiles?(files: CfnptrFileIn[]): CfnptrFactsOut[];
  /** Path-driven, internally threaded sweep — the kernel reads each file
   *  itself. OPTIONAL: absent on older binaries. */
  cfnptrScanPaths?(files: CfnptrPathIn[]): CfnptrFactsOut[];
  /** Native `stripCommentsForRegex(text, 'c')` — differential-oracle hook. */
  cfnptrStripC?(text: string): string;
  /** Path-driven per-file env extraction for stage C's `buildEnv`, internally
   *  threaded — output is index-aligned with input. OPTIONAL: absent on older
   *  binaries — the synthesizer keeps its lazy LRU-cached extractor path. */
  cfnptrFileEnvs?(paths: string[]): (CfnptrFileEnvOut | null)[];
  /** Stages D+E of the fn-pointer synthesis — field←field propagation to a
   *  fixpoint, then dispatch-site edges — internally threaded, file-order
   *  deterministic. OPTIONAL: absent on older binaries. */
  cfnptrLink?(files: CfnptrLinkFileIn[], tables: CfnptrLinkTablesIn): CfnptrLinkOut;
}

const debugEnabled = () => process.env.CODEGRAPH_KERNEL_DEBUG === '1';
function debug(msg: string): void {
  if (debugEnabled()) process.stderr.write(`[codegraph-kernel] ${msg}\n`);
}

/** Languages the loaded binary supports (contract-verified). Empty when no kernel. */
let kernelLanguages: ReadonlySet<string> = new Set();
/** undefined = not attempted yet; null = attempted and unavailable. */
let cached: KernelModule | null | undefined;

function candidatePaths(): string[] {
  const candidates: string[] = [];
  if (process.env.CODEGRAPH_KERNEL_PATH) candidates.push(process.env.CODEGRAPH_KERNEL_PATH);
  const packageRoot = path.resolve(__dirname, '..', '..', '..');
  candidates.push(path.join(packageRoot, 'kernel', 'codegraph-kernel.node'));
  candidates.push(
    path.join(
      packageRoot,
      'codegraph-kernel',
      'prebuilds',
      `${process.platform}-${process.arch}`,
      'codegraph-kernel.node'
    )
  );
  return candidates;
}

/**
 * Verify the binary speaks our wire contract: same ABI version and byte-equal
 * NodeKind/EdgeKind tables (kinds cross the boundary as indexes into these).
 */
function verifyContract(mod: KernelModule, from: string): boolean {
  const info = mod.contractInfo();
  if (info.abiVersion !== KERNEL_ABI_VERSION) {
    debug(`${from}: ABI ${info.abiVersion} != expected ${KERNEL_ABI_VERSION} — ignoring kernel`);
    return false;
  }
  const sameTable = (a: readonly string[], b: readonly string[]) =>
    a.length === b.length && a.every((v, i) => v === b[i]);
  if (!sameTable(info.nodeKinds, NODE_KINDS) || !sameTable(info.edgeKinds, EDGE_KINDS)) {
    debug(`${from}: NodeKind/EdgeKind tables differ from src/types.ts — ignoring kernel`);
    return false;
  }
  return true;
}

/**
 * Load (once per process) and return the kernel module, or null when
 * unavailable. Fail-soft callers (feature probes, tests that skip without a
 * binary) use this; parse-time callers use {@link requireKernel}.
 */
/** Thrown by {@link requireKernel} when no usable kernel binary was found. */
export class KernelUnavailableError extends Error {
  constructor(readonly searched: string[]) {
    super(
      `CodeGraph's native engine (codegraph-kernel.node for ${process.platform}-${process.arch}) was not found. ` +
        `Looked in: ${searched.join(', ')}. ` +
        `Install a release bundle for this platform, or build from source with \`npm run build:kernel\` (needs a Rust toolchain).`
    );
    this.name = 'KernelUnavailableError';
  }
}

/**
 * The kernel, or a {@link KernelUnavailableError}. Since the wasm path was
 * removed (kernel-only-extraction-plan.md, Phase 5) there is no other parser,
 * so every parse-time caller goes through this rather than tolerating null.
 */
export function requireKernel(): KernelModule {
  const k = getKernel();
  if (!k) throw new KernelUnavailableError(candidatePaths());
  return k;
}

export function getKernel(): KernelModule | null {
  if (cached !== undefined) return cached;
  cached = null;
  for (const candidate of candidatePaths()) {
    try {
      if (!fs.existsSync(candidate)) continue;
      // createRequire: works identically from CJS output and future ESM.
      const req = createRequire(__filename);
      const mod = req(candidate) as KernelModule;
      if (typeof mod.extractFile !== 'function' || typeof mod.contractInfo !== 'function') {
        debug(`${candidate}: missing expected exports — ignoring`);
        continue;
      }
      if (!verifyContract(mod, candidate)) continue;
      kernelLanguages = new Set(mod.contractInfo().languages);
      debug(`loaded ${candidate} (languages: ${[...kernelLanguages].join(', ')})`);
      cached = mod;
      break;
    } catch (err) {
      debug(`${candidate}: failed to load — ${err instanceof Error ? err.message : String(err)}`);
    }
  }
  return cached;
}

/** True when the kill switch is off, a verified binary is loaded, and it supports `language`. */
export function kernelSupports(language: string): boolean {
  if (process.env.CODEGRAPH_KERNEL === '0') return false;
  return getKernel() !== null && kernelLanguages.has(language);
}

/** Test hook: forget the loaded module so a changed env is re-evaluated. */
export function resetKernelForTests(): void {
  cached = undefined;
  kernelLanguages = new Set();
}
