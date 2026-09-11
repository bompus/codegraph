/**
 * Golden graph dumps — the regression gate for every extraction change.
 *
 * Phase 0 of docs/design/kernel-only-extraction-plan.md. Each fixture is
 * indexed from scratch into a temp directory and dumped with
 * scripts/dump-graph.mjs (natural keys, sorted, no timestamps or rowids);
 * the dump must equal the checked-in `__tests__/fixtures/golden/<name>.dump`.
 *
 * The dump covers the WHOLE graph — nodes, edges, unresolved refs, files —
 * so it pins extraction, resolution and synthesis together. Any intended
 * change to any of them re-baselines with:
 *
 *   UPDATE_GOLDEN=1 npx vitest run __tests__/kernel-golden-dumps.test.ts
 *
 * and the resulting diff in the .dump files IS the review artifact: a PR
 * that changes a golden must explain every added or removed line.
 *
 * Today the kernel and wasm paths produce identical graphs for every routed
 * language (the kernel-*-parity suites pin that), so the goldens hold on
 * both paths and this suite runs whether or not a kernel binary is staged.
 * Phase 1 of the plan (native error recovery becomes canonical) will
 * re-baseline these once and thereafter the goldens describe the kernel.
 *
 * The name matches the `__tests__/kernel-*.test.ts` glob the release
 * workflow runs against the freshly built linux-x64 kernel, so the gate is
 * exercised on the native path at release time without extra wiring.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import CodeGraph from '../src/index';

const FIXTURES = path.join(__dirname, 'fixtures');
const GOLDEN_DIR = path.join(FIXTURES, 'golden');
const DUMP_SCRIPT = path.join(__dirname, '..', 'scripts', 'dump-graph.mjs');
const UPDATE = process.env.UPDATE_GOLDEN === '1';

/**
 * name → source directory. Sources outside `golden/` are existing fixtures
 * reused as-is so the corpus does not duplicate them; `golden/<name>/` holds
 * the fixtures that exist only for this gate.
 */
const CORPUS: ReadonlyArray<{ name: string; source: string; why: string }> = [
  {
    name: 'torture-multilang',
    source: path.join(FIXTURES, 'kernel-parity'),
    why: 'one torture file per kernel-routed language (20 languages, 38 files)',
  },
  {
    name: 'payroll-go',
    source: path.join(FIXTURES, 'payroll-go'),
    why: 'Go module with packages, interfaces and cross-package calls',
  },
  {
    name: 'php-import-alias-static',
    source: path.join(FIXTURES, 'php-import-alias-static'),
    why: 'PHP namespaces, use-aliases and static calls',
  },
  {
    name: 'vue-sfc',
    source: path.join(GOLDEN_DIR, 'vue-sfc'),
    why: 'Vue SFCs (script setup TS, options API, styles) + vue-router + pinia — the embedded-language path',
  },
  {
    name: 'markdown-docs',
    source: path.join(GOLDEN_DIR, 'markdown-docs'),
    why: 'Markdown sections, doc→code and code→doc path references',
  },
];

function copyDir(src: string, dst: string): void {
  fs.cpSync(src, dst, { recursive: true });
}

function dump(projectDir: string): string {
  const out = execFileSync(process.execPath, [DUMP_SCRIPT, projectDir], {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  return out.replace(/\r\n/g, '\n');
}

const tmpDirs: string[] = [];
afterEach(() => {
  for (const d of tmpDirs.splice(0)) fs.rmSync(d, { recursive: true, force: true });
});

describe('golden graph dumps', () => {
  for (const { name, source, why } of CORPUS) {
    it(`${name}: ${why}`, async () => {
      expect(fs.existsSync(source), `fixture source missing: ${source}`).toBe(true);
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), `cg-golden-${name}-`));
      tmpDirs.push(tmp);
      copyDir(source, tmp);

      const cg = CodeGraph.initSync(tmp);
      try {
        await cg.indexAll();
      } finally {
        cg.destroy();
      }

      const actual = dump(tmp);
      const goldenPath = path.join(GOLDEN_DIR, `${name}.dump`);

      if (UPDATE) {
        fs.writeFileSync(goldenPath, actual);
        return;
      }

      expect(
        fs.existsSync(goldenPath),
        `no golden for ${name}; generate with UPDATE_GOLDEN=1`,
      ).toBe(true);
      const golden = fs.readFileSync(goldenPath, 'utf8').replace(/\r\n/g, '\n');

      // Line-level diff on mismatch so the failure names the changed rows
      // instead of printing two multi-thousand-line strings.
      if (actual !== golden) {
        const a = new Set(actual.split('\n'));
        const g = new Set(golden.split('\n'));
        const added = [...a].filter((l) => !g.has(l));
        const removed = [...g].filter((l) => !a.has(l));
        const show = (xs: string[]) => xs.slice(0, 25).join('\n') + (xs.length > 25 ? `\n… (${xs.length - 25} more)` : '');
        throw new Error(
          `golden mismatch for ${name} (+${added.length} / -${removed.length} lines). ` +
            `If intended, re-baseline with UPDATE_GOLDEN=1 and explain the diff in the PR.\n` +
            `--- added\n${show(added)}\n--- removed\n${show(removed)}`,
        );
      }
    }, 120_000);
  }

  it('every golden on disk belongs to a corpus entry', () => {
    const onDisk = fs
      .readdirSync(GOLDEN_DIR)
      .filter((f) => f.endsWith('.dump'))
      .map((f) => f.replace(/\.dump$/, ''))
      .sort();
    const expected = CORPUS.map((c) => c.name).sort();
    expect(onDisk).toEqual(expected);
  });
});
