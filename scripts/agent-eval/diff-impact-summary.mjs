#!/usr/bin/env node
// Per-arm summary of a diff-impact run: mean recall per arm, and per case.
//
// Usage: diff-impact-summary.mjs <scores.jsonl>
//
// Recall is the share of the real commit's other changed files (source and
// tests) the answer names. Cost, tool-call and Read counts come from each run's
// own parse-run.mjs / ARM COMPARISON block in run-all.log; they are not repeated
// here. A mean over a handful of cases with one to three expected files each is
// coarse: report the per-case table, not just the mean.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export function summarize(rows) {
  const mean = (xs) => (xs.length === 0 ? null : xs.reduce((a, b) => a + b, 0) / xs.length);
  const by = (key) => {
    const map = new Map();
    for (const r of rows) {
      const k = key(r);
      if (!map.has(k)) map.set(k, []);
      map.get(k).push(r);
    }
    return map;
  };
  const arms = [...by((r) => r.arm)].map(([arm, rs]) => ({
    arm,
    runs: rs.length,
    recall: mean(rs.map((r) => r.recall).filter((x) => x !== null)),
    precisionFloor: mean(rs.map((r) => r.precisionFloor).filter((x) => x !== null)),
  }));
  const cases = [...by((r) => r.id)].map(([id, rs]) => ({
    id,
    byArm: Object.fromEntries(
      [...new Set(rs.map((r) => r.arm))].map((arm) => [
        arm,
        mean(rs.filter((r) => r.arm === arm && r.recall !== null).map((r) => r.recall)),
      ]),
    ),
  }));
  return { arms, cases };
}

if (import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const path = process.argv[2];
  if (!path) {
    console.error('usage: diff-impact-summary.mjs <scores.jsonl>');
    process.exit(2);
  }
  const rows = readFileSync(path, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));
  const { arms, cases } = summarize(rows);
  const pct = (x) => (x === null ? '  n/a' : `${(x * 100).toFixed(0).padStart(4)}%`);
  console.log('\nARM            runs  recall  precision-floor');
  for (const a of arms) console.log(`${a.arm.padEnd(15)}${String(a.runs).padStart(4)}  ${pct(a.recall)}   ${pct(a.precisionFloor)}`);
  console.log('\nCASE                    ' + arms.map((a) => a.arm.padEnd(18)).join(''));
  for (const c of cases) console.log(c.id.padEnd(24) + arms.map((a) => pct(c.byArm[a.arm] ?? null).padEnd(18)).join(''));
}
