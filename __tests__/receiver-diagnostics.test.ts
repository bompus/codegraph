import { afterEach, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { DatabaseConnection } from '../src/db';
import { QueryBuilder } from '../src/db/queries';
import { runMigrations } from '../src/db/migrations';
import { ReferenceResolver } from '../src/resolution';

let db: DatabaseConnection | undefined;
let dir: string;
afterEach(() => { db?.close(); if (dir) fs.rmSync(dir, { recursive: true, force: true }); });
function setup() {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-receiver-reason-'));
  db = DatabaseConnection.initialize(path.join(dir, 'graph.db'));
  const raw = db.getDb();
  const queries = new QueryBuilder(raw);
  queries.insertNodes([{ id: 'caller', name: 'caller', qualifiedName: 'caller', kind: 'function', language: 'typescript', filePath: 'app.ts', startLine: 1, endLine: 5, startColumn: 0, endColumn: 0, updatedAt: 0 }]);
  queries.insertUnresolvedRefsBatch([1, 2].map(line => ({ fromNodeId: 'caller', referenceName: 'unknown.run', referenceKind: 'calls', filePath: 'app.ts', language: 'typescript', line, column: 0 })));
  return { raw, queries };
}
it('migrates existing unresolved rows without changing their retry state', () => {
  const { raw, queries } = setup();
  queries.markReferencesFailed([{ fromNodeId: 'caller', referenceName: 'unknown.run', referenceKind: 'calls' }]);
  raw.exec('ALTER TABLE unresolved_refs DROP COLUMN failure_reason');
  raw.exec('DELETE FROM schema_versions WHERE version = 12');
  runMigrations(raw, 11);
  expect(queries.getRetryableFailedReferences(['run'])).toHaveLength(2);
  expect(queries.getRetryableFailedReferences(['run']).every(r => r.failureReason === undefined)).toBe(true);
});
it('persists reasons by call site, exposes them, and clears a stale reason on a new outcome', () => {
  const { queries } = setup();
  const refs = queries.getUnresolvedReferences();
  queries.markReferencesFailedByRowIds([{ rowId: refs[0]!.rowId!, referenceName: 'unknown.run', failureReason: 'unknown-receiver' }]);
  expect(queries.getUnresolvedReferencesCount()).toBe(1);
  expect(queries.getUnresolvedReferencesInFile('app.ts').map(r => r.failureReason)).toEqual(['unknown-receiver', undefined]);
  const retry = queries.getRetryableFailedReferences(['run']);
  expect(retry).toHaveLength(1);
  queries.markReferencesFailedByRowIds([{ rowId: retry[0]!.rowId!, referenceName: 'unknown.run' }]);
  expect(queries.getRetryableFailedReferences(['run'])[0]!.failureReason).toBeUndefined();
});
it.each(['sync', 'yielding', 'batched'] as const)('records unknown receivers even without a same-named project method (%s)', async mode => {
  const { queries } = setup();
  const resolver = new ReferenceResolver(dir, queries);
  if (mode === 'sync') resolver.resolveAndPersist(queries.getUnresolvedReferences());
  else if (mode === 'yielding') await resolver.resolveAndPersistListYielding(queries.getUnresolvedReferences());
  else await resolver.resolveAndPersistBatched();
  expect(queries.getUnresolvedReferencesInFile('app.ts').map(r => r.failureReason)).toEqual(['unknown-receiver', 'unknown-receiver']);
});
