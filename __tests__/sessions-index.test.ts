/**
 * Session index — FTS5 over agent-session transcripts (src/sessions).
 *
 * Covers the reader (which entries become prose docs), the query quoting that
 * keeps flags and paths out of FTS5 syntax, porter stemming, the role / since /
 * session / any filters, incremental refresh (unchanged files are not re-read,
 * a rewritten file is replaced rather than duplicated, a deleted file is
 * forgotten), and the project-level switches: `CODEGRAPH_SESSIONS_DIR`, the
 * Claude Code slug lookup, and `"sessions": false` in codegraph.json.
 */
import { describe, it, expect, afterEach, beforeEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { Worker } from 'worker_threads';
import {
  claudeProjectSlug,
  claudeSessionsDir,
  transcriptDocs,
  transcriptTitle,
} from '../src/sessions/claude-code';
import { parseCodexTranscript, codexFilesForProject } from '../src/sessions/codex';
import { parseAgyTranscript, agyFilesForProject } from '../src/sessions/agy';
import { opencodeSessionsForProject } from '../src/sessions/opencode';
import { devinSessionsForProject } from '../src/sessions/devin';
import { parseCursorTranscript, cursorFilesForProject, cursorProjectSlug, cursorStamp } from '../src/sessions/cursor';
import { cwdBelongsToProject, cwdInRoots } from '../src/sessions/project-roots';
import {
  SessionsIndex,
  enterWalMode,
  ftsQuery,
  querySessions,
  NoSessionsError,
  formatSessionHits,
  FULL_PASSAGE_BUDGET,
} from '../src/sessions';
import { clearProjectConfigCache } from '../src/project-config';
import { isInjectedDoc, slashCommandText, splitPassages, indexableDocs } from '../src/sessions/noise';
import { normalizeRemote } from '../src/sessions/codex';
import { parseGrokTranscript, grokFilesForProject } from '../src/sessions/grok';
import { sessionsMentioning, isDistinctiveName } from '../src/sessions/index';
import { execFileSync } from 'child_process';
import { createDatabase } from '../src/db/sqlite-adapter';

const at = '2026-09-04T20:00:00.000Z';
const user = (text: unknown, extra: Record<string, unknown> = {}) => ({
  type: 'user',
  timestamp: at,
  message: { content: text },
  ...extra,
});
const assistant = (blocks: unknown[]) => ({ type: 'assistant', timestamp: at, message: { content: blocks } });

const dirs: string[] = [];
const fixtureDir = (): string => {
  // Resolved, as project roots are in production: the temp dir can sit behind
  // a symlink (macOS /var, the test run's short socket alias).
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-sessions-')));
  dirs.push(d);
  return d;
};
const writeJsonl = (file: string, entries: unknown[], mtimeSec: number): void => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, entries.map((e) => JSON.stringify(e)).join('\n') + '\n');
  fs.utimesSync(file, mtimeSec, mtimeSec);
};
const savedEnv = { ...process.env };
beforeEach(() => {
  // Point every host at nothing by default, so no test reads the developer's own stores.
  process.env.CODEGRAPH_DEVIN_DIR = path.join(os.tmpdir(), 'codegraph-no-devin');
  process.env.CODEGRAPH_GROK_DIR = path.join(os.tmpdir(), 'codegraph-no-grok');
});
afterEach(() => {
  // Under Bun (a contributor running vitest on it), node:sqlite keeps the file
  // handle of a prepared statement until GC even after `close()`, so the temp
  // dir holding the session database is EBUSY without this. A no-op on Node.
  (globalThis as { Bun?: { gc?: (force: boolean) => void } }).Bun?.gc?.(true);
  for (const d of dirs.splice(0)) fs.rmSync(d, { recursive: true, force: true });
  for (const k of [
    'CODEGRAPH_SESSIONS_DIR',
    'CLAUDE_CONFIG_DIR',
    'CODEX_HOME',
    'CURSOR_CONFIG_DIR',
    'CODEGRAPH_OPENCODE_DB',
    'CODEGRAPH_ANTIGRAVITY_DIR',
    'CODEGRAPH_DEVIN_DIR',
    'CODEGRAPH_GROK_DIR',
  ]) {
    if (savedEnv[k] === undefined) delete process.env[k];
    else process.env[k] = savedEnv[k];
  }
  clearProjectConfigCache();
});

describe('Claude Code reader', () => {
  it('keeps prompts, replies and compaction summaries; drops tool traffic, thinking, meta and short text', () => {
    const docs = transcriptDocs([
      user('please merge the two dedupe paths into one'),
      user('ok'),
      user('<meta prompt that is long enough to index>', { isMeta: true }),
      user('Summary: the ring is deduped at write time only', { isCompactSummary: true }),
      user([{ type: 'tool_result', tool_use_id: 't1', content: 'a long tool result payload here' }]),
      assistant([
        { type: 'thinking', thinking: 'private reasoning that is long enough to index' },
        { type: 'tool_use', id: 't1', name: 'Read', input: {} },
        { type: 'text', text: 'Merged: aggregateTurnReady now reads the ring as booked.' },
      ]),
      { type: 'queue-operation', timestamp: at },
      { type: 'user', message: { content: 'no timestamp so this one is skipped entirely' } },
    ]);
    expect(docs.map((d) => d.role)).toEqual(['user', 'summary', 'assistant']);
    expect(docs[2]!.text).toMatch(/^Merged:/);
  });

  it('indexes a prompt sent mid-turn (a queued_command attachment) as the user', () => {
    const docs = transcriptDocs([
      {
        type: 'attachment',
        timestamp: at,
        attachment: {
          type: 'queued_command',
          prompt: [{ type: 'text', text: 'follow-up: retest Node vs Bun performance metrics' }],
        },
        rendered: [{ content: [{ type: 'text', text: '<system-reminder>The user sent a new message…' }] }],
      },
      { type: 'attachment', timestamp: at, attachment: { type: 'file', content: 'a file attachment is not prose' } },
    ]);
    expect(docs).toEqual([{ ts: at, role: 'user', text: 'follow-up: retest Node vs Bun performance metrics' }]);
  });

  it('transcriptTitle returns the last stored title or null', () => {
    expect(transcriptTitle([{ customTitle: 'a' }, { customTitle: 'b' }])).toBe('b');
    expect(transcriptTitle([user('x')])).toBeNull();
  });

  it('derives the project slug the way Claude Code does and finds either drive-letter case', () => {
    const root = fixtureDir();
    const config = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = config;
    expect(claudeSessionsDir(root)).toBeNull();
    const lower = path.join(config, 'projects', claudeProjectSlug(root).toLowerCase());
    fs.mkdirSync(lower, { recursive: true });
    // A case-insensitive filesystem answers the exact-case probe with the same directory.
    expect(claudeSessionsDir(root)?.toLowerCase()).toBe(lower.toLowerCase());
  });
});

describe('Codex and Cursor reader edges', () => {
  it('reads a Codex session_meta line longer than one 64 KB read', () => {
    const project = fixtureDir();
    const codexHome = fixtureDir();
    process.env.CODEX_HOME = codexHome;
    // Codex puts the whole base instructions in session_meta, so the cwd can
    // follow a payload that spans several reads.
    const file = path.join(codexHome, 'sessions', 'rollout-long-meta.jsonl');
    writeJsonl(
      file,
      [
        {
          timestamp: at,
          type: 'session_meta',
          payload: { base_instructions: { text: 'x'.repeat(200 * 1024) }, session_id: 'codex-long', cwd: project },
        },
        { timestamp: at, type: 'response_item', payload: { type: 'message', role: 'user', content: [] } },
      ],
      1_700_000_000,
    );
    expect(codexFilesForProject([project])).toEqual([file]);
  });

  it('converts Cursor prompt stamps with half-hour offsets, 12 AM/PM and a date change', () => {
    const iso = (stamp: string) => cursorStamp(`<timestamp>${stamp}</timestamp>\nhi`)?.iso;
    expect(iso('Monday, Sep 28, 2026, 9:15 AM (UTC+5:30)')).toBe('2026-09-28T03:45:00.000Z');
    expect(iso('Monday, Sep 28, 2026, 9:15 AM (UTC-3:30)')).toBe('2026-09-28T12:45:00.000Z');
    expect(iso('Monday, Sep 28, 2026, 12:05 AM (UTC+0)')).toBe('2026-09-28T00:05:00.000Z');
    expect(iso('Monday, Sep 28, 2026, 12:05 PM (UTC+0)')).toBe('2026-09-28T12:05:00.000Z');
    expect(iso('Wednesday, Dec 31, 2026, 11:30 PM (UTC-6)')).toBe('2027-01-01T05:30:00.000Z');
    expect(iso('Monday, September 28, 2026, 1:00 PM (UTC+10)')).toBe('2026-09-28T03:00:00.000Z');
  });
});

describe('ftsQuery', () => {
  it('quotes every word so flags, paths and punctuation cannot break the MATCH syntax', () => {
    expect(ftsQuery('turn-readiness dedupe --limit "5" scripts/cg-probe.ts')).toBe(
      '"turn" "readiness" "dedupe" "limit" "5" "scripts" "cg" "probe" "ts"',
    );
    expect(ftsQuery('  ')).toBe('');
    expect(ftsQuery('ring cap', true)).toBe('"ring" OR "cap"');
  });

  it('drops stopwords unless the query is nothing else', () => {
    expect(ftsQuery('why did we keep the ring cap')).toBe('"ring" "cap"');
    expect(ftsQuery('what is it')).toBe('"what" "is" "it"');
  });
});

describe('remembered roots', () => {
  it('keeps matching a worktree root after it leaves the worktree list', () => {
    const index = SessionsIndex.open(':memory:');
    expect(index.rememberRoots(['/repo', '/wt/a']).sort()).toEqual(['/repo', '/wt/a']);
    // /wt/a was removed: it is no longer passed in, but stays remembered.
    expect(index.rememberRoots(['/repo']).sort()).toEqual(['/repo', '/wt/a']);
    expect(cwdInRoots('/wt/a/src', index.rememberRoots(['/repo']))).toBe(true);
    index.close();
  });
});

describe('noise rules', () => {
  it('drops host text by its opening, not by a phrase a person could type', () => {
    expect(isInjectedDoc('user', '  <system-reminder>\nstuff</system-reminder>')).toBe(true);
    expect(isInjectedDoc('user', 'Base directory for this skill: /x')).toBe(true);
    expect(isInjectedDoc('user', 'can you output a summary of the failing tests?')).toBe(false);
    expect(isInjectedDoc('user', 'fix this <system-reminder> quoted in the middle')).toBe(false);
    expect(isInjectedDoc('user', 'Your task: output a summary of this conversation.\n' + 'x '.repeat(3000))).toBe(true);
    expect(isInjectedDoc('assistant', '<system-reminder>')).toBe(false);
  });

  it('keeps what a slash command asked for and drops a bare one', () => {
    const cmd = '<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>add retry to the uploader</command-args>';
    expect(slashCommandText(cmd)).toBe('/review add retry to the uploader');
    expect(slashCommandText('<command-name>/clear</command-name>\n<command-args></command-args>')).toBeNull();
    const docs = indexableDocs([
      { ts: at, role: 'user', text: cmd },
      { ts: at, role: 'user', text: '<command-name>/clear</command-name>' },
    ]);
    expect(docs.map((d) => d.text)).toEqual(['/review add retry to the uploader']);
  });

  it('cuts long prose at spaces and folds a short tail into the passage before it', () => {
    const words = 'lorem ipsum '.repeat(700).trim(); // one 8 KB paragraph, no line breaks
    const parts = splitPassages(words);
    expect(parts.length).toBeGreaterThan(1);
    // Every cut falls between words.
    expect(parts.every((p) => /^(lorem|ipsum)\b/.test(p) && /\b(lorem|ipsum)$/.test(p))).toBe(true);
    expect(parts.every((p) => p.length >= 200)).toBe(true);
    expect(splitPassages('a'.repeat(3000) + '\n\n' + 'b'.repeat(1200) + '\n\nshort tail').at(-1)).toMatch(/short tail$/);
    expect(splitPassages('a'.repeat(3000) + '\n\n' + 'b'.repeat(1200) + '\n\nshort tail').every((p) => p.length >= 200)).toBe(true);
  });

  it('normalizes remotes across schemes, users, ports and suffixes', () => {
    const want = 'example.com/acme/widget';
    for (const url of [
      'https://example.com/acme/widget.git',
      'git@example.com:acme/widget',
      'ssh://git@example.com:2222/acme/widget.git',
      'https://example.com/acme/widget.git/',
      'HTTPS://Example.com/Acme/Widget/',
    ]) {
      expect(normalizeRemote(url)).toBe(want);
    }
  });
});

describe('unreadable sources', () => {
  it('retries a record whose source could not be read instead of storing it empty', () => {
    const index = SessionsIndex.open(':memory:');
    let readable = false;
    const record = {
      path: 'store:1',
      mtime: 1,
      size: 1,
      load: () => (readable ? { session: 'store:1', title: null, docs: [{ ts: at, role: 'user' as const, text: 'the flaky store came back online' }] } : null),
    };
    expect(index.refreshRecords([record]).refreshed).toBe(0);
    readable = true;
    expect(index.refreshRecords([record]).refreshed).toBe(1);
    expect(index.search('flaky store')).toHaveLength(1);
    index.close();
  });
});

describe('what gets indexed and how hits rank', () => {
  it('skips injected text, cuts long prose into passages, ranks coverage first and collapses repeats', () => {
    const dir = fixtureDir();
    const para = (word: string) => `${word} `.repeat(300).trim();
    const long = [para('alpha'), para('bravo'), 'the timer fast-forwards the runtime checkout', para('charlie')].join('\n\n');
    writeJsonl(
      path.join(dir, 'aaaa-1111.jsonl'),
      [
        user('Base directory for this skill: /skills/x\n\nthe timer fast-forwards the runtime checkout every ten minutes'),
        user(long),
        assistant([{ type: 'text', text: 'timer mentioned once here, nothing else relevant to anything' }]),
        user('continue with the timer checkout please'),
      ],
      1_700_000_000,
    );
    writeJsonl(path.join(dir, 'bbbb-2222.jsonl'), [user('continue with the timer checkout please')], 1_700_000_000);
    const index = SessionsIndex.open(':memory:');
    index.refresh(dir);

    const hits = index.search('timer runtime checkout');
    // The skill body is not indexed, and the pasted log's matching paragraph
    // is its own passage rather than one 5 KB row.
    expect(hits.filter((h) => !h.partial)).toHaveLength(1);
    expect(hits[0]!.partial).toBeUndefined();
    expect(hits[0]!.snippet).not.toMatch(/alpha|charlie/);
    // The every-word query ran short, so some-word hits follow, marked.
    expect(hits.slice(1).every((h) => h.partial)).toBe(true);
    expect(hits.fallback).toBeUndefined();

    const wide = index.search('timer runtime checkout', { any: true });
    expect(wide[0]!.snippet).toMatch(/runtime/);
    // The same "continue" prompt in two sessions appears once.
    expect(wide.filter((h) => h.snippet.includes('continue'))).toHaveLength(1);
    index.close();
  });
});

describe('SessionsIndex', () => {
  it('stems, ranks, filters, and re-reads only files that moved', () => {
    const dir = fixtureDir();
    const a = path.join(dir, 'aaaa-1111.jsonl');
    writeJsonl(
      a,
      [
        { type: 'custom-title', customTitle: 'ponytail sweep' },
        user('we merged the two dedupe paths in turnReadiness'),
        assistant([{ type: 'text', text: 'The merge kept the write-time dedupe and dropped the read-time one.' }]),
      ],
      1_700_000_000,
    );
    // A subagent transcript nests under its parent's directory and is indexed too.
    const b = path.join(dir, 'aaaa-1111', 'subagents', 'agent-1.jsonl');
    writeJsonl(b, [user('the subagent found the ring cap at forty rows')], 1_700_000_000);
    const index = SessionsIndex.open(':memory:');
    expect(index.refresh(dir)).toEqual({ files: 2, refreshed: 2, docs: 3 });

    // Porter: "merging" reaches "merged" and "merge".
    const hits = index.search('merging dedupe');
    expect(hits).toHaveLength(2);
    expect(hits[0]).toMatchObject({ session: 'aaaa-1111', title: 'ponytail sweep' });
    expect(hits.every((h) => h.snippet.includes('['))).toBe(true);
    expect(index.search('merging', { role: 'assistant' }).map((h) => h.role)).toEqual(['assistant']);
    expect(index.search('merging', { sinceIso: '2027-01-01T00:00:00.000Z' })).toEqual([]);
    // No passage holds both words: the any-word query answers, flagged as a fallback.
    const partial = index.search('unrelatedword kept');
    expect(partial).toHaveLength(1);
    expect(partial.fallback).toBe(true);
    expect(index.search('unrelatedword kept', { any: true }).fallback).toBeUndefined();
    expect(index.search('merging dedupe').fallback).toBeUndefined();
    expect(hits[0]!.file).toBe(a);
    expect(index.search('ring cap', { session: 'agent' })).toHaveLength(1);
    expect(index.search('merging', { session: 'bbbb' })).toEqual([]);

    // Unchanged: nothing re-read. Rewritten: replaced, not duplicated. Deleted: forgotten.
    expect(index.refresh(dir).refreshed).toBe(0);
    writeJsonl(a, [user('only this prompt remains after the rewrite')], 1_700_000_100);
    expect(index.refresh(dir)).toEqual({ files: 2, refreshed: 1, docs: 1 });
    expect(index.search('merging')).toEqual([]);
    expect(index.search('rewrite')).toHaveLength(1);
    fs.rmSync(b);
    expect(index.refresh(dir)).toEqual({ files: 1, refreshed: 0, docs: 0 });
    expect(index.search('ring cap')).toEqual([]);
    index.close();
  });

  it('waits for another connection mid-write and skips a file it already indexed', async () => {
    // Parallel MCP calls run on worker threads, one connection each, and all
    // see the same changed transcript. Another thread holds the write lock and
    // indexes the file while this thread's refresh is under way: the refresh
    // must wait rather than throw "database is locked", then find the row the
    // other thread wrote and leave the file alone instead of indexing it twice.
    const dir = fixtureDir();
    const file = path.join(dir, 'aaaa-1111.jsonl');
    writeJsonl(file, [user('the pool sees one transcript from two threads')], 1_700_000_000);
    const dbPath = path.join(fixtureDir(), 'sessions-v2.db');
    const index = SessionsIndex.open(dbPath);
    const st = fs.statSync(file);
    const other = new Worker(
      `const { workerData, parentPort } = require('worker_threads');
       const { DatabaseSync } = require('node:sqlite');
       const db = new DatabaseSync(workerData.dbPath);
       db.exec('BEGIN IMMEDIATE');
       parentPort.postMessage('locked');
       Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 300);
       const inserted = db.prepare('INSERT INTO docs (text, file, role, ts) VALUES (?, ?, ?, ?)')
         .run('the pool sees one transcript from two threads', workerData.file, 'user', workerData.ts);
       db.prepare('INSERT INTO doc_sources (doc_rowid, file) VALUES (?, ?)')
         .run(inserted.lastInsertRowid, workerData.file);
       db.prepare('INSERT OR REPLACE INTO files (path, session, title, mtime, size) VALUES (?, ?, ?, ?, ?)')
         .run(workerData.file, 'aaaa-1111', null, workerData.mtime, workerData.size);
       db.exec('COMMIT');
       db.close();`,
      { eval: true, workerData: { dbPath, file, ts: at, mtime: st.mtimeMs, size: st.size } },
    );
    await new Promise((resolve) => other.once('message', resolve));
    expect(index.refresh(dir)).toEqual({ files: 1, refreshed: 0, docs: 0 });
    await new Promise((resolve) => other.once('exit', resolve));
    expect(index.search('pool transcript threads')).toHaveLength(1);
    index.close();
  });

  it('opens a fresh database while another connection holds it, converting to WAL once free', async () => {
    // An ordinary lock wait on the conversion is covered by busy_timeout: the
    // holder commits and this open then converts, rather than throwing.
    const dbPath = path.join(fixtureDir(), 'sessions-v2.db');
    const other = new Worker(
      `const { workerData, parentPort } = require('worker_threads');
       const { DatabaseSync } = require('node:sqlite');
       const db = new DatabaseSync(workerData.dbPath);
       db.exec('BEGIN EXCLUSIVE');
       parentPort.postMessage('locked');
       Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 300);
       db.exec('COMMIT');
       db.close();`,
      { eval: true, workerData: { dbPath } },
    );
    await new Promise((resolve) => other.once('message', resolve));
    const index = SessionsIndex.open(dbPath);
    await new Promise((resolve) => other.once('exit', resolve));
    const db = createDatabase(dbPath).db;
    expect(String(db.pragma('journal_mode', { simple: true })).toLowerCase()).toBe('wal');
    db.close();
    index.close();
  });

  it('retries a WAL conversion that collides with another process converting the same fresh file', () => {
    // What busy_timeout does NOT cover: several processes converting one
    // brand-new database at the same moment collide inside the conversion
    // rather than queueing on a lock, and it surfaces as either of these two
    // transient errors. That collision only reproduces probabilistically, so
    // the retry is driven directly here.
    for (const message of ['database is locked', 'disk I/O error']) {
      let calls = 0;
      const db = {
        pragma(sql: string) {
          if (!/journal_mode\s*=/i.test(sql)) return 'delete';
          if (++calls < 3) throw new Error(message);
          return 'wal';
        },
      };
      expect(() => enterWalMode(db as never)).not.toThrow();
      expect(calls).toBe(3);
    }
  });

  it('gives up on a WAL conversion error that is not the transient collision', () => {
    const db = {
      pragma() {
        throw new Error('unable to open database file');
      },
    };
    expect(() => enterWalMode(db as never)).toThrow(/unable to open database file/);
  });
});

describe('querySessions (project entry point)', () => {
  it('indexes into .codegraph/sessions-v2.db from CODEGRAPH_SESSIONS_DIR and honors "sessions": false', () => {
    const project = fixtureDir();
    fs.mkdirSync(path.join(project, '.codegraph'));
    const transcripts = fixtureDir();
    writeJsonl(path.join(transcripts, 's1.jsonl'), [user('decided to keep the write-time dedupe')], 1_700_000_000);
    process.env.CODEGRAPH_SESSIONS_DIR = transcripts;

    const result = querySessions(project, 'deciding dedupe');
    expect(result.index).toEqual({ files: 1, refreshed: 1, docs: 1 });
    expect(result.hits.map((h) => h.session)).toEqual(['claude:s1']);
    // Same reader version: the file is not re-read. An index written by an
    // older reader (user_version behind) is re-read once in full.
    expect(querySessions(project, 'deciding dedupe').index.refreshed).toBe(0);
    const { db } = createDatabase(path.join(project, '.codegraph', 'sessions-v2.db'));
    db.exec('PRAGMA user_version = 1');
    db.close();
    expect(querySessions(project, 'deciding dedupe').index).toEqual({ files: 1, refreshed: 1, docs: 1 });
    expect(fs.existsSync(path.join(project, '.codegraph', 'sessions-v2.db'))).toBe(true);
    expect(formatSessionHits('deciding dedupe', result)).toMatch(/^Sessions matching "deciding dedupe" — 1 hit across 1 transcript:/);
    expect(formatSessionHits('nothing', { index: result.index, hits: [] })).toMatch(/holds any of these words/);

    fs.writeFileSync(path.join(project, 'codegraph.json'), JSON.stringify({ sessions: false }));
    clearProjectConfigCache();
    expect(() => querySessions(project, 'dedupe')).toThrow(NoSessionsError);
  });

  it.skipIf(process.platform === 'win32')('names the Cursor project directory the way Cursor does', () => {
    // The shape of the folders Cursor writes under ~/.cursor/projects: a
    // separator run such as "/." collapses to a single dash.
    expect(cursorProjectSlug('/home/user/my-repo')).toBe('home-user-my-repo');
    expect(cursorProjectSlug('/home/user/.t3/worktrees/myRepo/t3code-51125c9e')).toBe(
      'home-user-t3-worktrees-myRepo-t3code-51125c9e',
    );
    expect(cursorProjectSlug('/home/user/.local/share/scratch/check')).toBe(
      'home-user-local-share-scratch-check',
    );
  });

  it('indexes Codex and Cursor prose for this project and skips other-repo or tool traffic', () => {
    const project = fixtureDir();
    const other = fixtureDir();
    fs.mkdirSync(path.join(project, '.codegraph'));
    const codexHome = fixtureDir();
    const cursorHome = fixtureDir();
    process.env.CODEX_HOME = codexHome;
    process.env.CURSOR_CONFIG_DIR = cursorHome;
    process.env.CLAUDE_CONFIG_DIR = fixtureDir();
    process.env.CODEGRAPH_OPENCODE_DB = path.join(fixtureDir(), 'no-opencode.db');
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = fixtureDir();

    const match = path.join(codexHome, 'sessions', '2026', '09', '11', 'rollout-match.jsonl');
    writeJsonl(
      match,
      [
        {
          timestamp: at,
          type: 'session_meta',
          payload: { session_id: 'codex-match', cwd: project },
        },
        {
          timestamp: at,
          type: 'response_item',
          payload: {
            type: 'message',
            role: 'user',
            content: [
              { type: 'input_text', text: 'please keep the write-time dedupe path' },
              { type: 'input_text', text: '<recommended_plugins>\nHere is a list of plugins that are available' },
            ],
          },
        },
        {
          timestamp: at,
          type: 'response_item',
          payload: { type: 'custom_tool_call', name: 'exec', input: 'a long tool payload that must not index' },
        },
        {
          timestamp: at,
          type: 'response_item',
          payload: {
            type: 'message',
            role: 'assistant',
            content: [{ type: 'output_text', text: 'Kept the write-time dedupe and dropped the read-time one.' }],
          },
        },
      ],
      1_700_000_000,
    );
    const elsewhere = path.join(codexHome, 'sessions', 'rollout-other.jsonl');
    writeJsonl(
      elsewhere,
      [
        { timestamp: at, type: 'session_meta', payload: { session_id: 'codex-other', cwd: other } },
        {
          timestamp: at,
          type: 'response_item',
          payload: {
            type: 'message',
            role: 'user',
            content: [{ type: 'input_text', text: 'this other repo should not appear in the search' }],
          },
        },
      ],
      1_700_000_000,
    );

    const sid = '11111111-1111-4111-8111-111111111111';
    const cursorFile = path.join(
      cursorHome,
      'projects',
      cursorProjectSlug(project),
      'agent-transcripts',
      sid,
      `${sid}.jsonl`,
    );
    writeJsonl(
      cursorFile,
      [
        {
          role: 'user',
          message: {
            content: [
              { type: 'text', text: 'what did we decide about the write-time dedupe?' },
              { type: 'tool_use', name: 'Read', input: { path: 'secret' } },
            ],
          },
        },
        {
          role: 'assistant',
          message: { content: [{ type: 'text', text: 'We kept write-time dedupe in the last Cursor session.' }] },
        },
      ],
      1_700_000_000,
    );

    expect(cwdBelongsToProject(project, project)).toBe(true);
    expect(cwdBelongsToProject(other, project)).toBe(false);
    expect(codexFilesForProject([project])).toEqual([match]);
    // A session from a worktree that is gone still matches by repository URL.
    const retired = path.join(codexHome, 'sessions', 'rollout-retired.jsonl');
    writeJsonl(
      retired,
      [
        {
          timestamp: at,
          type: 'session_meta',
          payload: {
            session_id: 'codex-retired',
            cwd: path.join(other, 'removed-worktree'),
            git: { repository_url: 'git@example.com:acme/widget.git' },
          },
        },
      ],
      1_700_000_000,
    );
    expect(codexFilesForProject([project], ['https://example.com/acme/widget']).sort()).toEqual([match, retired].sort());
    // A live clone that shares the remote keeps its own sessions.
    const liveClone = path.join(codexHome, 'sessions', 'rollout-clone.jsonl');
    writeJsonl(
      liveClone,
      [
        {
          timestamp: at,
          type: 'session_meta',
          payload: { session_id: 'codex-clone', cwd: other, git: { repository_url: 'https://example.com/acme/widget' } },
        },
      ],
      1_700_000_000,
    );
    expect(codexFilesForProject([project], ['https://example.com/acme/widget'])).not.toContain(liveClone);
    fs.rmSync(retired);
    fs.rmSync(liveClone);
    expect(parseCodexTranscript(match).docs.map((d) => d.role)).toEqual(['user', 'assistant']);
    expect(parseCodexTranscript(match).docs[0]!.text).not.toMatch(/recommended_plugins/);
    expect(cursorFilesForProject([project])).toEqual([cursorFile]);
    expect(parseCursorTranscript(cursorFile).docs).toHaveLength(2);
    expect(cursorStamp('<timestamp>Sunday, Sep 27, 2026, 3:02 PM (UTC-6)</timestamp>\nhello there')).toEqual({
      iso: '2026-09-27T21:02:00.000Z',
      rest: 'hello there',
    });
    expect(cursorStamp('no stamp here')).toBeNull();
    expect(parseCursorTranscript(cursorFile).docs[0]!.text).not.toMatch(/secret/);

    const result = querySessions(project, 'write-time dedupe');
    expect([...new Set(result.hits.map((h) => h.session))].sort()).toEqual(
      [`cursor:${sid}`, 'codex:codex-match'].sort(),
    );
    expect(result.hits.some((h) => h.session === 'codex:codex-other')).toBe(false);
    expect(formatSessionHits('write-time dedupe', result)).toMatch(/claude:|codex:|cursor:/);
  });

  it('indexes OpenCode sqlite sessions and AGY transcripts with a workspace URI, skipping the rest', () => {
    const project = fixtureDir();
    const other = fixtureDir();
    fs.mkdirSync(path.join(project, '.codegraph'));
    process.env.CLAUDE_CONFIG_DIR = fixtureDir();
    process.env.CODEX_HOME = fixtureDir();
    process.env.CURSOR_CONFIG_DIR = fixtureDir();

    const ocDb = path.join(fixtureDir(), 'opencode.db');
    process.env.CODEGRAPH_OPENCODE_DB = ocDb;
    const { db } = createDatabase(ocDb);
    db.exec(`
      CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT, time_updated INTEGER);
      CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
      CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);
    `);
    db.prepare('INSERT INTO session (id, directory, title, time_updated) VALUES (?, ?, ?, ?)').run(
      'ses_match',
      project,
      'opencode match',
      1_700_000_000_000,
    );
    db.prepare('INSERT INTO session (id, directory, title, time_updated) VALUES (?, ?, ?, ?)').run(
      'ses_other',
      other,
      'other repo',
      1_700_000_000_000,
    );
    db.prepare('INSERT INTO message (id, session_id, time_created, data) VALUES (?, ?, ?, ?)').run(
      'msg1',
      'ses_match',
      1_700_000_000_000,
      JSON.stringify({ role: 'user' }),
    );
    db.prepare('INSERT INTO part (id, message_id, session_id, data) VALUES (?, ?, ?, ?)').run(
      'prt1',
      'msg1',
      'ses_match',
      JSON.stringify({ type: 'text', text: 'please keep the write-time dedupe in OpenCode' }),
    );
    db.prepare('INSERT INTO part (id, message_id, session_id, data) VALUES (?, ?, ?, ?)').run(
      'prt2',
      'msg1',
      'ses_match',
      JSON.stringify({ type: 'tool', tool: 'bash', callID: 'c1' }),
    );
    db.prepare('INSERT INTO message (id, session_id, time_created, data) VALUES (?, ?, ?, ?)').run(
      'msg2',
      'ses_other',
      1_700_000_000_000,
      JSON.stringify({ role: 'user' }),
    );
    db.prepare('INSERT INTO part (id, message_id, session_id, data) VALUES (?, ?, ?, ?)').run(
      'prt3',
      'msg2',
      'ses_other',
      JSON.stringify({ type: 'text', text: 'this other OpenCode repo should not appear' }),
    );
    // OpenCode 2 tables: a copy of the v1 session (read once, from v1) and a v2-only session.
    db.exec(`
      CREATE TABLE session_v2 (id TEXT PRIMARY KEY, directory TEXT, title TEXT, time_updated INTEGER);
      CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER, data TEXT);
    `);
    const addV2 = db.prepare('INSERT INTO session_v2 (id, directory, title, time_updated) VALUES (?, ?, ?, ?)');
    addV2.run('ses_match', project, 'opencode match', 1_700_000_000_000);
    // Migrated, then continued in OpenCode 2 only: the newer v2 copy wins.
    db.prepare('INSERT INTO session (id, directory, title, time_updated) VALUES (?, ?, ?, ?)').run('ses_moved', project, 'moved', 1_600_000_000_000);
    addV2.run('ses_moved', project, 'moved', 1_800_000_000_000);
    addV2.run('ses_v2', project, 'opencode two', 1_700_000_000_000);
    const addV2Msg = db.prepare(
      'INSERT INTO session_message (id, session_id, type, seq, time_created, data) VALUES (?, ?, ?, ?, ?, ?)',
    );
    addV2Msg.run('m1', 'ses_match', 'user', 1, 1_700_000_000_000, JSON.stringify({ text: 'duplicate write-time dedupe copy' }));
    addV2Msg.run('m2', 'ses_v2', 'user', 1, 1_700_000_000_000, JSON.stringify({ text: 'does OpenCode two keep write-time dedupe?' }));
    addV2Msg.run(
      'm3',
      'ses_v2',
      'assistant',
      2,
      1_700_000_000_000,
      JSON.stringify({ content: [{ type: 'reasoning', text: 'hidden reasoning text' }, { type: 'text', text: 'Yes, OpenCode two kept it.' }] }),
    );
    addV2Msg.run('m4', 'ses_v2', 'system', 3, 1_700_000_000_000, JSON.stringify({ text: 'system prompt must not index' }));
    addV2Msg.run('m5', 'ses_moved', 'user', 1, 1_800_000_000_000, JSON.stringify({ text: 'continued write-time dedupe in version two' }));
    db.close();

    const agy = fixtureDir();
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = agy;
    const cid = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';
    fs.mkdirSync(path.join(agy, 'cache'), { recursive: true });
    fs.writeFileSync(
      path.join(agy, 'cache', 'conversation_metadata.json'),
      JSON.stringify({
        conversations: {
          [cid]: { summary: { WorkspaceURIs: [`file://${project}`] } },
          skipped: { summary: { WorkspaceURIs: [`file://${other}`], ProjectID: 'default-cli-project' } },
        },
      }),
    );
    const agyFile = path.join(agy, 'brain', cid, '.system_generated', 'logs', 'transcript_full.jsonl');
    writeJsonl(
      agyFile,
      [
        {
          type: 'USER_INPUT',
          created_at: at,
          content: '<USER_REQUEST>\nwhat did we decide about write-time dedupe in AGY?\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nskip\n</ADDITIONAL_METADATA>',
        },
        {
          type: 'PLANNER_RESPONSE',
          created_at: at,
          thinking: 'private reasoning that must not be indexed at all here',
          tool_calls: [{ name: 'Read' }],
          content: 'We kept write-time dedupe in the AGY planner reply.',
        },
      ],
      1_700_000_000,
    );

    const oc = opencodeSessionsForProject([project]);
    expect(oc.map((s) => s.session)).toEqual(['opencode:ses_match', 'opencode:ses_moved', 'opencode:ses_v2']);
    expect(oc[1]!.docs()!.map((d) => d.text)).toEqual(['continued write-time dedupe in version two']);
    expect(oc[2]!.docs()!.map((d) => [d.role, d.text])).toEqual([
      ['user', 'does OpenCode two keep write-time dedupe?'],
      ['assistant', 'Yes, OpenCode two kept it.'],
    ]);
    expect(agyFilesForProject([project])).toEqual([agyFile]);
    expect(parseAgyTranscript(agyFile).docs.map((d) => d.role)).toEqual(['user', 'assistant']);
    expect(parseAgyTranscript(agyFile).docs[0]!.text).not.toMatch(/ADDITIONAL_METADATA/);
    expect(parseAgyTranscript(agyFile).docs[1]!.text).not.toMatch(/private reasoning/);

    const result = querySessions(project, 'write-time dedupe');
    expect([...new Set(result.hits.map((h) => h.session))].sort()).toEqual(
      [`agy:${cid}`, 'opencode:ses_match', 'opencode:ses_moved', 'opencode:ses_v2'].sort(),
    );
    expect(result.hits.some((h) => h.snippet.includes('duplicate'))).toBe(false);
  });

  it('indexes Devin sqlite sessions by working directory, skipping hidden, other-project and tool rows', () => {
    const project = fixtureDir();
    const other = fixtureDir();
    fs.mkdirSync(path.join(project, '.codegraph'));
    process.env.CLAUDE_CONFIG_DIR = fixtureDir();
    process.env.CODEX_HOME = fixtureDir();
    process.env.CURSOR_CONFIG_DIR = fixtureDir();
    process.env.CODEGRAPH_OPENCODE_DB = path.join(fixtureDir(), 'no-opencode.db');
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = fixtureDir();

    const devinRoot = fixtureDir();
    process.env.CODEGRAPH_DEVIN_DIR = devinRoot;
    const devinDb = path.join(devinRoot, 'cli-next', 'sessions.db');
    fs.mkdirSync(path.dirname(devinDb), { recursive: true });
    const { db } = createDatabase(devinDb);
    db.exec(`
      CREATE TABLE sessions (
        id TEXT PRIMARY KEY, working_directory TEXT NOT NULL, backend_type TEXT NOT NULL,
        model TEXT NOT NULL, agent_mode TEXT NOT NULL, created_at INTEGER NOT NULL,
        last_activity_at INTEGER NOT NULL, title TEXT, hidden INTEGER NOT NULL DEFAULT 0
      );
      CREATE TABLE message_nodes (
        row_id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
        node_id INTEGER NOT NULL, parent_node_id INTEGER, chat_message TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
    `);
    const addSession = db.prepare(
      'INSERT INTO sessions (id, working_directory, backend_type, model, agent_mode, created_at, last_activity_at, title, hidden) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)',
    );
    const addNode = db.prepare(
      'INSERT INTO message_nodes (session_id, node_id, chat_message, created_at) VALUES (?, ?, ?, ?)',
    );
    addSession.run('brisk-otter', project, 'cli', 'swe-2-high', 'default', 1_700_000_000, 1_700_000_100, 'devin match', 0);
    addSession.run('calm-finch', other, 'cli', 'swe-2-high', 'default', 1_700_000_000, 1_700_000_100, 'other repo', 0);
    addSession.run('quiet-mole', project, 'cli', 'swe-2-high', 'default', 1_700_000_000, 1_700_000_100, 'hidden', 1);
    const msg = (role: string, content: string) => JSON.stringify({ role, content });
    addNode.run('brisk-otter', 1, msg('user', 'keep the write-time dedupe in Devin too'), 1_700_000_010);
    addNode.run('brisk-otter', 2, msg('assistant', 'Devin kept the write-time dedupe path.'), 1_700_000_020);
    addNode.run('brisk-otter', 3, msg('system', 'injected harness text that must never index'), 1_700_000_030);
    addNode.run('brisk-otter', 4, msg('tool', 'tool output that must never index at all'), 1_700_000_040);
    addNode.run('brisk-otter', 5, msg('user', 'ok'), 1_700_000_050);
    db.close();

    expect(devinSessionsForProject([project]).map((s) => s.session)).toEqual(['devin:brisk-otter']);
    expect(devinSessionsForProject([project])[0]!.docs()!.map((d) => d.role)).toEqual(['user', 'assistant']);

    const result = querySessions(project, 'write-time dedupe');
    expect([...new Set(result.hits.map((h) => h.session))]).toEqual(['devin:brisk-otter']);
    expect(formatSessionHits('write-time dedupe', result)).toContain('devin:');
  });

  it('indexes Grok chunks, commit messages, and finds sessions that name a symbol', () => {
    const project = fs.realpathSync(fixtureDir());
    fs.mkdirSync(path.join(project, '.codegraph'));
    process.env.CLAUDE_CONFIG_DIR = fixtureDir();
    process.env.CODEX_HOME = fixtureDir();
    process.env.CURSOR_CONFIG_DIR = fixtureDir();
    process.env.CODEGRAPH_OPENCODE_DB = path.join(fixtureDir(), 'no-opencode.db');
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = fixtureDir();
    process.env.CODEGRAPH_DEVIN_DIR = fixtureDir();

    const git = (...args: string[]) =>
      execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@example.com', ...args], { cwd: project, stdio: 'pipe' });
    git('init', '-q');
    fs.writeFileSync(path.join(project, 'a.txt'), 'x');
    git('add', 'a.txt');
    git('commit', '-q', '-m', 'keep the ring buffer bounded', '-m', 'Unbounded growth ate the heap on long sessions.');

    const grok = fixtureDir();
    process.env.CODEGRAPH_GROK_DIR = grok;
    const sid = '01a0-grok-session';
    const file = path.join(grok, encodeURIComponent(project), sid, 'updates.jsonl');
    const chunk = (sessionUpdate: string, text: string) => ({
      timestamp: 1_700_000_000,
      params: { update: { sessionUpdate, content: { type: 'text', text } } },
    });
    writeJsonl(
      file,
      [
        chunk('user_message_chunk', 'why does flushRingBuffer drop '),
        chunk('user_message_chunk', 'the oldest entries first?'),
        chunk('user_message_chunk', '<runtime_info>host boilerplate</runtime_info>\n\n<pull_request_linking>more</pull_request_linking>'),
        chunk('agent_message_chunk', ''),
        chunk('agent_thought_chunk', 'private thinking that must not index'),
        chunk('agent_message_chunk', 'flushRingBuffer drops the oldest so '),
        { timestamp: 1_700_000_000, params: { update: { sessionUpdate: 'agent_message_chunk', content: { type: 'image' } } } },
        chunk('agent_message_chunk', 'the newest context survives.'),
        { timestamp: 1_700_000_001, params: { update: { sessionUpdate: 'tool_call', title: 'read' } } },
        // A person's own tagged prompt stays; a junk timestamp does not throw.
        { timestamp: 'soon', params: { update: { sessionUpdate: 'user_message_chunk', content: { type: 'text', text: '<task>Refactor the ring buffer flush</task>' } } } },
        { timestamp: 1_700_000_001, params: { update: { sessionUpdate: 'tool_call', title: 'read' } } },
      ],
      1_700_000_000,
    );
    fs.writeFileSync(path.join(path.dirname(file), 'summary.json'), JSON.stringify({ session_summary: 'ring buffer' }));
    writeJsonl(path.join(grok, encodeURIComponent('/elsewhere'), 'x', 'updates.jsonl'), [chunk('user_message_chunk', 'another project entirely here')], 1);

    expect(grokFilesForProject([project])).toEqual([file]);
    const parsed = parseGrokTranscript(file);
    expect(parsed.title).toBe('ring buffer');
    expect(parsed.docs.map((d) => [d.role, d.text])).toEqual([
      ['user', 'why does flushRingBuffer drop the oldest entries first?'],
      ['assistant', 'flushRingBuffer drops the oldest so the newest context survives.'],
      ['user', '<task>Refactor the ring buffer flush</task>'],
    ]);
    expect(parsed.docs[2]!.ts).toBe('');

    const commits = querySessions(project, 'unbounded heap', { role: 'commit' });
    expect(commits.hits).toHaveLength(1);
    expect(commits.hits[0]!.session).toBe(`git:${path.basename(project)}`);
    expect(commits.hits[0]!.snippet).toMatch(/Unbounded/);
    expect(querySessions(project, 'boilerplate').hits).toEqual([]);

    expect(isDistinctiveName('flushRingBuffer')).toBe(true);
    expect(isDistinctiveName('ring_buffer')).toBe(true);
    expect(isDistinctiveName('search')).toBe(false);
    const mentions = sessionsMentioning(project, ['flushRingBuffer', 'search', 'neverMentionedName', 'ring_buffer']);
    // "ring buffer" in prose is not the identifier `ring_buffer`.
    expect([...mentions.keys()]).toEqual(['flushRingBuffer']);
    expect(mentions.get('flushRingBuffer')).toMatchObject({ total: 1, recent: [{ session: `grok:${sid}`, title: 'ring buffer' }] });

    // Renaming a session rewrites only summary.json; the new title still shows.
    const summary = path.join(path.dirname(file), 'summary.json');
    fs.writeFileSync(summary, JSON.stringify({ session_summary: 'bounded ring buffer' }));
    fs.utimesSync(summary, 1_700_000_100, 1_700_000_100);
    expect(querySessions(project, 'flushRingBuffer').hits[0]).toMatchObject({ session: `grok:${sid}`, title: 'bounded ring buffer' });
    // A passage with no time of its own keeps the transcript's, not the rename's.
    expect(querySessions(project, 'Refactor').hits[0]!.ts).toBe(new Date(1_700_000_000_000).toISOString());
  });

  it('keeps commits to a subdirectory project and needs a transcript to search at all', () => {
    const repo = fs.realpathSync(fixtureDir());
    const project = path.join(repo, 'packages', 'app');
    fs.mkdirSync(path.join(project, '.codegraph'), { recursive: true });
    process.env.CLAUDE_CONFIG_DIR = fixtureDir();
    process.env.CODEX_HOME = fixtureDir();
    process.env.CURSOR_CONFIG_DIR = fixtureDir();
    process.env.CODEGRAPH_OPENCODE_DB = path.join(fixtureDir(), 'no-opencode.db');
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = fixtureDir();
    const git = (...args: string[]) =>
      execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@example.com', ...args], { cwd: repo, stdio: 'pipe' });
    git('init', '-q');
    fs.writeFileSync(path.join(project, 'a.txt'), 'x');
    fs.writeFileSync(path.join(repo, 'b.txt'), 'y');
    git('add', '.');
    git('commit', '-q', '-m', 'app: keep the retry budget small');
    fs.writeFileSync(path.join(repo, 'b.txt'), 'z');
    git('commit', '-q', '-am', 'root: unrelated retry change elsewhere');

    // Commits alone are not session history.
    expect(() => querySessions(project, 'retry')).toThrow(NoSessionsError);

    const grok = fixtureDir();
    process.env.CODEGRAPH_GROK_DIR = grok;
    writeJsonl(
      path.join(grok, encodeURIComponent(project), 's1', 'updates.jsonl'),
      [{ timestamp: 1_700_000_000, params: { update: { sessionUpdate: 'user_message_chunk', content: { type: 'text', text: 'a session in the app package' } } } }],
      1_700_000_000,
    );
    const hits = querySessions(project, 'retry', { role: 'commit' }).hits;
    expect(hits.map((h) => h.snippet)).toEqual([expect.stringMatching(/budget small/)]);
  });
});

describe('stores that exist but cannot be read', () => {
  const entry = (text: string) => ({ ...user(text), timestamp: at });
  /** A project with one Claude transcript, so a refresh runs while another store is unreadable. */
  const project = (): string => {
    const root = fixtureDir();
    fs.mkdirSync(path.join(root, '.codegraph'));
    const claude = fixtureDir();
    process.env.CLAUDE_CONFIG_DIR = claude;
    process.env.CODEX_HOME = fixtureDir();
    process.env.CURSOR_CONFIG_DIR = fixtureDir();
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = fixtureDir();
    process.env.CODEGRAPH_OPENCODE_DB = path.join(fixtureDir(), 'no-opencode.db');
    writeJsonl(
      path.join(claude, 'projects', claudeProjectSlug(root), 'c1.jsonl'),
      [entry('an unrelated prompt about parsing')],
      1_700_000_000,
    );
    return root;
  };
  const sessions = (root: string): string[] =>
    [...new Set(querySessions(root, 'write-time dedupe').hits.map((h) => h.session))].sort();
  /** Replace `store` with something that exists but cannot be opened as one, run, then restore it. */
  const blocked = (store: string, as: 'directory' | 'file', run: () => void): void => {
    const aside = `${store}.aside`;
    fs.renameSync(store, aside);
    if (as === 'directory') fs.mkdirSync(store);
    else fs.writeFileSync(store, '');
    try {
      run();
    } finally {
      fs.rmSync(store, { recursive: true, force: true });
      fs.renameSync(aside, store);
    }
  };

  it('keeps Devin and OpenCode sessions indexed while their databases cannot be opened', () => {
    const root = project();
    const devinRoot = fixtureDir();
    process.env.CODEGRAPH_DEVIN_DIR = devinRoot;
    const devinDb = path.join(devinRoot, 'cli-next', 'sessions.db');
    fs.mkdirSync(path.dirname(devinDb), { recursive: true });
    const devin = createDatabase(devinDb).db;
    devin.exec(`
      CREATE TABLE sessions (id TEXT PRIMARY KEY, working_directory TEXT NOT NULL, title TEXT,
        last_activity_at INTEGER NOT NULL, hidden INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE message_nodes (row_id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
        node_id INTEGER NOT NULL, chat_message TEXT NOT NULL, created_at INTEGER NOT NULL);
    `);
    devin.prepare('INSERT INTO sessions VALUES (?, ?, ?, ?, 0)').run('brisk-otter', root, 'devin', 1_700_000_100);
    devin
      .prepare('INSERT INTO message_nodes (session_id, node_id, chat_message, created_at) VALUES (?, ?, ?, ?)')
      .run('brisk-otter', 1, JSON.stringify({ role: 'user', content: 'keep the write-time dedupe in Devin' }), 1_700_000_010);
    devin.close();

    const ocDb = path.join(fixtureDir(), 'opencode.db');
    process.env.CODEGRAPH_OPENCODE_DB = ocDb;
    const oc = createDatabase(ocDb).db;
    oc.exec(`
      CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT, time_updated INTEGER);
      CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
      CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT, time_updated INTEGER);
    `);
    oc.prepare('INSERT INTO session VALUES (?, ?, ?, ?)').run('ses_match', root, 'opencode', 1_700_000_000_000);
    oc.prepare('INSERT INTO message VALUES (?, ?, ?, ?)').run('msg1', 'ses_match', 1_700_000_000_000, JSON.stringify({ role: 'user' }));
    oc.prepare('INSERT INTO part VALUES (?, ?, ?, ?, ?)').run(
      'prt1', 'msg1', 'ses_match', JSON.stringify({ type: 'text', text: 'keep the write-time dedupe in OpenCode' }), 1_700_000_000_000,
    );
    oc.close();
    (globalThis as { Bun?: { gc?: (force: boolean) => void } }).Bun?.gc?.(true);

    const both = ['devin:brisk-otter', 'opencode:ses_match'];
    expect(sessions(root)).toEqual(both);
    blocked(devinDb, 'directory', () => {
      blocked(ocDb, 'directory', () => {
        expect(sessions(root)).toEqual(both);
      });
    });
    // Back to readable: nothing was forgotten, so nothing is read again.
    const again = querySessions(root, 'write-time dedupe');
    expect(again.index.refreshed).toBe(0);
    expect([...new Set(again.hits.map((h) => h.session))].sort()).toEqual(both);
  });

  it('keeps Codex sessions indexed while the sessions directory cannot be listed', () => {
    const root = project();
    const sessionsDir = path.join(process.env.CODEX_HOME!, 'sessions');
    writeJsonl(
      path.join(sessionsDir, 'rollout-a.jsonl'),
      [
        { timestamp: at, type: 'session_meta', payload: { session_id: 'codex-a', cwd: root } },
        {
          timestamp: at,
          type: 'response_item',
          payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'keep the write-time dedupe in Codex' }] },
        },
      ],
      1_700_000_000,
    );
    expect(sessions(root)).toEqual(['codex:codex-a']);
    blocked(sessionsDir, 'file', () => {
      expect(sessions(root)).toEqual(['codex:codex-a']);
    });
  });

  // File permissions block reads only on POSIX, and not for root.
  it.runIf(process.platform !== 'win32' && process.getuid?.() !== 0)(
    'keeps a Codex session indexed while its rollout cannot be read',
    () => {
      const root = project();
      const rollout = path.join(process.env.CODEX_HOME!, 'sessions', 'rollout-a.jsonl');
      writeJsonl(
        rollout,
        [
          { timestamp: at, type: 'session_meta', payload: { session_id: 'codex-a', cwd: root } },
          {
            timestamp: at,
            type: 'response_item',
            payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'keep the write-time dedupe in Codex' }] },
          },
        ],
        1_700_000_000,
      );
      expect(sessions(root)).toEqual(['codex:codex-a']);
      fs.chmodSync(rollout, 0o000);
      try {
        expect(sessions(root)).toEqual(['codex:codex-a']);
      } finally {
        fs.chmodSync(rollout, 0o644);
      }
      expect(querySessions(root, 'write-time dedupe').index.refreshed).toBe(0);
    },
  );

  it('answers from the index when the only transcript store cannot be read', () => {
    const root = fixtureDir();
    fs.mkdirSync(path.join(root, '.codegraph'));
    process.env.CLAUDE_CONFIG_DIR = fixtureDir();
    process.env.CURSOR_CONFIG_DIR = fixtureDir();
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = fixtureDir();
    process.env.CODEGRAPH_OPENCODE_DB = path.join(fixtureDir(), 'no-opencode.db');
    process.env.CODEX_HOME = fixtureDir();
    const sessionsDir = path.join(process.env.CODEX_HOME, 'sessions');
    writeJsonl(
      path.join(sessionsDir, 'rollout-a.jsonl'),
      [
        { timestamp: at, type: 'session_meta', payload: { session_id: 'codex-a', cwd: root } },
        {
          timestamp: at,
          type: 'response_item',
          payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'keep the write-time dedupe in Codex' }] },
        },
      ],
      1_700_000_000,
    );
    expect(sessions(root)).toEqual(['codex:codex-a']);
    blocked(sessionsDir, 'file', () => {
      expect(sessions(root)).toEqual(['codex:codex-a']);
    });
  });

  it('still forgets sessions whose store was removed', () => {
    const root = project();
    const sessionsDir = path.join(process.env.CODEX_HOME!, 'sessions');
    writeJsonl(
      path.join(sessionsDir, 'rollout-a.jsonl'),
      [
        { timestamp: at, type: 'session_meta', payload: { session_id: 'codex-a', cwd: root } },
        {
          timestamp: at,
          type: 'response_item',
          payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'keep the write-time dedupe in Codex' }] },
        },
      ],
      1_700_000_000,
    );
    expect(sessions(root)).toEqual(['codex:codex-a']);
    fs.rmSync(sessionsDir, { recursive: true });
    expect(sessions(root)).toEqual([]);
  });
});

describe('repeat scans', () => {
  const meta = (cwd: string, id: string) => ({ timestamp: at, type: 'session_meta', payload: { session_id: id, cwd } });
  const codexStore = (): string => {
    const home = fixtureDir();
    process.env.CODEX_HOME = home;
    return path.join(home, 'sessions');
  };

  it.runIf(process.platform !== 'win32' && process.getuid?.() !== 0)(
    'matches a Codex rollout seen before without reading it again',
    () => {
      const project = fixtureDir();
      const file = path.join(codexStore(), 'rollout-seen.jsonl');
      writeJsonl(file, [meta(project, 'codex-seen')], 1_700_000_000);
      const index = SessionsIndex.open(':memory:');
      try {
        expect(index.codexFiles([project], [], [])).toEqual([file]);
        fs.chmodSync(file, 0o000);
        const unavailable: string[] = [];
        expect(index.codexFiles([project], [], unavailable)).toEqual([file]);
        expect(unavailable).toEqual([]);
      } finally {
        fs.chmodSync(file, 0o644);
        index.close();
      }
    },
  );

  it('reads a Codex rollout again until its first line is complete, and again once it is replaced', () => {
    const project = fixtureDir();
    const other = fixtureDir();
    const file = path.join(codexStore(), 'rollout-growing.jsonl');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const line = JSON.stringify(meta(project, 'codex-growing'));
    fs.writeFileSync(file, line.slice(0, 20));
    const index = SessionsIndex.open(':memory:');
    try {
      expect(index.codexFiles([project], [], [])).toEqual([]);
      fs.appendFileSync(file, `${line.slice(20)}\n`);
      expect(index.codexFiles([project], [], [])).toEqual([file]);
      // A new file at the same path is a different session.
      const replacement = `${file}.new`;
      writeJsonl(replacement, [meta(other, 'codex-other')], 1_700_000_000);
      fs.renameSync(replacement, file);
      expect(index.codexFiles([project], [], [])).toEqual([]);
    } finally {
      index.close();
    }
  });

  /** An AGY store with three conversations, each placed in `project` by a different source. */
  const agyStore = (project: string): { root: string; files: string[] } => {
    const root = fixtureDir();
    process.env.CODEGRAPH_ANTIGRAVITY_DIR = root;
    const transcript = (id: string): string => {
      const file = path.join(root, 'brain', id, '.system_generated', 'logs', 'transcript_full.jsonl');
      writeJsonl(file, [{ type: 'USER_INPUT', created_at: at, content: `keep the write-time dedupe in ${id}` }], 1_700_000_000);
      return file;
    };
    const files = ['by-metadata', 'by-summary', 'by-blob', 'elsewhere'].map(transcript);
    fs.mkdirSync(path.join(root, 'cache'));
    fs.writeFileSync(
      path.join(root, 'cache', 'conversation_metadata.json'),
      JSON.stringify({ conversations: { 'by-metadata': { summary: { WorkspaceURIs: [`file://${project}`] } } } }),
    );
    const summaries = createDatabase(path.join(root, 'conversation_summaries.db')).db;
    summaries.exec('CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, workspace_uris TEXT)');
    const put = summaries.prepare('INSERT INTO conversation_summaries VALUES (?, ?)');
    put.run('by-summary', JSON.stringify([`file://${project}`]));
    put.run('elsewhere', JSON.stringify([`file://${fixtureDir()}`]));
    put.run('malformed', '{not json');
    summaries.close();
    fs.mkdirSync(path.join(root, 'conversations'));
    const blob = createDatabase(path.join(root, 'conversations', 'by-blob.db')).db;
    blob.exec('CREATE TABLE trajectory_metadata_blob (data BLOB)');
    blob.prepare('INSERT INTO trajectory_metadata_blob VALUES (?)').run(Buffer.from(`ws file://${project} end`, 'latin1'));
    blob.close();
    return { root, files: files.slice(0, 3) };
  };

  it('places AGY conversations by metadata file, summaries database or conversation database', () => {
    const project = fixtureDir();
    const { files } = agyStore(project);
    expect(agyFilesForProject([project]).sort()).toEqual([...files].sort());
  });

  it('keeps AGY conversations indexed while the summaries database cannot be opened', () => {
    const project = fixtureDir();
    fs.mkdirSync(path.join(project, '.codegraph'));
    process.env.CLAUDE_CONFIG_DIR = fixtureDir();
    process.env.CODEX_HOME = fixtureDir();
    process.env.CURSOR_CONFIG_DIR = fixtureDir();
    process.env.CODEGRAPH_OPENCODE_DB = path.join(fixtureDir(), 'no-opencode.db');
    const { root } = agyStore(project);
    const found = () => [...new Set(querySessions(project, 'write-time dedupe').hits.map((h) => h.session))].sort();
    const expected = ['agy:by-blob', 'agy:by-metadata', 'agy:by-summary'];
    expect(found()).toEqual(expected);
    const db = path.join(root, 'conversation_summaries.db');
    fs.renameSync(db, `${db}.aside`);
    fs.mkdirSync(db);
    expect(found()).toEqual(expected);
    const unavailable: string[] = [];
    expect(agyFilesForProject([project], unavailable)).toEqual([]);
    expect(unavailable).toEqual([path.join(root, 'brain') + path.sep]);
  });
});

describe('whole passages (full)', () => {
  const store = (texts: string[]): string => {
    const project = fixtureDir();
    fs.mkdirSync(path.join(project, '.codegraph'));
    const transcripts = fixtureDir();
    writeJsonl(path.join(transcripts, 's1.jsonl'), texts.map((t) => user(t)), 1_700_000_000);
    process.env.CODEGRAPH_SESSIONS_DIR = transcripts;
    return project;
  };

  it('returns the stored passage beside the snippet only when asked, in the printed answer too', () => {
    const tail = ' the second paragraph line explains the retry budget in detail and names the limit of three attempts.';
    const project = store([`Decision: keep write-time dedupe.\n\nThe table lists:\n| a | b |\n|---|---|${tail}`]);
    const plain = querySessions(project, 'write-time dedupe');
    expect(plain.hits[0]!.text).toBeUndefined();
    expect(plain.fullCut).toBeUndefined();
    const full = querySessions(project, 'write-time dedupe', { full: true });
    expect(full.hits[0]!.text).toContain('| a | b |\n|---|---|');
    expect(full.hits[0]!.snippet).toBe(plain.hits[0]!.snippet);
    expect(full.fullCut).toBe(0);
    const printed = formatSessionHits('write-time dedupe', full);
    expect(printed).toContain('> | a | b |\n> |---|---|');
    expect(printed).toContain('> Decision: keep write-time dedupe.');
    expect(formatSessionHits('write-time dedupe', plain)).not.toContain('> | a | b |');
  });

  it('spends one byte budget in rank order and says how many hits fell back to the snippet', () => {
    const body = (i: number) => `Note ${i} on budgetword: ${`alpha${i} beta${i} gamma${i} `.repeat(160)}`.slice(0, 3000);
    const project = store(Array.from({ length: 7 }, (_, i) => body(i)));
    const result = querySessions(project, 'budgetword', { full: true });
    expect(result.hits).toHaveLength(7);
    const whole = result.hits.filter((h) => h.text !== undefined);
    const bytes = whole.reduce((n, h) => n + Buffer.byteLength(h.text!), 0);
    expect(bytes).toBeLessThanOrEqual(FULL_PASSAGE_BUDGET);
    expect(whole.length).toBeGreaterThan(0);
    expect(whole.length).toBeLessThan(7);
    expect(result.fullCut).toBe(7 - whole.length);
    // Whole passages are a prefix of the ranking: a smaller later hit never jumps the queue.
    expect(result.hits.slice(0, whole.length).every((h) => h.text !== undefined)).toBe(true);
    const printed = formatSessionHits('budgetword', result);
    expect(printed).toContain(`Whole passages are limited to 16,000 bytes in all: the last ${result.fullCut} hits show only the matching snippet.`);
  });

  it('keeps the role filter and the one-hit wording', () => {
    const project = store([`Decision: keep write-time dedupe. ${'x'.repeat(2000)}`.replace(/x/g, 'padding ')]);
    const hit = querySessions(project, 'write-time dedupe', { full: true, role: 'user' });
    expect(hit.hits).toHaveLength(1);
    expect(querySessions(project, 'write-time dedupe', { full: true, role: 'assistant' }).hits).toEqual([]);
  });
});
