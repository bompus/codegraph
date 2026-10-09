/**
 * `codegraph prompt-hook` runs on every prompt. When its cheap gates already
 * say no — nothing code-shaped, or no index reachable — it must exit before
 * the rest of the CLI loads (commander, command code), and stay silent.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { spawnSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

const BIN = path.resolve(__dirname, '../dist/bin/codegraph.js');

describe('prompt-hook no-op exits before the CLI loads', () => {
  let tmp: string;
  let report: string;

  beforeEach(() => {
    tmp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'cg-hook-fast-')));
    report = path.join(tmp, 'loaded.json');
  });
  afterEach(() => { fs.rmSync(tmp, { recursive: true, force: true }); });

  function hook(prompt: string): { stdout: string; loaded: string[] } {
    // Record the modules loaded when the process exits.
    const probe = path.join(tmp, 'probe.cjs');
    fs.writeFileSync(probe, `process.on('exit', () => require('fs').writeFileSync(${JSON.stringify(report)}, JSON.stringify(Object.keys(require.cache))));`);
    const result = spawnSync(process.execPath, ['--require', probe, BIN, 'prompt-hook'], {
      cwd: tmp,
      input: JSON.stringify({ cwd: tmp, prompt }),
      encoding: 'utf8',
      timeout: 15_000,
      env: { ...process.env, CODEGRAPH_TELEMETRY: '0', CODEGRAPH_NO_PROMPT_HOOK: '0', CODEGRAPH_PROMPT_HOOK: '1' },
    });
    expect(result.status, result.stderr).toBe(0);
    return { stdout: result.stdout, loaded: JSON.parse(fs.readFileSync(report, 'utf8')) as string[] };
  }

  it.each([
    ['a prompt with nothing code-shaped', 'yes do it'],
    ['a prompt in a directory with no index', 'who calls parseToken'],
  ])('%s', (_label, prompt) => {
    const { stdout, loaded } = hook(prompt);
    expect(stdout).toBe('');
    expect(loaded.some((f) => f.includes(`${path.sep}commander${path.sep}`))).toBe(false);
    expect(loaded.some((f) => f.endsWith(`${path.sep}codegraph.js`) && f.includes(`${path.sep}dist${path.sep}`) && !f.includes(`${path.sep}bin${path.sep}`))).toBe(false);
  });
});
