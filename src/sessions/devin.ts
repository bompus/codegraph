/**
 * Devin (CLI and Desktop share one local harness) stores sessions in SQLite
 * under `<data>/devin/<harness>/sessions.db` — `cli/` and `cli-next/` today.
 * `sessions.working_directory` is the project cwd; messages live in
 * `message_nodes.chat_message` as JSON `{role, content, tool_calls}`.
 * Roles `system` and `tool` stay out, as do deleted (`hidden`) sessions.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createDatabase } from '../db/sqlite-adapter';
import { MIN_DOC_CHARS, type SessionDoc } from './claude-code';
import { cwdBelongsToProject } from './project-roots';

export function devinDataDir(): string {
  if (process.env.CODEGRAPH_DEVIN_DIR) return process.env.CODEGRAPH_DEVIN_DIR;
  const data = process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
  return path.join(data, 'devin');
}

function devinDbPaths(): string[] {
  const root = devinDataDir();
  if (!fs.existsSync(root)) return [];
  return fs
    .readdirSync(root, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => path.join(root, d.name, 'sessions.db'))
    .filter((p) => fs.existsSync(p));
}

function isoFromSeconds(s: number): string {
  return new Date(s * 1000).toISOString();
}

function contentText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .map((b) => (typeof b === 'object' && b !== null && typeof (b as { text?: unknown }).text === 'string' ? (b as { text: string }).text : ''))
    .join('\n');
}

export function devinSessionsForProject(projectRoot: string): Array<{
  path: string;
  mtime: number;
  size: number;
  session: string;
  title: string | null;
  docs: SessionDoc[];
}> {
  const out: ReturnType<typeof devinSessionsForProject> = [];
  for (const dbPath of devinDbPaths()) {
    let db: ReturnType<typeof createDatabase>['db'];
    try {
      db = createDatabase(dbPath, { readOnly: true }).db;
    } catch {
      continue;
    }
    try {
      db.pragma('busy_timeout = 5000');
      const sessions = db
        .prepare('SELECT id, working_directory, title, last_activity_at, hidden FROM sessions')
        .all() as Array<{
        id: string;
        working_directory: string | null;
        title: string | null;
        last_activity_at: number | null;
        hidden: number;
      }>;
      for (const row of sessions) {
        const cwd = row.working_directory?.trim();
        if (row.hidden !== 0 || !cwd || cwd === '/' || !cwdBelongsToProject(cwd, projectRoot)) continue;
        const docs = docsForSession(db, row.id);
        if (docs.length === 0) continue;
        const size = docs.reduce((n, d) => n + d.text.length, 0);
        out.push({
          path: `devin:${dbPath}:${row.id}`,
          mtime: (row.last_activity_at ?? 0) * 1000,
          size,
          session: `devin:${row.id}`,
          title: row.title,
          docs,
        });
      }
    } catch {
      // A store whose schema predates these columns contributes nothing.
    } finally {
      db.close();
    }
  }
  return out;
}

function docsForSession(db: ReturnType<typeof createDatabase>['db'], sessionId: string): SessionDoc[] {
  const nodes = db
    .prepare('SELECT chat_message, created_at FROM message_nodes WHERE session_id = ? ORDER BY node_id')
    .all(sessionId) as Array<{ chat_message: string; created_at: number }>;
  const docs: SessionDoc[] = [];
  for (const node of nodes) {
    let msg: { role?: string; content?: unknown };
    try {
      msg = JSON.parse(node.chat_message) as typeof msg;
    } catch {
      continue;
    }
    if (msg.role !== 'user' && msg.role !== 'assistant') continue;
    const text = contentText(msg.content).trim();
    if (text.length < MIN_DOC_CHARS) continue;
    docs.push({ ts: isoFromSeconds(node.created_at), role: msg.role, text });
  }
  return docs;
}
