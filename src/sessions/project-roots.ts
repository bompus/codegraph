/**
 * Which checkouts count as "this project" for session transcripts: the
 * requested root and every `git worktree` of the same repository.
 */
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';

export function resolveExisting(p: string): string | null {
  try {
    return fs.realpathSync(p);
  } catch {
    return null;
  }
}

export function projectWorktreeRoots(projectRoot: string): string[] {
  const resolved = resolveExisting(projectRoot) ?? path.resolve(projectRoot);
  const roots = new Set<string>([resolved]);
  try {
    const out = execFileSync('git', ['worktree', 'list', '--porcelain'], {
      cwd: projectRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    for (const line of out.split('\n')) {
      if (!line.startsWith('worktree ')) continue;
      const dir = resolveExisting(line.slice('worktree '.length).trim());
      if (dir) roots.add(dir);
    }
  } catch {
    // Not a git checkout — the resolved root is enough.
  }
  return [...roots];
}

/** True when `cwd` is a project worktree or a directory inside one. */
export function cwdBelongsToProject(cwd: string, projectRoot: string): boolean {
  const resolved = resolveExisting(cwd);
  if (!resolved) return false;
  for (const root of projectWorktreeRoots(projectRoot)) {
    if (resolved === root || resolved.startsWith(root + path.sep)) return true;
  }
  return false;
}
