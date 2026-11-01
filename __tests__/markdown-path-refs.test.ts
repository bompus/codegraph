/**
 * Markdown path mentions are only resolvable through the file-path arm, which
 * collects candidates by basename. A target whose basename no indexed file has
 * therefore cannot resolve, and creating an import node plus a reference for
 * it only adds failed rows (and resolution work) to the graph. The extractor
 * skips those targets when it is given the index's known file basenames, while
 * keeping every target the resolver can still match: a live basename at a
 * different path (the suffix/basename fallback), symbols inside a live file,
 * and in-page anchors.
 */
import { describe, it, expect } from 'vitest';
import { extractFromSource } from '../src/extraction/tree-sitter';
import type { MarkdownExtractorContext } from '../src/extraction/markdown-extractor';

function known(...files: string[]): MarkdownExtractorContext {
  return {
    knownFileNames: new Set(files.map((file) => file.split('/').pop()!)),
  };
}

const SOURCE = [
  '# Guide',
  '',
  'Preamble about `createWidget`.',
  '',
  '## Links',
  '',
  '- [widget](../src/widget.ts)',
  '- [missing](../src/missing.ts)',
  '- [same name, other path](../apps/widget.ts#usage)',
  '- [pricing](/pricing)',
  '- [this section](#links)',
  '',
  'Mention `scripts/run.sh`, `scripts/widget.ts` and `src/widget.ts::createWidget`.',
  'A missing symbol target: `src/missing.ts::createWidget`.',
  '',
  '```sh',
  'bash scripts/missing.sh',
  'bash scripts/exists.sh',
  '```',
].join('\n');

function refs(context?: MarkdownExtractorContext): string[] {
  return extractFromSource('docs/guide.md', SOURCE, 'markdown', [], context)
    .unresolvedReferences.map((ref) => ref.referenceName);
}

function importTargets(context?: MarkdownExtractorContext): string[] {
  return extractFromSource('docs/guide.md', SOURCE, 'markdown', [], context)
    .nodes.filter((node) => node.kind === 'import')
    .map((node) => node.qualifiedName);
}

describe('Markdown path-existence check', () => {
  it('keeps every path target when no known-file set is supplied', () => {
    const names = refs();
    expect(names).toContain('src/widget.ts');
    expect(names).toContain('src/missing.ts');
    expect(names).toContain('docs/scripts/run.sh');
    expect(importTargets().some((qn) => qn.includes('missing.ts'))).toBe(true);
  });

  it('drops targets no indexed file can satisfy', () => {
    const context = known('docs/guide.md', 'src/widget.ts', 'scripts/exists.sh');
    const names = refs(context);
    expect(names).not.toContain('src/missing.ts');
    expect(names).not.toContain('scripts/run.sh');
    expect(names).not.toContain('scripts/missing.sh');
    expect(names.some((name) => name.startsWith('src/missing.ts::'))).toBe(false);
    expect(importTargets(context).some((qn) => qn.includes('missing.ts'))).toBe(false);
  });

  it('keeps targets the resolver reaches by basename, symbol, anchor or command', () => {
    const context = known('docs/guide.md', 'src/widget.ts', 'scripts/exists.sh');
    const names = refs(context);
    // A live basename at another path still resolves through the suffix and
    // basename arms, so it must survive.
    expect(names).toContain('apps/widget.ts#usage');
    expect(names).toContain('docs/scripts/widget.ts');
    // A bare name is not a file path: `/pricing` normalizes to `pricing`,
    // which the resolver can match to a route or module node by name.
    expect(names).toContain('pricing');
    // A symbol inside a live file.
    expect(names).toContain('docs/src/widget.ts::createWidget');
    // A pure in-page anchor names this file.
    expect(names.some((name) => name.startsWith('docs/guide.md#'))).toBe(true);
    // A script the fence runs.
    expect(names).toContain('docs/scripts/exists.sh');
  });
});
