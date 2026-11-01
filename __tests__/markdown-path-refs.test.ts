/**
 * Markdown path mentions are only resolvable through the file-path arm, which
 * collects candidates by basename. A relative file-shaped target whose
 * basename no indexed file has therefore cannot resolve. On a full index the
 * extractor skips those targets, while keeping everything the resolver can
 * still match: routes (root-absolute or extensionless), modules and table
 * rows named without a file, symbols inside a live file, and in-page anchors.
 * Incremental sync keeps every target so one added later can still resolve
 * from the parked reference.
 */
import { describe, it, expect } from 'vitest';
import { extractFromSource } from '../src/extraction/tree-sitter';
import type { MarkdownExtractorContext } from '../src/extraction/markdown-extractor';

function context(prune: boolean, ...files: string[]): MarkdownExtractorContext {
  return {
    knownFileNames: new Set(files.map((file) => file.split('/').pop()!)),
    pruneMissingTargets: prune,
  };
}

const FULL_INDEX = context(true, 'docs/guide.md', 'src/widget.ts', 'scripts/exists.sh');
const SYNC = context(false, 'docs/guide.md', 'src/widget.ts', 'scripts/exists.sh');

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
  '- [users route](/api/users/)',
  '- [guide route](../guides/auth)',
  '- [diagram](../assets/releaseblueprint.svg)',
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

function extract(withContext?: MarkdownExtractorContext) {
  return extractFromSource('docs/guide.md', SOURCE, 'markdown', [], withContext);
}

function refs(withContext?: MarkdownExtractorContext): string[] {
  return extract(withContext).unresolvedReferences.map((ref) => ref.referenceName);
}

function importTargets(withContext?: MarkdownExtractorContext): string[] {
  return extract(withContext)
    .nodes.filter((node) => node.kind === 'import')
    .map((node) => node.qualifiedName);
}

describe('Markdown path-existence check', () => {
  it('keeps every path target when no context is supplied', () => {
    const names = refs();
    expect(names).toContain('src/widget.ts');
    expect(names).toContain('src/missing.ts');
    expect(names).toContain('docs/scripts/run.sh');
    expect(importTargets().some((qn) => qn.includes('missing.ts'))).toBe(true);
  });

  it('drops relative file-shaped targets no indexed file can satisfy', () => {
    const names = refs(FULL_INDEX);
    expect(names).not.toContain('src/missing.ts');
    expect(names).not.toContain('docs/scripts/run.sh');
    expect(names).not.toContain('docs/scripts/missing.sh');
    expect(names.some((name) => name.startsWith('src/missing.ts::'))).toBe(false);
    expect(importTargets(FULL_INDEX).some((qn) => qn.includes('missing.ts'))).toBe(false);
  });

  it('keeps sync references so a target added later can resolve', () => {
    const names = refs(SYNC);
    expect(names).toContain('src/missing.ts');
    expect(names).toContain('docs/scripts/run.sh');
    expect(importTargets(SYNC).some((qn) => qn.includes('missing.ts'))).toBe(true);
  });

  it('keeps routes and targets the resolver reaches by basename, symbol, anchor or command', () => {
    const names = refs(FULL_INDEX);
    // A live basename at another path still resolves through the suffix and
    // basename arms, so it must survive.
    expect(names).toContain('apps/widget.ts#usage');
    expect(names).toContain('docs/scripts/widget.ts');
    // Root-absolute and extensionless links can name routes, modules or table
    // rows; the file-path arm is not their only route to a match.
    expect(names).toContain('pricing');
    expect(names).toContain('api/users/');
    expect(names).toContain('guides/auth');
    // A symbol inside a live file.
    expect(names).toContain('docs/src/widget.ts::createWidget');
    // A pure in-page anchor names this file.
    expect(names).toContain('docs/guide.md#links');
    // A script the fence runs.
    expect(names).toContain('docs/scripts/exists.sh');
  });

  it('keeps a pruned target in the file digest for search', () => {
    const result = extract(FULL_INDEX);
    const fileNode = result.nodes.find((node) => node.kind === 'file');
    expect(fileNode?.docstring).toContain('releaseblueprint.svg');
  });
});
