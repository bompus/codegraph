#!/usr/bin/env node
// Replay saved agent queries against two builds and diff the answers.
//
//   node scripts/agent-eval/replay-queries.mjs <repo> --tasks <corpus.json> --base <build> --cand <build> --out <dir> [--ids globs]
//
// <repo> is an indexed checkout at the corpus's pinned commit (the corpus file
// names it). Each build answers every query through probe-explore.mjs in its
// own process. A query regresses when the candidate's answer loses its Source
// Code section, drops a source file the base rendered (worst when the query
// names that file), or misses a mustMatch pattern the base found. Prints a Markdown table of changed queries and a
// summary line; exits 1 on any regression, 0 otherwise. Answers are saved
// under <out>/base and <out>/cand for reading the diff; keep <out> off /tmp.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

/** How one query's answer moved from base to candidate. */
export function compareRow(base, cand, prompt = '', sameText = base.chars === cand.chars) {
  const lostFiles = base.files.filter((f) => !cand.files.includes(f));
  const lostNamed = lostFiles.filter((f) => prompt.includes(f));
  const gainedFiles = cand.files.filter((f) => !base.files.includes(f));
  const lostSource = base.sourceAt >= 0 && cand.sourceAt < 0;
  const lostMatch = base.mustMatchAt.filter((at, i) => at >= 0 && (cand.mustMatchAt[i] ?? -1) < 0).length;
  const regressed = lostSource || lostFiles.length > 0 || lostMatch > 0;
  const changed = regressed || gainedFiles.length > 0 || !sameText;
  return { id: base.id, regressed, changed, lostSource, lostFiles, lostNamed, gainedFiles, lostMatch, baseChars: base.chars, candChars: cand.chars };
}

function probe(build, repo, tasks, ids, out) {
  const script = fileURLToPath(new URL('./probe-explore.mjs', import.meta.url));
  const args = [script, repo, '--tasks', tasks, '--build', build, '--out', out];
  if (ids) args.push('--ids', ids);
  const res = spawnSync(process.execPath, args, { encoding: 'utf8', maxBuffer: 64 << 20, stdio: ['ignore', 'pipe', 'inherit'] });
  if (res.status !== 0) throw new Error(`probe-explore with ${build} exited ${res.status}`);
  const rows = new Map();
  for (const line of res.stdout.split('\n')) {
    if (!line.startsWith('{')) continue;
    const row = JSON.parse(line);
    if (row.id !== 'memory') rows.set(row.id, row);
  }
  return rows;
}

function main() {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: { tasks: { type: 'string' }, base: { type: 'string' }, cand: { type: 'string' }, out: { type: 'string' }, ids: { type: 'string' } },
  });
  const [repoArg] = positionals;
  if (!repoArg || !values.tasks || !values.base || !values.cand || !values.out) {
    console.error('usage: replay-queries.mjs <repo> --tasks <corpus.json> --base <build> --cand <build> --out <dir> [--ids globs]');
    return 2;
  }
  const repo = resolve(repoArg);
  const out = resolve(values.out);
  const corpus = JSON.parse(readFileSync(values.tasks, 'utf8'));
  if (corpus.commit) {
    const head = spawnSync('git', ['-C', repo, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).stdout.trim();
    if (!head.startsWith(corpus.commit)) {
      console.error(`${repo} is at ${head.slice(0, 12)}, the corpus is pinned to ${corpus.commit}; answers would not be comparable`);
      return 2;
    }
  }
  const base = probe(resolve(values.base), repo, values.tasks, values.ids, join(out, 'base'));
  const cand = probe(resolve(values.cand), repo, values.tasks, values.ids, join(out, 'cand'));
  if (base.size === 0) {
    console.error('no queries selected; check --ids and the corpus');
    return 2;
  }
  const prompts = new Map(corpus.tasks.map((t) => [t.id, t.prompt]));
  const answer = (side, id) => readFileSync(join(out, side, `${id}.md`), 'utf8');
  const rows = [...base.values()].map((b) => {
    const c = cand.get(b.id);
    if (!c) throw new Error(`candidate has no answer for ${b.id}`);
    return compareRow(b, c, prompts.get(b.id), answer('base', b.id) === answer('cand', b.id));
  });
  const changed = rows.filter((r) => r.changed);
  const regressed = rows.filter((r) => r.regressed);
  if (changed.length) {
    console.log('| Query | Result | Chars (base → cand) | Files lost | Files gained |');
    console.log('|---|---|---:|---|---|');
    for (const r of changed) {
      const result = !r.regressed ? 'changed'
        : r.lostSource ? 'REGRESSED: no source'
        : r.lostNamed.length ? 'REGRESSED: named file lost'
        : r.lostMatch ? 'REGRESSED: pattern missing'
        : 'REGRESSED: file lost';
      console.log(`| ${r.id} | ${result} | ${r.baseChars} → ${r.candChars} | ${r.lostFiles.join(', ') || '-'} | ${r.gainedFiles.join(', ') || '-'} |`);
    }
    console.log('');
  }
  console.log(`${rows.length} queries: ${rows.length - changed.length} identical, ${changed.length - regressed.length} changed without loss, ${regressed.length} regressed. Answers: ${out}`);
  return regressed.length ? 1 : 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exit(main());
  } catch (error) {
    console.error(`[replay-queries] ${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  }
}
