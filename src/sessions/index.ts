/**
 * Session index: full-text search over the agent-session transcripts that
 * belong to a project — "what did the last session decide about X" as one
 * query instead of a grep over hundreds of megabytes of JSONL.
 *
 * An FTS5 table (porter stemming, BM25 rank) over the prose of every
 * transcript, stored in its own file, `.codegraph/sessions.db`, beside the
 * graph. Its own file on purpose: the graph's schema, migrations and bulk-load
 * FTS rebuild stay untouched, and the two indexes have different lifetimes (a
 * transcript changes while the code does not). Refresh happens on query and
 * re-reads only files whose mtime or size moved, so a call after one live
 * session costs tens of milliseconds; the first index of a few hundred
 * transcripts takes about a second.
 *
 * Readers live beside this file, one per agent host (Claude Code, Codex, Cursor).
 */
import * as fs from 'fs';
import * as path from 'path';
import { createDatabase, type SqliteDatabase, type SqliteStatement } from '../db/sqlite-adapter';
import { getCodeGraphDir } from '../directory';
import { loadSessionsEnabled } from '../project-config';
import {
  claudeSessionsDir,
  parseEntries,
  sessionIdOf,
  transcriptDocs,
  transcriptTitle,
  walkJsonl,
} from './claude-code';
import { parseCodexTranscript, codexFilesForProject } from './codex';
import { parseCursorTranscript, cursorFilesForProject } from './cursor';
import { parseAgyTranscript, agyFilesForProject } from './agy';
import { opencodeSessionsForProject } from './opencode';
import { projectWorktreeRoots } from './project-roots';

export const SESSIONS_DB_FILENAME = 'sessions.db';

/** How long a connection waits for another's write before giving up. */
export const BUSY_TIMEOUT_MS = 5000;

/** Bump when the readers' notion of prose changes, so existing indexes rebuild. */
const INDEX_VERSION = 4;

/**
 * `busy_timeout` does cover an ordinary lock wait on this pragma: a connection
 * that merely holds the database is waited out and the conversion then
 * succeeds. What it does not cover is several processes converting the SAME
 * brand-new database at the same moment — they collide inside the conversion
 * itself rather than queueing on a lock. That is only ever the first run: WAL
 * is persistent, so once the file is in WAL nobody converts it again.
 *
 * Both errors that collision raises are transient and clear on their own.
 * `database is locked` is the conversion losing the race; `disk I/O error` is
 * the shared-memory `-shm` file being created underneath a concurrent opener,
 * seen on Windows. Retrying either inside the existing budget is enough.
 * Tolerating a failed conversion is not an option: the connection does not
 * survive one, and the next statement on it fails too.
 *
 * Exported for the test that drives the retry directly — the collision itself
 * only reproduces probabilistically, so the retry is asserted here instead.
 */
export function enterWalMode(db: SqliteDatabase): void {
  const deadline = Date.now() + BUSY_TIMEOUT_MS;
  for (;;) {
    try {
      db.pragma('journal_mode = WAL');
      return;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      const transient = /database is locked|database is busy|disk i\/o error/i.test(message);
      if (!transient || Date.now() >= deadline) throw err;
      // Jittered, so the losers of one collision do not retry in lockstep.
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5 + Math.random() * 20);
    }
  }
}

export interface SessionsIndexStats {
  /** Transcript files seen. */
  files: number;
  /** Files re-read because their mtime or size moved. */
  refreshed: number;
  /** Docs written for the refreshed files. */
  docs: number;
}

export interface SessionHit {
  session: string;
  title: string | null;
  role: string;
  ts: string;
  /** The matching passage with `[match]` marks, about 24 tokens wide. */
  snippet: string;
  /** BM25 rank; lower is better, relative within one query only. */
  score: number;
}

export interface SessionSearchOptions {
  /** Max hits (default 10). */
  limit?: number;
  /** `user`, `assistant` or `summary`. */
  role?: string;
  /** ISO timestamp; hits before it are dropped. */
  sinceIso?: string;
  /** Session id prefix. */
  session?: string;
  /** OR the words instead of ANDing them. */
  any?: boolean;
}

/**
 * Every word quoted, ANDed or ORed, so a flag, a path or punctuation in the
 * query can never break FTS5's MATCH syntax. Porter stemming happens inside
 * FTS5, so "merging" reaches "merged".
 */
export function ftsQuery(raw: string, any = false): string {
  return (raw.match(/[\p{L}\p{N}_]+/gu) ?? []).map((w) => `"${w}"`).join(any ? ' OR ' : ' ');
}

interface FileRow {
  path: string;
  mtime: number;
  size: number;
}

type LoadedTranscript = {
  session: string;
  title: string | null;
  docs: ReturnType<typeof transcriptDocs>;
};

type TranscriptRecord = {
  path: string;
  mtime: number;
  size: number;
  load: () => LoadedTranscript;
};

export class SessionsIndex {
  private readonly fileRow: SqliteStatement;
  private readonly putFile: SqliteStatement;
  private readonly dropDocs: SqliteStatement;
  private readonly addDoc: SqliteStatement;

  private constructor(private readonly db: SqliteDatabase) {
    db.exec(`
      CREATE TABLE IF NOT EXISTS files (
        path TEXT PRIMARY KEY, session TEXT NOT NULL, title TEXT, mtime REAL NOT NULL, size INTEGER NOT NULL
      );
      CREATE VIRTUAL TABLE IF NOT EXISTS docs USING fts5(
        text, file UNINDEXED, role UNINDEXED, ts UNINDEXED, tokenize = 'porter unicode61'
      );
    `);
    // A reader change (what counts as prose) only reaches transcripts that
    // change afterwards; bumping INDEX_VERSION re-reads every file once.
    if (db.pragma('user_version', { simple: true }) !== INDEX_VERSION) {
      db.exec(`DELETE FROM docs; DELETE FROM files; PRAGMA user_version = ${INDEX_VERSION}`);
    }
    this.fileRow = db.prepare('SELECT mtime, size FROM files WHERE path = ?');
    this.putFile = db.prepare(
      'INSERT OR REPLACE INTO files (path, session, title, mtime, size) VALUES (?, ?, ?, ?, ?)',
    );
    this.dropDocs = db.prepare('DELETE FROM docs WHERE file = ?');
    this.addDoc = db.prepare('INSERT INTO docs (text, file, role, ts) VALUES (?, ?, ?, ?)');
  }

  /** Open (creating if needed) the index at `dbPath`; `:memory:` for tests. */
  static open(dbPath: string): SessionsIndex {
    if (dbPath !== ':memory:') fs.mkdirSync(path.dirname(dbPath), { recursive: true });
    const { db } = createDatabase(dbPath);
    // Parallel tool calls run on worker threads, one connection each, and all
    // of them see the same changed transcript. node:sqlite's busy timeout is
    // zero, so without this the losers fail with "database is locked" instead
    // of waiting the few hundred milliseconds the winner's write takes. Set
    // before the constructor's schema and version writes, which race the same way.
    db.pragma(`busy_timeout = ${BUSY_TIMEOUT_MS}`);
    if (dbPath !== ':memory:') enterWalMode(db);
    return new SessionsIndex(db);
  }

  private loadClaudeFile(file: string): {
    session: string;
    title: string | null;
    docs: ReturnType<typeof transcriptDocs>;
  } {
    const entries = parseEntries(file);
    return { session: sessionIdOf(file), title: transcriptTitle(entries), docs: transcriptDocs(entries) };
  }

  /**
   * Index a list of transcript files with a host-specific loader. `refresh(dir)`
   * stays Claude-shaped for tests; `querySessions` passes prefixed ids.
   */
  refreshListed(
    files: string[],
    load: (file: string) => LoadedTranscript,
  ): SessionsIndexStats {
    return this.refreshRecords(
      files.map((file) => {
        const st = fs.statSync(file);
        return { path: file, mtime: st.mtimeMs, size: st.size, load: () => load(file) };
      }),
    );
  }

  refreshRecords(records: TranscriptRecord[]): SessionsIndexStats {
    const known = new Map(
      (this.db.prepare('SELECT path, mtime, size FROM files').all() as FileRow[]).map((r) => [r.path, r]),
    );
    const stats: SessionsIndexStats = { files: records.length, refreshed: 0, docs: 0 };
    const present = new Set(records.map((r) => r.path));
    const unchanged = (row: Omit<FileRow, 'path'> | undefined, mtime: number, size: number): boolean =>
      row !== undefined && row.mtime === mtime && row.size === size;
    for (const rec of records) {
      if (unchanged(known.get(rec.path), rec.mtime, rec.size)) continue;
      const docs = this.replaceRecord(rec, unchanged);
      if (docs === null) continue;
      stats.docs += docs;
      stats.refreshed += 1;
    }
    const forget = this.db.transaction((gone: string[]) => {
      const dropFile = this.db.prepare('DELETE FROM files WHERE path = ?');
      for (const file of gone) {
        this.dropDocs.run(file);
        dropFile.run(file);
      }
    });
    const gone = [...known.keys()].filter((p) => !present.has(p));
    if (gone.length) forget(gone);
    return stats;
  }

  /**
   * Bring the index up to date with the transcripts under `dir`. Each changed
   * file is one transaction, so a crash mid-refresh leaves every other file
   * whole. Files that vanished from disk are forgotten.
   */
  refresh(dir: string): SessionsIndexStats {
    return this.refreshListed(walkJsonl(dir), (file) => this.loadClaudeFile(file));
  }

  /**
   * Re-index one file, or return null when another connection already did.
   * `BEGIN IMMEDIATE` takes the write lock first (waiting out `busy_timeout`),
   * then the file row is read again under it: a deferred transaction that read
   * first and wrote second would fail with SQLITE_BUSY_SNAPSHOT the moment the
   * other connection committed, and the busy handler never retries that.
   */
  private replaceRecord(
    rec: TranscriptRecord,
    unchanged: (row: Omit<FileRow, 'path'> | undefined, mtime: number, size: number) => boolean,
  ): number | null {
    this.db.exec('BEGIN IMMEDIATE');
    try {
      if (unchanged(this.fileRow.get(rec.path) as Omit<FileRow, 'path'> | undefined, rec.mtime, rec.size)) {
        this.db.exec('COMMIT');
        return null;
      }
      const loaded = rec.load();
      this.dropDocs.run(rec.path);
      for (const d of loaded.docs) {
        this.addDoc.run(d.text, rec.path, d.role, d.ts || new Date(rec.mtime).toISOString());
      }
      this.putFile.run(rec.path, loaded.session, loaded.title, rec.mtime, rec.size);
      this.db.exec('COMMIT');
      return loaded.docs.length;
    } catch (err) {
      this.db.exec('ROLLBACK');
      throw err;
    }
  }

  search(raw: string, opts: SessionSearchOptions = {}): SessionHit[] {
    const q = ftsQuery(raw, opts.any);
    if (!q) return [];
    const where = ['docs MATCH ?'];
    const params: Array<string | number> = [q];
    const filters: Array<[string, string | undefined]> = [
      ['docs.role = ?', opts.role],
      ['docs.ts >= ?', opts.sinceIso],
      ['files.session GLOB ?', opts.session ? `${opts.session}*` : undefined],
    ];
    for (const [clause, value] of filters) {
      if (value) {
        where.push(clause);
        params.push(value);
      }
    }
    params.push(Math.max(1, Math.min(opts.limit ?? 10, 100)));
    return this.db
      .prepare(
        `SELECT files.session, files.title, docs.role, docs.ts,
                snippet(docs, 0, '[', ']', '…', 24) AS snippet, bm25(docs) AS score
         FROM docs JOIN files ON files.path = docs.file
         WHERE ${where.join(' AND ')}
         ORDER BY score LIMIT ?`,
      )
      .all(...params) as SessionHit[];
  }

  close(): void {
    this.db.close();
  }
}

/** Where a project's session index lives. */
export function sessionsDbPath(projectRoot: string): string {
  return path.join(getCodeGraphDir(projectRoot), SESSIONS_DB_FILENAME);
}

/**
 * The transcript directory to index for a project: `CODEGRAPH_SESSIONS_DIR`
 * when set (tests, unusual layouts), else Claude Code's store for that
 * project. Null when there is nothing to index, or `codegraph.json` says
 * `"sessions": false`. Codex and Cursor files are gathered separately in
 * `querySessions` when this override is unset.
 */
export function sessionsSourceDir(projectRoot: string): string | null {
  if (!loadSessionsEnabled(projectRoot)) return null;
  const override = process.env.CODEGRAPH_SESSIONS_DIR;
  if (override) return fs.existsSync(override) ? override : null;
  return claudeSessionsDir(projectRoot);
}

export function claudeFilesForProject(projectRoot: string): string[] {
  const files: string[] = [];
  for (const root of projectWorktreeRoots(projectRoot)) {
    const dir = claudeSessionsDir(root);
    if (dir) files.push(...walkJsonl(dir));
  }
  return files;
}

type HostedTranscript = { file: string; host: 'claude' | 'codex' | 'cursor' | 'agy' };

function hostedTranscripts(projectRoot: string): HostedTranscript[] {
  const out: HostedTranscript[] = [];
  for (const file of claudeFilesForProject(projectRoot)) out.push({ file, host: 'claude' });
  for (const file of codexFilesForProject(projectRoot)) out.push({ file, host: 'codex' });
  for (const file of cursorFilesForProject(projectRoot)) out.push({ file, host: 'cursor' });
  for (const file of agyFilesForProject(projectRoot)) out.push({ file, host: 'agy' });
  return out;
}

function loadHosted(file: string, host: HostedTranscript['host']): LoadedTranscript {
  if (host === 'codex') return parseCodexTranscript(file);
  if (host === 'cursor') return parseCursorTranscript(file);
  if (host === 'agy') return parseAgyTranscript(file);
  const entries = parseEntries(file);
  return {
    session: `claude:${sessionIdOf(file)}`,
    title: transcriptTitle(entries),
    docs: transcriptDocs(entries),
  };
}

function collectRecords(projectRoot: string): TranscriptRecord[] {
  const records: TranscriptRecord[] = hostedTranscripts(projectRoot).map((h) => {
    const st = fs.statSync(h.file);
    return { path: h.file, mtime: st.mtimeMs, size: st.size, load: () => loadHosted(h.file, h.host) };
  });
  for (const session of opencodeSessionsForProject(projectRoot)) {
    records.push({
      path: session.path,
      mtime: session.mtime,
      size: session.size,
      load: () => ({ session: session.session, title: session.title, docs: session.docs }),
    });
  }
  return records;
}

export interface SessionsQueryResult {
  index: SessionsIndexStats;
  hits: SessionHit[];
}

/**
 * The one entry point the CLI and the MCP tool share: refresh, then search.
 * Throws when the project has no transcripts to index (or has opted out) —
 * the caller renders that as guidance, not a failure.
 */
export function querySessions(
  projectRoot: string,
  query: string,
  opts: SessionSearchOptions = {},
): SessionsQueryResult {
  if (!loadSessionsEnabled(projectRoot)) throw new NoSessionsError(projectRoot);
  const override = process.env.CODEGRAPH_SESSIONS_DIR;
  const index = SessionsIndex.open(sessionsDbPath(projectRoot));
  try {
    if (override) {
      if (!fs.existsSync(override)) throw new NoSessionsError(projectRoot);
      const stats = index.refreshListed(walkJsonl(override), (file) => loadHosted(file, 'claude'));
      return { index: stats, hits: index.search(query, opts) };
    }
    const records = collectRecords(projectRoot);
    if (records.length === 0) throw new NoSessionsError(projectRoot);
    const stats = index.refreshRecords(records);
    return { index: stats, hits: index.search(query, opts) };
  } finally {
    index.close();
  }
}

export class NoSessionsError extends Error {
  constructor(projectRoot: string) {
    super(
      `No agent-session transcripts to index for ${projectRoot}: no Claude Code, Codex, Cursor, OpenCode, or AGY ` +
        'transcripts belong to this project, CODEGRAPH_SESSIONS_DIR points nowhere, ' +
        'or codegraph.json sets "sessions": false.',
    );
    this.name = 'NoSessionsError';
  }
}

/** The text both the CLI and the MCP tool print for a set of hits. */
export function formatSessionHits(query: string, result: SessionsQueryResult): string {
  const { hits, index } = result;
  const head = `Sessions matching "${query}" — ${hits.length} hit${hits.length === 1 ? '' : 's'} across ${index.files} transcript${index.files === 1 ? '' : 's'}`;
  if (hits.length === 0) {
    return `${head}.\nNo transcript prose matches every word. Fewer words, a stem ("merge" also finds "merged", "merging"), or any=true (OR the words) widen the search.`;
  }
  const lines = [head + ':', ''];
  for (const h of hits) {
    const title = h.title ? ` · ${h.title}` : '';
    lines.push(`## ${h.session}${title}`);
    lines.push(`${h.role} · ${h.ts}`);
    lines.push(h.snippet.replace(/\s+/g, ' ').trim());
    lines.push('');
  }
  lines.push('A hit names its session id (`claude:`, `codex:`, `cursor:`, `opencode:`, or `agy:`); the transcript itself is the next step when the snippet is not enough.');
  return lines.join('\n');
}
