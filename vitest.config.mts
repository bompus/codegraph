import { defineConfig } from 'vitest/config';

/**
 * The SHARED base. `vitest.workspace.mts` extends it twice — once for the
 * engine's node-environment suites and once for the viewer package's jsdom
 * one — so the environment, the plugins and the module-resolution conditions
 * a browser test needs cannot leak into the other 200-odd suites.
 */
export default defineConfig({
  test: {
    globals: true,
    environment: 'node',
    include: ['__tests__/**/*.test.ts'],
    // Bun's os.homedir() ignores runtime HOME mutation; the setup file
    // restores Node semantics for suites that redirect HOME for isolation.
    // A throwaway home dir (and git global config) per test file, inside that
    // file's temp dir, so nothing the suite runs can write to the developer's
    // real one (#2275). Inherited by the engine project only — the ui project
    // stands alone.
    setupFiles: [
      '__tests__/bun-homedir.setup.ts',
      '__tests__/file-tmpdir.setup.ts',
      '__tests__/setup-home-sandbox.ts',
    ],
    // Suites need a kernel and a current dist/ (#1879). Every run's
    // temp files go in one directory that is removed afterwards.
    globalSetup: ['__tests__/kernel.global-setup.ts', '__tests__/global-setup-dist.ts', '__tests__/run-tmpdir.global-setup.ts'],
    env: {
      /**
       * The suite spawns real CLI/MCP processes; without this they would write
       * telemetry state into the contributor's real ~/.codegraph and count test
       * tool calls as real usage. The telemetry unit tests are unaffected —
       * they inject their own `env` via the Telemetry constructor.
       */
      CODEGRAPH_TELEMETRY: '0',
    },
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json', 'html'],
    },
  },
});
