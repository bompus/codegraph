import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { spawnSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

const BIN = path.resolve(__dirname, '../dist/bin/codegraph.js');

// Preloaded into the CLI: hides every real addon so the loader finds none,
// while a CODEGRAPH_KERNEL_PATH under another name stays visible.
const HIDE_KERNEL = `
const fs = require('fs');
const existsSync = fs.existsSync;
fs.existsSync = (p) => (String(p).endsWith('codegraph-kernel.node') ? false : existsSync(p));
`;

describe('CLI without a usable native engine', () => {
  let root: string;
  let preload: string;

  beforeAll(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-no-kernel-'));
    preload = path.join(root, 'hide-kernel.cjs');
    fs.writeFileSync(preload, HIDE_KERNEL);
  });

  afterAll(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  function run(args: string[], env: Record<string, string> = {}) {
    return spawnSync(process.execPath, ['--require', preload, BIN, ...args], {
      cwd: root,
      encoding: 'utf-8',
      timeout: 20_000,
      env: { ...process.env, CODEGRAPH_NO_DAEMON: '1', CODEGRAPH_TELEMETRY: '0', NO_COLOR: '1', ...env },
    });
  }

  it('still reports its version', () => {
    const result = run(['version']);
    expect(result.stderr).not.toContain('native engine');
    expect(result.status).toBe(0);
    expect(result.stdout.trim()).not.toBe('');
  });

  it('refuses to index, saying the engine was not found', () => {
    const result = run(['index', root]);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain('was not found');
  });

  it('names a binary it found but could not load, and why', () => {
    const bogus = path.join(root, 'bogus.node');
    fs.writeFileSync(bogus, 'not an addon');
    const result = run(['index', root], { CODEGRAPH_KERNEL_PATH: bogus });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain(`could not be used: ${bogus} (failed to load:`);
    expect(result.stderr).not.toContain('was not found');
  });
});
