/**
 * Session index: full-text search over the agent-session transcripts that
 * belong to a project — "what did the last session decide about X" as one
 * query instead of a grep over hundreds of megabytes of JSONL.
 *
 * An FTS5 table (porter stemming, BM25 rank) over the prose of every
 * transcript, stored in its own file, `.codegraph/sessions-v2.db`, beside the
 * graph. Its own file on purpose: the graph's schema, migrations and bulk-load
 * FTS rebuild stay untouched, and the two indexes have different lifetimes (a
 * transcript changes while the code does not). Refresh happens on query and
 * re-reads only files whose mtime or size moved. An ordinary indexed table
 * maps each source to its FTS rows, so replacement does not scan other prose.
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
  type UnavailableStores,
} from './claude-code';
import { parseCodexTranscript, codexFilesForProject, normalizeRemote, type CodexMeta } from './codex';
import { parseCursorTranscript, cursorFilesForProject } from './cursor';
import { parseAgyTranscript, agyFilesForProject } from './agy';
import { parseGrokTranscript, grokFilesForProject, grokSummaryFile } from './grok';
import { gitCommitDocs, gitHead } from './git-log';
import { opencodeSessionsForProject } from './opencode';
import { devinSessionsForProject } from './devin';
import { indexableDocs } from './noise';
import { projectWorktreeRoots } from './project-roots';

export const SESSIONS_DB_FILENAME = 'sessions-v2.db';
const LEGACY_SESSIONS_DB_FILENAME = 'sessions.db';

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
  private readonly dropSources: SqliteStatement;
  private readonly addSource: SqliteStatement;

  private constructor(private readonly db: SqliteDatabase, legacyPath?: string) {
    // An older executable can still write sessions.db. Keep its FTS row IDs
    // separate from the mapping maintained by this storage generation.
    const schemaReady = (): boolean => {
      const rows = db.prepare(`SELECT name FROM sqlite_master
        WHERE (type = 'table' AND name IN ('files', 'roots', 'docs', 'doc_sources', 'codex_meta'))
           OR (type = 'index' AND name = 'doc_sources_file')`).all() as Array<{ name: string }>;
      return rows.length === 6 && db.pragma('user_version', { simple: true }) === INDEX_VERSION;
    };
    if (!schemaReady()) {
      db.exec('BEGIN IMMEDIATE');
      try {
        if (!schemaReady()) {
          const hadRoots = db.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'roots'").get();
          db.exec(`
            CREATE TABLE IF NOT EXISTS files (
              path TEXT PRIMARY KEY, session TEXT NOT NULL, title TEXT, mtime REAL NOT NULL, size INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS roots (path TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS codex_meta (
              path TEXT PRIMARY KEY, ino INTEGER NOT NULL, cwd TEXT, remote TEXT
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS docs USING fts5(
              text, file UNINDEXED, role UNINDEXED, ts UNINDEXED, tokenize = 'porter unicode61'
            );
          `);
          const mapped = db.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'doc_sources'").get();
          if (!mapped) {
            db.exec(`CREATE TABLE doc_sources (doc_rowid INTEGER PRIMARY KEY, file TEXT NOT NULL);
              INSERT INTO doc_sources SELECT rowid, file FROM docs;`);
          }
          db.exec('CREATE INDEX IF NOT EXISTS doc_sources_file ON doc_sources(file)');
          // Roots come over once, when this store is created; roots an older
          // executable records in sessions.db afterwards stay there.
          if (!hadRoots && legacyPath && fs.existsSync(legacyPath)) {
            const legacy = createDatabase(legacyPath, { readOnly: true }).db;
            try {
              legacy.pragma(`busy_timeout = ${BUSY_TIMEOUT_MS}`);
              // Reader versions 1–4 predate remembered roots. A newer store
              // missing that table is damaged, so its import must still fail.
              const version = legacy.pragma('user_version', { simple: true }) as number;
              const tables = legacy.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('files', 'docs', 'roots')")
                .all() as Array<{ name: string }>;
              const preRoots = version >= 1 && version <= 4 && tables.length === 2
                && tables.some((t) => t.name === 'files') && tables.some((t) => t.name === 'docs');
              const roots = preRoots ? [] : legacy.prepare('SELECT path FROM roots').all() as Array<{ path: string }>;
              const put = db.prepare('INSERT OR IGNORE INTO roots (path) VALUES (?)');
              for (const root of roots) put.run(root.path);
            } finally {
              legacy.close();
            }
          }
          // Reader changes rebuild prose, while remembered project roots stay.
          if (db.pragma('user_version', { simple: true }) !== INDEX_VERSION) {
            db.exec(`DELETE FROM docs; DELETE FROM doc_sources; DELETE FROM files;
              PRAGMA user_version = ${INDEX_VERSION}`);
          }
        }
        db.exec('COMMIT');
      } catch (err) {
        db.exec('ROLLBACK');
        throw err;
      }
    }
    this.fileRow = db.prepare('SELECT mtime, size FROM files WHERE path = ?');
    this.putFile = db.prepare(
      'INSERT OR REPLACE INTO files (path, session, title, mtime, size) VALUES (?, ?, ?, ?, ?)',
    );
    this.dropDocs = db.prepare('DELETE FROM docs WHERE rowid IN (SELECT doc_rowid FROM doc_sources WHERE file = ?)');
    this.dropSources = db.prepare('DELETE FROM doc_sources WHERE file = ?');
    this.addSource = db.prepare('INSERT INTO doc_sources (doc_rowid, file) VALUES (?, ?)');
    this.addDoc = db.prepare('INSERT INTO docs (text, file, role, ts) VALUES (?, ?, ?, ?)');
  }

  /** Open (creating if needed) the index at `dbPath`; `:memory:` for tests. */
  static open(dbPath: string): SessionsIndex {
    if (path.basename(dbPath) === LEGACY_SESSIONS_DB_FILENAME) {
      throw new Error(`The legacy sessions.db belongs to older writers; open ${SESSIONS_DB_FILENAME} instead.`);
    }
    if (dbPath !== ':memory:') fs.mkdirSync(path.dirname(dbPath), { recursive: true });
    const { db } = createDatabase(dbPath);
    // Parallel tool calls run on worker threads, one connection each, and all
    // of them see the same changed transcript. node:sqlite's busy timeout is
    // zero, so without this the losers fail with "database is locked" instead
    // of waiting the few hundred milliseconds the winner's write takes. Set
    // before the constructor's schema and version writes, which race the same way.
    try {
      db.pragma(`busy_timeout = ${BUSY_TIMEOUT_MS}`);
      if (dbPath !== ':memory:') enterWalMode(db);
      const legacyPath = path.basename(dbPath) === SESSIONS_DB_FILENAME
        ? path.join(path.dirname(dbPath), LEGACY_SESSIONS_DB_FILENAME) : undefined;
      return new SessionsIndex(db, legacyPath);
    } catch (err) {
      db.close();
      throw err;
    }
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

  /**
   * Index `records` and forget indexed paths no longer listed, except those
   * under an `unavailable` prefix: a store that exists but could not be read
   * on this query keeps what the index already holds.
   */
  refreshRecords(records: TranscriptRecord[], unavailable: readonly string[] = []): SessionsIndexStats {
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
        this.dropSources.run(file);
        dropFile.run(file);
      }
    });
    const gone = [...known.keys()].filter(
      (p) => !present.has(p) && !unavailable.some((prefix) => p.startsWith(prefix)),
    );
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
      this.dropSources.run(rec.path);
      for (const d of docs) {
        const inserted = this.addDoc.run(d.text, rec.path, d.role, d.ts || new Date(rec.mtime).toISOString());
        this.addSource.run(inserted.lastInsertRowid, rec.path);
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
   * Codex rollouts that belong to the project, reading each rollout's first
   * line only the first time it is seen: the remembered answers live in
   * `codex_meta`, and rows for rollouts no longer listed are dropped.
   */
  codexFiles(roots: readonly string[], remotes: readonly string[], unavailable: UnavailableStores): string[] {
    const known = new Map(
      (this.db.prepare('SELECT path, ino, cwd, remote FROM codex_meta').all() as Array<CodexMeta & { path: string; ino: number }>)
        .map((r) => [r.path, r]),
    );
    const seen = new Set<string>();
    const added: Array<[string, number, CodexMeta]> = [];
    const files = codexFilesForProject(roots, remotes, unavailable, {
      get: (file, ino) => {
        seen.add(file);
        const row = known.get(file);
        return row && row.ino === ino ? { cwd: row.cwd, remote: row.remote } : undefined;
      },
      set: (file, ino, meta) => added.push([file, ino, meta]),
    });
    const gone = [...known.keys()].filter((p) => !seen.has(p) && !unavailable.some((prefix) => p.startsWith(prefix)));
    if (added.length || gone.length) {
      const put = this.db.prepare('INSERT OR REPLACE INTO codex_meta (path, ino, cwd, remote) VALUES (?, ?, ?, ?)');
      const drop = this.db.prepare('DELETE FROM codex_meta WHERE path = ?');
      this.db.transaction(() => {
        for (const [file, ino, meta] of added) put.run(file, ino, meta.cwd, meta.remote);
        for (const file of gone) drop.run(file);
      })();
    }
    return files;
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

export function claudeFilesForProject(roots: readonly string[], unavailable?: UnavailableStores): string[] {
  const files: string[] = [];
  for (const root of roots) {
    const dir = claudeSessionsDir(root);
    if (dir) files.push(...walkJsonl(dir, unavailable));
  }
  return files;
}

type HostedTranscript = { file: string; host: 'claude' | 'codex' | 'cursor' | 'agy' | 'grok' };

function hostedTranscripts(
  index: SessionsIndex,
  roots: readonly string[],
  remotes: readonly string[],
  unavailable: UnavailableStores,
): HostedTranscript[] {
  const out: HostedTranscript[] = [];
  for (const file of claudeFilesForProject(roots, unavailable)) out.push({ file, host: 'claude' });
  for (const file of index.codexFiles(roots, remotes, unavailable)) out.push({ file, host: 'codex' });
  for (const file of cursorFilesForProject(roots, unavailable)) out.push({ file, host: 'cursor' });
  for (const file of agyFilesForProject(roots, unavailable)) out.push({ file, host: 'agy' });
  for (const file of grokFilesForProject(roots, unavailable)) out.push({ file, host: 'grok' });
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

function mtimeOf(file: string): number {
  try {
    return fs.statSync(file).mtimeMs;
  } catch {
    return 0;
  }
}

/** The project's transcripts; stores that exist but cannot be read now go to `unavailable`. */
function collectRecords(
  index: SessionsIndex,
  roots: readonly string[],
  remotes: readonly string[],
  unavailable: UnavailableStores,
  projectRoot?: string,
): TranscriptRecord[] {
  const records: TranscriptRecord[] = [];
  for (const h of hostedTranscripts(index, roots, remotes, unavailable)) {
    let st: fs.Stats;
    try {
      st = fs.statSync(h.file);
    } catch {
      continue; // Removed between listing and stat.
    }
    // A Grok title lives in its own file; renaming a session touches only that one.
    const mtime = h.host === 'grok' ? Math.max(st.mtimeMs, mtimeOf(grokSummaryFile(h.file))) : st.mtimeMs;
    records.push({ path: h.file, mtime, size: st.size, load: () => loadHosted(h.file, h.host) });
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
  for (const session of [...opencodeSessionsForProject(roots, unavailable), ...devinSessionsForProject(roots, unavailable)]) {
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
    const unavailable: UnavailableStores = [];
    const records = collectRecords(index, roots, projectRemotes(projectRoot), unavailable, projectRoot);
    // Commit messages alone are not session history: without a transcript the
    // guidance (which hosts, how to opt out) says more than "0 hits". A store
    // that exists but cannot be read now still answers from what is indexed.
    if (records.every((r) => r.path.startsWith('git:')) && unavailable.length === 0) {
      throw new NoSessionsError(projectRoot);
    }
    const stats = index.refreshRecords(records, unavailable);
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
