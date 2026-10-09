import type { SessionHit } from './index';

/** Offline prototype only. No CLI, MCP, index or automatic source reader calls this module. */
export interface T3MetadataScope {
  enabled: boolean;
  sourceInstance: string;
  projectId: string;
}

export interface T3ThreadAssociation {
  threadId: string;
  title: string;
  parentThreadId: string | null;
  link: string;
}

export interface T3MetadataResult {
  /** Original provider hits, without edits or reordering. Annotations are separate. */
  hits: readonly SessionHit[];
  annotations: Array<{ hitIndex: number; threads: T3ThreadAssociation[] }>;
  status: 'disabled' | 'unavailable' | 'ready' | 'partial';
  issues: Array<{ reason: string; threadId?: string }>;
}

interface ProviderThread {
  id: string;
  driver: string;
  nativeId: string | null;
  strength: 'strong' | 'weak' | 'none';
}

interface AppThread {
  id: string;
  title: string;
  projectId: string;
  parentThreadId: string | null;
  archived: boolean;
  deleted: boolean;
  providerThreads: ProviderThread[];
}

interface Snapshot {
  version: 1;
  sourceInstance: string;
  projectId: string;
  revision: string;
  complete: boolean;
  threads: AppThread[];
}

const MAX_BYTES = 1024 * 1024;
const UUID = '[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}';
const NATIVE_UUID = new RegExp(`^${UUID}$`);
const ROLLOUT = new RegExp(`^rollout-\\d{4}-\\d{2}-\\d{2}T\\d{2}-\\d{2}-\\d{2}-(${UUID})\\.jsonl$`);
const ID = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}$/;

function object(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid snapshot');
  const row = value as Record<string, unknown>;
  if (Object.keys(row).length !== keys.length || keys.some(key => !Object.hasOwn(row, key))) {
    throw new Error('invalid snapshot');
  }
  return row;
}

function id(value: unknown): string {
  if (typeof value !== 'string' || !ID.test(value)) throw new Error('invalid snapshot');
  return value;
}

function flag(value: unknown): boolean {
  if (typeof value !== 'boolean') throw new Error('invalid snapshot');
  return value;
}

function parseSnapshot(json: string): Snapshot {
  if (Buffer.byteLength(json, 'utf8') > MAX_BYTES) throw new Error('invalid snapshot');
  const row = object(JSON.parse(json), ['version', 'sourceInstance', 'projectId', 'revision', 'complete', 'threads']);
  if (row.version !== 1 || !Array.isArray(row.threads) || row.threads.length > 1000) throw new Error('invalid snapshot');
  const threadIds = new Set<string>();
  const nativeRefs = new Map<string, string>();
  const threads = row.threads.map(value => {
    const thread = object(value, ['id', 'title', 'projectId', 'parentThreadId', 'archived', 'deleted', 'providerThreads']);
    const threadId = id(thread.id);
    if (threadIds.has(threadId)) throw new Error('invalid snapshot');
    threadIds.add(threadId);
    if (typeof thread.title !== 'string' || !thread.title.trim() || thread.title.length > 512 || /[\x00-\x1f\x7f]/.test(thread.title)) {
      throw new Error('invalid snapshot');
    }
    if (!Array.isArray(thread.providerThreads) || thread.providerThreads.length > 64) throw new Error('invalid snapshot');
    const localRefs = new Set<string>();
    const providerThreads = thread.providerThreads.map(value => {
      const provider = object(value, ['id', 'driver', 'nativeId', 'strength']);
      const providerId = id(provider.id);
      if (localRefs.has(providerId)) throw new Error('invalid snapshot');
      localRefs.add(providerId);
      const driver = id(provider.driver);
      const nativeId = provider.nativeId === null ? null : id(provider.nativeId);
      if (provider.strength !== 'strong' && provider.strength !== 'weak' && provider.strength !== 'none') throw new Error('invalid snapshot');
      const signature = JSON.stringify([driver, nativeId, provider.strength]);
      if (nativeRefs.has(providerId) && nativeRefs.get(providerId) !== signature) throw new Error('invalid snapshot');
      nativeRefs.set(providerId, signature);
      return { id: providerId, driver, nativeId, strength: provider.strength } as ProviderThread;
    });
    return { id: threadId, title: thread.title, projectId: id(thread.projectId),
      parentThreadId: thread.parentThreadId === null ? null : id(thread.parentThreadId),
      archived: flag(thread.archived), deleted: flag(thread.deleted), providerThreads };
  });
  return { version: 1, sourceInstance: id(row.sourceInstance), projectId: id(row.projectId),
    revision: id(row.revision), complete: flag(row.complete), threads };
}

/** Only recognized rollout filenames corroborating the existing canonical key can join. */
function codexNativeId(hit: SessionHit): string | null {
  const basename = hit.file.split(/[\\/]/).at(-1) ?? '';
  const nativeId = ROLLOUT.exec(basename)?.[1];
  if (!nativeId || (hit.session !== `codex:${nativeId}` && hit.session !== `codex:${basename.slice(0, -6)}`)) return null;
  return nativeId;
}

/**
 * Decorate explicit provider hits with allowlisted, caller-supplied T3 metadata.
 * Identity strength is a source contract, not authentication of the caller.
 * This function opens no paths, stores no cache and does not render titles as markup.
 */
export function decorateT3SessionHits(
  hits: readonly SessionHit[],
  snapshotJson: string | undefined,
  scope: T3MetadataScope,
): T3MetadataResult {
  const result: T3MetadataResult = { hits, annotations: [], status: 'disabled', issues: [] };
  if (scope.enabled !== true) return result;
  result.status = 'unavailable';
  if (snapshotJson === undefined) {
    result.issues.push({ reason: 'snapshot-unavailable' });
    return result;
  }
  let snapshot: Snapshot;
  try {
    id(scope.sourceInstance);
    id(scope.projectId);
    snapshot = parseSnapshot(snapshotJson);
  } catch {
    result.issues.push({ reason: 'invalid-snapshot' });
    return result;
  }
  if (snapshot.sourceInstance !== scope.sourceInstance || snapshot.projectId !== scope.projectId) {
    result.issues.push({ reason: 'snapshot-scope-mismatch' });
    return result;
  }
  result.status = snapshot.complete ? 'ready' : 'partial';
  const eligible = snapshot.threads.filter(t => t.projectId === scope.projectId && !t.archived && !t.deleted);
  const visibleIds = new Set(eligible.map(t => t.id));
  const byNative = new Map<string, T3ThreadAssociation[]>();
  const hitNativeIds = new Set(hits.map(codexNativeId).filter((native): native is string => native !== null));
  for (const thread of snapshot.threads) {
    if (thread.projectId !== scope.projectId) {
      result.issues.push({ reason: 'foreign-project' });
      continue;
    }
    if (thread.archived || thread.deleted) continue;
    const added = new Set<string>();
    for (const provider of thread.providerThreads) {
      let reason: string | undefined;
      if (provider.driver !== 'codex') reason = 'unsupported-provider';
      else if (!provider.nativeId) reason = 'native-reference-missing';
      else if (provider.strength !== 'strong') reason = 'native-reference-unverified';
      else if (!NATIVE_UUID.test(provider.nativeId)) reason = 'native-reference-unsupported';
      else if (!hitNativeIds.has(provider.nativeId)) reason = 'provider-hit-unmatched';
      if (reason) {
        result.issues.push({ reason, threadId: thread.id });
        continue;
      }
      const native = provider.nativeId!;
      if (added.has(native)) continue;
      added.add(native);
      const aliases = byNative.get(native) ?? [];
      aliases.push({ threadId: thread.id, title: thread.title,
        parentThreadId: thread.parentThreadId && visibleIds.has(thread.parentThreadId) ? thread.parentThreadId : null,
        link: `t3-thread://v1/${thread.id}` });
      byNative.set(native, aliases);
    }
  }
  for (const aliases of byNative.values()) aliases.sort((a, b) => a.threadId < b.threadId ? -1 : a.threadId > b.threadId ? 1 : 0);
  hits.forEach((hit, hitIndex) => {
    const native = codexNativeId(hit);
    const aliases = native ? byNative.get(native) : undefined;
    if (aliases?.length) result.annotations.push({ hitIndex, threads: aliases.map(alias => ({ ...alias })) });
  });
  return result;
}
