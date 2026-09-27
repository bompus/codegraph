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

/** Prefixes (lowercased, after leading whitespace) of text a host wrote. */
const INJECTED_PREFIXES = [
  'base directory for this skill',
  '<task-notification>',
  '<system-reminder>',
  '<local-command-stdout>',
  '<recommended_plugins>',
  '<environment_context>',
  '<skills_instructions>',
  '<user_instructions>',
  '# agents.md instructions',
];

/**
 * Phrases of a summarizer's instructions, matched in the first
 * `SUMMARIZER_HEAD` characters, and only in text long enough to carry a whole
 * conversation: "can you output a summary of the failing tests?" is a prompt.
 */
const SUMMARIZER_MARKERS = [
  'conversation to summarize',
  'now summarize the conversation',
  'output a summary',
  'output a new summary',
  '=== message 0',
];
const SUMMARIZER_HEAD = 300;
const SUMMARIZER_MIN = 4000;

/** True for user-role prose a host injected; see the module comment. */
export function isInjectedDoc(role: SessionDoc['role'], text: string): boolean {
  if (role !== 'user') return false;
  const head = text.trimStart().slice(0, SUMMARIZER_HEAD).toLowerCase();
  if (INJECTED_PREFIXES.some((p) => head.startsWith(p))) return true;
  return text.length >= SUMMARIZER_MIN && SUMMARIZER_MARKERS.some((m) => head.includes(m));
}

/**
 * A slash command as the user typed it, `/name args`, from the
 * `<command-message>`/`<command-name>`/`<command-args>` block a host records;
 * null when `text` is not such a block or the command had no arguments (a bare
 * `/clear` says nothing worth finding).
 */
export function slashCommandText(text: string): string | null {
  const head = text.trimStart().toLowerCase();
  if (!head.startsWith('<command-message>') && !head.startsWith('<command-name>')) return null;
  const args = /<command-args>([\s\S]*?)<\/command-args>/.exec(text)?.[1]?.trim();
  if (!args) return null;
  const name = /<command-name>([\s\S]*?)<\/command-name>/.exec(text)?.[1]?.trim() ?? '';
  return `${name} ${args}`.trim();
}

/** Docs longer than this are indexed as paragraph-bounded passages. */
export const PASSAGE_LIMIT = 4000;
const PASSAGE_TARGET = 2000;
const PASSAGE_MIN = 200;

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
      // A line break, else a space, else a hard cut: never mid-word when avoidable.
      let cut = block.lastIndexOf('\n', 2 * PASSAGE_TARGET);
      if (cut < PASSAGE_TARGET / 2) cut = block.lastIndexOf(' ', 2 * PASSAGE_TARGET);
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
  // A short tail joins the passage before it rather than ranking on its own.
  if (current && current.length < PASSAGE_MIN && chunks.length) chunks.push(`${chunks.pop()}\n\n${current}`);
  else if (current) chunks.push(current);
  return chunks;
}

/** The indexable form of a transcript's docs: injected text dropped, long prose cut. */
export function indexableDocs(docs: readonly SessionDoc[]): SessionDoc[] {
  const out: SessionDoc[] = [];
  for (const d of docs) {
    const command = d.role === 'user' ? slashCommandText(d.text) : null;
    if (command !== null) {
      out.push({ ...d, text: command });
      continue;
    }
    if (isInjectedDoc(d.role, d.text) || /^\s*<command-(message|name)>/i.test(d.text)) continue;
    for (const text of splitPassages(d.text)) out.push({ ...d, text });
  }
  return out;
}
