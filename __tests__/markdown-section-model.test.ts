/**
 * Markdown keeps file and heading nodes plus the import nodes its links need.
 * Commands, table rows and list items are text inside their section: the
 * section docstring is searchable and already carries the text, so a node per
 * command/row/item only added a `contains` edge and resolution work. A section
 * command summary keeps a command past the docstring cap searchable, and a
 * script a command runs still keeps its `calls` reference from the section.
 */
import { describe, it, expect } from 'vitest';
import { extractFromSource } from '../src/extraction/tree-sitter';
import type { MarkdownExtractorContext } from '../src/extraction/markdown-extractor';

const CONTEXT: MarkdownExtractorContext = {
  knownFileNames: new Set(['guide.md', 'widget.ts', 'exists.sh', 'release.mjs']),
  pruneMissingTargets: true,
};

const SOURCE = [
  '```sh',
  'npx create-app',
  '```',
  '',
  '# Guide',
  '',
  'Intro about the guide.',
  '',
  '## Usage',
  '',
  'Run `npm install` first.',
  '',
  '```sh',
  'npm install',
  'bash scripts/exists.sh',
  '```',
  '',
  '```',
  '│ │ │',
  'PR opened / updated',
  '```',
  '',
  '| Tool | Path |',
  '| --- | --- |',
  '| widget | src/widget.ts |',
  '',
  '- see src/widget.ts::createWidget',
  '- XCG-713 quietbudgets feature',
  '',
  'This paragraph is deliberately long so the section preview fills its six',
  'hundred character budget before the next table and fence. It repeats a',
  'little prose about the widget, its installation, and how the pipeline',
  'moves from the CLI to the widget renderer and back again, to push the',
  'useful rows well past the preview cap. The widget guide keeps going so the',
  'preview is full: install, configure, render, inspect, repeat. The widget',
  'guide keeps going so the preview is full: install, configure, render.',
  'It keeps going a little further so the table header and the late list',
  'item are clearly outside the six hundred character preview budget.',
  '',
  '| RollbackWindow | Description |',
  '| --- | --- |',
  '| CG-713 | quietbudgets feature |',
  '',
  '- CG-713 quietbudgets feature',
  '',
  '```sh',
  'npm install',
  'npm run build',
  'npm run lint',
  'npm run test',
  'npm run typecheck',
  'npm run format',
  'npm run docs',
  'npm run e2e',
  'npm run rescueall',
  '```',
  '',
  '```',
  'node scripts/release.mjs',
  'codegraph init --no-seed',
  '```',
].join('\n');

function extract() {
  return extractFromSource('docs/guide.md', SOURCE, 'markdown', [], CONTEXT);
}

describe('Markdown section model', () => {
  it('keeps only file, heading and import nodes', () => {
    const kinds = new Set(extract().nodes.map((node) => node.kind));
    expect(kinds.has('file')).toBe(true);
    expect(kinds.has('module')).toBe(true);
    expect(kinds.has('function')).toBe(false);
    expect(kinds.has('constant')).toBe(false);
  });

  it('keeps commands searchable on the owning section', () => {
    const usage = extract().nodes.find((node) => node.kind === 'module' && node.name === 'Usage');
    expect(usage?.docstring).toContain('commands: npm install, bash scripts/exists.sh');
    // Output and tree lines in an unlabeled fence are not commands. (The
    // section's own text still carries them; only the summary must not.)
    const summary = usage?.docstring?.split('commands:')[1] ?? '';
    expect(summary).not.toContain('│');
    expect(summary).not.toContain('PR opened');
  });

  it('keeps a file-level command in the file digest', () => {
    const file = extract().nodes.find((node) => node.kind === 'file');
    expect(file?.docstring).toContain('commands: npx create-app');
  });

  it('keeps late rows, headers and commands searchable on the owning section', () => {
    const result = extract();
    const usage = result.nodes.find((node) => node.kind === 'module' && node.name === 'Usage')!;
    // The preview and the 8-command summary are bounded...
    expect(usage.docstring).not.toContain('rescueall');
    expect(usage.docstring).not.toContain('RollbackWindow');
    expect(usage.docstring).not.toContain('no-seed');
    // ...while the search text keeps the late row, header, 9th command and the
    // unrecognized unlabeled line. `XCG-713` is in the preview, so the late
    // `CG-713` also proves the token-boundary dedupe.
    expect(usage.searchText).toContain('RollbackWindow');
    expect(usage.searchText).toContain('CG-713');
    expect(usage.searchText).toContain('quietbudgets');
    expect(usage.searchText).toContain('rescueall');
    expect(usage.searchText).toContain('no-seed');
  });

  it('keeps an unlabeled Node.js command as a script reference', () => {
    const result = extract();
    const calls = result.unresolvedReferences.filter((ref) => ref.referenceKind === 'calls');
    expect(calls.some((ref) => ref.referenceName === 'docs/scripts/release.mjs')).toBe(true);
  });

  it('attaches command and table/list references to the section', () => {
    const result = extract();
    const usage = result.nodes.find((node) => node.kind === 'module' && node.name === 'Usage')!;
    const calls = result.unresolvedReferences.filter((ref) => ref.referenceKind === 'calls');
    expect(calls.some((ref) => ref.referenceName === 'docs/scripts/exists.sh' && ref.fromNodeId === usage.id)).toBe(true);
    const refs = result.unresolvedReferences.filter(
      (ref) => ref.referenceKind === 'references' && ref.referenceName.startsWith('docs/src/widget.ts')
    );
    expect(refs.length).toBeGreaterThan(0);
    expect(refs.every((ref) => ref.fromNodeId === usage.id)).toBe(true);
  });
});
