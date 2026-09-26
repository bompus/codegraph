/**
 * A component file's relative import (`import Child from './Child.svelte'`)
 * links to the imported file. The path-shaped import arm covered only the
 * TS/JS family, so a Svelte, Vue or Astro import fell through to name
 * matching and, once a same-named file elsewhere made the basename
 * ambiguous, linked to the importing file's own `import` statement.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

async function importTargets(files: Record<string, string>, from: string): Promise<string[]> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-sfc-import-'));
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), content);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  const file = cg.getNodesInFile(from).find((n) => n.kind === 'file' || n.kind === 'component');
  expect(file).toBeDefined();
  return cg
    .getNodesInFile(from)
    .flatMap((n) => cg!.getOutgoingEdges(n.id))
    .filter((e) => e.kind === 'imports')
    .map((e) => cg!.getNode(e.target))
    .filter((n): n is NonNullable<typeof n> => !!n)
    .map((n) => `${n.kind}:${n.filePath}`);
}

describe('component-file imports', () => {
  it('Svelte: a relative import links to the imported file', async () => {
    const targets = await importTargets({
      'src/App.svelte': "<script>\n  import Child from './Child.svelte';\n</script>\n\n<Child />\n",
      'src/Child.svelte': "<script>\n  export let label = 'child';\n</script>\n\n<p>{label}</p>\n",
      // A same-named component elsewhere makes a basename guess ambiguous.
      'other/Child.svelte': "<script>\n  export let label = 'other';\n</script>\n\n<p>{label}</p>\n",
    }, 'src/App.svelte');
    expect(targets).toContain('file:src/Child.svelte');
    expect(targets.filter((t) => t.startsWith('import:'))).toEqual([]);
  });

  it('Vue: a relative import links to the imported file', async () => {
    const targets = await importTargets({
      'src/App.vue': "<script setup>\nimport { helper } from './util.js';\nhelper();\n</script>\n",
      'src/util.js': 'export function helper() {}\n',
      'other/util.js': 'export function helper() {}\n',
    }, 'src/App.vue');
    expect(targets).toContain('file:src/util.js');
    expect(targets.filter((t) => t.startsWith('import:'))).toEqual([]);
  });
});
