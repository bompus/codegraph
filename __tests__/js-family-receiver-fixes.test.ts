/**
 * JS-family resolution fixes: an import from an unmapped workspace package,
 * Svelte/Astro member calls (including receivers and imports used in markup,
 * a `{...spread()}` call and rune-wrapped receivers), a re-export cycle's
 * default, and `T | null` fields reached through a typed receiver.
 */
import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function index(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-js-receiver-fixes-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

/** `file:qualifiedName@confidence` for each call out of `file`. */
function callsFrom(cg: CodeGraph, file: string): string[] {
  const ids = cg.getNodesInFile(file).map((n) => n.id);
  return cg
    .getOutgoingEdgesFrom(ids, ['calls'])
    .map((e) => {
      const t = cg.getNode(e.target)!;
      return `${t.filePath}:${t.qualifiedName}@${e.metadata?.confidence}`;
    })
    .sort();
}

describe('JS-family receiver and import fixes', () => {
  it('keeps a name guess for an unmapped workspace package below trusted confidence', async () => {
    // No root manifest and no workspace globs: `@app/shared` is local only by
    // the `workspace:` text, and nothing says which directory it is.
    const cg = await index({
      'frontend/package.json': JSON.stringify({ name: 'frontend', dependencies: { '@app/shared': 'workspace:*' } }),
      'frontend/src/use.ts': `import { formatUrl } from '@app/shared';
export function go() { return formatUrl('/x'); }
`,
      'docs/src/lib/format.ts': `export function formatUrl(u: string) { return u; }
`,
      // Control: a relative import is still the import's target.
      'frontend/src/local.ts': `export function slug(s: string) { return s; }
`,
      'frontend/src/use-local.ts': `import { slug } from './local';
export function go2() { return slug('x'); }
`,
    });
    try {
      expect(callsFrom(cg, 'frontend/src/use.ts')).toEqual(['docs/src/lib/format.ts:formatUrl@0.7']);
      expect(callsFrom(cg, 'frontend/src/use-local.ts')).toEqual(['frontend/src/local.ts:slug@0.9']);
    } finally {
      cg.close();
    }
  });

  it('takes Svelte and Astro member calls through the receiver binding, like TypeScript', async () => {
    const cg = await index({
      'src/stores.ts': `import { writable } from 'svelte/store';
export const count = writable(0);
`,
      'src/UserCache.ts': `export class UserCache { set(v: number) { return v; } }
`,
      'src/App.svelte': `<script lang="ts">
  import { count } from './stores';
  import { userCache } from 'some-lib';
  function inc() { count.set(1); userCache.set(2); }
</script>
<button on:click={inc}>x</button>
`,
      'src/page.astro': `---
import { count } from './stores';
import { userCache } from 'some-lib';
function bump() { count.set(2); userCache.set(3); }
bump();
---
<div>x</div>
`,
      // A receiver the component declares, called from markup or from a
      // second script, where no binding scope reaches.
      'src/Counter.svelte': `<script>
  class Counter {
    double() { return 2; }
    getCount() { return 1; }
  }
  const counter = new Counter();
</script>

<button on:click={() => counter.double()}>{counter.getCount()}</button>
`,
      'src/Logic.svelte': `<script module>
  class SomeLogic {
    trigger() { return 1; }
  }
  const someLogic = new SomeLogic();
</script>

<script>
  function increment() {
    someLogic.trigger();
  }
</script>

<button on:click={increment}>x</button>
`,
      // Control: the same calls from TypeScript.
      'src/app.ts': `import { count } from './stores';
import { userCache } from 'some-lib';
export function inc() { count.set(1); userCache.set(2); }
`,
    });
    try {
      // The imported store constant, not an unrelated class's `set`.
      expect(callsFrom(cg, 'src/App.svelte')).toEqual(['src/stores.ts:count@0.9']);
      expect(callsFrom(cg, 'src/page.astro')).toEqual(['src/page.astro:bump@0.9', 'src/stores.ts:count@0.9']);
      expect(callsFrom(cg, 'src/app.ts')).toEqual(['src/stores.ts:count@0.9']);
      expect(callsFrom(cg, 'src/Counter.svelte')).toEqual(['src/Counter.svelte:Counter::double@0.9', 'src/Counter.svelte:Counter::getCount@0.9']);
      expect(callsFrom(cg, 'src/Logic.svelte')).toEqual(['src/Logic.svelte:SomeLogic::trigger@0.8']);
    } finally {
      cg.close();
    }
  });

  it('takes a script-level import as the receiver in Svelte markup and a second script', async () => {
    const cg = await index({
      'src/spread.js': `export function spread() { return {}; }
`,
      'src/fmt.js': `export class Fmt { static money(v) { return v; } }
`,
      'src/stores.ts': `import { writable } from 'svelte/store';
export const count = writable(0);
`,
      'src/UserCache.ts': `export class UserCache { set(v: number) { return v; } }
`,
      // The Paraglide `{m.key()}` idiom, and a class import used in markup.
      'src/Markup.svelte': `<script>
  import * as m from './spread.js';
  import { Fmt } from './fmt.js';
</script>
<p>{m.spread()}</p>
<p>{Fmt.money(1)}</p>
`,
      // An import in `<script module>` used from the instance script.
      'src/Module.svelte': `<script module>
  import { Fmt } from './fmt.js';
</script>

<script>
  function show() {
    return Fmt.money(2);
  }
</script>
`,
      // A function-local `count` elsewhere doesn't hide the script's
      // import from markup: `count.set` is still a call on the imported
      // store constant, which it links to, not a guess at some class's `set`.
      'src/Shadow.svelte': `<script>
  import { count } from './stores';
  function other() {
    let count = 0;
    return count;
  }
</script>
<button on:click={() => count.set(1)}>x</button>
`,
    });
    try {
      expect({
        markup: callsFrom(cg, 'src/Markup.svelte'),
        module: callsFrom(cg, 'src/Module.svelte'),
        shadow: callsFrom(cg, 'src/Shadow.svelte'),
      }).toEqual({
        markup: ['src/fmt.js:Fmt::money@0.9', 'src/spread.js:spread@0.9'],
        module: ['src/fmt.js:Fmt::money@0.9'],
        shadow: ['src/stores.ts:count@0.9'],
      });
    } finally {
      cg.close();
    }
  });

  it('types a receiver wrapped in a Svelte rune', async () => {
    const cg = await index({
      'src/Runes.svelte': `<script>
  class Model {
    toggle() { return 1; }
  }
  let model = $state(new Model());
  let raw = $state.raw(new Model());
  const view = $derived(new Model());
  function tick() {
    model.toggle();
    raw.toggle();
    view.toggle();
  }
</script>
`,
      'src/box.svelte.ts': `export class Box {
  open() { return 1; }
}
export function useBox() {
  const box = $state(new Box());
  box.open();
}
`,
    });
    try {
      const targets = (file: string, name: string) => {
        const from = cg.getNodesInFile(file).find((n) => n.name === name)!;
        return cg
          .getOutgoingEdges(from.id)
          .filter((e) => e.kind === 'calls')
          .map((e) => `${cg.getNode(e.target)!.qualifiedName}@${e.metadata?.confidence}`);
      };
      expect({ tick: targets('src/Runes.svelte', 'tick'), useBox: targets('src/box.svelte.ts', 'useBox') }).toEqual({
        tick: ['Model::toggle@0.9', 'Model::toggle@0.9', 'Model::toggle@0.9'],
        useBox: ['Box::open@0.9'],
      });
    } finally {
      cg.close();
    }
  });

  it('reads a spread call as a plain call of its import', async () => {
    const cg = await index({
      'src/spread.js': `export function spread() { return {}; }
`,
      'other/spread.js': `export function spread() { return { other: true }; }
`,
      'src/Spread.svelte': `<script>
  import { spread } from './spread.js';
</script>
<p {...spread()}>x</p>
`,
      'src/use.ts': `import { spread } from './spread.js';
export function build() { return [...spread()]; }
`,
    });
    try {
      expect(callsFrom(cg, 'src/Spread.svelte')).toEqual(['src/spread.js:spread@0.9']);
      expect(callsFrom(cg, 'src/use.ts')).toEqual(['src/spread.js:spread@0.9']);
    } finally {
      cg.close();
    }
  });

  it('does not take a guessed default from a re-export cycle', async () => {
    // ESM rejects this graph: `Card` is `card.js`'s default, which is
    // `index.js`'s default again.
    const cg = await index({
      'cards/card.js': `export { default } from './index.js';
`,
      'cards/index.js': `export { default as Card } from './card.js';
export function real() { return 1; }
`,
      'use.js': `import { Card } from './cards/index.js';
export function draw() { return Card(); }
`,
      // Control: a default re-export that ends at a declaration.
      'panels/panel.js': `export default function Panel() { return 2; }
`,
      'panels/index.js': `export { default as Panel } from './panel.js';
`,
      'use-panel.js': `import { Panel } from './panels/index.js';
export function show() { return Panel(); }
`,
    });
    try {
      expect(callsFrom(cg, 'use.js')).toEqual([]);
      expect(callsFrom(cg, 'use-panel.js')).toEqual(['panels/panel.js:Panel@0.9']);
    } finally {
      cg.close();
    }
  });

  it('dereferences a `T | null` field on a typed receiver', async () => {
    const cg = await index({
      'conn.ts': `export class Conn { query() { return 1; } }
`,
      'other.ts': `export class Other { query() { return 2; } }
`,
      'svc.ts': `import { Conn } from './conn';
export class Service {
  conn: Conn | null = null;
  alt: Conn | undefined;
  many: Conn | Other;
}
import { Other } from './other';
`,
      'main.ts': `import { Service } from './svc';
export function viaParam(s: Service) { s.conn.query(); }
export function viaNew() { const s = new Service(); s.alt.query(); }
export function viaUnion(s: Service) { s.many.query(); }
`,
    });
    try {
      const node = (name: string) => cg.getNodesInFile('main.ts').find((n) => n.name === name)!;
      const targets = (name: string) =>
        cg.getOutgoingEdges(node(name).id).filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!.qualifiedName);
      expect(targets('viaParam')).toEqual(['Conn::query']);
      expect(targets('viaNew')).toEqual(['Conn::query']);
      // A union of two classes still names no single owner.
      expect(targets('viaUnion')).toEqual([]);
    } finally {
      cg.close();
    }
  });

  it('types a property written as a getter by its declared return type', async () => {
    // `c.req.header()` reaches Req.header, not Res.header, because `get req(): Req`
    // types `req` even though the constructor has an untyped-as-field `req: Request`
    // parameter. A getter with no return type says nothing.
    const cg = await index({
      'src/req.ts': `export class Req { header(name: string): string { return name; } }
`,
      'src/res.ts': `export class Res { header(name: string): string { return name; } }
`,
      'src/ctx.ts': `import { Req } from './req';
import { Res } from './res';
export class Ctx {
  #req: Req | undefined;
  // An object literal's getter inside a one-line method is not the class's.
  probe() { return { get req(): Res { return new Res(); } }; }
  // A plain constructor parameter named like the getter is a local, not the field.
  constructor(req: Request) {
    void req;
  }
  get req(): Req {
    this.#req ??= new Req();
    return this.#req;
  }
  get res() {
    return new Res();
  }
}
`,
      'src/use.ts': `import { Ctx } from './ctx';
export function fromReq(c: Ctx) { return c.req.header('x'); }
export function fromRes(c: Ctx) { return c.res.header('x'); }
`,
    });
    try {
      const calls = callsFrom(cg, 'src/use.ts');
      expect(calls.filter((c) => c.includes('src/req.ts:Req::header'))).toHaveLength(1);
      expect(calls.some((c) => c.includes('src/res.ts'))).toBe(false);
    } finally {
      cg.close();
    }
  });

  it('does not treat a plain constructor parameter as a field', async () => {
    const cg = await index({
      'src/req.ts': `export class Req { header(name: string): string { return name; } }
`,
      'src/plain.ts': `import { Req } from './req';
export class Plain {
  constructor(req: Req) {
    void req;
  }
}
export class Held {
  constructor(private req: Req) {}
}
`,
      'src/use.ts': `import { Plain, Held } from './plain';
export function viaPlain(p: Plain) { return p.req.header('x'); }
export function viaHeld(h: Held) { return h.req.header('x'); }
`,
    });
    try {
      const calls = callsFrom(cg, 'src/use.ts');
      // Only the parameter property `private req` makes `req` a field.
      expect(calls.filter((c) => c.includes('src/req.ts:Req::header'))).toHaveLength(1);
    } finally {
      cg.close();
    }
  });
});
