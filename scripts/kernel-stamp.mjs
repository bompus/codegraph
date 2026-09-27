/**
 * Which kernel sources a prebuild was built from.
 *
 * `build-kernel.mjs` writes this stamp next to the staged `.node`, and
 * `ensure-kernel.mjs` compares it before the test suite runs, so a prebuild
 * left over from older kernel sources (an earlier checkout, or one copied from
 * another worktree) is rebuilt instead of failing tests far from the cause.
 *
 * The stamp is git's tree hash of `codegraph-kernel/`, which costs nothing to
 * read even over the vendored grammars, plus a digest of any uncommitted
 * changes there. Outside a git checkout there is no stamp (null).
 */
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';

function git(root, args) {
  const r = spawnSync('git', args, { cwd: root, encoding: 'buffer', maxBuffer: 256 * 1024 * 1024 });
  return r.status === 0 ? r.stdout : null;
}

/** The stamp for the kernel sources under `root`, or null when git cannot tell. */
export function kernelSourceStamp(root) {
  const tree = git(root, ['rev-parse', 'HEAD:codegraph-kernel']);
  if (!tree) return null;
  const stamp = tree.toString().trim();
  // Uncommitted edits: the diff against HEAD, plus the names of untracked files.
  const status = git(root, ['status', '--porcelain', '--', 'codegraph-kernel']);
  if (status === null) return null;
  if (status.length === 0) return stamp;
  const diff = git(root, ['diff', 'HEAD', '--binary', '--', 'codegraph-kernel']);
  if (diff === null) return null;
  return `${stamp}+${createHash('sha256').update(status).update(diff).digest('hex').slice(0, 16)}`;
}

/** Where the stamp for a staged prebuild lives. */
export function stampPath(prebuild) {
  return `${prebuild}.stamp`;
}
