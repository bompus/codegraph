import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

describe('a value parameter beside an imported type', () => {
  let root: string;
  let graph: CodeGraph;
  beforeAll(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-type-value-'));
    fs.writeFileSync(path.join(root, 'widget.ts'), 'export class Widget {}\nexport type Size = { width: number };\n');
    fs.writeFileSync(path.join(root, 'use.ts'), `import { Widget, type Size } from './widget';
export function annotation(Widget: number) { const value: Widget = {}; return value; }
export function query(Widget: number) { type Value = typeof Widget; return Widget; }
export function value(Widget: number) { return Widget; }
export function measure(Size: number): number { const size: Size = { width: Size }; return size.width; }
`);
    graph = await CodeGraph.init(root, { index: true });
  });
  afterAll(() => { graph?.close(); fs.rmSync(root, { recursive: true, force: true }); });
  function references(name: string): string[] {
    const caller = graph.getNodesByKind('function').find((node) => node.name === name)!;
    return graph.getOutgoingEdges(caller.id).filter((edge) => edge.kind === 'references')
      .map((edge) => graph.getNode(edge.target)!).filter((node) => node.filePath === 'widget.ts').map((node) => node.name);
  }
  it('keeps the imported class in an annotation', () => { expect(references('annotation')).toContain('Widget'); });
  it('keeps the exact upstream Size annotation', () => { expect(references('measure')).toContain('Size'); });
  it('keeps typeof in the value namespace', () => { expect(references('query')).not.toContain('Widget'); });
  it('keeps a returned value in the value namespace', () => { expect(references('value')).not.toContain('Widget'); });
});
