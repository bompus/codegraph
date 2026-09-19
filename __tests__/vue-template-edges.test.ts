/**
 * Vue SFC template → registered-component edges.
 *
 * `vueTemplateEdges` resolves a `<my-comp>` tag first through the app's
 * REGISTRATIONS — `app.component('my-comp', MyComp)` globally and the SFC's own
 * `components: { 'my-comp': MyComp }` locally — before the kebab→Pascal name
 * guess. A registration is the authoritative tag→component binding: it catches
 * tags whose kebab name never PascalCases to the component's own name
 * (`'fancy-widget'` → `FancyThing`), and it stays precise because the
 * registered value must itself resolve to a real component node.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

const JSX_RENDER_EDGES = `
  SELECT s.name source, t.name target, t.file_path tf,
         json_extract(e.metadata,'$.via') via
  FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
  WHERE json_extract(e.metadata,'$.synthesizedBy') = 'jsx-render'`;

function writeVue(dir: string, rel: string, template: string, script: string): void {
  fs.mkdirSync(path.join(dir, path.dirname(rel)), { recursive: true });
  fs.writeFileSync(
    path.join(dir, rel),
    `<template>${template}</template>\n<script lang="ts">${script}</script>\n`
  );
}

describe('vue template edges — registered-name resolution', () => {
  let dir: string;
  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'vue-reg-'));
    fs.writeFileSync(path.join(dir, 'package.json'), '{"dependencies":{"vue":"^3.0.0"}}');
  });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  it('resolves a global `app.component(\'name\', Comp)` registration', async () => {
    writeVue(dir, 'components/FancyThing.vue', '<div />', 'export default {};');
    fs.writeFileSync(
      path.join(dir, 'main.ts'),
      `import { createApp } from 'vue';
import FancyThing from './components/FancyThing.vue';
const app = createApp({});
app.component('fancy-widget', FancyThing);
`
    );
    writeVue(dir, 'App.vue', '<fancy-widget />', 'export default {};');

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db.prepare(JSX_RENDER_EDGES).all();
    // `fancy-widget` PascalCases to `FancyWidget`, which names nothing — only
    // the registration map binds the tag to FancyThing.
    expect(edges.map((r: any) => `${r.source}->${r.target}(${r.via})`)).toEqual([
      'App->FancyThing(fancy-widget)',
    ]);
    cg.close?.();
  });

  it('resolves a local `components: { kebab: Comp }` registration, incl. shorthand and lazy forms', async () => {
    writeVue(dir, 'components/LocalWidget.vue', '<div />', 'export default {};');
    writeVue(dir, 'components/ShorthandCard.vue', '<div />', 'export default {};');
    writeVue(dir, 'components/LazyPanel.vue', '<div />', 'export default {};');
    writeVue(
      dir,
      'App.vue',
      '<local-comp /><shorthand-card /><lazy-panel />',
      `import ShorthandCard from './components/ShorthandCard.vue';
import LocalWidget from './components/LocalWidget.vue';
export default {
  components: {
    'local-comp': LocalWidget,
    ShorthandCard,
    'lazy-panel': () => import('./components/LazyPanel.vue'),
  },
};`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db.prepare(JSX_RENDER_EDGES).all();
    expect(edges.map((r: any) => `${r.source}->${r.target}(${r.via})`).sort()).toEqual([
      'App->LazyPanel(lazy-panel)',
      'App->LocalWidget(local-comp)',
      'App->ShorthandCard(shorthand-card)',
    ]);
    cg.close?.();
  });

  it('prefers the local registration over a same-spelled global name', async () => {
    writeVue(dir, 'components/GlobalThing.vue', '<div />', 'export default {};');
    writeVue(dir, 'components/LocalThing.vue', '<div />', 'export default {};');
    fs.writeFileSync(
      path.join(dir, 'main.ts'),
      `import { createApp } from 'vue';
import GlobalThing from './components/GlobalThing.vue';
createApp({}).component('shared-tag', GlobalThing);
`
    );
    writeVue(
      dir,
      'App.vue',
      '<shared-tag />',
      `import LocalThing from './components/LocalThing.vue';
export default { components: { 'shared-tag': LocalThing } };`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db.prepare(JSX_RENDER_EDGES).all();
    // The SFC's own `components` map is the innermost registry — `<shared-tag>`
    // is LocalThing here, not the globally registered GlobalThing.
    expect(edges.map((r: any) => `${r.source}->${r.target}`)).toEqual(['App->LocalThing']);
    cg.close?.();
  });

  it('ignores a `.component(` call that is not a Vue app registration (gate)', async () => {
    writeVue(dir, 'components/FakeThing.vue', '<div />', 'export default {};');
    fs.writeFileSync(
      path.join(dir, 'registry.ts'),
      `// A builder registry, not a Vue app — no createApp/vueApp/Vue.component.
class Palette { component(name: string, impl: unknown) { return impl; } }
export function buildPalette(FakeThing: unknown) {
  return new Palette().component('fake-widget', FakeThing);
}
`
    );
    writeVue(dir, 'App.vue', '<fake-widget />', 'export default {};');

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db.prepare(JSX_RENDER_EDGES).all();
    expect(edges).toEqual([]);
    cg.close?.();
  });
});
