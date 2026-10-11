/**
 * Experimental cross-project session search (src/sessions/fanout.ts): each
 * project keeps its own session index, the search runs per project and the
 * answers are merged. Synthetic projects only; transcripts sit where Claude
 * Code keeps them (CLAUDE_CONFIG_DIR/projects/<slug>).
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { ToolHandler, __setLoadCodeGraphForTests } from '../src/mcp/tools';
import { clearProjectConfigCache } from '../src/project-config';
import { claudeProjectSlug } from '../src/sessions/claude-code';
import { querySessions } from '../src/sessions';
import { formatCrossProjectHits, MAX_PROJECTS, querySessionsAcross, type ProjectTarget } from '../src/sessions/fanout';

const at = '2026-09-04T20:00:00.000Z';
const dirs: string[] = [];
const savedEnv = { ...process.env };

const fixtureDir = (): string => {
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-fanout-')));
  dirs.push(d);
  return d;
};

/** A project root with a `.codegraph/` and one transcript per entry of `prompts`. */
function project(config: string, name: string, prompts: string[]): string {
  const root = path.join(fixtureDir(), name);
  fs.mkdirSync(path.join(root, '.codegraph'), { recursive: true });
  if (prompts.length > 0) {
    const dir = path.join(config, 'projects', claudeProjectSlug(root));
    fs.mkdirSync(dir, { recursive: true });
    prompts.forEach((text, i) => {
      const file = path.join(dir, `${name}-${i}.jsonl`);
      fs.writeFileSync(file, JSON.stringify({ type: 'user', timestamp: at, message: { content: text } }) + '\n');
      fs.utimesSync(file, 1_700_000_000 + i, 1_700_000_000 + i);
    });
  }
  return root;
}

const roots = (...r: string[]): ProjectTarget[] => r.map((root) => ({ root }));

afterEach(() => {
  (globalThis as { Bun?: { gc?: (force: boolean) => void } }).Bun?.gc?.(true);
  for (const d of dirs.splice(0)) fs.rmSync(d, { recursive: true, force: true });
  for (const k of ['CODEGRAPH_SESSIONS_DIR', 'CLAUDE_CONFIG_DIR']) {
    if (savedEnv[k] === undefined) delete process.env[k];
    else process.env[k] = savedEnv[k];
  }
  clearProjectConfigCache();
  __setLoadCodeGraphForTests(null);
});

const words = (n: number) => `We settled the sprocket handling for case ${n} by keeping the flange; sprocket flange decision recorded.`;

describe('querySessionsAcross', () => {
  it('interleaves by each project\'s own rank and labels every hit with its project', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const a = project(config, 'alpha', [words(1), words(2), words(3)]);
    const b = project(config, 'beta', [words(4), words(5)]);
    const result = querySessionsAcross(roots(a, b), 'sprocket flange', { limit: 10 });
    expect(result.hits.map((h) => path.basename(h.project))).toEqual(['alpha', 'beta', 'alpha', 'beta', 'alpha']);
    expect(result.projects.map((p) => [p.name, p.hits, p.files])).toEqual([['alpha', 3, 3], ['beta', 2, 2]]);
    const text = formatCrossProjectHits('sprocket flange', result);
    expect(text).toContain('across 2 projects (experimental) — 5 hits');
    expect(text).toContain('## alpha · claude:alpha-');
    expect(text).toContain('## beta · claude:beta-');
  });

  it('applies one limit across the projects', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const a = project(config, 'alpha', [words(1), words(2), words(3)]);
    const b = project(config, 'beta', [words(4), words(5), words(6)]);
    const result = querySessionsAcross(roots(a, b), 'sprocket flange', { limit: 3 });
    expect(result.hits.map((h) => path.basename(h.project))).toEqual(['alpha', 'beta', 'alpha']);
    expect(result.projects.map((p) => p.hits)).toEqual([2, 1]);
  });

  it('lists a project that cannot answer as skipped and still answers from the rest', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const a = project(config, 'alpha', [words(1)]);
    const empty = project(config, 'empty', []);
    const off = project(config, 'off', [words(2)]);
    fs.writeFileSync(path.join(off, 'codegraph.json'), JSON.stringify({ sessions: false }));
    const missing: ProjectTarget = { path: '/nonexistent/project', skipped: 'CodeGraph is not initialized there' };
    const result = querySessionsAcross([...roots(a, empty, off), missing], 'sprocket flange');
    expect(result.hits).toHaveLength(1);
    expect(result.projects.filter((p) => p.skipped).map((p) => p.name)).toEqual(['empty', 'off', 'project']);
    const text = formatCrossProjectHits('sprocket flange', result);
    expect(text).toContain('across 1 project (experimental)');
    expect(text).toContain('empty');
    expect(text).toMatch(/off .*: skipped, no session transcripts, or sessions are turned off/);
  });

  it('searches at most five projects and ignores a repeated one', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const all = Array.from({ length: 7 }, (_, i) => project(config, `p${i}`, [words(i)]));
    const result = querySessionsAcross(roots(all[0]!, all[0]!, ...all.slice(1)), 'sprocket flange', { limit: 20 });
    expect(result.projects).toHaveLength(MAX_PROJECTS);
    expect(result.dropped).toBe(2);
    expect(result.hits).toHaveLength(MAX_PROJECTS);
    expect(formatCrossProjectHits('sprocket flange', result)).toContain('2 more were left out');
  });

  it('keeps the per-project indexes apart: each holds only its own transcripts', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const a = project(config, 'alpha', [words(1)]);
    const b = project(config, 'beta', [words(2), words(3)]);
    querySessionsAcross(roots(a, b), 'sprocket flange');
    const alone = querySessions(a, 'sprocket flange');
    expect(alone.index).toEqual({ files: 1, refreshed: 0, docs: 0 });
    expect(alone.hits.map((h) => h.session)).toEqual(['claude:alpha-0']);
    expect(querySessions(b, 'sprocket flange').index.refreshed).toBe(0);
  });

  it('spends one byte budget across projects', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const long = (n: number) => `sprocket flange ${n} ${'x'.repeat(3500)}`;
    const a = project(config, 'alpha', [long(1), long(2), long(3)]);
    const b = project(config, 'beta', [long(4), long(5)]);
    const separately = [a, b].map((p) => querySessions(p, 'sprocket flange', { full: true }).fullCut);
    expect(separately).toEqual([0, 0]);
    const result = querySessionsAcross(roots(a, b), 'sprocket flange', { full: true });
    expect(result.hits.filter((h) => h.text !== undefined)).toHaveLength(4);
    expect(result.fullCut).toBe(1);
    expect(formatCrossProjectHits('sprocket flange', result)).toContain('across projects: the last 1 hit');
  });
});

describe('querySessionsAcross review cases', () => {
  it('counts passages cut by the shared budget, not by a per-project one', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const long = (n: number) => `sprocket flange ${n} ${'x'.repeat(3500)}`;
    const a = project(config, 'alpha', [1, 2, 3, 4, 5].map(long));
    const result = querySessionsAcross(roots(a), 'sprocket flange', { full: true, limit: 5 });
    expect(result.hits).toHaveLength(5);
    expect(result.hits.filter((h) => h.text !== undefined)).toHaveLength(4);
    expect(result.fullCut).toBe(1);
  });

  it('names a project by its root when two listed projects share a directory name', () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const a = project(config, 'app', [words(1)]);
    const b = project(config, 'app', [words(2)]);
    expect(path.basename(a)).toBe(path.basename(b));
    const text = formatCrossProjectHits('sprocket flange', querySessionsAcross(roots(a, b), 'sprocket flange'));
    expect(text).toContain(`## ${a} · claude:app-0`);
    expect(text).toContain(`## ${b} · claude:app-0`);
  });
});

describe('codegraph_sessions projects argument', () => {
  it('searches the listed projects, reports an unindexed one as skipped, and leaves the single-project path alone', async () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const a = project(config, 'alpha', [words(1)]);
    const b = project(config, 'beta', [words(2)]);
    for (const root of [a, b]) {
      fs.mkdirSync(path.join(root, 'src'), { recursive: true });
      fs.writeFileSync(path.join(root, 'src', 'a.ts'), 'export const a = 1;\n');
      fs.rmSync(path.join(root, '.codegraph'), { recursive: true, force: true });
      const cg = CodeGraph.initSync(root);
      await cg.indexAll();
      cg.close();
    }
    const unindexed = fixtureDir();
    __setLoadCodeGraphForTests(CodeGraph);
    const handler = new ToolHandler(null);
    const res = await handler.execute('codegraph_sessions', { query: 'sprocket flange', projects: [a, b, unindexed] });
    const text = res.content[0]!.text;
    expect(res.isError).toBeFalsy();
    expect(text).toContain('across 2 projects (experimental) — 2 hits');
    expect(text).toContain('## alpha · claude:alpha-0');
    expect(text).toContain('## beta · claude:beta-0');
    expect(text).toMatch(/skipped/);
    const single = await handler.execute('codegraph_sessions', { query: 'sprocket flange', projectPath: a });
    expect(single.content[0]!.text).toMatch(/^Sessions matching "sprocket flange" — 1 hit across 1 transcript/);
    expect(single.content[0]!.text).not.toContain('experimental');
  });
  it('answers a projects-only call with no default project, even when none of them is indexed, and bounds the list', async () => {
    __setLoadCodeGraphForTests(CodeGraph);
    const handler = new ToolHandler(null);
    const schema = handler.getTools().find((t) => t.name === 'codegraph_sessions')!;
    expect(schema.inputSchema.required).toEqual(['query']);
    const none = Array.from({ length: 7 }, (_, i) => path.join(fixtureDir(), `none-${i}`));
    const res = await handler.execute('codegraph_sessions', { query: 'sprocket flange', projects: none });
    const text = res.content[0]!.text;
    expect(res.isError).toBeFalsy();
    expect(text).toContain('across 0 projects (experimental) — 0 hits');
    expect(text.match(/: skipped, /g)).toHaveLength(MAX_PROJECTS);
    expect(text).toContain('2 more were left out');
  });
});

describe('codegraph sessions --project', () => {
  it('searches the listed projects from the shell and marks an unindexed one as skipped', async () => {
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    const a = project(config, 'alpha', [words(1)]);
    const b = project(config, 'beta', [words(2)]);
    for (const root of [a, b]) {
      fs.rmSync(path.join(root, '.codegraph'), { recursive: true, force: true });
      const cg = CodeGraph.initSync(root);
      await cg.indexAll();
      cg.close();
    }
    const unindexed = fixtureDir();
    const run = (...args: string[]) =>
      execFileSync(process.execPath, [path.resolve(__dirname, '../dist/bin/codegraph.js'), 'sessions', ...args], {
        encoding: 'utf-8',
        env: { ...process.env, CODEGRAPH_NO_DAEMON: '1' },
        stdio: ['ignore', 'pipe', 'ignore'],
      });
    const out = run('sprocket', 'flange', '--project', a, b, unindexed);
    expect(out).toContain('across 2 projects (experimental) — 2 hits');
    expect(out).toContain('## alpha · claude:alpha-0');
    expect(out).toContain('## beta · claude:beta-0');
    expect(out).toContain('skipped, CodeGraph is not initialized there');
    const json = JSON.parse(run('sprocket', 'flange', '--project', a, b, '--json'));
    expect(json.projects.map((p: { name: string }) => p.name)).toEqual(['alpha', 'beta']);
  });
});
