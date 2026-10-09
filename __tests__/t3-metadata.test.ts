import { describe, expect, it } from 'vitest';
import { decorateT3SessionHits } from '../src/sessions/t3-metadata';
import type { SessionHit } from '../src/sessions/index';

const NATIVE = '11111111-1111-4111-8111-111111111111';
const OTHER = '22222222-2222-4222-8222-222222222222';
const FILE = `rollout-2026-01-02T03-04-05-${NATIVE}.jsonl`;
const scope = { enabled: true, sourceInstance: 'fixture-source', projectId: 'project-one' };
const hit: SessionHit = { session: `codex:${FILE.slice(0, -6)}`, title: 'Provider title',
  role: 'assistant', ts: '2026-01-02T03:04:05Z', snippet: 'A retained provider decision',
  file: `/fixtures/codex/${FILE}`, score: -2.5 };
const provider = { id: 'provider-thread-one', driver: 'codex', nativeId: NATIVE, strength: 'strong' };
const thread = { id: 'app-thread-one', title: 'App title', projectId: 'project-one',
  parentThreadId: null, archived: false, deleted: false, providerThreads: [provider] };

function snapshot(threads: unknown[] = [thread], extra = {}) {
  return { version: 1, sourceInstance: 'fixture-source', projectId: 'project-one',
    revision: 'revision-one', complete: true, threads, ...extra };
}

function run(value: unknown, hits: SessionHit[] = [hit]) {
  return decorateT3SessionHits(hits, JSON.stringify(value), scope);
}

describe('offline T3 metadata decorator', () => {
  it('is disabled without parsing supplied data and leaves provider hits unchanged', () => {
    const hits = [hit];
    const result = decorateT3SessionHits(hits, '{not-json', { ...scope, enabled: false });
    expect(result).toEqual({ hits, annotations: [], status: 'disabled', issues: [] });
    expect(result.hits).toBe(hits);
  });

  it('reports missing input as unavailable rather than an empty verified source', () => {
    const result = decorateT3SessionHits([hit], undefined, scope);
    expect(result.status).toBe('unavailable');
    expect(result.issues).toEqual([{ reason: 'snapshot-unavailable' }]);
    expect(result.hits).toEqual([hit]);
  });

  it('joins exact rollout identity and keeps the provider key, title, snippet, scores and order', () => {
    const hits = [hit, { ...hit, session: `codex:${NATIVE}`, score: -1, snippet: 'Another passage' }];
    const original = structuredClone(hits);
    const input = snapshot();
    const result = run(input, hits);
    expect(result.status).toBe('ready');
    expect(result.issues).toEqual([]);
    expect(result.hits).toBe(hits);
    expect(hits).toEqual(original);
    expect(result.annotations).toEqual([0, 1].map(hitIndex => ({ hitIndex, threads: [{
      threadId: 'app-thread-one', title: 'App title', parentThreadId: null,
      link: 't3-thread://v1/app-thread-one',
    }] })));
  });

  it('recognizes Windows separators without platform-dependent path resolution', () => {
    expect(run(snapshot(), [{ ...hit, file: `C:\\fixtures\\codex\\${FILE}` }]).annotations).toHaveLength(1);
  });

  it.each([
    { session: `codex:${OTHER}` }, { session: `claude:${NATIVE}` },
    { file: `/fixtures/${NATIVE}.jsonl` }, { file: `/fixtures/prefix-${FILE}` },
    { file: `/fixtures/${FILE}.backup` }, { file: `/fixtures/${FILE.replace(NATIVE, OTHER)}` },
  ])('does not join ambiguous or conflicting provider source identity %j', changes => {
    const result = run(snapshot(), [{ ...hit, ...changes }]);
    expect(result.annotations).toEqual([]);
    expect(result.issues).toEqual([{ reason: 'provider-hit-unmatched', threadId: thread.id }]);
  });

  it.each([
    ['weak', { strength: 'weak' }, 'native-reference-unverified'],
    ['none', { strength: 'none' }, 'native-reference-unverified'],
    ['missing', { nativeId: null }, 'native-reference-missing'],
    ['unsupported-native', { nativeId: 'not-a-native-uuid' }, 'native-reference-unsupported'],
    ['unsupported-driver', { driver: 'claudeAgent' }, 'unsupported-provider'],
    ['instance-alias', { driver: 'codex-custom' }, 'unsupported-provider'],
  ])('retains provider hits for %s references', (_name, changes, reason) => {
    const result = run(snapshot([{ ...thread, providerThreads: [{ ...provider, ...changes }] }]));
    expect(result.annotations).toEqual([]);
    expect(result.issues).toEqual([{ reason, threadId: thread.id }]);
    expect(result.hits).toEqual([hit]);
  });

  it('does not expose foreign-project titles or parent identifiers', () => {
    const result = run(snapshot([
      { ...thread, parentThreadId: 'foreign-thread' },
      { ...thread, id: 'foreign-thread', projectId: 'other-project', title: 'Foreign confidential title' },
    ]));
    expect(result.annotations[0]!.threads[0]!.parentThreadId).toBeNull();
    expect(result.issues).toEqual([{ reason: 'foreign-project' }]);
    expect(JSON.stringify(result)).not.toContain('Foreign confidential title');
    expect(JSON.stringify(result)).not.toContain('foreign-thread');
  });

  it.each([{ projectId: 'other-project' }, { sourceInstance: 'different-source' }])('rejects a snapshot from a different scope %j', changes => {
    const result = run(snapshot([thread], changes));
    expect(result.status).toBe('unavailable');
    expect(result.issues).toEqual([{ reason: 'snapshot-scope-mismatch' }]);
    expect(result.annotations).toEqual([]);
  });

  it('keeps multiple app aliases deterministically and includes only visible parent references', () => {
    const threads = [{ ...thread, id: 'app-z', parentThreadId: 'app-a' }, { ...thread, id: 'app-a' }];
    const result = run(snapshot(threads));
    expect(result.annotations[0]!.threads.map(t => t.threadId)).toEqual(['app-a', 'app-z']);
    expect(result.annotations[0]!.threads[1]!.parentThreadId).toBe('app-a');
    expect(run(snapshot([...threads].reverse())).annotations).toEqual(result.annotations);
  });

  it('does not duplicate an alias for repeated native references on the same app thread', () => {
    const result = run(snapshot([{ ...thread, providerThreads: [provider, { ...provider, id: 'provider-thread-two' }] }]));
    expect(result.annotations[0]!.threads).toHaveLength(1);
  });

  it('replaces annotations on rename, deletion, archival, empty snapshot and disable without erasing provider hits', () => {
    expect(run(snapshot([{ ...thread, title: 'Renamed title' }])).annotations[0]!.threads[0]!.title).toBe('Renamed title');
    for (const changes of [{ deleted: true }, { archived: true }]) {
      const result = run(snapshot([{ ...thread, ...changes }]));
      expect(result.annotations).toEqual([]);
      expect(result.hits).toEqual([hit]);
    }
    expect(run(snapshot([]))).toMatchObject({ status: 'ready', annotations: [], hits: [hit] });
    expect(decorateT3SessionHits([hit], JSON.stringify(snapshot()), { ...scope, enabled: false }).annotations).toEqual([]);
  });

  it('labels partial snapshots without claiming source absence or deleting provider-only records', () => {
    const providerOnly = { ...hit, session: 'claude:standalone' };
    const result = run(snapshot([thread], { complete: false }), [hit, providerOnly]);
    expect(result.status).toBe('partial');
    expect(result.annotations.map(a => a.hitIndex)).toEqual([0]);
    expect(result.hits).toEqual([hit, providerOnly]);
  });

  it('keeps titles as unrendered data and constructs links from validated IDs only', () => {
    const title = '<script>inert</script> [label](https://example.invalid)';
    const result = run(snapshot([{ ...thread, title }]));
    expect(result.annotations[0]!.threads[0]).toEqual({ threadId: thread.id, title, parentThreadId: null,
      link: `t3-thread://v1/${thread.id}` });
    expect(run(snapshot([{ ...thread, id: 'unsafe/id?query' }])).status).toBe('unavailable');
  });

  it.each([
    ['unknown-version', snapshot([thread], { version: 2 })],
    ['missing-completeness', { version: 1, sourceInstance: 'fixture-source', projectId: 'project-one', revision: 'revision-one', threads: [] }],
    ['body-field', snapshot([{ ...thread, text: 'Never retained' }])],
    ['credential-field', snapshot([thread], { token: 'synthetic-never-retained' })],
    ['raw-provider-payload', snapshot([{ ...thread, providerThreads: [{ ...provider, payload: 'Never retained' }] }])],
    ['duplicate-thread', snapshot([thread, thread])],
    ['duplicate-provider', snapshot([{ ...thread, providerThreads: [provider, provider] }])],
    ['conflicting-provider-identity', snapshot([thread, { ...thread, id: 'app-two', providerThreads: [{ ...provider, nativeId: OTHER }] }])],
    ['bad-flag', snapshot([{ ...thread, deleted: 'false' }])],
    ['invalid-strength', snapshot([{ ...thread, providerThreads: [{ ...provider, strength: 'guess' }] }])],
    ['oversized-title', snapshot([{ ...thread, title: 'x'.repeat(513) }])],
    ['title-control', snapshot([{ ...thread, title: 'unsafe\nheading' }])],
    ['too-many-threads', snapshot(Array.from({ length: 1001 }, (_, i) => ({ ...thread, id: `thread-${i}` })))],
    ['too-many-provider-refs', snapshot([{ ...thread, providerThreads: Array.from({ length: 65 }, (_, i) => ({ ...provider, id: `ref-${i}` })) }])],
  ])('rejects malformed or disallowed snapshot data for %s', (_name, input) => {
    const result = run(input);
    expect(result).toEqual({ hits: [hit], annotations: [], status: 'unavailable', issues: [{ reason: 'invalid-snapshot' }] });
    expect(JSON.stringify(result)).not.toContain('Never retained');
    expect(JSON.stringify(result)).not.toContain('synthetic-never-retained');
  });

  it('rejects invalid JSON and oversized input before exposing metadata', () => {
    const oversized = JSON.stringify(snapshot()) + ' '.repeat(1024 * 1024);
    expect(JSON.parse(oversized)).toEqual(snapshot());
    for (const json of ['{', oversized]) {
      expect(decorateT3SessionHits([hit], json, scope)).toEqual({ hits: [hit], annotations: [], status: 'unavailable', issues: [{ reason: 'invalid-snapshot' }] });
    }
  });
});
