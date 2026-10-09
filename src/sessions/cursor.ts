/**
 * Reader for Cursor / T3-via-Cursor agent transcripts under
 * `~/.cursor/projects/<path-slug>/agent-transcripts/`.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { MIN_DOC_CHARS, type SessionDoc, type UnavailableStores } from './claude-code';
import { walkSessionJsonl } from './collect';

interface CursorLine {
  role?: string;
  message?: { content?: unknown };
}

function cursorConfigDir(): string {
  return process.env.CURSOR_CONFIG_DIR || path.join(os.homedir(), '.cursor');
}

/**
 * Same path → folder mapping Cursor uses for `projects/<slug>`: each run of
 * non-alphanumerics becomes one dash (`/home/u/.t3/wt` → `home-u-t3-wt`),
 * unlike Claude Code, which maps every character.
 */
export function cursorProjectSlug(projectRoot: string): string {
  return path.resolve(projectRoot).replace(/[^a-zA-Z0-9]+/g, '-').replace(/^-+|-+$/g, '');
}

export function cursorFilesForProject(roots: readonly string[], unavailable?: UnavailableStores): string[] {
  const projects = path.join(cursorConfigDir(), 'projects');
  const files: string[] = [];
  for (const root of roots) {
    const dir = path.join(projects, cursorProjectSlug(root), 'agent-transcripts');
    if (fs.existsSync(dir)) files.push(...walkSessionJsonl(dir, unavailable));
  }
  return files;
}

function textOf(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .filter(
      (b): b is { type: string; text?: string } =>
        typeof b === 'object' &&
        b !== null &&
        (b as { type?: unknown }).type === 'text' &&
        typeof (b as { text?: unknown }).text === 'string',
    )
    .map((b) => b.text ?? '')
    .join('\n');
}

const MONTHS = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec'];
const STAMP = /^\s*<timestamp>[^,]*,\s*([A-Za-z]{3})[a-z]*\s+(\d{1,2}),\s*(\d{4}),\s*(\d{1,2}):(\d{2})\s*([AP]M)\s*\(UTC([+-]\d{1,2})(?::?(\d{2}))?\)\s*<\/timestamp>\s*/i;

/**
 * Cursor stores no per-entry time; each user prompt instead opens with
 * `<timestamp>Sunday, Sep 27, 2026, 3:02 AM (UTC-6)</timestamp>`. Returns the
 * ISO instant and the prompt without that line, or null when it is absent.
 */
export function cursorStamp(text: string): { iso: string; rest: string } | null {
  const m = STAMP.exec(text);
  if (!m) return null;
  const month = MONTHS.indexOf(m[1]!.toLowerCase());
  if (month < 0) return null;
  let hour = Number(m[4]) % 12;
  if (m[6]!.toUpperCase() === 'PM') hour += 12;
  const sign = m[7]!.startsWith('-') ? -1 : 1;
  const offsetMin = Number(m[7]) * 60 + sign * Number(m[8] ?? 0);
  const utc = Date.UTC(Number(m[3]), month, Number(m[2]), hour, Number(m[5])) - offsetMin * 60_000;
  return { iso: new Date(utc).toISOString(), rest: text.slice(m[0].length) };
}

export function parseCursorTranscript(file: string): { session: string; title: string | null; docs: SessionDoc[] } {
  const session = path.basename(path.dirname(file));
  const docs: SessionDoc[] = [];
  // A reply carries the time of the prompt before it; the file's mtime is the
  // fallback for a transcript whose prompts have no stamp.
  let ts = fs.statSync(file).mtime.toISOString();
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    let row: CursorLine;
    try {
      row = JSON.parse(line) as CursorLine;
    } catch {
      continue;
    }
    if (row.role !== 'user' && row.role !== 'assistant') continue;
    let text = textOf(row.message?.content);
    const stamp = row.role === 'user' ? cursorStamp(text) : null;
    if (stamp) {
      ts = stamp.iso;
      text = stamp.rest;
    }
    text = text.trim();
    if (text.length < MIN_DOC_CHARS) continue;
    docs.push({ ts, role: row.role, text });
  }
  return { session: `cursor:${session}`, title: null, docs };
}
