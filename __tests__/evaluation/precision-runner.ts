/**
 * Precision gate on a pinned real repository
 * (docs/design/resolution-binding-model-plan.md, Phase 0).
 *
 *   npm run eval:precision -- vite            # fetch + index + score
 *   EVAL_REPOS=/path/to/dir npm run eval:precision -- vite
 *
 * Fetches PRECISION_CORPORA[<corpus>] at its pinned commit (shallow, into
 * $EVAL_REPOS or $TMPDIR/codegraph-precision/<corpus>), indexes it with the
 * library, scores every edge case for the corpus, prints the resolved-edge
 * histogram by resolver (the LOST/GAINED methodology the PRs used), and
 * writes a JSON report next to the recall reports. Exit 1 when any `absent`
 * case is violated or any `present` control is missing.
 */
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { DatabaseSync } from 'node:sqlite';
import { CodeGraph } from '../../src/index.js';
import { scoreEdgeCase } from './scoring.js';
import { edgeCases, PRECISION_CORPORA } from './edge-cases.js';
import type { EdgeCaseResult, PrecisionReport } from './types.js';

const corpusKey = process.argv[2];
const corpus = corpusKey ? PRECISION_CORPORA[corpusKey] : undefined;
if (!corpus) {
  console.error(`usage: npx tsx __tests__/evaluation/precision-runner.ts <${Object.keys(PRECISION_CORPORA).join('|')}>`);
  process.exit(2);
}

const reposDir = process.env.EVAL_REPOS ?? path.join(os.tmpdir(), 'codegraph-precision');
const repoDir = path.join(reposDir, corpus.key);

function sh(cmd: string, args: string[], cwd?: string, quiet = false): string {
  return execFileSync(cmd, args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', quiet ? 'ignore' : 'inherit'] }).trim();
}

function fetchPinned(): void {
  if (!fs.existsSync(path.join(repoDir, '.git'))) {
    fs.mkdirSync(repoDir, { recursive: true });
    sh('git', ['init', '-q'], repoDir);
    sh('git', ['remote', 'add', 'origin', corpus!.repo], repoDir);
  }
  const head = (() => { try { return sh('git', ['rev-parse', 'HEAD'], repoDir, true); } catch { return ''; } })();
  if (!head.startsWith(corpus!.commit)) {
    console.log(`fetching ${corpus!.repo} @ ${corpus!.commit}`);
    sh('git', ['fetch', '-q', '--depth', '1', 'origin', corpus!.commit], repoDir);
    sh('git', ['checkout', '-q', '--force', 'FETCH_HEAD'], repoDir);
  }
}

function resolvedByHistogram(): { resolvedBy: Record<string, number>; edges: number } {
  const db = new DatabaseSync(path.join(repoDir, '.codegraph', 'codegraph.db'), { readOnly: true });
  try {
    const rows = db
      .prepare(`SELECT COALESCE(json_extract(metadata, '$.resolvedBy'), '(none)') AS r, COUNT(*) AS n FROM edges GROUP BY r ORDER BY n DESC`)
      .all() as Array<{ r: string; n: number }>;
    const resolvedBy: Record<string, number> = {};
    let edges = 0;
    for (const { r, n } of rows) { resolvedBy[r] = n; edges += n; }
    return { resolvedBy, edges };
  } finally {
    db.close();
  }
}

async function run(): Promise<void> {
  fetchPinned();
  const commit = sh('git', ['rev-parse', '--short', 'HEAD'], repoDir);
  let codegraphSha = 'unknown';
  try { codegraphSha = sh('git', ['rev-parse', '--short', 'HEAD']); } catch {}

  console.log(`\nCodeGraph precision — ${corpus!.key} @ ${commit} (codegraph ${codegraphSha})`);
  console.log(`${corpus!.note}\n`);

  fs.rmSync(path.join(repoDir, '.codegraph'), { recursive: true, force: true });
  const cg = CodeGraph.initSync(repoDir);
  const t0 = performance.now();
  await cg.indexAll();
  console.log(`indexed in ${((performance.now() - t0) / 1000).toFixed(1)} s`);

  const results: EdgeCaseResult[] = [];
  for (const c of edgeCases.filter((c) => c.corpus === corpus!.key)) {
    const r = scoreEdgeCase(c, cg);
    results.push(r);
    const status = r.pass ? '\x1b[32mHELD\x1b[0m' : '\x1b[31mVIOLATED\x1b[0m';
    console.log(`  ${c.id.padEnd(44)} ${c.expect.padEnd(7)} ${status}  (${c.source})`);
    for (const f of r.found) {
      const from = cg.getNode(f.from); const to = cg.getNode(f.to);
      console.log(`      ${from?.filePath}:${from?.name} → ${to?.filePath}:${to?.name} [${f.resolvedBy ?? '?'}]`);
    }
    if (r.missingEndpoints.length) console.log(`      endpoints not in graph: ${r.missingEndpoints.join('; ')}`);
  }
  cg.close();

  const { resolvedBy, edges } = resolvedByHistogram();
  console.log(`\nedges: ${edges}`);
  for (const [k, v] of Object.entries(resolvedBy)) console.log(`  ${k.padEnd(18)} ${v}`);

  const absent = results.filter((r) => r.expect === 'absent');
  const present = results.filter((r) => r.expect === 'present');
  const report: PrecisionReport = {
    timestamp: new Date().toISOString(),
    corpus: corpus!.key,
    repo: corpus!.repo,
    commit,
    codegraphSha,
    summary: {
      absentCases: absent.length,
      absentHeld: absent.filter((r) => r.pass).length,
      presentCases: present.length,
      presentHeld: present.filter((r) => r.pass).length,
      unjudgeable: results.filter((r) => r.missingEndpoints.length > 0 && r.expect === 'absent' && !r.pass).length,
    },
    resolvedBy,
    edges,
    results,
  };
  const resultsDir = path.join(__dirname, 'results');
  fs.mkdirSync(resultsDir, { recursive: true });
  const file = path.join(resultsDir, `precision-${corpus!.key}-${commit}-${codegraphSha}.json`);
  fs.writeFileSync(file, JSON.stringify(report, null, 2));
  const failed = results.filter((r) => !r.pass).length;
  console.log(`\nSUMMARY: absent ${report.summary.absentHeld}/${absent.length} held, present ${report.summary.presentHeld}/${present.length} held`);
  console.log(`Report saved: ${file}`);
  process.exit(failed > 0 ? 1 : 0);
}

run().catch((err) => { console.error(err); process.exit(1); });
