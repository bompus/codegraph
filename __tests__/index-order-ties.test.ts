import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let dir: string;
let cg: CodeGraph;

// Node's readdirSync returns names sorted; Bun's returns the filesystem's own
// order. Equal-score search hits fall back to indexing order, so the walk must
// read directories in name order on both runtimes.
beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-index-order-'));
  for (const name of ['large-doc2.md', 'large-doc0.md', 'large-doc1.md']) {
    fs.writeFileSync(path.join(dir, name), '## Section300\n\nSame text in every file.\n');
  }
  cg = CodeGraph.initSync(dir);
  await cg.indexAll();
}, 60_000);

afterAll(() => {
  cg?.destroy();
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});

describe('indexing order', () => {
  it('breaks equal-score search ties in file-name order', () => {
    const files = cg.searchNodes('Section300', { limit: 10 }).map((r) => r.node.filePath);
    expect(files).toEqual(['large-doc0.md', 'large-doc1.md', 'large-doc2.md']);
  });
});
