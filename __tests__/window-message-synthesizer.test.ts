import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

describe('window-message synthesizer', () => {
  let dir: string;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'window-message-')); });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  function synthEdges(cg: CodeGraph) {
    const db = (cg as any).db.db;
    return db.prepare(
      `SELECT s.name source, t.name target, json_extract(e.metadata,'$.event') event
       FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
       WHERE json_extract(e.metadata,'$.synthesizedBy') = 'window-message'`,
    ).all() as Array<{ source: string; target: string; event: string }>;
  }

  it('links a publisher to the listener that checks the same source discriminator', async () => {
    fs.writeFileSync(path.join(dir, 'constants.ts'),
      'export const MESSAGES = { TTD_AJAX_EVENT: "BOMPUS_TTD_AJAX", OTHER_EVENT: "BOMPUS_OTHER" };\n');
    fs.writeFileSync(path.join(dir, 'publish.ts'), `import { MESSAGES } from "./constants";
export function publishTtdLivewire() {
  window.postMessage({ source: MESSAGES.TTD_AJAX_EVENT, payload: {} }, "*");
}
export function publishOther() {
  window.postMessage({ source: MESSAGES.OTHER_EVENT, payload: {} }, "*");
}
`);
    fs.writeFileSync(path.join(dir, 'listen.ts'), `import { MESSAGES } from "./constants";
export function listenForTtdLivewireAjax() {
  window.addEventListener("message", (ev) => {
    if (ev.data.source !== MESSAGES.TTD_AJAX_EVENT) return;
    receiveTtdSnapshot(ev.data.payload);
  });
}
export function receiveTtdSnapshot(payload: unknown) { return payload; }
export function listenWithoutCheck() {
  window.addEventListener("message", (ev) => { console.log(ev.data); });
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const edges = synthEdges(cg);
    expect(edges).toEqual([
      expect.objectContaining({ source: 'publishTtdLivewire', target: 'listenForTtdLivewireAjax', event: 'TTD_AJAX_EVENT' }),
    ]);
    expect(edges.some((e) => e.source === 'publishOther')).toBe(false);
    expect(edges.some((e) => e.target === 'listenWithoutCheck')).toBe(false);
    expect(edges.some((e) => e.target === 'receiveTtdSnapshot')).toBe(false);
    cg.close?.();
  });

  it('does not pair a string discriminator with a different member name', async () => {
    fs.writeFileSync(path.join(dir, 'a.ts'), `export function send() {
  window.postMessage({ source: "BOMPUS_TTD_AJAX" }, "*");
}
`);
    fs.writeFileSync(path.join(dir, 'b.ts'), `export function listen() {
  window.addEventListener("message", (ev) => {
    if (ev.data.source !== OtherHub.TTD_OTHER_EVENT) return;
  });
}
`);
    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    expect(synthEdges(cg)).toEqual([]);
    cg.close?.();
  });
});
