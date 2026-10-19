/**
 * A per-user cache of release kernel builds, keyed by kernel source stamp.
 *
 * A fresh worktree otherwise rebuilds the kernel (about two minutes and
 * 600 MB of `target/`) even when its kernel sources are byte-identical to a
 * build made a minute ago in another worktree. `build-kernel.mjs` saves each
 * release host build here and `ensure-kernel.mjs` and `build-kernel.mjs`
 * restore from it before compiling.
 *
 * Only builds of committed sources are cached (a stamp with no `+` digest),
 * and only release builds, so a cached kernel is exactly what a release build
 * of that tree produces. The newest few entries per platform are kept.
 *
 * Location: `CODEGRAPH_KERNEL_CACHE`, else `$XDG_CACHE_HOME/codegraph/kernel`
 * (`%LOCALAPPDATA%` on Windows, `~/.cache` otherwise). Set
 * `CODEGRAPH_KERNEL_CACHE=off` to disable it.
 */
import { copyFileSync, existsSync, mkdirSync, readdirSync, renameSync, rmSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import * as path from 'node:path';
import { stampPath } from './kernel-stamp.mjs';

const KEEP = 4;

function cacheRoot() {
  const env = process.env.CODEGRAPH_KERNEL_CACHE;
  if (env === 'off' || env === '0') return null;
  if (env) return path.resolve(env);
  const base = process.env.XDG_CACHE_HOME
    || (process.platform === 'win32' ? process.env.LOCALAPPDATA : undefined)
    || path.join(homedir(), '.cache');
  return path.join(base, 'codegraph', 'kernel');
}

function entry(platform, stamp) {
  const root = cacheRoot();
  if (!root || !stamp || stamp.includes('+')) return null;
  return path.join(root, platform, stamp, 'codegraph-kernel.node');
}

/** Stage the cached release build for `stamp` at `dest`; true when it was there. */
export function restoreKernel(platform, stamp, dest) {
  const cached = entry(platform, stamp);
  if (!cached || !existsSync(cached)) return false;
  const partial = `${dest}.${process.pid}.partial`;
  try {
    mkdirSync(path.dirname(dest), { recursive: true });
    // Copy beside the prebuild first: if another worktree prunes the entry
    // meanwhile, the current prebuild is still there and the caller compiles.
    copyFileSync(cached, partial);
    // A fresh inode, as build-kernel.mjs does: macOS caches code signatures per inode.
    rmSync(dest, { force: true });
    renameSync(partial, dest);
    writeFileSync(stampPath(dest), `${stamp}\n`);
    const now = new Date();
    utimesSync(path.dirname(cached), now, now); // most recently used survives pruning
    return true;
  } catch (err) {
    rmSync(partial, { force: true });
    console.error(`[kernel] warning: could not restore from the kernel cache: ${err.message}`);
    return false;
  }
}

/** Save a release build of committed sources, then keep only the newest entries. */
export function saveKernel(platform, stamp, built) {
  const cached = entry(platform, stamp);
  if (!cached) return;
  const partial = `${cached}.${process.pid}.partial`;
  try {
    const dir = path.dirname(cached);
    mkdirSync(dir, { recursive: true });
    // Copy then rename, so a concurrent restore never reads a half-written file.
    copyFileSync(built, partial);
    renameSync(partial, cached);
    const platformDir = path.dirname(dir);
    const entries = readdirSync(platformDir, { withFileTypes: true })
      .filter((d) => d.isDirectory())
      .map(({ name }) => ({ name, mtime: statSync(path.join(platformDir, name)).mtimeMs }))
      .sort((a, b) => b.mtime - a.mtime);
    for (const old of entries.slice(KEEP)) rmSync(path.join(platformDir, old.name), { recursive: true, force: true });
  } catch (err) {
    rmSync(partial, { force: true });
    console.error(`[kernel] warning: could not save the build to the kernel cache: ${err.message}`);
  }
}
