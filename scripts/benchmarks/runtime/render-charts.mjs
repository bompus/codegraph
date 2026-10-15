#!/usr/bin/env node
// Turns a benchmark run's raw output into the README chart, the benchmark doc's
// heatmap and the Markdown tables both pages quote, so no figure is typed by hand.
//
//   node scripts/benchmarks/runtime/render-charts.mjs $BENCH_OUT \
//     --date 2026-10-13 --upstream 34dd3d2f --fork f045e5f7 --node 24.21.0 --bun 1.4.3 \
//     [--cards assets/fork-vs-upstream-performance.svg] \
//     [--heatmap docs/benchmarks/fork-vs-upstream-heatmap-2026-10-13.svg] [--tables tables.md]
//
// Per corpus the accepted attempt is the first `index-<corpus>-aN` directory whose
// `status` file reads `accepted`; the same goes for `mcp-<corpus>-aN` and `cli-gin-aN`.
// Medians are taken over the runs in that attempt (two per arm, mirrored order).
// A change counts as better or worse only outside the 3% reporting band.
import { readFileSync, readdirSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const argv = process.argv.slice(2);
const root = argv.find((a, i) => !a.startsWith('--') && !(argv[i - 1] ?? '').startsWith('--'));
const opt = (k, d) => { const i = argv.indexOf(`--${k}`); return i >= 0 ? argv[i + 1] : d; };
if (!root) { console.error('usage: render-charts.mjs <BENCH_OUT> --date D --upstream SHA --fork SHA --node V --bun V [--cards F] [--heatmap F] [--tables F]'); process.exit(2); }
const META = { date: opt('date'), upstream: opt('upstream'), fork: opt('fork'), node: opt('node'), bun: opt('bun') };
for (const [k, v] of Object.entries(META)) if (!v) { console.error(`missing --${k}`); process.exit(2); }

const ARMS = ['upnode', 'forknode', 'forkbun', 'upbun'];
const CORPORA = [['gin', 'gin'], ['Alamofire', 'Alamofire'], ['pretix', 'pretix'], ['cpython', 'CPython'], ['discourse', 'discourse'], ['supabase', 'supabase'], ['n8n', 'n8n']];
const BAND = 0.03;
const median = (a) => { const s = [...a].sort((x, y) => x - y), m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };
const seconds = (t) => { const p = t.split(':'); return parseFloat(p.at(-1)) + 60 * parseFloat(p.at(-2)); };
const accepted = (prefix) => {
  const attempt = (d) => +d.slice(d.lastIndexOf('-a') + 2);
  const dir = readdirSync(root).filter((d) => d.startsWith(prefix + '-a') && Number.isInteger(attempt(d))).sort((x, y) => attempt(x) - attempt(y))
    .find((d) => existsSync(join(root, d, 'status')) && readFileSync(join(root, d, 'status'), 'utf8').trim() === 'accepted');
  if (!dir) throw new Error(`no accepted attempt for ${prefix}`);
  return join(root, dir);
};

// ---- load
const index = {};
for (const [key] of CORPORA) {
  const rows = readFileSync(join(accepted(`index-${key}`), 'index-sync.tsv'), 'utf8').trim().split('\n').map((l) => l.split('\t'));
  index[key] = {};
  for (const arm of ARMS) {
    const i = rows.filter((r) => r[1] === arm && r[2] === 'index');
    const s = rows.filter((r) => r[1] === arm && r[2] !== 'index');
    if (!i.length || !s.length) throw new Error(`${key} ${arm}: missing rows`);
    // A sync that exits non-zero has no time worth comparing: its comparisons are left out and drawn as n/a.
    const syncFailed = s.some((r) => r[3] !== '0');
    index[key][arm] = {
      indexS: median(i.map((r) => seconds(r[4]))), peakMiB: median(i.map((r) => +r[5])) / 1024,
      dbMiB: median(i.map((r) => +r[7])) / 2 ** 20, nodes: +i[0][8], edges: +i[0][9],
      syncS: syncFailed ? NaN : median(s.map((r) => seconds(r[4]))), runs: i.length, syncRuns: s.length,
    };
  }
}
const mcp = {};
for (const key of ['gin', 'n8n']) {
  const runs = readFileSync(join(accepted(`mcp-${key}`), 'mcp.jsonl'), 'utf8').trim().split('\n').map((l) => JSON.parse(l));
  mcp[key] = {};
  for (const arm of ARMS) {
    const x = runs.filter((r) => r.arm === arm);
    const g = (k) => {
      // A timed-out watcher sync is recorded as null; publishing the other run alone would mislead.
      if (x.length === 0 || x.some((r) => r[k] == null)) throw new Error(`mcp ${key} ${arm}: ${k} is missing in ${x.length} run(s)`);
      return median(x.map((r) => r[k]));
    };
    mcp[key][arm] = {
      initMs: g('initMs'), firstMs: g('coldToFirstAnswerMs'), warmMs: g('warmMedianMs'), p90Ms: g('warmP90Ms'),
      wave8Ms: g('wave8Ms'), watchMs: g('watchSyncMs'), busyMiB: g('busy_totalMB'), idleMiB: g('idle_totalMB'), idleCpu: g('idleCpuPct'),
    };
  }
}
const cli = {};
{
  const dir = accepted('cli-gin');
  for (const [n, label] of [['version', '--version'], ['status', 'status'], ['explore', 'explore']]) {
    const res = JSON.parse(readFileSync(join(dir, `cli-${n}.json`), 'utf8')).results;
    cli[n] = { label };
    for (const arm of ARMS) {
      const w = res.find((r) => r.name === arm).summary.time_wall_clock;
      cli[n][arm] = { mean: w.mean * 1000, sd: w.stddev * 1000 };
    }
  }
}

// ---- formatting
const fSec = (v) => Number.isNaN(v) ? 'sync failed' : `${v < 10 ? v.toFixed(2) : v.toFixed(1)} s`;
const fMem = (miB) => (miB >= 1024 ? `${(miB / 1024).toFixed(2)} GiB` : miB < 100 ? `${miB.toFixed(1)} MiB` : `${Math.round(miB)} MiB`);
const fMs = (v) => (v >= 1000 ? fSec(v / 1000) : `${Math.round(v)} ms`);
const fPct = (v) => `${v.toFixed(2)}%`;
const pctChange = (r) => { if (!Number.isFinite(r)) return 'n/a'; const d = Math.round((r - 1) * 100); return (d > 0 ? '+' : d < 0 ? '−' : '') + Math.abs(d) + '%'; };
const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;');

// ---- every comparison: [group, label, {arm: value}, formatter]
const metrics = [];
for (const [key, name] of CORPORA) metrics.push(['Full index: time', name, pick(index[key], 'indexS'), fSec]);
for (const [key, name] of CORPORA) metrics.push(['Full index: peak memory', name, pick(index[key], 'peakMiB'), fMem]);
for (const [key, name] of CORPORA) metrics.push(['One-file sync: time', name, pick(index[key], 'syncS'), fSec]);
const MCP_ROWS = [['firstMs', 'Start→answer', fMs], ['warmMs', 'Warm ask', fMs], ['wave8Ms', '8 asks at once', fMs], ['watchMs', 'File watcher', fMs],
  ['busyMiB', 'Memory busy', fMem], ['idleMiB', 'Memory idle', fMem], ['idleCpu', 'CPU idle', fPct]];
for (const key of ['n8n', 'gin']) for (const [k, label, f] of MCP_ROWS) metrics.push([`MCP server on ${key}`, label, pick(mcp[key], k), f]);
for (const n of ['version', 'status', 'explore']) metrics.push(['Command-line startup on gin', cli[n].label, Object.fromEntries(ARMS.map((a) => [a, cli[n][a].mean])), fMs]);
function pick(o, k) { return Object.fromEntries(ARMS.map((a) => [a, o[a][k]])); }

// Two zero measurements are equal; a zero baseline with a positive fork value is Infinity.
const ratio = (m, rt) => {
  const [up, fk] = rt === 'Node' ? [m[2].upnode, m[2].forknode] : [m[2].upbun, m[2].forkbun];
  return up === 0 ? (fk === 0 ? 1 : Infinity) : fk / up;
};
const every = [];
for (const m of metrics) for (const rt of ['Node', 'Bun']) every.push({ m, rt, r: ratio(m, rt) });
const excluded = every.filter((c) => Number.isNaN(c.r));
const ARM_LABEL = { upnode: 'upstream on Node', forknode: 'fork on Node', forkbun: 'fork on Bun', upbun: 'upstream on Bun' };
const failedArms = ARMS.filter((a) => CORPORA.some(([k]) => Number.isNaN(index[k][a].syncS))).map((a) => ARM_LABEL[a]).join(' and ');
const cmp = every.filter((c) => !Number.isNaN(c.r));
const nBetter = cmp.filter((c) => c.r < 1 - BAND).length;
const nSame = cmp.filter((c) => c.r >= 1 - BAND && c.r <= 1 + BAND).length;
const nWorse = cmp.filter((c) => c.r > 1 + BAND).length;

// ---- SVG pieces
const C = { bg: '#0d1117', panel: '#161b22', text: '#e6edf3', muted: '#8b949e', good: '#2ea043', same: '#d29922', bad: '#f85149' };
const col = (r) => Number.isNaN(r) ? C.muted : (r < 1 - BAND ? C.good : r > 1 + BAND ? C.bad : C.same);
const FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif";
const T = (x, y, s, o = {}) => `<text x="${x}" y="${y}" font-size="${o.size || 12}" font-weight="${o.w || 400}" fill="${o.fill || C.text}" text-anchor="${o.a || 'start'}">${esc(s)}</text>`;
const wrap = (W, H, body, title, desc) => `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" font-family="${FONT}" role="img" aria-labelledby="t d">\n<title id="t">${esc(title)}</title>\n<desc id="d">${esc(desc)}</desc>\n<rect width="${W}" height="${H}" rx="8" fill="${C.bg}"/>\n${body}\n</svg>\n`;
const wrapLine = (s, n) => { const out = []; let cur = ''; for (const w of s.split(' ')) { if ((cur + ' ' + w).trim().length > n) { out.push(cur); cur = w; } else cur = (cur + ' ' + w).trim(); } if (cur) out.push(cur); return out; };
const FOOT = `Measured ${META.date} · upstream ${META.upstream} vs fork ${META.fork} · Node ${META.node}, Bun ${META.bun} · 16-vCPU WSL2 · two runs per arm, median · fork on Node unless noted`;
const find = (group, label) => metrics.find((m) => m[0] === group && m[1] === label);

function cards() {
  const W = 880;
  const idx = (name) => find('Full index: time', name), mem = (name) => find('Full index: peak memory', name);
  const speedups = ['n8n', 'CPython'].map((n) => 1 / ratio(idx(n), 'Node'));
  const savings = ['n8n', 'CPython'].map((n) => (1 - ratio(mem(n), 'Node')) * 100);
  const lo = (a) => Math.min(...a), hi = (a) => Math.max(...a);
  const sp = lo(speedups).toFixed(1) === hi(speedups).toFixed(1) ? `${lo(speedups).toFixed(1)}×` : `${lo(speedups).toFixed(1)}–${hi(speedups).toFixed(1)}×`;
  const title = `n8n and CPython: the fork indexes ${sp} faster and uses ${Math.round(lo(savings))}–${Math.round(hi(savings))}% less memory`;
  const card = [
    ['Index n8n from scratch', idx('n8n'), 'time'],
    ['Update after editing one file', find('One-file sync: time', 'n8n'), 'time'],
    ['Answer a question (warm)', find('MCP server on n8n', 'Warm ask'), 'time'],
    ['Memory while indexing n8n', mem('n8n'), 'mem'],
    ['MCP server memory when idle', find('MCP server on n8n', 'Memory idle'), 'mem'],
    ['Start the command line', find('Command-line startup on gin', '--version'), 'time'],
  ];
  let b = T(24, 38, title, { size: 19, w: 700 });
  b += T(24, 60, 'Numbers are for n8n (the largest test repo) and the command line. Lower is better. Upstream → fork.', { size: 12, fill: C.muted });
  card.forEach(([t, m, kind], i) => {
    const x = 24 + (i % 3) * 284, y = 82 + Math.floor(i / 3) * 150, r = ratio(m, 'Node');
    const badge = Number.isNaN(r) ? 'n/a' : kind === 'mem' ? `${Math.round((1 - r) * 100)}% less` : `${(1 / r).toFixed(1)}× faster`;
    const f = m[3], v = m[2];
    b += `<rect x="${x}" y="${y}" width="272" height="138" rx="8" fill="${C.panel}"/>`;
    b += T(x + 16, y + 26, t, { size: 13, fill: C.muted });
    b += T(x + 16, y + 72, badge, { size: 30, w: 700, fill: col(r) });
    b += T(x + 16, y + 100, `${f(v.upnode)} → ${f(v.forknode)}`, { size: 15 });
    b += T(x + 16, y + 120, `on Bun: ${f(v.upbun)} → ${f(v.forkbun)}`, { size: 12, fill: C.muted });
  });

  // every comparison that is not clearly better, grouped by what was measured
  const phrase = { 'Full index: time': (l) => `Indexing ${l}`, 'Full index: peak memory': (l) => `Memory indexing ${l}`, 'One-file sync: time': (l) => `Syncing one file in ${l}` };
  const name = (m) => (phrase[m[0]] ? phrase[m[0]](m[1]) : m[0].startsWith('MCP server on ') ? `${m[1]} on ${m[0].slice(14)} (MCP server)` : `codegraph ${m[1]}`);
  const byMetric = new Map();
  for (const c of cmp) if (c.r > 1 + BAND) { const k = c.m; (byMetric.get(k) ?? byMetric.set(k, []).get(k)).push(c); }
  const worse = [...byMetric].map(([m, cs]) => `${name(m)}: ` + cs.map((c) => { const v = c.rt === 'Node' ? [m[2].upnode, m[2].forknode] : [m[2].upbun, m[2].forkbun]; return `${m[3](v[0])} → ${m[3](v[1])} on ${c.rt} (${pctChange(c.r)})`; }).join(', '));
  const lower = (s) => s[0].toLowerCase() + s.slice(1);
  const same = cmp.filter((c) => c.r >= 1 - BAND && c.r <= 1 + BAND).map((c) => `${lower(name(c.m))} on ${c.rt}`);
  const wrapText = (s, n) => { const out = []; let cur = ''; for (const w of s.split(' ')) { if ((cur + ' ' + w).trim().length > n) { out.push(cur); cur = w; } else cur = (cur + ' ' + w).trim(); } if (cur) out.push(cur); return out; };
  const lines = [...worse, `About the same (within 3%): ${same.join('; ')}.`].flatMap((l) => wrapText(l, 118));
  const boxH = 44 + lines.length * 22;
  let y = 392;
  b += `<rect x="24" y="${y}" width="832" height="${boxH}" rx="8" fill="${C.panel}" stroke="${C.bad}" stroke-opacity=".5"/>`;
  b += T(40, y + 26, 'Where the fork is not better', { size: 14, w: 700, fill: C.bad });
  lines.forEach((s, i) => { b += T(40, y + 54 + i * 22, s, { size: 12.5 }); });
  y += boxH + 26;
  const tally = `Over all ${cmp.length} comparisons on Node and Bun: better in ${nBetter}, about the same in ${nSame}, worse in ${nWorse}.${excluded.length ? ` ${excluded.length} more are left out because the one-file sync failed for ${failedArms}.` : ''}`;
  wrapLine(tally, 120).forEach((l, i) => { if (i) y += 18; b += T(24, y, l, { size: 12, fill: C.muted }); });
  y += 20;
  b += T(24, y, FOOT, { size: 11, fill: C.muted });
  const n8 = index.n8n;
  const desc = `On n8n the fork indexes in ${fSec(n8.forknode.indexS)} against ${fSec(n8.upnode.indexS)}, syncs one edited file in ${fSec(n8.forknode.syncS)} against ${fSec(n8.upnode.syncS)}, and answers a warm question in ${fMs(mcp.n8n.forknode.warmMs)} against ${fMs(mcp.n8n.upnode.warmMs)}. Of ${cmp.length} comparisons it is better in ${nBetter}, about the same in ${nSame} and worse in ${nWorse}.`;
  return { svg: wrap(W, y + 16, b, 'Fork vs upstream CodeGraph: headline numbers', desc), lines };
}

function heatmap() {
  const W = 880;
  let y = 36, b = T(24, y, 'Every result at a glance', { size: 20, w: 700 });
  y += 22;
  b += T(24, y, 'Each cell is the fork’s change versus upstream. Negative is better (less time or memory). Green = better by over 3%, amber = about the same, red = worse.', { size: 12, fill: C.muted });
  y += 26;
  const groups = [...new Set(metrics.map((m) => m[0]))];
  for (const g of groups) {
    const cols = metrics.filter((m) => m[0] === g), LX = 24, LW = 70, CW = Math.min(112, (W - LX - LW - 24) / cols.length), CH = 34;
    b += T(24, y, g, { size: 14, w: 700 });
    y += 14;
    cols.forEach((c, i) => { b += T(LX + LW + i * CW + CW / 2, y + 10, c[1], { size: 11, fill: C.muted, a: 'middle' }); });
    y += 18;
    for (const rt of ['Node', 'Bun']) {
      b += T(LX, y + CH / 2 + 4, `Fork on ${rt}`, { size: 11, fill: C.muted });
      cols.forEach((c, i) => {
        const r = ratio(c, rt), op = r > 1 + BAND ? 0.85 : r < 1 - BAND ? 0.25 + Math.min(0.6, 1 - r) : 0.55;
        b += `<rect x="${LX + LW + i * CW + 2}" y="${y}" width="${CW - 4}" height="${CH - 4}" rx="4" fill="${col(r)}" fill-opacity="${op}"/>`;
        b += T(LX + LW + i * CW + CW / 2, y + CH / 2 + 3, pctChange(r), { size: 13, w: 700, a: 'middle' });
      });
      y += CH;
    }
    y += 18;
  }
  if (excluded.length) for (const l of wrapLine(`Grey n/a: the one-file sync exited with an error for ${failedArms}, so there is no time to compare.`, 130)) { b += T(24, y + 4, l, { size: 11, fill: C.muted }); y += 18; }
  b += T(24, y + 4, FOOT, { size: 11, fill: C.muted });
  const desc = `Heatmap of the fork's change against upstream for full index time, peak memory and one-file sync on seven repositories, the MCP server on n8n and gin, and command-line startup. Of ${cmp.length} comparisons it is better in ${nBetter}, about the same in ${nSame} and worse in ${nWorse}.`;
  return wrap(W, y + 24, b, 'Fork vs upstream CodeGraph: every result', desc);
}

// ---- Markdown tables
function tables() {
  const L = [];
  const row = (cells) => `| ${cells.join(' | ')} |`;
  const cell = (key, arm, k, f) => f(index[key][arm][k]);
  L.push('## Full index, time and peak memory (README: Upstream Node, Fork Node, Fork Bun)', '');
  L.push(row(['Corpus', 'Upstream, Node', 'Fork, Node', 'Fork, Bun']), row(['---', '---', '---', '---']));
  for (const [key, name] of CORPORA) L.push(row([name, ...['upnode', 'forknode', 'forkbun'].map((a) => `${fSec(index[key][a].indexS)}, ${fMem(index[key][a].peakMiB)}`)]));
  L.push('', '## Full index (doc: all four arms)', '', row(['Corpus', 'Upstream, Node', 'Fork, Node', 'Fork, Bun', 'Upstream, Bun']), row(['---', '---', '---', '---', '---']));
  for (const [key, name] of CORPORA) L.push(row([name, ...ARMS.map((a) => `${fSec(index[key][a].indexS)}, ${fMem(index[key][a].peakMiB)}`)]));
  L.push('', '## Graph size', '', row(['Corpus', 'Upstream nodes / edges / database', 'Fork nodes / edges / database']), row(['---', '---', '---']));
  const num = (n) => n.toLocaleString('en-US');
  for (const [key, name] of CORPORA) L.push(row([name, ...['upnode', 'forknode'].map((a) => `${num(index[key][a].nodes)} / ${num(index[key][a].edges)} / ${fMem(index[key][a].dbMiB)}`)]));
  L.push('', '## One-file sync (README)', '', row(['Corpus', 'Upstream, Node', 'Fork, Node', 'Fork, Bun']), row(['---', '---', '---', '---']));
  for (const [key, name] of CORPORA) L.push(row([name, ...['upnode', 'forknode', 'forkbun'].map((a) => fSec(index[key][a].syncS))]));
  L.push('', '## One-file sync (doc)', '', row(['Corpus', 'Upstream, Node', 'Fork, Node', 'Fork, Bun', 'Upstream, Bun']), row(['---', '---', '---', '---', '---']));
  for (const [key, name] of CORPORA) L.push(row([name, ...ARMS.map((a) => fSec(index[key][a].syncS))]));
  for (const key of ['n8n', 'gin']) {
    L.push('', `## MCP server on ${key}`, '', row(['Measure', 'Upstream, Node', 'Fork, Node', 'Fork, Bun', 'Upstream, Bun']), row(['---', '---', '---', '---', '---']));
    for (const [label, k, f] of [['`initialize` answered', 'initMs', fMs], ['Start to first explore answer', 'firstMs', fMs], ['Warm explore, median', 'warmMs', fMs], ['Warm explore, p90', 'p90Ms', fMs],
      ['8 concurrent explores', 'wave8Ms', fMs], ['Memory while busy', 'busyMiB', fMem], ['Edited file re-indexed', 'watchMs', fMs], ['Memory at idle', 'idleMiB', fMem], ['CPU at idle', 'idleCpu', fPct]])
      L.push(row([label, ...ARMS.map((a) => f(mcp[key][a][k]))]));
  }
  L.push('', '## CLI startup on gin (mean ± sd)', '', row(['Command', 'Upstream, Node', 'Fork, Node', 'Fork, Bun', 'Upstream, Bun']), row(['---', '---', '---', '---', '---']));
  for (const n of ['version', 'status', 'explore']) L.push(row([cli[n].label, ...ARMS.map((a) => `${Math.round(cli[n][a].mean)} ± ${Math.round(cli[n][a].sd)} ms`)]));
  // derived statements the README prose quotes
  const lines = [];
  const range = (f, keys) => { const v = keys.map(f); return [Math.min(...v), Math.max(...v)]; };
  const ks = CORPORA.map((c) => c[0]);
  const pct = (a, b) => (1 - a / b) * 100;
  const fr = (lo, hi) => `${Math.round(lo)}–${Math.round(hi)}%`;
  L.push('', '## Derived (README prose)', '');
  const bunPeak = range((k) => pct(index[k].forkbun.peakMiB, index[k].forknode.peakMiB), ks);
  L.push(`- Bun peak index memory vs Node (fork), % lower: ${fr(...bunPeak)} (${ks.map((k) => `${k} ${pct(index[k].forkbun.peakMiB, index[k].forknode.peakMiB).toFixed(0)}`).join(', ')})`);
  L.push(`- Bun idle MCP memory vs Node (fork), % lower: ${['n8n', 'gin'].map((k) => `${k} ${pct(mcp[k].forkbun.idleMiB, mcp[k].forknode.idleMiB).toFixed(0)}`).join(', ')}`);
  L.push(`- Bun index time vs Node (fork), % change (negative faster): ${ks.map((k) => `${k} ${((index[k].forkbun.indexS / index[k].forknode.indexS - 1) * 100).toFixed(0)}`).join(', ')}`);
  L.push(`- Bun sync time vs Node (fork), % change: ${ks.map((k) => `${k} ${((index[k].forkbun.syncS / index[k].forknode.syncS - 1) * 100).toFixed(0)}`).join(', ')}`);
  L.push(`- Bun 8 concurrent explores vs Node (fork), % change: ${['n8n', 'gin'].map((k) => `${k} ${((mcp[k].forkbun.wave8Ms / mcp[k].forknode.wave8Ms - 1) * 100).toFixed(0)}`).join(', ')}`);
  L.push(`- Idle CPU, fork Bun / fork Node: ${['n8n', 'gin'].map((k) => `${k} ${(mcp[k].forkbun.idleCpu / mcp[k].forknode.idleCpu).toFixed(1)}x (${fPct(mcp[k].forkbun.idleCpu)} vs ${fPct(mcp[k].forknode.idleCpu)})`).join(', ')}`);
  L.push(`- Database size, fork vs upstream, % bigger: ${ks.map((k) => `${k} ${((index[k].forknode.dbMiB / index[k].upnode.dbMiB - 1) * 100).toFixed(0)}`).join(', ')}`);
  L.push(`- Edges, fork vs upstream, % change: ${ks.map((k) => `${k} ${((index[k].forknode.edges / index[k].upnode.edges - 1) * 100).toFixed(1)}`).join(', ')}`);
  L.push(`- Node/edge counts identical across runtimes: ${ks.every((k) => index[k].upnode.nodes === index[k].upbun.nodes && index[k].forknode.nodes === index[k].forkbun.nodes && index[k].upnode.edges === index[k].upbun.edges && index[k].forknode.edges === index[k].forkbun.edges)}`);
  L.push(`- Comparisons: ${cmp.length}; better ${nBetter}, about the same ${nSame}, worse ${nWorse}`);
  return L.join('\n') + '\n';
}

const cardOut = cards();
const cardsPath = opt('cards'), heatPath = opt('heatmap'), tablesPath = opt('tables');
if (cardsPath) writeFileSync(cardsPath, cardOut.svg);
if (heatPath) writeFileSync(heatPath, heatmap());
const md = tables();
if (tablesPath) writeFileSync(tablesPath, md); else process.stdout.write(md);
console.error(`comparisons ${cmp.length}: better ${nBetter}, same ${nSame}, worse ${nWorse}`);
for (const l of cardOut.lines) console.error('  not-better line: ' + l);
