/**
 * Antigravity / AGY CLI transcripts under `~/.gemini/antigravity-cli/`.
 * A conversation is indexed only when a `file://` workspace URI is present
 * (metadata JSON, summaries DB, or the conversation blob). `ProjectID:
 * default-cli-project` is not used.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createDatabase } from '../db/sqlite-adapter';
import { MIN_DOC_CHARS, type SessionDoc } from './claude-code';
import { cwdBelongsToProject } from './project-roots';

const FILE_URI = /file:\/\/(\/[\w./@+\-]+)/g;

export function antigravityDir(): string {
  return process.env.CODEGRAPH_ANTIGRAVITY_DIR || path.join(os.homedir(), '.gemini', 'antigravity-cli');
}

function fileUriToPath(uri: string): string {
  return uri.startsWith('file://') ? decodeURIComponent(uri.slice('file://'.length)) : uri;
}

function pathsFromText(text: string): string[] {
  const out: string[] = [];
  FILE_URI.lastIndex = 0;
  for (const m of text.matchAll(FILE_URI)) {
    if (m[1]) out.push(m[1]);
  }
  return out;
}

function metadataWorkspaces(root: string, id: string): string[] {
  const file = path.join(root, 'cache', 'conversation_metadata.json');
  if (!fs.existsSync(file)) return [];
  try {
    const parsed = JSON.parse(fs.readFileSync(file, 'utf8')) as {
      conversations?: Record<string, { summary?: { WorkspaceURIs?: string[] } }>;
    };
    return (parsed.conversations?.[id]?.summary?.WorkspaceURIs ?? []).map(fileUriToPath);
  } catch {
    return [];
  }
}

function summariesWorkspaces(root: string, id: string): string[] {
  const dbPath = path.join(root, 'conversation_summaries.db');
  if (!fs.existsSync(dbPath)) return [];
  const { db } = createDatabase(dbPath, { readOnly: true });
  try {
    const row = db
      .prepare('SELECT workspace_uris FROM conversation_summaries WHERE conversation_id = ?')
      .get(id) as { workspace_uris?: string } | undefined;
    if (!row?.workspace_uris) return [];
    const uris = JSON.parse(row.workspace_uris) as unknown;
    if (!Array.isArray(uris)) return [];
    return uris.filter((u): u is string => typeof u === 'string').map(fileUriToPath);
  } catch {
    return [];
  } finally {
    db.close();
  }
}

function blobWorkspaces(root: string, id: string): string[] {
  const dbPath = path.join(root, 'conversations', `${id}.db`);
  if (!fs.existsSync(dbPath)) return [];
  const { db } = createDatabase(dbPath, { readOnly: true });
  try {
    const row = db.prepare('SELECT data FROM trajectory_metadata_blob LIMIT 1').get() as
      | { data?: Buffer | Uint8Array | string }
      | undefined;
    if (!row?.data) return [];
    const buf = typeof row.data === 'string' ? Buffer.from(row.data) : Buffer.from(row.data);
    return pathsFromText(buf.toString('latin1'));
  } catch {
    return [];
  } finally {
    db.close();
  }
}

export function conversationWorkspaces(root: string, id: string): string[] {
  const found = [...metadataWorkspaces(root, id), ...summariesWorkspaces(root, id), ...blobWorkspaces(root, id)];
  return [...new Set(found)];
}

function belongs(projectRoot: string, workspaces: string[]): boolean {
  return workspaces.some((ws) => cwdBelongsToProject(ws, projectRoot));
}

function userRequest(content: string): string {
  const m = content.match(/<USER_REQUEST>\s*([\s\S]*?)\s*<\/USER_REQUEST>/);
  return (m?.[1] ?? content).trim();
}

export function parseAgyTranscript(file: string): { session: string; title: string | null; docs: SessionDoc[] } {
  const id = path.basename(path.dirname(path.dirname(path.dirname(file))));
  const docs: SessionDoc[] = [];
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    let row: {
      type?: string;
      created_at?: string;
      content?: string;
    };
    try {
      row = JSON.parse(line) as typeof row;
    } catch {
      continue;
    }
    let text = '';
    let role: SessionDoc['role'] | null = null;
    if (row.type === 'USER_INPUT' && typeof row.content === 'string') {
      role = 'user';
      text = userRequest(row.content);
    } else if (row.type === 'PLANNER_RESPONSE' && typeof row.content === 'string') {
      role = 'assistant';
      text = row.content.trim();
    }
    if (!role || text.length < MIN_DOC_CHARS) continue;
    docs.push({ ts: row.created_at ?? '', role, text });
  }
  return { session: `agy:${id}`, title: null, docs };
}

export function agyFilesForProject(projectRoot: string): string[] {
  const root = antigravityDir();
  const brain = path.join(root, 'brain');
  if (!fs.existsSync(brain)) return [];
  const files: string[] = [];
  for (const dirent of fs.readdirSync(brain, { withFileTypes: true })) {
    if (!dirent.isDirectory()) continue;
    const workspaces = conversationWorkspaces(root, dirent.name);
    if (!belongs(projectRoot, workspaces)) continue;
    const full = path.join(brain, dirent.name, '.system_generated', 'logs', 'transcript_full.jsonl');
    const short = path.join(brain, dirent.name, '.system_generated', 'logs', 'transcript.jsonl');
    if (fs.existsSync(full)) files.push(full);
    else if (fs.existsSync(short)) files.push(short);
  }
  return files;
}
