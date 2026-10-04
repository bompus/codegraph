/**
 * Explore lists the symbol-shaped names it could not find. A query that mixes
 * real names with a guessed one (`finalHistoryReconciled`) used to come back
 * with the real names' code and no word about the guess, so the agent read
 * on as if the returned code held it. The summary now names the misses, and
 * stays quiet for words that only look precise (a sentence-initial `How`),
 * quoted literals, pinned files, camel infixes that seed by definer, and names
 * that appear in source without a node of their own (imports, parameters,
 * object keys, module stems).
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';

const FILES: Record<string, string> = {
  'src/snapshot.ts': [
    'export function verifySnapshot(rows: string[]): boolean {',
    '  const key = getSnapshotKey(rows);',
    "  return key.length > 0 && localStorage.getItem('draft.pick_state') !== null;",
    '}',
    '',
    'export function getSnapshotKey(rows: string[]): string {',
    "  return rows.join(':');",
    '}',
    '',
  ].join('\n'),
  'src/draftStoreState.ts': [
    "import { spawnSync } from 'child_process';",
    '',
    'export function runDraft(projectPath: string): number {',
    "  const events = { DRAFT_STATE_UPDATED: 'updated', maxBuffer: 1024 };",
    "  const out = spawnSync('draft', [projectPath], { maxBuffer: events.maxBuffer });",
    '  process.exitCode = out.status ?? 1;',
    '  return out.status ?? 1;',
    '}',
    '',
  ].join('\n'),
  // A stem whose extension the explore token cleanup does not strip.
  'config/draft_rules.yaml': 'a: 1\n',
  // Prose that names the guess: docs are not code, so this must not hide it.
  'docs/notes.md': '# Notes\n\nWe planned `finalHistoryReconciled` but never wrote it.\n',
  'src/pickLedger.ts': [
    "import { verifySnapshot } from './snapshot';",
    '',
    'export function recordPick(rows: string[]): void {',
    '  if (!verifySnapshot(rows)) throw new Error("bad snapshot");',
    '}',
    '',
  ].join('\n'),
};

let dir: string;
let cg: CodeGraph;

async function explore(query: string): Promise<string> {
  const res = await new ToolHandler(cg).execute('codegraph_explore', { query });
  return res.content?.[0]?.text ?? '';
}

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-unmatched-'));
  for (const [rel, text] of Object.entries(FILES)) {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), text);
  }
  cg = CodeGraph.initSync(dir);
  await cg.indexAll();
}, 180_000);

afterAll(() => {
  cg?.destroy();
  if (dir && fs.existsSync(dir)) fs.rmSync(dir, { recursive: true, force: true });
});

describe('unmatched names in the explore summary', () => {
  it('names a guessed symbol beside the real one it still answers', async () => {
    const out = await explore('verifySnapshot finalHistoryReconciled');
    expect(out).toContain('**`src/snapshot.ts`');
    expect(out).toContain('Not found in the index: `finalHistoryReconciled`.');
  });

  // An apostrophe is not a quote: the guess in a plain sentence is still a
  // guess, and a double-quoted phrase stays a phrase around one.
  it('reads apostrophes as part of words, not as quotes', async () => {
    const out = await explore("verifySnapshot don't call finalHistoryReconciled if it's gone");
    expect(out).toContain('Not found in the index: `finalHistoryReconciled`.');
    for (const query of [
      'verifySnapshot "we don\'t call finalHistoryReconciled here"',
      "verifySnapshot 'we don't call finalHistoryReconciled here'",
    ]) {
      expect(await explore(query), query).not.toContain('Not found in the index');
    }
  });

  // Quotes pair left to right, and a word quoted once but also written bare
  // is still the agent's own guess.
  it('reads the words between and outside quoted spans as unquoted', async () => {
    for (const query of [
      'verifySnapshot "draft" finalHistoryReconciled "state"',
      'verifySnapshot "we skip finalHistoryReconciled here" then finalHistoryReconciled',
      'verifySnapshot "we skip finalHistoryReconciled here" then "finalHistoryReconciled"',
    ]) {
      expect(await explore(query), query).toContain('Not found in the index: `finalHistoryReconciled`.');
    }
  });

  // A quoted lone name is a name the agent expects in code, unlike a quoted
  // literal that some file holds.
  it('lists a quoted name that neither the graph nor any literal holds', async () => {
    expect(await explore('verifySnapshot "finalHistoryReconciled"')).toContain('Not found in the index: `finalHistoryReconciled`.');
  });

  it('lists qualified and snake_case guesses too', async () => {
    const out = await explore('verifySnapshot Ledger.replayAll draft_state_guard');
    expect(out).toContain('`Ledger.replayAll`');
    expect(out).toContain('`draft_state_guard`');
  });

  it('stays quiet for prose, quoted literals, pinned files and camel infixes', async () => {
    for (const query of [
      'How does verifySnapshot work',
      'verifySnapshot "draft.pick_state"',
      'pickLedger.ts recordPick',
      'verifySnapshot snapshotKey',
      'verifySnapshot "snapshotKey"',
      // Real code the node table does not hold: an import, a parameter, a
      // module stem, object keys, a global member, a version string.
      'runDraft spawnSync projectPath',
      'runDraft draftStoreState',
      'runDraft DRAFT_STATE_UPDATED maxBuffer',
      'runDraft process.exitCode',
      'runDraft v1.0.352',
      'verifySnapshot draft_rules',
      'verifySnapshot "some finalHistoryReconciled phrase"',
    ]) {
      expect(await explore(query), query).not.toContain('Not found in the index');
    }
  });

  // A file that cannot be read leaves the absence unproven: the guess may be
  // an object key in exactly that file.
  it.runIf(process.platform !== 'win32' && process.getuid?.() !== 0)('lists nothing when a source file cannot be read', async () => {
    const file = path.join(dir, 'src/draftStoreState.ts');
    fs.chmodSync(file, 0o000);
    try {
      expect(await explore('verifySnapshot finalHistoryReconciled')).not.toContain('Not found in the index');
    } finally {
      fs.chmodSync(file, 0o644);
    }
  });
});
