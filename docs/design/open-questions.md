# Open questions

Things this fork has measured but cannot yet explain. The README links each
one from the sentence that states it, and `__tests__/readme-sync.test.ts` fails
when a README sentence says a cause is unknown without linking an entry here, or
when an entry here is no longer linked from the README. Remove the entry in the
same change that finds the cause.

## Slower full index on small projects

On Node, a full index of gin and Alamofire takes 0.14–0.43 s longer than
upstream's. The runs are in
[fork-vs-upstream-node-bun-2026-10-03.md](../benchmarks/fork-vs-upstream-node-bun-2026-10-03.md).
The cause has not been found.

What would close it: a profile of the fixed per-index costs on a small project
(startup, schema setup, the maintenance step) for the fork and for upstream on
the same host, since the gap does not grow with project size.
