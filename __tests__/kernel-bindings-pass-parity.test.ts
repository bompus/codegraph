/**
 * The AST-only binding pass (`bindingsFile`, the generic extractor's path: a
 * stack-guard defer, ArkTS) must emit the same rows the full walk emits for
 * the same source — the resolver reads one table and does not know which
 * path filled it. Every parity/golden fixture in a binding-emitting language
 * is compared here: the two multisets of rows, node ids aside (the AST pass
 * has none; the TS side attaches them by name and line), must be identical.
 */
import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { tryKernelExtract, tryKernelBindings } from '../src/extraction/kernel';
import { detectLanguage } from '../src/extraction/grammars';
import type { Binding } from '../src/types';

const KERNEL_PATH = path.join(
  __dirname, '..', 'codegraph-kernel', 'prebuilds', `${process.platform}-${process.arch}`, 'codegraph-kernel.node',
);
const kernelBuilt = fs.existsSync(KERNEL_PATH);

const FIXTURE_DIRS = [
  path.join(__dirname, 'fixtures', 'kernel-parity'),
  path.join(__dirname, 'fixtures', 'payroll-go'),
  path.join(__dirname, 'fixtures', 'php-import-alias-static'),
];

function walk(dir: string, out: string[]): string[] {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else out.push(full);
  }
  return out;
}

/** One row, node id dropped, as a stable string for multiset comparison. */
function key(b: Binding): string {
  return JSON.stringify([
    b.kind, b.exportForm ?? null, b.scopeStart, b.scopeEnd, b.name,
    b.targetSpec ?? null, b.targetName ?? null, b.exportedAs ?? null, b.storage ?? null, b.line,
  ]);
}

function diff(walkRows: string[], passRows: string[]): { onlyWalk: string[]; onlyPass: string[] } {
  const count = new Map<string, number>();
  for (const r of walkRows) count.set(r, (count.get(r) ?? 0) + 1);
  const onlyPass: string[] = [];
  for (const r of passRows) {
    const n = count.get(r) ?? 0;
    if (n === 0) onlyPass.push(r);
    else count.set(r, n - 1);
  }
  const onlyWalk: string[] = [];
  for (const [r, n] of count) for (let i = 0; i < n; i++) onlyWalk.push(r);
  return { onlyWalk, onlyPass };
}

describe.skipIf(!kernelBuilt)('kernel bindings: AST-only pass ≡ full walk', () => {
  const files = FIXTURE_DIRS.filter((d) => fs.existsSync(d)).flatMap((d) => walk(d, []));
  expect(files.length).toBeGreaterThan(20);
  const covered: string[] = [];
  for (const file of files) {
    const source = fs.readFileSync(file, 'utf-8');
    const rel = path.relative(path.join(__dirname, 'fixtures'), file);
    const language = detectLanguage(file, source);
    const pass = tryKernelBindings(rel, source, language);
    if (pass === null) continue; // not a binding-emitting language
    const walked = tryKernelExtract(rel, source, language);
    if (!walked) continue; // the walker deferred; the pass is the only emitter
    covered.push(rel);
    it(`${rel} (${language})`, () => {
      const { onlyWalk, onlyPass } = diff((walked.bindings ?? []).map(key), pass.map(key));
      expect({ onlyWalk, onlyPass }).toEqual({ onlyWalk: [], onlyPass: [] });
    });
  }
  it('covers every binding language', () => {
    const langs = new Set(covered.map((f) => detectLanguage(f)));
    // tsx/jsx ride the typescript/javascript walker — the fixtures use them.
    for (const l of ['tsx', 'javascript', 'python', 'go', 'java', 'kotlin', 'php', 'c', 'cpp', 'rust']) {
      expect(langs, `no fixture covered ${l}`).toContain(l);
    }
  });
});
