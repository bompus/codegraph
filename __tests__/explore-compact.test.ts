/**
 * Compact explore output: a map is the default so a session does not keep
 * ~10k tokens of source per call resident for every later turn; snippets keep
 * the first lines of each file, and source is unchanged. The map is for
 * deciding where to look — codegraph_node (or mode "source") returns a body.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Node } from '../src/types';
import { compactExploreText, compactSymbolRows, getStaticTools, injectCompactSymbolTables, resolveExploreMode, ToolHandler } from '../src/mcp/tools';

const RENDERED = [
  '**Exploration: Widget**',
  '',
  'Found 1 symbol across 2 files.',
  '',
  '**Blast radius — what depends on these (update/verify before editing)**',
  '',
  '- `A` (a.ts:1) — 1 caller',
  '- `B` (b.ts:2) — 1 caller',
  '- `C` (c.ts:3) — 1 caller',
  '- `D` (d.ts:4) — 1 caller',
  '- `E` (e.ts:5) — 1 caller',
  '- `F` (f.ts:6) — 1 caller',
  '- `G` (g.ts:7) — 1 caller',
  '',
  '**Relationships**',
  '',
  '**calls:**',
  '- A → B',
  '- A → C',
  '- A → D',
  '- A → E',
  '- A → F',
  '',
  '**extends:**',
  '- A → Base',
  '',
  '**imports:**',
  '- A → util',
  '',
  '**references:**',
  '- A → type',
  '',
  '**Source Code**',
  '',
  '> The code below is the verbatim source.',
  '',
  '**`a.ts`** — A(calls)',
  '',
  '```typescript',
  '1\tfunction a() {',
  '2\t  return 1;',
  '3\t}',
  '4\tconst late = 2;',
  '5\tconst later = 3;',
  '6\tconst latest = 4;',
  '7\tconst tail = 5;',
  '8\tconst end = 6;',
  '9\tconst more = 7;',
  '10\tconst extra = 8;',
  '11\tconst beyond = 9;',
  '12\tconst twelve = 10;',
  '13\tconst thirteen = 11;',
  '```',
  '',
  '**`b.ts`** — B',
  '',
  '```typescript',
  '1\tfunction b() { return 2; }',
  '```',
  '',
  '---',
  '> **Verbatim source for 2 files is included above.**',
].join('\n');

let tmpDir: string;
beforeEach(() => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-explore-mode-'));
});
afterEach(() => {
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

describe('compactExploreText', () => {
  it('map keeps the summary, bounded context and file headers, and drops source', () => {
    const out = compactExploreText(RENDERED, 'map');
    expect(out).toContain('Found 1 symbol across 2 files.');
    expect(out).toContain('**Blast radius');
    expect(out).toContain('- `F`');
    expect(out).not.toContain('- `G`');
    expect(out).toContain('**calls:**');
    expect(out).toContain('- A → E');
    expect(out).not.toContain('- A → F');
    // Only three relationship kinds fit, and the fourth kind's entries must
    // not be relabelled under the third.
    expect(out).toContain('**imports:**');
    expect(out).toContain('- A → util');
    expect(out).not.toContain('**references:**');
    expect(out).not.toContain('- A → type');
    expect(out).toContain('**`a.ts`**');
    expect(out).toContain('**`b.ts`**');
    expect(out).not.toContain('function a()');
    expect(out).toContain('call codegraph_node');
    expect(out).toMatch(/Source omitted in compact map mode/);
  });

  it('snippets keeps the first source lines of each file', () => {
    const out = compactExploreText(RENDERED, 'snippets');
    expect(out).toContain('function a()');
    expect(out).toContain('const twelve');
    expect(out).not.toContain('const thirteen');
    expect(out).toContain('...');
    expect(out).toMatch(/Only the first 12 lines of each returned excerpt/);
  });

  it('stays under the map budget on a large response', () => {
    const headers = Array.from({ length: 200 }, (_, i) => `**\`f${i}.ts\`** — symbol`).join('\n');
    const big = RENDERED.replace('**Source Code**', `**Source Code**\n\n${headers}`);
    const out = compactExploreText(big, 'map');
    expect(out.length).toBeLessThanOrEqual(4000 + 200);
    expect(out).toContain('compact response truncated');
  });
});

describe('compactExploreText promises', () => {
  const TEXT = [
    '**Flow**',
    '',
    '> These sites choose their call target at runtime (registry) — run codegraph_explore or codegraph_node on a candidate; source for the sites above is included below.',
    '',
    '> Source for these symbols follows; gap markers identify any ranges omitted to fit the output budget.',
    '',
    '**Source Code**',
    '',
    '```typescript',
    '1\tconst x = 1;',
    '```',
  ].join('\n');

  it('does not promise source a map does not contain', () => {
    const out = compactExploreText(TEXT, 'map');
    expect(out).not.toContain('included below');
    expect(out).not.toContain('Source for these symbols follows');
    expect(out).toContain('fetch a candidate\'s body with codegraph_node includeCode: true');
    expect(out).toContain('**Source Code** — omitted in map mode');
    expect(out).not.toContain('const x = 1;');
  });

  it('leaves snippets promises alone because the source does follow', () => {
    const out = compactExploreText(TEXT, 'snippets');
    expect(out).toContain('included below');
    expect(out).toContain('Source for these symbols follows');
    expect(out).toContain('**Source Code**\n');
    expect(out).toContain('const x = 1;');
  });
});

describe('compactExploreText sections', () => {
  it('preserves graph sections that follow documentation source', () => {
    const text = [
      '**Exploration: README runNode**',
      '',
      'Found 2 symbols across 2 files.',
      '',
      '**Source Code**',
      '',
      '**`README.md`** — sections',
      '',
      '```markdown',
      '1\t# Title',
      '```',
      '',
      '**`a.ts`** — runNode(function)',
      '',
      '```typescript',
      '1\tfunction runNode() {}',
      '```',
      '',
      '**Blast radius — what depends on these (update/verify before editing)**',
      '',
      '- `runNode` (a.ts:1) — 1 caller',
      '',
      '**Relationships**',
      '',
      '**calls:**',
      '- A → B',
      '',
    ].join('\n');
    const out = compactExploreText(text, 'map');
    expect(out).toContain('**Blast radius');
    expect(out).toContain('- `runNode` (a.ts:1) — 1 caller');
    expect(out).toContain('**Relationships**');
    expect(out).toContain('- A → B');
    expect(out).toContain('**`a.ts`**');
  });

  it('ends the blast limit before the session-history section', () => {
    const text = [
      '**Blast radius — what depends on these (update/verify before editing)**',
      '',
      ...Array.from({ length: 7 }, (_, i) => `- b${i}`),
      '',
      '**Discussed in earlier sessions** (codegraph_sessions with the name reads them)',
      '',
      '- `runNode`: claude (2026-11-01)',
      '',
      '**Relationships**',
      '',
      '**calls:**',
      '- A → B',
    ].join('\n');
    const out = compactExploreText(text, 'map');
    expect(out).toContain('- b5');
    expect(out).not.toContain('- b6');
    expect(out).toContain('1 more blast-radius entry');
    expect(out).toContain('- `runNode`: claude (2026-11-01)');
  });

  it('reports relationship entries and kinds the compact limits omit', () => {
    const out = compactExploreText(RENDERED, 'map');
    expect(out).toContain('2 more relationship entries, 1 more relationship kind');
  });

  it('bounds the echoed query before the map budget is applied', () => {
    const query = 'x'.repeat(500);
    const out = compactExploreText(`**Exploration: ${query}**\n\nFound 1 symbol across 1 file.`, 'map');
    expect(out.split('\n')[0]!.length).toBeLessThanOrEqual(175);
    expect(out).toContain('…');
  });

  it('only names codegraph_node when the client can call it', () => {
    const served = compactExploreText(RENDERED, 'map', true);
    expect(served).toContain('codegraph_node');
    const unserved = compactExploreText(RENDERED, 'map', false);
    expect(unserved).not.toContain('codegraph_node');
    expect(unserved).toContain('mode: "source"');
    // Snippets keeps the source, so it keeps the "included below" promise but
    // must still not name a tool the client cannot call.
    const snippets = compactExploreText(
      '> These sites choose their call target at runtime; source for the sites above is included below.',
      'snippets', false,
    );
    expect(snippets).not.toContain('codegraph_node');
    expect(snippets).toContain('included below');
  });
});

describe('compact symbol rows', () => {
  const node = {
    id: 'n1', name: 'runNode', kind: 'method', startLine: 1349, language: 'typescript',
    filePath: 'a.ts', signature: 'async runNode(workflow: Workflow)',
  } as unknown as Node;

  it('names the location and signature before the body is dropped', () => {
    expect(compactSymbolRows([node], 6)).toEqual([
      '- `runNode` (method) :1349 — async runNode(workflow: Workflow)',
    ]);
  });

  it('inserts the rows under the matching file header', () => {
    const text = '**`a.ts`** — runNode(function)\n```typescript\n1\tfunction runNode() {}\n```';
    expect(injectCompactSymbolTables(text, new Map([['a.ts', ['- `runNode` (method) :1349']]])))
      .toBe('**`a.ts`** — runNode(function)\n- `runNode` (method) :1349\n```typescript\n1\tfunction runNode() {}\n```');
  });
});

describe('resolveExploreMode', () => {
  it('falls back to the product default and reads codegraph.json', () => {
    expect(resolveExploreMode(undefined, tmpDir, {})).toBe('map');
    fs.writeFileSync(path.join(tmpDir, 'codegraph.json'), JSON.stringify({ explore: { mode: 'snippets' } }));
    expect(resolveExploreMode(undefined, tmpDir, {})).toBe('snippets');
  });

  it('prefers the per-call value, then the environment, then the project file', () => {
    fs.writeFileSync(path.join(tmpDir, 'codegraph.json'), JSON.stringify({ explore: { mode: 'source' } }));
    expect(resolveExploreMode(undefined, tmpDir, { CODEGRAPH_EXPLORE_MODE: 'snippets' })).toBe('snippets');
    expect(resolveExploreMode('map', tmpDir, { CODEGRAPH_EXPLORE_MODE: 'snippets' })).toBe('map');
    expect(resolveExploreMode('bogus', tmpDir, {})).toBe('source');
    // The launching session's override beats the daemon's inherited env.
    expect(resolveExploreMode(undefined, tmpDir, { CODEGRAPH_EXPLORE_MODE: 'source' }, 'map')).toBe('map');
    expect(resolveExploreMode('source', tmpDir, { CODEGRAPH_EXPLORE_MODE: 'map' }, 'source')).toBe('source');
  });
});

describe('the default surface follows the explore mode', () => {
  const ENV = 'CODEGRAPH_EXPLORE_MODE';
  const original = process.env[ENV];
  afterEach(() => {
    if (original === undefined) delete process.env[ENV];
    else process.env[ENV] = original;
  });

  const listed = () => new ToolHandler(null).getTools().map((t) => t.name).sort();

  it('serves node beside explore on the product default', () => {
    delete process.env[ENV];
    expect(listed()).toEqual(['codegraph_explore', 'codegraph_node', 'codegraph_sessions']);
    delete process.env.CODEGRAPH_MCP_TOOLS;
    // The pre-project proxy surface must agree, or the agent sees node listed
    // and then cannot find it after the project opens.
    expect(getStaticTools().map((t) => t.name)).toContain('codegraph_node');
  });

  it('keeps node off the source-mode surface', () => {
    process.env[ENV] = 'source';
    expect(listed()).toEqual(['codegraph_explore', 'codegraph_sessions']);
    expect(getStaticTools().map((t) => t.name)).not.toContain('codegraph_node');
    process.env[ENV] = 'snippets';
    expect(listed()).toContain('codegraph_node');
  });

  it('lets an explicit allowlist replace the mode-derived surface', () => {
    process.env[ENV] = 'map';
    process.env.CODEGRAPH_MCP_TOOLS = 'explore';
    try {
      expect(listed()).toEqual(['codegraph_explore']);
    } finally {
      delete process.env.CODEGRAPH_MCP_TOOLS;
    }
  });
});
