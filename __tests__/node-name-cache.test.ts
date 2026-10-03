import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { DatabaseConnection } from '../src/db';
import { QueryBuilder } from '../src/db/queries';
import { Node } from '../src/types';

function makeNode(name: string): Node {
  return {
    id: name,
    kind: 'function',
    name,
    qualifiedName: name,
    filePath: 'src/users.ts',
    language: 'typescript',
    startLine: 1,
    endLine: 1,
    startColumn: 0,
    endColumn: 0,
    updatedAt: Date.now(),
  };
}

// The fuzzy fallback reads the distinct-name list from a cache; a write from
// this connection or another one must not leave it stale.
describe('distinct node-name cache', () => {
  let dir: string;
  let connections: DatabaseConnection[];

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-name-cache-'));
    connections = [];
  });

  afterEach(() => {
    for (const connection of connections) connection.close();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  function track(connection: DatabaseConnection): QueryBuilder {
    connections.push(connection);
    return new QueryBuilder(connection.getDb());
  }

  it('sees a node this connection inserts after the list was read', () => {
    const queries = track(DatabaseConnection.initialize(path.join(dir, 'test.db')));
    queries.insertNode(makeNode('getUser'));
    expect(queries.searchNodes('getUsre').map((r) => r.node.name)).toEqual(['getUser']);
    queries.insertNode(makeNode('loadAccount'));
    expect(queries.searchNodes('loadAcount').map((r) => r.node.name)).toEqual(['loadAccount']);
  });

  it('sees a node another connection commits after the list was read', () => {
    const reader = track(DatabaseConnection.initialize(path.join(dir, 'test.db')));
    reader.insertNode(makeNode('getUser'));
    expect(reader.getAllNodeNames()).toEqual(['getUser']);
    const writer = track(DatabaseConnection.open(path.join(dir, 'test.db')));
    writer.insertNode(makeNode('loadAccount'));
    expect(reader.getAllNodeNames().sort()).toEqual(['getUser', 'loadAccount']);
    expect(reader.searchNodes('loadAcount').map((r) => r.node.name)).toEqual(['loadAccount']);
  });

  it('drops the list when rebound to another database', () => {
    // Fresh connections to two databases report the same change stamp.
    for (const [file, name] of [['a.db', 'getUser'], ['b.db', 'loadAccount']]) {
      const writer = DatabaseConnection.initialize(path.join(dir, file));
      new QueryBuilder(writer.getDb()).insertNode(makeNode(name));
      writer.close();
    }
    const reader = track(DatabaseConnection.open(path.join(dir, 'a.db')));
    expect(reader.getAllNodeNames()).toEqual(['getUser']);
    const other = DatabaseConnection.open(path.join(dir, 'b.db'));
    connections.push(other);
    reader.rebind(other.getDb());
    expect(reader.getAllNodeNames()).toEqual(['loadAccount']);
  });
});
