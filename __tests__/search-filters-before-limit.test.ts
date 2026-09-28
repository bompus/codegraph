/**
 * `path:` and `name:` are hard filters, so they must narrow the candidate set
 * before `limit` cuts it. Applied after the cut, a query whose best-scoring
 * matches sit outside the filter returns nothing even though matching nodes
 * exist.
 */

import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { DatabaseConnection } from '../src/db';
import { QueryBuilder } from '../src/db/queries';
import { Node } from '../src/types';

function makeNode(id: string, name: string, filePath: string): Node {
  return {
    id,
    kind: 'function',
    name,
    qualifiedName: name,
    filePath,
    language: 'typescript',
    startLine: 1,
    endLine: 2,
    startColumn: 0,
    endColumn: 0,
    updatedAt: Date.now(),
  };
}

describe('searchNodes applies path: and name: before the limit', () => {
  let dir: string;
  let conn: DatabaseConnection;
  let q: QueryBuilder;

  beforeAll(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'search-filters-'));
    conn = DatabaseConnection.initialize(path.join(dir, 'test.db'));
    q = new QueryBuilder(conn.getDb());
    // 150 exact `render` matches under src/ outrank the one under lib/, and
    // outnumber the FTS candidate fetch for a small limit (max(limit*5, 100)).
    // Both lib/ names sort after `render`, past a filter-only fetch too.
    for (let i = 0; i < 150; i++) q.insertNode(makeNode(`src-${i}`, 'render', `src/views/v${i}.ts`));
    q.insertNode(makeNode('lib-target', 'renderLegacyWidget', 'lib/legacy/widget.ts'));
    q.insertNode(makeNode('lib-other', 'zoomPaint', 'lib/legacy/paint.ts'));
    // SQLite's lower() folds ASCII only; these match only with Unicode folding.
    q.insertNode(makeNode('eclair', 'renderÉclair', 'src/Pâtisserie/Éclair.ts'));
    q.insertNode(makeNode('kelvin', 'renderTemp', 'src/\u212Aelvin/temp.ts'));
    // A word-final Σ lowercases to ς, not σ.
    q.insertNode(makeNode('sigma', 'renderΟΔΟΣ', 'src/greek/odos.ts'));
  });

  afterAll(() => {
    conn.close();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('path: finds a match that ranks below the limit', () => {
    const ids = q.searchNodes('render path:lib/legacy', { limit: 5 }).map((r) => r.node.id);
    expect(ids).toEqual(['lib-target']);
  });

  it('name: finds a match that ranks below the limit', () => {
    const ids = q.searchNodes('render name:legacy', { limit: 5 }).map((r) => r.node.id);
    expect(ids).toEqual(['lib-target']);
  });

  it('a filter-only query finds a match past the first limit*5 names', () => {
    const ids = q.searchNodes('path:lib/legacy', { limit: 1 }).map((r) => r.node.id);
    expect(ids).toHaveLength(1);
    expect(['lib-target', 'lib-other']).toContain(ids[0]);
  });

  it('name: and path: fold non-ASCII case like the JS gate', () => {
    expect(q.searchNodes('render name:éclair', { limit: 5 }).map((r) => r.node.id)).toEqual(['eclair']);
    expect(q.searchNodes('render path:pâtisserie/éclair', { limit: 5 }).map((r) => r.node.id)).toEqual(['eclair']);
    expect(q.searchNodes('render path:PÂTISSERIE', { limit: 5 }).map((r) => r.node.id)).toEqual(['eclair']);
  });

  it('a non-ASCII filter narrows the candidates too, not only the final gate', () => {
    // Filter-only fetches go in name order, and all 150 `render` rows sort
    // ahead of `renderÉclair`; the `render` query ranks them above it as well.
    expect(q.searchNodes('path:PÂTISSERIE', { limit: 1 }).map((r) => r.node.id)).toEqual(['eclair']);
    expect(q.searchNodes('name:ÉCLAIR', { limit: 1 }).map((r) => r.node.id)).toEqual(['eclair']);
    expect(q.searchNodes('render path:pâtisserie', { limit: 1 }).map((r) => r.node.id)).toEqual(['eclair']);
    expect(q.searchNodes('render name:éclair path:src', { limit: 1 }).map((r) => r.node.id)).toEqual(['eclair']);
    expect(q.searchNodes('name:οδος', { limit: 1 }).map((r) => r.node.id)).toEqual(['sigma']);
  });

  it('an ASCII filter still matches a code point that lowercases into ASCII', () => {
    // U+212A KELVIN SIGN lowercases to `k`.
    expect(q.searchNodes('render path:kelvin', { limit: 5 }).map((r) => r.node.id)).toEqual(['kelvin']);
  });

  it('still caps the filtered results at the limit', () => {
    expect(q.searchNodes('render path:src/views', { limit: 5 })).toHaveLength(5);
  });
});
