import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { Worker } from 'worker_threads';
import { createDatabase } from '../src/db/sqlite-adapter';
import { SessionsIndex, sessionsDbPath } from '../src/sessions';

const dirs: string[] = [];
const indexes: SessionsIndex[] = [];
const fixture = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sessions-storage-'));
  dirs.push(dir);
  const current = sessionsDbPath(dir);
  return { legacy: path.join(path.dirname(current), 'sessions.db'), current };
};
const open = (file: string) => {
  const index = SessionsIndex.open(file);
  indexes.push(index);
  return index;
};
const record = (file: string, mtime: number, text = 'Durable ownership decisions remain searchable across project worktrees.') => ({
  path: file, mtime, size: mtime,
  load: () => ({ session: file, title: null, docs: [{ text, role: 'user', ts: '2026-10-07T00:00:00Z' }] }),
});
const legacyStore = (file: string) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const { db } = createDatabase(file);
  db.exec(`CREATE TABLE roots(path TEXT PRIMARY KEY);
    INSERT INTO roots VALUES ('removed-worktree');
    CREATE VIRTUAL TABLE docs USING fts5(text, file UNINDEXED);
    INSERT INTO docs VALUES ('Older writers retain their own transcript passages.', 'old');
    PRAGMA user_version = 5;`);
  return db;
};

afterEach(() => {
  for (const index of indexes.splice(0)) index.close();
  (globalThis as { Bun?: { gc?: (force: boolean) => void } }).Bun?.gc?.(true);
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5 });
});

describe('session storage generations', () => {
  it('upgrades a version-4 index that predates remembered roots without altering it', () => {
    const { legacy, current } = fixture();
    fs.mkdirSync(path.dirname(legacy), { recursive: true });
    const { db } = createDatabase(legacy);
    db.exec(`CREATE TABLE files (
      path TEXT PRIMARY KEY, session TEXT NOT NULL, title TEXT, mtime REAL NOT NULL, size INTEGER NOT NULL
    );
    CREATE VIRTUAL TABLE docs USING fts5(
      text, file UNINDEXED, role UNINDEXED, ts UNINDEXED, tokenize = 'porter unicode61'
    );
    PRAGMA user_version = 4;`);
    db.close();
    const before = fs.readFileSync(legacy);
    const index = open(current);
    expect(index.rememberRoots(['current-worktree'])).toEqual(['current-worktree']);
    expect(index.refreshRecords([record('current', 1)])).toEqual({ files: 1, refreshed: 1, docs: 1 });
    expect(index.search('durable ownership')).toHaveLength(1);
    expect(fs.readFileSync(legacy)).toEqual(before);
  });

  it('imports remembered roots without copying prose or changing the older writer store', () => {
    const { legacy, current } = fixture();
    legacyStore(legacy).close();
    const before = fs.readFileSync(legacy);
    const index = open(current);
    expect(index.rememberRoots([])).toEqual(['removed-worktree']);
    expect(index.search('older transcript')).toEqual([]);
    expect(fs.readFileSync(legacy)).toEqual(before);
    expect(() => SessionsIndex.open(legacy)).toThrow(/legacy sessions.db.*sessions-v2.db/);
    expect(fs.readFileSync(legacy)).toEqual(before);
  });

  it('keeps legacy writes, replacement, deletion and reopen independent in every three-event ordering', () => {
    const events = ['legacy', 'replace', 'delete', 'reopen'] as const;
    const sequences: Array<Array<typeof events[number]>> = [];
    const enumerate = (prefix: Array<typeof events[number]>) => {
      if (prefix.length === 3) { sequences.push(prefix); return; }
      for (const event of events) enumerate([...prefix, event]);
    };
    enumerate([]);
    for (const sequence of sequences) {
      const { legacy, current } = fixture();
      const old = legacyStore(legacy);
      let index = SessionsIndex.open(current);
      let stamp = 1;
      let present = true;
      index.refreshRecords([record('current', stamp)]);
      try {
        for (const event of sequence) {
          if (event === 'legacy') {
            old.exec("DELETE FROM docs; INSERT INTO docs VALUES ('Legacy row identifiers may be reused independently.', 'other');");
          } else if (event === 'replace') {
            present = true;
            index.refreshRecords([record('current', ++stamp)]);
          } else if (event === 'delete') {
            present = false;
            index.refreshRecords([]);
          } else {
            index.close();
            index = SessionsIndex.open(current);
          }
          expect(index.search('durable ownership'), sequence.join(' -> ')).toHaveLength(present ? 1 : 0);
          expect(index.rememberRoots([]), sequence.join(' -> ')).toEqual(['removed-worktree']);
          const check = createDatabase(current, { readOnly: true }).db;
          try {
            expect(check.prepare('SELECT count(*) AS n FROM docs').get(), sequence.join(' -> '))
              .toEqual({ n: present ? 1 : 0 });
            expect(check.prepare('SELECT doc_rowid AS rowid, file FROM doc_sources ORDER BY doc_rowid').all())
              .toEqual(check.prepare('SELECT rowid, file FROM docs ORDER BY rowid').all());
          } finally { check.close(); }
        }
      } finally { index.close(); old.close(); }
    }
  });

  it('rolls back a failed mapping insert with its transcript replacement', () => {
    const { current } = fixture();
    const index = open(current);
    index.refreshRecords([record('current', 1)]);
    const before = index.search('durable ownership');
    const { db } = createDatabase(current);
    try {
      db.exec("CREATE TRIGGER reject_mapping BEFORE INSERT ON doc_sources BEGIN SELECT RAISE(ABORT, 'mapping rejected'); END;");
      expect(() => index.refreshRecords([record('current', 2, 'Changed content must not survive a failed replacement.')])).toThrow('mapping rejected');
      expect(index.search('durable ownership')).toEqual(before);
      expect(index.search('changed content')).toEqual([]);
      db.exec('DROP TRIGGER reject_mapping');
      expect(index.refreshRecords([record('current', 2)])).toEqual({ files: 1, refreshed: 1, docs: 1 });
    } finally { db.close(); }
  });

  it('rolls back failed root import and succeeds after the legacy store is repaired', () => {
    const { legacy, current } = fixture();
    const old = legacyStore(legacy);
    old.exec('DROP TABLE roots');
    old.close();
    expect(() => SessionsIndex.open(current)).toThrow('no such table: roots');
    const { db } = createDatabase(current);
    try {
      expect(db.prepare("SELECT name FROM sqlite_master WHERE name IN ('files', 'roots', 'docs', 'doc_sources')").all()).toEqual([]);
    } finally { db.close(); }
    const repair = createDatabase(legacy).db;
    repair.exec("CREATE TABLE roots(path TEXT PRIMARY KEY); INSERT INTO roots VALUES ('recovered-worktree');");
    repair.close();
    expect(open(current).rememberRoots([])).toEqual(['recovered-worktree']);
  });

  it('opens and searches an initialized store before the other writer releases its lock', async () => {
    const { current } = fixture();
    const index = SessionsIndex.open(current);
    index.refreshRecords([record('current', 1)]);
    index.close();
    const barrier = new Int32Array(new SharedArrayBuffer(4));
    const holder = new Worker(`
      const { parentPort, workerData } = require('worker_threads');
      const { DatabaseSync } = require('node:sqlite');
      const db = new DatabaseSync(workerData.file);
      db.exec('BEGIN IMMEDIATE');
      parentPort.postMessage('locked');
      Atomics.wait(new Int32Array(workerData.barrier), 0, 0);
      db.exec('ROLLBACK');
      db.close();
      require(workerData.teardown).collectBeforeExit();
    `, { eval: true, workerData: {
      file: current, barrier: barrier.buffer,
      teardown: path.resolve(__dirname, '../dist/worker-teardown.js'),
    } });
    const ended = new Promise<number>((resolve, reject) => { holder.once('exit', resolve); holder.once('error', reject); });
    try {
      await new Promise((resolve, reject) => { holder.once('message', resolve); holder.once('error', reject); });
      expect(open(current).search('durable ownership')).toHaveLength(1);
      expect(Atomics.load(barrier, 0)).toBe(0);
    } finally {
      Atomics.store(barrier, 0, 1);
      Atomics.notify(barrier, 0);
      expect(await ended).toBe(0);
    }
  }, 10_000);
});
