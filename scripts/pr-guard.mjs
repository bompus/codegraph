#!/usr/bin/env node
// Fail when the branch checked out in the current directory adds a file an
// upstream pull request must not carry. Run it before every push to a branch
// meant for colbymchenry/codegraph.
//
//   node <fork>/scripts/pr-guard.mjs [--base upstream/main] [--allow path,path]
//
// Upstream installs with npm and tracks only package-lock.json. Running Bun or
// another package manager in a PR worktree writes one of the lockfiles below,
// and a broad `git add` then ships it unnoticed (it happened on upstream #1699).
// Run it from the fork checkout's path: a branch based on upstream main does
// not contain this script.
import { execFileSync } from 'node:child_process';
import { basename, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const PR_DENIED_FILES = ['bun.lock', 'bun.lockb', 'yarn.lock', 'pnpm-lock.yaml'];

/** Added paths an upstream PR should not carry, minus the ones explicitly allowed. */
export function deniedPrFiles(added, allow = []) {
  return added.filter((f) => PR_DENIED_FILES.includes(basename(f)) && !allow.includes(f));
}

function main() {
  const args = process.argv.slice(2);
  const opt = (name) => {
    const i = args.indexOf(`--${name}`);
    return i >= 0 ? args[i + 1] : undefined;
  };
  const base = opt('base') ?? 'upstream/main';
  const allow = (opt('allow') ?? '').split(',').filter(Boolean);
  let out;
  try {
    out = execFileSync('git', ['diff', '--name-only', '--diff-filter=A', `${base}...HEAD`], { encoding: 'utf8' });
  } catch (error) {
    console.error(`[pr-guard] git diff ${base}...HEAD failed: ${error.stderr?.toString().trim() || error.message}`);
    return 2;
  }
  const added = out.split(/\r?\n/).filter(Boolean);
  const denied = deniedPrFiles(added, allow);
  if (denied.length > 0) {
    console.error(`[pr-guard] branch adds files upstream does not track: ${denied.join(', ')}`);
    console.error('[pr-guard] drop them from the commit that added them, or pass --allow <path> when the PR needs one');
    return 1;
  }
  console.log(`[pr-guard] ok: ${added.length} added file(s) against ${base}, none denied`);
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
