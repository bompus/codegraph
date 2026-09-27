#!/usr/bin/env node
/**
 * Make sure a kernel prebuild exists for this host before the test suite runs.
 *
 * Since Phase 5 of docs/design/kernel-only-extraction-plan.md the native
 * kernel is the only parser, so a source checkout needs one to run anything
 * that parses. Release bundles ship it; a contributor builds it once with
 * `npm run build:kernel` (a Rust toolchain). This runs from `pretest` and is
 * a no-op when the prebuild is already staged; without cargo it explains
 * rather than failing silently later in the suite. A prebuild whose source
 * stamp (scripts/kernel-stamp.mjs) differs from the checkout's kernel sources
 * is rebuilt the same way: it would otherwise run older kernel code than the
 * TypeScript beside it expects.
 */
import { existsSync, readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import * as path from 'node:path';
import { kernelSourceStamp, stampPath } from './kernel-stamp.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const prebuild = path.join(root, 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node');
if (process.env.CODEGRAPH_KERNEL_PATH && existsSync(process.env.CODEGRAPH_KERNEL_PATH)) process.exit(0);

let reason = `no prebuild at ${prebuild}`;
if (existsSync(prebuild)) {
  const want = kernelSourceStamp(root);
  if (want === null) process.exit(0); // not a git checkout: nothing to compare against
  let have = null;
  try {
    have = readFileSync(stampPath(prebuild), 'utf8').trim();
  } catch {
    // No stamp: built before stamps, or copied from elsewhere without one.
  }
  if (have === want) process.exit(0);
  reason = `the prebuild at ${prebuild} was not built from this checkout's kernel sources`;
}

const cargo = spawnSync('cargo', ['--version'], { encoding: 'utf8' });
if (cargo.status !== 0) {
  if (existsSync(prebuild)) {
    console.error(`[ensure-kernel] warning: ${reason}, and no Rust toolchain (cargo) is on PATH to rebuild it; tests run with it as is.`);
    process.exit(0);
  }
  console.error(`[ensure-kernel] no kernel prebuild at ${prebuild} and no Rust toolchain (cargo) on PATH.`);
  console.error('[ensure-kernel] Install rustup (https://rustup.rs) and run `npm run build:kernel`, or set CODEGRAPH_KERNEL_PATH to a built codegraph-kernel.node.');
  process.exit(1);
}
console.log(`[ensure-kernel] ${reason}; building with scripts/build-kernel.mjs`);
const build = spawnSync(process.execPath, [path.join(root, 'scripts', 'build-kernel.mjs')], { stdio: 'inherit' });
process.exit(build.status ?? 1);
