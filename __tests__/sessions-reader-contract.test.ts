/**
 * The shared session-reader contract (`__tests__/fixtures/session-reader-contract/`): the prose entries every
 * reader must index identically. Another indexer reads the same fixtures; see the README beside them.
 */
import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { parseEntries, transcriptDocs } from '../src/sessions/claude-code';
import { parseCodexTranscript } from '../src/sessions/codex';

const DIR = path.join(__dirname, 'fixtures', 'session-reader-contract');
type Entry = { host: string; session: string; role: string; ts: string; text: string };
const expected: Entry[] = JSON.parse(fs.readFileSync(path.join(DIR, 'expected.json'), 'utf8'));

function read(host: 'claude' | 'codex'): Entry[] {
  const dir = path.join(DIR, host);
  return fs.readdirSync(dir).flatMap((name) => {
    const file = path.join(dir, name);
    if (host === 'claude') {
      const session = path.basename(name, '.jsonl');
      return transcriptDocs(parseEntries(file)).map((d) => ({ host, session, role: d.role, ts: d.ts, text: d.text }));
    }
    const parsed = parseCodexTranscript(file);
    return parsed.docs.map((d) => ({ host, session: parsed.session.replace(/^codex:/, ''), role: d.role, ts: d.ts, text: d.text }));
  });
}

describe('session reader contract', () => {
  for (const host of ['claude', 'codex'] as const) {
    it(`${host} reader returns exactly the shared prose entries`, () => {
      expect(read(host)).toEqual(expected.filter((e) => e.host === host));
    });
  }
});
