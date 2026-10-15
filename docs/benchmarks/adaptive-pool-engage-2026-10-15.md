# Starting the resolver pool part-way through a mid-size index (2026-10-15)

Full index of five mid-size projects is 11–48% faster when the resolver pool starts mid-run, at 49–129% more peak memory. A project that never reaches the start condition is unchanged. The graph is identical in every run.

## What changed

The resolver pool used to start only when the index began with at least 150,000 pending references. Below that the whole resolve ran on the main thread. Now, after each main-thread batch, the loop projects the time the main thread would still need (measured kernel resolve milliseconds per reference, times the references left) and boots the pool when at least 20,000 references remain and the projection is at least 400 ms (`shouldEngageAdaptively` in `src/resolution/resolver-pool.ts`). The pool keeps its existing preflight, so it still declines on hosts with fewer than three cores or little memory. The two constants are upstream's and are untuned here.

## Method

Baseline `9f76edff`, candidate the same tree plus this change, built into two separate `dist` directories. Node 24.22.0, Linux, `codegraph init --yes` on a fresh `.cg-bench` directory per run, `CODEGRAPH_NO_DAEMON=1`, host lock held, load under 2 before each run, run under `systemd-run --scope -p MemoryMax=16G`. Three rounds per corpus, order alternating each round (baseline first, candidate first, baseline first). There is no separate A/A arm: the spread of the three baseline runs is the noise reference. Identity gate: md5 of the sorted `source,target,kind,line` rows of the `edges` table; baseline and candidate hashes are equal for every corpus and every round.

## Results

Medians of three runs. Wall time is `/usr/bin/time` elapsed; memory is maximum resident set size; CPU is user plus system time.

| Corpus (pending refs) | Wall, baseline → candidate | Wall benefit | Baseline range → candidate range (s) | Peak memory, baseline → candidate | CPU, baseline → candidate |
| --- | --- | --- | --- | --- | --- |
| Alamofire (22,684) | 1.66 s → 1.42 s | −14% | 1.61–1.91 → 1.34–1.51 | 716 → 1,065 MiB (+49%) | 3.4 → 5.0 s |
| bloc (37,140) | 2.40 s → 2.13 s | −11% | 2.37–2.40 → 2.10–2.14 | 695 → 1,070 MiB (+54%) | 5.0 → 6.8 s |
| koel (84,654) | 4.85 s → 3.60 s | −26% | 4.85–4.91 → 3.48–4.09 | 791 → 1,455 MiB (+84%) | 9.7 → 12.1 s |
| paperless-ngx (102,380) | 5.10 s → 3.41 s | −33% | 5.09–5.14 → 3.39–3.50 | 881 → 1,613 MiB (+83%) | 8.8 → 11.9 s |
| JetBrains Exposed (110,854) | 18.71 s → 9.69 s | −48% | 18.51–22.00 → 9.32–9.70 | 1,089 → 2,493 MiB (+129%) | 23.3 → 36.2 s |
| gin (13,892, control) | 0.99 s → 0.96 s | no clear change | 0.93–1.01 → 0.95–0.97 | 408 → 409 MiB | 1.5 → 1.5 s |

In the five engaged rows the candidate's range does not overlap the baseline's. gin never reaches the 20,000-reference floor, so the pool does not start and its two arms are the same code path.

## Limits

- Three runs per arm per corpus on one host; the ranges above are the evidence of run-to-run spread.
- The memory cost is real and large in relative terms. It is the same trade the 150,000-reference default already makes.
- A 400 ms bar projected from the first batches is close to the edge on Alamofire (about 500 ms projected). The floor and bar are untuned; a different value could move which corpora engage.
- pretix and projects between 111,000 and 150,000 references were not re-measured with this change.
- Single host, Linux only. The pool's own preflight is unchanged, so hosts it declines are unaffected.
