/** Native highlighting runs in a child that a deadline can kill and reap. */
import { existsSync } from 'fs';
import * as path from 'path';
import { fork, type ChildProcess } from 'child_process';
import type { TokenizeResult } from '../../extraction/syntax-tokens';
import type { Language } from '../../types';

export const TOKENIZE_DEADLINE_MS = 2000;
export type BoundedTokenize = { result: TokenizeResult | null } | { timedOut: true };
interface Pending { resolve: (outcome: BoundedTokenize) => void; timer: NodeJS.Timeout; }
let child: ChildProcess | null = null;
let nextId = 1;
const pending = new Map<number, Pending>();

function childFile(): string | null {
  const sibling = path.join(__dirname, 'tokenize-worker.js');
  if (existsSync(sibling)) return sibling;
  const built = path.resolve(__dirname, '../../../dist/ui-server/highlight/tokenize-worker.js');
  return existsSync(built) ? built : null;
}

/** Clear queued requests, then resolve them only after the captured child has exited. */
function retire(outcome: BoundedTokenize): Promise<void> {
  const dead = child;
  child = null;
  const waiting = [...pending.values()];
  pending.clear();
  for (const p of waiting) clearTimeout(p.timer);
  const stopped = !dead || dead.exitCode !== null || dead.signalCode !== null ? Promise.resolve()
    : new Promise<void>((resolve) => { dead.ref(); dead.once('close', () => resolve()); dead.kill('SIGKILL'); });
  return stopped.then(() => { for (const p of waiting) p.resolve(outcome); });
}

function ensureChild(file: string): ChildProcess {
  if (child) return child;
  const c = fork(file, [], { stdio: ['ignore', 'ignore', 'ignore', 'ipc'], execArgv: [] });
  c.unref();
  c.channel?.unref();
  c.on('message', (msg: { id: number; result: TokenizeResult | null }) => {
    if (child !== c) return;
    const p = pending.get(msg.id);
    if (!p) return;
    clearTimeout(p.timer);
    pending.delete(msg.id);
    p.resolve({ result: msg.result });
  });
  c.on('error', () => { if (child === c) void retire({ result: null }); });
  c.on('exit', () => { if (child === c) void retire({ result: null }); });
  child = c;
  return c;
}

export async function tokenizeBounded(text: string, language: Language, deadlineMs = TOKENIZE_DEADLINE_MS): Promise<BoundedTokenize> {
  const file = childFile();
  if (!file) return { result: null };
  let c: ChildProcess;
  try { c = ensureChild(file); } catch { return { result: null }; }
  const id = nextId++;
  return new Promise<BoundedTokenize>((resolve) => {
    const timer = setTimeout(() => { if (child === c) void retire({ timedOut: true }); }, deadlineMs);
    pending.set(id, { resolve, timer });
    c.send({ id, text, language }, (error) => { if (error && child === c) void retire({ result: null }); });
  });
}

export function stopHighlightWorker(): Promise<void> { return retire({ result: null }); }
process.once('exit', () => { child?.kill('SIGKILL'); });
