import { afterEach, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';
import { ToolHandler } from '../src/mcp/tools';

let graph: CodeGraph | undefined;
let root: string;
afterEach(() => {
  graph?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

it('keeps complete requested flow bodies when adjacent functions form a long cluster', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-flow-bodies-'));
  const names = ['beginPipeline', 'transformPipeline', 'finishPipeline'];
  const bodies = names.map((name, index) => [
    `export function ${name}() {`,
    ...(index < 2 ? [`  ${names[index + 1]}();`] : []),
    ...Array.from({ length: 75 }, (_, i) => `  // ${name} step ${i}`),
    `  return '${name} complete';`,
    '}',
  ].join('\n'));
  fs.writeFileSync(path.join(root, 'pipeline.ts'), [
    ...bodies,
    ...Array.from({ length: 300 }, (_, i) => `// unrelated padding ${i} ${'x'.repeat(80)}`),
  ].join('\n'));
  graph = await CodeGraph.init(root, { index: true });
  const response = await new ToolHandler(graph).execute('codegraph_explore', { query: names.join(' ') });
  const text = response.content?.[0]?.text ?? '';
  expect(text).toContain('**Flow');
  for (const body of bodies) {
    for (const line of body.split('\n')) expect(text).toContain(line);
  }
  expect(text.length).toBeLessThanOrEqual(25000);
});

it('retains the call site when a two-symbol flow has an oversized caller', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-short-flow-'));
  fs.writeFileSync(path.join(root, 'pipeline.ts'), [
    'export function resolveSettings() {',
    ...Array.from({ length: 90 }, (_, i) => `  // setup ${i} ${'x'.repeat(50)}`),
    '  const loaded = loadSettings();',
    ...Array.from({ length: 400 }, (_, i) => `  // unrelated option ${i} ${'x'.repeat(60)}`),
    '  return loaded;',
    '}',
    'export function loadSettings() {',
    "  return { source: 'config.json' };",
    '}',
  ].join('\n'));
  graph = await CodeGraph.init(root, { index: true });
  const response = await new ToolHandler(graph).execute('codegraph_explore', {
    query: 'resolveSettings loadSettings',
  });
  const text = response.content?.[0]?.text ?? '';
  expect(text).toContain('const loaded = loadSettings();');
  expect(text).toContain("return { source: 'config.json' };");
  expect(text.length).toBeLessThanOrEqual(25000);
});

it('corroborates an overloaded callable using another query symbol in its file', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-overloaded-flow-'));
  fs.writeFileSync(path.join(root, 'transport.ts'), 'export function resolveTransport() { return 42; }');
  fs.writeFileSync(path.join(root, 'core.ts'), [
    "import { resolveTransport } from './transport';",
    'export interface ServerShape {',
    ...Array.from({ length: 160 }, (_, i) => `  field${i}: string; // server configuration`),
    '}',
    'export function createServer() {',
    '  return resolveTransport();',
    '}',
  ].join('\n'));
  for (let i = 0; i < 4; i++) fs.writeFileSync(path.join(root, `decoy${i}.ts`), [
    'export function createServer() {',
    ...Array.from({ length: 90 }, (_, j) => `  // unrelated implementation ${j}`),
    `  return ${i};`,
    '}',
  ].join('\n'));
  graph = await CodeGraph.init(root, { index: true });
  const response = await new ToolHandler(graph).execute('codegraph_explore', {
    query: 'createServer resolveTransport ServerShape',
  });
  expect(response.content?.[0]?.text).toContain('return resolveTransport();');
});

it('does not reduce an explicitly requested implementation file to signatures', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-pinned-implementations-'));
  fs.writeFileSync(path.join(root, 'pipeline.ts'), [
    'export function runPipeline() { return preparePipeline(); }',
    'export function preparePipeline() { return finishPipeline(); }',
    'export function finishPipeline() { return 42; }',
  ].join('\n'));
  fs.writeFileSync(path.join(root, 'renderers.ts'), [
    'interface Renderer { render(): string; }',
    "export class FirstRenderer implements Renderer {\n  render() {\n    return 'first renderer';\n  }\n}",
    "export class SecondRenderer implements Renderer {\n  render() {\n    return 'second renderer';\n  }\n}",
    "export class ThirdRenderer implements Renderer {\n  render() {\n    return 'third renderer';\n  }\n}",
  ].join('\n'));
  graph = await CodeGraph.init(root, { index: true });
  const response = await new ToolHandler(graph).execute('codegraph_explore', {
    query: 'renderers.ts runPipeline preparePipeline finishPipeline render',
  });
  const text = response.content?.[0]?.text ?? '';
  expect(text).toContain("return 'first renderer';");
  expect(text).toContain("return 'third renderer';");
});
