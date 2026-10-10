import { describe, expect, it } from 'vitest';
import { builtinPatchingReachesImporters, emptyPathBlocksSpawn } from './helpers/runtime-capabilities';

// Tests that need these capabilities skip themselves where they are missing. This
// keeps that skip to Bun: on Node a missing capability would otherwise silently
// turn a whole group of tests off.
describe('runtime capabilities the suites skip on', () => {
  it('lets a patched builtin reach its importers, except on Bun', () => {
    expect(builtinPatchingReachesImporters || !!process.versions.bun).toBe(true);
  });

  it('lets an emptied PATH block a spawn by name, except on Bun', () => {
    expect(emptyPathBlocksSpawn || !!process.versions.bun).toBe(true);
  });
});
