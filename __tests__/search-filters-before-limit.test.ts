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

  it('still caps the filtered results at the limit', () => {
    expect(q.searchNodes('render path:src/views', { limit: 5 })).toHaveLength(5);
  });
});
