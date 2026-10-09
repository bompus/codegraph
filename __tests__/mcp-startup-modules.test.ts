/**
 * Startup module graph: loading the MCP server entry (what every `serve --mcp`
 * process does before it knows its mode) must not load the file watcher,
 * extraction or resolution. Only a session that opens a project needs them,
 * and they cost every other session memory.
 */
import { describe, it, expect } from 'vitest';
import { execFileSync } from 'child_process';
import * as path from 'path';

const DIST = path.resolve(__dirname, '../dist');

function loadedAfterRequire(entry: string): string[] {
  const script = `require(${JSON.stringify(path.join(DIST, entry))});` +
    `process.stdout.write(JSON.stringify(Object.keys(require.cache)));`;
  const out = execFileSync(process.execPath, ['-e', script], { encoding: 'utf8' });
  return (JSON.parse(out) as string[]).map((f) => path.relative(DIST, f).split(path.sep).join('/'));
}

describe('MCP startup modules', () => {
  it('loading the server entry leaves the watcher, extraction and the CodeGraph core unloaded', () => {
    const loaded = loadedAfterRequire('mcp/index.js');
    expect(loaded).toContain('mcp/proxy.js');
    for (const heavy of ['sync/watcher.js', 'extraction/index.js', 'resolution/index.js', 'codegraph.js']) {
      expect(loaded).not.toContain(heavy);
    }
  });
});
