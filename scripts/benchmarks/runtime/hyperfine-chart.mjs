#!/usr/bin/env node
// Render the README chart and a Markdown summary from a Hyperfine campaign
// directory produced by hyperfine-index-sync.sh.
//
// usage: node hyperfine-chart.mjs <results-dir> [--svg <path>] [--date YYYY-MM-DD]
//          [--upstream <sha>] [--fork <sha>] [--arms <base>,<candidate>]
//          [--base-label <label>] [--cand-label <label>]
//          [--corpora <csv>] [--allow-missing]
//          [--expect-index <n>] [--expect-sync <n>]
//
// Files are named <corpus>-<arm>-<ns>-<index|sync>.json by the runner; arm
// names default to upstream,fork and follow --arms. Corpus names are matched
// case-insensitively. Peak RSS is Hyperfine's memory_peak_resident. A
// comparison counts as a change only when the fork/base elapsed-time gap
// exceeds the larger arm's (max-min)/median spread, matching the benchmark
// record's rule. A directory must hold one campaign: the default sample counts
// are 2 index and 4 sync measurements per arm, and a mismatch is rejected.

import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';

const args = process.argv.slice(2);
const dir = args.shift();
if (!dir || dir.startsWith('--')) {
  console.error('usage: hyperfine-chart.mjs <results-dir> [--svg <path>] [--date YYYY-MM-DD] [--upstream <sha>] [--fork <sha>]');
  process.exit(2);
}
const flag = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const has = (name) => args.includes(name);
const date = flag('--date') ?? new Date().toISOString().slice(0, 10);
const upstream = flag('--upstream') ?? 'upstream';
const fork = flag('--fork') ?? 'fork';
const baseLabel = flag('--base-label') ?? 'Upstream';
const candLabel = flag('--cand-label') ?? 'Fork';
const svgPath = flag('--svg') ?? 'assets/fork-vs-upstream-performance.svg';
const arms = (flag('--arms') ?? 'upstream,fork').split(',').map((a) => a.trim()).filter(Boolean);
if (arms.length !== 2) {
  console.error('--arms needs exactly two names: <base>,<candidate>');
  process.exit(2);
}
const [baseArm, candArm] = arms;
const expectedIndex = Number(flag('--expect-index') ?? 2);
const expectedSync = Number(flag('--expect-sync') ?? 4);

const LABELS = {
  gin: 'gin (Go)',
  alamofire: 'Alamofire (Swift)',
  pretix: 'pretix (Python)',
  cpython: 'CPython (C/Py)',
  discourse: 'discourse (Ruby)',
  supabase: 'supabase (TS)',
  n8n: 'n8n (Vue/TS)',
};
const ORDER = ['gin', 'alamofire', 'pretix', 'cpython', 'discourse', 'supabase', 'n8n'];
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const FILE = new RegExp(`^(.*)-(${arms.map(escapeRe).join('|')})-\\d+-(index|sync)\\.json$`);

const samples = new Map(); // corpus -> arm -> kind -> { times, rss }
for (const file of readdirSync(dir)) {
  const m = FILE.exec(file);
  if (!m) continue;
  const corpus = m[1].toLowerCase();
  const arm = m[2];
  const kind = m[3];
  const data = JSON.parse(readFileSync(join(dir, file), 'utf8'));
  for (const result of data.results ?? []) {
    for (const run of result.measurements ?? []) {
      const key = `${corpus}\0${arm}\0${kind}`;
      const slot = samples.get(key) ?? { times: [], rss: [] };
      slot.times.push(run.time_wall_clock.value);
      slot.rss.push(run.memory_peak_resident.value / 2 ** 20);
      samples.set(key, slot);
    }
  }
}

function stats(corpus, arm, kind) {
  const s = samples.get(`${corpus}\0${arm}\0${kind}`);
  if (!s || s.times.length === 0) return null;
  const sorted = [...s.times].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  const median = sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
  return {
    median,
    min: sorted[0],
    max: sorted[sorted.length - 1],
    spread: sorted[sorted.length - 1] > 0 ? (sorted[sorted.length - 1] - sorted[0]) / median : 0,
    rss: Math.max(...s.rss),
    n: sorted.length,
  };
}

const expectedCorpora = (flag('--corpora') ?? ORDER.join(',')).split(',').map((c) => c.trim().toLowerCase()).filter(Boolean);
const present = ORDER.filter((c) => stats(c, baseArm, 'index') && stats(c, candArm, 'index'));
const missing = expectedCorpora.filter((c) => !present.includes(c));
if (missing.length > 0 && !has('--allow-missing')) {
  console.error(`missing results for: ${missing.join(', ')} (use --allow-missing for a partial corpus set)`);
  process.exit(1);
}
const corpora = expectedCorpora.filter((c) => present.includes(c));
if (corpora.length === 0) {
  console.error(`no ${basename(dir)} Hyperfine results found for arms ${baseArm},${candArm}`);
  process.exit(1);
}

for (const corpus of corpora) {
  for (const arm of [baseArm, candArm]) {
    for (const kind of ['index', 'sync']) {
      const expected = kind === 'index' ? expectedIndex : expectedSync;
      const s = stats(corpus, arm, kind);
      if (!s) {
        console.error(`missing ${kind} results for ${corpus}/${arm}`);
        process.exit(1);
      }
      if (s.n !== expected) {
        console.error(
          `expected ${expected} ${kind} measurements for ${corpus}/${arm}, found ${s.n}; ` +
            'use a fresh BENCH_OUT (or pass --expect-index/--expect-sync for a different protocol)',
        );
        process.exit(1);
      }
    }
  }
}

const fmt = (n, digits = 2) => n.toFixed(digits);
const pct = (n) => `${(Math.abs(n) * 100).toFixed(0)}%`;
function verdict(displayRatio, a, b, higherIsBetter = false) {
  // displayRatio is candidate/base for index and base/candidate for sync. The
  // qualification gap is always the elapsed-time ratio fork/base.
  const elapsedRatio = higherIsBetter ? 1 / displayRatio : displayRatio;
  const spread = Math.max(a.spread, b.spread);
  const gap = Math.abs(elapsedRatio - 1);
  if (gap <= spread) return 'no clear change';
  const faster = elapsedRatio < 1;
  return `${pct(gap)} ${faster ? 'faster' : 'slower'}`;
}

console.log('| Corpus | Index base | Index candidate | Candidate / base | Verdict | Sync base | Sync candidate | Base / candidate | Verdict |');
console.log('|---|---:|---:|---:|---|---:|---:|---:|---|');
for (const corpus of corpora) {
  const bi = stats(corpus, baseArm, 'index');
  const ci = stats(corpus, candArm, 'index');
  const bs = stats(corpus, baseArm, 'sync');
  const cs = stats(corpus, candArm, 'sync');
  const ir = ci.median / bi.median;
  const sr = bs.median / cs.median;
  console.log(
    `| ${corpus} | ${fmt(bi.median, 3)} s | ${fmt(ci.median, 3)} s | ${fmt(ir, 3)} | ${verdict(ir, bi, ci)} | ` +
      `${fmt(bs.median, 3)} s | ${fmt(cs.median, 3)} s | ${fmt(sr, 3)} | ${verdict(sr, bs, cs, true)} |`,
  );
}

// Chart geometry copied from the 2026-10-31 chart: index panel 0.5x-1.5x,
// sync panel 0.5x-3x, seven rows.
const indexX1 = 241.9;
const indexScale = 391.8;
const syncX1 = 609.2;
const syncScale = 172.4;
function bar(x1, scale, ratio) {
  if (ratio <= 1) {
    const w = (1 - ratio) * scale;
    return { x: x1 - w, w, tx: x1 - w - 6, anchor: 'end' };
  }
  const w = (ratio - 1) * scale;
  return { x: x1, w, tx: x1 + w + 6, anchor: 'start' };
}
const lines = [];
lines.push('<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="780" viewBox="0 0 1000 780" role="img" aria-label="Fork versus upstream on seven corpora, Node, ' + date + '">');
lines.push('<title>Fork versus upstream on seven corpora, Node, ' + date + '</title>');
lines.push('<desc>Two panels: full index candidate/base ratios and one-file sync base/candidate speedups for gin, Alamofire, pretix, CPython, discourse, supabase and n8n.</desc>');
lines.push('<rect width="1000" height="780" fill="#0d1117"/>');
lines.push('<style>text{font-family:Inter,Segoe UI,system-ui,sans-serif;fill:#e6edf3}.title{font-size:26px;font-weight:700}.sub{font-size:14px;fill:#9da7b3}.corpus{font-size:15px;font-weight:600}.val{font-size:14px;font-weight:700}.axis{font-size:12px;fill:#8b949e}.good{fill:#3fb950}.bad{fill:#f85149}.tie{fill:#8b949e}</style>');
lines.push('<text x="46" y="56" class="title">Fork vs upstream on Node — seven corpora, ' + date + '</text>');
lines.push('<text x="46" y="82" class="sub">' + baseLabel + ' ' + upstream + ' · ' + candLabel + ' ' + fork + ' · Node 24.22.2 · Hyperfine 2.0.0</text>');
lines.push('<text x="46" y="104" class="sub">Index: median of 2 runs, candidate / base (lower is better). Sync: median of 4 runs, base / candidate (higher is better).</text>');
lines.push('<text x="46" y="140" style="font-size:17px;font-weight:700">Full index</text>');
for (const [label, x, muted] of [['0.5×', 46.0, false], ['0.75×', 144.0, false], ['1×', indexX1, true], ['1.25×', 339.9, false], ['1.5×', 437.8, false]]) {
  lines.push(`<line x1="${x}" y1="180" x2="${x}" y2="602" stroke="${muted ? '#8b949e' : '#30363d'}" stroke-width="1"/>`);
  lines.push(`<text x="${x}" y="174" class="axis" text-anchor="middle">${label}</text>`);
}
let y = 200;
for (const corpus of corpora) {
  const bi = stats(corpus, baseArm, 'index');
  const ci = stats(corpus, candArm, 'index');
  const ratio = ci.median / bi.median;
  const b = bar(indexX1, indexScale, ratio);
  lines.push(`<text x="46" y="${y}" class="corpus">${LABELS[corpus] ?? corpus}</text>`);
  lines.push(`<rect x="${b.x.toFixed(1)}" y="${y - 12}" width="${Math.max(b.w, 1).toFixed(1)}" height="18" rx="3" class="tie"/>`);
  lines.push(`<text x="${b.tx.toFixed(1)}" y="${y}" class="val tie" text-anchor="${b.anchor}">${fmt(ratio)}×</text>`);
  y += 58;
}
lines.push('<text x="523.0" y="140" style="font-size:17px;font-weight:700">One-file sync</text>');
for (const [label, x, muted] of [['0.5×', 523.0, false], ['1×', syncX1, true], ['1.5×', 695.4, false], ['2×', 781.6, false], ['2.5×', 867.8, false], ['3×', 954.0, false]]) {
  lines.push(`<line x1="${x}" y1="180" x2="${x}" y2="602" stroke="${muted ? '#8b949e' : '#30363d'}" stroke-width="1"/>`);
  lines.push(`<text x="${x}" y="174" class="axis" text-anchor="middle">${label}</text>`);
}
y = 200;
for (const corpus of corpora) {
  const bs = stats(corpus, baseArm, 'sync');
  const cs = stats(corpus, candArm, 'sync');
  const ratio = bs.median / cs.median;
  const b = bar(syncX1, syncScale, ratio);
  const cls = ratio >= 1 ? 'good' : 'bad';
  lines.push(`<text x="523.0" y="${y}" class="corpus">${LABELS[corpus] ?? corpus}</text>`);
  lines.push(`<rect x="${b.x.toFixed(1)}" y="${y - 12}" width="${Math.max(b.w, 1).toFixed(1)}" height="18" rx="3" class="${cls}"/>`);
  lines.push(`<text x="${b.tx.toFixed(1)}" y="${y}" class="val ${cls}" text-anchor="${b.anchor}">${fmt(ratio)}×</text>`);
  y += 58;
}
lines.push('<text x="46" y="708" class="axis">Exact ratios; the tables mark which gaps clear the larger arm\'s run-to-run spread.</text>');
lines.push(`<text x="46" y="726" class="axis">Method, spreads, graph sizes, admission receipts: docs/benchmarks/fork-vs-upstream-${date}.md</text>`);
lines.push('<text x="46" y="762" class="axis">CodeGraph fork · bompus/codegraph · measured under the host benchmark lock</text>');
lines.push('</svg>');
writeFileSync(svgPath, lines.join('\n') + '\n');
console.log(`wrote ${svgPath}`);
