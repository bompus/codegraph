import { afterEach, expect, it, vi } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { CodeGraph } from '../src';
import { FileWatcher, __emitWatchEventForTests } from '../src/sync/watcher';

let root: string;
let graph: CodeGraph | undefined;
let watcher: FileWatcher | undefined;
afterEach(() => {
  watcher?.stop(); graph?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
  vi.useRealTimers();
});
function write(file: string, text: string) {
  fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
  fs.writeFileSync(path.join(root, file), text);
}
async function emit(file: string) {
  __emitWatchEventForTests(root, file);
  await vi.advanceTimersByTimeAsync(100);
}

it('reconciles JSON classification when the last marker disappears or a schema is created', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-shopify-marker-'));
  for (const theme of ['a', 'b']) {
    write(`${theme}/layout/theme.liquid`, '{{ content_for_layout }}');
    write(`${theme}/templates/index.json`, '{"sections": {}}');
    write(`${theme}/sections/header.json`, '{"sections": {}}');
  }
  write('unmarked/templates/index.json', '{"sections": {}}');
  graph = await CodeGraph.init(root, { index: true });
  const calls: (string[] | undefined)[] = [];
  let complete!: () => void;
  watcher = new FileWatcher(root, async paths => {
    calls.push(paths);
    const result = await graph!.sync(paths ? { paths } : undefined);
    return { filesChanged: result.filesAdded + result.filesModified + result.filesRemoved, durationMs: 0 };
  }, { inertForTests: true, debounceMs: 10, onSyncComplete: () => complete() });
  const changed = async (file: string) => {
    const done = new Promise<void>(resolve => { complete = resolve; });
    await emit(file);
    await done;
  };
  watcher.start(); await watcher.waitUntilReady();
  vi.useFakeTimers();
  const json = () => graph!.getNodesByKind('file').map(n => n.filePath).filter(p => p.endsWith('.json')).sort();
  fs.unlinkSync(path.join(root, 'a/layout/theme.liquid'));
  await changed('a/layout/theme.liquid');
  expect(calls).toEqual([undefined]);
  expect(json()).toEqual(['b/sections/header.json', 'b/templates/index.json']);
  write('a/config/settings_schema.json', '[]');
  await changed('a/config/settings_schema.json');
  expect(calls).toEqual([undefined, undefined]);
  expect(json()).toEqual(['a/sections/header.json', 'a/templates/index.json', 'b/sections/header.json', 'b/templates/index.json']);
  write('a/layout/theme.liquid', '{{ content_for_layout }}');
  await changed('a/layout/theme.liquid');
  fs.unlinkSync(path.join(root, 'a/layout/theme.liquid'));
  await changed('a/layout/theme.liquid');
  expect(json()).toEqual(['a/sections/header.json', 'a/templates/index.json', 'b/sections/header.json', 'b/templates/index.json']);
  fs.unlinkSync(path.join(root, 'a/config/settings_schema.json'));
  await changed('a/config/settings_schema.json');
  expect(json()).toEqual(['b/sections/header.json', 'b/templates/index.json']);
  expect(calls).toEqual([undefined, undefined, undefined, undefined, undefined]);
});

it('preserves a marker event that arrives during an ordinary scoped sync', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-shopify-marker-'));
  write('src/a.ts', 'export const a = 1;');
  let release!: () => void;
  const first = new Promise<void>(resolve => { release = resolve; });
  const calls: (string[] | undefined)[] = [];
  watcher = new FileWatcher(root, async paths => {
    calls.push(paths);
    if (calls.length === 1) await first;
    return { filesChanged: 0, durationMs: 0 };
  }, { inertForTests: true, debounceMs: 10 });
  watcher.start(); await watcher.waitUntilReady(); vi.useFakeTimers();
  try {
    await emit('src/a.ts');
    expect(calls).toEqual([['src/a.ts']]);
    write('theme/config/settings_schema.json', '[]');
    __emitWatchEventForTests(root, 'theme/config/settings_schema.json');
    release(); await vi.advanceTimersByTimeAsync(100);
    expect(calls).toEqual([['src/a.ts'], undefined]);
  } finally { release(); }
});

it.each(['lock contention', 'sync failure'])('retries marker reconciliation after %s', async failure => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-shopify-marker-'));
  write('theme/config/settings_schema.json', '[]');
  const { LockUnavailableError } = await import('../src/sync/watcher');
  const sync = vi.fn().mockRejectedValueOnce(failure === 'lock contention' ? new LockUnavailableError() : new Error('injected')).mockResolvedValue({ filesChanged: 0, durationMs: 0 });
  watcher = new FileWatcher(root, sync, { inertForTests: true, debounceMs: 10 });
  watcher.start(); await watcher.waitUntilReady(); vi.useFakeTimers();
  __emitWatchEventForTests(root, 'theme/config/settings_schema.json');
  await vi.advanceTimersByTimeAsync(1000);
  expect(sync.mock.calls.map(call => call[0])).toEqual([undefined, undefined]);
});

it('keeps unchanged marker events and ordinary files out of full reconciliation', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-shopify-marker-'));
  write('layout/theme.liquid', '{{ content_for_layout }}');
  write('settings_schema.json', '[]');
  write('config/settings_schema.json.bak', '[]');
  write('sections/header.liquid', '{{ title }}');
  const sync = vi.fn().mockResolvedValue({ filesChanged: 0, durationMs: 0 });
  watcher = new FileWatcher(root, sync, { inertForTests: true, debounceMs: 10 }, file => file === 'layout/theme.liquid');
  watcher.start(); await watcher.waitUntilReady(); vi.useFakeTimers();
  await emit('layout/theme.liquid');
  await emit('settings_schema.json');
  await emit('config/settings_schema.json.bak');
  expect(sync).not.toHaveBeenCalled();
  await emit('sections/header.liquid');
  expect(sync.mock.calls.map(call => call[0])).toEqual([['sections/header.liquid']]);
});
