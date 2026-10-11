import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

// @ts-expect-error plain .mjs harness script, no type declarations
import { buildFixture, expectedFiles } from '../scripts/agent-eval/diff-impact-fixture.mjs';
// @ts-expect-error plain .mjs harness script, no type declarations
import { finalAnswer, namedFiles, scoreAnswer } from '../scripts/agent-eval/diff-impact-score.mjs';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

const git = (cwd: string, ...args: string[]) =>
  execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@example.invalid', ...args], { cwd, encoding: 'utf8' });

describe('diff-impact expected files', () => {
  it('drops the primary file and non-code files, and splits source from tests', () => {
    const got = expectedFiles(
      ['lib/a.js', 'lib/b.js', 'test/b.js', 'History.md', 'package.json'],
      'lib/a.js',
      ['\\.js$'],
      '^test/',
    );
    expect(got).toEqual({ source: ['lib/b.js'], tests: ['test/b.js'] });
  });
});

describe('diff-impact answer scoring', () => {
  const files = ['context.go', 'logger.go', 'render/json.go', 'render/render_test.go', 'a/util.go', 'b/util.go'];
  const truth = {
    id: 't',
    primary: 'context.go',
    source: ['logger.go'],
    tests: ['render/render_test.go'],
  };

  it('counts a full path, and a base name only when it is unique in the repo', () => {
    const answer = 'Update `logger.go` and render/render_test.go. Also util.go and a/util.go.';
    expect(namedFiles(answer, files, 'context.go')).toEqual(['a/util.go', 'logger.go', 'render/render_test.go']);
  });

  it('never counts the primary file and does not match inside a longer name', () => {
    expect(namedFiles('context.go and my_logger.go.bak', files, 'context.go')).toEqual([]);
  });

  it('reports recall, a precision floor and the missed files', () => {
    const score = scoreAnswer('logger.go and render/json.go', truth, files);
    expect(score.sourceRecall).toBe(1);
    expect(score.testRecall).toBe(0);
    expect(score.recall).toBe(0.5);
    expect(score.precisionFloor).toBe(0.5);
    expect(score.missed).toEqual(['render/render_test.go']);
  });

  it('reads the last successful result event as the answer', () => {
    const jsonl = [
      JSON.stringify({ type: 'result', subtype: 'success', result: 'first' }),
      'not json',
      JSON.stringify({ type: 'result', subtype: 'error_max_turns', result: 'ignored' }),
      JSON.stringify({ type: 'result', subtype: 'success', result: 'last' }),
    ].join('\n');
    expect(finalAnswer(jsonl)).toBe('last');
    expect(finalAnswer('')).toBeNull();
  });
});

describe('diff-impact fixture', () => {
  it('exports the parent without history and leaves only the primary file edited', () => {
    const root = mkdtempSync(join(tmpdir(), 'codegraph-diff-impact-'));
    roots.push(root);
    const clone = join(root, 'clone');
    mkdirSync(join(clone, 'lib'), { recursive: true });
    mkdirSync(join(clone, 'test'), { recursive: true });
    git(clone, 'init', '-q');
    writeFileSync(join(clone, 'lib/a.js'), 'exports.a = 1;\n');
    writeFileSync(join(clone, 'lib/b.js'), 'exports.b = 1;\n');
    writeFileSync(join(clone, 'test/b.js'), 'exports.b = 1;\n');
    git(clone, 'add', '-A');
    git(clone, 'commit', '-q', '-m', 'parent');
    writeFileSync(join(clone, 'lib/a.js'), 'exports.a = 2;\n');
    writeFileSync(join(clone, 'lib/b.js'), 'exports.b = 2;\n');
    writeFileSync(join(clone, 'test/b.js'), 'exports.b = 2;\n');
    git(clone, 'add', '-A');
    git(clone, 'commit', '-q', '-m', 'change');
    const sha = git(clone, 'rev-parse', 'HEAD').trim();

    const out = join(root, 'out');
    const { repo, truth } = buildFixture(
      { id: 'x', sha, primary: 'lib/a.js', testPattern: '^test/' },
      ['\\.js$'],
      clone,
      out,
    );

    expect(truth.source).toEqual(['lib/b.js']);
    expect(truth.tests).toEqual(['test/b.js']);
    expect(readFileSync(join(repo, 'lib/a.js'), 'utf8')).toBe('exports.a = 2;\n');
    expect(readFileSync(join(repo, 'lib/b.js'), 'utf8')).toBe('exports.b = 1;\n');
    expect(git(repo, 'status', '--short').trim()).toBe('M lib/a.js');
    mkdirSync(join(repo, '.codegraph'));
    writeFileSync(join(repo, '.codegraph', 'x.db'), '');
    expect(git(repo, 'status', '--short').trim()).toBe('M lib/a.js');
    expect(git(repo, 'rev-list', '--count', 'HEAD').trim()).toBe('1');
    expect(existsSync(resolve(out, 'truth.json'))).toBe(true);
  });
});
