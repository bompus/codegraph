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
