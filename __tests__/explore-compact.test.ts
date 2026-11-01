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
import { compactExploreText, getStaticTools, resolveExploreMode, ToolHandler } from '../src/mcp/tools';

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
    expect(out).toMatch(/Only the first 12 source lines/);
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
