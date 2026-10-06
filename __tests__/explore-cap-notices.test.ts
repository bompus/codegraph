import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';
import { ExploreSessionState } from '../src/mcp/explore-session-state';
import { fileFingerprint } from '../src/mcp/explore-dedup';

let dir: string;
let cg: CodeGraph;
const bigSource = Array.from({ length: 360 }, (_, i) => [
  `export function work${String(i).padStart(4, '0')}() {`,
  `  // independent source ${'x'.repeat(160)}`,
  `  return ${i};`,
  '}',
].join('\n')).join('\n');

async function explore(query: string, extra: Record<string, unknown> = {}): Promise<string> {
  const result = await new ToolHandler(cg).execute('codegraph_explore', { query, ...extra });
  return result.content?.[0]?.text ?? '';
}

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cap-notices-'));
  fs.writeFileSync(path.join(dir, 'big.ts'), bigSource);
  fs.writeFileSync(path.join(dir, 'compact.ts'), Array.from({ length: 360 }, (_, i) => `export const val${i}=${i};`).join(' '));
  fs.writeFileSync(path.join(dir, 'manual.md'), Array.from({ length: 301 }, (_, i) => `## Chapter${String(i).padStart(3, '0')}\n\nParagraph ${i}.\n`).join('\n'));
  for (let j = 0; j < 3; j++) fs.writeFileSync(path.join(dir, `large-doc${j}.md`),
    Array.from({ length: 301 }, (_, i) => `## Section${i}\n\n${i === 300 ? Array.from({ length: 110 }, (_, k) => `Last section row ${k} ${'q'.repeat(45)}`).join('\n') : 'Small paragraph.'}\n`).join('\n'));
  if (process.platform !== 'win32') {
    const nested = path.join(dir, ...Array.from({ length: 14 }, () => 'd'.repeat(200)));
    fs.mkdirSync(nested, { recursive: true });
    for (let i = 0; i < 7; i++) fs.writeFileSync(path.join(nested, `extra${i}.ts`), `export const extraValue${i} = ${i};`);
    fs.writeFileSync(path.join(nested, 'enormous.ts'), bigSource);
  }
  for (let i = 0; i < 10; i++) fs.writeFileSync(path.join(dir, `unit${i}.ts`), `export function uniqueUnit${i}() { return ${i}; }`);
  cg = CodeGraph.initSync(dir);
  await cg.indexAll();
}, 180_000);

afterAll(() => {
  cg?.destroy();
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});

describe('bounded explore exclusion notices', () => {
  it('offers a usable continuation for a pure-path gather tail', async () => {
    const out = await explore('big.ts');
    const pointer = /Additional indexed source not included: `big\.ts:(\d+)-(\d+)`/.exec(out);
    expect(pointer).not.toBeNull();
    expect(Number(pointer![1])).toBeGreaterThan(1000);
    expect(out.length).toBeLessThanOrEqual(19500);
    const continued = await explore(`big.ts:${pointer![1]}-${pointer![2]}`);
    const firstLine = bigSource.split('\n')[Number(pointer![1]) - 1]!;
    expect(continued).toContain(firstLine.trim());
  });

  it('suppresses gather warnings when the complete file covers more than300 nodes', async () => {
    expect(cg.getNodesInFile('compact.ts').filter(n => !['file', 'import', 'export'].includes(n.kind)).length).toBeGreaterThan(300);
    const out = await explore('compact.ts');
    expect(out).toContain('export const val359=359;');
    expect(out).not.toContain('Additional indexed source not included');
  });

  it('retains named source beyond the gather cap', async () => {
    const out = await explore('big.ts work0359');
    expect(out).toContain('return 359;');
  });

  it('names overflow references without adding source file slots', async () => {
    const out = await explore('unit0.ts unit1.ts unit1.ts', { maxFiles: 1 });
    expect(out).toContain("Requested files not pinned within this call's initial file limit: `unit1.ts`");
    expect(out.match(/\*\*`unit[01]\.ts`/g)?.length).toBe(1);
    expect(out).not.toContain('No indexed file uniquely matches');
  });

  it('keeps capacity notices off at the exact file limit', async () => {
    const out = await explore('unit0.ts unit1.ts', { maxFiles: 2 });
    expect(out).toContain('uniqueUnit0');
    expect(out).toContain('uniqueUnit1');
    expect(out).not.toContain('Requested files not pinned');
  });

  it('reports unexamined references beyond the scan budget', async () => {
    const out = await explore(Array.from({ length: 10 }, (_, i) => `unit${i}.ts`).join(' '), { maxFiles: 1 });
    expect(out).toContain('Further path-like references were not examined');
    const notice = out.split("Requested files not pinned within this call's initial file limit: ")[1]!.split('. Explore')[0]!;
    expect(notice).toContain('unit7.ts');
    expect(notice).not.toContain('unit8.ts');
    expect(out.length).toBeLessThanOrEqual(19500);
  });

  it('recognizes the selected Markdown section as delivered past the gather cap', async () => {
    const out = await explore('manual.md Chapter300');
    expect(out).toContain('Paragraph 300.');
    expect(out).not.toContain('Additional indexed source not included');
  });

  it('reports an unexamined path even when no relevant source was found', async () => {
    const out = await explore(Array.from({ length: 9 }, (_, i) => `zzmissing${i}.zzz`).join(' '));
    expect(out).toContain('No relevant code found');
    expect(out).toContain('Further path-like references were not examined');
  });

  it.runIf(process.platform !== 'win32')('bounds long path notices while retaining pinned source', async () => {
    // POSIX allows this fixture below its4096-byte path limit. Windows long-path
    // configuration is outside this retrieval test's contract.
    const out = await explore('unit0.ts ' + Array.from({ length: 7 }, (_, i) => `extra${i}.ts`).join(' '), { maxFiles: 1 });
    expect(out).toContain('uniqueUnit0');
    expect(out).toContain("Requested files not pinned within this call's initial file limit:");
    expect(out).toContain('(7 more)');
    expect(out.length).toBeLessThanOrEqual(19500);
  });

  it.runIf(process.platform !== 'win32')('keeps gather guidance usable when its full path label cannot fit', async () => {
    const out = await explore('enormous.ts');
    expect(out).toContain('Additional indexed source not included: (1 more)');
    expect(out).toContain('Explore fewer requested files or narrower line ranges');
    expect(out).not.toContain('Explore these ranges to continue');
    expect(out.length).toBeLessThanOrEqual(19500);
  });

  it('reports a capped section dropped at the final output boundary', async () => {
    const out = await explore('large-doc0.md large-doc1.md large-doc2.md Section300', { maxFiles: 3 });
    expect(out).toContain('output truncated to budget');
    const missing = /Additional indexed source not included: `([^`]+):(\d+)-(\d+)`/.exec(out);
    expect(missing).not.toBeNull();
    expect(out).not.toContain('**`' + missing![1] + '`');
    expect(out).toContain('Last section row');
    expect(out.length).toBeLessThanOrEqual(19500);
  });

  it('counts verified source held earlier in the same conversation', async () => {
    vi.stubEnv('CODEGRAPH_EXPLORE_DEDUP', '1');
    const state = new ExploreSessionState();
    state.record({ projectRoot: dir, query: 'prior', sourceBytes: bigSource.length, responseBytes: bigSource.length,
      files: [{ path: 'big.ts', fingerprint: fileFingerprint(bigSource), bytes: bigSource.length,
        ranges: [{ start: 1, end: bigSource.split('\n').length }] }] });
    try {
      const result = await new ToolHandler(cg).execute('codegraph_explore', { query: 'big.ts' }, state);
      const out = result.content?.[0]?.text ?? '';
      expect(out).not.toContain('Additional indexed source not included');
    } finally { vi.unstubAllEnvs(); }
  });

  it('does not offer stale indexed continuation ranges', async () => {
    fs.appendFileSync(path.join(dir, 'big.ts'), '\n// disk drift');
    try {
      const out = await explore('big.ts');
      expect(out).toContain('changed on disk');
      expect(out).not.toContain('Additional indexed source not included');
    } finally { fs.writeFileSync(path.join(dir, 'big.ts'), bigSource); }
  });
});
