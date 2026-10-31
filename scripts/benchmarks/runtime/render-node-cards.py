#!/usr/bin/env python3
"""Render the Node-only fork-vs-upstream headline card from a run's TSV.

Usage:
  python3 scripts/benchmarks/runtime/render-node-cards.py <bench-out> <out.svg> \
      --date 2026-10-31 --upstream cd04b21a --fork cf2a37a5 --node 24.22.2

Reads <bench-out>/index-sync.tsv (the flat runner layout: corpus, arm, step,
exit, elapsed, maxRSS…). Arms are `upstream` and `fork`. The index panel plots
fork/upstream (lower is better); the sync panel plots upstream/fork (higher is
better). Ratios are exact; the tables decide which gaps clear their spread.
"""
import argparse
import csv
from html import escape
from statistics import median

p = argparse.ArgumentParser()
p.add_argument('bench_out')
p.add_argument('out')
p.add_argument('--date', required=True)
p.add_argument('--upstream', required=True)
p.add_argument('--fork', required=True)
p.add_argument('--node', required=True)
args = p.parse_args()

rows = list(csv.reader(open(f'{args.bench_out}/index-sync.tsv'), delimiter='\t'))


def secs(s):
    parts = s.split(':')
    return int(parts[-2]) * 60 + float(parts[-1])


data = {}
for r in rows:
    if len(r) < 6:
        continue
    corpus, arm, step = r[0], r[1], r[2]
    key = 'sync' if step.startswith('sync') else 'index'
    data.setdefault((corpus, arm, key), []).append(secs(r[4]))

corpora = ['gin', 'alamofire', 'pretix', 'cpython', 'discourse', 'supabase', 'n8n']
labels = {
    'gin': 'gin (Go)', 'alamofire': 'Alamofire (Swift)', 'pretix': 'pretix (Python)',
    'cpython': 'CPython (C/Py)', 'discourse': 'discourse (Ruby)', 'supabase': 'supabase (TS)',
    'n8n': 'n8n (Vue/TS)',
}


def med(corpus, arm, key):
    return median(data[(corpus, arm, key)])


W, H = 1000, 780
pad = 46
panel_w = (W - 3 * pad) / 2
row_h = 58
top = 150

svg = [
    f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-label="Fork versus upstream on seven corpora, Node, {escape(args.date)}">',
    f'<title>Fork versus upstream on seven corpora, Node, {escape(args.date)}</title>',
    '<desc>Two panels: full index fork/upstream ratios and one-file sync upstream/fork speedups for gin, Alamofire, pretix, CPython, discourse, supabase and n8n.</desc>',
    f'<rect width="{W}" height="{H}" fill="#0d1117"/>',
    '<style>text{font-family:Inter,Segoe UI,system-ui,sans-serif;fill:#e6edf3}'
    '.title{font-size:26px;font-weight:700}.sub{font-size:14px;fill:#9da7b3}'
    '.corpus{font-size:15px;font-weight:600}.val{font-size:14px;font-weight:700}'
    '.axis{font-size:12px;fill:#8b949e}.good{fill:#3fb950}.bad{fill:#f85149}.tie{fill:#8b949e}'
    '</style>',
    f'<text x="{pad}" y="56" class="title">Fork vs upstream on Node — seven corpora, {escape(args.date)}</text>',
    f'<text x="{pad}" y="82" class="sub">Upstream {escape(args.upstream)} · fork {escape(args.fork)} · Node {escape(args.node)} · Node-only run</text>',
    f'<text x="{pad}" y="104" class="sub">Index: median of 2 runs, fork / upstream (lower is better). Sync: median of 4 runs, upstream / fork (higher is better).</text>',
]


def panel(x0, title, kind):
    svg.append(f'<text x="{x0}" y="140" style="font-size:17px;font-weight:700">{escape(title)}</text>')
    base_y = top + 46
    lo, hi = (0.5, 1.6) if kind == 'index' else (0.5, 3.0)
    ticks = (0.5, 0.75, 1.0, 1.25, 1.5) if kind == 'index' else (0.5, 1.0, 1.5, 2.0, 2.5, 3.0)

    def px(v):
        return x0 + (v - lo) / (hi - lo) * panel_w

    for tick in ticks:
        X = px(tick)
        last = base_y + row_h * len(corpora)
        svg.append(f'<line x1="{X:.1f}" y1="{base_y - 16}" x2="{X:.1f}" y2="{last}" stroke="{"#30363d" if tick != 1.0 else "#8b949e"}" stroke-width="1"/>')
        svg.append(f'<text x="{X:.1f}" y="{base_y - 22}" class="axis" text-anchor="middle">{tick:g}×</text>')
    for i, c in enumerate(corpora):
        y = base_y + i * row_h
        svg.append(f'<text x="{x0}" y="{y + 4}" class="corpus">{escape(labels[c])}</text>')
        if kind == 'index':
            ratio = med(c, 'fork', 'index') / med(c, 'upstream', 'index')
            cls, txt = 'tie', f'{ratio:.2f}×'
        else:
            ratio = med(c, 'upstream', 'sync') / med(c, 'fork', 'sync')
            cls, txt = 'good', f'{ratio:.2f}×'
        x1, x2 = px(1.0), px(min(max(ratio, lo), hi))
        svg.append(f'<rect x="{min(x1, x2):.1f}" y="{y - 8}" width="{abs(x2 - x1):.1f}" height="18" rx="3" class="{cls}"/>')
        anchor = 'start' if (kind == 'sync' or ratio >= 1) else 'end'
        off = 6 if anchor == 'start' else -6
        svg.append(f'<text x="{x2 + off:.1f}" y="{y + 4}" class="val {cls}" text-anchor="{anchor}">{txt}</text>')


panel(pad, 'Full index', 'index')
panel(pad * 2 + panel_w, 'One-file sync', 'sync')
svg.append(f'<text x="{pad}" y="{H - 72}" class="axis">Exact ratios; the tables mark which gaps clear the larger arm\'s run-to-run spread.</text>')
svg.append(f'<text x="{pad}" y="{H - 54}" class="axis">Method, spreads, graph sizes, admission receipts: docs/benchmarks/fork-vs-upstream-2026-10-31b.md</text>')
svg.append(f'<text x="{pad}" y="{H - 18}" class="axis">CodeGraph fork · bompus/codegraph · measured under the host benchmark lock</text>')
svg.append('</svg>')
open(args.out, 'w').write('\n'.join(svg))
print(args.out)
