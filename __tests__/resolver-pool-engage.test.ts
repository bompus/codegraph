/**
 * Mid-run pool engagement: below the fixed ref-count gate the resolve loop
 * boots the pool once the projected main-thread kernel time for the remaining
 * refs is high enough, and never for a small remainder.
 */
import { describe, it, expect } from 'vitest';
import {
  ADAPTIVE_ENGAGE_MIN_REFS,
  ADAPTIVE_ENGAGE_RESOLVE_MS,
  shouldEngageAdaptively,
} from '../src/resolution/resolver-pool';

describe('shouldEngageAdaptively', () => {
  it('engages when the remainder is large and its projected time clears the bar', () => {
    expect(shouldEngageAdaptively(ADAPTIVE_ENGAGE_RESOLVE_MS, ADAPTIVE_ENGAGE_MIN_REFS)).toBe(true);
    expect(shouldEngageAdaptively(5_000, 100_000)).toBe(true);
  });

  it('stays on the main thread when the projected time is under the bar', () => {
    expect(shouldEngageAdaptively(ADAPTIVE_ENGAGE_RESOLVE_MS - 1, 100_000)).toBe(false);
  });

  it('stays on the main thread for a small remainder, however slow its measured rate', () => {
    expect(shouldEngageAdaptively(60_000, ADAPTIVE_ENGAGE_MIN_REFS - 1)).toBe(false);
  });
});
