import { it, expect, vi } from 'vitest';

type Request = { id: number; text: string; language: string };
type Child = { pid: number; killed: boolean; closed: boolean; sent: Request[]; finish(): void; emit(event: string, ...args: unknown[]): void };
const state = vi.hoisted(() => ({ children: [] as Child[], nextPid: 1 }));
vi.mock('fs', () => ({ existsSync: () => true }));
vi.mock('child_process', async () => {
  const { EventEmitter } = await import('events');
  class Child extends EventEmitter {
    pid = state.nextPid++;
    killed = false;
    closed = false;
    exitCode: number | null = null;
    signalCode: string | null = null;
    channel = { unref() {} };
    sent: Request[] = [];
    unref() {}
    ref() {}
    send(message: Request, callback: (error: Error | null) => void) { this.sent.push(message); callback(null); }
    kill() { this.killed = true; return true; }
    finish() { if (this.closed) return; this.closed = true; this.exitCode = 0; this.emit('exit', 0); this.emit('close', 0); }
  }
  return { fork: () => { const child = new Child(); state.children.push(child); return child; } };
});
import { tokenizeBounded, stopHighlightWorker } from '../src/ui-server/highlight/bounded-tokenize';

type Event = 'request' | 'reply' | 'timeout' | 'stop' | 'old-error' | 'old-close';
const events: Event[] = ['request', 'reply', 'timeout', 'stop', 'old-error', 'old-close'];
function sequences(prefix: Event[] = []): Event[][] {
  if (prefix.length === 4) return [prefix];
  return [prefix, ...events.flatMap((event) => sequences([...prefix, event]))];
}

it('isolates child generations and settles requests after every bounded event ordering', async () => {
  vi.useFakeTimers();
  const violations: string[] = [];
  try {
    for (const sequence of sequences()) {
      state.children.length = 0;
      const allowedKills = new Set<number>();
      const requests: Promise<void>[] = [];
      const stops: Promise<void>[] = [];
      let requestCount = 0;
      const allowCurrent = () => { const child = state.children.at(-1); if (child && !child.closed) allowedKills.add(child.pid); };
      for (const event of sequence) {
        if (event === 'request') {
          const label = `request-${++requestCount}`;
          requests.push(tokenizeBounded(label, 'typescript', 10).then((result) => {
            if ('result' in result && result.result && result.result.grammars[0] !== label) violations.push(`${sequence}: wrong reply for ${label}`);
          }));
        } else if (event === 'reply') {
          const child = state.children.at(-1);
          const message = child?.sent.at(-1);
          if (message) child!.emit('message', { id: message.id, result: { spans: [], grammars: [message.text] } });
        } else if (event === 'timeout') {
          allowCurrent();
          vi.advanceTimersByTime(10);
        } else if (event === 'stop') {
          allowCurrent();
          stops.push(stopHighlightWorker());
        } else {
          const old = state.children.find((child) => child.killed && !child.closed);
          if (event === 'old-error') old?.emit('error', new Error('old child'));
          else old?.finish();
        }
        await Promise.resolve();
        for (const child of state.children) if (child.killed && !allowedKills.has(child.pid)) violations.push(`${sequence}: stale event killed child ${child.pid}`);
      }
      allowCurrent();
      stops.push(stopHighlightWorker());
      for (const child of state.children) child.finish();
      await Promise.all([...requests, ...stops]);
      expect(vi.getTimerCount(), sequence.join(' → ')).toBe(0);
    }
    expect(violations).toEqual([]);
  } finally {
    for (const child of state.children) child.finish();
    await stopHighlightWorker();
    vi.useRealTimers();
  }
});


it.each(['timeout', 'stop'] as const)('keeps a replacement child alive after %s then a stale error', async (event) => {
  vi.useFakeTimers();
  try {
    const first = tokenizeBounded('first', 'typescript', 10);
    const old = state.children.at(-1)!;
    const stopping = event === 'stop' ? stopHighlightWorker() : Promise.resolve();
    if (event === 'timeout') vi.advanceTimersByTime(10);
    const second = tokenizeBounded('second', 'typescript', 10);
    const replacement = state.children.at(-1)!;
    old.emit('error', new Error('stale'));
    expect(replacement.killed).toBe(false);
    old.finish();
    await Promise.all([first, stopping]);
    const message = replacement.sent[0]!;
    replacement.emit('message', { id: message.id, result: { spans: [], grammars: ['second'] } });
    expect(await second).toEqual({ result: { spans: [], grammars: ['second'] } });
    const finalStop = stopHighlightWorker();
    replacement.finish();
    await finalStop;
  } finally {
    const cleanup = stopHighlightWorker();
    for (const child of state.children) child.finish();
    await cleanup;
    vi.useRealTimers();
  }
});
