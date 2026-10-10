/**
 * Behaviors the engine suites rely on that Bun's Node compatibility does not
 * provide. Each is measured, not assumed from the runtime name, so a test
 * comes back on its own once the runtime supports it; `runtime-capabilities.test.ts`
 * fails if a capability goes missing anywhere but Bun.
 */
import * as fsNamespace from 'node:fs';
import fsDefault from 'node:fs';
import { spawnSync } from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';

/**
 * Replacing a method on a builtin's CommonJS object and calling
 * `syncBuiltinESMExports()` reaches code that imports the builtin by name or as
 * a namespace. Bun's `syncBuiltinESMExports` is a no-op (a fix is open as
 * oven-sh/bun#42616), so spies on `fs` and `child_process` never reach the
 * library under test there.
 */
export const builtinPatchingReachesImporters = (() => {
  const original = fsDefault.accessSync;
  const stub = (() => undefined) as typeof fsDefault.accessSync;
  fsDefault.accessSync = stub;
  try {
    syncBuiltinESMExports();
    return fsNamespace.accessSync === stub;
  } finally {
    fsDefault.accessSync = original;
    syncBuiltinESMExports();
  }
})();

/** A child spawned by bare name fails with ENOENT once `process.env.PATH` is emptied. Bun treats an empty PATH as unset and still finds the binary (oven-sh/bun#44948). */
export const emptyPathBlocksSpawn = (() => {
  const saved = process.env.PATH;
  process.env.PATH = '';
  try {
    return (spawnSync('git', ['--version']).error as NodeJS.ErrnoException | undefined)?.code === 'ENOENT';
  } finally {
    if (saved === undefined) delete process.env.PATH;
    else process.env.PATH = saved;
  }
})();
