/**
 * What stays out of the session index, and how long prose is cut, applied to
 * every host's docs in one place (`SessionsIndex.replaceRecord`).
 *
 * Injected text is user-role prose a host or harness wrote rather than the
 * person: skill bodies, subagent completion notices (the report is indexed
 * again from the subagent's own transcript), system reminders, and the input a
 * summarizer is handed (the whole conversation, re-sent as one "user"
 * message). It holds every word of the session, so it wins any query and
 * buries the passage that answers it. Compaction *summaries* (role
 * `summary`) stay: they are the shortest statement of what a session did.
 */
import type { SessionDoc } from './claude-code';

/** Markers matched in the first `INJECTED_HEAD` characters, lowercased. */
const INJECTED_MARKERS = [
  'base directory for this skill',
  '<task-notification>',
  '<command-message>',
  '<command-name>',
  '<system-reminder>',
  '<local-command-stdout>',
  'conversation to summarize',
  'now summarize the conversation',
  'output a summary',
  'output a new summary',
  '=== message 0',
  '<recommended_plugins>',
  '<environment_context>',
  '<skills_instructions>',
  '<user_instructions>',
  '# agents.md instructions',
];
const INJECTED_HEAD = 300;

/** True for user-role prose a host injected; see the module comment. */
export function isInjectedDoc(role: SessionDoc['role'], text: string): boolean {
  if (role !== 'user') return false;
  const head = text.slice(0, INJECTED_HEAD).toLowerCase();
  return INJECTED_MARKERS.some((m) => head.includes(m));
}

/** Docs longer than this are indexed as paragraph-bounded passages. */
export const PASSAGE_LIMIT = 4000;
const PASSAGE_TARGET = 2000;

/**
 * Cut `text` into chunks near `PASSAGE_TARGET` characters on paragraph
 * boundaries; a paragraph longer than twice the target splits on lines, then
 * hard. A 60 KB pasted log becomes thirty rows, so the one paragraph that
 * answers a query ranks on its own instead of losing to the whole paste.
 */
export function splitPassages(text: string): string[] {
  if (text.length <= PASSAGE_LIMIT) return [text];
  const pieces: string[] = [];
  for (const raw of text.split(/\n\s*\n/)) {
    let block = raw.trim();
    while (block.length > 2 * PASSAGE_TARGET) {
      let cut = block.lastIndexOf('\n', 2 * PASSAGE_TARGET);
      if (cut < PASSAGE_TARGET / 2) cut = 2 * PASSAGE_TARGET;
      pieces.push(block.slice(0, cut).trim());
      block = block.slice(cut).trim();
    }
    if (block) pieces.push(block);
  }
  const chunks: string[] = [];
  let current = '';
  for (const piece of pieces) {
    if (current && current.length + piece.length > PASSAGE_TARGET) {
      chunks.push(current);
      current = '';
    }
    current = current ? `${current}\n\n${piece}` : piece;
  }
  if (current) chunks.push(current);
  return chunks;
}

/** The indexable form of a transcript's docs: injected text dropped, long prose cut. */
export function indexableDocs(docs: readonly SessionDoc[]): SessionDoc[] {
  const out: SessionDoc[] = [];
  for (const d of docs) {
    if (isInjectedDoc(d.role, d.text)) continue;
    for (const text of splitPassages(d.text)) out.push({ ...d, text });
  }
  return out;
}
