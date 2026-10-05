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
import { lstatSync, readFileSync, readlinkSync } from 'node:fs';
import { sep } from 'node:path';

function git(root, args, rejectStderr = false) {
  const r = spawnSync('git', args, { cwd: root, encoding: 'buffer', maxBuffer: 256 * 1024 * 1024 });
  return r.status === 0 && (!rejectStderr || r.stderr.length === 0) ? r.stdout : null;
}

/** The kernel source stamp, or null outside a committed Git checkout. */
export function kernelSourceStamp(root) {
  const tree = git(root, ['rev-parse', 'HEAD:codegraph-kernel']);
  if (!tree) return null;
  const stamp = tree.toString().trim();
  // Git can exit zero after skipping unreadable directories; reject its warnings.
  const untracked = git(root, ['ls-files', '--others', '--exclude-standard', '-z', '--', 'codegraph-kernel'], true);
  if (untracked === null) throw new Error('Cannot enumerate untracked kernel sources');
  const status = git(root, ['status', '--porcelain', '--', 'codegraph-kernel']);
  if (status === null) throw new Error('Cannot read kernel source status');
  if (status.length === 0 && untracked.length === 0) return stamp;
  const diff = git(root, ['diff', 'HEAD', '--binary', '--', 'codegraph-kernel']);
  if (diff === null) throw new Error('Cannot read kernel source diff');
  const hash = createHash('sha256').update(status).update(diff);
  // Keep Git's raw, NUL-separated paths, including newlines and non-ASCII bytes.
  const paths = [];
  for (let start = 0, end; (end = untracked.indexOf(0, start)) !== -1; start = end + 1) {
    paths.push(untracked.subarray(start, end));
  }
  paths.sort(Buffer.compare);
  for (const name of paths) {
    const file = Buffer.concat([Buffer.from(`${root}${sep}`), name]);
    const stat = lstatSync(file);
    const link = stat.isSymbolicLink();
    if (!link && !stat.isFile()) throw new Error(`Unsupported kernel source input: ${name}`);
    // Git stores link text, not external target bytes; dangling links are valid.
    const content = link ? readlinkSync(file, { encoding: 'buffer' }) : readFileSync(file);
    hash.update('\0').update(name).update('\0').update(link ? 'l' : 'f');
    hash.update(createHash('sha256').update(content).digest());
  }
  return `${stamp}+${hash.digest('hex').slice(0, 16)}`;
}

/** Where the stamp for a staged prebuild lives. */
export function stampPath(prebuild) {
  return `${prebuild}.stamp`;
}
