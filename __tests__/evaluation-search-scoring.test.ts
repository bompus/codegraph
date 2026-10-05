import { describe, expect, it } from 'vitest';
import { scoreSearchNodes } from './evaluation/scoring';

const results = (names: string[]) => names.map((name) => ({ node: { name }, score: 1 }));

describe('scoreSearchNodes', () => {
  it('uses the highest-ranked expected symbol regardless of expectation order', () => {
    const ranked = results(['Noise', 'Early', 'Other', 'Late']);
    const forward = scoreSearchNodes('search-order', ['Late', 'Early'], ranked, 17);
    const reversed = scoreSearchNodes('search-order', ['Early', 'Late'], ranked, 17);

    expect(forward).toEqual({
      caseId: 'search-order', pass: true, recall: 1, mrr: 1 / 2,
      foundSymbols: ['Late', 'Early'], missedSymbols: [], latencyMs: 17,
    });
    expect(reversed).toEqual({ ...forward, foundSymbols: ['Early', 'Late'] });
  });

  it('skips missing expectations and matches names without case sensitivity', () => {
    const score = scoreSearchNodes('search-partial', ['Missing', 'LATE', 'EARLY'],
      results(['early', 'Noise', 'late']), 9);

    expect(score).toEqual({
      caseId: 'search-partial', pass: true, recall: 2 / 3, mrr: 1,
      foundSymbols: ['LATE', 'EARLY'], missedSymbols: ['Missing'], latencyMs: 9,
    });
  });

  it('uses the first result occurrence for a single expected symbol', () => {
    const score = scoreSearchNodes('search-single', ['Target'],
      results(['Noise', 'Target', 'Target']), 0);

    expect(score.mrr).toBe(1 / 2);
    expect(score.recall).toBe(1);
    expect(score.pass).toBe(true);
  });

  it.each([
    { expected: ['Missing'], ranked: ['Other'], missed: ['Missing'] },
    { expected: ['Missing'], ranked: [], missed: ['Missing'] },
    { expected: [], ranked: ['Other'], missed: [] },
  ])('returns zero reciprocal rank and recall with no match ($expected, $ranked)', ({ expected, ranked, missed }) => {
    expect(scoreSearchNodes('search-no-hit', expected, results(ranked), 0)).toEqual({
      caseId: 'search-no-hit', pass: false, recall: 0, mrr: 0,
      foundSymbols: [], missedSymbols: missed, latencyMs: 0,
    });
  });
});
