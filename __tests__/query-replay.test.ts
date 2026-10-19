import { readdirSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
// @ts-expect-error untyped ESM script
import { callsInLine, repoSlug } from '../scripts/agent-eval/harvest-queries.mjs';
// @ts-expect-error untyped ESM script
import { compareRow } from '../scripts/agent-eval/replay-queries.mjs';

const row = (files: string[], over: Record<string, unknown> = {}) => ({
  id: 'q', chars: 100, sourceAt: 10, files, mustMatchAt: [], ...over,
});

describe('replay comparison', () => {
  it('passes the same files in another order', () => {
    const r = compareRow(row(['a.ts', 'b.ts']), row(['b.ts', 'a.ts']));
    expect(r.regressed).toBe(false);
    expect(r.changed).toBe(false);
  });

  it('flags a lost file, and names it when the query asked for it', () => {
    const r = compareRow(row(['a.ts', 'src/b.ts']), row(['a.ts']), 'explain src/b.ts');
    expect(r.regressed).toBe(true);
    expect(r.lostFiles).toEqual(['src/b.ts']);
    expect(r.lostNamed).toEqual(['src/b.ts']);
  });

  it('flags a lost source section and a pattern the base found', () => {
    expect(compareRow(row(['a.ts']), row([], { sourceAt: -1 })).lostSource).toBe(true);
    expect(compareRow(row([], { mustMatchAt: [5] }), row([], { mustMatchAt: [-1] })).lostMatch).toBe(1);
  });

  it('counts a gained file as a change without loss', () => {
    const r = compareRow(row(['a.ts']), row(['a.ts', 'c.ts']));
    expect(r).toMatchObject({ regressed: false, changed: true, gainedFiles: ['c.ts'] });
  });
});

describe('query harvest', () => {
  it('reads Claude tool_use, Codex function calls and Codex code-mode calls', () => {
    expect(callsInLine({ message: { content: [{ type: 'tool_use', name: 'mcp__codegraph__codegraph_explore', input: { query: 'A b', projectPath: '/x' } }] } }))
      .toEqual([{ query: 'A b', projectPath: '/x' }]);
    expect(callsInLine({ payload: { type: 'function_call', name: 'mcp__codegraph__codegraph_explore', arguments: '{"query":"C"}' } }))
      .toEqual([{ query: 'C', projectPath: undefined }]);
    const input = 'text(await tools.mcp__codegraph__codegraph_explore({projectPath:"/y",query:"D \\"e\\"",maxFiles:8}));';
    expect(callsInLine({ payload: { type: 'custom_tool_call', input } })).toEqual([{ query: 'D "e"', projectPath: '/y' }]);
    expect(callsInLine({ message: { content: [{ type: 'tool_use', name: 'Read', input: { query: 'no' } }] } })).toEqual([]);
  });

  it('maps GitHub remotes to owner/repo', () => {
    expect(repoSlug('git@github.com:Owner/Repo.git\n')).toBe('owner/repo');
    expect(repoSlug('https://github.com/owner/repo')).toBe('owner/repo');
    expect(repoSlug('https://gitlab.com/owner/repo.git')).toBeNull();
  });
});

describe('committed replay corpora', () => {
  const dir = resolve(import.meta.dirname, '../scripts/agent-eval/replay');
  for (const name of readdirSync(dir).filter((f) => f.endsWith('.json'))) {
    it(`${name} is pinned, unique and has no local paths`, () => {
      const text = readFileSync(join(dir, name), 'utf8');
      const corpus = JSON.parse(text);
      expect(corpus.commit).toMatch(/^[0-9a-f]{7,40}$/);
      const ids = corpus.tasks.map((t: { id: string }) => t.id);
      expect(new Set(ids).size).toBe(ids.length);
      expect(text).not.toMatch(/\/home\/|\/Users\/|[A-Z]:\\\\|~\//);
    });
  }
});
