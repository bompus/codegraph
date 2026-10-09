/**
 * OpenCode stores sessions in SQLite (`~/.local/share/opencode/opencode.db`),
 * not JSONL. `session.directory` is the project cwd. OpenCode 2 keeps its own
 * `session_v2` and `session_message` tables and copies each OpenCode 1 session
 * into them on first start, so a v2 session is read only when v1 lacks it.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createDatabase } from '../db/sqlite-adapter';
import { MIN_DOC_CHARS, type SessionDoc, type StoredSession, type UnavailableStores } from './claude-code';
import { cwdInRoots } from './project-roots';

type Db = ReturnType<typeof createDatabase>['db'];

export function opencodeDbPath(): string {
  if (process.env.CODEGRAPH_OPENCODE_DB) return process.env.CODEGRAPH_OPENCODE_DB;
  const data = process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
  return path.join(data, 'opencode', 'opencode.db');
}

function isoFromMs(ms: number): string {
  return new Date(ms).toISOString();
}

function openStore(dbPath: string): Db | null {
  try {
    const { db } = createDatabase(dbPath, { readOnly: true });
    db.pragma('busy_timeout = 5000');
    return db;
  } catch {
    return null;
  }
}

/** Rows of `sql`, or none when the table does not exist in this version. */
function rows<T>(db: Db, sql: string, ...params: string[]): T[] {
  try {
    return db.prepare(sql).all(...params) as T[];
  } catch {
    return [];
  }
}

type SessionRow = { id: string; directory: string | null; title: string | null; time_updated: number };

/**
 * Per version: where its sessions live, the change signature of one session's
 * rows, and its reader. The signature is the newest row update and the row
 * count, so a reply still streaming when first indexed is read again once it
 * completes. `time_updated` precedes `data` in both tables, so neither query
 * reads message bodies.
 */
const VERSIONS = [
  {
    table: 'session',
    signature: 'SELECT count(*) AS n, max(time_updated) AS t FROM part WHERE session_id = ?',
    load: v1Docs,
  },
  {
    table: 'session_v2',
    signature: 'SELECT count(*) AS n, max(time_updated) AS t FROM session_message WHERE session_id = ?',
    load: v2Docs,
  },
];

export function opencodeSessionsForProject(roots: readonly string[], unavailable?: UnavailableStores): StoredSession[] {
  const dbPath = opencodeDbPath();
  if (!fs.existsSync(dbPath)) return [];
  const db = openStore(dbPath);
  if (!db) {
    // Present but unreadable (permissions, lock): keep what the index holds.
    unavailable?.push(`opencode:${dbPath}:`);
    return [];
  }
  try {
    // A session present in both versions is read from the copy updated last:
    // OpenCode 2 keeps writing a migrated session to its own tables only.
    const chosen = new Map<string, { row: SessionRow; version: (typeof VERSIONS)[number] }>();
    for (const version of VERSIONS) {
      for (const row of rows<SessionRow>(db, `SELECT id, directory, title, time_updated FROM ${version.table}`)) {
        const prior = chosen.get(row.id);
        if (prior && (prior.row.time_updated || 0) >= (row.time_updated || 0)) continue;
        chosen.set(row.id, { row, version });
      }
    }
    const out: StoredSession[] = [];
    for (const { row, version } of chosen.values()) {
      const cwd = row.directory?.trim();
      if (!cwd || cwd === '/' || !cwdInRoots(cwd, roots)) continue;
      const sig = rows<{ n: number; t: number | null }>(db, version.signature, row.id)[0];
      out.push({
        path: `opencode:${dbPath}:${row.id}`,
        mtime: Math.max(row.time_updated || 0, sig?.t ?? 0),
        size: sig?.n ?? 0,
        session: `opencode:${row.id}`,
        title: row.title,
        docs: () => withStore(dbPath, (store) => version.load(store, row.id)),
      });
    }
    return out;
  } finally {
    db.close();
  }
}

/** `read` on a fresh read-only connection, or null when the store cannot be read. */
function withStore(dbPath: string, read: (db: Db) => SessionDoc[]): SessionDoc[] | null {
  const db = openStore(dbPath);
  if (!db) return null;
  try {
    return read(db);
  } catch {
    return null;
  } finally {
    db.close();
  }
}

function pushDoc(docs: SessionDoc[], ts: number, role: string, text: string): void {
  const trimmed = text.trim();
  if ((role === 'user' || role === 'assistant') && trimmed.length >= MIN_DOC_CHARS) {
    docs.push({ ts: isoFromMs(ts), role, text: trimmed });
  }
}

function v1Docs(db: Db, sessionId: string): SessionDoc[] {
  const messages = db
    .prepare('SELECT id, time_created, data FROM message WHERE session_id = ? ORDER BY time_created')
    .all(sessionId) as Array<{ id: string; time_created: number; data: string }>;
  const parts = db.prepare('SELECT data FROM part WHERE message_id = ?');
  const docs: SessionDoc[] = [];
  for (const msg of messages) {
    let role: string | undefined;
    try {
      role = (JSON.parse(msg.data) as { role?: string }).role;
    } catch {
      continue;
    }
    if (role !== 'user' && role !== 'assistant') continue;
    const texts: string[] = [];
    for (const part of parts.all(msg.id) as Array<{ data: string }>) {
      try {
        const body = JSON.parse(part.data) as { type?: string; text?: string };
        if (body.type === 'text' && typeof body.text === 'string') texts.push(body.text);
      } catch {
        // Skip a malformed part.
      }
    }
    pushDoc(docs, msg.time_created, role, texts.join('\n'));
  }
  return docs;
}

/** OpenCode 2: user rows carry `text`, assistant rows a `content` list of typed parts. */
function v2Docs(db: Db, sessionId: string): SessionDoc[] {
  const messages = db
    .prepare(
      "SELECT type, time_created, data FROM session_message WHERE session_id = ? AND type IN ('user', 'assistant') ORDER BY seq",
    )
    .all(sessionId) as Array<{ type: string; time_created: number; data: string }>;
  const docs: SessionDoc[] = [];
  for (const msg of messages) {
    let body: { text?: unknown; content?: unknown };
    try {
      body = JSON.parse(msg.data) as typeof body;
    } catch {
      continue;
    }
    const texts =
      msg.type === 'user'
        ? [body.text]
        : (Array.isArray(body.content) ? body.content : []).map((p) =>
            typeof p === 'object' && p !== null && (p as { type?: unknown }).type === 'text'
              ? (p as { text?: unknown }).text
              : undefined,
          );
    pushDoc(docs, msg.time_created, msg.type, texts.filter((t): t is string => typeof t === 'string').join('\n'));
  }
  return docs;
}
