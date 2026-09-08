/**
 * Git Worktree Awareness
 *
 * A CodeGraph index lives in a `.codegraph/` directory and is resolved by
 * walking up parent directories to the nearest one (see
 * `findNearestCodeGraphRoot`). That walk is unaware of git worktrees: when a
 * worktree is created *inside* the main checkout (e.g. some tools place them
 * under `.gitignore`d paths like `.claude/worktrees/<name>/`), a command run
 * from the worktree walks up and silently resolves the MAIN checkout's index.
 *
 * Every query then returns results from the main tree's code — usually a
 * different branch — rather than the worktree the user is actually editing.
 * Symbols added or changed only in the worktree are invisible. This module
 * detects that "borrowed index" situation so callers can warn about it.
 *
 * Detection is best-effort: when git is unavailable or the path isn't a repo,
 * it reports "no mismatch" and callers carry on unchanged.
 */

import * as fs from 'fs';
import * as path from 'path';
import { execFileSync } from 'child_process';

/**
 * Absolute, symlink-resolved toplevel of the git working tree that `dir`
 * belongs to, or null when `dir` isn't inside a git repo (or git is missing).
 *
 * `git rev-parse --show-toplevel` returns the per-worktree root: the main
 * checkout and each linked worktree report their own distinct directory, which
 * is exactly the distinction this module relies on.
 */
export function gitWorktreeRoot(dir: string): string | null {
  try {
    const out = execFileSync('git', ['rev-parse', '--show-toplevel'], {
      cwd: dir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
      // Bounded like every git call in extraction/: this runs (memoized) on
      // the daemon's main event loop, where an unbounded hang would trip the
      // 60s liveness watchdog and SIGKILL a healthy daemon (#1139).
      timeout: 5000,
    }).trim();
    return out ? realpath(out) : null;
  } catch {
    return null;
  }
}

/**
 * Absolute, symlink-resolved git **common** directory for `dir` — the shared
 * `.git` that all worktrees of one repository point at. Linked worktrees of the
 * same repo report the SAME common dir; a submodule or an embedded clone is a
 * DIFFERENT repository and reports its own (`…/.git/modules/<name>` or its own
 * `.git`). That distinction is what separates a genuine "borrowed worktree"
 * from a nested repo the parent index already covers. Null when not a repo.
 */
export function gitCommonDir(dir: string): string | null {
  try {
    const out = execFileSync('git', ['rev-parse', '--git-common-dir'], {
      cwd: dir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
      timeout: 5000, // same rationale as gitWorktreeRoot
    }).trim();
    if (!out) return null;
    // `--git-common-dir` is relative to cwd unless already absolute.
    return realpath(path.isAbsolute(out) ? out : path.resolve(dir, out));
  } catch {
    return null;
  }
}

/**
 * Every working-tree root of the repository `dir` belongs to — the main
 * checkout and each linked worktree — or an empty array outside git.
 *
 * `--porcelain` is the stable form: the human output pads paths into columns
 * and appends `[branch]`, so it cannot be split reliably on a path containing
 * spaces, which Windows paths routinely do.
 */
export function gitWorktreeRoots(dir: string): string[] {
  try {
    const out = execFileSync('git', ['worktree', 'list', '--porcelain'], {
      cwd: dir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
      timeout: 5000, // same rationale as gitWorktreeRoot
    });
    return out
      .split('\n')
      .filter((line) => line.startsWith('worktree '))
      .map((line) => realpath(line.slice('worktree '.length).trim()));
  } catch {
    return [];
  }
}

export interface UnindexedWorktree {
  /** The git working tree the command was run from; it has no index. */
  worktreeRoot: string;
  /** Sibling working trees of the SAME repository that do have one. */
  indexedSiblings: string[];
}

/**
 * Detect the inverse of {@link detectWorktreeIndexMismatch}: `startPath` is a
 * git working tree with **no** index of its own, while a sibling worktree of
 * the same repository has one.
 *
 * That case is silent today and the silence is costly. A worktree nested inside
 * the main checkout walks *up* and borrows its index, which
 * `detectWorktreeIndexMismatch` catches; a worktree placed **outside** it — how
 * `git worktree add ../name` and most agent tooling lay them out — resolves no
 * index at all. The server then sends its generic no-root-index instructions,
 * which tell the agent to use its own Read/Grep tools instead. So the agent is
 * not working around a gap, it is following our instruction, and nothing in the
 * session says an index for this tree is one command away (#1236).
 *
 * Returns null — meaning "nothing to say" — when `startPath` is outside git,
 * when it already has its own index, or when no sibling worktree has one.
 *
 * **Two git spawns, whatever the repository's size.** This runs on the MCP
 * handshake path, which answers before any heavy init (#172), so cost here is
 * latency every session pays. `git worktree list` already enumerates only the
 * trees of *this* repository, so no per-sibling common-dir check is needed to
 * exclude a submodule or embedded clone — they are separate repositories and
 * never appear in it. Filtering each candidate instead would have cost one more
 * spawn per worktree, which on an eleven-worktree repo is fourteen rather than
 * two. Measured on that repo: ~70 ms, paid once per session and only when the
 * tree has no index of its own.
 */
export function detectUnindexedWorktree(
  startPath: string,
  isIndexed: (root: string) => boolean,
): UnindexedWorktree | null {
  const worktreeRoot = gitWorktreeRoot(startPath);
  if (!worktreeRoot) return null;
  if (isIndexed(worktreeRoot)) return null;

  const indexedSiblings = gitWorktreeRoots(worktreeRoot)
    .filter((root) => root !== worktreeRoot)
    .filter(isIndexed);

  return indexedSiblings.length > 0 ? { worktreeRoot, indexedSiblings } : null;
}

/**
 * Instructions shown when this working tree has no index but a sibling does.
 *
 * It names `codegraph init` and stops short of telling the agent to fall back
 * to its own tools, which is the line the generic variant carries and the one
 * that makes this case invisible. Borrowing the sibling's index is not offered:
 * a single graph cannot represent two branches at once (#155), and its results
 * would be that branch's code with anything changed only here missing.
 */
export function unindexedWorktreeNotice(u: UnindexedWorktree): string {
  const sibling = u.indexedSiblings[0];
  const more =
    u.indexedSiblings.length > 1 ? ` (and ${u.indexedSiblings.length - 1} more)` : '';
  return (
    `This git worktree has no CodeGraph index, but a sibling worktree of the same ` +
    `repository does: ${sibling}${more}. Run "codegraph init" here to index this tree — ` +
    `results then reflect this branch and its uncommitted work. Codegraph will not answer ` +
    `from the sibling's index: that is a different branch, and symbols changed only here ` +
    `would be missing.`
  );
}

export interface WorktreeIndexMismatch {
  /** The git working tree the command was run from. */
  worktreeRoot: string;
  /** The (different) working tree whose `.codegraph` index is being used. */
  indexRoot: string;
}

/**
 * Detect when `startPath` lives in one git working tree but the resolved
 * CodeGraph index (`indexRoot`) belongs to a *different* working tree.
 *
 * Returns null — meaning "nothing to warn about" — when:
 *   - `startPath` isn't in a git repo (or git is unavailable),
 *   - the index already lives in `startPath`'s own working tree, or
 *   - `indexRoot` isn't itself a working-tree root (an unrelated parent dir
 *     that merely happens to contain a `.codegraph/`), which keeps non-git
 *     and monorepo-subdir layouts from producing false warnings.
 */
export function detectWorktreeIndexMismatch(
  startPath: string,
  indexRoot: string,
): WorktreeIndexMismatch | null {
  const worktreeRoot = gitWorktreeRoot(startPath);
  if (!worktreeRoot) return null;

  const resolvedIndexRoot = realpath(indexRoot);
  if (worktreeRoot === resolvedIndexRoot) return null;

  // Only flag it when the index root is itself a real working-tree root. This
  // distinguishes "borrowed another worktree's index" from "index sits in a
  // plain ancestor directory", and avoids warning outside git entirely.
  if (gitWorktreeRoot(resolvedIndexRoot) !== resolvedIndexRoot) return null;

  // Don't flag a nested repo (submodule / embedded clone) that `indexRoot`'s
  // index ALREADY covers: indexing a super-repo descends into its submodules
  // and gitlinked clones, so a query run from inside one resolves up to the
  // parent index — whose graph *does* contain that nested repo's files. The
  // warning's premise ("results are a different branch; symbols changed only
  // here are missing") is false there, and its "run codegraph init -i" advice
  // would needlessly fragment the unified workspace index. A genuine borrowed
  // worktree and the index root are the SAME repository (they share a git
  // common dir); a submodule/embedded clone is a DIFFERENT repository and does
  // not — so suppress only when the two clearly differ. (#1031, #1033)
  const worktreeCommon = gitCommonDir(worktreeRoot);
  const indexCommon = gitCommonDir(resolvedIndexRoot);
  if (worktreeCommon && indexCommon && worktreeCommon !== indexCommon) return null;

  return { worktreeRoot, indexRoot: resolvedIndexRoot };
}

/** One-line-per-fact warning describing a detected mismatch. */
export function worktreeMismatchWarning(m: WorktreeIndexMismatch): string {
  return (
    `This CodeGraph index belongs to a different git working tree.\n` +
    `  Running in: ${m.worktreeRoot}\n` +
    `  Index from: ${m.indexRoot}\n` +
    `Results reflect that tree's code (often a different branch), not this worktree — ` +
    `symbols changed only here are missing. Run "codegraph init -i" in this worktree ` +
    `for a worktree-local index.`
  );
}

/**
 * Compact, single-line variant for prefixing a tool's result. Read tools
 * return their answer inline, so the heads-up has to ride on the same payload
 * the agent is already reading — a multi-line block would bury the result.
 */
export function worktreeMismatchNotice(m: WorktreeIndexMismatch): string {
  return (
    `⚠ CodeGraph results below come from a different git worktree (${m.indexRoot}), ` +
    `not where you're working (${m.worktreeRoot}) — they may reflect another branch, ` +
    `and symbols changed only here are missing. Run "codegraph init -i" here for a ` +
    `worktree-local index.`
  );
}

/** Resolve symlinks where possible so tmp/realpath quirks don't break equality. */
function realpath(p: string): string {
  try {
    return fs.realpathSync(path.resolve(p));
  } catch {
    return path.resolve(p);
  }
}
