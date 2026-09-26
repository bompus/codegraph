#!/usr/bin/env node
/**
 * Build the native extraction kernel (codegraph-kernel) and stage the .node
 * where the TS loader (src/extraction/kernel/loader.ts) finds it for
 * from-source runs and tests:
 *
 *   codegraph-kernel/prebuilds/<platform>-<arch>/codegraph-kernel.node
 *
 * The kernel is the only parser. Building it from source requires a Rust
 * toolchain (rustup.rs); packaged installations include a native prebuild.
 * Node rather than a shell script so the same command works from PowerShell,
 * where `bash` can resolve to WSL's and build for Linux.
 *
 * Usage:
 *   node scripts/build-kernel.mjs                 # host platform
 *   node scripts/build-kernel.mjs --target <rust-triple> [--platform <plat-arch>]
 *
 * The cross-compile form is what the release workflow uses (e.g.
 * --target x86_64-apple-darwin --platform darwin-x64 on a macos-arm runner).
 */
import { copyFileSync, existsSync, mkdirSync, rmSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import * as path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const crate = path.join(root, 'codegraph-kernel');

// Rust triples → the bundle-target names used across the release pipeline.
const PLATFORM_OF_TARGET = {
  'aarch64-apple-darwin': 'darwin-arm64',
  'x86_64-apple-darwin': 'darwin-x64',
  'x86_64-unknown-linux-gnu': 'linux-x64',
  'aarch64-unknown-linux-gnu': 'linux-arm64',
  'x86_64-pc-windows-msvc': 'win32-x64',
  'aarch64-pc-windows-msvc': 'win32-arm64',
};

let target = '';
let platform = '';
const args = process.argv.slice(2);
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--target') target = args[++i] ?? '';
  else if (args[i] === '--platform') platform = args[++i] ?? '';
  else fail(`unknown arg: ${args[i]}`);
}
if (!platform) {
  platform = target ? PLATFORM_OF_TARGET[target] : `${process.platform}-${process.arch}`;
  if (!platform) fail(`cannot map rust target '${target}' to a platform name; pass --platform`);
}

console.log(`[kernel] building codegraph-kernel for ${platform}${target ? ` (target ${target})` : ''}`);
let outDir;
if (target) {
  run('rustup', ['target', 'add', target], { allowFailure: true, quiet: true });
  run('cargo', ['build', '--release', '--target', target]);
  outDir = path.join(crate, 'target', target, 'release');
} else {
  // Lint gate: lib.rs denies clippy::all, so clippy must run where code is
  // written — a host build without it lets denied lints accumulate silently.
  // Cross-builds reuse this source and are covered by the host gate.
  run('rustup', ['component', 'add', 'clippy'], { allowFailure: true, quiet: true });
  if (run('cargo', ['clippy', '--version'], { allowFailure: true, quiet: true })) {
    run('cargo', ['clippy', '--release', '--lib', '--', '-D', 'warnings']);
  } else {
    console.error('[kernel] warning: clippy unavailable; lint gate skipped');
  }
  run('cargo', ['build', '--release']);
  outDir = path.join(crate, 'target', 'release');
}

// cdylib name differs per OS; the staged name is always codegraph-kernel.node.
const libName = platform.startsWith('darwin-')
  ? 'libcodegraph_kernel.dylib'
  : platform.startsWith('win32-')
    ? 'codegraph_kernel.dll'
    : 'libcodegraph_kernel.so';
const lib = path.join(outDir, libName);
if (!existsSync(lib)) fail(`built library not found at ${lib}`);

const destDir = path.join(crate, 'prebuilds', platform);
const dest = path.join(destDir, 'codegraph-kernel.node');
mkdirSync(destDir, { recursive: true });
// rm first so the copy lands on a FRESH inode: overwriting a signed dylib in
// place leaves macOS's per-inode signature cache stale, and every process
// that then dlopens the staged .node is SIGKILLed at load (the on-disk
// signature still verifies, which makes it maddening to diagnose).
rmSync(dest, { force: true });
copyFileSync(lib, dest);
console.log(`[kernel] staged ${dest} (${Math.round(statSync(dest).size / (1024 * 1024))}M)`);

/** Run a command in the crate; true on success. Exits on failure unless allowed. */
function run(cmd, cmdArgs, { allowFailure = false, quiet = false } = {}) {
  const r = spawnSync(cmd, cmdArgs, { cwd: crate, stdio: quiet ? 'ignore' : 'inherit' });
  if (r.status === 0) return true;
  if (allowFailure) return false;
  fail(`${cmd} ${cmdArgs.join(' ')} exited with ${r.status ?? r.signal ?? r.error?.message}`);
}

function fail(message) {
  console.error(`[kernel] error: ${message}`);
  process.exit(1);
}
