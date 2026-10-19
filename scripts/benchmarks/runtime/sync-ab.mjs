#!/usr/bin/env node
// Before/after comparison of a one-file `codegraph sync` between two builds (or runtimes).
// See README.md, "One-file sync comparison". Needs Node 22.13 or newer (node:sqlite) and /usr/bin/time.
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const USAGE = `usage: sync-ab.mjs --corpus DIR --file REL --append TEXT --index DIR --out NEW_DIR
       --arm base=RUNTIME@BUILD --arm cand=RUNTIME@BUILD
       [--pairs 3] [--controls 2] [--sessions 2] [--band 0.03] [--metric peak|wall|cpu]
       [--noise FILE] [--save-noise FILE] [--proxies] [--no-relabel] [--allow-work-diff]
       [--max-load 4] [--min-avail-gib 8] [--env KEY=VALUE ...]`;

const { values: o } = parseArgs({
  options: {
    corpus: { type: 'string' }, file: { type: 'string' }, append: { type: 'string' },
    index: { type: 'string' }, out: { type: 'string' }, arm: { type: 'string', multiple: true },
    pairs: { type: 'string', default: '3' }, controls: { type: 'string', default: '2' },
    sessions: { type: 'string', default: '2' }, band: { type: 'string', default: '0.03' },
    metric: { type: 'string', default: 'peak' }, noise: { type: 'string' }, 'save-noise': { type: 'string' },
    proxies: { type: 'boolean', default: false }, 'no-relabel': { type: 'boolean', default: false },
    'allow-work-diff': { type: 'boolean', default: false }, 'max-load': { type: 'string', default: '4' },
    'min-avail-gib': { type: 'string', default: '8' }, env: { type: 'string', multiple: true, default: [] },
    cgdir: { type: 'string', default: '.cg-ab' },
  },
});
if (!o.corpus || !o.file || o.append === undefined || !o.index || !o.out || o.arm?.length !== 2) {
  console.error(USAGE);
  process.exit(2);
}
const METRICS = ['peak', 'wall', 'cpu'];
const UNIT = { peak: 'MiB', wall: 's', cpu: 's' };
if (!METRICS.includes(o.metric)) throw new Error(`--metric must be one of ${METRICS.join(', ')}`);
const band = Number(o.band);
const corpus = path.resolve(o.corpus);
const target = path.join(corpus, o.file);
const appendText = o.append.replace(/\\n/g, '\n');
const out = path.resolve(o.out);
// Exclusive creation keeps one run's evidence from mixing with another's.
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.mkdirSync(out);
fs.mkdirSync(path.join(out, 'logs'));
const cgIndex = path.join(corpus, o.cgdir);
const wrap = (process.env.BENCH_WRAP ?? '').split(/\s+/).filter(Boolean);
const extraEnv = Object.fromEntries(o.env.map((kv) => [kv.slice(0, kv.indexOf('=')), kv.slice(kv.indexOf('=') + 1)]));

const arms = o.arm.map((spec) => {
  const [name, rest] = [spec.slice(0, spec.indexOf('=')), spec.slice(spec.indexOf('=') + 1)];
  const runtime = rest.slice(0, rest.lastIndexOf('@'));
  const build = path.resolve(rest.slice(rest.lastIndexOf('@') + 1));
  const bin = path.join(build, 'dist/bin/codegraph.js');
  if (!name || !fs.existsSync(bin)) throw new Error(`arm ${spec}: no dist/bin/codegraph.js under ${build}`);
  const version = execFileSync(runtime, ['--version'], { encoding: 'utf8' }).trim();
  let commit = null;
  try { commit = execFileSync('git', ['-C', build, 'rev-parse', '--short=8', 'HEAD'], { encoding: 'utf8' }).trim(); } catch {}
  return { name, runtime, build, bin, version, commit, index: path.resolve(o.index), isNode: !version.startsWith('1.') };
});
const [A, B] = arms;

const sha = (buf) => createHash('sha256').update(buf).digest('hex').slice(0, 16);
function treeHash(dir) {
  const h = createHash('sha256');
  for (const f of fs.readdirSync(dir, { recursive: true }).sort()) {
    const p = path.join(dir, f);
    if (fs.statSync(p).isFile()) h.update(f).update(fs.readFileSync(p));
  }
  return h.digest('hex').slice(0, 16);
}
function withDb(dir, fn, readOnly = true) {
  const db = new DatabaseSync(path.join(dir, 'codegraph.db'), { readOnly });
  try { return fn(db); } finally { db.close(); }
}
const skipsVersion = (db) => db.prepare("SELECT value FROM project_metadata WHERE key = 'synth_skips_version'").get()?.value ?? null;
function tableHash(db, sql) {
  try { return sha(JSON.stringify(db.prepare(sql).all())); } catch { return null; }
}
function graphState(dir) {
  return withDb(dir, (db) => ({
    nodes: db.prepare('SELECT count(*) AS n FROM nodes').get().n,
    edges: db.prepare('SELECT count(*) AS n FROM edges').get().n,
    edgesHash: tableHash(db, 'SELECT source, target, kind, line, col, provenance, metadata FROM edges ORDER BY 1, 2, 3, 4, 5, 6, 7'),
    nearDupHash: tableHash(db, 'SELECT * FROM near_duplicates ORDER BY 1, 2'),
    skipsVersion: skipsVersion(db),
  }));
}

// Wait for a quiet host: no capped job running, enough memory, low load.
function admit() {
  const deadline = Date.now() + 10 * 60_000;
  for (;;) {
    const scopes = spawnSync('systemctl', ['--user', 'list-units', '--plain', '--no-legend', '--state=running', 'capped-*'], { encoding: 'utf8' }).stdout?.trim() ?? '';
    const avail = Number(/MemAvailable:\s+(\d+)/.exec(fs.readFileSync('/proc/meminfo', 'utf8'))[1]) / 2 ** 20;
    const load = Number(fs.readFileSync('/proc/loadavg', 'utf8').split(' ')[0]);
    if (!scopes && avail >= Number(o['min-avail-gib']) && load < Number(o['max-load'])) return { load1: load, availGib: Math.round(avail * 10) / 10 };
    if (Date.now() > deadline) throw new Error(`host not quiet for 10 min: ${JSON.stringify({ scopes, avail, load })}`);
    spawnSync('sleep', ['5']);
  }
}

// Timing lines (CODEGRAPH_SYNTH_TIMINGS) carry work counts in parentheses: refs resolved,
// files resynthesized, pairs re-scored. Two builds doing the same work print the same counts.
function workCounters(stderr) {
  const counters = {};
  for (const m of stderr.matchAll(/^\[(?:phase|synth)-timing\] ([^:]+): \d+ms(?: \((.*)\))?$/gm)) counters[m[1]] = m[2] ?? '';
  return counters;
}
function diffCounters(a, b) {
  return [...new Set([...Object.keys(a), ...Object.keys(b)])].filter((k) => a[k] !== b[k]).map((k) => `${k}: ${a[k] ?? '(absent)'} vs ${b[k] ?? '(absent)'}`);
}

const pristineHash = {};
function restore(arm) {
  fs.rmSync(cgIndex, { recursive: true, force: true });
  fs.cpSync(arm.index, cgIndex, { recursive: true });
  pristineHash[arm.index] ??= treeHash(arm.index);
  if (treeHash(cgIndex) !== pristineHash[arm.index]) throw new Error(`restored index differs from ${arm.index}`);
}

let seq = 0;
const runsFile = path.join(out, 'runs.jsonl');
function run(arm, label, mode = 'timed') {
  const id = `${String(++seq).padStart(3, '0')}-${label}`;
  restore(arm);
  const before = withDb(cgIndex, skipsVersion);
  const admission = admit();
  const timeFile = path.join(out, 'logs', `${id}.time`);
  const perfFile = path.join(out, 'logs', `${id}.perf`);
  const allocFile = path.join(out, 'logs', `${id}.alloc.json`);
  const pre = mode === 'alloc' ? ['--require', path.join(HERE, 'alloc-total.cjs')] : [];
  const perf = mode === 'instructions' ? ['perf', 'stat', '-x,', '-e', 'instructions:u', '-o', perfFile, '--'] : [];
  const cmd = [...wrap, '/usr/bin/time', '-v', '-o', timeFile, ...perf, arm.runtime, ...pre, arm.bin, 'sync', corpus];
  const env = { ...process.env, CODEGRAPH_DIR: o.cgdir, CODEGRAPH_NO_DAEMON: '1', CODEGRAPH_TELEMETRY: '0', DO_NOT_TRACK: '1', CODEGRAPH_ALLOW_UNSAFE_NODE: '1', CODEGRAPH_SYNTH_TIMINGS: '1', ...extraEnv, ...(mode === 'alloc' ? { ALLOC_TOTAL_OUT: allocFile } : {}) };
  const original = fs.readFileSync(target);
  let r;
  try {
    fs.writeFileSync(target, Buffer.concat([original, Buffer.from(appendText)]));
    r = spawnSync(cmd[0], cmd.slice(1), { env, encoding: 'utf8', maxBuffer: 256 << 20 });
  } finally {
    fs.writeFileSync(target, original);
  }
  fs.writeFileSync(path.join(out, 'logs', `${id}.stderr`), r.stderr ?? '');
  const t = fs.existsSync(timeFile) ? fs.readFileSync(timeFile, 'utf8') : '';
  const num = (re) => Number(re.exec(t)?.[1] ?? NaN);
  const wallParts = /Elapsed \(wall clock\) time \(h:mm:ss or m:ss\): (.*)/.exec(t)?.[1].split(':').map(Number) ?? [NaN];
  const rec = {
    id, arm: arm.name, label, mode, exit: r.status, admission,
    peak: Math.round(num(/Maximum resident set size \(kbytes\): (\d+)/) / 1024),
    wall: Math.round(wallParts.reduce((acc, v) => acc * 60 + v, 0) * 100) / 100,
    cpu: Math.round((num(/User time \(seconds\): ([\d.]+)/) + num(/System time \(seconds\): ([\d.]+)/)) * 100) / 100,
    counters: workCounters(r.stderr ?? ''), skipsBefore: before, ...graphState(cgIndex),
  };
  if (mode === 'instructions' && fs.existsSync(perfFile)) {
    rec.instructions = Number(fs.readFileSync(perfFile, 'utf8').split('\n').find((l) => l.includes('instructions'))?.split(',')[0]);
  }
  if (mode === 'alloc' && fs.existsSync(allocFile)) rec.allocMiB = JSON.parse(fs.readFileSync(allocFile, 'utf8')).allocatedMiB;
  rec.rescanned = rec.skipsBefore !== null && rec.skipsVersion !== rec.skipsBefore;
  fs.rmSync(cgIndex, { recursive: true, force: true });
  fs.appendFileSync(runsFile, JSON.stringify(rec) + '\n');
  console.log(`${id} exit=${rec.exit} peak=${rec.peak}MiB wall=${rec.wall}s cpu=${rec.cpu}s${rec.rescanned ? ' RESCANNED' : ''}${rec.instructions ? ` instr=${rec.instructions}` : ''}${rec.allocMiB ? ` alloc=${rec.allocMiB}MiB` : ''}`);
  if (rec.exit !== 0) throw new Error(`${id} exited ${rec.exit}; see logs/${id}.stderr`);
  return rec;
}

// The saved index keeps its synthesis skip list under a fingerprint of the resolution code
// that wrote it. A build whose resolution code differs ignores the list and rescans every
// file, which is a real cost (the first sync after an upgrade) but not the change under test.
// A warm-up run learns each build's fingerprint; the arm then gets an index copy carrying it.
function prepare(arm) {
  const warm = run(arm, `${arm.name}-warmup`);
  if (!warm.rescanned || o['no-relabel']) return { warmup: warm.id, relabelled: false };
  const copy = path.join(out, `index-${arm.name}`);
  fs.cpSync(arm.index, copy, { recursive: true });
  withDb(copy, (db) => {
    db.prepare("UPDATE project_metadata SET value = ? WHERE key = 'synth_skips_version'").run(warm.skipsVersion);
    db.exec('PRAGMA wal_checkpoint(TRUNCATE)');
  }, false);
  arm.index = copy;
  return { warmup: warm.id, relabelled: true, from: warm.skipsBefore, to: warm.skipsVersion };
}

const median = (xs) => { const s = [...xs].sort((a, b) => a - b); const m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };
const r3 = (x) => Math.round(x * 1000) / 1000;

const corpusRev = execFileSync('git', ['-C', corpus, 'rev-parse', '--short=8', 'HEAD'], { encoding: 'utf8' }).trim();
const noiseKey = (arm) => [os.hostname(), os.release(), arm.runtime, arm.version, path.basename(corpus), corpusRev, o.file, sha(appendText), JSON.stringify(extraEnv)].join('|');
const meta = {
  started: new Date().toISOString(), host: os.hostname(), kernel: os.release(), corpus, corpusRev, file: o.file, append: appendText,
  arms: arms.map(({ name, runtime, version, build, commit }) => ({ name, runtime, version, build, commit })),
  band, metric: o.metric, wrap, env: extraEnv,
};

const prep = { [A.name]: prepare(A), [B.name]: prepare(B) };
meta.prepare = prep;
for (const p of Object.values(prep)) if (p.relabelled) console.log(`note: relabelled the skip list ${p.from} -> ${p.to}; this assumes the change does not alter which files a pass may skip`);

if (o.proxies) {
  if (spawnSync('perf', ['--version']).status !== 0) throw new Error('--proxies needs perf (linux-tools) on PATH');
  // One run of each proxy per arm: user-space instructions (perf) and, on Node, bytes
  // allocated (sampling heap profiler). Both vary far less than peak memory or wall time.
  const res = {};
  for (const arm of arms) {
    res[arm.name] = { instructions: run(arm, `${arm.name}-instr`, 'instructions').instructions };
    if (arm.isNode) res[arm.name].allocMiB = run(arm, `${arm.name}-alloc`, 'alloc').allocMiB;
  }
  const ratio = (k) => (res[A.name][k] && res[B.name][k] ? r3(res[B.name][k] / res[A.name][k]) : null);
  const summary = { ...meta, proxies: res, instructionsRatio: ratio('instructions'), allocRatio: ratio('allocMiB') };
  fs.writeFileSync(path.join(out, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
  console.log(`instructions ${B.name}/${A.name} = ${summary.instructionsRatio}; allocation ${B.name}/${A.name} = ${summary.allocRatio ?? 'n/a (not Node)'}`);
  process.exit(0);
}

// Stored noise floor: the largest same-build deviation seen for this host, kernel, runtime and
// workload. When it matches, later comparisons reuse it instead of running their own controls.
const noiseStore = o.noise && fs.existsSync(o.noise) ? JSON.parse(fs.readFileSync(o.noise, 'utf8')) : {};
const storedNoise = o.noise && noiseKey(A) === noiseKey(B) ? noiseStore[noiseKey(A)] : undefined;
const controls = storedNoise ? 0 : Number(o.controls);
if (storedNoise) console.log(`using stored noise floor from ${storedNoise.measured} (${JSON.stringify(storedNoise.maxDev)})`);

const sessions = [];
let firstPairChecked = false;
for (let s = 1; s <= Number(o.sessions); s++) {
  const pairs = [];
  const ctl = [];
  for (let i = 0; i < Number(o.pairs); i++) {
    // Order alternates inside a session, so each session is balanced on its own.
    const order = (i + s) % 2 === 0 ? [A, B] : [B, A];
    const recs = Object.fromEntries(order.map((arm) => [arm.name, run(arm, `s${s}-p${i + 1}-${arm.name}`)]));
    const a = recs[A.name];
    const b = recs[B.name];
    if (!firstPairChecked) {
      firstPairChecked = true;
      const graph = ['edgesHash', 'nearDupHash', 'nodes', 'edges'].filter((k) => a[k] !== b[k]).map((k) => `${k}: ${a[k]} vs ${b[k]}`);
      const work = diffCounters(a.counters, b.counters);
      if (a.rescanned || b.rescanned) work.push(`skip list rebuilt: ${A.name}=${a.rescanned} ${B.name}=${b.rescanned}`);
      meta.firstPair = { graph, work };
      if (graph.length) console.log(`graph differs on the first pair:\n  ${graph.join('\n  ')}`);
      if (work.length) console.log(`work differs on the first pair:\n  ${work.join('\n  ')}`);
      if (graph.length || (work.length && !o['allow-work-diff'])) {
        fs.writeFileSync(path.join(out, 'summary.json'), JSON.stringify({ ...meta, stopped: 'first pair differs' }, null, 1) + '\n');
        console.error('stopped: the two arms did different work (pass --allow-work-diff when the change is meant to change it)');
        process.exit(3);
      }
    }
    pairs.push({ a, b });
  }
  for (let i = 0; i < controls; i++) {
    const arm = i % 2 === 0 ? A : B;
    ctl.push({ arm: arm.name, x: run(arm, `s${s}-c${i + 1}-${arm.name}-x`), y: run(arm, `s${s}-c${i + 1}-${arm.name}-y`) });
  }
  sessions.push({ pairs, ctl });
  const v = verdicts(sessions);
  if (s === 1 && Number(o.sessions) > 1 && v[o.metric].earlyStop) {
    console.log(`decided after session 1: every pair is outside the band and twice the control spread on ${o.metric}`);
    break;
  }
}

function verdicts(ss) {
  const res = {};
  for (const m of METRICS) {
    const devs = ss.flatMap(({ ctl }) => ctl.map(({ x, y }) => Math.abs(y[m] / x[m] - 1)));
    const maxDev = devs.length ? Math.max(...devs) : (storedNoise?.maxDev[m] ?? null);
    const floor = Math.max(band, maxDev ?? Infinity);
    const per = ss.map(({ pairs }) => {
      const ratios = pairs.map(({ a, b }) => b[m] / a[m]);
      const med = median(ratios);
      return { ratios: ratios.map(r3), median: r3(med), clear: Math.abs(med - 1) > floor };
    });
    const all = per.flatMap((p) => p.ratios);
    const sameSide = all.every((r) => r < 1) || all.every((r) => r > 1);
    const minDev = Math.min(...all.map((r) => Math.abs(r - 1)));
    const base = median(ss.flatMap(({ pairs }) => pairs.map(({ a }) => a[m])));
    const cand = median(ss.flatMap(({ pairs }) => pairs.map(({ b }) => b[m])));
    res[m] = {
      sessions: per, maxControlDev: maxDev === null ? null : r3(maxDev), base, cand, change: r3(cand / base - 1),
      qualified: per.every((p) => p.clear) && per.every((p) => Math.sign(p.median - 1) === Math.sign(per[0].median - 1)),
      earlyStop: sameSide && maxDev !== null && minDev > Math.max(band, 2 * maxDev),
    };
  }
  return res;
}

const v = verdicts(sessions);
if (o['save-noise'] && controls > 0) {
  const store = fs.existsSync(o['save-noise']) ? JSON.parse(fs.readFileSync(o['save-noise'], 'utf8')) : {};
  for (const arm of arms) store[noiseKey(arm)] = { measured: meta.started, maxDev: Object.fromEntries(METRICS.map((m) => [m, v[m].maxControlDev])), runs: sessions.length * controls * 2 };
  fs.writeFileSync(o['save-noise'], JSON.stringify(store, null, 1) + '\n');
}

const fmt = (x, m) => (m === 'peak' ? x.toLocaleString('en-US') : x.toFixed(2));
const word = (c) => (Math.abs(c) <= band ? 'no clear change' : `${Math.abs(c * 100).toFixed(1)}% ${c < 0 ? 'lower' : 'higher'}`);
const lines = [
  `| Metric (${path.basename(corpus)} one-file sync) | ${A.name} | ${B.name} | Change | Qualified |`, '|---|---:|---:|---|---|',
  ...METRICS.map((m) => `| ${{ peak: 'Peak memory', wall: 'Wall time', cpu: 'CPU time' }[m]} | ${fmt(v[m].base, m)} ${UNIT[m]} | ${fmt(v[m].cand, m)} ${UNIT[m]} | ${word(v[m].change)} | ${v[m].qualified ? 'yes' : 'no'} |`),
  '',
  `Per-session median ${B.name}/${A.name} ratio: ${METRICS.map((m) => `${m} ${v[m].sessions.map((s) => s.median).join(', ')}`).join('; ')}.`,
  `Largest same-build deviation: ${METRICS.map((m) => `${m} ${v[m].maxControlDev === null ? 'n/a' : `${(v[m].maxControlDev * 100).toFixed(1)}%`}`).join(', ')}${storedNoise ? ` (stored, measured ${storedNoise.measured})` : ''}.`,
  `Runs: ${sessions.length} session(s), ${sessions.reduce((n, s) => n + s.pairs.length * 2 + s.ctl.length * 2, 0)} timed runs plus 2 warm-ups; graph identical on every run: ${new Set(readRuns().map((r) => `${r.edgesHash}/${r.nearDupHash}`)).size === 1 ? 'yes' : 'NO'}.`,
];
function readRuns() { return fs.readFileSync(runsFile, 'utf8').trim().split('\n').map((l) => JSON.parse(l)).filter((r) => r.mode === 'timed'); }
fs.writeFileSync(path.join(out, 'summary.md'), lines.join('\n') + '\n');
fs.writeFileSync(path.join(out, 'summary.json'), JSON.stringify({ ...meta, finished: new Date().toISOString(), verdicts: v }, null, 1) + '\n');
// Input for the before/after chart script used in pull requests (benefit-chart.py).
fs.writeFileSync(path.join(out, 'chart.json'), JSON.stringify({
  headline: '', workload: `${path.basename(corpus)} ${corpusRev}, one edited file, ${A.version} / ${B.version}`,
  arms: ['Before', 'After'], band: Math.max(band, v[o.metric].maxControlDev ?? 0), note: 'Lower is better.',
  rows: METRICS.filter((m) => v[m].qualified).map((m) => ({ label: { peak: 'Peak memory', wall: 'Wall time', cpu: 'CPU time' }[m], before: v[m].base, after: v[m].cand, unit: UNIT[m] })),
}, null, 1) + '\n');
console.log('\n' + lines.join('\n'));
