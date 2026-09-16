/**
 * Bun compatibility shim — tests only.
 *
 * Bun's `os.homedir()` snapshots $HOME at process start and ignores
 * `process.env.HOME` mutations made afterwards (oven-sh/bun#29244; fix
 * tracked in oven-sh/bun#42599). Node re-reads the environment per call,
 * which is what the installer/daemon suites rely on when they redirect
 * HOME/USERPROFILE to a tmpdir for isolation. Patch the module to Node's
 * env-first semantics so those suites pass under `bun --bun x vitest run`.
 * Remove this file when the upstream fix ships.
 *
 * Two patches are needed because callers reach the module two ways:
 *  - `mock.module` covers `import ... from 'os'` / `import * as os` (Bun's
 *    ESM namespace for builtins is a frozen snapshot — assigning
 *    `os.homedir` on it throws, and mutating the require exports does not
 *    propagate into it);
 *  - mutating the `createRequire` exports covers `require('os')` callers
 *    (mock.module does not intercept CommonJS resolution).
 */
import { createRequire } from 'node:module';

if (process.versions.bun) {
  const { mock } = await import('bun:test');
  const original = await import('node:os');
  const realHomedir = original.homedir;
  const homedir = () => {
    if (process.platform === 'win32') {
      return (
        process.env.USERPROFILE ||
        (process.env.HOMEDRIVE && process.env.HOMEPATH
          ? process.env.HOMEDRIVE + process.env.HOMEPATH
          : realHomedir())
      );
    }
    return process.env.HOME || realHomedir();
  };

  const patched = { ...original, homedir, default: { ...original, homedir } };
  mock.module('node:os', () => patched);
  mock.module('os', () => patched);

  (createRequire(import.meta.url)('node:os') as typeof original).homedir = homedir;
}
