Status: 5/5 — complete; PR #2 targets fork/consolidated

- [x] 1 Update fork/consolidated — 0c4664a; upstream main and latest integrated PR heads included; only Markdown language docs change the tree.
- [x] 2 Add static HTTP routing for Hono, Elysia, Fastify, Hyper-Express, Koa, H3, Bun, Effect v4, Vixeny — 33 focused extraction and end-to-end tests pass; TypeScript passes.
- [x] 3 Repair Nuxt default file routes and verify existing Next Pages support — 115 focused HTTP, Nuxt, Next and Vue tests pass; build passes.
- [x] 4 Validate real examples, unaffected controls, full suite, build, and update coverage/docs — build passes; full suite with matching native kernel: 4,342 pass, 46 skip; 116 focused routing tests pass; six official-source fixtures match their documented scope.
- [x] 5 Review and open PR against fork/consolidated — findings fixed and rechecked; implementation 0684cc0; [PR #2](https://github.com/bompus/codegraph/pull/2) base/head verified, mergeable.

Acceptance: literal routes produce method-qualified endpoint nodes and correct handler references; unrelated methods, shadowed bindings, and computed paths produce no fabricated routes. Preserve existing Express and Next behavior. No new dependencies, execution of application code, or arbitrary dynamic path evaluation.

New file justification: `src/resolution/frameworks/http-routing.ts` owns import-aware HTTP declarations across these frameworks. `express.ts` implements Express-specific text matching and middleware/mount rules; extending it would conflate incompatible APIs. Use the existing tree-sitter parser, not another parser or scanner. Merge back only if the existing Express implementation adopts the same binding-aware extraction. Dedicated tests belong in `__tests__/http-routing.test.ts`.
