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
 * Readers live beside this file, one per agent host (Claude Code, Codex, Cursor,
 * OpenCode, AGY, Devin, Grok), plus `git-log.ts` for commit messages.
 */
import { execFileSync } from 'child_process';
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
import { parseCodexTranscript, codexFilesForProject, normalizeRemote } from './codex';
import { parseCursorTranscript, cursorFilesForProject } from './cursor';
import { parseAgyTranscript, agyFilesForProject } from './agy';
import { parseGrokTranscript, grokFilesForProject } from './grok';
import { gitCommitDocs, gitHead } from './git-log';
import { opencodeSessionsForProject } from './opencode';
import { devinSessionsForProject } from './devin';
import { indexableDocs } from './noise';
import { projectWorktreeRoots } from './project-roots';

export const SESSIONS_DB_FILENAME = 'sessions.db';

/** How long a connection waits for another's write before giving up. */
export const BUSY_TIMEOUT_MS = 5000;

/** Bump when the readers' notion of prose changes, so existing indexes rebuild. */
const INDEX_VERSION = 5;

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
  /** The transcript the hit came from: a file path, or `host:<db>:<id>` for SQLite stores. */
  file: string;
  /** BM25 rank; lower is better, relative within one query only. */
  score: number;
  /** Holds only some of the query words: the every-word query ran short. */
  partial?: boolean;
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
 * Words too common to narrow a search. A question phrased in full ("why did we
 * keep the timer") would otherwise demand "why", "did" and "we" appear in the
 * same passage, and match nothing.
 */
const STOPWORDS = new Set(
  `a about after an and any are as at be been but by can could did do does don for from get got had has
  have how i if in into is it its just keep of on or our s should so still t than that the their them then
  there these they this to us use used using was we were what whats when where which while who why will
  with would you`.split(/\s+/),
);

/** The query's words minus stopwords; all of them when every word is a stopword. */
export function queryWords(raw: string): string[] {
  const words = raw.match(/[\p{L}\p{N}_]+/gu) ?? [];
  const kept = words.filter((w) => !STOPWORDS.has(w.toLowerCase()));
  return kept.length ? kept : words;
}

/**
 * Every word quoted, ANDed or ORed, so a flag, a path or punctuation in the
 * query can never break FTS5's MATCH syntax. Porter stemming happens inside
 * FTS5, so "merging" reaches "merged".
 */
export function ftsQuery(raw: string, any = false): string {
  return queryWords(raw)
    .map((w) => `"${w}"`)
    .join(any ? ' OR ' : ' ');
}

/** A crude stem, enough to count "merging" and "merged" as one query word. */
function stem(word: string): string {
  const w = word.toLowerCase();
  const m = /^(.{3,}?)(?:ing|ed|es|s)$/.exec(w);
  return m ? m[1]! : w;
}

/** How many distinct query words `text` holds, by stem prefix. */
function coverage(stems: readonly string[], text: string): number {
  const tokens = text.toLowerCase().match(/[\p{L}\p{N}_]+/gu) ?? [];
  return stems.filter((s) => tokens.some((t) => t.startsWith(s))).length;
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
  /** Null when the source could not be read now; the record is retried next query. */
  load: () => LoadedTranscript | null;
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
      CREATE TABLE IF NOT EXISTS roots (path TEXT PRIMARY KEY);
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
      if (loaded === null) {
        this.db.exec('COMMIT');
        return null;
      }
      const docs = indexableDocs(loaded.docs);
      this.dropDocs.run(rec.path);
      for (const d of docs) {
        this.addDoc.run(d.text, rec.path, d.role, d.ts || new Date(rec.mtime).toISOString());
      }
      this.putFile.run(rec.path, loaded.session, loaded.title, rec.mtime, rec.size);
      this.db.exec('COMMIT');
      return docs.length;
    } catch (err) {
      this.db.exec('ROLLBACK');
      throw err;
    }
  }

  /**
   * Remember `roots` and return them with every root remembered before. A
   * removed worktree drops out of `git worktree list`, but its transcripts
   * still belong to the project; the remembered root keeps them matched.
   */
  rememberRoots(roots: readonly string[]): string[] {
    const put = this.db.prepare('INSERT OR IGNORE INTO roots (path) VALUES (?)');
    this.db.transaction(() => {
      for (const r of roots) put.run(r);
    })();
    const known = (this.db.prepare('SELECT path FROM roots').all() as Array<{ path: string }>).map((r) => r.path);
    return [...new Set([...roots, ...known])];
  }

  /**
   * Best hits first: passages holding more of the distinct query words, then
   * BM25. The every-word query runs first. When it fills fewer than `limit`
   * slots (a long question rarely has every word in one passage), hits from
   * the any-word query follow, each marked `partial`; `fallback` says every
   * hit is partial. Identical passages (a report quoted in two sessions, a
   * repeated "continue") collapse to their best-ranked copy.
   */
  search(raw: string, opts: SessionSearchOptions = {}): SessionHit[] & { fallback?: boolean } {
    const limit = Math.max(1, Math.min(opts.limit ?? 10, 100));
    const stems = [...new Set(queryWords(raw).map(stem))];
    const where: string[] = [];
    const params: string[] = [];
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
    const select = this.db.prepare(
      `SELECT docs.rowid AS id, files.session, files.title, docs.role, docs.ts, docs.file, docs.text,
              snippet(docs, 0, '[', ']', '…', 24) AS snippet, bm25(docs) AS score
       FROM docs JOIN files ON files.path = docs.file
       WHERE ${['docs MATCH ?', ...where].join(' AND ')}
       ORDER BY score LIMIT ?`,
    );
    const hits: SessionHit[] & { fallback?: boolean } = [];
    const seen = new Set<string>();
    for (const any of opts.any ? [true] : [false, true]) {
      const q = ftsQuery(raw, any);
      if (!q || hits.length === limit) break;
      const rows = select.all(q, ...params, Math.max(limit * 20, 100)) as Array<
        SessionHit & { id: number; text: string }
      >;
      const cover = new Map(rows.map((r) => [r, coverage(stems, r.text)]));
      rows.sort((a, b) => cover.get(b)! - cover.get(a)! || a.score - b.score);
      for (const { id, text, ...hit } of rows) {
        // The row id catches the every-word hits again; the text key, repeats.
        const key = text.toLowerCase().replace(/\s+/g, ' ').slice(0, 500);
        if (seen.has(key) || seen.has(`#${id}`)) continue;
        seen.add(key);
        seen.add(`#${id}`);
        hits.push(any && !opts.any ? { ...hit, partial: true } : hit);
        if (hits.length === limit) break;
      }
    }
    if (hits.length > 0 && hits.every((h) => h.partial)) hits.fallback = true;
    return hits;
  }

  close(): void {
    this.db.close();
  }
}

/** Where a project's session index lives. */
export function sessionsDbPath(projectRoot: string): string {
  return path.join(getCodeGraphDir(projectRoot), SESSIONS_DB_FILENAME);
}

export function claudeFilesForProject(roots: readonly string[]): string[] {
  const files: string[] = [];
  for (const root of roots) {
    const dir = claudeSessionsDir(root);
    if (dir) files.push(...walkJsonl(dir));
  }
  return files;
}

type HostedTranscript = { file: string; host: 'claude' | 'codex' | 'cursor' | 'agy' | 'grok' };

function hostedTranscripts(roots: readonly string[], remotes: readonly string[]): HostedTranscript[] {
  const out: HostedTranscript[] = [];
  for (const file of claudeFilesForProject(roots)) out.push({ file, host: 'claude' });
  for (const file of codexFilesForProject(roots, remotes)) out.push({ file, host: 'codex' });
  for (const file of cursorFilesForProject(roots)) out.push({ file, host: 'cursor' });
  for (const file of agyFilesForProject(roots)) out.push({ file, host: 'agy' });
  for (const file of grokFilesForProject(roots)) out.push({ file, host: 'grok' });
  return out;
}

/**
 * The project's git remote URLs, normalized. Codex records the repository URL
 * of each session, which still matches after the session's worktree is gone.
 */
export function projectRemotes(projectRoot: string): string[] {
  try {
    const out = execFileSync('git', ['config', '--get-regexp', '^remote\\..*\\.url$'], {
      cwd: projectRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
    });
    return [...new Set(out.split('\n').map((l) => l.split(' ')[1]).filter(Boolean).map((u) => normalizeRemote(u!)))];
  } catch {
    return [];
  }
}

function loadHosted(file: string, host: HostedTranscript['host']): LoadedTranscript {
  if (host === 'codex') return parseCodexTranscript(file);
  if (host === 'cursor') return parseCursorTranscript(file);
  if (host === 'agy') return parseAgyTranscript(file);
  if (host === 'grok') return parseGrokTranscript(file);
  const entries = parseEntries(file);
  return {
    session: `claude:${sessionIdOf(file)}`,
    title: transcriptTitle(entries),
    docs: transcriptDocs(entries),
  };
}

function collectRecords(roots: readonly string[], remotes: readonly string[], projectRoot?: string): TranscriptRecord[] {
  const records: TranscriptRecord[] = [];
  for (const h of hostedTranscripts(roots, remotes)) {
    let st: fs.Stats;
    try {
      st = fs.statSync(h.file);
    } catch {
      continue; // Removed between listing and stat.
    }
    records.push({ path: h.file, mtime: st.mtimeMs, size: st.size, load: () => loadHosted(h.file, h.host) });
  }
  const head = projectRoot ? gitHead(projectRoot) : null;
  if (head && projectRoot) {
    records.push({
      path: `git:${projectRoot}`,
      mtime: head.mtime,
      // The hash's leading bits: a rebase or reset to an older commit changes
      // HEAD without moving its time forward.
      size: parseInt(head.sha.slice(0, 8), 16),
      load: () => ({ session: `git:${path.basename(projectRoot)}`, title: 'commit messages', docs: gitCommitDocs(projectRoot) }),
    });
  }
  for (const session of [...opencodeSessionsForProject(roots), ...devinSessionsForProject(roots)]) {
    records.push({
      path: session.path,
      mtime: session.mtime,
      size: session.size,
      load: () => {
        const docs = session.docs();
        return docs && { session: session.session, title: session.title, docs };
      },
    });
  }
  return records;
}

export interface SessionsQueryResult {
  index: SessionsIndexStats;
  hits: SessionHit[];
  /** True when no passage held every word and the hits match any of them. */
  fallback?: boolean;
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
      return result(stats, index.search(query, opts));
    }
    const roots = index.rememberRoots(projectWorktreeRoots(projectRoot));
    const records = collectRecords(roots, projectRemotes(projectRoot), projectRoot);
    // Commit messages alone are not session history: without a transcript the
    // guidance (which hosts, how to opt out) says more than "0 hits".
    if (records.every((r) => r.path.startsWith('git:'))) throw new NoSessionsError(projectRoot);
    const stats = index.refreshRecords(records);
    return result(stats, index.search(query, opts));
  } finally {
    index.close();
  }
}

function result(index: SessionsIndexStats, found: ReturnType<SessionsIndex['search']>): SessionsQueryResult {
  const hits = [...found];
  return found.fallback ? { index, hits, fallback: true } : { index, hits };
}

/**
 * An identifier worth looking up in session prose: camelCase, PascalCase with
 * an inner capital, or snake_case, at least 6 characters. A plain word like
 * `search` or `open` appears in hundreds of sessions that never meant the symbol.
 */
export function isDistinctiveName(name: string): boolean {
  return name.length >= 6 && (/^[A-Za-z_$][\w$]*[a-z][A-Z]/.test(name) || /[A-Za-z]_[A-Za-z]/.test(name));
}

export interface SessionMention {
  session: string;
  title: string | null;
  /** Latest time the session mentioned the name. */
  ts: string;
}

/**
 * Transcripts that mention each of `names`, most recent first, from the
 * session index as it stands: no refresh, so a code query never waits on
 * transcript I/O. Empty when the project opted out or never indexed sessions.
 * Commit messages are left out; the graph already knows where code came from.
 */
export function sessionsMentioning(
  projectRoot: string,
  names: readonly string[],
  perName = 3,
): Map<string, { total: number; recent: SessionMention[] }> {
  const out = new Map<string, { total: number; recent: SessionMention[] }>();
  const wanted = [...new Set(names.filter(isDistinctiveName))];
  const dbPath = sessionsDbPath(projectRoot);
  if (wanted.length === 0 || !fs.existsSync(dbPath) || !loadSessionsEnabled(projectRoot)) return out;
  let db: SqliteDatabase;
  try {
    db = createDatabase(dbPath, { readOnly: true }).db;
    db.pragma(`busy_timeout = ${BUSY_TIMEOUT_MS}`);
  } catch {
    return out;
  }
  try {
    const query = db.prepare(
      `SELECT files.session, files.title, max(docs.ts) AS ts
       FROM docs JOIN files ON files.path = docs.file
       WHERE docs MATCH ? AND docs.role != 'commit' AND instr(lower(docs.text), lower(?)) > 0
       GROUP BY files.session ORDER BY ts DESC`,
    );
    for (const name of wanted) {
      // FTS narrows (a quoted name is one phrase, so `build_index` reaches
      // "build index"); instr keeps only text that spells the name itself.
      const rows = query.all(`"${name.replace(/"/g, '')}"`, name) as SessionMention[];
      if (rows.length) out.set(name, { total: rows.length, recent: rows.slice(0, perName) });
    }
  } catch {
    // An index from an older version or mid-rebuild: no mentions rather than an error.
  } finally {
    db.close();
  }
  return out;
}

export class NoSessionsError extends Error {
  constructor(projectRoot: string) {
    super(
      `No agent-session transcripts to index for ${projectRoot}: no Claude Code, Codex, Cursor, OpenCode, AGY, Devin, or Grok ` +
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
    return `${head}.\nNo transcript prose holds any of these words. A stem ("merge" also finds "merged", "merging") or a different term may find it.`;
  }
  const lines = [head + ':', ''];
  if (result.fallback) {
    lines.push('No passage holds every word; these hits match some of them, most words first.', '');
  } else if (hits.some((h) => h.partial)) {
    lines.push('Hits marked "some words" hold only part of the query; they follow the passages that hold every word.', '');
  }
  for (const h of hits) {
    const title = h.title ? ` · ${h.title}` : '';
    lines.push(`## ${h.session}${title}`);
    lines.push(`${h.role} · ${h.ts} · ${h.file}${h.partial && !result.fallback ? ' · some words' : ''}`);
    lines.push(h.snippet.replace(/\s+/g, ' ').trim());
    lines.push('');
  }
  lines.push('A hit names its session id (`claude:`, `codex:`, `cursor:`, `opencode:`, `agy:`, `devin:`, `grok:`, or `git:` for commit messages); the path after the timestamp is the transcript to read when the snippet is not enough.');
  return lines.join('\n');
}
