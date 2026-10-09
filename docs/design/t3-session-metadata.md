# Offline T3 session metadata prototype

This source prototype associates explicit T3 thread metadata with existing Codex search hits. It has no CLI, MCP, database reader or automatic collection path. It is disabled unless its caller supplies `enabled: true`.

Provider hits remain the source of searchable conversation text. The function returns the original hits unchanged, with separate annotations keyed by hit position. Canonical session keys, native titles, snippets, ranking and scores stay unchanged.

The caller supplies a normalized JSON snapshot and selects its source instance and project. Both must match the snapshot. A strong Codex native reference must match a recognized rollout filename and its canonical session key. Titles, paths and text similarity cannot establish a join. Claude, Cursor, custom drivers and uncertain references remain unsupported by this prototype.

Each annotation includes the T3 thread ID, its plain-text title, a link and an eligible parent thread ID. Multiple T3 aliases for one native session are retained. Foreign-project titles and IDs are omitted. Archived or deleted threads lose annotations; provider conversations remain available.

Every call recomputes annotations from the supplied snapshot. There is no cache or file access. Missing, malformed or mismatched snapshots return an unavailable status with the provider hits intact. Partial snapshots are marked partial and cannot imply source deletion.

Snapshots use version 1 with exact allowlisted fields. Limits are 1 MiB of UTF-8 JSON, 1,000 threads, 64 provider references per thread and 512 UTF-16 code units per title. Unknown fields, conflicting identities, control characters and unsafe IDs are rejected. The snapshot must contain no conversation bodies, raw provider payloads or credentials.

The caller must establish the project and source-instance mapping before constructing this normalized snapshot. The prototype does not infer ownership from working directories or app titles. A synthetic snapshot looks like this:

```json
{
  "version": 1,
  "sourceInstance": "fixture-source",
  "projectId": "project-one",
  "revision": "revision-one",
  "complete": true,
  "threads": [{
    "id": "app-thread-one",
    "title": "App title",
    "projectId": "project-one",
    "parentThreadId": null,
    "archived": false,
    "deleted": false,
    "providerThreads": [{
      "id": "provider-thread-one",
      "driver": "codex",
      "nativeId": "11111111-1111-4111-8111-111111111111",
      "strength": "strong"
    }]
  }]
}
```

Identifiers accept ASCII letters, digits and `_`, `.`, `:` or `-`, with an alphanumeric first character and a maximum length of 256 characters. Codex native IDs require lowercase UUID notation. A provider reference's `strength` is `strong`, `weak` or `none`; it is a caller assertion, not authentication. `revision` identifies the snapshot but does not create a cache or establish freshness.

This prototype does not prove that T3 can export this normalized format. A supported metadata export, broader provider identity contracts, rendering and live activation require separate design and selection. No retrieval benefit, index-size reduction or performance gain has been measured.
