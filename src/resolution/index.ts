/**
 * Reference Resolution Orchestrator
 *
 * Coordinates all reference resolution strategies.
 */

import * as fs from 'fs';
import * as path from 'path';
import { Binding, Language, Node, UnresolvedReference, Edge } from '../types';
import { QueryBuilder } from '../db/queries';
import {
  UnresolvedRef,
  ResolvedRef,
  ResolutionResult,
  KernelResolveStats,
  ResolutionContext,
  FrameworkResolver,
  ImportMapping,
  SUPERTYPE_TARGET_KINDS,
  isInheritanceRef,
  isImportableKind,
} from './types';
import { isBindingReceiverCall,  crossesKnownFamily, crossesCodeBoundary, resolveAmbiguousNameCeiling} from './gates';
import { extractImportMappings, importMappingsFromBindings,  loadCppIncludeDirs, isBoundToOutOfRepoImport, clearImportResolverMemos } from './import-resolver';
import { ResolverPool, minRefsForPool } from './resolver-pool';
import { resolveAliasBinding } from './alias-binding';
import { detectFrameworks } from './frameworks';
import { synthesizeCallbackEdges } from './callback-synthesizer';
import { createYielder, type MaybeYield } from './cooperative-yield';
import { loadProjectAliases, type AliasMap } from './path-aliases';
import { loadGoModule, type GoModule } from './go-module';
import { loadWorkspacePackages, type WorkspacePackages } from './workspace-packages';
import { logDebug } from '../errors';
import { lexicalPathWithinRoot, safeJsonParse } from '../utils';
import { builtinModules } from 'module';
import { getKernel, type KernelResolverLike, type ResolveRefIn, type ResolveOutcome } from '../extraction/kernel/loader';
import { LRUCache } from './lru-cache';

// SUPERTYPE_TARGET_KINDS (the kinds an extends/implements edge may TARGET)
// lives in ./types — the name-matcher needs the same set to restrict its
// candidate pool before ranking. It is deliberately wider than
// SUPERTYPE_BEARING_KINDS above, which is about the DECLARING side.

/** Below this many refs a sync's kernel looks nodes up by query instead of
 *  loading the whole node table: the load is a fixed cost that per-name
 *  queries only overtake near 16k refs (measured on ktor, ledger §5.74). */
const SYNC_NODE_TABLE_MIN_REFS = 15_000;
/** Refs per kernel `resolveChunk` call on the list paths. */
const KERNEL_CHUNK = 500;

type KernelStats = Required<KernelResolveStats>;

/**
 * Cache size limits. Each per-resolver cache is bounded so memory
 * stays flat on large codebases (20k+ files). Sizes were chosen to
 * cover the working set for typical resolution batches without
 * exceeding a few hundred MB worst-case. Override via the env var
 * `CODEGRAPH_RESOLVER_CACHE_SIZE` (single integer applied to all
 * caches) when tuning for very large or very small projects.
 */
const DEFAULT_CACHE_LIMIT = 5_000;
function resolveCacheLimit(): number {
  const raw = process.env.CODEGRAPH_RESOLVER_CACHE_SIZE;
  if (!raw) return DEFAULT_CACHE_LIMIT;
  const parsed = Number.parseInt(raw, 10);
  if (Number.isFinite(parsed) && parsed > 0) return parsed;
  return DEFAULT_CACHE_LIMIT;
}

/** C/C++ `Type(...)` that resolved to the class: keep `calls` on a constructor. */
function cppConstructorForType(type: Node, queries: QueryBuilder): Node | null {
  if (type.language !== 'cpp' && type.language !== 'c') return null;
  const suffix = `::${type.name}::${type.name}`;
  const exact = `${type.qualifiedName}::${type.name}`;
  const ctors = queries.getNodesByName(type.name).filter((n) =>
    n.kind === 'method' &&
    n.filePath === type.filePath &&
    n.language === type.language &&
    (n.qualifiedName === exact || n.qualifiedName.endsWith(suffix) || n.qualifiedName === `${type.name}::${type.name}`)
  );
  return ctors[0] ?? null;
}

// Re-export types
export * from './types';

/**
 * Reference Resolver
 *
 * Orchestrates reference resolution using multiple strategies.
 */
export class ReferenceResolver {
  private projectRoot: string;
  private queries: QueryBuilder;
  private context: ResolutionContext;
  private frameworks: FrameworkResolver[] = [];
  // Chained static-factory/fluent call refs the first pass couldn't resolve,
  // collected in-memory and left pending in the DB until the post-pass
  // finishes, so a restart can recover the queue (#1577). Drained by
  // resolveChainedCallsViaConformance
  // once implements/extends edges exist, to resolve methods on a supertype the
  // receiver conforms to (#750).
  private deferredChainRefs: UnresolvedRef[] = [];
  // `this.<member>` function-as-value refs whose member is NOT on the
  // enclosing class itself — possibly inherited. Collected in-memory for the
  // same reason as deferredChainRefs and drained by
  // resolveDeferredThisMemberRefs once implements/extends edges exist (#808).
  private deferredThisMemberRefs: UnresolvedRef[] = [];
  private deferredRowIds = new Set<number>();
  // All per-resolver caches are LRU-bounded. Previously these were
  // unbounded Maps that grew with every distinct lookup and OOM'd on
  // codebases with 20k+ files (see issue: unbounded cache growth).
  private nodeCache: LRUCache<string, Node[]>; // per-file node cache
  private fileCache: LRUCache<string, string | null>; // per-file content cache
  private importMappingCache: LRUCache<string, ImportMapping[]>;
  private bindingsCache: LRUCache<string, Binding[]>; // file → bindings rows
  private nameCache: LRUCache<string, Node[]>; // name → nodes cache
  private qualifiedNameCache: LRUCache<string, Node[]>; // qualified_name → nodes cache
  private fileLinesCache: LRUCache<string, string[] | null>; // file → split lines cache
  // Node kinds are a small fixed set (~24), so this is a plain Map, not an LRU.
  // getNodesByKind returns the FULL node list for a kind; it was previously
  // uncached — a per-ref `SELECT * FROM nodes WHERE kind=?` + row-mapping. Called
  // for every dotted call ref by the Spring resolver (constants) and every
  // `hook_` ref by the Drupal resolver (functions), that scan dominated
  // resolution on large repos (#1180). The node set is stable within a
  // resolution pass (same lifetime assumption as nameCache); clearCaches() resets
  // it between passes. Callers must treat the returned array as read-only.
  private nodesByKindCache = new Map<Node['kind'], Node[]>();
  private knownFiles: Set<string> | null = null;
  private cachesWarmed = false;
  // tsconfig/jsconfig path-alias map. `undefined` = not yet computed,
  // `null` = computed and absent. Treated as immutable for the
  // resolver's lifetime; callers re-create the resolver if config changes.
  private projectAliases: AliasMap | null | undefined = undefined;
  // go.mod module path. Same lazy/immutable convention as projectAliases.
  private goModule: GoModule | null | undefined = undefined;
  // Monorepo workspace member packages. Same lazy/immutable convention.
  private workspacePackages: WorkspacePackages | null | undefined = undefined;

  constructor(projectRoot: string, queries: QueryBuilder) {
    this.projectRoot = projectRoot;
    this.queries = queries;

    const limit = resolveCacheLimit();
    // The content cache is heavier (full file text), so we give it a
    // smaller budget than the metadata caches.
    const contentLimit = Math.max(64, Math.floor(limit / 5));
    this.nodeCache = new LRUCache(limit);
    this.fileCache = new LRUCache(contentLimit);
    this.importMappingCache = new LRUCache(limit);
    this.bindingsCache = new LRUCache(limit);
    this.nameCache = new LRUCache(limit);
    this.qualifiedNameCache = new LRUCache(limit);
    // Split-lines arrays are heavier than content strings; refs arrive
    // file-ordered, so a small cache still hits nearly always.
    this.fileLinesCache = new LRUCache(contentLimit);

    this.context = this.createContext();
  }

  /**
   * Initialize the resolver (detect frameworks, etc.)
   */
  initialize(): void {
    this.frameworks = detectFrameworks(this.context);
    this.clearCaches();
  }

  /**
   * Run each framework resolver's cross-file finalization pass and persist
   * the returned node updates. Idempotent — safe to call after every indexAll
   * and every incremental sync. Returns the number of nodes updated.
   *
   * Caches are cleared before/after so the post-extract pass sees fresh DB
   * state and downstream queries see the updated names.
   */
  runPostExtract(): number {
    let updated = 0;
    this.clearCaches();
    for (const fw of this.frameworks) {
      if (!fw.postExtract) continue;
      try {
        const nodes = fw.postExtract(this.context);
        for (const node of nodes) {
          this.queries.updateNode(node);
          updated++;
        }
      } catch (err) {
        logDebug(`Framework '${fw.name}' postExtract failed`, {
          error: err instanceof Error ? err.message : String(err),
        });
      }
    }
    if (updated > 0) this.clearCaches();
    return updated;
  }

  /**
   * Pre-build the known-file set the framework resolvers' file checks read.
   * Node lookups go through indexed SQLite queries.
   */
  warmCaches(): void {
    if (this.cachesWarmed) return;
    this.knownFiles = new Set(this.queries.getAllFilePaths());
    this.cachesWarmed = true;
  }

  /**
   * Re-run dynamic-edge synthesis over the current graph. The full index runs
   * it at the end of resolution; an incremental sync resolves only the changed
   * files and calls this afterwards, so the edges a changed file wires up come
   * back. Inserts are idempotent (INSERT OR IGNORE on the edge identity).
   */
  /**
   * Completed synthesis passes at the end of {@link resolveAndPersistBatched}.
   * A sync compares it across its orphan sweep to tell whether that pass
   * already refreshed the synthesized edges.
   */
  synthesisRuns = 0;

  async resynthesize(): Promise<number> {
    this.clearCaches();
    return synthesizeCallbackEdges(this.queries, this.context);
  }

  /**
   * Clear internal caches
   */
  clearCaches(): void {
    this.nodeCache.clear();
    this.fileCache.clear();
    this.importMappingCache.clear();
    this.bindingsCache.clear();
    this.nameCache.clear();
    this.qualifiedNameCache.clear();
    this.fileLinesCache.clear();
    this.nodesByKindCache.clear();
    this.knownFiles = null;
    this.cachesWarmed = false;
    // The import-resolver's per-context memos assume the
    // same stable window as the caches above — drop them together.
    if (this.context) {
      clearImportResolverMemos(this.context);
    }
  }

  /** `readFile` through the LRU content cache (null = read failed, also cached). */
  private readFileCached(filePath: string): string | null {
    if (this.fileCache.has(filePath)) {
      return this.fileCache.get(filePath)!;
    }
    const fullPath = path.join(this.projectRoot, filePath);
    try {
      const content = fs.readFileSync(fullPath, 'utf-8');
      this.fileCache.set(filePath, content);
      return content;
    } catch (error) {
      logDebug('Failed to read file for resolution', { filePath, error: String(error) });
      this.fileCache.set(filePath, null);
      return null;
    }
  }

  /**
   * Create the resolution context
   */
  private createContext(): ResolutionContext {
    return {
      // The kernel's import arm; open whenever frameworks resolve refs.
      resolveImport: (ref) => {
        const kernel = this.kernelResolver;
        if (!kernel) return null;
        return this.kernelVerdict(ref, kernel.resolveViaImportRef({
          rowId: ref.rowId,
          fromNodeId: ref.fromNodeId,
          referenceName: ref.referenceName,
          referenceKind: ref.referenceKind,
          line: ref.line,
          column: ref.column,
          filePath: ref.filePath,
          language: ref.language,
        }));
      },
      getNodesInFile: (filePath: string) => {
        if (!this.nodeCache.has(filePath)) {
          this.nodeCache.set(filePath, this.queries.getNodesByFile(filePath));
        }
        return this.nodeCache.get(filePath)!;
      },

      getNodesByName: (name: string) => {
        const cached = this.nameCache.get(name);
        if (cached !== undefined) return cached;
        const result = this.queries.getNodesByName(name);
        this.nameCache.set(name, result);
        return result;
      },

      getNodesByQualifiedName: (qualifiedName: string) => {
        const cached = this.qualifiedNameCache.get(qualifiedName);
        if (cached !== undefined) return cached;
        const result = this.queries.getNodesByQualifiedNameExact(qualifiedName);
        this.qualifiedNameCache.set(qualifiedName, result);
        return result;
      },

      getNodesByKind: (kind: Node['kind']) => {
        const cached = this.nodesByKindCache.get(kind);
        if (cached !== undefined) return cached;
        const result = this.queries.getNodesByKind(kind);
        this.nodesByKindCache.set(kind, result);
        return result;
      },

      // Streamed, uncached — synthesizers scan-and-filter whole kinds, and
      // both the materialized array AND the per-kind cache retention are
      // O(nodes) memory (#1212). Per-ref resolvers keep the cached array
      // variant above.
      iterateNodesByKind: (kind: Node['kind']) => this.queries.iterateNodesByKind(kind),

      fileExists: (filePath: string) => {
        // Check pre-built known files set first (O(1))
        if (this.knownFiles) {
          const normalized = filePath.replace(/\\/g, '/');
          if (this.knownFiles.has(filePath) || this.knownFiles.has(normalized)) {
            return true;
          }
        }
        // Fall back to filesystem for files not yet indexed. `path.join` does
        // not clamp, and relative-import resolution hands us paths carrying
        // `../` segments, so the probe has to be contained (#1631): a path
        // outside the root can never be an indexed project file, and the
        // `knownFiles` check above already answered for everything that is.
        // Lexical containment only: this is a per-candidate hot path, and the
        // symlink half of `validatePathWithinRoot` costs two `realpathSync`
        // calls per probe (~70x slower here). It would also be wrong to apply
        // — indexing deliberately follows in-root symlinks whose targets live
        // outside the root (#935), so only the `../` escape is refused.
        const fullPath = lexicalPathWithinRoot(this.projectRoot, filePath);
        if (fullPath === null) return false;
        try {
          return fs.existsSync(fullPath);
        } catch (error) {
          logDebug('Error checking file existence', { filePath, error: String(error) });
          return false;
        }
      },

      readFile: (filePath: string) => this.readFileCached(filePath),

      getFileLines: (filePath: string) => {
        const cached = this.fileLinesCache.get(filePath);
        if (cached !== undefined) return cached;
        const source = this.readFileCached(filePath);
        const lines = source === null ? null : source.split(/\r?\n/);
        this.fileLinesCache.set(filePath, lines);
        return lines;
      },

      getProjectRoot: () => this.projectRoot,

      getAllFiles: () => {
        return this.queries.getAllFilePaths();
      },

      listDirectories: (relativePath: string) => {
        const target = relativePath === '.' || relativePath === ''
          ? this.projectRoot
          : path.join(this.projectRoot, relativePath);
        try {
          return fs
            .readdirSync(target, { withFileTypes: true })
            .filter((entry) => entry.isDirectory())
            .map((entry) => entry.name);
        } catch (error) {
          logDebug('Failed to list directory for resolution', {
            relativePath,
            error: String(error),
          });
          return [];
        }
      },

      getNodeById: (id: string) => {
        return this.queries.getNodeById(id);
      },

      getBindings: (filePath: string) => {
        const cached = this.bindingsCache.get(filePath);
        if (cached) return cached;
        const rows = this.queries.getBindingsByFile(filePath);
        this.bindingsCache.set(filePath, rows);
        return rows;
      },

      getImportMappings: (filePath: string, language) => {
        const cacheKey = filePath;
        const cached = this.importMappingCache.get(cacheKey);
        if (cached) return cached;

        // A file with binding rows answers from them: one `import` row per
        // local name, ESM and `require` alike, from the AST.
        const fromRows = importMappingsFromBindings(this.context.getBindings!(filePath));
        if (fromRows) {
          this.importMappingCache.set(cacheKey, fromRows);
          return fromRows;
        }

        const content = this.context.readFile(filePath);
        if (!content) {
          this.importMappingCache.set(cacheKey, []);
          return [];
        }

        const mappings = extractImportMappings(filePath, content, language);
        this.importMappingCache.set(cacheKey, mappings);
        return mappings;
      },

      getProjectAliases: () => {
        if (this.projectAliases === undefined) {
          this.projectAliases = loadProjectAliases(this.projectRoot);
        }
        return this.projectAliases;
      },

      getGoModule: () => {
        if (this.goModule === undefined) {
          this.goModule = loadGoModule(this.projectRoot);
        }
        return this.goModule;
      },

      getWorkspacePackages: () => {
        if (this.workspacePackages === undefined) {
          this.workspacePackages = loadWorkspacePackages(this.projectRoot);
        }
        return this.workspacePackages;
      },

      getCppIncludeDirs: () => {
        return loadCppIncludeDirs(this.projectRoot);
      },
    };
  }

  /**
   * Resolve all unresolved references through the kernel. Opens the live
   * db's kernel conn when none is open and closes it again afterwards.
   */
  resolveAll(
    unresolvedRefs: UnresolvedReference[],
    onProgress?: (current: number, total: number) => void
  ): ResolutionResult {
    this.warmCaches();
    const opened = !this.kernelResolver;
    const kernel = this.liveKernel(unresolvedRefs.length);
    const resolved: ResolvedRef[] = [];
    const unresolved: UnresolvedRef[] = [];
    const byMethod: Record<string, number> = {};
    const stats = ReferenceResolver.emptyKernelStats();
    const total = unresolvedRefs.length;
    let lastReportedPercent = -1;
    try {
      for (let i = 0; i < total; i += KERNEL_CHUNK) {
        const chunk = unresolvedRefs.slice(i, i + KERNEL_CHUNK);
        const outcomes = this.resolveChunkWithKernel(kernel, chunk);
        for (let k = 0; k < chunk.length; k++) {
          const { ref, result } = this.settleKernelRef(chunk[k]!, outcomes[k]!, stats);
          if (result) {
            resolved.push(result);
            byMethod[result.resolvedBy] = (byMethod[result.resolvedBy] || 0) + 1;
          } else {
            unresolved.push(ref);
          }
        }
        // Report progress every 1% to avoid too many updates
        const done = Math.min(total, i + KERNEL_CHUNK);
        const currentPercent = Math.floor((done / total) * 100);
        if (onProgress && currentPercent > lastReportedPercent) {
          lastReportedPercent = currentPercent;
          onProgress(done, total);
        }
      }
    } finally {
      if (opened) this.closeKernel();
    }
    return {
      resolved,
      unresolved,
      stats: { total, resolved: resolved.length, unresolved: unresolved.length, byMethod },
    };
  }

  /**
   * The tail of every settled ref: unknown-receiver marking plus the `calls`
   * alias-binding forward, applied to the framework merge's winner.
   */
  private applyResolveTail(resolved: ResolvedRef | null, ref: UnresolvedRef): ResolvedRef | null {
    if (!resolved && isBindingReceiverCall(ref)) ref.failureReason = 'unknown-receiver';
    if (!resolved || ref.referenceKind !== 'calls') return resolved;

    const target = this.queries.getNodeById(resolved.targetNodeId);
    if (!target) return resolved;

    const dot = ref.referenceName.lastIndexOf('.');
    const memberName = dot >= 0 ? ref.referenceName.slice(dot + 1) : null;
    const forwarded = resolveAliasBinding(target, memberName, this.context);
    if (!forwarded || forwarded.id === resolved.targetNodeId) return resolved;

    return {
      ...resolved,
      targetNodeId: forwarded.id,
      confidence: Math.min(resolved.confidence, 0.85),
    };
  }

  // ------------------------------------------------------------------
  // The kernel resolves every ref (resolution-binding-model-plan.md §4 and
  // Phase 6): it reads bindings/nodes/unresolved_refs natively and returns a
  // verdict or a candidate list, and settleKernelOutcome runs the framework
  // merge over it.
  // ------------------------------------------------------------------

  /**
   * KernelResolver held per resolver instance — the main thread's batch loop
   * uses it when no pool runs, and each resolver-pool worker holds its own
   * (its 'open' handler calls initKernelResolver) so refs resolve natively
   * across cores. Construction is the expensive step (the kernel warms
   * its own tables); per-batch work is then a read + a settle call.
   */
  private kernelResolver: KernelResolverLike | null = null;
  /**
   * Checkpointed copy of the db file handed to resolver-pool workers' kernel
   * conns — see ensureKernelReaderSnapshot for why it exists. `undefined` =
   * not yet attempted this run; `null` = unavailable (fold/copy failed).
   */
  private kernelReaderSnapshot: string | null | undefined;

  /**
   * Open this instance's kernel resolver, replacing any open one. `dbPath`
   * undefined means the live db. Workers pass the pool's snapshot copy and
   * must never open the LIVE file: a rusqlite conn on the real -shm races
   * node:sqlite's wal-index state (the two SQLite builds' intra-process locks
   * can't see each other). Throws when the kernel can't open.
   */
  initKernelResolver(
    dbPath?: string,
    generation?: string,
    supertypesComplete = false,
    snapshot = false,
    queryLookups = false,
  ): void {
    this.closeKernel();
    this.kernelResolver = this.openKernelResolver(dbPath, generation, supertypesComplete, snapshot, queryLookups);
  }

  /** The open kernel resolver, or the live db's opened on demand. */
  private liveKernel(refCount: number): KernelResolverLike {
    if (!this.kernelResolver) {
      // The live db: prerequisites persist before the rest resolves, so every
      // supertype edge the walks read is already written. A small list looks
      // nodes up by query: loading the whole node table costs more than it.
      this.initKernelResolver(undefined, undefined, true, false, refCount < SYNC_NODE_TABLE_MIN_REFS);
    }
    return this.kernelResolver!;
  }

  /**
   * Release the kernel resolver at a known point (index teardown, valve
   * stopped, pool destroyed) rather than at GC time. A conn on the live db
   * is parked inside the kernel, not closed: closing any descriptor on the
   * db drops this process's POSIX locks, node:sqlite's included. A later pass
   * re-opens lazily and reuses it.
   */
  closeKernel(): void {
    const kr = this.kernelResolver;
    this.kernelResolver = null;
    try { kr?.close(); } catch { /* best-effort teardown */ }
  }

  /**
   * Fold the live WAL into the db file (off-thread) and copy the file once
   * per run; resolver-pool workers open THEIR KernelResolver connections on
   * the copy, never the live file. Why a copy rather than the live path:
   * the kernel links a second SQLite build, and the two builds' intra-process
   * wal-index locks (unixShmNode.aLock[]) are invisible to each other —
   * POSIX fcntl is per-process — so a worker kernel conn's walIndexRecover
   * can rebuild the shared -shm index while the node:sqlite writer commits,
   * and the writer then appends frames over committed ones (measured on the
   * linux corpus: whole cleanup transactions vanished from a checksum-clean
   * WAL). The copy is a separate inode with a private -shm over
   * extraction-static tables — resolveChunk reads no edges/unresolved_refs —
   * so worker kernel conns do zero shm I/O on the live db. Returns null on
   * any failure: the run then resolves on the main-thread kernel, no pool.
   */
  /**
   * A second snapshot for the same run, taken after the prerequisite phase so
   * it holds every implements/extends edge the call phase reads. A new path:
   * the workers still have the first copy open. Returns null on any failure
   * (the caller then destroys the pool and resolves on the main thread).
   */
  private async refreshKernelReaderSnapshot(fold: () => Promise<boolean>): Promise<string | null> {
    const dbPath = this.queries.getDatabasePath();
    if (!dbPath || !(await fold())) return null;
    const snap = `${dbPath}.kr-snapshot-${++this.kernelSnapshotSeq}`;
    try {
      fs.rmSync(`${snap}-wal`, { force: true });
      fs.rmSync(`${snap}-shm`, { force: true });
      await fs.promises.copyFile(dbPath, snap);
    } catch {
      return null;
    }
    this.kernelReaderSnapshot = snap;
    return snap;
  }
  private kernelSnapshotSeq = 0;

  private async ensureKernelReaderSnapshot(fold?: () => Promise<boolean>): Promise<string | null> {
    if (process.env.CODEGRAPH_NO_KERNEL_SNAPSHOT === '1') return null;
    if (this.kernelReaderSnapshot !== undefined) return this.kernelReaderSnapshot;
    this.kernelReaderSnapshot = null;
    const dbPath = this.queries.getDatabasePath();
    if (!dbPath || !fold) return null;
    try {
      // The copy is only complete once every WAL frame is backfilled into
      // the dbfile; a partial fold would hand workers a stale graph. The
      // fold runs off-thread — a multi-GB backfill inline could starve the
      // #850 watchdog on slow storage.
      if (!(await fold())) return null;
      const snap = `${dbPath}.kr-snapshot`;
      fs.rmSync(`${snap}-wal`, { force: true });
      fs.rmSync(`${snap}-shm`, { force: true });
      await fs.promises.copyFile(dbPath, snap);
      this.kernelReaderSnapshot = snap;
    } catch { /* non-WAL / un-copyable — workers run without a kernel conn */ }
    return this.kernelReaderSnapshot;
  }

  private openKernelResolver(
    parallelDbPath: string | undefined,
    generation?: string,
    supertypesComplete = false,
    snapshot = false,
    queryLookups = false,
  ): KernelResolverLike {
    const kernelModule = getKernel();
    if (!kernelModule?.KernelResolver) {
      throw new Error('The native kernel is unavailable on this platform; reference resolution needs it.');
    }
    const dbPath = parallelDbPath ?? this.queries.getDatabasePath();
    if (!dbPath) throw new Error('Reference resolution needs a file-backed database.');
    if (!snapshot) this.queries.mapWalIndex();
    const aliases = this.context.getProjectAliases?.() ?? null;
    const workspaces = this.context.getWorkspacePackages?.() ?? null;
    const goModule = this.context.getGoModule?.() ?? null;
    const toKv = (m: Map<string, string> | undefined): { key: string; value: string }[] =>
      m ? [...m].map(([key, value]) => ({ key, value })) : [];
    return new kernelModule.KernelResolver({
      dbPath,
      projectRoot: this.projectRoot,
      aliases: aliases
        ? { baseUrl: aliases.baseUrl, patterns: aliases.patterns }
        : undefined,
      workspaces: workspaces
        ? {
            sourceEntries: toKv(workspaces.sourceEntries),
            byName: toKv(workspaces.byName),
            entryByName: workspaces.entryByName ? toKv(workspaces.entryByName) : undefined,
            localLinkNames: workspaces.localLinkNames ? [...workspaces.localLinkNames] : undefined,
          }
        : undefined,
      goModulePath: goModule?.modulePath,
      cppIncludeDirs: this.context.getCppIncludeDirs?.() ?? [],
      nodeBuiltinSpecifiers: [...builtinModules],
      frameworksActive: this.frameworks.length > 0,
      frameworkNames: this.frameworks.map((f) => f.name),
      ambiguousNameCeiling: resolveAmbiguousNameCeiling(),
      generation,
      supertypesComplete,
      snapshot,
      // CODEGRAPH_KERNEL_QUERY_LOOKUPS=1 forces query mode everywhere — a
      // dev switch for gating it against the table on full indexes.
      queryLookups: queryLookups || process.env.CODEGRAPH_KERNEL_QUERY_LOOKUPS === '1',
    });
  }

  /** Kernel row → the UnresolvedReference the batch machinery carries. */
  private static kernelRowToUnresolved(kr: ResolveRefIn): UnresolvedReference {
    return {
      fromNodeId: kr.fromNodeId,
      referenceName: kr.referenceName,
      referenceKind: kr.referenceKind as UnresolvedReference['referenceKind'],
      line: kr.line,
      column: kr.column,
      candidates: kr.candidates ? safeJsonParse<string[] | undefined>(kr.candidates, undefined) : undefined,
      filePath: kr.filePath || undefined,
      language: (kr.language || undefined) as Language | undefined,
      rowId: kr.rowId ?? undefined,
      failureReason: kr.failureReason as UnresolvedReference['failureReason'],
    };
  }

  private kernelVerdict(ref: UnresolvedRef, outcome: ResolveOutcome): ResolvedRef | null {
    if (outcome.status !== 'resolved' || !outcome.targetNodeId) return null;
    return {
      original: ref,
      targetNodeId: outcome.targetNodeId,
      confidence: outcome.confidence ?? 0,
      resolvedBy: (outcome.resolvedBy ?? 'exact-match') as ResolvedRef['resolvedBy'],
    };
  }

  /**
   * Turn one kernel outcome into the ResolvedRef the persist paths expect.
   * Without frameworks the verdict stands on its own (the kernel already
   * applied gateTargetKind and the calls alias-forward). With frameworks the
   * framework loop runs here — resolveOneInner's Strategy 1 unchanged — and
   * the kernel's raw candidate list joins the merged first-max.
   */
  private settleKernelOutcome(ref: UnresolvedRef, outcome: ResolveOutcome): ResolvedRef | null {
    const verdict = this.kernelVerdict(ref, outcome);
    // Kernel `unresolved` without a candidate list is terminal — it means the
    // ref died at the builtin or prefilter gate, both of which sit BEFORE the
    // framework loop in resolveOneInner. Running frameworks here would
    // fabricate edges the sequential path never emits (e.g. react's resolver
    // claiming the REACT_HOOKS builtin `useState`). An exhausted ref that
    // should still see frameworks arrives with `candidates: []` (the kernel's
    // no_candidates marker), which falls through to the merge below.
    // The tail still runs: bound-receiver refusals stamp failureReason.
    if (outcome.status === 'unresolved' && !outcome.candidates) {
      // resolveThisMemberFnRef's deferral: the member may be inherited.
      if (outcome.reason === 'defer-this') this.deferReference(ref, this.deferredThisMemberRefs);
      return this.applyResolveTail(verdict, ref);
    }
    // An arm that runs before the framework loop in resolveOneInner (function
    // refs, JVM imports, Razor, PHP static calls, …): no framework may
    // overturn its verdict.
    if (outcome.preFramework && verdict) return verdict;
    if (this.frameworks.length === 0) {
      // A kernel verdict already passed gateTargetKind + the alias forward
      // inside finish — re-running the tail on it would double-forward alias
      // chains. Only a null verdict needs the tail's unknown-receiver stamp.
      if (verdict) return verdict;
      if (outcome.reason === 'defer') this.deferReference(ref, this.deferredChainRefs);
      return this.applyResolveTail(null, ref);
    }
    const candidates: ResolvedRef[] = [];
    for (const framework of this.frameworks) {
      const result = this.gateFrameworkLanguage(framework.resolve(ref, this.context), ref);
      if (result) {
        if (result.confidence >= 0.9) {
          // Same early-win as resolveOneInner — still passes the tail gates.
          return this.applyResolveTail(this.gateTargetKind(result, ref), ref);
        }
        candidates.push(result);
      }
    }
    // The kernel's ≥0.9 import verdict outranks <0.9 framework candidates —
    // resolveOneInner early-returns it and discards them. A verdict reported
    // without a candidate list is likewise already the merged winner.
    // A final miss (the chain branch) keeps only a ≥0.9 framework hit.
    if (outcome.isFinal) return verdict ?? this.applyResolveTail(null, ref);
    if (!outcome.candidates && verdict) return verdict;
    for (const kc of outcome.candidates ?? []) {
      candidates.push({
        original: ref,
        targetNodeId: kc.targetNodeId,
        confidence: kc.confidence,
        resolvedBy: kc.resolvedBy as ResolvedRef['resolvedBy'],
      });
    }
    if (candidates.length === 0) {
      // No framework took the chain call either: resolveOneInner defers it.
      if (outcome.reason === 'defer') this.deferReference(ref, this.deferredChainRefs);
      return this.applyResolveTail(null, ref);
    }
    const winner = candidates.reduce((best, curr) => (curr.confidence > best.confidence ? curr : best));
    return this.applyResolveTail(this.gateTargetKind(winner, ref), ref);
  }

  /**
   * Create edges from resolved references
   */
  createEdges(resolved: ResolvedRef[]): Edge[] {
    return resolved.flatMap((ref) => {
      // `function_ref` (#756) is internal-only: it persists as a `references`
      // edge (the registration site depends on the callback), distinguishable
      // by metadata.resolvedBy === 'function-ref'. callers/impact already
      // traverse `references`, so registration sites surface with no
      // graph-layer changes.
      let kind: Edge['kind'] =
        ref.edgeKind ??
        (ref.original.referenceKind === 'function_ref' ? 'references' : ref.original.referenceKind);

      // Promote "extends" to "implements" when a class/struct targets an interface
      if (kind === 'extends') {
        const targetNode = this.queries.getNodeById(ref.targetNodeId);
        if (targetNode && (targetNode.kind === 'interface' || targetNode.kind === 'protocol')) {
          const sourceNode = this.queries.getNodeById(ref.original.fromNodeId);
          if (sourceNode && sourceNode.kind !== 'interface' && sourceNode.kind !== 'protocol') {
            kind = 'implements';
          }
        }
      }

      // Promote "calls" to "instantiates" when the resolved target is a
      // class/struct/union. Languages without a `new` keyword (Python, Ruby)
      // express instantiation as `Foo()` — extraction can't tell that
      // apart from a function call without symbol info, but resolution
      // can: if `Foo` resolves to a class, the call IS an instantiation.
      // C/C++ `Type(...)` is the same shape, but when the type has a
      // constructor node the call is that constructor (nlohmann `items()`
      // → `iteration_proxy::iteration_proxy`). Keep `calls` then; only
      // promote when no constructor was extracted.
      if (kind === 'calls') {
        const targetNode = this.queries.getNodeById(ref.targetNodeId);
        if (
          targetNode &&
          (targetNode.kind === 'class' || targetNode.kind === 'struct' || targetNode.kind === 'union')
        ) {
          const ctor = cppConstructorForType(targetNode, this.queries);
          if (ctor) {
            ref = { ...ref, targetNodeId: ctor.id };
          } else {
            kind = 'instantiates';
          }
        }
      }

      // One reference can name several targets — a navigation whose
      // destination is a conditional reaches every arm. Each becomes its own
      // edge, sharing this resolution's kind and confidence.
      const targets = [
        { targetNodeId: ref.targetNodeId, metadata: ref.metadata },
        ...(ref.alsoTargets ?? []),
      ];
      return targets.map((t) => ({
        source: ref.original.fromNodeId,
        target: t.targetNodeId,
        kind,
        line: ref.original.line,
        column: ref.original.column,
        metadata: {
          ...(t.metadata ?? {}),
          confidence: ref.confidence,
          resolvedBy: ref.resolvedBy,
          // The ORIGINAL reference text (and kind, when edge-kind promotion
          // rewrote it — calls→instantiates, extends→implements,
          // function_ref→references). If this edge's target is later removed
          // by a re-index, the edge is resurrected as exactly this ref and
          // re-resolved (#1240 removal case) — a faithful resurrection, so
          // re-resolution can never bind anywhere a full re-index wouldn't.
          // Reconstruction from the target node's name instead would strip
          // receiver/qualifier context (`h.greet` → `greet`) and risk a
          // wrong rebind; edges without refName (pre-#1240, synthesized) are
          // deliberately NOT resurrected for the same reason.
          refName: ref.original.referenceName,
          ...(ref.original.referenceKind !== kind ? { refKind: ref.original.referenceKind } : {}),
          // Uniform marker for function-as-value edges (#756), regardless of
          // which strategy resolved them (import vs matchFunctionRef) — lets
          // tooling label "callback registration" and lets validation diff
          // exactly the edges this feature added.
          ...(ref.original.referenceKind === 'function_ref' ? { fnRef: true } : {}),
        },
      }));
    });
  }

  /**
   * Split resolved refs into rows deletable by id and hand-built refs that
   * must fall back to the key-tuple delete. Rows loaded from the database
   * carry their row id and are deleted by exactly that id; the key tuple
   * omits line/col, so it also removes SIBLING rows — the same caller calling
   * the same callee at other lines — that a later batch hadn't attempted yet:
   * when a batch boundary split a caller's same-named call sites, the later
   * sites' edges were silently never created (#1269).
   */
  private static partitionResolvedCleanup(resolved: ResolvedRef[]): {
    rowIds: number[];
    legacyKeys: Array<{ fromNodeId: string; referenceName: string; referenceKind: string }>;
  } {
    const rowIds: number[] = [];
    const legacyKeys: Array<{ fromNodeId: string; referenceName: string; referenceKind: string }> = [];
    for (const r of resolved) {
      if (r.original.rowId != null) rowIds.push(r.original.rowId);
      else legacyKeys.push({
        fromNodeId: r.original.fromNodeId,
        referenceName: r.original.referenceName,
        referenceKind: r.original.referenceKind,
      });
    }
    return { rowIds, legacyKeys };
  }

  /**
   * Same row-id precision for parking unresolvable refs as status='failed'
   * (#1240): the key-tuple fallback would flip same-key sibling rows in later
   * batches to 'failed' before they were ever attempted, and resolution
   * outcome can differ per call site (receiver-type inference reads the
   * ref's line), so a sibling must not inherit this row's failure (#1269).
   */
  private static partitionFailedCleanup(unresolved: UnresolvedRef[]): {
    byRowId: Array<{ rowId: number; referenceName: string; failureReason?: UnresolvedRef['failureReason'] }>;
    legacyKeys: Array<{ fromNodeId: string; referenceName: string; referenceKind: string; failureReason?: UnresolvedRef['failureReason'] }>;
  } {
    const byRowId: Array<{ rowId: number; referenceName: string; failureReason?: UnresolvedRef['failureReason'] }> = [];
    const legacyKeys: Array<{ fromNodeId: string; referenceName: string; referenceKind: string; failureReason?: UnresolvedRef['failureReason'] }> = [];
    for (const r of unresolved) {
      if (r.rowId != null) byRowId.push({ rowId: r.rowId, referenceName: r.referenceName, failureReason: r.failureReason });
      else legacyKeys.push({
        fromNodeId: r.fromNodeId,
        referenceName: r.referenceName,
        referenceKind: r.referenceKind,
        failureReason: r.failureReason,
      });
    }
    return { byRowId, legacyKeys };
  }

  /** A deferred attempt is unfinished work, not a final failure (#1577). */
  private nonDeferredFailures(unresolved: UnresolvedRef[]): UnresolvedRef[] {
    return unresolved.filter((ref) => ref.rowId == null || !this.deferredRowIds.has(ref.rowId));
  }

  private deferReference(ref: UnresolvedRef, queue: UnresolvedRef[]): void {
    queue.push(ref);
    if (ref.rowId != null) this.deferredRowIds.add(ref.rowId);
  }

  /**
   * Resolve and persist edges to database
   */
  resolveAndPersist(
    unresolvedRefs: UnresolvedReference[],
    onProgress?: (current: number, total: number) => void
  ): ResolutionResult {
    const prerequisites = unresolvedRefs.filter(ReferenceResolver.isPrerequisite);
    if (prerequisites.length > 0 && prerequisites.length < unresolvedRefs.length) {
      const first = this.resolveAndPersist(prerequisites, (current) => onProgress?.(current, unresolvedRefs.length));
      const rest = this.resolveAndPersist(
        unresolvedRefs.filter((ref) => !ReferenceResolver.isPrerequisite(ref)),
        (current) => onProgress?.(prerequisites.length + current, unresolvedRefs.length)
      );
      return ReferenceResolver.mergeResults(first, rest);
    }
    const result = this.resolveAll(unresolvedRefs, onProgress);

    // Create edges from resolved references
    const edges = this.createEdges(result.resolved);

    // Insert edges into database
    if (edges.length > 0) {
      this.queries.insertEdges(edges);
    }

    // Clean up resolved refs from unresolved_refs table so metrics are accurate
    if (result.resolved.length > 0) {
      const { rowIds, legacyKeys } = ReferenceResolver.partitionResolvedCleanup(result.resolved);
      this.queries.deleteReferencesByRowIds(rowIds);
      this.queries.deleteSpecificResolvedReferences(legacyKeys);
    }

    // Park unresolvable refs as status='failed' — parity with
    // resolveAndPersistBatched. Deleting them was wrong (#1240): a ref whose
    // own file never changes is otherwise gone forever, so when a DIFFERENT
    // file later gains the export/symbol that would satisfy it, no sync can
    // recreate the edge — only a full re-index. Failed rows are excluded from
    // the pending readers, which preserves the #1187 orphan sweep's
    // invariant in status form: after a COMPLETED pass nothing it processed
    // is still 'pending', so any pending row at rest belongs to an
    // interrupted run and the sweep can key off the pending count.
    if (result.unresolved.length > 0) {
      const { byRowId, legacyKeys } = ReferenceResolver.partitionFailedCleanup(this.nonDeferredFailures(result.unresolved));
      this.queries.markReferencesFailedByRowIds(byRowId);
      this.queries.markReferencesFailed(legacyKeys);
    }

    return result;
  }

  /**
   * Yielding counterpart of {@link resolveAndPersist} for a caller-supplied
   * ref list — used by sync's failed-ref retry pass (#1240). Same persistence
   * semantics: resolved refs become edges and their rows are deleted;
   * still-unresolvable refs are (re-)marked failed (a no-op for rows already
   * in that status). Yields per-ref because sync can run on the daemon's
   * liveness-watchdog thread (#850/#1091) and a retry set is unbounded when
   * a large edit lands many popular symbol names at once.
   */
  async resolveAndPersistListYielding(
    refs: UnresolvedReference[],
    options: {
      onProgress?: (current: number, total: number) => void;
      /** The caller's WAL valve: stopped while the kernel conn is open (see
       *  resolveAndPersistBatched's quiesceValveForKernel), restarted after. */
      walValve?: { stop(): void; start(): void; drain(): Promise<void> } | null;
    } = {},
  ): Promise<ResolutionResult> {
    const valve = options.walValve ?? null;
    if (valve) {
      valve.stop();
      await valve.drain();
    }
    this.liveKernel(refs.length);
    try {
      return await this.resolveAndPersistListInner(refs, options.onProgress);
    } finally {
      this.closeKernel();
      valve?.start();
    }
  }

  private async resolveAndPersistListInner(
    refs: UnresolvedReference[],
    onProgress?: (current: number, total: number) => void,
    done = 0,
    total = refs.length,
  ): Promise<ResolutionResult> {
    const prerequisites = refs.filter(ReferenceResolver.isPrerequisite);
    if (prerequisites.length > 0 && prerequisites.length < refs.length) {
      const first = await this.resolveAndPersistListInner(prerequisites, onProgress, done, total);
      const rest = await this.resolveAndPersistListInner(
        refs.filter((ref) => !ReferenceResolver.isPrerequisite(ref)),
        onProgress,
        done + prerequisites.length,
        total,
      );
      return ReferenceResolver.mergeResults(first, rest);
    }
    const maybeYield = createYielder();
    const result = await this.resolveBatchKernelFirst(refs, maybeYield);
    await this.persistResolutionResult(result, maybeYield);
    onProgress?.(done + refs.length, total);
    return result;
  }

  private async persistResolutionResult(result: ResolutionResult, maybeYield: MaybeYield): Promise<number> {
    const PERSIST_CHUNK = 1000;
    const edges = this.createEdges(result.resolved);
    for (let i = 0; i < edges.length; i += PERSIST_CHUNK) {
      this.queries.insertEdges(edges.slice(i, i + PERSIST_CHUNK));
      await maybeYield();
    }

    const resolvedCleanup = ReferenceResolver.partitionResolvedCleanup(result.resolved);
    for (let i = 0; i < resolvedCleanup.rowIds.length; i += PERSIST_CHUNK) {
      this.queries.deleteReferencesByRowIds(resolvedCleanup.rowIds.slice(i, i + PERSIST_CHUNK));
      await maybeYield();
    }
    for (let i = 0; i < resolvedCleanup.legacyKeys.length; i += PERSIST_CHUNK) {
      this.queries.deleteSpecificResolvedReferences(resolvedCleanup.legacyKeys.slice(i, i + PERSIST_CHUNK));
      await maybeYield();
    }

    const failedCleanup = ReferenceResolver.partitionFailedCleanup(this.nonDeferredFailures(result.unresolved));
    for (let i = 0; i < failedCleanup.byRowId.length; i += PERSIST_CHUNK) {
      this.queries.markReferencesFailedByRowIds(failedCleanup.byRowId.slice(i, i + PERSIST_CHUNK));
      await maybeYield();
    }
    for (let i = 0; i < failedCleanup.legacyKeys.length; i += PERSIST_CHUNK) {
      this.queries.markReferencesFailed(failedCleanup.legacyKeys.slice(i, i + PERSIST_CHUNK));
      await maybeYield();
    }

    return edges.length;
  }

  /** Finalize the durable queue only AFTER its edges have been inserted. */
  private async persistDeferredReferences(deferred: UnresolvedRef[], resolved: ResolvedRef[]): Promise<number> {
    for (const ref of deferred) if (ref.rowId != null) this.deferredRowIds.delete(ref.rowId);
    const matched = new Set(resolved.map((ref) => ref.original));
    const unresolved = deferred.filter((ref) => !matched.has(ref));
    const count = await this.persistResolutionResult({
      resolved,
      unresolved,
      stats: { total: deferred.length, resolved: resolved.length, unresolved: unresolved.length, byMethod: {} },
    }, createYielder());
    if (count > 0) this.clearCaches();
    return count;
  }

  /** Same two phases as the bounded DB reader: persist wiring before calls. */
  private static isPrerequisite(ref: UnresolvedReference): boolean {
    return ref.referenceKind === 'imports' || ref.referenceKind === 'extends' || ref.referenceKind === 'implements';
  }

  private static mergeResults(first: ResolutionResult, rest: ResolutionResult): ResolutionResult {
    const byMethod = { ...first.stats.byMethod };
    for (const [method, count] of Object.entries(rest.stats.byMethod)) {
      byMethod[method] = (byMethod[method] ?? 0) + count;
    }
    return {
      resolved: first.resolved.concat(rest.resolved),
      unresolved: first.unresolved.concat(rest.unresolved),
      stats: {
        total: first.stats.total + rest.stats.total,
        resolved: first.stats.resolved + rest.stats.resolved,
        unresolved: first.stats.unresolved + rest.stats.unresolved,
        byMethod,
      },
    };
  }

  /**
   * Second resolution pass for chained static-factory / fluent calls whose
   * chained method is defined on a SUPERTYPE the receiver's type conforms to —
   * a protocol-extension / inherited / default-interface method (#750). The
   * first pass can't resolve these because `implements`/`extends` edges aren't
   * built yet; this runs AFTER edges are persisted, so the kernel's
   * conformance walk can follow them.
   *
   * Operates only on the leftover unresolved refs that have the `inner().method`
   * chain shape, for the dotted-chain languages — a small set — and is idempotent
   * (re-resolving an already-resolved ref is a no-op since it's been deleted).
   * Returns the number of newly-created edges.
   */
  async resolveChainedCallsViaConformance(): Promise<number> {
    const deferred = this.deferredChainRefs;
    this.deferredChainRefs = [];
    if (deferred.length === 0) return 0;

    // Read fresh edges (the main pass built the implements/extends edges after
    // these refs were deferred).
    this.clearCaches();
    // This post-pass runs synchronously on the indexer's main thread; yield
    // periodically so the #850 liveness watchdog heartbeat can fire on a repo
    // with many deferred chained calls (#1091).
    const maybeYield = createYielder();
    const resolved: ResolvedRef[] = [];
    // The kernel runs the chain arms over the live db, which now holds every
    // supertype edge.
    const native = this.resolveDeferredNatively(deferred, (k, rows) => k.resolveDeferredChains(rows));
    for (let i = 0; i < deferred.length; i++) {
      const verdict = this.kernelVerdict(deferred[i]!, native[i]!);
      if (verdict) resolved.push(verdict);
      await maybeYield();
    }
    return this.persistDeferredReferences(deferred, resolved);
  }

  /** One kernel pass over a deferred queue. The caller has quiesced the WAL
   *  valve; teardown closes the conn. */
  private resolveDeferredNatively(
    deferred: UnresolvedRef[],
    run: (kernel: KernelResolverLike, rows: ResolveRefIn[]) => ResolveOutcome[],
  ): ResolveOutcome[] {
    const kernel = this.liveKernel(deferred.length);
    const rows: ResolveRefIn[] = deferred.map((ref) => ({
      rowId: ref.rowId,
      fromNodeId: ref.fromNodeId,
      referenceName: ref.referenceName,
      referenceKind: ref.referenceKind,
      line: ref.line,
      column: ref.column,
      filePath: ref.filePath,
      language: ref.language,
    }));
    const outcomes = run(kernel, rows);
    if (outcomes.length !== deferred.length) {
      throw new Error(`kernel deferred pass returned ${outcomes.length} outcomes for ${deferred.length} refs`);
    }
    return outcomes;
  }

  /** One kernel `resolveChunk` over `refs`, in order. Throws on any native failure. */
  private resolveChunkWithKernel(kernel: KernelResolverLike, refs: UnresolvedReference[]): ResolveOutcome[] {
    const kernelRows: ResolveRefIn[] = refs.map((raw) => ({
      rowId: raw.rowId,
      fromNodeId: raw.fromNodeId,
      referenceName: raw.referenceName,
      referenceKind: raw.referenceKind,
      line: raw.line,
      column: raw.column,
      candidates: raw.candidates ? JSON.stringify(raw.candidates) : undefined,
      filePath: raw.filePath || this.getFilePathFromNodeId(raw.fromNodeId) || '',
      language: raw.language || this.getLanguageFromNodeId(raw.fromNodeId) || '',
      failureReason: raw.failureReason,
    }));
    const outcomes = kernel.resolveChunk(kernelRows);
    if (outcomes.length !== refs.length) {
      throw new Error(`kernel resolveChunk returned ${outcomes.length} outcomes for ${refs.length} refs`);
    }
    return outcomes;
  }

  private static emptyKernelStats(): KernelStats {
    return { handled: 0, frameworkMerge: 0, frameworkMergeWithCands: 0 };
  }

  /** Settle one ref from its kernel outcome through the framework merge. */
  private settleKernelRef(
    raw: UnresolvedReference,
    outcome: ResolveOutcome,
    stats: KernelStats,
  ): { ref: UnresolvedRef; result: ResolvedRef | null } {
    const ref: UnresolvedRef = {
      fromNodeId: raw.fromNodeId,
      referenceName: raw.referenceName,
      referenceKind: raw.referenceKind,
      line: raw.line,
      column: raw.column,
      filePath: raw.filePath || this.getFilePathFromNodeId(raw.fromNodeId),
      language: raw.language || this.getLanguageFromNodeId(raw.fromNodeId),
      rowId: raw.rowId,
    };
    stats.handled++;
    // unresolved + a kernel candidate list = the no_candidates marker;
    // settleKernelOutcome still runs the framework merge over it.
    // candidates=[] merges can only produce framework candidates —
    // candidates=[…] is a real first-max over kernel + framework hits.
    if (outcome.candidates && this.frameworks.length > 0) {
      if (outcome.candidates.length === 0) stats.frameworkMerge++;
      else stats.frameworkMergeWithCands++;
    }
    return { ref, result: this.settleKernelOutcome(ref, outcome) };
  }

  /**
   * Resolve one batch through the open kernel conn, a chunk at a time, with
   * a yield checkpoint between every ref so the #850 liveness heartbeat can
   * fire on a slow or dense batch (#1091, #1122).
   */
  private async resolveBatchKernelFirst(
    batch: UnresolvedReference[],
    maybeYield: MaybeYield,
  ): Promise<ResolutionResult> {
    const kernel = this.liveKernel(batch.length);
    this.warmCaches();
    const resolved: ResolvedRef[] = [];
    const unresolved: UnresolvedRef[] = [];
    const byMethod: Record<string, number> = {};
    const stats = ReferenceResolver.emptyKernelStats();
    for (let i = 0; i < batch.length; i += KERNEL_CHUNK) {
      const chunk = batch.slice(i, i + KERNEL_CHUNK);
      const outcomes = this.resolveChunkWithKernel(kernel, chunk);
      for (let k = 0; k < chunk.length; k++) {
        const { ref, result } = this.settleKernelRef(chunk[k]!, outcomes[k]!, stats);
        if (result) {
          resolved.push(result);
          byMethod[result.resolvedBy] = (byMethod[result.resolvedBy] || 0) + 1;
        } else {
          unresolved.push(ref);
        }
        const y = maybeYield();
        if (y) await y;
      }
    }
    return {
      resolved,
      unresolved,
      stats: { total: batch.length, resolved: resolved.length, unresolved: unresolved.length, byMethod },
    };
  }

  /**
   * Resolve a list of refs and return everything the ADMISSION side needs to
   * persist the outcome: resolutions, failures, the deferred post-pass refs
   * this run produced (drained, so the caller owns routing them), and stats.
   * The resolver-worker entry point; results are in input order.
   */
  resolveListForAdmission(refs: UnresolvedReference[]): {
    resolved: ResolvedRef[];
    unresolved: UnresolvedRef[];
    deferredChain: UnresolvedRef[];
    deferredThisMember: UnresolvedRef[];
    byMethod: Record<string, number>;
    kernel?: KernelResolveStats;
  } {
    // Each pool worker holds a KernelResolver over its own snapshot copy
    // (opened at 'open'), so refs resolve natively across cores; the
    // framework merge runs here too (the worker's resolver detected them).
    const kernel = this.kernelResolver;
    if (!kernel) throw new Error('resolveListForAdmission: no kernel resolver is open');
    this.warmCaches();
    const resolved: ResolvedRef[] = [];
    const unresolved: UnresolvedRef[] = [];
    const byMethod: Record<string, number> = {};
    const kernelStats = ReferenceResolver.emptyKernelStats();
    const outcomes = this.resolveChunkWithKernel(kernel, refs);
    for (let i = 0; i < refs.length; i++) {
      const { ref, result } = this.settleKernelRef(refs[i]!, outcomes[i]!, kernelStats);
      if (result) {
        resolved.push(result);
        byMethod[result.resolvedBy] = (byMethod[result.resolvedBy] || 0) + 1;
      } else {
        unresolved.push(ref);
      }
    }
    this.deferredRowIds.clear(); // the admission side now owns both queues
    return {
      resolved,
      unresolved,
      deferredChain: this.deferredChainRefs.splice(0),
      deferredThisMember: this.deferredThisMemberRefs.splice(0),
      byMethod,
      kernel: kernelStats,
    };
  }

  /**
   * The resolver's live ResolutionContext — resolver-pool workers use it to
   * run synthesis passes against their own read-only connection.
   */
  getResolutionContext(): ResolutionContext {
    return this.context;
  }

  /**
   * Re-queue deferred post-pass refs produced by resolver workers, preserving
   * their admission order so resolveChainedCallsViaConformance /
   * resolveDeferredThisMemberRefs process them exactly as the sequential path
   * would have.
   */
  appendDeferredFromWorkers(deferredChain: UnresolvedRef[], deferredThisMember: UnresolvedRef[]): void {
    for (const ref of deferredChain) this.deferReference(ref, this.deferredChainRefs);
    for (const ref of deferredThisMember) this.deferReference(ref, this.deferredThisMemberRefs);
  }

  /**
   * Resolve and persist in batches to keep memory bounded.
   * Processes unresolved references in chunks, persisting edges and cleaning
   * up resolved refs after each batch to avoid accumulating large arrays.
   */
  async resolveAndPersistBatched(
    onProgress?: (current: number, total: number) => void,
    batchSize: number = 5000,
    onSynthesisProgress?: (done: number, total: number) => void,
    // When provided, big batches fan out across a read-only resolver-worker
    // pool with results admitted in canonical order (see resolver-pool.ts).
    // Main-thread fallback on any pool failure. CODEGRAPH_NO_PARALLEL_RESOLVE=1
    // disables entirely. bulkEdgeLoad hooks (when provided) bracket the batch
    // loop with drop/recreate of the non-unique edge indexes on big runs —
    // see DatabaseConnection.beginBulkEdgeLoad. backpressure (when provided)
    // is the WAL valve's writer-side backstop (WalCheckpointValve.backpressure):
    // called at pool-idle boundaries so a full backfill can actually complete —
    // the valve's timer-driven passive passes stay perpetually partial against
    // the pool's continuous reads, which is how a kernel-scale resolution grew
    // a 22GB WAL on a 4.6GB DB (migration plan §7a.1).
    parallel?: {
      dbPath: string;
      bulkEdgeLoad?: { begin: () => void; end: () => void | Promise<void> };
      /** unresolved_refs index window for the batched loop — the loop only
       *  reads the status index + PK; dropping the sync-path ref indexes cuts
       *  each per-batch DELETE's B-tree work (DatabaseConnection.beginBulkRefLoad). */
      refIndexLoad?: { begin: () => void; end: () => void | Promise<void> };
      backpressure?: () => Promise<void> | null;
      /** The off-thread checkpoint valve — while any kernel (rusqlite) conn
       *  shares the live -shm its SQLite build's wal-index locks are
       *  invisible to node:sqlite's (fcntl is per-process), so the valve's
       *  worker-thread checkpoints must not run. The loop stops it before
       *  kernel init and restarts it once no kernel conn is live. */
      walValve?: { stop(): void; start(): void; drain(): Promise<void> } | null;
      /** Off-thread full-WAL fold for the worker-kernel snapshot copy —
       *  resolves true only when every WAL frame is backfilled into the db
       *  file. See ensureKernelReaderSnapshot. */
      foldWalForSnapshot?: () => Promise<boolean>;
    }
  ): Promise<ResolutionResult> {
    // Resolution runs on the indexer's MAIN thread, and the #850 liveness
    // watchdog SIGKILLs a process whose event loop stalls past its window (60s
    // by default). A single dense batch's resolveAll — or the synthesis pass
    // below — can exceed that on a large repo, killing a VALID in-progress index
    // (#1091). A shared yielder lets both give the watchdog heartbeat a regular
    // window to fire; see ./cooperative-yield.
    const maybeYield = createYielder();

    if (process.env.CODEGRAPH_SYNTH_TIMINGS) {
      console.error(`[pool-timing] backpressure hook: ${parallel?.backpressure ? 'present' : 'absent'}`);
    }

    // CODEGRAPH_RESOLVE_PROFILE loop-stage attribution: the §7a.2 kernel-scale
    // histogram showed resolveOne owns only ~93s of the ~436s batch loop —
    // these counters name where the other ~340s goes (reads, edge build+insert,
    // deletes/marks, the per-batch count guard).
    const loopProf: Record<string, number> | null = process.env.CODEGRAPH_RESOLVE_PROFILE
      ? { read: 0, settle: 0, backpressure: 0, recycle: 0, createEdges: 0, insertEdges: 0, deletes: 0, marks: 0, countGuard: 0 }
      : null;
    const lp = (k: string, t0: number): void => { if (loopProf) loopProf[k] = (loopProf[k] ?? 0) + (Date.now() - t0); };
    let tLp = 0;

    this.warmCaches();

    // The main thread resolves through its own kernel conn whenever no pool
    // runs; with a pool, the workers' kernels do (see createPool).
    //
    // While a kernel (rusqlite) conn shares the live -shm, no other-build
    // shm writer may run: the bundled SQLite's wal-index locks can't see
    // node:sqlite's (POSIX fcntl is per-process), so an off-thread
    // checkpoint could rewrite the wal-index header under a writer commit →
    // frames appended over committed ones (the linux-corpus undo signature).
    // Quiesce the valve BEFORE the conn opens — open runs walIndexRecover —
    // and keep it stopped while any kernel conn is live; dropKernel
    // restarts it.
    const quiesceValveForKernel = async (): Promise<void> => {
      parallel?.walValve?.stop();
      await parallel?.walValve?.drain();
    };
    let kernel = this.kernelResolver;
    if (kernel) {
      // Covers a conn opened by an earlier call while the valve was running.
      await quiesceValveForKernel();
    } else {
      parallel?.walValve?.start(); // no kernel conn → off-thread checkpoints safe
    }
    // Open the main-thread kernel on the live db. Only while no pool is up:
    // once workers attach, no rusqlite conn may share the live -shm.
    const openMainKernel = async (): Promise<KernelResolverLike> => {
      if (kernel) return kernel;
      await quiesceValveForKernel();
      this.initKernelResolver(parallel?.dbPath, undefined, true);
      kernel = this.kernelResolver!;
      return kernel;
    };
    // Drop the kernel conn and re-arm the valve in one step (pool engage).
    // The close() is deliberate: a GC-timed destructor could fire while
    // workers or the valve are mid-shm I/O (cross-build locks can't see each
    // other), so the conn must tear down HERE, while the valve is stopped and
    // no worker has attached.
    const dropKernel = (): void => {
      kernel = null;
      this.closeKernel();
      parallel?.walValve?.start();
    };

    const total = this.queries.getUnresolvedReferencesCount();
    let processed = 0;
    const aggregateStats = {
      total: 0,
      resolved: 0,
      unresolved: 0,
      byMethod: {} as Record<string, number>,
      kernel: undefined as KernelResolveStats | undefined,
    };

    // Parallel pool, started before the loop. The first fan-out waits for
    // the workers to boot (module load + readonly DB open + framework detect
    // + cache warm + the snapshot kernel's node table). A pool failure
    // downgrades to the main-thread kernel permanently.
    let pool: ResolverPool | null = null;
    let poolReady = false;
    // Supertype walks read implements/extends edges, which only the
    // prerequisite phase writes. A worker snapshot taken before that phase
    // ends lacks some of them, so the snapshot is refreshed at the phase
    // boundary (refreshSnapshotIfDue) before any calls batch fans out.
    let inPrereqPhase = true;
    // Prerequisite batches read whose supertype edges are not inserted yet.
    // The prefetch reads the first calls page (ending the phase) while the
    // last prerequisite batch is still in flight, so the phase flag alone
    // says nothing about what the db holds.
    let prereqUnpersisted = 0;
    let prereqBatchesPersisted = 0;
    const supertypesPersisted = (): boolean => !inPrereqPhase && prereqUnpersisted === 0;
    let snapshotComplete = false;
    const destroyPool = async (): Promise<void> => {
      const p = pool;
      pool = null;
      poolReady = false;
      if (p) await p.destroy().catch(() => undefined);
    };
    const createPool = async (): Promise<ResolverPool | null> => {
      if (!parallel) return null;
      const t0 = Date.now();
      // Worker kernel conns resolve against a checkpointed COPY of the db —
      // their rusqlite build's wal-index locks are invisible to node:sqlite's,
      // so on the live file a worker's walIndexRecover could rebuild the -shm
      // header under a writer commit (the linux-corpus undo signature: whole
      // cleanup transactions overwritten, checksum-clean WAL). The copy is a
      // private inode — its own -shm — over extraction-static tables.
      // Preflight the decline gates first so a host that can't carry a pool
      // doesn't fold+copy a multi-GB snapshot it would never use.
      if (ResolverPool.preflight(parallel.dbPath) === null) return null;
      // Once workers attach, NO rusqlite conn may share the live -shm — not
      // even the main-thread kernel conn (only same-thread conns are
      // serialized). Close it BEFORE the snapshot fold and BEFORE workers
      // spawn: the fold is off-thread shm work (its wal-index reset can
      // leave the idle conn's mapping stale → its later close SIGBUSes), and
      // dropping the JS ref alone leaves teardown to GC time.
      dropKernel();
      const kernelDbPath = await this.ensureKernelReaderSnapshot(parallel.foldWalForSnapshot);
      if (process.env.CODEGRAPH_SYNTH_TIMINGS) {
        console.error(`[pool-timing] kernel-reader snapshot: ${kernelDbPath ?? 'unavailable — no pool'}`);
      }
      // Workers resolve only through a snapshot kernel.
      if (kernelDbPath === null) return null;
      snapshotComplete = supertypesPersisted();
      const p = ResolverPool.tryCreate(parallel.dbPath, this.projectRoot, kernelDbPath, snapshotComplete);
      p?.ready().then(
        () => {
          if (process.env.CODEGRAPH_SYNTH_TIMINGS) console.error(`[pool-timing] pool ready after ${Date.now() - t0}ms`);
        },
        () => undefined, // awaitPool handles the failure
      );
      return p;
    };
    // Wait for the workers to boot; a boot failure destroys the pool.
    const awaitPool = async (): Promise<boolean> => {
      if (!pool) return false;
      if (poolReady) return true;
      try {
        await pool.ready();
        poolReady = true;
      } catch (err) {
        logDebug('Resolver pool failed to start; resolving on the main thread', {
          error: err instanceof Error ? err.message : String(err),
        });
        await destroyPool();
      }
      return poolReady;
    };
    if (parallel && total >= minRefsForPool()) {
      pool = await createPool();
    }

    // Process in PIPELINED batches (double-buffer). The enumeration is the
    // head of the pending set in rowid order; every ref a persisted batch
    // processed leaves the pending set (resolved rows are deleted,
    // unresolvable ones flip to status='failed'), shifting the remaining
    // pending rows forward.
    let prevRemaining = Number.POSITIVE_INFINITY;

    // Cadence for the worker connection recycling below — ~8 batches
    // ≈ 40k refs between recycles keeps the WAL shallow at kernel scale
    // while a small sync never recycles at all. (25 recovered only half
    // the write tax — the WAL re-deepened between recycles; reopens are
    // sub-millisecond so the shorter cadence is ~free.)
    const RECYCLE_EVERY_BATCHES = (() => {
      const v = Number(process.env.CODEGRAPH_POOL_RECYCLE_EVERY);
      return Number.isFinite(v) && v > 0 ? Math.floor(v) : 8;
    })();
    let batchesSinceRecycle = 0;

    // Fan-out result of ResolverPool.resolveBatch, settled (never rejecting)
    // so a fan-out begun before the previous batch's persist can't produce an
    // unhandled rejection while it waits to be awaited.
    type PoolSettled =
      | { ok: true; out: Awaited<ReturnType<ResolverPool['resolveBatch']>> }
      | { ok: false; err: unknown };
    /** One page of pending refs: the loop's UnresolvedReference rows plus,
     *  when the kernel did the read, the native rows resolveChunk consumes. */
    type BatchPage = { refs: UnresolvedReference[]; kernelRefs: ResolveRefIn[] | null; prereq: boolean };
    type InFlight =
      | { mode: 'pool'; settled: Promise<PoolSettled> }
      | { mode: 'kernel'; outcomes: ResolveOutcome[] };

    // The last prerequisite batch's supertype edges are in: every edge the
    // walks read is persisted. Hand the workers a snapshot that has them (a
    // fresh copy only if a prerequisite batch wrote any) before the first
    // calls batch fans out. A failed refresh destroys the pool.
    const refreshSnapshotIfDue = async (): Promise<void> => {
      if (!pool || snapshotComplete || !supertypesPersisted() || !this.kernelReaderSnapshot || !parallel?.foldWalForSnapshot) return;
      snapshotComplete = true;
      tLp = Date.now();
      try {
        const stale = this.kernelReaderSnapshot;
        const fresh = prereqBatchesPersisted > 0
          ? await this.refreshKernelReaderSnapshot(parallel.foldWalForSnapshot)
          : stale;
        if (!fresh) throw new Error('snapshot copy failed');
        await pool.recycleWorkers(fresh);
        batchesSinceRecycle = 0;
        if (process.env.CODEGRAPH_SYNTH_TIMINGS) {
          console.error(`[pool-timing] kernel snapshot refreshed after the prerequisite phase: ${fresh} (${Date.now() - tLp}ms)`);
        }
        if (fresh !== stale) {
          for (const suf of ['', '-wal', '-shm']) {
            try { fs.rmSync(stale + suf, { force: true }); } catch { /* best-effort scratch cleanup */ }
          }
        }
      } catch (err) {
        logDebug('Kernel snapshot refresh failed; resolving on the main thread', {
          error: err instanceof Error ? err.message : String(err),
        });
        await destroyPool();
      }
      lp('recycle', tLp);
    };

    // Resolve a page on the main-thread kernel. Only while no pool is up.
    const resolveOnMainKernel = async (batch: BatchPage): Promise<ResolveOutcome[]> => {
      const k = await openMainKernel();
      if (!batch.kernelRefs) return this.resolveChunkWithKernel(k, batch.refs);
      const outcomes = k.resolveChunk(batch.kernelRefs);
      if (outcomes.length !== batch.kernelRefs.length) {
        throw new Error(`kernel resolveChunk returned ${outcomes.length} outcomes for ${batch.kernelRefs.length} refs`);
      }
      return outcomes;
    };

    // Begin one batch. With a pool, the workers resolve batch k+1 WHILE the
    // main thread persists batch k (persist measured at ~58% of resolution
    // wall on a 255k-ref repo). Each worker holds its own KernelResolver, so
    // the batch resolves natively across cores. Without a pool the main
    // kernel resolves eagerly here — it reads only nodes/bindings/files and
    // the supertype edges already persisted, so begin-time sees the same
    // state as settle-time.
    const beginBatch = async (batch: BatchPage): Promise<InFlight> => {
      if (await awaitPool()) await refreshSnapshotIfDue();
      if (pool && poolReady) {
        return {
          mode: 'pool',
          settled: pool.resolveBatch(batch.refs).then(
            (out) => ({ ok: true as const, out }),
            (err: unknown) => ({ ok: false as const, err })
          ),
        };
      }
      return { mode: 'kernel', outcomes: await resolveOnMainKernel(batch) };
    };

    // Settle an in-flight batch to a ResolutionResult. Deferred post-pass refs
    // are appended HERE, in loop order — never inside the fan-out promise — so
    // admission order stays exactly the sequential order even while a later
    // batch resolves concurrently. A pool failure downgrades to the main
    // kernel permanently and re-resolves this batch there.
    const settleBatch = async (
      inFlight: InFlight,
      batch: BatchPage
    ): Promise<ResolutionResult> => {
      if (inFlight.mode === 'pool') {
        const settled = await inFlight.settled;
        if (settled.ok) {
          this.appendDeferredFromWorkers(settled.out.deferredChain, settled.out.deferredThisMember);
          return {
            resolved: settled.out.resolved,
            unresolved: settled.out.unresolved,
            stats: {
              total: batch.refs.length,
              resolved: settled.out.resolved.length,
              unresolved: settled.out.unresolved.length,
              byMethod: settled.out.byMethod,
              kernel: settled.out.kernel,
            },
          };
        }
        logDebug('Parallel resolution failed; resolving on the main thread', {
          error: settled.err instanceof Error ? settled.err.message : String(settled.err),
        });
        await destroyPool();
        inFlight = { mode: 'kernel', outcomes: await resolveOnMainKernel(batch) };
      }
      const byMethod: Record<string, number> = {};
      const kernelStats = ReferenceResolver.emptyKernelStats();
      const resolved: ResolvedRef[] = [];
      const unresolved: UnresolvedRef[] = [];
      for (let i = 0; i < batch.refs.length; i++) {
        const { ref, result } = this.settleKernelRef(batch.refs[i]!, inFlight.outcomes[i]!, kernelStats);
        if (result) {
          resolved.push(result);
          byMethod[result.resolvedBy] = (byMethod[result.resolvedBy] || 0) + 1;
        } else {
          unresolved.push(ref);
        }
        const y = maybeYield();
        if (y) await y;
      }
      return {
        resolved,
        unresolved,
        stats: {
          total: batch.refs.length,
          resolved: resolved.length,
          unresolved: unresolved.length,
          byMethod,
          kernel: kernelStats,
        },
      };
    };

    // Bulk edge load: on big runs, drop the non-unique edge indexes for the
    // duration of the batch loop (the identity index stays — OR IGNORE dedup
    // and the source-keyed supertype-walk reads both live on it). Recreated in
    // the inner finally BEFORE synthesis, whose passes read kind-keyed.
    // Measured on a 224k-edge resolution set: insert 2.8s → 1.1s + 0.3s
    // recreate. Same ref-count gate as the pool so small syncs never pay the
    // recreate cost.
    let bulkEdgesActive = false;
    if (parallel?.bulkEdgeLoad && total >= minRefsForPool()) {
      try {
        parallel.bulkEdgeLoad.begin();
        bulkEdgesActive = true;
      } catch { /* keep the indexes; inserts just pay the per-row maintenance */ }
    }
    // Same gate for the ref-index window: the loop's deletes stop maintaining
    // the five sync-path unresolved_refs indexes, and the end-of-loop rebuild
    // is near-free (only failed refs survive the loop).
    let bulkRefsActive = false;
    if (parallel?.refIndexLoad && total >= minRefsForPool()) {
      try {
        parallel.refIndexLoad.begin();
        bulkRefsActive = true;
      } catch { /* keep the indexes; deletes just pay the per-row maintenance */ }
    }

    try {
    try {
    // Orphans retain interruption/re-extraction order, not clean-index order.
    // A caller can precede its imports or supertypes by many batches (#1577).
    // Drain those prerequisites first, then start a fresh keyset cursor over
    // the remaining kinds. The disjoint filters let us prefetch across the
    // phase boundary before cleanup without re-reading the current batch.
    let prerequisites = true;
    let afterRowId = 0;
    // One page of the keyset scan — kernel read when the main kernel is open
    // (identical predicate + ordering), TypeScript read otherwise.
    const readPage = (after: number, prereq: boolean): BatchPage => {
      if (kernel) {
        const kernelRefs = kernel.readPendingBatch(after, batchSize, prereq);
        return { refs: kernelRefs.map(ReferenceResolver.kernelRowToUnresolved), kernelRefs, prereq };
      }
      return {
        refs: this.queries.getUnresolvedReferencesBatchAfter(after, batchSize, prereq),
        kernelRefs: null,
        prereq,
      };
    };
    const readNextBatch = (): BatchPage => {
      let next = readPage(afterRowId, prerequisites);
      if (next.refs.length === 0 && prerequisites) {
        prerequisites = false;
        inPrereqPhase = false;
        afterRowId = 0;
        next = readPage(afterRowId, prerequisites);
      }
      if (next.refs.length > 0) afterRowId = next.refs[next.refs.length - 1]!.rowId!;
      if (next.refs.length > 0 && next.prereq) prereqUnpersisted++;
      return next;
    };
    tLp = Date.now();
    let batch = readNextBatch();
    lp('read', tLp);
    let inFlight: InFlight | null = batch.refs.length > 0 ? await beginBatch(batch) : null;
    while (batch.refs.length > 0 && inFlight) {
      // Prefetch the NEXT batch before this one persists: this batch's rows
      // are still pending (nothing has mutated the table since they were
      // read), so seeking past this batch's last row id in the same rowid
      // enumeration yields the following batch (keyset — OFFSET re-walked the
      // accumulated failed prefix every read, 54.6s at kernel scale, §7a.2).
      tLp = Date.now();
      const nextBatch = readNextBatch();
      lp('read', tLp);

      const tBatch = Date.now();
      const result = await settleBatch(inFlight, batch);
      if (process.env.CODEGRAPH_SYNTH_TIMINGS) console.error(`[pool-timing] batch ${inFlight.mode}: ${batch.refs.length} refs in ${Date.now() - tBatch}ms`);
      if (process.env.CODEGRAPH_RESOLVE_DEBUG) {
        const accounted = result.resolved.length + result.unresolved.length;
        const first = batch.refs[0]?.rowId ?? -1;
        const last = batch.refs[batch.refs.length - 1]?.rowId ?? -1;
        const seen = new Set<number>();
        for (const r of result.resolved) if (r.original.rowId != null) seen.add(r.original.rowId);
        for (const u of result.unresolved) if (u.rowId != null) seen.add(u.rowId);
        const missing = batch.refs.filter((r) => r.rowId != null && !seen.has(r.rowId)).length;
        if (missing > 0 || accounted !== batch.refs.length) {
          console.error(`[resolve-debug] batch [${first}..${last}] mode=${inFlight.mode} refs=${batch.refs.length} resolved=${result.resolved.length} unresolved=${result.unresolved.length} missingRowIds=${missing}`);
        }
      }
      lp('settle', tBatch);

      // WAL-valve backstop at the ONE pool-idle boundary of the double-buffer
      // (this batch settled, the next not yet fanned out): past the hard cap
      // the writer parks for a full backfill here, where the pool's readers
      // are all between statements — so the backfill completes, readers
      // re-enter at SQLite's backfilled mark, and the next persist commit
      // WRAPS the WAL instead of growing it. No-op (one fstat) under the cap.
      tLp = Date.now();
      // Skipped while a kernel conn is live — the valve is stopped then, and
      // a parked-barrier backfill is still an off-thread shm writer.
      const bp = kernel ? null : parallel?.backpressure?.();
      if (bp) await bp;
      lp('backpressure', tLp);

      // Recycle the workers' read connections periodically at this same
      // worker-idle boundary (batch k settled, batch k+1 not yet fanned
      // out): a long-lived reader pins WAL checkpoint progress, and the
      // deep WAL that accumulates behind it taxes the writer's OWN page
      // operations — the §7a.6 writes-under-readers finding (deletes
      // 42.6s → 118.8s from 0 to 4 attached readers; an aggressive valve
      // recovered the writes but paid +129s in full-park folds). Releasing
      // the read marks every ~25 batches lets the existing checkpoints
      // advance instead, at ~milliseconds of reopen cost. A failed recycle
      // downgrades to the main-thread kernel permanently, as a failed fan-out does.
      if (pool && poolReady && ++batchesSinceRecycle >= RECYCLE_EVERY_BATCHES) {
        batchesSinceRecycle = 0;
        tLp = Date.now();
        try {
          await pool.recycleWorkers();
        } catch (err) {
          logDebug('Worker connection recycle failed; resolving on the main thread', {
            error: err instanceof Error ? err.message : String(err),
          });
          await destroyPool();
        }
        lp('recycle', tLp);
      }

      // Persist in bounded sub-transactions with yields between: a whole
      // batch's edge insert / keyed deletes are otherwise one solid
      // synchronous span each on a multi-GB index, sitting BETWEEN the
      // per-ref yields — the last unyielded stretch of the resolution loop.
      // Crash semantics are unchanged (already several transactions): edges
      // land before their refs are deleted, so a kill mid-way re-resolves
      // the remainder idempotently on the next run/sweep (#1187).
      const PERSIST_CHUNK = 1000;
      const tPersist = Date.now();

      // Persist SUPERTYPE edges before fanning out the next batch: later
      // batches read this batch's edges — resolveMethodOnType walks
      // supertype chains over `extends`/`implements` edges that earlier
      // batches resolved, so a receiver typed as a subclass only reaches a
      // method declared on its base class if those edges are visible.
      // (Validated on dubbo: fanning out first downgraded exactly those
      // supertype-method resolutions from the 0.9 typed-receiver path to the
      // 0.65 word-overlap fallback.) Every OTHER edge kind has no mid-loop
      // reader — the only loop-time edge lookups are supertype/`contains`
      // walks, and `contains` edges all come from extraction — so they
      // persist AFTER fan-out, overlapped with the next batch's resolution
      // like the ref cleanup below. The extends→implements promotion in
      // createEdges stays inside the supertype set, so partitioning on the
      // ref's effective edge kind is exact; the identity index keys on kind
      // too, so dedup can't collide across the partition.
      tLp = Date.now();
      const supertypeRefs: ResolvedRef[] = [];
      const otherRefs: ResolvedRef[] = [];
      for (const r of result.resolved) {
        const k = r.edgeKind ?? r.original.referenceKind;
        (k === 'extends' || k === 'implements' ? supertypeRefs : otherRefs).push(r);
      }
      const supertypeEdges = this.createEdges(supertypeRefs);
      lp('createEdges', tLp);
      tLp = Date.now();
      for (let i = 0; i < supertypeEdges.length; i += PERSIST_CHUNK) {
        this.queries.insertEdges(supertypeEdges.slice(i, i + PERSIST_CHUNK));
        await maybeYield();
      }
      if (batch.prereq) {
        prereqUnpersisted--;
        prereqBatchesPersisted++;
      }
      lp('insertEdges', tLp);

      // NOW fan the next batch out — workers see the supertype edge state
      // the sequential baseline would, while the main thread spends the
      // REST of the persist (all other edges + ref deletes + failed parking
      // below) overlapped with their resolution — the double-buffer.
      const nextInFlight = nextBatch.refs.length > 0 ? await beginBatch(nextBatch) : null;

      tLp = Date.now();
      const otherEdges = this.createEdges(otherRefs);
      lp('createEdges', tLp);
      tLp = Date.now();
      for (let i = 0; i < otherEdges.length; i += PERSIST_CHUNK) {
        this.queries.insertEdges(otherEdges.slice(i, i + PERSIST_CHUNK));
        await maybeYield();
      }
      lp('insertEdges', tLp);

      // Clean up resolved refs so they don't appear in the next batch —
      // by row id, so a same-key sibling ref in a LATER batch (same caller
      // calling the same callee at another line) is left pending for its own
      // attempt instead of being swept out with this batch's rows (#1269).
      tLp = Date.now();
      let removedThisBatch = 0;
      const resolvedCleanup = ReferenceResolver.partitionResolvedCleanup(result.resolved);
      for (let i = 0; i < resolvedCleanup.rowIds.length; i += PERSIST_CHUNK) {
        removedThisBatch += this.queries.deleteReferencesByRowIds(resolvedCleanup.rowIds.slice(i, i + PERSIST_CHUNK));
        await maybeYield();
      }
      for (let i = 0; i < resolvedCleanup.legacyKeys.length; i += PERSIST_CHUNK) {
        removedThisBatch += this.queries.deleteSpecificResolvedReferences(resolvedCleanup.legacyKeys.slice(i, i + PERSIST_CHUNK));
        await maybeYield();
      }
      lp('deletes', tLp);

      // Park unresolvable refs from this batch as status='failed' so they
      // leave the pending set (the batch reader and non-progress guard below
      // only see pending rows) but stay retryable when a later sync adds a
      // symbol that could satisfy them (#1240).
      tLp = Date.now();
      const failures = this.nonDeferredFailures(result.unresolved);
      const deferredCount = result.unresolved.length - failures.length;
      const failedCleanup = ReferenceResolver.partitionFailedCleanup(failures);
      for (let i = 0; i < failedCleanup.byRowId.length; i += PERSIST_CHUNK) {
        removedThisBatch += this.queries.markReferencesFailedByRowIds(failedCleanup.byRowId.slice(i, i + PERSIST_CHUNK));
        await maybeYield();
      }
      for (let i = 0; i < failedCleanup.legacyKeys.length; i += PERSIST_CHUNK) {
        removedThisBatch += this.queries.markReferencesFailed(failedCleanup.legacyKeys.slice(i, i + PERSIST_CHUNK));
        await maybeYield();
      }
      lp('marks', tLp);
      if (process.env.CODEGRAPH_RESOLVE_DEBUG) {
        const first = batch.refs[0]?.rowId ?? -1;
        const last = batch.refs[batch.refs.length - 1]?.rowId ?? -1;
        console.error(`[resolve-debug] batch [${first}..${last}] mode=${inFlight.mode} refs=${batch.refs.length} res=${result.resolved.length} unres=${result.unresolved.length} deferred=${deferredCount} removed=${removedThisBatch}`);
      }
      if (process.env.CODEGRAPH_SYNTH_TIMINGS) console.error(`[pool-timing] batch persist: ${Date.now() - tPersist}ms`);

      // Aggregate stats
      aggregateStats.total += result.stats.total;
      aggregateStats.resolved += result.stats.resolved;
      aggregateStats.unresolved += result.stats.unresolved;
      for (const [method, count] of Object.entries(result.stats.byMethod)) {
        aggregateStats.byMethod[method] = (aggregateStats.byMethod[method] || 0) + count;
      }
      if (result.stats.kernel) {
        aggregateStats.kernel ??= { handled: 0 };
        aggregateStats.kernel.handled += result.stats.kernel.handled;
        aggregateStats.kernel.frameworkMerge =
          (aggregateStats.kernel.frameworkMerge ?? 0) + (result.stats.kernel.frameworkMerge ?? 0);
        aggregateStats.kernel.frameworkMergeWithCands =
          (aggregateStats.kernel.frameworkMergeWithCands ?? 0) +
          (result.stats.kernel.frameworkMergeWithCands ?? 0);
      }

      processed += batch.refs.length;
      onProgress?.(processed, total);

      // Yield so progress UI can render between batches
      await new Promise(resolve => setImmediate(resolve));

      // NOTE: there used to be an extra early break here when a batch resolved
      // nothing (`result.unresolved.length === batch.length`). That was wrong:
      // an all-unresolvable batch still DELETES its rows (progress), yet the
      // break abandoned every batch after it in the same run — on a repo whose
      // first 5000 refs are all external/stdlib calls, resolution stopped at
      // batch one and left the rest of the table as permanent orphans (#1187).
      // The count-based guard below catches the true no-progress case.

      // Non-progress guard (defense-in-depth). Ordinary attempts must leave
      // the pending set; a mismatched original reference can make legacy-key
      // cleanup a no-op. Keep the guard against that broken persistence even
      // though keyset pagination now advances independently of row cleanup.
      // An abandoned prefetched batch has no side effects until settleBatch.
      // Non-progress signal, now O(1): `changes` summed across this batch's
      // deletes + failed-parks is the DIRECT evidence the guard's old count
      // diff inferred — a resolver returning a mismatched name makes the keyed
      // cleanup no-op, which shows up here as zero removals. The per-batch
      // COUNT(*) it replaces walked every remaining pending row — O(N²/batch)
      // over a run, 93.9s of the kernel-scale batch loop (§7a.2). A REAL count
      // runs only on the suspicious path (claimed-work batch removed nothing —
      // e.g. every row was a sibling a legacy-key sweep already consumed),
      // where it arbitrates stop-vs-continue exactly as before.
      // Deferred refs legitimately remain pending for the post-pass. The
      // keyset cursor advances past them; they must not trigger this guard.
      if (removedThisBatch + deferredCount <= 0 && batch.refs.length > 0) {
        tLp = Date.now();
        const remaining = this.queries.getUnresolvedReferencesCount();
        lp('countGuard', tLp);
        if (remaining >= prevRemaining) break;
        prevRemaining = remaining;
      }

      // Advance the pipeline: the prefetched batch (already fanned out when
      // the pool is on) becomes the current one.
      batch = nextBatch;
      inFlight = nextInFlight;
    }
    } finally {
      // The kernel's settle work ended with the loop — close the conn now.
      // The index-recreate fold and synthesis both invoke backpressure,
      // which runs an OFF-THREAD checkpoint even while the valve's timer is
      // stopped: that must never coexist with a live rusqlite conn on the
      // same -shm (cross-build wal-index locks can't see each other).
      // Deferred passes lazily re-open it below.
      if (kernel) dropKernel();
      // Recreate the edge indexes BEFORE synthesis (kind-keyed reads) and on
      // any error path. A crash before this line is healed by the next
      // DatabaseConnection open (schema.sql re-applies IF NOT EXISTS).
      if (bulkRefsActive) {
        const tRef = Date.now();
        await parallel!.refIndexLoad!.end();
        if (process.env.CODEGRAPH_SYNTH_TIMINGS) console.error(`[phase-timing] ref-index-recreate: ${Date.now() - tRef}ms`);
      }
      if (bulkEdgesActive) {
        const tIdx = Date.now();
        await parallel!.bulkEdgeLoad!.end();
        if (process.env.CODEGRAPH_SYNTH_TIMINGS) console.error(`[phase-timing] edge-index-recreate: ${Date.now() - tIdx}ms`);
        // The recreate just wrote every non-unique edge index into the WAL
        // (multi-GB at kernel scale) with the pool idle — fold before the
        // synthesis passes pin readers against it for minutes.
        const bp = parallel?.backpressure?.();
        if (bp) await bp;
      }
    }

    // Dynamic-edge synthesis: now that all base `calls` edges are persisted,
    // synthesize observer/callback dispatch edges (dispatcher → registered
    // callbacks) that static parsing leaves out. Best-effort — never fail the
    // index on it. The pool (when it survived resolution) is REUSED to fan the
    // independent passes across its read-only workers — that's why its destroy
    // lives in the finally below, after synthesis, not at the end of the batch
    // loop. See docs/design/callback-edge-synthesis.md.
    const tSynth = Date.now();
    try {
      aggregateStats.byMethod['callback-synthesis'] = await synthesizeCallbackEdges(
        this.queries,
        this.context,
        onSynthesisProgress,
        pool,
        parallel?.backpressure
      );
      this.synthesisRuns++;
    } catch {
      // synthesis is additive and optional; ignore failures
    }
    if (process.env.CODEGRAPH_SYNTH_TIMINGS) console.error(`[phase-timing] callback-synthesis: ${Date.now() - tSynth}ms`);
    } finally {
      if (pool) await pool.destroy().catch(() => undefined);
      // The workers' kernel-reader snapshot is per-run scratch — remove it.
      // The pool is gone now, so a later pass (deferred/chained resolution,
      // a subsequent resolveAndPersistBatched) may lazily re-init the main
      // kernel conn on the live db: no cross-build concurrency remains.
      if (this.kernelReaderSnapshot !== undefined) {
        const snap = this.kernelReaderSnapshot;
        this.kernelReaderSnapshot = undefined;
        if (snap) {
          for (const suf of ['', '-wal', '-shm']) {
            try { fs.rmSync(snap + suf, { force: true }); } catch { /* best-effort scratch cleanup */ }
          }
        }
      }
    }

    if (process.env.CODEGRAPH_RESOLVE_DEBUG) {
      const pending = this.queries.getUnresolvedReferencesCount();
      console.error(`[resolve-debug] loop end: pending=${pending} deferredRowIds=${this.deferredRowIds.size} deferredChain=${this.deferredChainRefs.length} deferredThis=${this.deferredThisMemberRefs.length}`);
      if (pending > 0) {
        const rows = this.queries.getUnresolvedReferencesBatchAfter(0, 4000, undefined);
        const inDeferred = rows.filter((r) => r.rowId != null && this.deferredRowIds.has(r.rowId)).length;
        console.error(`[resolve-debug] pending sample(${rows.length}): deferredRowIds∩pending=${inDeferred} ${rows.slice(0, 10).map((r) => `${r.rowId}:${r.referenceKind}:${r.referenceName}`).join(' ')}`);
      }
    }
    if (aggregateStats.kernel && process.env.CODEGRAPH_RESOLVE_PROFILE) {
      const { handled, frameworkMerge, frameworkMergeWithCands } = aggregateStats.kernel;
      console.error(`[resolve-profile] kernel: handled=${handled}`);
      if (frameworkMerge || frameworkMergeWithCands) {
        console.error(
          `[resolve-profile] kernel framework-merge refs: no_candidates=${frameworkMerge ?? 0} with_candidates=${frameworkMergeWithCands ?? 0}`
        );
      }
    }
    if (loopProf) {
      const parts = Object.entries(loopProf).map(([k, v]) => `${k}=${(v / 1000).toFixed(1)}s`).join(' ');
      console.error(`[resolve-profile] loop-stages ${parts}`);
    }

    return {
      resolved: [],
      unresolved: [],
      stats: aggregateStats,
    };
  }

  /**
   * Get detected frameworks
   */
  getDetectedFrameworks(): string[] {
    return this.frameworks.map((f) => f.name);
  }

  /**
   * Get file path from node ID
   */
  private getFilePathFromNodeId(nodeId: string): string {
    const node = this.queries.getNodeById(nodeId);
    return node?.filePath || '';
  }

  /**
   * Get language from node ID
   */
  private getLanguageFromNodeId(nodeId: string): UnresolvedRef['language'] {
    const node = this.queries.getNodeById(nodeId);
    return node?.language || 'unknown';
  }

  /**
   * Second pass for `this.<member>` refs whose member wasn't on the enclosing
   * class itself (#808): once implements/extends edges exist, walk the
   * class's supertypes (transitively, depth-capped) and resolve the member on
   * the nearest one that declares it — `this.handleSubmit` registered in a
   * subclass resolves to `FormBase::handleSubmit`. Validated targets only
   * (function/method kind, same language family); no match → no edge.
   * Mirrors resolveChainedCallsViaConformance's lifecycle. Returns the number
   * of newly-created edges.
   */
  async resolveDeferredThisMemberRefs(): Promise<number> {
    const deferred = this.deferredThisMemberRefs;
    this.deferredThisMemberRefs = [];
    if (deferred.length === 0) return 0;

    this.clearCaches();
    // Synchronous main-thread post-pass with a per-ref supertype BFS — yield
    // periodically so the #850 liveness watchdog heartbeat can fire (#1091).
    const maybeYield = createYielder();
    const resolved: ResolvedRef[] = [];
    const native = this.resolveDeferredNatively(deferred, (k, rows) => k.resolveDeferredThisMembers(rows));
    for (let i = 0; i < deferred.length; i++) {
      const verdict = this.kernelVerdict(deferred[i]!, native[i]!);
      if (verdict) resolved.push(verdict);
      await maybeYield();
    }
    return this.persistDeferredReferences(deferred, resolved);
  }

  /**
   * Drop a resolution whose target cannot be what the reference names.
   * Applied at the `resolveOne` seam so it covers every strategy uniformly —
   * framework, import, name-match, chain, CFML component path.
   *
   * For `imports`: the target must be importable. A member that only exists
   * inside a type never is.
   *
   * For `extends`/`implements`, it cannot be describing a real supertype when:
   *
   *  1. The target's kind can never be a supertype (an enum member, a method,
   *     a variable). `matchByExactName` additionally narrows its candidate
   *     pool by the same set, so a legitimate supertype outranks a same-named
   *     non-type rather than merely losing its edge.
   *  2. The name is imported from outside the repo, so NO local node is the
   *     referent. Without this, filtering by kind alone just relocates the
   *     false edge onto the next same-named local type.
   *
   * Direction is one-way: this only ever REMOVES an edge, never adds one. A
   * dropped ref stays in `unresolved_refs` as `failed`, which is the honest
   * record for a supertype that lives outside the repo — silent beats wrong.
   */
  private gateTargetKind(result: ResolvedRef | null, ref: UnresolvedRef): ResolvedRef | null {
    if (!result) return result;

    // An `imports` reference names something importable — never a member that
    // only exists inside a type.
    if (ref.referenceKind === 'imports') {
      const target = this.queries.getNodeById(result.targetNodeId);
      return target && !isImportableKind(target.kind) ? null : result;
    }

    if (!isInheritanceRef(ref)) return result;
    const target = this.queries.getNodeById(result.targetNodeId);
    if (target && !SUPERTYPE_TARGET_KINDS.has(target.kind)) return null;
    if (isBoundToOutOfRepoImport(ref, this.context)) return null;
    return result;
  }

  /**
   * Drop a FRAMEWORK-strategy resolution that crosses two *known* language
   * families for a type-usage (`references`) or import-binding (`imports`)
   * edge. The framework strategy is intentionally ungated for cross-language
   * bridges, but those legitimate bridges are either `calls` edges (RN/Expo
   * JS → native) or config↔code edges whose config side (`yaml`/`blade`/…) is
   * not a known programming-language family. A `references`/`imports` edge
   * between two *known* families is always a coincidental name collision — the
   * React/Svelte/Vue PascalCase component resolvers name-match `getNodesByName`
   * without a language check, so a TS `<TestRunner>` ref happily matched a
   * Kotlin `class TestRunner`. Gating only the both-known-cross-family case
   * lets config bridges and `calls` bridges through untouched. Anything else
   * that crosses a code boundary is the same collision — a Svelte
   * `new String()` is not a Dart class — and so is a call bridge that lands
   * on something other than a function or method (JS `Function(…)` is not a
   * Kotlin class).
   */
  private gateFrameworkLanguage(result: ResolvedRef | null, ref: UnresolvedRef): ResolvedRef | null {
    if (!result) return result;
    const tgt = this.getLanguageFromNodeId(result.targetNodeId);
    if (tgt && ref.language && crossesCodeBoundary(tgt, ref.language)) {
      if (ref.referenceKind !== 'calls') return null;
      const kind = this.queries.getNodeById(result.targetNodeId)?.kind;
      if (kind !== 'function' && kind !== 'method') return null;
    }
    if (ref.referenceKind !== 'references' && ref.referenceKind !== 'imports') return result;
    // Package imports cannot target prose found by a framework's name lookup.
    if (ref.referenceKind === 'imports' && tgt === 'markdown' && ref.language !== 'markdown') return null;
    if (tgt && ref.language && crossesKnownFamily(tgt, ref.language)) return null;
    return result;
  }
}

/**
 * Create a reference resolver instance
 */
export function createResolver(projectRoot: string, queries: QueryBuilder): ReferenceResolver {
  const resolver = new ReferenceResolver(projectRoot, queries);
  resolver.initialize();
  return resolver;
}
