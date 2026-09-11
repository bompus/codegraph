import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createDatabase, type SqliteDatabase } from '../src/db/sqlite-adapter';

describe('SQLite transaction recovery', () => {
  let dir: string;
  let db: SqliteDatabase;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'cg-transaction-'));
    db = createDatabase(join(dir, 'test.db')).db;
    db.exec('CREATE TABLE items (id INTEGER PRIMARY KEY)');
  });

  afterEach(() => {
    db.close();
    rmSync(dir, { recursive: true, force: true });
  });

  function expectAtomicRecovery() {
    const error = new Error('later failure');
    expect(() => db.transaction(() => {
      db.exec('INSERT INTO items VALUES (2)');
      throw error;
    })()).toThrow(error);
    expect(db.prepare('SELECT * FROM items').all()).toEqual([]);
    expect(db.transaction((id: number) => {
      db.prepare('INSERT INTO items VALUES (?)').run(id);
      return id;
    })(3)).toBe(3);
    expect(db.prepare('SELECT id FROM items').all()).toEqual([{ id: 3 }]);
  }

  it('preserves the constraint error when SQLite has already rolled back', () => {
    expect(() => db.transaction(() => {
      db.exec('INSERT INTO items VALUES (1)');
      db.exec('INSERT OR ROLLBACK INTO items VALUES (1)');
    })()).toThrow('UNIQUE constraint failed');
  });

  it('keeps subsequent failing transactions atomic after automatic rollback', () => {
    try {
      db.transaction(() => {
        db.exec('INSERT INTO items VALUES (1)');
        db.exec('INSERT OR ROLLBACK INTO items VALUES (1)');
      })();
    } catch {
      // The assertion below detects autocommit even if the first error is masked.
    }
    expectAtomicRecovery();
  });

  it('recovers after a normal callback failure', () => {
    const error = new Error('callback failure');
    expect(() => db.transaction(() => {
      db.exec('INSERT INTO items VALUES (1)');
      throw error;
    })()).toThrow(error);
    expectAtomicRecovery();
  });

  it('rolls back successful nested work when its enclosing transaction fails', () => {
    expect(() => db.transaction(() => {
      db.transaction(() => db.exec('INSERT INTO items VALUES (1)'))();
      throw new Error('outer failure');
    })()).toThrow('outer failure');
    expectAtomicRecovery();
  });

  it('recovers after a nested callback failure', () => {
    expect(() => db.transaction(() => {
      db.exec('INSERT INTO items VALUES (1)');
      db.transaction(() => { throw new Error('nested failure'); })();
    })()).toThrow('nested failure');
    expectAtomicRecovery();
  });

  it('rolls back a deferred constraint failure during commit', () => {
    db.exec('PRAGMA foreign_keys = ON');
    db.exec('CREATE TABLE children (parent INTEGER REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED)');
    expect(() => db.transaction(() => {
      db.exec('INSERT INTO children VALUES (99)');
    })()).toThrow('FOREIGN KEY constraint failed');
    expect(db.prepare('SELECT * FROM children').all()).toEqual([]);
    expectAtomicRecovery();
  });
});
