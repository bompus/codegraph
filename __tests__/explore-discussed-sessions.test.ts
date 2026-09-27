/**
 * codegraph_explore "Discussed in earlier sessions" section: the entry symbols
 * that earlier agent sessions named, read from the session index as it stands
 * (no refresh). Plain-word names are not looked up, and the section is absent
 * when the project has no session index.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';
import { SessionsIndex, sessionsDbPath } from '../src/sessions';

describe('codegraph_explore — discussed in earlier sessions', () => {
  let testDir: string;
  let cg: CodeGraph;
  let handler: ToolHandler;

  beforeEach(async () => {
    testDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-discussed-'));
    fs.mkdirSync(path.join(testDir, 'src'));
    fs.writeFileSync(
      path.join(testDir, 'src', 'ring.ts'),
      'export function flushRingBuffer() { return 1; }\nexport function caller() { return flushRingBuffer(); }\n',
    );
    cg = CodeGraph.initSync(testDir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
    handler = new ToolHandler(cg);
  });

  afterEach(() => {
    if (cg) cg.destroy();
    fs.rmSync(testDir, { recursive: true, force: true });
  });

  const explore = async (query: string) =>
    (await handler.execute('codegraph_explore', { query })).content[0].text as string;

  it('names the sessions that mentioned an entry symbol', async () => {
    expect(await explore('flushRingBuffer')).not.toContain('Discussed in earlier sessions');

    const transcripts = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-discussed-t-'));
    const line = (text: string) =>
      JSON.stringify({ type: 'user', timestamp: '2026-09-20T10:00:00.000Z', message: { content: text } });
    fs.writeFileSync(path.join(transcripts, 'aaaa-1.jsonl'), line('why does flushRingBuffer drop the oldest entries?') + '\n');
    fs.writeFileSync(path.join(transcripts, 'bbbb-2.jsonl'), line('an unrelated conversation about the caller') + '\n');
    const index = SessionsIndex.open(sessionsDbPath(testDir));
    index.refresh(transcripts);
    index.close();
    fs.rmSync(transcripts, { recursive: true, force: true });

    const text = await explore('flushRingBuffer');
    expect(text).toContain('**Discussed in earlier sessions**');
    expect(text).toContain('- `flushRingBuffer`: aaaa-1 (2026-09-20)');
    expect(text).not.toContain('bbbb-2');
  });
});
