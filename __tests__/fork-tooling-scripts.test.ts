/**
 * The fork's maintenance scripts: index metrics and the call-edge diff
 * (scripts/index-metrics.mjs), the upstream-PR lockfile guard
 * (scripts/pr-guard.mjs), probe-explore's row format and MCP mode
 * (scripts/agent-eval/probe-explore.mjs), and the changelog rebuild after an
 * upstream merge (scripts/changelog-reconcile.mjs). The probe rows are a
 * contract: a deployment gate reads `sourceAt` and `mustMatchAt` from them.
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
// @ts-expect-error untyped .mjs script
import { reconcileChangelog } from '../scripts/changelog-reconcile.mjs';

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

describe('changelog-reconcile', () => {
  const HEAD = '# Changelog\n\nIntro.\n\n## [Unreleased]';
  const log = (unreleased: string, ...releases: string[]) =>
    [HEAD, unreleased, ...releases].join('\n').replace(/\n*$/, '\n');
  const OLD = '## [1.0.0] - 2026-01-01\n\n### Fixes\n\n- Old fix.\n';
  const unreleasedOf = (text: string) => text.split('## [Unreleased]\n')[1].split('\n## [')[0];

  it('drops entries upstream released, continuation lines included, and keeps the fork-only ones', () => {
    const shared = '- Shared fix.\n  - with a nested point;\n  and a closing line.';
    const fork = log(`\n### Fixes\n\n${shared}\n\n- Fork fix.\n  More about it.\n`, OLD);
    const upstream = log('', `## [1.1.0] - 2026-02-01\n\n### Fixes\n\n${shared}\n`, OLD);
    const result = reconcileChangelog({ fork, upstream });
    expect(unreleasedOf(result.text)).toBe('\n### Fixes\n\n- Fork fix.\n  More about it.\n');
    expect(result.text.slice(result.text.indexOf('## [1.1.0]'))).toBe(upstream.slice(upstream.indexOf('## [1.1.0]')));
    expect(result.dropped).toEqual([{ entry: shared, reason: 'released in 1.1.0' }]);
  });

  it('drops an entry carried from upstream that upstream released under new wording', () => {
    const base = log('\n### Fixes\n\n- Calls through a module alias now reach the function it names.\n', OLD);
    const fork = log('\n### Fixes\n\n- Calls through a module alias now reach the function it names.\n- Fork fix.\n', OLD);
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n### Fixes\n\n- Calls made through a module alias now reach the function the alias names.\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork, upstream, base }).text)).toBe('\n### Fixes\n\n- Fork fix.\n');
    // Without the merge base the carried entry stays, flagged as reading like the released one.
    const result = reconcileChangelog({ fork, upstream });
    expect(unreleasedOf(result.text)).toContain('- Calls through a module alias now reach the function it names.');
    expect(result.review).toEqual([
      { entry: '- Calls through a module alias now reach the function it names.', like: '- Calls made through a module alias now reach the function the alias names.', release: '1.1.0' },
    ]);
  });

  it('dedupes against every release since the fork parent and reviews only against those', () => {
    const fork = log(
      '\n### Fixes\n\n- First release fix.\n\n- Second release fix.\n\n- Old fix, reworded by the fork.\n',
      OLD,
    );
    const upstream = log(
      '',
      '## [1.2.0] - 2026-03-01\n\n### Fixes\n\n- Second release fix.\n',
      '## [1.1.0] - 2026-02-01\n\n### Fixes\n\n- First release fix.\n',
      OLD,
    );
    const result = reconcileChangelog({ fork, upstream });
    expect(unreleasedOf(result.text)).toBe('\n### Fixes\n\n- Old fix, reworded by the fork.\n');
    expect(result.newReleases).toEqual(['1.2.0', '1.1.0']);
    expect(result.review).toEqual([]);
  });

  it('removes the headings a drop empties and keeps one already empty', () => {
    const fork = log(
      '\n### Removed\n\n### Fixes\n\n#### Parsing\n\n- Shared fix.\n\n#### Indexing\n\n- Fork fix.\n\n### Features\n\n- Shared feature.\n',
      OLD,
    );
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n- Shared fix.\n- Shared feature.\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork, upstream }).text)).toBe(
      '\n### Removed\n\n### Fixes\n\n#### Indexing\n\n- Fork fix.\n',
    );
  });

  it("adds upstream's own unreleased entries under their heading", () => {
    const fork = log('\n### Fixes\n\n- Fork fix.\n', OLD);
    const upstream = log('\n### Fixes\n\n- New upstream fix.\n\n### Features\n\n- New upstream feature.\n', OLD);
    const result = reconcileChangelog({ fork, upstream });
    expect(unreleasedOf(result.text)).toBe(
      '\n### Fixes\n\n- Fork fix.\n- New upstream fix.\n\n### Features\n\n- New upstream feature.\n',
    );
    expect(result.added).toEqual(['- New upstream fix.', '- New upstream feature.']);
  });

  it('leaves the file alone when upstream released nothing, and notes a released section the fork edited', () => {
    const fork = log('\n### Fixes\n\n- Fork fix.\n\n\n- Spaced fork fix.\n', OLD);
    expect(reconcileChangelog({ fork, upstream: log('', OLD), base: log('', OLD) })).toMatchObject({
      text: fork,
      tailDiffers: false,
    });
    const edited = reconcileChangelog({ fork, upstream: log('', OLD.replace('Old fix', 'Old fix, as upstream wrote it')) });
    expect(edited.tailDiffers).toBe(true);
    expect(edited.text).toContain('- Old fix, as upstream wrote it.');
  });

  it('drops only as many carried copies as the base had, under its heading first', () => {
    const base = log('\n### Fixes\n\n#### Go\n\n- Calls now resolve correctly.\n', OLD);
    const fork = log('\n### Fixes\n\n#### Go\n\n- Calls now resolve correctly.\n\n#### Rust\n\n- Calls now resolve correctly.\n', OLD);
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n- Go calls now resolve correctly.\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork, upstream, base }).text)).toBe(
      '\n### Fixes\n\n#### Rust\n\n- Calls now resolve correctly.\n',
    );
  });

  it('keeps an entry whose unindented continuation makes it differ from a released one', () => {
    const fork = log('\n### Fixes\n\n- Handles missing files\nand keeps fork-only symlinks.\n', OLD);
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n- Handles missing files\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork, upstream }).text)).toBe(
      '\n### Fixes\n\n- Handles missing files\nand keeps fork-only symlinks.\n',
    );
  });

  it('treats a whitespace-only line as blank and never widens the gap a removal leaves', () => {
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n- Released fix.\n', OLD);
    const spaced = reconcileChangelog({ fork: log('\n### Fixes\n\n- Released fix.\n  \n- Fork fix.\n', OLD), upstream });
    expect(unreleasedOf(spaced.text)).toBe('\n### Fixes\n\n- Fork fix.\n');
    const wide = reconcileChangelog({ fork: log('\n### Fixes\n\n- A.\n\n\n- Released fix.\n\n\n- B.\n', OLD), upstream });
    expect(unreleasedOf(wide.text)).toBe('\n### Fixes\n\n- A.\n\n\n- B.\n');
  });

  it("files upstream's entries under their own sub-heading, filling an empty one", () => {
    const fork = log('\n### Fixes\n\n#### Parsing\n\n- Fork parser.\n\n#### Indexing\n\n- Fork index.\n\n### Features\n', OLD);
    const upstream = log(
      '\n### Fixes\n\n#### Parsing\n\n- Upstream parser.\n\n#### Watching\n\n- Upstream watcher.\n\n### Features\n\n- Upstream feature.\n',
      OLD,
    );
    expect(unreleasedOf(reconcileChangelog({ fork, upstream }).text)).toBe(
      '\n### Fixes\n\n#### Parsing\n\n- Fork parser.\n- Upstream parser.\n\n#### Indexing\n\n- Fork index.\n\n#### Watching\n\n- Upstream watcher.\n\n### Features\n\n- Upstream feature.\n',
    );
  });

  it('keeps an empty sub-heading by position, not by its title', () => {
    const fork = log('\n### Features\n\n#### Other\n\n### Fixes\n\n#### Other\n\n- Released fix.\n\n- Fork fix.\n', OLD);
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n- Released fix.\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork, upstream }).text)).toBe(
      '\n### Features\n\n#### Other\n\n### Fixes\n\n#### Other\n\n- Fork fix.\n',
    );
    const emptied = log('\n### Features\n\n#### Other\n\n### Fixes\n\n#### Other\n\n- Released fix.\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork: emptied, upstream }).text)).toBe('\n### Features\n\n#### Other\n');
  });

  it("counts carried copies per heading before matching copies the fork moved", () => {
    const base = log('\n### Fixes\n\n#### Go\n\n- Calls resolve.\n\n#### Rust\n\n- Calls resolve.\n', OLD);
    const fork = log('\n### Fixes\n\n#### Go\n\n- Calls resolve.\n- Calls resolve.\n\n#### Rust\n\n- Calls resolve.\n', OLD);
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n- Go and Rust calls resolve.\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork, upstream, base }).text)).toBe('\n### Fixes\n\n#### Go\n\n- Calls resolve.\n');
  });

  it('ends an entry at an unindented line that opens its own Markdown block', () => {
    const fork = log('\n### Fixes\n\n- Shared.\n> Upgrade note.\n', OLD);
    const upstream = log('', '## [1.1.0] - 2026-02-01\n\n- Shared.\n> Upgrade note.\n', OLD);
    expect(unreleasedOf(reconcileChangelog({ fork, upstream }).text)).toBe('\n### Fixes\n\n> Upgrade note.\n');
  });

  it("adds upstream's entry after the text a heading opens with, and only once", () => {
    const upstream = log('\n### Fixes\n\n- New.\n', OLD);
    const fork = log('\n### Fixes\nUpgrade note.\n\n#### Go\n\n- Fork.\n', OLD);
    const once = reconcileChangelog({ fork, upstream }).text;
    expect(unreleasedOf(once)).toBe('\n### Fixes\nUpgrade note.\n\n- New.\n\n#### Go\n\n- Fork.\n');
    expect(reconcileChangelog({ fork: once, upstream }).text).toBe(once);
    const headingless = reconcileChangelog({ fork: log('\nUpgrade note.\n\n### Fixes\n\n- Fork.\n', OLD), upstream: log('\n- New.\n', OLD) });
    expect(unreleasedOf(headingless.text)).toBe('\nUpgrade note.\n\n- New.\n\n### Fixes\n\n- Fork.\n');
  });

  it('rebuilds the file a union merge of an upstream release got wrong', () => {
    const repo = scratch();
    const run = (...args: string[]) => {
      const r = spawnSync('git', args, { cwd: repo, encoding: 'utf8' });
      if (r.status !== 0) throw new Error(`git ${args.join(' ')}: ${r.stderr}`);
      return r.stdout;
    };
    // A trailing blank line shows the release sections are copied byte for byte.
    const commit = (text: string, message: string) => {
      writeFileSync(join(repo, 'CHANGELOG.md'), `${text}\n`);
      run('add', '.');
      run('-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-qm', message);
    };
    run('init', '-q', '-b', 'upstream');
    writeFileSync(join(repo, '.gitattributes'), 'CHANGELOG.md merge=union\n');
    commit(log('\n### Fixes\n\n- Upstream fix.\n', OLD), 'base');
    run('checkout', '-qb', 'fork');
    commit(log('\n### Fixes\n\n- Upstream fix.\n- Fork fix.\n', OLD), 'fork');
    run('checkout', '-q', 'upstream');
    commit(log('', '## [1.1.0] - 2026-02-01\n\n### Fixes\n\n- Upstream fix, as released.\n', OLD), 'release');
    run('checkout', '-q', 'fork');
    run('-c', 'user.name=t', '-c', 'user.email=t@t', 'merge', '-q', '--no-edit', 'upstream');

    const script = resolve('scripts/changelog-reconcile.mjs');
    const check = spawnSync(process.execPath, [script, '--check'], { cwd: repo, encoding: 'utf8' });
    expect(check.status).toBe(1);
    const write = spawnSync(process.execPath, [script], { cwd: repo, encoding: 'utf8' });
    expect(write.status, write.stderr).toBe(0);
    expect(readFileSync(join(repo, 'CHANGELOG.md'), 'utf8')).toBe(
      `${log('\n### Fixes\n\n- Fork fix.\n', '## [1.1.0] - 2026-02-01\n\n### Fixes\n\n- Upstream fix, as released.\n', OLD)}\n`,
    );
    expect(spawnSync(process.execPath, [script, '--check'], { cwd: repo }).status).toBe(0);
    // The fork's entry has no released look-alike, so --strict passes too.
    expect(spawnSync(process.execPath, [script, '--check', '--strict'], { cwd: repo }).status).toBe(0);
    const half = spawnSync(process.execPath, [script, '--fork', 'HEAD'], { cwd: repo, encoding: 'utf8' });
    expect(half.status).toBe(2);
    expect(half.stderr).toContain('pass both --fork and --upstream');
  });
});
