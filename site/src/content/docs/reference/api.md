---
title: API
description: Use CodeGraph as a TypeScript library.
---

CodeGraph ships a TypeScript API. The package exports the `CodeGraph` class, storage primitives and installer runtime control.

```typescript
import CodeGraph from '@colbymchenry/codegraph';

const cg = await CodeGraph.init('/path/to/project');
// Or open an existing index:
// const cg = await CodeGraph.open('/path/to/project');

await cg.indexAll({
  onProgress: (p) => console.log(`${p.phase}: ${p.current}/${p.total}`),
});

const results = cg.searchNodes('UserService');
const callers = cg.getCallers(results[0].node.id);
const context = await cg.buildContext('fix login bug', {
  maxNodes: 20,
  includeCode: true,
  format: 'markdown',
});
const impact = cg.getImpactRadius(results[0].node.id, 2);

cg.watch();   // auto-sync on file changes
cg.unwatch(); // stop watching
cg.close();
```

## Key methods

| Method | Purpose |
|---|---|
| `CodeGraph.init(path)` / `CodeGraph.open(path)` | Create or open a project index |
| `indexAll(opts)` | Full index, with progress callback |
| `sync()` | Incremental update |
| `getIndexHealth()` | Sorted project-relative `needsReindex` and `parseErrors` paths |
| `searchNodes(query)` | Full-text symbol search |
| `getCallers(id)` / `getCallees(id)` | Walk the call graph |
| `getImpactRadius(id, depth)` | Transitive impact of a change |
| `buildContext(task, opts)` | Markdown / JSON context for AI |
| `watch()` / `unwatch()` | Start / stop the file watcher |
| `close()` | Close the database connection |

CommonJS works too — `const { CodeGraph } = require('@colbymchenry/codegraph');`.

## Lower-level building blocks

The same entry point exports primitives for callers that drive the graph directly rather than through the `CodeGraph` facade: `DatabaseConnection`, `QueryBuilder`, `getDatabasePath`, `initGrammars` / `loadGrammarsForLanguages`, and `FileLock`.

```typescript
import {
  CodeGraph,
  DatabaseConnection,
  QueryBuilder,
  getDatabasePath,
  initGrammars,
  loadGrammarsForLanguages,
  FileLock,
} from '@colbymchenry/codegraph';
```

## Embedding requirements

- **Install from npm** (`npm i @colbymchenry/codegraph`) so the matching per-platform package — which carries the compiled library — is fetched alongside the shim.
- The API runs on **your** runtime, so it needs **Node 22.13+** or **Bun 1.4.0+** for the built-in `node:sqlite` module (an Electron main process qualifies when its bundled Node is 22.13+). The CLI and MCP server are unaffected — they ship with a self-contained bundled runtime and need no Node at all.
- TypeScript types ship with the package. Keep `@types/node` available and `skipLibCheck: true` (the common default).

## Installer runtime control

Installers may load the supported `dist/runtime-control.js` module from a
validated build. The same functions are exported from the package entry point.
`RUNTIME_CONTROL_PROTOCOL` is `1`.

| Function | Result |
|---|---|
| `getRuntimeIdentity(root, options?)` | Hello-verified `{pid, version}`, or `null`; `{timeoutMs, requireWatcher}` can require the exact active watcher in the hello |
| `stopRuntime(root, options?)` | `{root, pid, outcome, version?}` with verified signalling |
| `reserveRuntimeWriter(root, holderPid)` | Opaque JSON reservation for a live updater, or `null` when the slot is occupied |
| `claimRuntimeWriter(root, reservation)` | `true` when this bootstrap process claims that exact reservation |
| `releaseRuntimeWriter(root, reservation)` | `true` when the reservation is gone; preserves successor ownership |
| `checkRuntimeReady(root, identity, timeoutMs?, options?)` | The expected identity after hello, MCP initialization and status verification; `{requireWatcher: true}` requires the exact active watcher and writer ownership; adding `refresh: true` waits for a daemon-owned fresh reconciliation |
| `startRuntimeWatcher(root, options)` | `{pid, version, projectRoot, watching: true}` after guarded initialization or reuse, fresh reconciliation and active-watcher verification |

Ordinary startup accepts `{expectedVersion, cliPath, runtimePath?, timeoutMs?}`.
The caller supplies an absolute CLI path from the validated coordination build;
`runtimePath` defaults to the calling runtime, and `timeoutMs` defaults to 120000.
The module and selected CLI must match `expectedVersion` before initialization.
The elected daemon acquires writer ownership before creating a missing index at
the exact canonical root. No ancestor or child index is adopted.
The operation never stops an existing daemon, claims a promotion reservation or
starts a fallback watcher. Concurrent starters use existing daemon election and
writer guards; they reuse a verified winner or refuse with preserved ownership.
Legacy, uncertain, wrong-build, direct and promotion owners block startup.
Disabled, failed and initializing watchers cannot return ready. Every successful
startup waits for a fresh reconciliation, including when it reuses a watcher.
File indexing and queued synthesized edges must complete before acknowledgment.
The daemon and caller both reject changed ownership records. A reconciliation
failure, unsupported refresh protocol or timeout fails startup without stopping
the shared daemon. Complete reconciliation also refuses
`CODEGRAPH_SYNC_RESYNTHESIS=0`. These guards do not fence legacy CLI initialization that
ignores writer ownership.

`serve --mcp --path <root> --preserve-existing` applies the same preservation
policy on every connection attempt, including reconnect and detached election.
It may serve fallback reads without auto-sync; this is not watcher readiness.
`new MCPServer(root, {preserveExisting: true})` selects this policy for library
clients. Adding `--initialize-index`, or `{initializeIndex: true}` to this
constructor, permits guarded initialization and requires preservation plus an
explicit root. Ordinary startup launches its detached candidate through this
path. A timeout leaves a possibly shared elected daemon alone; its usual
client/idle lifecycle retires an unused daemon. Installation, immutable artifact
selection and service limits remain the caller's responsibility.

Only `term`, `kill`, `not-running` and `no-daemon` stop outcomes allow a
reservation attempt. `unverified`, `still-running` and `legacy-writer` require
standing down. A successful stop does not itself reserve the slot. Reserve
before moving artifacts, and stand down if another writer has claimed it.

By default, stop preserves a live daemon without writer coordination protocol
`1`. Set `{legacyQuiescent: true}` only after confirming all legacy MCP sessions
and direct or fallback writers for that project are stopped. Daemon death alone
does not establish that quiescence. A restored legacy daemon needs the same
explicit cutover before another promotion.

Keep the reservation value unchanged. Its generation distinguishes successive
reservations held by one PID. Claim it in the daemon bootstrap, then run the
selected CLI entry in that same process. Spawning another CLI child after claim
would hand ownership to the wrong PID. The bootstrap must use the validated
coordination module even when rollback selects an older CLI.

Keep `coordinationDir` separate from `executableDir`. Retain the coordination
build and its native kernel until promotion or rollback has finished. Host
process launch, service limits, artifact swaps and rollback policy remain with
the installer. Never reconstruct writer-file mutations in the adapter.

Release is idempotent and preserves a successor. A `false` result means release
could not complete; retry within a bounded deadline and report failure rather
than claiming cleanup succeeded. For example, retry every 100 ms for five
seconds. Do not move artifacts or start another replacement after failed claim,
unverified identity or failed readiness. Readiness rejects a changed build,
failed status, expected-condition guidance, closed socket or timeout; its default timeout is 120 seconds.
