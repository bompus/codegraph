import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';

let dir: string;
let cg: CodeGraph;

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-quoted-prose-'));
  fs.writeFileSync(path.join(dir, 'package.json'), '{"name":"quoted-prose-fixture"}');
  fs.writeFileSync(path.join(dir, 'notice.ts'), [
    ...Array.from({ length: 180 }, (_, i) => `export function helper${i}() { return ${i}; }`),
    'export function buildNotice(ready: boolean) {',
    '  if (!ready) return "Shipping updates are temporarily unavailable.";',
    '  return "notice-body-end";',
    '}',
  ].join('\n'));
  fs.writeFileSync(path.join(dir, 'panel.vue'), [
    '<template>',
    ...Array.from({ length: 310 }, () => '  <span>padding</span>'),
    '  <p>Changes cannot be saved until verification finishes.</p>',
    '</template>',
    '<script setup lang="ts">',
    'const unrelatedFlag = true;',
    '</script>',
  ].join('\n'));
  fs.writeFileSync(path.join(dir, 'decoy.ts'),
    'export function buildDecoy() { return "Preshipping updates are temporarily unavailable"; }');
  fs.writeFileSync(path.join(dir, 'protocol.ts'),
    'export function chooseProtocol() { return "Use protocol/2 for shipping updates"; }');
  fs.writeFileSync(path.join(dir, 'protocol-decoy.ts'),
    'export function chooseDecoyProtocol() { return "Use protocol for shipping updates"; }');
  fs.writeFileSync(path.join(dir, 'aaa.properties'),
    Array.from({ length: 16 }, (_, i) => `message${i}=Account changes require a confirmed email address.`).join('\n'));
  fs.writeFileSync(path.join(dir, 'billing.twig'),
    '<p>Account changes require a confirmed email address.</p>');
  fs.writeFileSync(path.join(dir, 'restricted.properties'),
    'message=Restricted account message must stay private.\n');
  cg = CodeGraph.initSync(dir);
  await cg.indexAll();
}, 60_000);

afterAll(() => {
  cg?.destroy();
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});

async function explore(query: string, maxFiles?: number) {
  const result = await new ToolHandler(cg).execute('codegraph_explore', { query, maxFiles });
  expect(result.isError).not.toBe(true);
  return result.content.map(c => 'text' in c ? c.text : '').join('\n');
}

describe('quoted prose source', () => {
  it('returns the complete enclosing symbol despite case and punctuation differences', async () => {
    const output = await explore('Where is "SHIPPING updates, are temporarily unavailable"?', 1);
    expect(output).toContain('notice.ts');
    expect(output).toContain('if (!ready) return "Shipping updates are temporarily unavailable.";');
    expect(output).toContain('return "notice-body-end";');
    expect(output).not.toContain('buildDecoy');
    expect(output.length).toBeLessThanOrEqual(25_000);
  });

  it('returns unquoted template prose far from indexed declarations', async () => {
    const output = await explore('Where is `Changes cannot be saved until verification finishes`?', 1);
    expect(output).toContain('panel.vue');
    expect(output).toContain('<p>Changes cannot be saved until verification finishes.</p>');
    expect(output.length).toBeLessThanOrEqual(25_000);
  });

  it('admits indexed template source that has no graph nodes', async () => {
    expect(cg.getFile('billing.twig')?.nodeCount).toBe(0);
    expect(cg.getFile('aaa.properties')?.language).toBe('properties');
    const output = await explore('Where is "Account changes require a confirmed email address"?', 1);
    expect(output).toContain('billing.twig');
    expect(output).toContain('<p>Account changes require a confirmed email address.</p>');
    expect(output.length).toBeLessThanOrEqual(25_000);
  });

  it('preserves number words in copied prose before symbol normalization', async () => {
    const output = await explore('Where is "Use protocol/2 for shipping updates"?', 1);
    expect(output).toContain('protocol.ts');
    expect(output).toContain('return "Use protocol/2 for shipping updates";');
    expect(output).not.toContain('return "Use protocol for shipping updates";');
  });

  it('preserves config-value source redaction for prose matches', async () => {
    expect(cg.getFile('restricted.properties')?.language).toBe('properties');
    const output = await explore('Where is "Restricted account message must stay private"?');
    expect(output).not.toContain('message=Restricted account message must stay private.');
  });

  it('does not scan source for an ordinary symbol query or a two-word quote', async () => {
    const scanFiles = vi.spyOn(cg, 'getTextScanFiles');
    try {
      await explore('buildNotice');
      await explore('buildNotice "shipping updates"');
      await explore("buildNotice \"don't stop\"");
      expect(scanFiles).not.toHaveBeenCalled();
    } finally { scanFiles.mockRestore(); }
  });
});
