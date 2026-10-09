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
import { MIN_DOC_CHARS, type SessionDoc, type UnavailableStores } from './claude-code';
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

/** A directory's entries, or null (recorded in `unavailable`) when it cannot be listed. */
function listDir(dir: string, unavailable?: UnavailableStores): fs.Dirent[] | null {
  try {
    return fs.readdirSync(dir, { withFileTypes: true });
  } catch (err) {
    if (!unavailable) throw err;
    unavailable.push(dir + path.sep);
    return null;
  }
}

export function grokFilesForProject(roots: readonly string[], unavailable?: UnavailableStores): string[] {
  const dir = grokSessionsDir();
  if (!fs.existsSync(dir)) return [];
  const files: string[] = [];
  for (const cwdDir of listDir(dir, unavailable) ?? []) {
    if (!cwdDir.isDirectory()) continue;
    const cwd = decodeCwd(cwdDir.name);
    if (!cwd || !path.isAbsolute(cwd) || !cwdInRoots(cwd, roots)) continue;
    const base = path.join(dir, cwdDir.name);
    for (const session of listDir(base, unavailable) ?? []) {
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

/** Blocks a host appends to a prompt; a person's own `<task>` or `<code>` stays. */
const HARNESS_TAGS = new Set([
  'runtime_info',
  'pull_request_linking',
  'system-reminder',
  'environment_context',
  'user_instructions',
  'skills_instructions',
]);
const TAGGED_BLOCK = /<([a-z_-]+)>[\s\S]*?<\/\1>/g;

/** A user chunk with the host's blocks removed. */
function withoutHarness(text: string): string {
  return text.replace(TAGGED_BLOCK, (block, tag: string) => (HARNESS_TAGS.has(tag) ? '' : block));
}

/** ACP timestamps are seconds; tolerate milliseconds and junk (empty = the file's time). */
function isoFrom(value: unknown): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) return '';
  return new Date(value > 1e11 ? value : value * 1000).toISOString();
}

/** The `summary.json` beside a session's `updates.jsonl`; it holds the title. */
export function grokSummaryFile(file: string): string {
  return path.join(path.dirname(file), 'summary.json');
}

function titleOf(file: string): string | null {
  try {
    const summary = JSON.parse(fs.readFileSync(grokSummaryFile(file), 'utf8')) as {
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
    // Agent chunks are pieces of one stream. User chunks are usually whole
    // blocks: joined on a blank line unless one side already has whitespace.
    const text = buf
      .reduce((acc, piece) => {
        if (!acc || role !== 'user' || /\s$/.test(acc) || /^\s/.test(piece)) return acc + piece;
        return `${acc}\n\n${piece}`;
      }, '')
      .trim();
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
    const kind = update?.sessionUpdate ?? '';
    const r = ROLE_OF[kind];
    if (!r) {
      // A tool call or other event between chunks ends the message; a thought does not.
      if (kind !== 'agent_thought_chunk') emit();
      continue;
    }
    let text = typeof update?.content?.text === 'string' ? update.content.text : '';
    if (r === 'user') text = withoutHarness(text);
    if (!text.trim()) continue; // An empty or non-text chunk neither adds to nor ends the message.
    if (role !== r) {
      emit();
      role = r;
      ts = isoFrom(row.timestamp);
    }
    buf.push(text);
  }
  emit();
  return { session: `grok:${path.basename(path.dirname(file))}`, title: titleOf(file), docs };
}
