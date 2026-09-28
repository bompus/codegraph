/**
 * The fork's maintenance scripts: index metrics and the call-edge diff
 * (scripts/index-metrics.mjs), the upstream-PR lockfile guard
 * (scripts/pr-guard.mjs), and probe-explore's row format and MCP mode
 * (scripts/agent-eval/probe-explore.mjs). The probe rows are a contract: a
 * deployment gate reads `sourceAt` and `mustMatchAt` from them.
 */
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
// vite strips the node: prefix from a static sqlite import, so require it.
const { DatabaseSync } = require('node:sqlite') as typeof import('node:sqlite');
import { afterEach, describe, expect, it } from 'vitest';
// @ts-expect-error untyped .mjs script
import { callEdgeKey, CALL_EDGES_SQL, diffCallEdges, formatEdgeDiff, readIndexMetrics, summarizeMetrics } from '../scripts/index-metrics.mjs';
// @ts-expect-error untyped .mjs script
import { deniedPrFiles } from '../scripts/pr-guard.mjs';
// @ts-expect-error untyped .mjs script
import { probeRow, selectTasks } from '../scripts/agent-eval/probe-explore.mjs';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});
function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), 'cg-fork-tooling-'));
  roots.push(root);
  return root;
}

describe('index-metrics', () => {
  const edge = (over = {}) => ({
    src: 'readRow', srcFile: 'callers.ts', dst: 'get', dstFile: 'storage.ts',
    refName: 'get', resolvedBy: 'exact-match', confidence: 0.9, ...over,
  });

  it('keys a call edge without its resolver or line', () => {
    expect(callEdgeKey(edge())).toBe(callEdgeKey(edge({ resolvedBy: 'fuzzy', confidence: 0.5 })));
    expect(callEdgeKey(edge())).not.toBe(callEdgeKey(edge({ refName: 'set' })));
  });

  it('counts multiplicities, so two call sites losing one is one loss', () => {
    const e = (src: string, dst: string) => edge({ src, srcFile: `${src}.ts`, dst, dstFile: `${dst}.ts`, refName: dst });
    const before = [e('a', 'get'), e('a', 'get'), e('b', 'set')];
    const after = [e('a', 'get'), e('c', 'ping')];
    const diff = diffCallEdges(before, after);
    expect(diff).toMatchObject({ totalA: 3, totalB: 2 });
    expect(diff.onlyInA).toEqual([
      expect.objectContaining({ src: 'a', n: 1 }),
      expect.objectContaining({ src: 'b', n: 1 }),
    ]);
    expect(diff.onlyInB).toEqual([expect.objectContaining({ src: 'c', n: 1 })]);
    expect(diffCallEdges(before, before).onlyInA).toEqual([]);
    const text = formatEdgeDiff(diff, 'daily', 'pr-1707');
    expect(text).toContain('daily: 3 calls edges; pr-1707: 2');
    expect(text).toContain('only in daily (2):');
    expect(text).toContain('1x a (a.ts) -> get (get.ts) ref=get exact-match@0.9');
  });

  it('reads metrics from a real index and summarizes only what moved', () => {
    const db = new DatabaseSync(':memory:');
    db.exec(`CREATE TABLE nodes (id TEXT, kind TEXT, name TEXT, qualified_name TEXT, file_path TEXT);
      CREATE TABLE edges (source TEXT, target TEXT, kind TEXT, metadata TEXT);
      CREATE TABLE files (path TEXT);
      INSERT INTO files VALUES ('a.ts');
      INSERT INTO nodes VALUES ('i','interface','Store','Store','a.ts'), ('m','method','get','Store::get','a.ts'),
        ('f','function','run','run','a.ts'), ('v','variable','cfg','cfg','a.ts');
      INSERT INTO edges VALUES ('f','f','calls','{"resolvedBy":"exact-match","refName":"run"}'),
        ('f','m','calls','{"resolvedBy":"import","refName":"get"}'),
        ('v','f','calls',NULL), ('f','m','imports',NULL);`);
    const before = readIndexMetrics(db, 'daily');
    expect(before).toMatchObject({
      label: 'daily', nodes: 4, edges: 4, files: 1, selfCallEdges: 1, interfaces: 1,
      interfaceMembers: 1, importToMemberEdges: 1, initializerCallEdges: 1,
    });
    expect(before.resolvedBy).toEqual({ 'exact-match': 1, import: 1, unresolved: 1 });
    expect(db.prepare(CALL_EDGES_SQL).all()).toHaveLength(3);
    const line = summarizeMetrics(before, { ...before, label: 'pr', edges: 3, selfCallEdges: 0 });
    expect(line).toContain('edges 4 -> 3');
    expect(line).toContain('self-calls 1 -> 0');
    expect(line).toContain('import-resolved 1,');
    db.close();
  });
});

describe('pr-guard', () => {
  it("rejects a stray lockfile at any depth unless allowed; npm's lockfile passes", () => {
    const added = ['bun.lock', 'ui/yarn.lock', 'site/package-lock.json', 'src/lock.ts'];
    expect(deniedPrFiles(added)).toEqual(['bun.lock', 'ui/yarn.lock']);
    expect(deniedPrFiles(added, ['ui/yarn.lock'])).toEqual(['bun.lock']);
  });
});

describe('probe-explore rows', () => {
  it('reports the source offset, rendered files and each pattern offset', () => {
    const text = 'intro\n**Source Code**\n**`src/a.ts`** (2 symbols)\ncode\n**Flow** `x`\n**`src/b.ts`**\n';
    const row = probeRow('t1', text, 12, ['src/b\\.ts', 'absent']);
    expect(row).toEqual({
      id: 't1', ms: 12, chars: text.length, sourceAt: 6,
      files: ['src/a.ts', 'src/b.ts'], mustMatchAt: [text.indexOf('src/b.ts'), -1],
    });
    expect(probeRow('t2', 'no source here', 1).sourceAt).toBe(-1);
  });

  it('selects tasks by id globs', () => {
    const tasks = [{ id: 'md-1' }, { id: 'md-2' }, { id: 'cx-1' }, { id: 'mdx' }];
    expect(selectTasks(tasks, 'md-*').map((t: { id: string }) => t.id)).toEqual(['md-1', 'md-2']);
    expect(selectTasks(tasks, 'cx-1,mdx').map((t: { id: string }) => t.id)).toEqual(['cx-1', 'mdx']);
    expect(selectTasks(tasks, undefined)).toHaveLength(4);
  });
});

describe('probe-explore --mcp', () => {
  const script = resolve(__dirname, '../scripts/agent-eval/probe-explore.mjs');

  function probe(mode: string) {
    const root = scratch();
    const bin = join(root, 'build', 'dist', 'bin');
    mkdirSync(bin, { recursive: true });
    writeFileSync(join(bin, 'codegraph.js'), `const mode = ${JSON.stringify(mode)};
console.error("SERVER_PID=" + process.pid);
console.error("server diagnostic: " + mode);
require("node:readline").createInterface({input: process.stdin}).on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  const initializing = request.method === "initialize";
  if (mode === (initializing ? "timeout-init" : "timeout-query")) return;
  if (mode === (initializing ? "exit-init" : "exit-query")) process.exit(23);
  const message = {jsonrpc:"2.0", id:request.id};
  if (!initializing && mode === "rpc-error") message.error = {code:-32603, message:"fixture RPC failure"};
  else message.result = initializing ? {} : {
    isError: mode === "tool-error",
    content:[{type:"text", text: mode === "tool-error" ? "fixture tool failure" : "**Source Code**\\n**\\u0060fixture.ts\\u0060**\\nfixture source"}]
  };
  console.log(JSON.stringify(message));
});
`);
    writeFileSync(join(root, 'build', 'package.json'), '{"type":"commonjs"}');
    const out = join(root, 'out');
    const result = spawnSync(process.execPath, [
      script, root, 'fixture', '--mcp', '--build', join(root, 'build'),
      '--out', out, '--timeout-ms', '1000', '--expect', 'fixture\\.ts',
    ], { encoding: 'utf8', timeout: 30_000 });
    const pid = Number(/SERVER_PID=(\d+)/.exec(result.stderr)?.[1]);
    return { result, pid, out };
  }

  it.each([
    ['timeout-init', 'initialize: timed out after 1000ms'],
    ['timeout-query', 'tools/call: timed out after 1000ms'],
    ['exit-init', 'initialize: server exited with code 23'],
    ['exit-query', 'tools/call: server exited with code 23'],
    ['rpc-error', 'fixture RPC failure'],
    ['tool-error', 'fixture tool failure'],
  ])('reports %s, keeps server stderr and reaps the server', (mode, diagnostic) => {
    const { result, pid } = probe(mode);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain(diagnostic);
    expect(result.stderr).toContain(`server diagnostic: ${mode}`);
    expect(result.stdout).not.toContain('"id":"literal"');
    expect(pid).toBeGreaterThan(0);
    expect(() => process.kill(pid, 0)).toThrow();
  });

  it('writes the row and payload for a successful probe and reaps the server', () => {
    const { result, pid, out } = probe('success');
    expect(result.status).toBe(0);
    const rows = result.stdout.trim().split('\n').map((line) => JSON.parse(line));
    expect(rows[0]).toMatchObject({ id: 'literal', sourceAt: 0, files: ['fixture.ts'], mustMatchAt: [19] });
    expect(rows[1].id).toBe('memory');
    expect(readFileSync(join(out, 'literal.md'), 'utf8')).toContain('fixture source');
    expect(() => process.kill(pid, 0)).toThrow();
  });
});
