/**
 * Reader for Codex / T3-via-Codex JSONL under `~/.codex/sessions/`.
 * Indexes user/assistant message text only; skips tools, developer prompts,
 * world state, and injected instruction blobs.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { MIN_DOC_CHARS, type SessionDoc } from './claude-code';
import { walkSessionJsonl } from './collect';
import { cwdInRoots } from './project-roots';

interface CodexLine {
  timestamp?: string;
  type?: string;
  payload?: {
    cwd?: string;
    session_id?: string;
    type?: string;
    role?: string;
    content?: unknown;
    git?: { repository_url?: string };
  };
}

function codexHome(): string {
  return process.env.CODEX_HOME || path.join(os.homedir(), '.codex');
}

export function codexSessionsDir(): string {
  return path.join(codexHome(), 'sessions');
}

/** Instruction blocks Codex prepends as their own content parts. */
function injectedBlob(text: string): boolean {
  return (
    text.startsWith('<recommended_plugins>') ||
    text.startsWith('<environment_context>') ||
    text.startsWith('<skills_instructions>') ||
    text.startsWith('<user_instructions>') ||
    text.startsWith('# AGENTS.md instructions')
  );
}

function messageText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  const parts: string[] = [];
  for (const block of content) {
    if (typeof block !== 'object' || block === null) continue;
    const type = (block as { type?: unknown }).type;
    const text = (block as { text?: unknown }).text;
    if ((type === 'input_text' || type === 'output_text' || type === 'text') && typeof text === 'string') {
      if (!injectedBlob(text)) parts.push(text);
    }
  }
  return parts.join('\n');
}

/** Past this, a first line is not a `session_meta` record worth parsing. */
const FIRST_LINE_MAX = 4 * 1024 * 1024;

/**
 * The file's first line, read in chunks until its newline: `session_meta`
 * comes first and carries the cwd, so there is no need to read (and JSON-parse)
 * a multi-megabyte rollout to decide whether it belongs to the project.
 */
function firstLine(file: string): string {
  const fd = fs.openSync(file, 'r');
  try {
    const chunks: Buffer[] = [];
    const buf = Buffer.alloc(64 * 1024);
    let total = 0;
    for (;;) {
      const n = fs.readSync(fd, buf, 0, buf.length, total);
      if (n === 0) break;
      const nl = buf.subarray(0, n).indexOf(0x0a);
      chunks.push(Buffer.from(buf.subarray(0, nl === -1 ? n : nl)));
      total += n;
      if (nl !== -1 || total >= FIRST_LINE_MAX) break;
    }
    return Buffer.concat(chunks).toString('utf8');
  } finally {
    fs.closeSync(fd);
  }
}

/** A session's cwd and git remote from its `session_meta` line. */
function sessionMeta(file: string): { cwd: string | null; remote: string | null } {
  try {
    const row = JSON.parse(firstLine(file)) as CodexLine;
    if (row.type === 'session_meta') {
      return { cwd: row.payload?.cwd ?? null, remote: row.payload?.git?.repository_url ?? null };
    }
  } catch {
    // Empty or truncated file from a session that just started.
  }
  return { cwd: null, remote: null };
}

/** `https://host/o/r.git`, `git@host:o/r` and `ssh://git@host/o/r` compare equal. */
export function normalizeRemote(url: string): string {
  return url
    .trim()
    .replace(/^[a-z+]+:\/\//i, '')
    .replace(/^[^@/]+@/, '')
    .replace(':', '/')
    .replace(/\.git$/, '')
    .replace(/\/+$/, '')
    .toLowerCase();
}

/**
 * Codex rollouts whose session ran in one of `roots`, or whose recorded git
 * remote is one of the project's (`remotes`, normalized): a worktree removed
 * before the index first saw it leaves no root behind, but its sessions still
 * name the repository.
 */
export function codexFilesForProject(roots: readonly string[], remotes: readonly string[] = []): string[] {
  const dir = codexSessionsDir();
  if (!fs.existsSync(dir)) return [];
  const wanted = new Set(remotes.map(normalizeRemote));
  return walkSessionJsonl(dir).filter((file) => {
    const { cwd, remote } = sessionMeta(file);
    if (cwd !== null && cwdInRoots(cwd, roots)) return true;
    return remote !== null && wanted.has(normalizeRemote(remote));
  });
}

export function parseCodexTranscript(file: string): { session: string; title: string | null; docs: SessionDoc[] } {
  const docs: SessionDoc[] = [];
  let session = path.basename(file, '.jsonl');
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    let row: CodexLine;
    try {
      row = JSON.parse(line) as CodexLine;
    } catch {
      continue;
    }
    if (row.type === 'session_meta' && row.payload?.session_id) {
      session = row.payload.session_id;
      continue;
    }
    if (row.type !== 'response_item' || row.payload?.type !== 'message') continue;
    const role = row.payload.role;
    if (role !== 'user' && role !== 'assistant') continue;
    const text = messageText(row.payload.content).trim();
    if (text.length < MIN_DOC_CHARS) continue;
    docs.push({ ts: row.timestamp ?? '', role, text });
  }
  return { session: `codex:${session}`, title: null, docs };
}
