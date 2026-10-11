#!/usr/bin/env node
// Score one diff-impact answer against its fixture's truth.json.
//
// Usage: diff-impact-score.mjs <run.jsonl> <truth.json> <repo-dir>
//
// The answer is the last successful `result` event of the stream-json run. A
// repo file counts as named when its full path appears in the answer, or when
// its base name appears as a whole word and no other file in the repo shares
// that base name. The primary file is the edit itself and is never counted.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';

/** True when `name` appears in `text` as its own token, not inside a longer name. */
function mentions(text, name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`(^|[^\\w./-])${escaped}(?![\\w-])(?!\\.\\w)`).test(text);
}

/** Repo files the answer names. `files` are repo-relative paths. */
export function namedFiles(answer, files, primary) {
  const byBase = new Map();
  for (const f of files) byBase.set(basename(f), (byBase.get(basename(f)) ?? 0) + 1);
  return files
    .filter((f) => f !== primary && (mentions(answer, f) || (byBase.get(basename(f)) === 1 && mentions(answer, basename(f)))))
    .sort();
}

export function scoreAnswer(answer, truth, files) {
  const named = namedFiles(answer, files, truth.primary);
  const hit = (list) => list.filter((f) => named.includes(f));
  const hits = hit([...truth.source, ...truth.tests]);
  const ratio = (n, d) => (d === 0 ? null : n / d);
  return {
    id: truth.id,
    namedFiles: named.length,
    sourceRecall: ratio(hit(truth.source).length, truth.source.length),
    testRecall: ratio(hit(truth.tests).length, truth.tests.length),
    recall: ratio(hits.length, truth.source.length + truth.tests.length),
    // A floor: a named file the real commit did not touch may still depend on the edit.
    precisionFloor: ratio(hits.length, named.length),
    missed: [...truth.source, ...truth.tests].filter((f) => !named.includes(f)),
  };
}

export function finalAnswer(jsonl) {
  let answer = null;
  for (const line of jsonl.split('\n')) {
    if (!line) continue;
    let event;
    try { event = JSON.parse(line); } catch { continue; }
    if (event.type === 'result' && event.subtype === 'success' && typeof event.result === 'string') answer = event.result;
  }
  return answer;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [runPath, truthPath, repoDir] = process.argv.slice(2);
  if (!runPath || !truthPath || !repoDir) {
    console.error('usage: diff-impact-score.mjs <run.jsonl> <truth.json> <repo-dir>');
    process.exit(2);
  }
  const answer = finalAnswer(readFileSync(runPath, 'utf8'));
  if (answer === null) {
    console.error(`no successful result in ${runPath}`);
    process.exit(1);
  }
  const truth = JSON.parse(readFileSync(truthPath, 'utf8'));
  const files = execFileSync('git', ['ls-files'], { cwd: repoDir, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
    .split('\n').filter(Boolean);
  console.log(JSON.stringify(scoreAnswer(answer, truth, files)));
}
