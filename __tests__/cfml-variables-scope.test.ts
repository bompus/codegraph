/**
 * CFML's `variables.m()` is a call on the component itself, like `this.m()`.
 * The untyped-receiver rule from upstream #2147/#2148 (a receiver must be
 * named after the method's component) must not drop it.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-cfml-variables-'));
  const files: Record<string, string> = {
    'models/Report.cfc': `component {
  function formatRows( rows ) {
    return rows;
  }
  function render( rows ) {
    return variables.formatRows( rows );
  }
}
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

describe('CFML variables-scope calls', () => {
  it('reach the component’s own method', () => {
    const ids = cg.getNodesInFile('models/Report.cfc').map((n) => n.id);
    const targets = cg
      .getOutgoingEdgesFrom(ids)
      .filter((e) => e.kind === 'calls')
      .map((e) => cg.getNode(e.target)!.qualifiedName);
    expect(targets).toContain('Report::formatRows');
  });
});
