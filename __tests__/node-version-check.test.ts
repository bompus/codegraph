/**
 * Pin the unsupported-runtime banners and the version floors. The recovery
 * commands and the override env var below are load-bearing: if any of them
 * get edited away, this test catches it.
 */

import { describe, it, expect } from 'vitest';
import {
  buildBunTooOldBanner,
  buildNodeTooOldBanner,
  isVersionBelow,
  MIN_BUN_VERSION,
  MIN_NODE_VERSION,
  unsupportedRuntimeBanner,
} from '../src/bin/node-version-check';
import pkg from '../package.json';

describe('runtime floors', () => {
  it('match package.json engines', () => {
    expect(pkg.engines.node).toBe(`>=${MIN_NODE_VERSION}`);
    expect(pkg.engines.bun).toBe(`>=${MIN_BUN_VERSION}`);
  });

  it('compare major, minor and patch numerically', () => {
    expect(isVersionBelow('22.12.9', '22.13.0')).toBe(true);
    expect(isVersionBelow('22.13.0', '22.13.0')).toBe(false);
    expect(isVersionBelow('22.9.0', '22.13.0')).toBe(true);
    expect(isVersionBelow('24.0.0', '22.13.0')).toBe(false);
    expect(isVersionBelow('1.3.14', '1.4.0')).toBe(true);
    expect(isVersionBelow('1.4.0-canary.1', '1.4.0')).toBe(false);
  });

  it('judge Bun by its own version, not the Node version it reports', () => {
    expect(unsupportedRuntimeBanner({ node: '24.3.0', bun: '1.3.14' })).toContain('Unsupported Bun version: 1.3.14');
    expect(unsupportedRuntimeBanner({ node: '26.3.0', bun: '1.4.2' })).toBeNull();
    expect(unsupportedRuntimeBanner({ node: '22.12.0' })).toContain('Unsupported Node.js version: 22.12.0');
    expect(unsupportedRuntimeBanner({ node: '24.21.0' })).toBeNull();
  });
});

describe('buildNodeTooOldBanner', () => {
  it('embeds the reported Node version and the floor', () => {
    const banner = buildNodeTooOldBanner('20.18.0');
    expect(banner).toContain('Unsupported Node.js version: 20.18.0');
    expect(banner).toContain(`requires Node.js ${MIN_NODE_VERSION} or newer`);
  });

  it('points users to Node 24 LTS via nvm and Homebrew', () => {
    const banner = buildNodeTooOldBanner('16.0.0');
    expect(banner).toContain('Node.js 24 LTS');
    expect(banner).toContain('nvm install 24');
    expect(banner).toContain('brew install node@24');
  });

  it('documents the CODEGRAPH_ALLOW_UNSAFE_NODE override', () => {
    expect(buildNodeTooOldBanner('18.0.0')).toContain('CODEGRAPH_ALLOW_UNSAFE_NODE=1');
    expect(buildBunTooOldBanner('1.3.0')).toContain('CODEGRAPH_ALLOW_UNSAFE_NODE=1');
  });

  it('tells Bun users to upgrade Bun', () => {
    expect(buildBunTooOldBanner('1.3.0')).toContain('bun upgrade');
  });
});
