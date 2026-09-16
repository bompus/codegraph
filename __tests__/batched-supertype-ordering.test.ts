/**
 * Cross-batch supertype-edge visibility.
 *
 * The batched loop partitions each batch's edges: `extends`/`implements`
 * edges are committed BEFORE the next batch fans out (resolveMethodOnType
 * walks supertype chains over them — the dubbo-validated barrier), while
 * every other edge kind persists AFTER fan-out, overlapped with the next
 * batch's resolution. This test pins the behavioral invariant: a call whose
 * resolution needs a supertype edge produced by an EARLIER batch still
 * resolves. The concurrent-visibility half of the invariant is covered by
 * the corpus identity gate; here the sequential-mode ordering proves the
 * partition doesn't drop, reorder or misclassify the eager set.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { DatabaseConnection } from '../src/db';
import { QueryBuilder } from '../src/db/queries';
import { createResolver } from '../src/resolution';
import { Node, UnresolvedReference } from '../src/types';

function makeNode(
  id: string,
  name: string,
  kind: Node['kind'],
  filePath: string,
  startLine: number,
  qualifiedName = name,
): Node {
  return {
    id,
    kind,
    name,
    qualifiedName,
    filePath,
    language: 'java',
    startLine,
    endLine: startLine + 2,
    startColumn: 0,
    endColumn: 0,
    updatedAt: Date.now(),
  };
}

describe('Cross-batch supertype edge visibility', () => {
  let dir: string;
  let db: DatabaseConnection;
  let q: QueryBuilder;

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-supertype-'));
    db = DatabaseConnection.initialize(path.join(dir, 'test.db'));
    q = new QueryBuilder(db.getDb());
    fs.writeFileSync(
      path.join(dir, 'Types.java'),
      'class Base { void greet() {} }\nclass Sub extends Base {}\n',
    );
    fs.writeFileSync(
      path.join(dir, 'Caller.java'),
      'class Caller { void run() { Sub s = new Sub(); s.greet(); } }\n',
    );
    q.insertNode(makeNode('class:Base', 'Base', 'class', 'Types.java', 1));
    q.insertNode(makeNode('class:Sub', 'Sub', 'class', 'Types.java', 2));
    q.insertNode(makeNode('method:Base.greet', 'greet', 'method', 'Types.java', 1, 'Base::greet'));
    q.insertNode(makeNode('method:Caller.run', 'run', 'method', 'Caller.java', 1, 'Caller::run'));
  });

  afterEach(() => {
    db.close();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('a call needing an earlier batch\'s extends edge still resolves via the supertype walk', async () => {
    // Row order = insertion order: the extends ref is batch 1, the receiver
    // call batch 2. `s.greet` infers `Sub` from `Sub s = new Sub()` in
    // Caller.java, finds no `Sub::greet`, and only resolves by walking the
    // committed Sub→Base extends edge to `Base::greet`.
    const refs: UnresolvedReference[] = [
      {
        fromNodeId: 'class:Sub',
        referenceName: 'Base',
        referenceKind: 'extends',
        line: 2,
        column: 23,
        filePath: 'Types.java',
        language: 'java',
      },
      {
        fromNodeId: 'method:Caller.run',
        referenceName: 's.greet',
        referenceKind: 'calls',
        line: 1,
        column: 55,
        filePath: 'Caller.java',
        language: 'java',
      },
    ];
    q.insertUnresolvedRefsBatch(refs);

    const resolver = createResolver(dir, q);
    await resolver.resolveAndPersistBatched(undefined, 1);

    const extendsEdges = q
      .getOutgoingEdges('class:Sub')
      .filter((e) => e.kind === 'extends' && e.target === 'class:Base');
    expect(extendsEdges).toHaveLength(1);

    const callEdges = q
      .getOutgoingEdges('method:Caller.run')
      .filter((e) => e.kind === 'calls' && e.target === 'method:Base.greet');
    expect(callEdges).toHaveLength(1);
    expect(q.getUnresolvedReferencesCount()).toBe(0);
  });

  it('non-supertype edges persist alongside supertype ones in the same batch', async () => {
    // Same batch holds an extends ref and a plain call — the partition must
    // not drop either half.
    q.insertNode(makeNode('fn:callee', 'callee', 'function', 'Callee.java', 1));
    fs.writeFileSync(path.join(dir, 'Callee.java'), 'class Callee { static void callee() {} }\n');
    q.insertUnresolvedRefsBatch([
      {
        fromNodeId: 'method:Caller.run',
        referenceName: 'callee',
        referenceKind: 'calls',
        line: 1,
        column: 40,
        filePath: 'Caller.java',
        language: 'java',
      },
      {
        fromNodeId: 'class:Sub',
        referenceName: 'Base',
        referenceKind: 'extends',
        line: 2,
        column: 23,
        filePath: 'Types.java',
        language: 'java',
      },
    ]);

    const resolver = createResolver(dir, q);
    await resolver.resolveAndPersistBatched(undefined, 10);

    expect(
      q.getOutgoingEdges('method:Caller.run').filter((e) => e.kind === 'calls' && e.target === 'fn:callee'),
    ).toHaveLength(1);
    expect(
      q.getOutgoingEdges('class:Sub').filter((e) => e.kind === 'extends' && e.target === 'class:Base'),
    ).toHaveLength(1);
    expect(q.getUnresolvedReferencesCount()).toBe(0);
  });
});
