/**
 * Known-wrong and known-right edges on pinned real repositories
 * (docs/design/resolution-binding-model-plan.md, Phase 0).
 *
 * Every `absent` case is an edge a resolution PR measured and removed on a
 * real corpus; the endpoints are the ones named in that PR's body. They must
 * stay gone. The `present` cases are controls: edges the same PRs reported
 * unchanged, so a change that silently kills resolution fails here rather
 * than looking like a precision win.
 *
 * Run with `npm run eval:precision -- <corpus>` (precision-runner.ts), which
 * fetches the pinned commit, indexes it, scores every case, and records the
 * resolved-edge histogram for LOST/GAINED comparison across builds.
 */
import type { EdgeCase } from './types.js';

export interface PrecisionCorpus {
  key: string;
  repo: string;
  /** Full SHA: GitHub only serves an arbitrary commit to `git fetch` by its full id. */
  commit: string;
  note: string;
}

export const PRECISION_CORPORA: Record<string, PrecisionCorpus> = {
  vite: {
    key: 'vite',
    repo: 'https://github.com/vitejs/vite.git',
    commit: '8492422b8f110625a90c702f42f30784e8cf19dc',
    note: 'The corpus #1713, #1718 and #1746 measured (LOST 4 / 12 / 157, GAINED 0).',
  },
  vitest: {
    key: 'vitest',
    repo: 'https://github.com/vitest-dev/vitest.git',
    commit: '7c818153add03b0bca54453a54e76961dd5be18d',
    note: '#1713: 34 fuzzy edges from bare imports removed, GAINED 0.',
  },
  svelte: {
    key: 'svelte',
    repo: 'https://github.com/sveltejs/svelte.git',
    commit: '5895c637b04dc8667020c8d326807c3f3a984472',
    note: '#1713: 5 fuzzy edges removed, GAINED 0.',
  },
  flask: {
    key: 'flask',
    repo: 'https://github.com/pallets/flask.git',
    commit: 'd73fa1cdcbd8b1465c151db8924ba58b1dd14e35',
    note: 'Python corpus for binding-model Phase 3 (import rows replace the Python import regex).',
  },
  gin: {
    key: 'gin',
    repo: 'https://github.com/gin-gonic/gin.git',
    commit: 'dcaa4296d111981ffb31ac3eba90bb63e1eb5ab9',
    note: 'Go corpus for binding-model Phase 3 (import rows replace the Go import regex).',
  },
  petclinic: {
    key: 'petclinic',
    repo: 'https://github.com/spring-projects/spring-petclinic.git',
    commit: '818c4136ea971c21674525f9053de0d9c7ad8cfe',
    note: 'Java corpus for binding-model Phase 3 (import rows replace the JVM import regex).',
  },
  exposed: {
    key: 'exposed',
    repo: 'https://github.com/JetBrains/Exposed.git',
    commit: '2155404863401e0257f89c301509cde59e7becd1',
    note: 'Kotlin corpus for binding-model Phase 3 (import rows replace the JVM import regex).',
  },
};

export const edgeCases: EdgeCase[] = [
  // --- #1713: a bare (npm / builtin) import must never fuzzy-match a project symbol ---
  {
    id: 'vite-bare-import-self-edge',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'scan.ts' },
    to: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'scan' },
    expect: 'absent',
    source: '#1713',
    why: "`import { scan } from 'rolldown/experimental'` resolved onto the importing file's OWN `scan` — a self-edge.",
  },
  {
    id: 'vite-bare-import-getEnv',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'packages/vite/src/node/config.ts', name: 'config.ts' },
    to: { file: 'packages/vite/src/node/plugins/importAnalysis.ts', name: 'getEnv' },
    expect: 'absent',
    source: '#1713',
    why: "`import { getEnv } from '@vitejs/devtools/config'` resolved onto an unrelated project `getEnv`.",
  },
  {
    id: 'vitest-bare-import-evaluatedModules',
    corpus: 'vitest',
    kind: 'imports',
    to: { file: '.ts', name: 'evaluatedModules' },
    expect: 'absent',
    source: '#1713',
    why: "`import type { EvaluatedModules } from 'vite/module-runner'` resolved (case-insensitively) onto the method VitestMocker::evaluatedModules in 17 files.",
  },
  {
    id: 'svelte-bare-import-bundle',
    corpus: 'svelte',
    kind: 'imports',
    to: { file: 'scripts/generate-browser-support.ts', name: 'bundle' },
    expect: 'absent',
    source: '#1713',
    why: "`import MagicString, { Bundle } from 'magic-string'` resolved onto the script function `bundle` in 5 files.",
  },

  // --- #1718 (supersedes #1709): fuzzy may reject a unique guess, never manufacture one ---
  {
    id: 'vite-fuzzy-nested-resolveConfig-getEnv',
    corpus: 'vite',
    kind: 'calls',
    from: { file: 'packages/vite/src/node/config.ts', name: 'resolveConfig' },
    to: { file: 'packages/vite/src/node/plugins/importAnalysis.ts', name: 'getEnv' },
    expect: 'absent',
    source: '#1718',
    why: 'A fuzzy call edge onto a nested function the call site cannot lexically reach.',
  },
  {
    id: 'vite-fuzzy-nested-build-scan',
    corpus: 'vite',
    kind: 'calls',
    from: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'build' },
    to: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'scan' },
    expect: 'absent',
    source: '#1718',
    why: 'The `calls` edge that followed the self-import above.',
  },

  // --- #1746 / #1719: a module that imports but exports nothing is sealed ---
  {
    id: 'vite-sealed-module-defineConfig',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'vite.config.ts', name: 'vite.config.ts' },
    to: { file: 'playground/ssr-html/test-stacktrace.js', name: 'vite' },
    expect: 'absent',
    source: '#1746',
    why: "Every `import { defineConfig } from 'vite'` across the playground resolved onto a module-scope `const vite` in a file that exports nothing (157 edges).",
  },

  // --- Controls: edges the PRs reported unchanged ---
  {
    id: 'vite-control-relative-import',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'packages/vite/src/node/config.ts', name: 'config.ts' },
    to: { file: 'packages/vite/src/node/logger.ts', name: 'createLogger' },
    expect: 'present',
    source: 'control',
    why: "`import { createLogger } from './logger'` is a relative project import; the import resolver must still bind it.",
  },
  {
    id: 'flask-control-relative-from-import',
    corpus: 'flask',
    kind: 'imports',
    from: { file: 'src/flask/app.py', name: 'app.py' },
    to: { file: 'src/flask/helpers.py', name: 'get_debug_flag' },
    expect: 'present',
    source: 'control',
    why: '`from .helpers import get_debug_flag` is a relative package import; the Python import mappings must still bind it.',
  },
];
