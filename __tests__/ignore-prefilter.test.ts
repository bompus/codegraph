import { describe, expect, it } from 'vitest';
import ignore from 'ignore';
import { withNoMatchFastPath } from '../src/extraction/ignore-prefilter';

const RULES = [
  'node_modules/',
  'dist',
  '*.log',
  '!keep.log',
  '/root-only.ts',
  'docs/**/generated/',
  '!docs/api/generated/',
  'Temp*/',
  'a/**/b',
];

const PATHS = [
  'src/index.ts',
  'node_modules/x/index.js',
  'packages/a/node_modules/y.js',
  'packages/dist/index.ts',
  'dist/',
  'err.log',
  'keep.log',
  'deep/keep.log',
  'root-only.ts',
  'nested/root-only.ts',
  'docs/guide/generated/page.md',
  'docs/api/generated/page.md',
  'TEMPDIR/file.ts',
  'tempdir/file.ts',
  'a/x/y/b',
  'a/b',
  'src/',
];

describe('ignore no-match fast path', () => {
  it('answers every path the way the stock matcher does', () => {
    for (const ignorecase of [true, false]) {
      const stock = ignore({ ignorecase }).add(RULES);
      const fast = withNoMatchFastPath(ignore({ ignorecase }).add(RULES));
      for (const p of PATHS) expect([p, fast.ignores(p)]).toEqual([p, stock.ignores(p)]);
    }
  });

  it('picks up rules added after it is installed', () => {
    const fast = withNoMatchFastPath(ignore().add('dist'));
    expect(fast.ignores('src/a.ts')).toBe(false);
    fast.add('src/');
    expect(fast.ignores('src/a.ts')).toBe(true);
  });

  it('is active on the installed ignore version', () => {
    // The fast path reads ignore's rule list; a release that reshapes it
    // turns the fast path off silently, which this makes visible.
    const ig = ignore();
    const before = (ig as unknown as { _rules: { test: unknown } })._rules.test;
    withNoMatchFastPath(ig);
    expect((ig as unknown as { _rules: { test: unknown } })._rules.test).not.toBe(before);
  });
});
