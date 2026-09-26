/**
 * Name-only resolution is labelled where an agent reads the graph.
 *
 * A call the resolver could bind only by the callee's name (several same-named
 * definitions, no import, no typed receiver) is right about half the time on
 * graded real repositories, against over 90% for every proven strategy
 * (docs/benchmarks/precision-replay-2026-09.md). codegraph_node's trail and
 * explore's flow mark those hops so the agent checks that one hop instead of
 * re-reading the flow; proven hops stay unmarked.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';
import { isNameGuess } from '../src/graph/edge-trust';
import type { Edge } from '../src/types';

const edge = (metadata: Record<string, unknown>, provenance?: Edge['provenance']): Edge =>
  ({ source: 'a', target: 'b', kind: 'calls', metadata, provenance }) as Edge;

describe('isNameGuess', () => {
  it('flags name matches below their proven confidence', () => {
    expect(isNameGuess(edge({ resolvedBy: 'exact-match', confidence: 0.7 }))).toBe(true);
    expect(isNameGuess(edge({ resolvedBy: 'exact-match', confidence: 0.4 }))).toBe(true);
    expect(isNameGuess(edge({ resolvedBy: 'fuzzy', confidence: 0.5 }))).toBe(true);
    expect(isNameGuess(edge({ resolvedBy: 'instance-method', confidence: 0.65 }))).toBe(true);
  });

  it('leaves proven strategies and synthesized edges unflagged', () => {
    expect(isNameGuess(edge({ resolvedBy: 'exact-match', confidence: 0.9 }))).toBe(false);
    expect(isNameGuess(edge({ resolvedBy: 'instance-method', confidence: 0.9 }))).toBe(false);
    expect(isNameGuess(edge({ resolvedBy: 'import', confidence: 0.9 }))).toBe(false);
    expect(isNameGuess(edge({ resolvedBy: 'qualified-name', confidence: 0.85 }))).toBe(false);
    expect(isNameGuess(edge({ resolvedBy: 'exact-match', confidence: 0.4 }, 'heuristic'))).toBe(false);
    expect(isNameGuess(edge({}))).toBe(false);
    expect(isNameGuess(null)).toBe(false);
  });
});

describe('codegraph_node trail labels name-only hops', () => {
  let dir: string;
  let cg: CodeGraph;

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-edge-trust-'));
    const write = (rel: string, body: string) => {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), body);
    };
    // Two unrelated `helper` definitions and no import: only a name can bind it.
    write('pkg_a/tools.py', 'def helper():\n    return 1\n');
    write('pkg_b/other.py', 'def helper():\n    return 2\n');
    // An imported callee: the import proves the target.
    write('pkg_c/util.py', 'def proven():\n    return 3\n');
    write('app/main.py', 'from pkg_c.util import proven\n\n\ndef run():\n    helper()\n    proven()\n');
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.py'], exclude: [] } });
    await cg.indexAll();
  });

  afterEach(() => {
    try { cg.close(); } catch { /* ignore */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('marks the name-matched callee and not the imported one', async () => {
    const result = await new ToolHandler(cg).execute('codegraph_node', { symbol: 'run', includeCode: false });
    const text = result.content?.[0]?.type === 'text' ? result.content[0].text : '';
    const calls = text.split('\n').find((l) => l.startsWith('**Calls →**')) ?? '';
    expect(calls).toMatch(/helper \([^)]*\) \[name match, unverified\]/);
    expect(calls).toMatch(/proven \([^)]*\)(?! \[name match)/);
  });
});
