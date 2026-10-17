# Starting the resolver pool part-way through a mid-size index (2026-10-15)

Full index of five mid-size projects is 11–48% faster when the resolver pool starts mid-run, at 56–132% more peak memory. A project that never reaches the start condition is unchanged. The graph (every node and edge column except timestamps and edge ids) is identical in every run.

## What changed

The resolver pool used to start only when the index began with at least 150,000 pending references. Below that the whole resolve ran on the main thread. Now, after each main-thread batch, the loop projects the time the main thread would still need (measured kernel resolve milliseconds per reference, times the references left) and boots the pool when at least 20,000 references remain and the projection is at least 400 ms (`shouldEngageAdaptively` in `src/resolution/resolver-pool.ts`). The pool needs a WAL-mode database (its workers read the live file), so the start switches a rollback-journal database to WAL first and declines if that fails. An explicit `CODEGRAPH_PARALLEL_RESOLVE_MIN` turns the projection off. The pool keeps its existing preflight, so it still declines on hosts with fewer than three cores or little memory. The two constants are upstream's and are untuned here.

## Method

Baseline `9f76edff`, candidate `d104ea71` plus the review fixes (WAL switch before the pool boots, explicit `CODEGRAPH_PARALLEL_RESOLVE_MIN` respected), built into two separate `dist` directories. Node 24.22.0, Linux, `codegraph init --yes` on a fresh `.cg-bench` directory per run, `CODEGRAPH_NO_DAEMON=1`, host lock held, load under 2 before each run, run under `systemd-run --scope -p MemoryMax=16G`. Three rounds per corpus, order alternating each round (baseline first, candidate first, baseline first). There is no separate A/A arm: the spread of the three baseline runs is the noise reference. Identity gate: md5 over every `nodes` row (all columns except `updated_at`), every `edges` row (all columns except the autoincrement `id`, sorted by value) and the `unresolved_refs` count per status; baseline and candidate hashes, and node and edge counts, are equal for every corpus and every round.

## Results

Medians of three runs. Wall time is `/usr/bin/time` elapsed; memory is maximum resident set size; CPU is user plus system time.

| Corpus (pending refs) | Wall, baseline → candidate | Wall benefit | Baseline range → candidate range (s) | Peak memory, baseline → candidate | CPU, baseline → candidate |
| --- | --- | --- | --- | --- | --- |
| Alamofire (22,684) | 1.65 s → 1.41 s | −15% | 1.64–1.72 → 1.37–1.44 | 660 → 1,056 MiB (+60%) | 3.4 → 4.8 s |
| bloc (37,140) | 2.57 s → 2.30 s | −11% | 2.41–2.59 → 2.29–2.39 | 682 → 1,064 MiB (+56%) | 5.2 → 6.9 s |
| koel (84,654) | 4.93 s → 4.11 s | −17% | 4.77–5.08 → 4.08–4.11 | 789 → 1,498 MiB (+90%) | 9.6 → 12.4 s |
| paperless-ngx (102,380) | 5.37 s → 3.95 s | −26% | 5.31–5.49 → 3.91–3.98 | 848 → 1,596 MiB (+88%) | 9.1 → 12.6 s |
| JetBrains Exposed (110,854) | 18.50 s → 9.55 s | −48% | 18.49–19.39 → 9.51–9.71 | 1,077 → 2,497 MiB (+132%) | 23.2 → 35.8 s |
| gin (13,892, control) | 0.97 s → 0.97 s | no clear change | 0.93–0.98 → 0.93–1.06 | 404 → 401 MiB | 1.5 → 1.5 s |

In the five engaged rows the candidate's range does not overlap the baseline's. gin never reaches the 20,000-reference floor, so the pool does not start and its two arms are the same code path.

## Limits

- Three runs per arm per corpus on one host; the ranges above are the evidence of run-to-run spread.
- The memory cost is real and large in relative terms. It is the same trade the 150,000-reference default already makes.
- A 400 ms bar projected from the first batches is close to the edge on Alamofire (about 500 ms projected). The floor and bar are untuned; a different value could move which corpora engage.
- pretix and projects between 111,000 and 150,000 references were not re-measured with this change.
- A fresh index (fast-init) below the 150,000-reference gate that starts the pool mid-run has no checkpoint valve; an index that already has a WAL-mode database creates the valve before indexing, and a fresh one creates it at the gate. One instrumented Exposed run, the largest engaged corpus, sampled the write-ahead log every 0.1 s: peak 9.5 MB against a 127.7 MB database (7%), pool started once. Other corpora were not sampled. A later change turns automatic checkpoints off for the whole resolution phase of a fresh index and arms the valve before resolution starts, so a pool that starts mid-run no longer checkpoints inside the batch commits; the write-ahead log then grows until the valve folds it instead (up to 492 MiB on ktor, 328 MiB on Exposed).
- Single host, Linux only. The pool's own preflight is unchanged, so hosts it declines are unaffected.
