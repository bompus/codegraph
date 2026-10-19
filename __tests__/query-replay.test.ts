import { spawnSync, execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
// @ts-expect-error untyped ESM script
import { callDirectory, callsInLine, repoSlug } from '../scripts/agent-eval/harvest-queries.mjs';
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

  it('counts different text with the same files and length as a change', () => {
    const r = compareRow(row(['a.ts']), row(['a.ts']), '', false);
    expect(r).toMatchObject({ regressed: false, changed: true });
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
    expect(callsInLine({ payload: { type: 'custom_tool_call', input } })).toEqual([{ query: 'D "e"', projectPath: '/y', maxFiles: 8 }]);
    expect(callsInLine({ message: { content: [{ type: 'tool_use', name: 'Read', input: { query: 'no' } }] } })).toEqual([]);
  });

  it('reads code-mode calls with braces, single quotes and file limits', () => {
    const call = (input: string) => callsInLine({ payload: { type: 'custom_tool_call', input } });
    expect(call('codegraph_explore({query:"make({a: 1}) } brace"})')).toEqual([{ query: 'make({a: 1}) } brace', projectPath: undefined }]);
    expect(call("codegraph_explore({ query: 'auth flow', maxFiles: 12 })")).toEqual([{ query: 'auth flow', projectPath: undefined, maxFiles: 12 }]);
    expect(call('codegraph_explore({query:`a ${x}`})')).toEqual([]);
    expect(callsInLine({ message: { content: [{ type: 'tool_use', name: 'mcp__codegraph__codegraph_explore', input: { query: 'Q', maxFiles: 3 } }] } }))
      .toEqual([{ query: 'Q', projectPath: undefined, maxFiles: 3 }]);
  });

  it('never credits a call with an unreadable or relative project to the wrong repository', () => {
    const call = (input: string) => callsInLine({ payload: { type: 'custom_tool_call', input } })[0];
    const variable = call('const p = "/private"; codegraph_explore({query:"Q", projectPath: p})');
    expect(variable).toEqual({ query: 'Q', projectPath: null });
    expect(callDirectory(variable, '/public')).toBeNull();
    expect(callDirectory(call('codegraph_explore({query:"Q", projectPath: join(a, "b")})'), '/public')).toBeNull();
    expect(callDirectory(call('codegraph_explore({query:"Q", projectPath: `${root}/x`})'), '/public')).toBeNull();
    expect(callDirectory({ query: 'Q', projectPath: 'sub/dir' }, '/session')).toBe(resolve('/session', 'sub/dir'));
    expect(callDirectory({ query: 'Q', projectPath: 'sub/dir' }, undefined)).toBeNull();
    expect(callDirectory({ query: 'Q', projectPath: 7 }, '/public')).toBeNull();
    expect(callDirectory({ query: 'Q', projectPath: undefined }, '/session')).toBe('/session');
    expect(callDirectory({ query: 'Q', projectPath: '/abs' }, '/session')).toBe('/abs');
  });
  it('harvests only calls whose project it can read', () => {
    const root = mkdtempSync(join(tmpdir(), 'harvest-'));
    try {
      const repo = join(root, 'repo');
      execFileSync('git', ['init', '-q', repo]);
      execFileSync('git', ['-C', repo, 'remote', 'add', 'origin', 'https://github.com/pub/repo.git']);
      const explore = (input: string) => JSON.stringify({ cwd: repo, payload: { type: 'custom_tool_call', input } });
      writeFileSync(join(root, 's.jsonl'), [
        explore('codegraph_explore({query:"kept"})'),
        explore('codegraph_explore({query:"unreadable", projectPath: p})'),
      ].join('\n'));
      const script = resolve(import.meta.dirname, '../scripts/agent-eval/harvest-queries.mjs');
      const run = spawnSync(process.execPath, [script, '--public', 'https://github.com/pub/repo', join(root, 's.jsonl')], { encoding: 'utf8' });
      expect(run.status).toBe(0);
      expect(JSON.parse(run.stdout).repos['pub/repo'].tasks.map((t: { prompt: string }) => t.prompt)).toEqual(['kept']);
      expect(run.stderr).toContain('kept 1, duplicates 0, dropped 1');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
  it('maps GitHub remotes to owner/repo', () => {
    expect(repoSlug('git@github.com:Owner/Repo.git\n')).toBe('owner/repo');
    expect(repoSlug('https://github.com/owner/repo')).toBe('owner/repo');
    expect(repoSlug('ssh://git@github.com/owner/repo.git')).toBe('owner/repo');
    expect(repoSlug('https://gitlab.com/owner/repo.git')).toBeNull();
    expect(repoSlug('https://notgithub.com/owner/repo.git')).toBeNull();
    expect(repoSlug('git@privategithub.com:owner/repo.git')).toBeNull();
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
