#!/usr/bin/env node
// Adapts the flat BENCH_OUT layout (index-sync.tsv, mcp.jsonl, cli-*.json with
// arm names upstream-node/fork-node/fork-bun/upstream-bun) into the attempt-dir
// layout render-charts.mjs consumes: <dir>/index-<c>-a1/{status,index-sync.tsv},
// mcp-<c>-a1/{status,mcp.jsonl}, cli-gin-a1/{status,cli-*.json}.
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const [src, dst] = process.argv.slice(2);
const ARM = { 'upstream-node': 'upnode', 'fork-node': 'forknode', 'fork-bun': 'forkbun', 'upstream-bun': 'upbun' };
const CORPORA = [['gin', 'gin'], ['Alamofire', 'alamofire'], ['pretix', 'pretix'], ['cpython', 'cpython'], ['discourse', 'discourse'], ['supabase', 'supabase'], ['n8n', 'n8n']];

const rows = readFileSync(join(src, 'index-sync.tsv'), 'utf8').trim().split('\n')
  .filter((l) => l.split('\t')[3] === '0') // drop rows from killed/failed attempts
  .map((l) => {
    const c = l.split('\t'); c[1] = ARM[c[1]] ?? c[1]; return c.join('\t');
  });
for (const [dirName, rowName] of CORPORA) {
  const d = join(dst, `index-${dirName}-a1`); mkdirSync(d, { recursive: true });
  writeFileSync(join(d, 'status'), 'accepted\n');
  writeFileSync(join(d, 'index-sync.tsv'), rows.filter((l) => l.startsWith(rowName + '\t')).join('\n') + '\n');
}
{
  const p = join(src, 'mcp.jsonl');
  if (existsSync(p)) {
    const lines = readFileSync(p, 'utf8').trim().split('\n').map((l) => { const r = JSON.parse(l); r.arm = ARM[r.arm] ?? r.arm; return r; });
    for (const c of ['gin', 'n8n']) {
      const rows = lines.filter((r) => r.corpus === c);
      if (!rows.length) continue;
      const d = join(dst, `mcp-${c}-a1`); mkdirSync(d, { recursive: true });
      writeFileSync(join(d, 'status'), 'accepted\n');
      writeFileSync(join(d, 'mcp.jsonl'), rows.map((r) => JSON.stringify(r)).join('\n') + '\n');
    }
  }
}
{
  const d = join(dst, 'cli-gin-a1'); mkdirSync(d, { recursive: true });
  writeFileSync(join(d, 'status'), 'accepted\n');
  for (const n of ['version', 'status', 'explore']) {
    const p = join(src, `cli-${n}.json`);
    if (!existsSync(p)) { console.error(`missing ${p}`); continue; }
    const j = JSON.parse(readFileSync(p, 'utf8'));
    for (const r of j.results) r.name = ARM[r.name] ?? r.name;
    writeFileSync(join(d, `cli-${n}.json`), JSON.stringify(j));
  }
}
console.log('adapted →', dst);
