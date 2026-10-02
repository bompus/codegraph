/**
 * Which checkouts count as "this project" for session transcripts: the
 * requested root, every `git worktree` of the same repository, and every
 * worktree root the session index has seen before (a removed worktree's
 * transcripts still belong to the project; see `SessionsIndex.knownRoots`).
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

/**
 * How long one `git worktree list` answer is reused. A query asks for the
 * roots once per candidate session (hundreds of Codex and OpenCode sessions),
 * and each spawn costs a few milliseconds; worktrees change far less often.
 */
const ROOTS_TTL_MS = 10_000;
const rootsCache = new Map<string, { at: number; roots: string[] }>();

/** Test hook: forget cached `git worktree list` answers. */
export function clearProjectRootsCache(): void {
  rootsCache.clear();
}

export function projectWorktreeRoots(projectRoot: string): string[] {
  const resolved = resolveExisting(projectRoot) ?? path.resolve(projectRoot);
  const cached = rootsCache.get(resolved);
  if (cached && Date.now() - cached.at < ROOTS_TTL_MS) return cached.roots;
  const roots = new Set<string>([resolved]);
  try {
    const out = execFileSync('git', ['worktree', 'list', '--porcelain'], {
      cwd: projectRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
    });
    for (const line of out.split('\n')) {
      if (!line.startsWith('worktree ')) continue;
      const dir = resolveExisting(line.slice('worktree '.length).trim());
      if (dir) roots.add(dir);
    }
  } catch {
    // Not a git checkout — the resolved root is enough.
  }
  const list = [...roots];
  rootsCache.set(resolved, { at: Date.now(), roots: list });
  return list;
}

/**
 * True when `cwd` is one of `roots` or a directory inside one. A cwd that no
 * longer exists (its worktree was removed) is compared as written, so its
 * transcripts still match a remembered root.
 */
export function cwdInRoots(cwd: string, roots: readonly string[]): boolean {
  const resolved = resolveExisting(cwd) ?? path.resolve(cwd);
  return roots.some((root) => resolved === root || resolved.startsWith(root + path.sep));
}

/** True when `cwd` is a project worktree or a directory inside one. */
export function cwdBelongsToProject(cwd: string, projectRoot: string): boolean {
  return cwdInRoots(cwd, projectWorktreeRoots(projectRoot));
}
