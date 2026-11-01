/**
 * A Markdown path target whose basename no indexed file has cannot resolve to
 * a file through the resolver's file-path arm. Its reference is still kept —
 * the resolver's route/module/symbol arms may match it, and sync can retry it
 * when a file appears later (#1240) — but its own import node only added a
 * search child and a `contains` edge. A full index skips that node; sync keeps
 * every node. Pruned targets stay in a separate, larger `links:` part of the
 * file digest so a link beyond the 12-item display digest is still searchable.
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
  '- [dotted route](api/users.json)',
  '- [one](../src/one.ts)',
  '- [two](../src/two.ts)',
  '- [three](../src/three.ts)',
  '- [four](../src/four.ts)',
  '- [five](../src/five.ts)',
  '- [six](../src/six.ts)',
  '- [seven](../src/seven.ts)',
  '- [eight](../src/eight.ts)',
  '- [nine](../src/nine.ts)',
  '- [ten](../src/ten.ts)',
  '- [eleven](../src/eleven.ts)',
  '- [twelve](../src/twelve.ts)',
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

describe('Markdown link-node pruning', () => {
  it('keeps every reference and link node without a context', () => {
    const names = refs();
    expect(names).toContain('src/widget.ts');
    expect(names).toContain('src/missing.ts');
    expect(names).toContain('docs/scripts/run.sh');
    expect(importTargets().some((qn) => qn.includes('missing.ts'))).toBe(true);
  });

  it('keeps the reference but skips the link node on a full index', () => {
    const names = refs(FULL_INDEX);
    // The reference survives so routes, modules and later syncs can match it.
    expect(names).toContain('src/missing.ts');
    expect(names).toContain('docs/scripts/run.sh');
    expect(names).toContain('docs/scripts/missing.sh');
    // The dead target's import node does not exist.
    expect(importTargets(FULL_INDEX).some((qn) => qn.includes('missing.ts'))).toBe(false);
    // A target whose basename is known keeps its node.
    expect(importTargets(FULL_INDEX).some((qn) => qn.includes('src/widget.ts'))).toBe(true);
  });

  it('keeps every link node on an incremental sync', () => {
    expect(importTargets(SYNC).some((qn) => qn.includes('missing.ts'))).toBe(true);
  });

  it('keeps routes and targets the resolver reaches by name', () => {
    const names = refs(FULL_INDEX);
    expect(names).toContain('apps/widget.ts#usage');
    expect(names).toContain('docs/scripts/widget.ts');
    expect(names).toContain('pricing');
    expect(names).toContain('api/users/');
    expect(names).toContain('guides/auth');
    // A relative route with a dotted final segment is kept for name matching.
    expect(names).toContain('docs/api/users.json');
    expect(names).toContain('docs/src/widget.ts::createWidget');
    expect(names).toContain('docs/guide.md#links');
    expect(names).toContain('docs/scripts/exists.sh');
  });

  it('keeps a pruned target searchable beyond the display digest budget', () => {
    const fileNode = extract(FULL_INDEX).nodes.find((node) => node.kind === 'file');
    // The target is the 13th link, past the 12-item `refs:` budget.
    expect(fileNode?.docstring).toContain('links:');
    expect(fileNode?.docstring).toContain('releaseblueprint.svg');
  });
});
