/**
 * Reader for Claude Code's session transcripts — the first agent host the
 * session index knows how to read. One reader per host; a second host (Cursor's
 * chat store, Codex's) is a sibling module with the same `SessionDoc` output.
 *
 * Claude Code keeps one JSONL file per session under
 * `~/.claude/projects/<slug>/` (subagent transcripts in subdirectories,
 * `memory/` holds notes rather than sessions), one JSON entry per line. The
 * slug is the project's absolute path with every non-alphanumeric character
 * replaced by `-`; on Windows the drive letter may be stored lowercased, so
 * the lookup tries both spellings and takes the one that exists.
 *
 * What counts as prose: the user's prompts (including one sent mid-turn, which
 * Claude Code stores as a `queued_command` attachment rather than a user
 * message), the assistant's text blocks and compaction summaries. Tool calls,
 * tool results and thinking blocks are not text blocks and stay out, as do
 * meta entries and anything shorter than `MIN_DOC_CHARS` ("ok").
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

export interface SessionDoc {
  /** ISO timestamp of the entry. */
  ts: string;
  /** `commit` is a git commit message (see `git-log.ts`), not a transcript entry. */
  role: 'user' | 'assistant' | 'summary' | 'commit';
  text: string;
}

interface Entry {
  type?: string;
  timestamp?: string;
  isMeta?: boolean;
  isCompactSummary?: boolean;
  customTitle?: string;
  message?: { content?: unknown };
  attachment?: { type?: string; prompt?: unknown };
}

/** Shorter text is a "yes"/"ok" turn — noise in a prose index. */
export const MIN_DOC_CHARS = 20;

/**
 * A session a SQLite-backed host (OpenCode, Devin) lists cheaply: `mtime` and
 * `size` summarize its message rows, and `docs()` reopens the store only when
 * the index finds the session changed. `docs()` returns null when the store
 * could not be read (busy, mid-migration), so the session is retried on the
 * next query instead of being recorded as empty.
 */
export interface StoredSession {
  path: string;
  mtime: number;
  size: number;
  session: string;
  title: string | null;
  docs: () => SessionDoc[] | null;
}

/** Claude Code's config dir: `CLAUDE_CONFIG_DIR` when set, else `~/.claude`. */
function claudeConfigDir(): string {
  return process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
}

/** The slug Claude Code derives from a project path. */
export function claudeProjectSlug(projectRoot: string): string {
  return path.resolve(projectRoot).replace(/[^a-zA-Z0-9]/g, '-');
}

/**
 * The transcript directory for a project, or null when Claude Code has never
 * run there. Tries the exact-case slug first, then the lowercased one (Windows
 * drive letters).
 */
export function claudeSessionsDir(projectRoot: string): string | null {
  const projects = path.join(claudeConfigDir(), 'projects');
  const slug = claudeProjectSlug(projectRoot);
  for (const candidate of [slug, slug.toLowerCase()]) {
    const dir = path.join(projects, candidate);
    if (fs.existsSync(dir) && fs.statSync(dir).isDirectory()) return dir;
  }
  return null;
}

/**
 * Path prefixes of stores that exist but could not be read on this query. The
 * index keeps what it already holds under them instead of treating them as
 * deleted; see `SessionsIndex.refreshRecords`.
 */
export type UnavailableStores = string[];

/**
 * Every `.jsonl` under `dir`, recursively, skipping `memory/`. With
 * `unavailable`, a directory that cannot be listed is recorded there and
 * skipped; without it, the listing error propagates.
 */
export function walkJsonl(dir: string, unavailable?: UnavailableStores): string[] {
  const out: string[] = [];
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch (err) {
    if (!unavailable) throw err;
    unavailable.push(dir + path.sep);
    return out;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name !== 'memory') out.push(...walkJsonl(full, unavailable));
    } else if (entry.name.endsWith('.jsonl')) {
      out.push(full);
    }
  }
  return out;
}

/** Parse every line of a JSONL transcript; a truncated trailing line from a live session is skipped. */
function parseLines(text: string): Entry[] {
  const entries: Entry[] = [];
  for (const line of text.split('\n')) {
    if (!line) continue;
    try {
      entries.push(JSON.parse(line) as Entry);
    } catch {
      // A partially written line from a session still running.
    }
  }
  return entries;
}

/**
 * What can make an entry carry prose in compact JSON, whatever the order of its
 * keys: a text block (`"type":"text"` stays contiguous), a mid-turn prompt, a
 * compaction summary flag, a custom title. Plain string `content` is the one
 * shape without a fixed marker; see {@link STRING_CONTENT}.
 */
const PROSE_MARKERS = ['"type":"text"', 'queued_command', 'isCompactSummary', '"customTitle"'].map((m) => Buffer.from(m));
/** A string `content` holds a prompt or reply, unless the entry is a tool result carrying its output as a string. */
const STRING_CONTENT = Buffer.from('"content":"');
const TOOL_RESULT = Buffer.from('"type":"tool_result"');
/** The same keys written with spaces mean the compact-JSON assumption no longer holds. */
const SPACED_MARKERS = ['"type": "', '"role": "', '"content": "'].map((m) => Buffer.from(m));

/** Transcripts below this are parsed whole: they cost little and the scan saves nothing. */
export const SCAN_MIN_BYTES = 256 * 1024;

/**
 * Only the lines holding a prose marker, parsed. A transcript is mostly tool
 * traffic (95% of entries on a real history), and reading it as bytes and
 * searching with the native `indexOf` costs a third of decoding and parsing
 * every line. Every shape `transcriptDocs` reads is covered whatever its key
 * order. Null when the scan cannot be trusted and the caller must parse the
 * whole file: spaced JSON, or a large file with no marker at all.
 */
function scanProseLines(buf: Buffer): Entry[] | null {
  if (SPACED_MARKERS.some((m) => buf.includes(m))) return null;
  const starts = new Set<number>();
  const collect = (marker: Buffer, keep?: (start: number, end: number) => boolean): void => {
    let at = buf.indexOf(marker);
    while (at !== -1) {
      const start = buf.lastIndexOf(10, at) + 1;
      let end = buf.indexOf(10, at);
      if (end === -1) end = buf.length;
      if (!keep || keep(start, end)) starts.add(start);
      at = buf.indexOf(marker, end);
    }
  };
  for (const marker of PROSE_MARKERS) collect(marker);
  collect(STRING_CONTENT, (start, end) => !buf.subarray(start, end).includes(TOOL_RESULT));
  if (starts.size === 0) return null;
  const entries: Entry[] = [];
  for (const start of [...starts].sort((a, b) => a - b)) {
    let end = buf.indexOf(10, start);
    if (end === -1) end = buf.length;
    try {
      entries.push(JSON.parse(buf.toString('utf8', start, end)) as Entry);
    } catch {
      // A partially written line from a session still running.
    }
  }
  return entries;
}

/**
 * Parse a JSONL transcript; a truncated trailing line from a live session is
 * skipped. Large files are scanned for the entries that can hold prose instead
 * of parsed whole; the docs and title come out the same.
 */
export function parseEntries(file: string): Entry[] {
  const buf = fs.readFileSync(file);
  if (buf.length >= SCAN_MIN_BYTES) {
    const scanned = scanProseLines(buf);
    if (scanned) return scanned;
  }
  return parseLines(buf.toString('utf8'));
}

function textBlocks(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .filter(
      (b): b is { type: 'text'; text?: string } =>
        typeof b === 'object' && b !== null && (b as { type?: unknown }).type === 'text',
    )
    .map((b) => b.text ?? '')
    .join('\n');
}

/** Which role an entry's prose belongs to, or null when the entry carries none. */
function docRole(e: Entry): { role: SessionDoc['role']; content: unknown } | null {
  if (e.type === 'user' || e.type === 'assistant') {
    return { role: e.isCompactSummary ? 'summary' : e.type, content: e.message?.content };
  }
  if (e.type === 'attachment' && e.attachment?.type === 'queued_command') {
    return { role: 'user', content: e.attachment.prompt };
  }
  return null;
}

/** The prose of a transcript, one doc per prompt, reply or compaction summary. */
export function transcriptDocs(entries: Entry[]): SessionDoc[] {
  const docs: SessionDoc[] = [];
  for (const e of entries) {
    if (!e.timestamp || e.isMeta) continue;
    const doc = docRole(e);
    if (!doc) continue;
    const text = textBlocks(doc.content).trim();
    if (text.length < MIN_DOC_CHARS) continue;
    docs.push({ ts: e.timestamp, role: doc.role, text });
  }
  return docs;
}

/** The session title Claude Code stored last, if any. */
export function transcriptTitle(entries: Entry[]): string | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const title = entries[i]?.customTitle;
    if (title) return title;
  }
  return null;
}

/** The session id is the file's basename; subagent transcripts nest under their parent's id. */
export function sessionIdOf(file: string): string {
  return path.basename(file, '.jsonl');
}
