#!/usr/bin/env python3
"""usage: summarize.py <BENCH_OUT>. Medians per corpus and arm from index-sync.tsv."""
import collections, csv, statistics as st, sys, os

def sec(t):
    p = t.split(':')
    return float(p[-1]) + 60 * float(p[-2])

def mem(kb):
    return f"{kb/1024:.0f} MiB" if kb < 1024 * 1024 else f"{kb/1024/1024:.2f} GiB"

def size(b):
    return f"{b/2**20:.1f} MiB" if b < 2**30 else f"{b/2**30:.2f} GiB"

rows = list(csv.reader(open(os.path.join(sys.argv[1], 'index-sync.tsv')), delimiter='\t'))
d = collections.defaultdict(list)
corpora, arms = [], []
for r in rows:
    if r[0] not in corpora: corpora.append(r[0])
    if r[1] not in arms: arms.append(r[1])
    d[(r[0], r[1], 'index' if r[2] == 'index' else 'sync')].append(r)
print('corpus | arm | index s | index peak | db | nodes | edges | sync s | sync peak')
for c in corpora:
    for a in arms:
        i, s = d[(c, a, 'index')], d[(c, a, 'sync')]
        if not i: continue
        print(' | '.join([c, a, f"{st.median(sec(r[4]) for r in i):.2f}", mem(st.median(int(r[5]) for r in i)),
                          size(st.median(int(r[7]) for r in i)), i[0][8], i[0][9],
                          f"{st.median(sec(r[4]) for r in s):.2f}", mem(st.median(int(r[5]) for r in s))]))
