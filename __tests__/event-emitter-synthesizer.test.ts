/**
 * EventEmitter string-keyed dispatch — the inline-listener arm.
 *
 * `.on('x', fn)` bridges emit → a NAMED handler node. `.on('x', (e) => {…})`,
 * `.on('x', e => …)` and `.on('x', function () {…})` have no handler symbol
 * (extraction deliberately attributes an inline closure's calls to its
 * enclosing function), so the subscription is attributed to that same
 * enclosing function — where the event's work demonstrably happens — or to
 * the smallest enclosing const/variable for an object-literal API site.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

const EMITTER_EDGES = `
  SELECT s.name source, s.id source_id, t.name target, t.id target_id,
         json_extract(e.metadata,'$.event') event
  FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
  WHERE json_extract(e.metadata,'$.synthesizedBy') = 'event-emitter'`;

describe('event-emitter synthesizer — inline listeners', () => {
  let dir: string;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'event-emitter-')); });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  it('bridges emit → the function enclosing an inline arrow listener', async () => {
    fs.writeFileSync(
      path.join(dir, 'bus.ts'),
      `import { EventEmitter } from 'events';
export const bus = new EventEmitter();
`
    );
    fs.writeFileSync(
      path.join(dir, 'billing.ts'),
      `import { bus } from './bus';
export function setupInvoiceListener() {
  bus.on('invoice.paid', (inv) => {
    console.log('paid', inv);
  });
}
export function setupOnce() {
  bus.once('invoice.paid', function () {
    console.log('first');
  });
}
export function setupAsync() {
  bus.on('invoice.paid', async (inv) => {
    console.log('async', inv);
  });
}
`
    );
    fs.writeFileSync(
      path.join(dir, 'checkout.ts'),
      `import { bus } from './bus';
export function completeCheckout() {
  bus.emit('invoice.paid', { id: 1 });
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db.prepare(EMITTER_EDGES).all();
    const pairs = edges.map((r: any) => `${r.source}->${r.target}`).sort();
    // Every inline-listener registration attributes to its enclosing fn; the
    // three registrations share one handler id per function, so the emit fans
    // out to each enclosing function once.
    expect(pairs).toEqual([
      'completeCheckout->setupAsync',
      'completeCheckout->setupInvoiceListener',
      'completeCheckout->setupOnce',
    ]);
    expect(edges.every((r: any) => r.event === 'invoice.paid')).toBe(true);
    cg.close?.();
  });

  it('attributes a method-shorthand registration to the enclosing method, a const-arrow one to the constant', async () => {
    fs.writeFileSync(
      path.join(dir, 'bus.ts'),
      `import { EventEmitter } from 'events';
export const bus = new EventEmitter();
`
    );
    fs.writeFileSync(
      path.join(dir, 'api.ts'),
      `import { bus } from './bus';
export const watcherApi = {
  start() {
    bus.on('tick', (d) => console.log(d));
  },
};
`
    );
    fs.writeFileSync(
      path.join(dir, 'tick.ts'),
      `import { bus } from './bus';
export function fireTick() {
  bus.emit('tick', 1);
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db.prepare(EMITTER_EDGES).all();
    // The inline listener sits inside the object literal's `start` method,
    // which extraction keeps as a node — the edge lands there, exactly where
    // the listener's own calls attribute.
    expect(edges.map((r: any) => `${r.source}->${r.target}`)).toEqual(['fireTick->start']);
    cg.close?.();
  });

  it('still prefers the named handler over the enclosing function', async () => {
    fs.writeFileSync(
      path.join(dir, 'bus.ts'),
      `import { EventEmitter } from 'events';
export const bus = new EventEmitter();
export function onSaved() { return 1; }
`
    );
    fs.writeFileSync(
      path.join(dir, 'reg.ts'),
      `import { bus, onSaved } from './bus';
export function register() {
  bus.on('doc.saved', onSaved);
}
`
    );
    fs.writeFileSync(
      path.join(dir, 'save.ts'),
      `import { bus } from './bus';
export function saveDoc() {
  bus.emit('doc.saved', {});
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db.prepare(EMITTER_EDGES).all();
    // The named handler resolves to `onSaved` itself — `register` (the
    // enclosing function of the `.on` call) must not be linked instead.
    expect(edges.map((r: any) => `${r.source}->${r.target}`)).toEqual(['saveDoc->onSaved']);
    cg.close?.();
  });

  it('emits nothing when no listener registers the emitted event', async () => {
    fs.writeFileSync(
      path.join(dir, 'a.ts'),
      `import { EventEmitter } from 'events';
const bus = new EventEmitter();
export function fire() {
  bus.emit('nothing.listens', 1);
}
export function unrelated() {
  bus.on('other.event', () => {});
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const c = db.prepare(`SELECT count(*) c FROM edges WHERE json_extract(metadata,'$.synthesizedBy') = 'event-emitter'`).get().c;
    expect(c).toBe(0);
    cg.close?.();
  });
});
