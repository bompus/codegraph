import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';

let dir: string;
let cg: CodeGraph;

const padding = Array.from({ length: 160 }, (_, i) =>
  `function fixture${i}() { return "${'irrelevant padding '.repeat(12)}"; }`).join('\n');
const testBuilders = [
  ['normal', 'it'],
  ['table', 'it.each([1, 2])'],
  ['matrix', 'test.each([1, 2])'],
  ['parallel', 'test.concurrent.each([1, 2])'],
  ['tagged', 'it.each`value\n${1}\n${2}`'],
  ['spaced', 'it .each([1, 2])'],
  ['commented', 'test /* builder */ .concurrent\n  .each([1, 2])'],
] as const;

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-requested-regressions-'));
  fs.writeFileSync(path.join(dir, 'package.json'), '{"name":"requested-source-fixture"}');
  fs.writeFileSync(path.join(dir, 'README.md'), '# Worker guide\n\n## Usage\n\nRun the worker.\n');
  fs.writeFileSync(path.join(dir, 'worker.ts'),
    'export function runWorker() { return "WORKER_BODY_MARKER"; }\n');
  for (const [name, builder] of testBuilders) {
    fs.writeFileSync(path.join(dir, `${name}.test.ts`),
      `${padding}\n${builder}("restores the cache", (value) => {\n` +
      '  expect(value).toBe(1); // ASSERTION_MARKER\n});\n');
  }
  const component = '<template><div class="cache_key" /></template>\n<script>\n' + padding +
    '\n</script>\n<style>\n.cache_key { width: 173px; /* STYLE_BODY_MARKER */ }\n</style>\n';
  fs.writeFileSync(path.join(dir, 'cache_key.vue'), component);
  fs.writeFileSync(path.join(dir, 'Board.vue'), component);
  fs.writeFileSync(path.join(dir, 'cache-key.vue'), component.replaceAll('cache_key', 'cache-key'));
  fs.writeFileSync(path.join(dir, 'cache-key.ts'),
    'export function siblingWorker() { return "SIBLING_BODY_MARKER"; }\n');
  cg = CodeGraph.initSync(dir);
  await cg.indexAll();
}, 60_000);

afterAll(() => {
  cg?.destroy();
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});

async function explore(query: string, maxFiles?: number): Promise<string> {
  const result = await new ToolHandler(cg).execute('codegraph_explore', { query, maxFiles });
  expect(result.isError).not.toBe(true);
  return result.content?.[0]?.text ?? '';
}

function sourceIn(output: string, file: string): string {
  const start = output.indexOf('**`' + file + '`');
  expect(start, file).toBeGreaterThanOrEqual(0);
  const section = output.slice(start).split('\n**`')[0]!;
  return [...section.matchAll(/```[^\n]*\n([\s\S]*?)```/g)].map(match => match[1]).join('\n');
}

describe('explicit source survives unrelated query context', () => {
  it.each(['README.md worker.ts', 'worker.ts README.md'])('returns both pinned files for %s', async query => {
    const output = await explore(query, 2);
    expect(sourceIn(output, 'README.md')).toContain('Worker guide');
    expect(sourceIn(output, 'worker.ts')).toContain('WORKER_BODY_MARKER');
  });

  it('keeps code-only and symbol-named mixed requests working', async () => {
    expect(sourceIn(await explore('worker.ts', 2), 'worker.ts')).toContain('WORKER_BODY_MARKER');
    expect(sourceIn(await explore('README.md worker.ts runWorker', 2), 'worker.ts')).toContain('WORKER_BODY_MARKER');
  });

  it('keeps an unrelated code file out of a documentation-only answer', async () => {
    const output = await explore('README.md');
    expect(sourceIn(output, 'README.md')).toContain('Worker guide');
    expect(output).not.toContain('**`worker.ts`');
  });

  it.each(testBuilders)('returns the requested callback for %s', async (name) => {
    const file = `${name}.test.ts`;
    const output = await explore(file);
    expect(sourceIn(output, file)).toContain('ASSERTION_MARKER');
    expect(sourceIn(output, file)).toContain('expect(value).toBe(1)');
  });

  it('keeps a quoted identifier when it equals the pinned filename stem', async () => {
    expect(sourceIn(await explore('cache_key.vue "cache_key"'), 'cache_key.vue')).toContain('STYLE_BODY_MARKER');
  });

  it('does not treat an unquoted filename stem as a requested CSS identifier', async () => {
    expect(sourceIn(await explore('cache_key.vue cache_key'), 'cache_key.vue')).not.toContain('STYLE_BODY_MARKER');
  });

  it.each(['"', "'", '`'])('keeps a quoted kebab identifier wrapped in %s', async quote => {
    const output = await explore(`cache-key.vue ${quote}cache-key${quote}`);
    expect(sourceIn(output, 'cache-key.vue')).toContain('STYLE_BODY_MARKER');
    expect(sourceIn(output, 'cache-key.ts')).toContain('SIBLING_BODY_MARKER');
  });

  it('keeps alternate-filename and alternate-term source controls working', async () => {
    expect(sourceIn(await explore('Board.vue "cache_key"'), 'Board.vue')).toContain('STYLE_BODY_MARKER');
    expect(sourceIn(await explore('cache_key.vue "173px"'), 'cache_key.vue')).toContain('STYLE_BODY_MARKER');
  });
});
