#!/usr/bin/env node
// Build one diff-impact fixture from a real commit.
//
// Usage: diff-impact-fixture.mjs <case-id> <clone-dir> <out-dir> [cases.json]
//   <clone-dir>  full-history clone of the case's repo (history is read here,
//                never copied into the fixture)
//   <out-dir>    receives repo/ (the fixture) and truth.json
//
// The fixture is the commit's parent exported without history, committed once,
// with ONE file (`primary`) brought to its post-commit state as an uncommitted
// edit. Without history the agent cannot read the answer out of `git log` or
// `git show`. truth.json lists the other code files the real commit changed,
// split into source and tests: the files a change to `primary` is expected to
// pull along. Files the commit added are left out, since the fixture does not
// contain them for an agent to name. See diff-impact-cases.json for what that proxy does and does not
// mean.
import { execFileSync } from 'node:child_process';
import { appendFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

function git(cwd, args, opts = {}) {
  return execFileSync('git', args, { cwd, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, ...opts });
}

/** The code files a commit changed, minus `primary`, split into source and tests. */
export function expectedFiles(changed, primary, codePatterns, testPattern) {
  const code = codePatterns.map((p) => new RegExp(p));
  const test = new RegExp(testPattern);
  const files = changed.filter((f) => f !== primary && code.some((re) => re.test(f)));
  return {
    source: files.filter((f) => !test.test(f)).sort(),
    tests: files.filter((f) => test.test(f)).sort(),
  };
}

export function buildFixture(spec, codePatterns, cloneDir, outDir) {
  const parent = git(cloneDir, ['rev-parse', `${spec.sha}^`]).trim();
  const full = git(cloneDir, ['rev-parse', spec.sha]).trim();
  const repo = join(outDir, 'repo');
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(repo, { recursive: true });

  const archive = execFileSync('git', ['archive', parent], { cwd: cloneDir, maxBuffer: 512 * 1024 * 1024 });
  execFileSync('tar', ['-x', '-C', repo], { input: archive });
  const id = ['-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid'];
  git(repo, ['init', '-q']);
  // The index the arms build must not show up as one of the "uncommitted changes".
  appendFileSync(join(repo, '.git', 'info', 'exclude'), '.codegraph/\n');
  git(repo, ['add', '-A']);
  git(repo, [...id, 'commit', '-q', '-m', 'base']);

  // The uncommitted edit: the primary file as the real commit left it.
  writeFileSync(join(repo, spec.primary), execFileSync('git', ['show', `${full}:${spec.primary}`], {
    cwd: cloneDir,
    maxBuffer: 64 * 1024 * 1024,
  }));

  const changed = git(cloneDir, ['diff', '--name-only', '--no-renames', '--diff-filter=MD', parent, full]).split('\n').filter(Boolean);
  const truth = {
    id: spec.id,
    sha: full,
    parent,
    primary: spec.primary,
    ...expectedFiles(changed, spec.primary, codePatterns, spec.testPattern),
  };
  writeFileSync(join(outDir, 'truth.json'), JSON.stringify(truth, null, 2) + '\n');
  return { repo, truth };
}

if (import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [id, cloneDir, outDir, casesPath = join(here, 'diff-impact-cases.json')] = process.argv.slice(2);
  if (!id || !cloneDir || !outDir) {
    console.error('usage: diff-impact-fixture.mjs <case-id> <clone-dir> <out-dir> [cases.json]');
    process.exit(2);
  }
  const cases = JSON.parse(readFileSync(casesPath, 'utf8'));
  const spec = cases.cases.find((c) => c.id === id);
  if (!spec) {
    console.error(`no case "${id}" in ${casesPath}`);
    process.exit(2);
  }
  const { repo, truth } = buildFixture(spec, cases.codeExtensions[spec.language], resolve(cloneDir), resolve(outDir));
  console.log(repo);
  console.error(`truth: ${truth.source.length} source, ${truth.tests.length} test file(s)`);
}
