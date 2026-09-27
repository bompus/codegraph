/**
 * `npm test` runs scripts/ensure-kernel.mjs first. A prebuild left over from
 * older kernel sources used to pass that check because only its existence was
 * tested, and the suite then failed far from the cause. These pin that the
 * check compares the prebuild's source stamp with the checkout's kernel
 * sources. A fake `cargo` that cannot run keeps the check from building.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';

const SCRIPTS = path.join(__dirname, '..', 'scripts');
let root: string;

const git = (...args: string[]) => {
  const r = spawnSync('git', args, { cwd: root, encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`git ${args.join(' ')}: ${r.stderr}`);
  return r.stdout.trim();
};
const write = (rel: string, body: string) => {
  fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
  fs.writeFileSync(path.join(root, rel), body);
};
const prebuild = () => path.join(root, 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node');

// Import fresh each time: the module has no state, but the path is outside the source tree.
const stamp = async (): Promise<string | null> =>
  (await import(path.join(root, 'scripts', 'kernel-stamp.mjs'))).kernelSourceStamp(root);

function ensure(): { status: number | null; out: string } {
  const bin = path.join(root, 'fake-bin');
  const env = { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}` };
  delete env.CODEGRAPH_KERNEL_PATH;
  const r = spawnSync(process.execPath, [path.join(root, 'scripts', 'ensure-kernel.mjs')], { cwd: root, env, encoding: 'utf8' });
  return { status: r.status, out: `${r.stdout}${r.stderr}` };
}

describe.runIf(process.platform !== 'win32')('ensure-kernel source stamp', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ensure-kernel-'));
    for (const f of ['ensure-kernel.mjs', 'kernel-stamp.mjs']) {
      write(`scripts/${f}`, fs.readFileSync(path.join(SCRIPTS, f), 'utf8'));
    }
    write('codegraph-kernel/.gitignore', 'prebuilds/\n');
    write('codegraph-kernel/src/lib.rs', 'pub fn one() -> u32 { 1 }\n');
    write('fake-bin/cargo', '#!/bin/sh\nexit 1\n');
    fs.chmodSync(path.join(root, 'fake-bin', 'cargo'), 0o755);
    git('init', '-q');
    git('-c', 'user.name=t', '-c', 'user.email=t@t', 'add', '.');
    git('-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-q', '-m', 'kernel');
    write(path.relative(root, prebuild()), 'binary');
  });

  afterEach(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('accepts a prebuild stamped with the current kernel sources', async () => {
    fs.writeFileSync(`${prebuild()}.stamp`, `${await stamp()}\n`);
    const r = ensure();
    expect(r.status).toBe(0);
    expect(r.out).toBe('');
  });

  it('flags a prebuild built from other sources, or with no stamp', async () => {
    fs.writeFileSync(`${prebuild()}.stamp`, `${await stamp()}\n`);
    write('codegraph-kernel/src/lib.rs', 'pub fn one() -> u32 { 2 }\n');
    let r = ensure();
    expect(r.status).toBe(0); // no toolchain to rebuild with: warn and go on
    expect(r.out).toContain('was not built from this checkout');

    fs.rmSync(`${prebuild()}.stamp`);
    r = ensure();
    expect(r.out).toContain('was not built from this checkout');
  });

  it('gives committed and uncommitted kernel sources different stamps', async () => {
    const clean = await stamp();
    expect(clean).toMatch(/^[0-9a-f]{40}$/);
    write('codegraph-kernel/src/lib.rs', 'pub fn one() -> u32 { 2 }\n');
    const dirty = await stamp();
    expect(dirty).not.toBe(clean);
    expect(dirty!.startsWith(clean!)).toBe(true);
    git('-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-qam', 'two');
    expect(await stamp()).not.toBe(clean);
    expect(await stamp()).not.toBe(dirty);
  });
});
