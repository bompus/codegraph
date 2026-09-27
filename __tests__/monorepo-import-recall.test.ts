/**
 * Imports between packages of a TypeScript monorepo.
 *
 * - A package's own tsconfig `paths` govern its files: `@/logger` in one
 *   package means that package's `src/logger.ts`, not the root config's
 *   mapping (or nothing, when the root declares no `paths`).
 * - A workspace package whose manifest points at uncommitted build output
 *   (`"main": "dist/index.js"`) resolves to the source that compiles to it.
 *
 * Both used to fall through to project-wide name matching, which picks
 * whichever same-named export it finds first.
 */
import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';
import { applyAliases, loadProjectAliases, loadScopedAliases, scopedAliasesForFile } from '../src/resolution/path-aliases';

let dir: string;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  fs.rmSync(dir, { recursive: true, force: true });
});

function write(files: Record<string, unknown>): void {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-monorepo-imports-'));
  for (const [file, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), typeof content === 'string' ? content : JSON.stringify(content));
  }
}

async function callTargets(fn: string): Promise<string[]> {
  graph = await CodeGraph.init(dir, { index: true });
  const caller = graph.getNodesByKind('function').find(n => n.name === fn)!;
  return graph.getOutgoingEdges(caller.id)
    .filter(e => e.kind === 'calls')
    .map(e => graph!.getNode(e.target)!.filePath);
}

const logger = (tag: string) => `export function logLine() { return '${tag}'; }\n`;

describe('nested tsconfig paths', () => {
  it('scopes each package to its own nearest config', () => {
    write({
      'tsconfig.json': { compilerOptions: { paths: { '@/*': ['./shared/*'] } } },
      'packages/cli/tsconfig.json': '// JSONC\n{ "extends": ["../../tsconfig.json"], "compilerOptions": { "paths": { "@/*": ["./src/*"], } } }',
      'packages/cli/src/a.ts': '',
      'packages/web/tsconfig.json': { compilerOptions: { strict: true } },
      'packages/web/src/b.ts': '',
    });
    const scopes = loadScopedAliases(dir, ['packages/cli/src/a.ts', 'packages/web/src/b.ts']);
    expect(scopes.map(s => s.dir)).toEqual(['packages/cli']);
    expect(applyAliases('@/x', scopedAliasesForFile('packages/cli/src/a.ts', scopes)!, dir)).toEqual(['packages/cli/src/x']);
    // A nested config without paths leaves its files on the root aliases alone.
    expect(scopedAliasesForFile('packages/web/src/b.ts', scopes)).toBeNull();
    expect(scopedAliasesForFile('packages/cli-extra/a.ts', scopes)).toBeNull();
    expect(applyAliases('@/x', loadProjectAliases(dir)!, dir)).toEqual(['shared/x']);
  });

  it('falls back to the root aliases when the nested target is uncommitted build output', async () => {
    write({
      'tsconfig.json': { compilerOptions: { paths: { lib: ['./packages/lib/src/index.ts'] } } },
      'test/e2e/tsconfig.json': { compilerOptions: { paths: { lib: ['../../packages/lib/dist/index.d.ts'] } } },
      'packages/lib/src/index.ts': 'export function check() { return 1; }\n',
      'packages/other/src/check.ts': 'export function check() { return 2; }\n',
      'test/e2e/use.test.ts': "import { check } from 'lib';\nexport function run() { return check(); }\n",
    });
    const targets = await callTargets('run');
    expect(targets).toContain('packages/lib/src/index.ts');
    expect(targets).not.toContain('packages/other/src/check.ts');
  });

  it('binds an aliased import to the importing package', async () => {
    write({
      'tsconfig.json': { compilerOptions: { paths: { '@/*': ['./packages/agents/src/*'] } } },
      'packages/cli/tsconfig.json': { compilerOptions: { paths: { '@/*': ['./src/*'] } } },
      'packages/cli/src/logger.ts': logger('cli'),
      'packages/cli/src/main.ts': "import { logLine } from '@/logger';\nexport function run() { return logLine(); }\n",
      'packages/agents/src/logger.ts': logger('agents'),
    });
    const targets = await callTargets('run');
    expect(targets).toContain('packages/cli/src/logger.ts');
    expect(targets).not.toContain('packages/agents/src/logger.ts');
  });
});

describe('workspace packages published from build output', () => {
  it('resolves a package import to the source behind its dist entry', async () => {
    write({
      'package.json': { workspaces: ['packages/*'] },
      'packages/utils/package.json': { name: '@s/utils', main: 'dist/index.js', types: 'dist/index.d.ts' },
      'packages/utils/src/index.ts': "export { sleep } from './sleep';\n",
      'packages/utils/src/sleep.ts': 'export function sleep() { return 1; }\n',
      'packages/dev/package.json': { name: '@s/dev' },
      'packages/dev/src/sleep.ts': 'export function sleep() { return 2; }\n',
      'packages/app/package.json': { name: '@s/app' },
      'packages/app/src/main.ts': "import { sleep } from '@s/utils';\nexport function run() { return sleep(); }\n",
    });
    const targets = await callTargets('run');
    expect(targets).toContain('packages/utils/src/sleep.ts');
    expect(targets).not.toContain('packages/dev/src/sleep.ts');
  });
});
