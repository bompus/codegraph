/**
 * Reader for Codex / T3-via-Codex JSONL under `~/.codex/sessions/`.
 * Indexes user/assistant message text only; skips tools, developer prompts,
 * world state, and injected instruction blobs.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { MIN_DOC_CHARS, type SessionDoc } from './claude-code';
import { cwdBelongsToProject, walkSessionJsonl } from './collect';

interface CodexLine {
  timestamp?: string;
  type?: string;
  payload?: {
    cwd?: string;
    session_id?: string;
    type?: string;
    role?: string;
    content?: unknown;
  };
}

function codexHome(): string {
  return process.env.CODEX_HOME || path.join(os.homedir(), '.codex');
}

export function codexSessionsDir(): string {
  return path.join(codexHome(), 'sessions');
}

function injectedBlob(text: string): boolean {
  return (
    text.startsWith('<recommended_plugins>') ||
    text.startsWith('<environment_context>') ||
    text.startsWith('<skills_instructions>') ||
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

function sessionCwd(file: string): string | null {
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    try {
      const row = JSON.parse(line) as CodexLine;
      if (row.type === 'session_meta' && row.payload?.cwd) return row.payload.cwd;
      if (row.type === 'turn_context' && row.payload?.cwd) return row.payload.cwd;
    } catch {
      // Live truncated line.
    }
  }
  return null;
}

export function codexFilesForProject(projectRoot: string): string[] {
  const dir = codexSessionsDir();
  if (!fs.existsSync(dir)) return [];
  return walkSessionJsonl(dir).filter((file) => {
    const cwd = sessionCwd(file);
    return cwd !== null && cwdBelongsToProject(cwd, projectRoot);
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
