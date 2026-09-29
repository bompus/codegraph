import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { extractFromSource } from '../src/extraction';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';

describe('kernel parse-collapse warning', () => {
  beforeAll(async () => {
    await initGrammars();
    await loadGrammarsForLanguages(['python']);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('stays valid JSON when the path holds a control character', () => {
    const stderr = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    const result = extractFromSource('dir\tname.py', ')))');

    // Unescaped, the tab made the kernel's warning invalid JSON: the walker
    // was reported as failed and the generic extractor took the file.
    expect(stderr.mock.calls.map(([chunk]) => String(chunk)).join('')).not.toContain('walker failed');
    expect(result.errors.map((e) => e.message)).toEqual([
      'dir\tname.py: parse produced no symbols (tree has errors) — ' +
        'the file is indexed but contributes nothing to the graph',
    ]);
  });
});
