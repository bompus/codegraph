/**
 * Reader for Grok CLI sessions under `~/.grok/sessions/<url-encoded cwd>/<id>/`.
 * `updates.jsonl` is the ACP `session/update` stream: a message arrives as
 * consecutive `user_message_chunk` or `agent_message_chunk` lines, joined here
 * into one doc. Thoughts (`agent_thought_chunk`) and tool calls stay out, as
 * do the harness blocks (`<runtime_info>`, `<pull_request_linking>`) a host
 * sends as separate user chunks. `summary.json` holds the session's title.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { MIN_DOC_CHARS, type SessionDoc } from './claude-code';
import { cwdInRoots } from './project-roots';

export function grokSessionsDir(): string {
  if (process.env.CODEGRAPH_GROK_DIR) return process.env.CODEGRAPH_GROK_DIR;
  return path.join(os.homedir(), '.grok', 'sessions');
}

function decodeCwd(name: string): string | null {
  try {
    return decodeURIComponent(name);
  } catch {
    return null;
  }
}

export function grokFilesForProject(roots: readonly string[]): string[] {
  const dir = grokSessionsDir();
  if (!fs.existsSync(dir)) return [];
  const files: string[] = [];
  for (const cwdDir of fs.readdirSync(dir, { withFileTypes: true })) {
    if (!cwdDir.isDirectory()) continue;
    const cwd = decodeCwd(cwdDir.name);
    if (!cwd || !path.isAbsolute(cwd) || !cwdInRoots(cwd, roots)) continue;
    const base = path.join(dir, cwdDir.name);
    for (const session of fs.readdirSync(base, { withFileTypes: true })) {
      const file = path.join(base, session.name, 'updates.jsonl');
      if (session.isDirectory() && fs.existsSync(file)) files.push(file);
    }
  }
  return files;
}

interface GrokLine {
  timestamp?: number;
  params?: { update?: { sessionUpdate?: string; content?: { type?: string; text?: unknown } } };
}

const ROLE_OF: Record<string, SessionDoc['role']> = {
  user_message_chunk: 'user',
  agent_message_chunk: 'assistant',
};

/** A user chunk that is a harness block, not the person's words. */
const HARNESS_BLOCK = /^\s*<[a-z_]+>[\s\S]*<\/[a-z_]+>\s*$/;

function titleOf(file: string): string | null {
  try {
    const summary = JSON.parse(fs.readFileSync(path.join(path.dirname(file), 'summary.json'), 'utf8')) as {
      session_summary?: unknown;
    };
    return typeof summary.session_summary === 'string' ? summary.session_summary : null;
  } catch {
    return null;
  }
}

export function parseGrokTranscript(file: string): { session: string; title: string | null; docs: SessionDoc[] } {
  const docs: SessionDoc[] = [];
  let role: SessionDoc['role'] | null = null;
  let ts = '';
  let buf: string[] = [];
  const emit = (): void => {
    const text = buf.join('').trim();
    if (role && text.length >= MIN_DOC_CHARS) docs.push({ ts, role, text });
    role = null;
    buf = [];
  };
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    let row: GrokLine;
    try {
      row = JSON.parse(line) as GrokLine;
    } catch {
      continue;
    }
    const update = row.params?.update;
    const r = ROLE_OF[update?.sessionUpdate ?? ''];
    const text = typeof update?.content?.text === 'string' ? update.content.text : '';
    if (!r || !text) {
      // Anything between chunks (a tool call, a thought) ends the message.
      if (update?.sessionUpdate !== 'agent_thought_chunk') emit();
      continue;
    }
    if (r === 'user' && HARNESS_BLOCK.test(text)) continue;
    if (role !== r) {
      emit();
      role = r;
      ts = new Date((row.timestamp ?? 0) * 1000).toISOString();
    }
    buf.push(text);
  }
  emit();
  return { session: `grok:${path.basename(path.dirname(file))}`, title: titleOf(file), docs };
}
