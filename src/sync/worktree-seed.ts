/**
 * Seed a new git worktree's index from a sibling worktree's index.
 *
 * Worktrees of one repository share their commits, and the index stores only
 * repo-relative paths plus the commit it was built at. So a snapshot of a
 * sibling's index, brought current by a normal sync, is this worktree's index:
 * the sync asks git what changed since that commit, compares content hashes
 * and re-parses only the files that differ. On a large repository that is
 * seconds instead of a full index.
 *
 * A sibling qualifies only when its index is complete, was built by the
 * running extraction version and has a schema this build can open. Among
 * those, the one whose commit is fewest changed files away wins; ties go to
 * the main checkout, which `git worktree list` names first.
 */

import { execFileSync } from 'child_process';
import * as fs from 'fs';
import { getDatabasePath } from '../db';
import { createDatabase, type SqliteDatabase } from '../db/sqlite-adapter';
import { CURRENT_SCHEMA_VERSION } from '../db/migrations';
import { EXTRACTION_VERSION } from '../extraction/extraction-version';
import { gitWorktreeRoot, gitWorktreeRoots } from './worktree';

export interface SeedSource {
  /** The sibling worktree whose index is copied. */
  root: string;
  /** The commit that index was built at. */
  commit: string;
  /** Files that differ between that commit and this worktree's HEAD. */
  changedFiles: number;
}

/** Commit and compatibility of a sibling's index, or null when it can't seed. */
function seedableCommit(dbPath: string): string | null {
  let db: SqliteDatabase | null = null;
  try {
    db = createDatabase(dbPath, { readOnly: true }).db;
    const meta = new Map<string, string>();
    for (const row of db.prepare('SELECT key, value FROM project_metadata').all() as { key: string; value: string }[]) {
      meta.set(row.key, row.value);
    }
    const schema = (db.prepare('SELECT MAX(version) AS v FROM schema_versions').get() as { v: number | null }).v ?? 0;
    if (meta.get('index_state') !== 'complete') return null;
    if (meta.get('indexed_with_extraction_version') !== String(EXTRACTION_VERSION)) return null;
    if (schema > CURRENT_SCHEMA_VERSION) return null;
    return meta.get('indexed_at_commit') || null;
  } catch {
    return null;
  } finally {
    db?.close();
  }
}

/** Files changed between `commit` and HEAD in `root`, or null if git can't say. */
function changedFilesSince(root: string, commit: string): number | null {
  try {
    const out = execFileSync('git', ['diff', '--name-only', commit, 'HEAD'], {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
      timeout: 10000,
      maxBuffer: 64 * 1024 * 1024,
    });
    return out.split('\n').filter(Boolean).length;
  } catch {
    return null;
  }
}

/** The best sibling index to seed `projectRoot` from, or null. */
export function findSeedSource(projectRoot: string): SeedSource | null {
  const root = gitWorktreeRoot(projectRoot);
  // Only a worktree root: a subdirectory index covers a different file set.
  if (!root || root !== fs.realpathSync(projectRoot)) return null;
  let best: SeedSource | null = null;
  for (const sibling of gitWorktreeRoots(root)) {
    if (sibling === root) continue;
    const dbPath = getDatabasePath(sibling);
    if (!fs.existsSync(dbPath)) continue;
    const commit = seedableCommit(dbPath);
    if (!commit) continue;
    const changedFiles = changedFilesSince(root, commit);
    if (changedFiles === null) continue;
    if (!best || changedFiles < best.changedFiles) best = { root: sibling, commit, changedFiles };
  }
  return best;
}

/**
 * Write a consistent snapshot of `source`'s index to `projectRoot`'s database
 * path. `VACUUM INTO` reads under one transaction, so a daemon writing the
 * sibling's index at the same time is safe. The `.codegraph/` directory must
 * already exist and hold no database.
 */
export function copySeedIndex(source: SeedSource, projectRoot: string): void {
  const target = getDatabasePath(projectRoot);
  const { db } = createDatabase(getDatabasePath(source.root), { readOnly: true });
  try {
    db.prepare('VACUUM INTO ?').run(target);
  } finally {
    db.close();
  }
}
