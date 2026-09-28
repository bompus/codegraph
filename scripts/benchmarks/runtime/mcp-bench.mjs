#!/usr/bin/env node
// MCP server bench on one corpus with a prebuilt index; prints one JSON line.
// Usage: node mcp-bench.mjs <arm> <runtime> <build> <repo> <cgdir-name> <edit-file> <queries.json>
// Measures: start to first explore answer, warm explore latency, 8 explores at
// once, watcher sync of one edited file, then idle memory and CPU.
import { spawn, execFileSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const [, , arm, runtime, build, repoRaw, cgName, EDIT, queriesFile] = process.argv;
if (!queriesFile) throw new Error('usage: mcp-bench.mjs <arm> <runtime> <build> <repo> <cgdir-name> <edit-file> <queries.json>');
const repo = path.resolve(repoRaw);
const CLI = path.join(build, 'dist/bin/codegraph.js');
const require = createRequire(import.meta.url);
const { stopDaemonAt } = require(path.join(build, 'dist/mcp/daemon-registry.js'));
const cgDir = path.join(repo, cgName);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const QUERIES = JSON.parse(fs.readFileSync(queriesFile, 'utf8'));

const env = { ...process.env, CODEGRAPH_DIR: cgName, CODEGRAPH_TELEMETRY: '0', DO_NOT_TRACK: '1', CODEGRAPH_MCP_LOG_ATTACH: '0', CODEGRAPH_ALLOW_UNSAFE_NODE: '1' };
delete env.CODEGRAPH_NO_DAEMON;
const child = spawn(runtime, [CLI, 'serve', '--mcp', '--path', repo], { env, stdio: ['pipe', 'pipe', 'inherit'] });
let buf = '';
const waiters = new Map();
child.stdout.setEncoding('utf8');
child.stdout.on('data', (c) => {
  buf += c;
  let i;
  while ((i = buf.indexOf('\n')) !== -1) {
    const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
    if (!line) continue;
    let m; try { m = JSON.parse(line); } catch { continue; }
    if (m.id !== undefined && waiters.has(m.id)) { waiters.get(m.id)(m); waiters.delete(m.id); }
  }
});
let rid = 0;
const request = (method, params) => new Promise((res) => {
  const id = ++rid;
  waiters.set(id, res);
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
});
const explore = async (q) => {
  const t = performance.now();
  const r = await request('tools/call', { name: 'codegraph_explore', arguments: { query: q } });
  const ms = performance.now() - t;
  const text = r.result?.content?.[0]?.text ?? '';
  if (r.error || r.result?.isError || text.length < 200) throw new Error(`explore failed: ${JSON.stringify(r).slice(0, 300)}`);
  return { ms, chars: text.length };
};

// Process tree helpers: the stdio server (child and its descendants) plus the detached daemon tree.
const stat = (pid) => {
  const s = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
  const f = s.slice(s.lastIndexOf(')') + 2).split(' ');
  return { ppid: Number(f[1]), ticks: Number(f[11]) + Number(f[12]) };
};
const tree = (roots) => {
  const kids = new Map();
  for (const d of fs.readdirSync('/proc')) {
    if (!/^\d+$/.test(d)) continue;
    try { const { ppid } = stat(d); if (!kids.has(ppid)) kids.set(ppid, []); kids.get(ppid).push(Number(d)); } catch {}
  }
  const out = new Set(); const q = [...roots];
  while (q.length) { const p = q.pop(); if (out.has(p)) continue; out.add(p); q.push(...(kids.get(p) ?? [])); }
  return [...out];
};
const rssMB = (pid) => {
  try { return Number(/^VmRSS:\s+(\d+)/m.exec(fs.readFileSync(`/proc/${pid}/status`, 'utf8'))?.[1] ?? 0) / 1024; } catch { return 0; }
};
const ticks = (pids) => pids.reduce((a, p) => { try { return a + stat(p).ticks; } catch { return a; } }, 0);
const daemonPid = () => JSON.parse(fs.readFileSync(path.join(cgDir, 'daemon.pid'), 'utf8')).pid;
const snapshot = (pid) => {
  const all = tree([child.pid, pid]);
  const d = tree([pid]);
  return { daemonMB: Math.round(d.reduce((a, p) => a + rssMB(p), 0)), totalMB: Math.round(all.reduce((a, p) => a + rssMB(p), 0)), procs: all.length };
};
const indexedAt = () => {
  const { DatabaseSync } = require('node:sqlite');
  const db = new DatabaseSync(path.join(cgDir, 'codegraph.db'), { readOnly: true });
  try { return db.prepare('SELECT indexed_at, content_hash FROM files WHERE path = ?').get(EDIT); } finally { db.close(); }
};
const waitReindexed = async (before, t0) => {
  for (;;) {
    const now = indexedAt();
    if (now && now.content_hash !== before.content_hash) return performance.now() - t0;
    if (performance.now() - t0 > 60000) return NaN;
    await sleep(25);
  }
};

const out = { arm, corpus: path.basename(repo) };
const t0 = performance.now();
await request('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'bench', version: '1' } });
child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
out.initMs = Math.round(performance.now() - t0);
const first = await explore(QUERIES[0]);
out.firstExploreMs = Math.round(first.ms);
out.coldToFirstAnswerMs = Math.round(performance.now() - t0);
const pid = daemonPid();

for (const q of QUERIES) await explore(q);
const lat = [];
for (const q of QUERIES) lat.push((await explore(q)).ms);
lat.sort((a, b) => a - b);
out.warmMedianMs = Math.round(lat[Math.floor(lat.length / 2)]);
out.warmP90Ms = Math.round(lat[Math.floor(lat.length * 0.9)]);

const tw = performance.now();
await Promise.all(QUERIES.slice(0, 8).map((q) => explore(q)));
out.wave8Ms = Math.round(performance.now() - tw);
await sleep(2000);
Object.assign(out, Object.fromEntries(Object.entries(snapshot(pid)).map(([k, v]) => [`busy_${k}`, v])));

const file = path.join(repo, EDIT);
const orig = fs.readFileSync(file, 'utf8');
const before = indexedAt();
const ts = performance.now();
fs.writeFileSync(file, orig + `\nexport function benchProbe${Date.now()}() { return 1; }\n`);
out.watchSyncMs = Math.round(await waitReindexed(before, ts));
const mid = indexedAt();
fs.writeFileSync(file, orig);
await waitReindexed(mid, performance.now());

// Idle: wait past the fork's 60 s worker-retire window, then sample RSS and CPU over 60 s.
await sleep(70000);
Object.assign(out, Object.fromEntries(Object.entries(snapshot(pid)).map(([k, v]) => [`idle_${k}`, v])));
const pids = tree([child.pid, pid]);
const c0 = ticks(pids); const i0 = performance.now();
await sleep(60000);
const c1 = ticks(tree([child.pid, pid]));
out.idleCpuPct = Number((((c1 - c0) * 10) / (performance.now() - i0) * 100).toFixed(3));

child.stdin.end();
child.kill();
await stopDaemonAt(repo);
execFileSync('git', ['-C', repo, 'diff', '--quiet', '--', EDIT]);
console.log(JSON.stringify(out));
