/**
 * Known-answer search baseline on a pinned real repository.
 *
 *   EVAL_REPOS=/path/to/dir npm run eval:search -- svelte
 *
 * Fetches PRECISION_CORPORA[<corpus>] at its pinned commit into
 * $EVAL_REPOS/<corpus>, indexes it with this checkout's library, runs every
 * case in search-cases.ts, prints per-category pass counts, MRR and latency,
 * and writes a JSON report to results/. Exit 1 when a gating case fails.
 * EVAL_REPOS is required: corpora and their indexes must stay off tmpfs.
 */
import * as fs from 'fs';
import * as path from 'path';
import { CodeGraph } from '../../src/index.js';
import { parseQuery } from '../../src/search/query-parser.js';
import type { Node, SearchResult } from '../../src/types.js';
import { PRECISION_CORPORA } from './edge-cases.js';
import { fetchPinned, scrubGitEnv, sh } from './pinned-corpus.js';
import { searchCases, type ExpectedHit, type SearchCase } from './search-cases.js';

const DEFAULT_LIMIT = 10;
const TIMED_RUNS = 5;

const corpusKey = process.argv[2];
const corpus = corpusKey ? PRECISION_CORPORA[corpusKey] : undefined;
const cases = searchCases.filter((c) => c.corpus === corpusKey);
if (!corpus || cases.length === 0) {
  const keys = [...new Set(searchCases.map((c) => c.corpus))];
  console.error(`usage: EVAL_REPOS=<dir> npx tsx __tests__/evaluation/search-baseline-runner.ts <${keys.join('|')}>`);
  process.exit(2);
}
if (!process.env.EVAL_REPOS) {
  console.error('EVAL_REPOS must name a disk-backed directory for the corpus clone and its index');
  process.exit(2);
}

scrubGitEnv();
const repoDir = path.join(process.env.EVAL_REPOS, corpus.key);

interface CaseResult {
  id: string;
  category: string;
  query: string;
  knownMiss?: string;
  pass: boolean;
  /** 1-based rank of each expected hit in the results; 0 when absent. */
  ranks: number[];
  mrr: number;
  filterViolations: string[];
  /** Scoped cases: best rank of an expected hit for the unfiltered text; 0 when beyond the limit. */
  unfilteredRank?: number;
  medianMs: number;
  problems: string[];
}

function matches(node: Node, e: ExpectedHit): boolean {
  return node.name === e.name && node.filePath.includes(e.file) && (!e.kind || node.kind === e.kind);
}

function rankOf(results: SearchResult[], e: ExpectedHit): number {
  return results.findIndex((r) => matches(r.node, e)) + 1;
}

/** Rows that break an explicit filter, checked the way the parser documents each one. */
function filterViolations(query: string, results: SearchResult[]): string[] {
  const q = parseQuery(query);
  const bad: string[] = [];
  for (const { node } of results) {
    const file = node.filePath.toLowerCase();
    const name = node.name.toLowerCase();
    if (q.kinds.length && !q.kinds.includes(node.kind)) bad.push(`${node.name}: kind ${node.kind}`);
    else if (q.languages.length && !q.languages.includes(node.language)) bad.push(`${node.name}: lang ${node.language}`);
    else if (q.pathFilters.length && !q.pathFilters.some((p) => file.includes(p.toLowerCase()))) bad.push(`${node.name}: path ${node.filePath}`);
    else if (q.nameFilters.length && !q.nameFilters.some((n) => name.includes(n.toLowerCase()))) bad.push(`${node.name}: name`);
  }
  return bad;
}

function timed(cg: CodeGraph, query: string, limit: number): { results: SearchResult[]; medianMs: number } {
  let results = cg.searchNodes(query, { limit });
  const times: number[] = [];
  for (let i = 0; i < TIMED_RUNS; i++) {
    const t = performance.now();
    results = cg.searchNodes(query, { limit });
    times.push(performance.now() - t);
  }
  times.sort((a, b) => a - b);
  return { results, medianMs: times[Math.floor(times.length / 2)]! };
}

function runCase(cg: CodeGraph, c: SearchCase): CaseResult {
  const limit = c.limit ?? DEFAULT_LIMIT;
  const { results, medianMs } = timed(cg, c.query, limit);
  const problems: string[] = [];
  const ranks = c.expect.map((e) => rankOf(results, e));
  const found = ranks.filter((r) => r > 0);
  const best = found.length ? Math.min(...found) : 0;

  if (c.expect.length === 0 && results.length > 0) problems.push(`expected no rows, got ${results.length}`);
  c.expect.forEach((e, i) => { if (!ranks[i]) problems.push(`missing ${e.name} (${e.file})`); });
  if (best && best > (c.maxRank ?? limit)) problems.push(`best rank ${best} > ${c.maxRank}`);
  const violations = filterViolations(c.query, results);
  if (violations.length) problems.push(`${violations.length} rows break a filter`);

  let unfilteredRank: number | undefined;
  if (c.category === 'scoped') {
    const text = parseQuery(c.query).text;
    const plain = cg.searchNodes(text, { limit });
    const r = c.expect.map((e) => rankOf(plain, e)).filter((x) => x > 0);
    unfilteredRank = r.length ? Math.min(...r) : 0;
    if (unfilteredRank) problems.push(`mislabeled: "${text}" alone already ranks the target ${unfilteredRank}`);
  }

  return {
    id: c.id, category: c.category, query: c.query, knownMiss: c.knownMiss,
    pass: problems.length === 0, ranks, mrr: best ? 1 / best : 0,
    filterViolations: violations, unfilteredRank, medianMs, problems,
  };
}

function percentile(sorted: number[], p: number): number {
  return sorted[Math.min(sorted.length - 1, Math.ceil(p * sorted.length) - 1)]!;
}

async function run(): Promise<void> {
  fetchPinned(corpus!, repoDir);
  const commit = sh('git', ['rev-parse', '--short', 'HEAD'], repoDir);
  let codegraphSha = 'unknown';
  try { codegraphSha = sh('git', ['rev-parse', '--short', 'HEAD']); } catch {}
  console.log(`\nCodeGraph search baseline — ${corpus!.key} @ ${commit} (codegraph ${codegraphSha})\n`);

  fs.rmSync(path.join(repoDir, '.codegraph'), { recursive: true, force: true });
  const cg = CodeGraph.initSync(repoDir);
  const t0 = performance.now();
  await cg.indexAll();
  console.log(`indexed in ${((performance.now() - t0) / 1000).toFixed(1)} s\n`);

  const results = cases.map((c) => runCase(cg, c));
  cg.close();

  for (const r of results) {
    const status = r.knownMiss
      ? (r.pass ? '\x1b[33mFIXED\x1b[0m' : 'LIMIT')
      : (r.pass ? '\x1b[32mPASS\x1b[0m' : '\x1b[31mFAIL\x1b[0m');
    const scoped = r.unfilteredRank === undefined ? '' : `  unfiltered ${r.unfilteredRank || '>limit'}`;
    console.log(`  ${status.padEnd(14)} ${r.id.padEnd(34)} ranks ${JSON.stringify(r.ranks).padEnd(10)} ${r.medianMs.toFixed(2).padStart(6)} ms${scoped}`);
    for (const p of r.problems) console.log(`      ${p}`);
    if (r.knownMiss) console.log(`      known limit: ${r.knownMiss}`);
  }

  const gating = results.filter((r) => !r.knownMiss);
  const byCategory: Record<string, { cases: number; passed: number; mrr: number }> = {};
  for (const r of gating) {
    const b = (byCategory[r.category] ??= { cases: 0, passed: 0, mrr: 0 });
    b.cases++; if (r.pass) b.passed++; b.mrr += r.mrr;
  }
  console.log('\ncategory   passed   MRR');
  for (const [k, b] of Object.entries(byCategory)) {
    b.mrr = Number((b.mrr / b.cases).toFixed(3));
    console.log(`  ${k.padEnd(9)} ${`${b.passed}/${b.cases}`.padEnd(8)} ${b.mrr.toFixed(3)}`);
  }
  const ms = results.map((r) => r.medianMs).sort((a, b) => a - b);
  const latency = { p50: Number(percentile(ms, 0.5).toFixed(2)), p95: Number(percentile(ms, 0.95).toFixed(2)), max: Number(ms.at(-1)!.toFixed(2)) };
  const limits = results.filter((r) => r.knownMiss);
  console.log(`latency per query (median of ${TIMED_RUNS}): p50 ${latency.p50} ms, p95 ${latency.p95} ms, max ${latency.max} ms`);
  console.log(`known limits: ${limits.filter((r) => !r.pass).length}/${limits.length} still miss`);

  const report = {
    timestamp: new Date().toISOString(),
    corpus: corpus!.key, repo: corpus!.repo, commit, codegraphSha,
    summary: { gatingCases: gating.length, passed: gating.filter((r) => r.pass).length, byCategory, latency,
      knownLimits: limits.length, knownLimitsFixed: limits.filter((r) => r.pass).length },
    results,
  };
  const resultsDir = path.join(__dirname, 'results');
  fs.mkdirSync(resultsDir, { recursive: true });
  const file = path.join(resultsDir, `search-${corpus!.key}-${commit}-${codegraphSha}.json`);
  fs.writeFileSync(file, JSON.stringify(report, null, 2));
  console.log(`\nSUMMARY: ${report.summary.passed}/${gating.length} gating cases pass`);
  console.log(`Report saved: ${file}`);
  process.exit(gating.every((r) => r.pass) ? 0 : 1);
}

run().catch((err) => { console.error(err); process.exit(1); });
