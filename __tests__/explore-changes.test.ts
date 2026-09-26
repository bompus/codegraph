/**
 * A change question ("what do my changes affect?", `main..HEAD`) is answered
 * from the diff: explore seeds itself with the symbols the hunks touch and
 * leads with who calls them. Any other query is left alone.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { execFileSync } from 'child_process';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';
import { parseUnifiedZero, symbolsForRanges } from '../src/mcp/explore-changes';
import type { Node } from '../src/types';

const git = (cwd: string, ...args: string[]) =>
  execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@t', '-c', 'commit.gpgsign=false', ...args], {
    cwd, stdio: 'pipe', env: { ...process.env, GIT_TEMPLATE_DIR: '/usr/share/git-core/templates' },
  });

describe('parseUnifiedZero', () => {
  it('reads new-side ranges and marks pure deletions at the line they follow', () => {
    const patch = [
      'diff --git a/src/a.ts b/src/a.ts',
      '--- a/src/a.ts',
      '+++ b/src/a.ts',
      '@@ -3,2 +3,3 @@ function x() {',
      '@@ -10 +11 @@',
      '@@ -20,4 +21,0 @@',
      'diff --git a/gone.ts b/gone.ts',
      '--- a/gone.ts',
      '+++ /dev/null',
      '@@ -1,5 +0,0 @@',
    ].join('\n');
    expect(parseUnifiedZero(patch)).toEqual([{ path: 'src/a.ts', ranges: [[3, 5], [11, 11], [21, 21]] }]);
  });
});

describe('symbolsForRanges', () => {
  const node = (id: string, kind: string, startLine: number, endLine: number) =>
    ({ id, kind, name: id, startLine, endLine, filePath: 'a.ts' }) as unknown as Node;
  const nodes = [
    node('Cls', 'class', 1, 30),
    node('method', 'method', 5, 12),
    node('local', 'variable', 6, 6),
    node('field', 'property', 3, 3),
  ];

  it('names the innermost member, never a local for its function', () => {
    expect(symbolsForRanges(nodes, [[6, 6]]).map((n) => n.id)).toEqual(['method']);
    expect(symbolsForRanges(nodes, [[3, 3]]).map((n) => n.id)).toEqual(['field']);
    expect(symbolsForRanges(nodes, [[20, 21]]).map((n) => n.id)).toEqual(['Cls']);
  });
});

describe('codegraph_explore answers change questions from the diff', () => {
  let dir: string;
  let cg: CodeGraph;

  const explore = async (query: string) => {
    const result = await new ToolHandler(cg).execute('codegraph_explore', { query });
    return result.content?.[0]?.type === 'text' ? result.content[0].text : '';
  };

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-explore-changes-'));
    fs.mkdirSync(path.join(dir, 'src'));
    fs.writeFileSync(path.join(dir, 'src/lib.ts'), [
      'export function pricing(n: number): number {',
      '  return n * 2;',
      '}',
      '',
      'export function unrelated(): string {',
      "  return 'x';",
      '}',
      '',
    ].join('\n'));
    fs.writeFileSync(path.join(dir, 'src/checkout.ts'), [
      "import { pricing } from './lib';",
      '',
      'export function checkout(total: number): number {',
      '  return pricing(total);',
      '}',
      '',
    ].join('\n'));
    fs.writeFileSync(path.join(dir, '.gitignore'), '.codegraph/\n');
    git(dir, 'init', '-q', '-b', 'main');
    git(dir, 'add', '.');
    git(dir, 'commit', '-q', '-m', 'base');
    git(dir, 'switch', '-q', '-c', 'feature');
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
  });

  afterEach(() => {
    try { cg.close(); } catch { /* ignore */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('seeds from the merge base plus uncommitted edits and leads with callers', async () => {
    const lib = path.join(dir, 'src/lib.ts');
    fs.writeFileSync(lib, fs.readFileSync(lib, 'utf-8').replace('n * 2', 'n * 3'));
    const text = await explore('what do my changes affect?');
    expect(text).toContain('**Changes (uncommitted edits) — 1 file, 1 symbol');
    expect(text).toMatch(/- `pricing` \(src\/lib\.ts:1\) — \d+ callers? in `src\/checkout\.ts`/);
    expect(text).not.toMatch(/- `unrelated`/);
  });

  it('diffs a revision range the query names', async () => {
    const lib = path.join(dir, 'src/lib.ts');
    fs.writeFileSync(lib, fs.readFileSync(lib, 'utf-8').replace("'x'", "'y'"));
    git(dir, 'commit', '-q', '-am', 'change unrelated');
    await cg.sync();
    const text = await explore('main..HEAD');
    expect(text).toContain('**Changes (`main..HEAD`) — 1 file, 1 symbol');
    expect(text).toMatch(/- `unrelated` \(src\/lib\.ts:5\) — no callers/);
    expect(text).not.toMatch(/- `pricing`/);

    const branch = await explore('review this branch');
    expect(branch).toContain('since the merge base with `main`');
    expect(branch).toMatch(/- `unrelated` \(src\/lib\.ts:5\) — no callers/);
  });

  it('leaves ordinary queries alone', async () => {
    const text = await explore('how does checkout reach pricing...');
    expect(text).not.toContain('**Changes');
    expect(text).toContain('pricing');
  });
});
