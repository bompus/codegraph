import { describe, it, expect, afterAll } from 'vitest';
import { highlightLines, clearHighlightCache } from '../src/ui-server/highlight';
import { tokenizeBounded, stopHighlightWorker } from '../src/ui-server/highlight/bounded-tokenize';

afterAll(() => stopHighlightWorker());

describe('highlighting a slice', () => {
  it('finishes a COBOL slice ending in the sequence area', async () => {
    clearHighlightCache();
    const started = Date.now();
    const slice = await highlightLines(['    PERFORM AssertOk', '    .'], { language: 'cobol' });
    expect(slice.reason ?? '').not.toMatch(/took too long/);
    expect(Date.now() - started).toBeLessThan(10_000);
    const fine = await highlightLines(['export const x: number = 1;'], { language: 'typescript' });
    expect(fine.engine).toBe('tree-sitter');
  }, 20_000);

  it('reaps a child past its deadline and classifies the next slice', async () => {
    await stopHighlightWorker();
    expect(await tokenizeBounded('export const x = 1;', 'typescript', 0)).toEqual({ timedOut: true });
    const next = await tokenizeBounded('export const value: number = 1;', 'typescript');
    expect('result' in next && next.result?.spans.length).toBeGreaterThan(0);
  });
});
