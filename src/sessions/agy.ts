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
import { MIN_DOC_CHARS, type SessionDoc, type UnavailableStores } from './claude-code';
import { cwdInRoots } from './project-roots';

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

/** Workspace paths per conversation from `cache/conversation_metadata.json`, read once per scan. */
function metadataWorkspaces(root: string): Map<string, string[]> {
  const out = new Map<string, string[]>();
  const file = path.join(root, 'cache', 'conversation_metadata.json');
  if (!fs.existsSync(file)) return out;
  try {
    const parsed = JSON.parse(fs.readFileSync(file, 'utf8')) as {
      conversations?: Record<string, { summary?: { WorkspaceURIs?: string[] } }>;
    };
    for (const [id, conv] of Object.entries(parsed.conversations ?? {})) {
      const uris = conv?.summary?.WorkspaceURIs;
      if (Array.isArray(uris)) out.set(id, uris.filter((u): u is string => typeof u === 'string').map(fileUriToPath));
    }
  } catch {
    // Unreadable or malformed: the other sources still apply.
  }
  return out;
}

/**
 * Workspace paths per conversation from `conversation_summaries.db`, read once
 * per scan, or null when the database exists but cannot be opened now.
 */
function summariesWorkspaces(root: string): Map<string, string[]> | null {
  const out = new Map<string, string[]>();
  const dbPath = path.join(root, 'conversation_summaries.db');
  if (!fs.existsSync(dbPath)) return out;
  let db: ReturnType<typeof createDatabase>['db'];
  try {
    db = createDatabase(dbPath, { readOnly: true }).db;
  } catch {
    return null;
  }
  try {
    const rows = db.prepare('SELECT conversation_id, workspace_uris FROM conversation_summaries').all() as Array<{
      conversation_id?: string;
      workspace_uris?: string;
    }>;
    for (const row of rows) {
      if (!row.conversation_id || !row.workspace_uris) continue;
      try {
        const uris = JSON.parse(row.workspace_uris) as unknown;
        if (Array.isArray(uris)) {
          out.set(row.conversation_id, uris.filter((u): u is string => typeof u === 'string').map(fileUriToPath));
        }
      } catch {
        // One malformed row does not hide the others.
      }
    }
  } catch {
    // Missing table or column: the other sources still apply.
  } finally {
    db.close();
  }
  return out;
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

function belongs(roots: readonly string[], workspaces: readonly string[]): boolean {
  return workspaces.some((ws) => cwdInRoots(ws, roots));
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

export function agyFilesForProject(roots: readonly string[], unavailable?: UnavailableStores): string[] {
  const root = antigravityDir();
  const brain = path.join(root, 'brain');
  if (!fs.existsSync(brain)) return [];
  const files: string[] = [];
  let conversations: fs.Dirent[];
  try {
    conversations = fs.readdirSync(brain, { withFileTypes: true });
  } catch (err) {
    if (!unavailable) throw err;
    unavailable.push(brain + path.sep);
    return files;
  }
  const metadata = metadataWorkspaces(root);
  const summaries = summariesWorkspaces(root);
  if (summaries === null) {
    // Which conversations it places in the project is unknown until it opens
    // again; keep whatever the index already holds for AGY.
    if (!unavailable) throw new Error(`cannot open ${path.join(root, 'conversation_summaries.db')}`);
    unavailable.push(brain + path.sep);
    return files;
  }
  for (const dirent of conversations) {
    if (!dirent.isDirectory()) continue;
    const id = dirent.name;
    // The conversation's own database is opened only when the shared sources
    // do not already place it in the project.
    const listed = [...(metadata.get(id) ?? []), ...(summaries.get(id) ?? [])];
    if (!belongs(roots, listed) && !belongs(roots, blobWorkspaces(root, id))) continue;
    const full = path.join(brain, dirent.name, '.system_generated', 'logs', 'transcript_full.jsonl');
    const short = path.join(brain, dirent.name, '.system_generated', 'logs', 'transcript.jsonl');
    if (fs.existsSync(full)) files.push(full);
    else if (fs.existsSync(short)) files.push(short);
  }
  return files;
}
