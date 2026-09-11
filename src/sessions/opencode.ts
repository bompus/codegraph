/**
 * OpenCode stores sessions in SQLite (`~/.local/share/opencode/opencode.db`),
 * not JSONL. `session.directory` is the project cwd.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createDatabase } from '../db/sqlite-adapter';
import { MIN_DOC_CHARS, type SessionDoc } from './claude-code';
import { cwdBelongsToProject } from './project-roots';

export function opencodeDbPath(): string {
  if (process.env.CODEGRAPH_OPENCODE_DB) return process.env.CODEGRAPH_OPENCODE_DB;
  const data = process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
  return path.join(data, 'opencode', 'opencode.db');
}

function isoFromMs(ms: number): string {
  return new Date(ms).toISOString();
}

export function opencodeSessionsForProject(projectRoot: string): Array<{
  path: string;
  mtime: number;
  size: number;
  session: string;
  title: string | null;
  docs: SessionDoc[];
}> {
  const dbPath = opencodeDbPath();
  if (!fs.existsSync(dbPath)) return [];
  let db: ReturnType<typeof createDatabase>['db'];
  try {
    db = createDatabase(dbPath, { readOnly: true }).db;
  } catch {
    return [];
  }
  try {
    db.pragma('busy_timeout = 5000');
    const sessions = db
      .prepare('SELECT id, directory, title, time_updated FROM session')
      .all() as Array<{ id: string; directory: string | null; title: string | null; time_updated: number }>;
    const out: ReturnType<typeof opencodeSessionsForProject> = [];
    for (const row of sessions) {
      const cwd = row.directory?.trim();
      if (!cwd || cwd === '/' || !cwdBelongsToProject(cwd, projectRoot)) continue;
      const docs = docsForSession(db, row.id);
      const size = docs.reduce((n, d) => n + d.text.length, 0);
      out.push({
        path: `opencode:${dbPath}:${row.id}`,
        mtime: row.time_updated || 0,
        size,
        session: `opencode:${row.id}`,
        title: row.title,
        docs,
      });
    }
    return out;
  } catch {
    return [];
  } finally {
    db.close();
  }
}

function docsForSession(db: ReturnType<typeof createDatabase>['db'], sessionId: string): SessionDoc[] {
  const messages = db
    .prepare('SELECT id, time_created, data FROM message WHERE session_id = ? ORDER BY time_created')
    .all(sessionId) as Array<{ id: string; time_created: number; data: string }>;
  const docs: SessionDoc[] = [];
  for (const msg of messages) {
    let role: string | undefined;
    try {
      role = (JSON.parse(msg.data) as { role?: string }).role;
    } catch {
      continue;
    }
    if (role !== 'user' && role !== 'assistant') continue;
    const parts = db
      .prepare('SELECT data FROM part WHERE message_id = ?')
      .all(msg.id) as Array<{ data: string }>;
    const texts: string[] = [];
    for (const part of parts) {
      try {
        const body = JSON.parse(part.data) as { type?: string; text?: string };
        if (body.type === 'text' && typeof body.text === 'string') texts.push(body.text);
      } catch {
        // Skip a malformed part.
      }
    }
    const text = texts.join('\n').trim();
    if (text.length < MIN_DOC_CHARS) continue;
    docs.push({ ts: isoFromMs(msg.time_created), role, text });
  }
  return docs;
}
