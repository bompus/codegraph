import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { afterEach, expect, it } from 'vitest';

const verifier = resolve(import.meta.dirname, '../scripts/add-lang/verify-extraction.mjs');
const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

function run(status: unknown, native = false) {
  const root = mkdtempSync(join(tmpdir(), 'add-lang-verifier-'));
  roots.push(root);
  const binary = join(root, native ? 'status' : 'task build.mjs');
  const args = join(root, 'args.json');
  writeFileSync(binary, `import { writeFileSync } from 'node:fs';
writeFileSync(${JSON.stringify(args)}, JSON.stringify(process.argv.slice(2)));
console.log(${JSON.stringify(JSON.stringify(status))});
`);
  const result = spawnSync(process.execPath, [verifier, 'owned sample', 'python'], {
    cwd: root, encoding: 'utf8',
    env: { ...process.env, PATH: '', CG_BIN: native ? process.execPath : binary },
  });
  return { result, args: existsSync(args) ? JSON.parse(readFileSync(args, 'utf8')) : undefined };
}
const healthy = { initialized: true, languages: ['python'], nodesByKind: { function: 2 }, fileCount: 1, edgeCount: 2 };

it('uses the selected JavaScript build without a codegraph command on PATH', () => {
  const { result, args } = run(healthy);
  expect(result.status).toBe(0);
  expect(result.stdout).toContain('RESULT: PASS');
  expect(args).toEqual(['status', 'owned sample', '--json']);
});

it('accepts a directly executable CG_BIN', () => {
  const { result, args } = run(healthy, true);
  expect(result.status).toBe(0);
  expect(args).toEqual(['owned sample', '--json']);
});

it('keeps extraction failures distinct from invocation failures', () => {
  const { result } = run({ ...healthy, nodesByKind: { file: 1, import: 1 } });
  expect(result.status).toBe(1);
  expect(result.stdout).toContain('RESULT: FAIL');
  const missing = spawnSync(process.execPath, [verifier, 'owned sample', 'python'], {
    encoding: 'utf8', env: { ...process.env, CG_BIN: join(tmpdir(), 'missing-codegraph-binary'), PATH: '' },
  });
  expect(missing.status).toBe(2);
  expect(missing.stderr).toContain('could not read codegraph status');
});
