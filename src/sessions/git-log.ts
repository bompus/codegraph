/**
 * Commit messages as session docs. A commit body is often the one place a
 * decision was written down ("keep X because Y"), and the session that made it
 * may be gone or on another machine. The last `GIT_LOG_MAX` commits reachable
 * from HEAD are one record, re-read when HEAD moves.
 */
import { execFileSync } from 'child_process';
import type { SessionDoc } from './claude-code';

const GIT_LOG_MAX = 2000;
const FIELD = '\x1f';
const RECORD = '\x1e';

function git(root: string, args: string[]): string | null {
  try {
    return execFileSync('git', args, {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      maxBuffer: 64 * 1024 * 1024,
    });
  } catch {
    return null;
  }
}

/**
 * HEAD's commit time (ms) and hash, the record's change signature; null outside
 * a git checkout or in a repository with no commits.
 */
export function gitHead(root: string): { mtime: number; sha: string } | null {
  const out = git(root, ['log', '-1', '--format=%ct %H'])?.trim();
  if (!out) return null;
  const [ct, sha] = out.split(' ');
  return { mtime: Number(ct) * 1000, sha: sha! };
}

/** One doc per commit: `<short sha> <subject>` then the body, role `commit`. */
export function gitCommitDocs(root: string): SessionDoc[] {
  const out = git(root, ['log', `-n${GIT_LOG_MAX}`, `--format=%h${FIELD}%aI${FIELD}%s${FIELD}%b${RECORD}`]);
  if (!out) return [];
  const docs: SessionDoc[] = [];
  for (const rec of out.split(RECORD)) {
    const [sha, ts, subject, body] = rec.replace(/^\n/, '').split(FIELD);
    if (!sha || !ts) continue;
    const text = `${sha} ${subject ?? ''}${body?.trim() ? `\n\n${body.trim()}` : ''}`;
    docs.push({ ts: new Date(ts).toISOString(), role: 'commit', text });
  }
  return docs;
}
