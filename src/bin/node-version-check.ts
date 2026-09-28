/**
 * Runtime version compatibility check.
 *
 * Owns the user-facing banner shown before exit when the running Node or Bun
 * is below the supported floor. Kept side-effect-free so it is safe to import
 * from tests without triggering CLI bootstrap. (The Node 25 block that lived
 * here guarded a V8 wasm-compiler bug; it went with the wasm path in Phase 5
 * of kernel-only-extraction-plan.md.)
 */

/**
 * Lowest supported Node.js version. Matches the `engines.node` floor in
 * package.json. 22.13 is the first 22.x release where `node:sqlite` loads
 * without `--experimental-sqlite`. `engines` alone only *warns* on install
 * (unless the user set `engine-strict`), so the CLI bootstrap also hard-blocks
 * here to actually enforce the floor.
 */
export const MIN_NODE_VERSION = '22.13.0';

/**
 * Lowest supported Bun version. Matches `engines.bun` in package.json. Bun
 * 1.4.0 is the first release with a built-in `node:sqlite`; 1.3.x fails at the
 * first database open.
 */
export const MIN_BUN_VERSION = '1.4.0';

/** True when dotted version `version` sorts below `min` (major.minor.patch). */
export function isVersionBelow(version: string, min: string): boolean {
  const a = version.split('.').map((part) => parseInt(part, 10) || 0);
  const b = min.split('.').map((part) => parseInt(part, 10) || 0);
  for (let i = 0; i < 3; i++) {
    if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) < (b[i] ?? 0);
  }
  return false;
}

/**
 * Return the banner for an unsupported runtime, or null when the runtime is
 * supported. Bun reports a `node` version too, so it is judged by its own.
 */
export function unsupportedRuntimeBanner(versions: { node: string; bun?: string }): string | null {
  if (versions.bun !== undefined) {
    return isVersionBelow(versions.bun, MIN_BUN_VERSION) ? buildBunTooOldBanner(versions.bun) : null;
  }
  return isVersionBelow(versions.node, MIN_NODE_VERSION) ? buildNodeTooOldBanner(versions.node) : null;
}

const OVERRIDE_LINES = [
  '',
  'To override (NOT recommended - unsupported):',
  '  CODEGRAPH_ALLOW_UNSAFE_NODE=1 codegraph ...',
];

/**
 * Build the bordered banner shown when CodeGraph detects a Node.js below
 * {@link MIN_NODE_VERSION}. Pinned via unit test so the recovery commands and
 * the override env var can't be silently stripped by future edits.
 *
 * Uses ASCII glyphs to stay readable on Windows OEM-codepage consoles
 * (see ../ui/glyphs.ts for the rationale).
 */
export function buildNodeTooOldBanner(nodeVersion: string): string {
  const sep = '-'.repeat(72);
  return [
    sep,
    `[CodeGraph] Unsupported Node.js version: ${nodeVersion}`,
    sep,
    `CodeGraph requires Node.js ${MIN_NODE_VERSION} or newer. Older versions`,
    'lack the built-in node:sqlite module and other APIs CodeGraph depends on,',
    'and are not tested or supported.',
    '',
    'Fix: install Node.js 24 LTS:',
    '  nvm install 24 && nvm use 24                          # nvm',
    '  brew install node@24 && brew link --overwrite --force node@24  # Homebrew',
    ...OVERRIDE_LINES,
    sep,
  ].join('\n');
}

/** Bun counterpart of {@link buildNodeTooOldBanner}. */
export function buildBunTooOldBanner(bunVersion: string): string {
  const sep = '-'.repeat(72);
  return [
    sep,
    `[CodeGraph] Unsupported Bun version: ${bunVersion}`,
    sep,
    `CodeGraph requires Bun ${MIN_BUN_VERSION} or newer. Older Bun releases lack`,
    'the built-in node:sqlite module.',
    '',
    'Fix: bun upgrade',
    ...OVERRIDE_LINES,
    sep,
  ].join('\n');
}
