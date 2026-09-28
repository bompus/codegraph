/**
 * The exact-name supplement adds whole-name matches BM25 may have left out of
 * the candidate fetch. It keeps 20 rows per term, so it must take definitions
 * before the import rows of a name the whole project imports.
 */

import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { DatabaseConnection } from '../src/db';
import { QueryBuilder } from '../src/db/queries';
import { Node, NodeKind } from '../src/types';

function makeNode(id: string, kind: NodeKind, name: string, filePath: string): Node {
  return {
    id,
    kind,
    name,
    qualifiedName: name,
    filePath,
    language: 'python',
    startLine: 1,
    endLine: 2,
    startColumn: 0,
    endColumn: 0,
    updatedAt: Date.now(),
  };
}

describe('searchNodes exact-name supplement', () => {
  let dir: string;
  let conn: DatabaseConnection;
  let q: QueryBuilder;

  beforeAll(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'search-exact-'));
    conn = DatabaseConnection.initialize(path.join(dir, 'test.db'));
    q = new QueryBuilder(conn.getDb());
    // `import flask` and `from flask import Flask` in every test module: more
    // rows than the FTS fetch (max(limit*5, 100)), in both cases of the name.
    for (let i = 0; i < 120; i++) {
      q.insertNode(makeNode(`imp-${i}`, 'import', 'flask', `tests/test_${i}.py`));
      q.insertNode(makeNode(`imp-cls-${i}`, 'import', 'Flask', `tests/test_${i}.py`));
    }
    q.insertNode(makeNode('flask-class', 'class', 'Flask', 'src/flask/app.py'));
  });

  afterAll(() => {
    conn.close();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('finds a class whose name every module imports', () => {
    const ids = q.searchNodes('Flask', { limit: 10 }).map((r) => r.node.id);
    expect(ids[0]).toBe('flask-class');
  });
});
