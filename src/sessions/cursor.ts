/**
 * Reader for Cursor / T3-via-Cursor agent transcripts under
 * `~/.cursor/projects/<path-slug>/agent-transcripts/`.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { MIN_DOC_CHARS, type SessionDoc } from './claude-code';
import { projectWorktreeRoots, walkSessionJsonl } from './collect';

interface CursorLine {
  role?: string;
  message?: { content?: unknown };
}

function cursorConfigDir(): string {
  return process.env.CURSOR_CONFIG_DIR || path.join(os.homedir(), '.cursor');
}

/** Same path → folder mapping Cursor uses for `projects/<slug>`. */
export function cursorProjectSlug(projectRoot: string): string {
  return path.resolve(projectRoot).replace(/^\/+/, '').replace(/[^a-zA-Z0-9]/g, '-');
}

export function cursorFilesForProject(projectRoot: string): string[] {
  const projects = path.join(cursorConfigDir(), 'projects');
  const files: string[] = [];
  for (const root of projectWorktreeRoots(projectRoot)) {
    const dir = path.join(projects, cursorProjectSlug(root), 'agent-transcripts');
    if (fs.existsSync(dir)) files.push(...walkSessionJsonl(dir));
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

export function parseCursorTranscript(file: string): { session: string; title: string | null; docs: SessionDoc[] } {
  const session = path.basename(path.dirname(file));
  const docs: SessionDoc[] = [];
  const mtime = fs.statSync(file).mtime.toISOString();
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    let row: CursorLine;
    try {
      row = JSON.parse(line) as CursorLine;
    } catch {
      continue;
    }
    if (row.role !== 'user' && row.role !== 'assistant') continue;
    const text = textOf(row.message?.content).trim();
    if (text.length < MIN_DOC_CHARS) continue;
    docs.push({ ts: mtime, role: row.role, text });
  }
  return { session: `cursor:${session}`, title: null, docs };
}
