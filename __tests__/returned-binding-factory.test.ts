/**
 * `const i18n = useI18n(); i18n.baseText()` where the factory carries no
 * return annotation but its whole body is `return <binding>;` over a typed
 * or constructed module-level value. The factory's result type is that
 * binding's type, so the member call resolves on it. Any other body shape
 * stays unresolved: a guess would be worse than a miss.
 */
import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let dir: string;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  fs.rmSync(dir, { recursive: true, force: true });
});

async function callTargets(files: Record<string, string>, fn: string): Promise<string[]> {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-returned-binding-'));
  for (const [file, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), content);
  }
  graph = await CodeGraph.init(dir, { index: true });
  const caller = graph.getNodesByKind('function').find(n => n.name === fn)!;
  return graph.getOutgoingEdges(caller.id)
    .filter(e => e.kind === 'calls')
    .map(e => graph!.getNode(e.target)!.qualifiedName);
}

const i18n = (body: string) => [
  'export class I18nClass {',
  '  baseText(key: string) { return key; }',
  '}',
  'export class Other {',
  '  baseText(key: string) { return key; }',
  '}',
  'export const i18n: I18nClass = new I18nClass();',
  'const plain = new I18nClass();',
  'export function useI18n() {',
  body,
  '}',
  '',
].join('\n');

const consumer = [
  "import { useI18n } from './i18n';",
  'export function label() {',
  '  const i18n = useI18n();',
  "  return i18n.baseText('x');",
  '}',
  '',
].join('\n');

describe('factory returning a module binding', () => {
  it('types the result by an annotated binding', async () => {
    const targets = await callTargets({ 'src/i18n.ts': i18n('  return i18n;'), 'src/use.ts': consumer }, 'label');
    expect(targets).toContain('I18nClass::baseText');
  });

  it('types the result by a constructed binding', async () => {
    const targets = await callTargets({ 'src/i18n.ts': i18n('  return plain;'), 'src/use.ts': consumer }, 'label');
    expect(targets).toContain('I18nClass::baseText');
  });

  it('resolves the same call from a Vue single-file component', async () => {
    const sfc = [
      '<script setup lang="ts">',
      "import { useI18n } from './i18n';",
      'const i18n = useI18n();',
      "function label() { return i18n.baseText('x'); }",
      '</script>',
      '<template><div>{{ label() }}</div></template>',
      '',
    ].join('\n');
    const targets = await callTargets({ 'src/i18n.ts': i18n('  return i18n;'), 'src/Label.vue': sfc }, 'label');
    expect(targets).toContain('I18nClass::baseText');
  });

  it('declines a body with more than one return', async () => {
    const body = '  if (Math.random()) return plain;\n  return i18n;';
    const targets = await callTargets({ 'src/i18n.ts': i18n(body), 'src/use.ts': consumer }, 'label');
    expect(targets.filter(t => t.endsWith('::baseText'))).toEqual([]);
  });

  it('declines a returned expression that is not a bare binding', async () => {
    const targets = await callTargets({ 'src/i18n.ts': i18n('  return make(plain);'), 'src/use.ts': consumer }, 'label');
    expect(targets.filter(t => t.endsWith('::baseText'))).toEqual([]);
  });
});
