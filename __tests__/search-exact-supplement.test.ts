/**
 * The exact-name supplement adds whole-name matches BM25 may have left out of
 * the candidate fetch. It keeps 20 rows per term, so it must take definitions
 * before the import rows of a name the whole project imports. It also looks
 * up the query's words joined into one name and a one-word query's stems,
 * spellings the FTS prefix match does not reach.
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
    // Single-word names that match one query word, many more than the fetch.
    for (let i = 0; i < 120; i++) {
      q.insertNode(makeNode(`add-${i}`, 'function', 'add', `src/pkg${i}/add.py`));
      q.insertNode(makeNode(`map-${i}`, 'function', 'map', `src/pkg${i}/map.py`));
      q.insertNode(makeNode(`svelte-${i}`, 'variable', 'svelte', `src/pkg${i}/cfg.py`));
      q.insertNode(makeNode(`mounting-${i}`, 'variable', `mounting_state_${i}`, `src/m${i}.py`));
    }
    q.insertNode(makeNode('add-url-rule', 'method', 'add_url_rule', 'src/flask/sansio/app.py'));
    q.insertNode(makeNode('svelte-map', 'class', 'SvelteMap', 'src/reactivity/map.py'));
    q.insertNode(makeNode('mount', 'function', 'mount', 'src/render.py'));
  });

  afterAll(() => {
    conn.close();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('finds a class whose name every module imports', () => {
    const ids = q.searchNodes('Flask', { limit: 10 }).map((r) => r.node.id);
    expect(ids[0]).toBe('flask-class');
  });

  it('finds a snake_case name from its words', () => {
    expect(q.searchNodes('add url rule', { limit: 10 })[0]?.node.id).toBe('add-url-rule');
  });

  it('finds a camelCase name from its lowercase words', () => {
    expect(q.searchNodes('svelte map', { limit: 10 })[0]?.node.id).toBe('svelte-map');
  });

  it('finds the base form of a one-word query', () => {
    expect(q.searchNodes('mounting', { limit: 10 }).map((r) => r.node.id)).toContain('mount');
  });
});
