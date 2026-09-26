/**
 * Change-seeded explore: a question about "my changes" or "this branch"
 * answers from the diff, not from the words.
 *
 * Asked "what do my changes affect?", explore used to search for the words
 * `changes` and `affect` and return whatever matched them. The agent already
 * calls explore with that question, so explore reads the diff itself: the
 * merge base with the default branch plus uncommitted edits, or a revision
 * range the query spells out (`main..HEAD`). The indexed symbols those hunks
 * touch become explore's seeds, and their callers lead the answer.
 *
 * Git trouble of any kind (not a repository, no default branch, an unknown
 * revision) yields null and explore answers the query as it always did.
 */

import { execFileSync } from 'child_process';
import type { Node } from '../types';

/** A changed file and the new-side line ranges its hunks touch (1-based, inclusive). */
export interface ChangedFile {
  path: string;
  ranges: Array<[number, number]>;
}

export interface ChangeSet {
  /** What the diff is against, as the answer shows it: "uncommitted edits", "`main..HEAD`". */
  label: string;
  files: ChangedFile[];
  /** The query with the change phrase removed, for the ordinary text match. */
  remainingQuery: string;
}

const CHANGE_PHRASES: RegExp[] = [
  /\b(?:my|our|these|current|local|uncommitted|unstaged|staged|pending|working[- ]tree)\s+(?:code\s+)?(?:changes|edits|diff|modifications)\b/gi,
  /\b(?:this|my|current)\s+(?:branch|pr|pull request)\b/gi,
  /\bwhat\s+(?:did|have)\s+(?:i|we)\s+(?:changed?|touched|modified|edited)\b/gi,
];
// `main..HEAD`, `origin/main...feature`: both sides start with a word character,
// so nothing that reaches git can be read as an option.
const REV_RANGE = /(?<![\w./~^@-])([\w][\w./~^@-]*)(\.\.\.?)([\w][\w./~^@-]*)?(?![\w./~^@-])/;

function git(root: string, args: string[]): string {
  return execFileSync('git', ['-c', 'core.quotepath=off', ...args], {
    cwd: root, encoding: 'utf-8', timeout: 5000, maxBuffer: 32 * 1024 * 1024,
    stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true,
  });
}

function verifyCommit(root: string, rev: string): boolean {
  try { git(root, ['rev-parse', '--verify', '--quiet', `${rev}^{commit}`]); return true; } catch { return false; }
}

/** The remote default branch (`origin/HEAD`), else the first of the usual names that exists. */
function defaultBranch(root: string): string | null {
  try {
    const ref = git(root, ['rev-parse', '--abbrev-ref', 'origin/HEAD']).trim();
    if (ref && ref !== 'origin/HEAD' && verifyCommit(root, ref)) return ref;
  } catch { /* no origin/HEAD */ }
  return ['origin/main', 'origin/master', 'main', 'master'].find((r) => verifyCommit(root, r)) ?? null;
}

/** New-side hunk ranges per file from a `-U0` patch. A pure deletion marks the line it follows. */
export function parseUnifiedZero(patch: string): ChangedFile[] {
  const files: ChangedFile[] = [];
  let current: ChangedFile | null = null;
  for (const line of patch.split('\n')) {
    if (line.startsWith('+++ ')) {
      const target = line.slice(4);
      current = target === '/dev/null' ? null : { path: target.replace(/^b\//, ''), ranges: [] };
      if (current) files.push(current);
      continue;
    }
    const hunk = current && /^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (hunk && current) {
      const start = Number(hunk[1]);
      const count = hunk[2] === undefined ? 1 : Number(hunk[2]);
      current.ranges.push(count === 0 ? [Math.max(1, start), Math.max(1, start)] : [start, start + count - 1]);
    }
  }
  return files.filter((f) => f.ranges.length > 0);
}

/**
 * The files and line ranges a change question is about, or null when the query
 * isn't one or git can't answer it.
 */
export function collectChanges(root: string, query: string): ChangeSet | null {
  const phrased = CHANGE_PHRASES.some((re) => { re.lastIndex = 0; return re.test(query); });
  const range = REV_RANGE.exec(query);
  if (!phrased && !range) return null;
  let remainingQuery = query;
  for (const re of CHANGE_PHRASES) remainingQuery = remainingQuery.replace(re, ' ');
  try {
    // An ellipsis or a version span (`1..10`) matches the range shape too; it
    // counts only when both ends are commits.
    const [rangeSpan, from, dots, to = 'HEAD'] = range ?? [];
    if (range && verifyCommit(root, from!) && verifyCommit(root, to)) {
      remainingQuery = remainingQuery.replace(rangeSpan!, ' ');
      const patch = git(root, ['diff', '-U0', '--find-copies', '--no-color', '--relative', `${from}${dots}${to}`, '--']);
      return { label: `\`${from}${dots}${to}\``, files: parseUnifiedZero(patch), remainingQuery: remainingQuery.trim() };
    }
    if (!phrased) return null;
    const head = git(root, ['rev-parse', 'HEAD']).trim();
    const branch = defaultBranch(root);
    const base = branch ? git(root, ['merge-base', 'HEAD', branch]).trim() : head;
    const label = base === head
      ? 'uncommitted edits'
      : `since the merge base with \`${branch}\` (\`${base.slice(0, 8)}\`), plus uncommitted edits`;
    const files = parseUnifiedZero(git(root, ['diff', '-U0', '--find-copies', '--no-color', '--relative', base, '--']));
    for (const p of git(root, ['ls-files', '--others', '--exclude-standard']).split('\n')) {
      if (p) files.push({ path: p, ranges: [[1, Number.MAX_SAFE_INTEGER]] });
    }
    return { label, files, remainingQuery: remainingQuery.trim() };
  } catch {
    return null;
  }
}

const SYMBOL_KINDS = new Set<string>([
  'function', 'method', 'class', 'interface', 'struct', 'union', 'trait', 'protocol', 'enum',
  'type_alias', 'component', 'constant', 'variable', 'property', 'field', 'route', 'module', 'namespace',
]);
const CALLABLE_KINDS = new Set<string>(['function', 'method', 'component']);

const span = (n: Node) => (n.endLine ?? n.startLine) - n.startLine;
const inside = (inner: Node, outer: Node) =>
  inner.id !== outer.id && inner.startLine >= outer.startLine && (inner.endLine ?? inner.startLine) <= (outer.endLine ?? outer.startLine);

/**
 * The innermost indexed symbols each hunk touches. A hunk inside a method names
 * the method, not its class; a local inside a function never stands in for the
 * function; a hunk between members (a class header, a new field) names what it
 * lands in.
 */
export function symbolsForRanges(nodesInFile: Node[], ranges: Array<[number, number]>): Node[] {
  const symbols = nodesInFile.filter((n) => SYMBOL_KINDS.has(n.kind) && n.startLine > 0);
  const callables = symbols.filter((n) => CALLABLE_KINDS.has(n.kind));
  const candidates = symbols.filter((n) => CALLABLE_KINDS.has(n.kind) || !callables.some((c) => inside(n, c)));
  const picked = new Map<string, Node>();
  for (const [start, end] of ranges) {
    const touched = candidates.filter((n) => n.startLine <= end && (n.endLine ?? n.startLine) >= start);
    for (const n of touched) {
      if (!touched.some((m) => inside(m, n))) picked.set(n.id, n);
    }
  }
  return [...picked.values()].sort((a, b) => a.startLine - b.startLine || span(a) - span(b));
}
