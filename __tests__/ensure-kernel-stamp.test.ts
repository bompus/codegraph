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
const cacheDir = () => path.join(root, 'kernel-cache');
const cached = (stampText: string) => path.join(cacheDir(), `${process.platform}-${process.arch}`, stampText, 'codegraph-kernel.node');
const prebuild = () => path.join(root, 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node');

// Import fresh each time: the module has no state, but the path is outside the source tree.
const stamp = async (): Promise<string | null> =>
  (await import(path.join(root, 'scripts', 'kernel-stamp.mjs'))).kernelSourceStamp(root);

function ensure(): { status: number | null; out: string } {
  const bin = path.join(root, 'fake-bin');
  const env = { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}` };
  delete env.CODEGRAPH_KERNEL_PATH;
  env.CODEGRAPH_KERNEL_CACHE = cacheDir();
  const r = spawnSync(process.execPath, [path.join(root, 'scripts', 'ensure-kernel.mjs')], { cwd: root, env, encoding: 'utf8' });
  return { status: r.status, out: `${r.stdout}${r.stderr}` };
}

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'ensure-kernel-'));
  for (const f of ['ensure-kernel.mjs', 'kernel-stamp.mjs', 'kernel-cache.mjs']) {
    write(`scripts/${f}`, fs.readFileSync(path.join(SCRIPTS, f), 'utf8'));
  }
  write('codegraph-kernel/.gitignore', 'target/\ntarget-*/\nprebuilds/\n*.node\n');
  write('.gitignore', 'kernel-cache/\nfake-bin/\n');
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

describe.runIf(process.platform !== 'win32')('ensure-kernel source stamp', () => {
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

  it('flags a prebuild after an untracked source changes at the same path', async () => {
    write('codegraph-kernel/src/pending.rs', 'pub fn pending() -> u32 { 1 }\n');
    fs.writeFileSync(`${prebuild()}.stamp`, `${await stamp()}\n`);
    write('codegraph-kernel/src/pending.rs', 'pub fn pending() -> u32 { 2 }\n');
    const r = ensure();
    expect(r.status).toBe(0);
    expect(r.out).toContain('was not built from this checkout');
  });

  it('restores a cached release build of the same committed sources without a toolchain', async () => {
    write(path.relative(root, cached((await stamp())!)), 'cached release');
    fs.rmSync(prebuild());
    const r = ensure();
    expect(r.status).toBe(0);
    expect(r.out).toContain('restored a release build of the same sources from the kernel cache');
    expect(fs.readFileSync(prebuild(), 'utf8')).toBe('cached release');
    expect(fs.readFileSync(`${prebuild()}.stamp`, 'utf8')).toBe(`${await stamp()}\n`);
  });

  it('never restores a cached build for uncommitted sources, or when the cache is off', async () => {
    const committed = (await stamp())!;
    write(path.relative(root, cached(committed)), 'cached release');
    write('codegraph-kernel/src/lib.rs', 'pub fn one() -> u32 { 2 }\n');
    let r = ensure();
    expect(r.out).toContain('was not built from this checkout');
    expect(fs.readFileSync(prebuild(), 'utf8')).toBe('binary');

    git('checkout', '--', 'codegraph-kernel/src/lib.rs');
    const bin = path.join(root, 'fake-bin');
    const off = spawnSync(process.execPath, [path.join(root, 'scripts', 'ensure-kernel.mjs')], {
      cwd: root, encoding: 'utf8',
      env: { ...process.env, CODEGRAPH_KERNEL_PATH: '', CODEGRAPH_KERNEL_CACHE: 'off', PATH: `${bin}${path.delimiter}${process.env.PATH}` },
    });
    expect(`${off.stdout}${off.stderr}`).toContain('was not built from this checkout');
    expect(fs.readFileSync(prebuild(), 'utf8')).toBe('binary');
  });

  it('accepts a quick build of the current sources and says it is not for measuring', async () => {
    fs.writeFileSync(`${prebuild()}.stamp`, `${await stamp()} quick\n`);
    const r = ensure();
    expect(r.status).toBe(0);
    expect(r.out).toContain('quick (unoptimized) kernel build');
  });

  it('fails visibly when Git cannot enumerate untracked inputs', async () => {
    fs.writeFileSync(`${prebuild()}.stamp`, `${await stamp()}\n`);
    const realGit = spawnSync('which', ['git'], { encoding: 'utf8' });
    expect(realGit.status).toBe(0);
    write('fake-bin/git', `#!/bin/sh\nif [ "$1" = ls-files ]; then exit 1; fi\nexec "${realGit.stdout.trim()}" "$@"\n`);
    fs.chmodSync(path.join(root, 'fake-bin/git'), 0o755);
    const r = ensure();
    expect(r.status).not.toBe(0);
    expect(r.out).toContain('Cannot enumerate untracked kernel sources');
  });
});

describe('kernel source stamp inputs', () => {
  it.each([false, true])('includes nested untracked bytes and additions with hidden status=%s', async (hidden) => {
    if (hidden) git('config', 'status.showUntrackedFiles', 'no');
    const clean = await stamp();
    write('codegraph-kernel/src/pending/one.rs', 'one');
    const added = await stamp();
    expect(added).not.toBe(clean);
    expect(await stamp()).toBe(added);
    write('codegraph-kernel/src/pending/one.rs', 'two');
    const edited = await stamp();
    expect(edited).not.toBe(added);
    write('codegraph-kernel/src/pending/two.rs', 'two');
    expect(await stamp()).not.toBe(edited);
  });

  it('preserves spaces and non-ASCII untracked filenames', async () => {
    write('codegraph-kernel/src/pending space é.rs', 'one');
    const first = await stamp();
    write('codegraph-kernel/src/pending space é.rs', 'two');
    expect(await stamp()).not.toBe(first);
    const edited = await stamp();
    fs.renameSync(path.join(root, 'codegraph-kernel/src/pending space é.rs'), path.join(root, 'codegraph-kernel/src/renamed é.rs'));
    const renamed = await stamp();
    expect(renamed).not.toBe(edited);
    expect(await stamp()).toBe(renamed);
  });

  it.runIf(process.platform !== 'win32')('preserves newlines in untracked filenames', async () => {
    write('codegraph-kernel/src/pending\nfile.rs', 'one');
    const first = await stamp();
    write('codegraph-kernel/src/pending\nfile.rs', 'two');
    expect(await stamp()).not.toBe(first);
  });

  it('excludes ignored build outputs and files outside the kernel', async () => {
    write('codegraph-kernel/src/pending.rs', 'one');
    const first = await stamp();
    for (const rel of ['target/cache', 'target-debug/cache', 'prebuilds/cache', 'native.node']) {
      write(`codegraph-kernel/${rel}`, 'output');
    }
    write('outside.rs', 'outside');
    expect(await stamp()).toBe(first);
  });

  it.runIf(process.platform !== 'win32')('hashes symlink text without following its target', async () => {
    write('outside.rs', 'one');
    const link = path.join(root, 'codegraph-kernel/src/pending.rs');
    fs.symlinkSync('../../outside.rs', link);
    const first = await stamp();
    write('outside.rs', 'two');
    expect(await stamp()).toBe(first);
    fs.unlinkSync(link);
    fs.symlinkSync('../../missing.rs', link);
    expect(await stamp()).not.toBe(first);
  });

  it.runIf(process.platform !== 'win32')('rejects an unreadable untracked input', async () => {
    write('codegraph-kernel/src/pending.rs', 'one');
    const file = path.join(root, 'codegraph-kernel/src/pending.rs');
    fs.chmodSync(file, 0o000);
    try {
      await expect(stamp()).rejects.toThrow(/EACCES/);
    } finally {
      fs.chmodSync(file, 0o644);
    }
  });

  it.runIf(process.platform !== 'win32')('rejects an unreadable untracked directory even when Git exits zero', async () => {
    write('codegraph-kernel/src/pending/one.rs', 'one');
    const dir = path.join(root, 'codegraph-kernel/src/pending');
    fs.chmodSync(dir, 0o000);
    try {
      await expect(stamp()).rejects.toThrow('Cannot enumerate untracked kernel sources');
    } finally {
      fs.chmodSync(dir, 0o755);
    }
  });

  it('keeps the no-Git stamp absent', async () => {
    fs.rmSync(path.join(root, '.git'), { recursive: true });
    expect(await stamp()).toBeNull();
  });
});

describe('kernel cache', () => {
  it('keeps the four most recently saved or restored builds', async () => {
    const { saveKernel, restoreKernel } = await import(path.join(SCRIPTS, 'kernel-cache.mjs'));
    const built = path.join(root, 'built.node');
    fs.writeFileSync(built, 'x');
    const saved = process.env.CODEGRAPH_KERNEL_CACHE;
    process.env.CODEGRAPH_KERNEL_CACHE = cacheDir();
    try {
      const dir = path.join(cacheDir(), 'p');
      for (let i = 0; i < 5; i++) {
        saveKernel('p', `s${i}`, built);
        fs.utimesSync(path.join(dir, `s${i}`), i + 1, i + 1);
      }
      expect(restoreKernel('p', 's1', path.join(root, 'out', 'k.node'))).toBe(true);
      saveKernel('p', 's5', built);
      expect(fs.readdirSync(dir).sort()).toEqual(['s1', 's3', 's4', 's5']);
      saveKernel('p', 'abc+dirty', built);
      expect(fs.existsSync(path.join(dir, 'abc+dirty'))).toBe(false);
    } finally {
      if (saved === undefined) delete process.env.CODEGRAPH_KERNEL_CACHE;
      else process.env.CODEGRAPH_KERNEL_CACHE = saved;
    }
  });
});
