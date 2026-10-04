/**
 * The source scan's limits are checked on the file it opened, so the answer
 * "this name appears nowhere" is given only after every file was read whole.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { execFileSync } from 'child_process';
import { scanIndexedSource } from '../src/mcp/source-scan';

let dir: string;
const LIMITS = { budgetMs: 10_000, maxFileBytes: 1024, maxTotalBytes: 4096 };

beforeAll(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-source-scan-'));
  fs.writeFileSync(path.join(dir, 'a.ts'), 'const alpha = 1;\n');
  fs.writeFileSync(path.join(dir, 'b.ts'), 'const beta = 2;\n');
  fs.writeFileSync(path.join(dir, 'big.ts'), `const gamma = '${'x'.repeat(2000)}';\n`);
});

afterAll(() => {
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});

function words(files: string[], limits = LIMITS) {
  const seen: string[] = [];
  // Index-time sizes are stale on purpose: the scan must use the opened file's.
  const result = scanIndexedSource(dir, files.map((p) => ({ path: p, size: 1 })), /\b(?:alpha|beta|gamma)\b/g, limits, (_f, _o, text) => {
    seen.push(text);
    return false;
  });
  return { ...result, seen };
}

describe('scanIndexedSource', () => {
  it('reads every file and reports a complete scan', () => {
    expect(words(['a.ts', 'b.ts'])).toEqual({ complete: true, skipped: 0, seen: ['alpha', 'beta'] });
  });

  it('skips a file over the per-file limit, which leaves absence unproven', () => {
    expect(words(['a.ts', 'big.ts'])).toEqual({ complete: true, skipped: 1, seen: ['alpha'] });
  });

  it('stops incomplete when the bytes read would pass the total limit', () => {
    const limits = { ...LIMITS, maxFileBytes: 4096, maxTotalBytes: 1024 };
    expect(words(['a.ts', 'big.ts', 'b.ts'], limits)).toEqual({ complete: false, skipped: 0, seen: ['alpha'] });
  });

  // A FIFO with no writer would block a plain open forever.
  it.runIf(process.platform !== 'win32')('skips a FIFO without waiting for a writer', () => {
    execFileSync('mkfifo', [path.join(dir, 'pipe.ts')]);
    // A child deadline makes a blocking-open regression fail without hanging
    // the test runner's own event loop.
    const script = `
      const { scanIndexedSource } = require(${JSON.stringify(path.resolve(__dirname, '../dist/mcp/source-scan.js'))});
      const seen = [];
      const result = scanIndexedSource(${JSON.stringify(dir)},
        ['a.ts', 'pipe.ts', 'b.ts'].map(path => ({ path, size: 1 })),
        /alpha|beta/g, ${JSON.stringify(LIMITS)},
        (_f, _o, word) => { seen.push(word); return false; });
      console.log(JSON.stringify({ ...result, seen }));
    `;
    const result = execFileSync(process.execPath, ['-e', script], {
      encoding: 'utf8', timeout: 3000, killSignal: 'SIGKILL',
    });
    expect(JSON.parse(result)).toEqual({ complete: true, skipped: 1, seen: ['alpha', 'beta'] });
  });

  it('counts a missing or escaping path as skipped', () => {
    expect(words(['a.ts', 'gone.ts', '../outside.ts'])).toEqual({ complete: true, skipped: 2, seen: ['alpha'] });
  });
});
