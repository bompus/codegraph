/**
 * Build or refresh the native kernel before the suite, as `pretest` does, so
 * a direct `vitest` run in a fresh checkout or worktree does not index with no
 * parser and fail far from the cause (tests see zero symbols).
 */
import { execFileSync } from 'child_process';
import * as path from 'path';

export default function setup(): void {
  execFileSync(process.execPath, [path.resolve(__dirname, '../scripts/ensure-kernel.mjs')], { stdio: 'inherit' });
}
