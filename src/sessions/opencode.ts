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
import { MIN_DOC_CHARS, type SessionDoc, type StoredSession } from './claude-code';
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

export function opencodeSessionsForProject(roots: readonly string[]): StoredSession[] {
  const dbPath = opencodeDbPath();
  if (!fs.existsSync(dbPath)) return [];
  const db = openStore(dbPath);
  if (!db) return [];
  try {
    const out: StoredSession[] = [];
    const v1 = new Set<string>();
    const tables: Array<{ table: string; count: string; load: typeof v1Docs }> = [
      { table: 'session', count: 'SELECT count(*) AS n FROM message WHERE session_id = ?', load: v1Docs },
      { table: 'session_v2', count: 'SELECT count(*) AS n FROM session_message WHERE session_id = ?', load: v2Docs },
    ];
    for (const { table, count, load } of tables) {
      for (const row of rows<SessionRow>(db, `SELECT id, directory, title, time_updated FROM ${table}`)) {
        if (table === 'session') v1.add(row.id);
        else if (v1.has(row.id)) continue;
        const cwd = row.directory?.trim();
        if (!cwd || cwd === '/' || !cwdInRoots(cwd, roots)) continue;
        const n = rows<{ n: number }>(db, count, row.id)[0]?.n ?? 0;
        out.push({
          path: `opencode:${dbPath}:${row.id}`,
          mtime: row.time_updated || 0,
          size: n,
          session: `opencode:${row.id}`,
          title: row.title,
          docs: () => withStore(dbPath, (store) => load(store, row.id)),
        });
      }
    }
    return out;
  } finally {
    db.close();
  }
}

function withStore(dbPath: string, read: (db: Db) => SessionDoc[]): SessionDoc[] {
  const db = openStore(dbPath);
  if (!db) return [];
  try {
    return read(db);
  } catch {
    return [];
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
  const messages = rows<{ id: string; time_created: number; data: string }>(
    db,
    'SELECT id, time_created, data FROM message WHERE session_id = ? ORDER BY time_created',
    sessionId,
  );
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
  const messages = rows<{ type: string; time_created: number; data: string }>(
    db,
    "SELECT type, time_created, data FROM session_message WHERE session_id = ? AND type IN ('user', 'assistant') ORDER BY seq",
    sessionId,
  );
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
