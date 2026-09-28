/**
 * Fetch a pinned real repository for the evaluation runners
 * (precision-runner.ts, search-baseline-runner.ts).
 */
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type { PrecisionCorpus } from './edge-cases.js';

// A git hook or `git bisect run` exports GIT_DIR and friends naming the ENGINE
// repository. Inherited, every git call inside the corpus reads the engine
// instead: the pinned-commit check below saw the engine's HEAD and fetched the
// corpus commit into the engine checkout, and the indexer listed the engine's
// files. This is `git rev-parse --local-env-vars`.
export function scrubGitEnv(): void {
  for (const v of [
    'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_CONFIG', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG_COUNT',
    'GIT_OBJECT_DIRECTORY', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_IMPLICIT_WORK_TREE', 'GIT_GRAFT_FILE',
    'GIT_INDEX_FILE', 'GIT_NO_REPLACE_OBJECTS', 'GIT_REPLACE_REF_BASE', 'GIT_PREFIX', 'GIT_SHALLOW_FILE',
    'GIT_COMMON_DIR',
  ]) delete process.env[v];
}

export function sh(cmd: string, args: string[], cwd?: string, quiet = false): string {
  return execFileSync(cmd, args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', quiet ? 'ignore' : 'inherit'] }).trim();
}

export function fetchPinned(corpus: PrecisionCorpus, repoDir: string): void {
  if (!fs.existsSync(path.join(repoDir, '.git'))) {
    fs.mkdirSync(repoDir, { recursive: true });
    sh('git', ['init', '-q'], repoDir);
    sh('git', ['remote', 'add', 'origin', corpus.repo], repoDir);
  }
  // Fetch and a forced checkout follow, so they must land in the corpus and
  // nowhere else: refuse a corpus directory git resolves to another repository.
  const top = sh('git', ['rev-parse', '--show-toplevel'], repoDir, true);
  if (fs.realpathSync(top) !== fs.realpathSync(repoDir)) {
    throw new Error(`${repoDir} resolves to the git repository at ${top}, not its own; refusing to fetch or check out there`);
  }
  const head = (() => { try { return sh('git', ['rev-parse', 'HEAD'], repoDir, true); } catch { return ''; } })();
  if (!head.startsWith(corpus.commit)) {
    console.log(`fetching ${corpus.repo} @ ${corpus.commit}`);
    sh('git', ['fetch', '-q', '--depth', '1', 'origin', corpus.commit], repoDir);
    sh('git', ['checkout', '-q', '--force', 'FETCH_HEAD'], repoDir);
  }
}
