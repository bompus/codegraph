#!/usr/bin/env node
/**
 * Make sure a kernel prebuild exists for this host before the test suite runs.
 *
 * Since Phase 5 of docs/design/kernel-only-extraction-plan.md the native
 * kernel is the only parser, so a source checkout needs one to run anything
 * that parses. Release bundles ship it; a contributor builds it once with
 * `npm run build:kernel` (a Rust toolchain). This runs from `pretest` and is
 * a no-op when the prebuild is already staged; without cargo it explains
 * rather than failing silently later in the suite.
 */
import { existsSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import * as path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const prebuild = path.join(root, 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node');
if (existsSync(prebuild)) process.exit(0);
if (process.env.CODEGRAPH_KERNEL_PATH && existsSync(process.env.CODEGRAPH_KERNEL_PATH)) process.exit(0);

const cargo = spawnSync('cargo', ['--version'], { encoding: 'utf8' });
if (cargo.status !== 0) {
  console.error(`[ensure-kernel] no kernel prebuild at ${prebuild} and no Rust toolchain (cargo) on PATH.`);
  console.error('[ensure-kernel] Install rustup (https://rustup.rs) and run `npm run build:kernel`, or set CODEGRAPH_KERNEL_PATH to a built codegraph-kernel.node.');
  process.exit(1);
}
console.log(`[ensure-kernel] no prebuild at ${prebuild}; building with scripts/build-kernel.sh`);
const build = spawnSync('bash', [path.join(root, 'scripts', 'build-kernel.sh')], { stdio: 'inherit' });
process.exit(build.status ?? 1);
