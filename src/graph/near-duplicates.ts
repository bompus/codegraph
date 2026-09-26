/**
 * Near-duplicate function bodies.
 *
 * Copy-pasted helpers drift: a fix lands in one copy and the others keep the
 * bug. Each function or method body gets a MinHash signature over 4-token
 * shingles. LSH banding finds candidate pairs without comparing
 * every body to every other; a candidate is kept when the exact Jaccard of the
 * two bodies' shingles is at least `NEAR_DUPLICATE_THRESHOLD`. (The 64-hash
 * estimate alone is only good to about ±0.04, so copies near the threshold
 * came and went between indexes.)
 *
 * Measured before building (ledger 5.51): on two repositories every sampled
 * pair above 0.8 was a real copy, and an agent shown the copies of the
 * function it was fixing named them to the user in 2 of 3 runs against 0 of 3
 * without.
 *
 * Test files, generated files and vendored code are left out: duplicated test
 * setup and vendored copies of a library are expected, and they drowned the
 * real pairs when included. So is a family of more than `FAMILY_CAP` alike
 * bodies (a framework's one-line override repeated in every view): that is a
 * pattern, not a set of copies to keep in step. Literals stay tokens for the
 * same reason; folding them made every `reverse('…')` one body.
 */

import type { QueryBuilder } from '../db/queries';
import { isTestFile } from '../search/query-utils';

export const NEAR_DUPLICATE_THRESHOLD = 0.8;

const HASHES = 64;
const BANDS = 16;
const ROWS = HASHES / BANDS;
const SHINGLE = 4;
/** Smaller bodies (a one-line getter, a delegating wrapper) are alike by construction. */
const MIN_TOKENS = 30;
const MIN_LINES = 5;
/** A band bucket this full is shared boilerplate, not a clone family. */
const BUCKET_CAP = 20;
/** More alike bodies than this is a pattern; none of its pairs is reported. */
const FAMILY_CAP = 8;

const TOKEN = /[A-Za-z_$][\w$]*|\d[\w.]*|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`[^`]*`|\S/g;
const VENDORED = /(?:^|\/)(?:vendor|vendored|third_party|third-party|node_modules|grammars)\/|\.(?:min|bundle)\.js$/;

/** Per-hash permutations `a·x + b mod 2^32`, fixed so signatures are stable across runs. */
const PERM_A = new Int32Array(HASHES);
const PERM_B = new Int32Array(HASHES);
{
  let state = 0x9e3779b9;
  const next = () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return state | 0;
  };
  for (let k = 0; k < HASHES; k++) {
    PERM_A[k] = next() | 1;
    PERM_B[k] = next();
  }
}

/** FNV-1a over a string: a token's identity. */
function fnv1a(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

export function tokenize(body: string): string[] {
  return body.match(TOKEN) ?? [];
}

/**
 * A body's shingle hashes, or null when it is too small to compare. The
 * function's own name is masked: copies are usually renamed (`fpUrlOf`,
 * `ttdUrlOf`), and the name alone moves several shingles.
 */
export function shingles(body: string, ownName?: string): Set<number> | null {
  const tokens = tokenize(body).map((t) => (t === ownName ? '\u0000self' : t));
  if (tokens.length < MIN_TOKENS) return null;
  // Each token hashed once; a shingle's hash mixes its tokens' in order.
  const th = tokens.map(fnv1a);
  const out = new Set<number>();
  for (let i = 0; i + SHINGLE <= th.length; i++) {
    let h = th[i]!;
    for (let j = 1; j < SHINGLE; j++) h = Math.imul(h ^ th[i + j]!, 0x01000193) ^ (h >>> 15);
    out.add(h >>> 0);
  }
  return out;
}

/** The share of shingles two bodies have in common. */
export function jaccard(a: Set<number>, b: Set<number>): number {
  let common = 0;
  for (const x of a) if (b.has(x)) common++;
  return common / (a.size + b.size - common);
}

/** The MinHash signature of a body, or null when it is too small to compare. */
export function signature(body: string, ownName?: string): Uint32Array | null {
  const set = shingles(body, ownName);
  if (!set) return null;
  const sig = new Uint32Array(HASHES).fill(0xffffffff);
  for (const x of set) {
    for (let k = 0; k < HASHES; k++) {
      const v = (Math.imul(PERM_A[k]!, x) + PERM_B[k]!) >>> 0;
      if (v < sig[k]!) sig[k] = v;
    }
  }
  return sig;
}

/** The share of hashes two signatures agree on: an estimate of their shingle overlap. */
export function similarity(a: Uint32Array, b: Uint32Array): number {
  let same = 0;
  for (let k = 0; k < HASHES; k++) if (a[k] === b[k]) same++;
  return same / HASHES;
}

function asSignature(bytes: Uint8Array): Uint32Array {
  return new Uint32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
}

export interface NearDuplicateDeps {
  queries: QueryBuilder;
  /** The file's text, or null when it cannot be read. */
  readFile(filePath: string): string | null;
  /** Generated or configured-away files, left out like tests and vendored code. */
  isExcluded?(filePath: string): boolean;
}

/**
 * Bring signatures up to date with the index and, when any changed, recompute
 * the pairs. Returns how many signatures were written and pairs stored
 * (`pairs` is null when nothing changed and the stored pairs stand).
 */
export function refreshNearDuplicates(deps: NearDuplicateDeps): { signed: number; pairs: number | null } {
  const { queries } = deps;
  const excluded = (p: string) => isTestFile(p) || VENDORED.test(p) || (deps.isExcluded?.(p) ?? false);

  // Group stale bodies by file so each file is read once.
  const stale = new Map<string, Array<{ id: string; name: string; startLine: number; endLine: number; updatedAt: number }>>();
  const drop: string[] = [];
  for (const c of queries.minhashCandidates(MIN_LINES)) {
    if (excluded(c.filePath)) {
      if (c.sigUpdatedAt !== null) drop.push(c.id);
      continue;
    }
    if (c.sigUpdatedAt === c.updatedAt) continue;
    const list = stale.get(c.filePath) ?? [];
    list.push(c);
    stale.set(c.filePath, list);
  }

  const rows: Array<{ nodeId: string; nodeUpdatedAt: number; sig: Uint8Array }> = [];
  for (const [filePath, bodies] of stale) {
    const lines = deps.readFile(filePath)?.split(/\r?\n/);
    for (const b of bodies) {
      const sig = lines ? signature(lines.slice(b.startLine - 1, b.endLine).join('\n'), b.name) : null;
      rows.push({ nodeId: b.id, nodeUpdatedAt: b.updatedAt, sig: sig ? new Uint8Array(sig.buffer) : new Uint8Array(0) });
    }
  }
  queries.upsertMinhash(rows);
  queries.deleteMinhash(drop);
  if (rows.length === 0 && drop.length === 0) return { signed: 0, pairs: null };
  return { signed: rows.length, pairs: pairNearDuplicates(deps) };
}

/** LSH over the stored signatures, one band at a time so memory stays linear in the bodies. */
function pairNearDuplicates(deps: NearDuplicateDeps): number {
  const { queries } = deps;
  const candidates = new Set<string>();
  const bandBytes = ROWS * 4;
  for (let band = 0; band < BANDS; band++) {
    const buckets = new Map<string, number[]>();
    for (const { rowid, band: bytes } of queries.minhashBand(band * bandBytes, bandBytes)) {
      const key = Buffer.from(bytes).toString('base64');
      const bucket = buckets.get(key);
      if (bucket) bucket.push(rowid);
      else buckets.set(key, [rowid]);
    }
    for (const ids of buckets.values()) {
      if (ids.length < 2 || ids.length > BUCKET_CAP) continue;
      for (let i = 0; i < ids.length; i++) {
        for (let j = i + 1; j < ids.length; j++) candidates.add(`${ids[i]}:${ids[j]}`);
      }
    }
  }

  // Candidates are decided on their exact shingle overlap, read back from
  // source; the estimate from the signatures is the fallback when a body
  // cannot be read.
  const files = new Map<string, string[] | null>();
  const lines = (filePath: string) => {
    if (!files.has(filePath)) files.set(filePath, deps.readFile(filePath)?.split(/\r?\n/) ?? null);
    return files.get(filePath)!;
  };
  const sigs = new Map<number, { nodeId: string; sig: Uint32Array; set: Set<number> | null }>();
  const load = (rowid: number) => {
    let hit = sigs.get(rowid);
    if (!hit) {
      const row = queries.minhashByRowid(rowid);
      if (!row) return null;
      const node = queries.getNodeById(row.nodeId);
      const text = node ? lines(node.filePath)?.slice(node.startLine - 1, node.endLine).join('\n') : undefined;
      hit = { nodeId: row.nodeId, sig: asSignature(row.sig), set: text ? shingles(text, node!.name) : null };
      sigs.set(rowid, hit);
    }
    return hit;
  };
  const pairs: Array<{ a: string; b: string; score: number }> = [];
  const degree = new Map<string, number>();
  for (const key of candidates) {
    const [x, y] = key.split(':').map(Number) as [number, number];
    const a = load(x);
    const b = load(y);
    if (!a || !b) continue;
    const score = a.set && b.set ? jaccard(a.set, b.set) : similarity(a.sig, b.sig);
    if (score < NEAR_DUPLICATE_THRESHOLD) continue;
    pairs.push({ a: a.nodeId, b: b.nodeId, score });
    degree.set(a.nodeId, (degree.get(a.nodeId) ?? 0) + 1);
    degree.set(b.nodeId, (degree.get(b.nodeId) ?? 0) + 1);
  }
  const kept = pairs.filter((p) => degree.get(p.a)! <= FAMILY_CAP && degree.get(p.b)! <= FAMILY_CAP);
  queries.replaceNearDuplicates(kept);
  return kept.length;
}
