import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { routeRoots } from '../src/ui-server/api/route-roots';

describe('Vike default pages and literal route overrides', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, content: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), content);
  };
  const config = `import react from '@vitejs/plugin-react';import vike from 'vike/plugin';export default {plugins:[react(),vike()]};`;
  const setup = () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vike-'));
    write(
      'package.json',
      JSON.stringify({ dependencies: { vike: '0.4.266', 'vike-react': '0.6.26' } }),
    );
    write('vite.config.js', config);
  };
  const page = (folder: string, name: string) =>
    write(`${folder}/+Page.tsx`, `export default function ${name}(){return <p/>}`);
  const routes = () => cg!.getNodesByKind('route').filter((n) => n.id.startsWith('route:vike:'));
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });

  // vikejs/vike@715c15d11be196caa69159b929ae09697f3ce734: react-minimal About and
  // file-structure-domain-driven/product/pages/index source examples share a minimal app.
  it('indexes official Page and route override fixtures with exact targets', async () => {
    setup();
    write(
      'pages/about/+Page.tsx',
      "export default Page\nimport React from 'react'\n\nfunction Page() {\n  return (\n    <>\n      <h1>About</h1>\n      <p>Example of using Vike.</p>\n    </>\n  )\n}\n",
    );
    write(
      'product/pages/index/+Page.jsx',
      "export default Page\n\nimport React from 'react'\n\nfunction Page({ routeParams }) {\n  return <>Product {routeParams.productId}</>\n}\n",
    );
    write('product/pages/index/+route.js', "export default '/product/@productId'\n");
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/about', '/product/:productId']);
    const roots = routeRoots(cg, routes());
    expect(
      routes()
        .map((n) => [n.name, roots.get(n.id)!.node.name, roots.get(n.id)!.node.filePath])
        .sort(),
    ).toEqual([
      ['/about', 'Page', 'pages/about/+Page.tsx'],
      ['/product/:productId', 'Page', 'product/pages/index/+Page.jsx'],
    ]);
  });
  it('uses whole-root filesystem conventions and keeps parent pages distinct from layouts', async () => {
    setup();
    for (const [folder, name] of [
      ['pages/index', 'Home'],
      ['src/pages/parent', 'Parent'],
      ['src/pages/parent/child', 'Child'],
      ['domain/pages/(shop)/index/@id', 'Item'],
      ['src/index/pages/renderer/Foo.bar', 'Dot'],
      ['pages/docs/catchall', 'CatchAll'],
    ])
      page(folder!, name!);
    write('pages/parent/+Layout.tsx', 'export default function Layout(){return <p/>}');
    write('pages/docs/catchall/+route.ts', `export default '/docs/*';`);
    write('pages/nothing/Page.tsx', 'export default function NotPlus(){return <p/>}');
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/', '/Foo.bar', '/docs/*', '/domain/:id', '/parent', '/parent/child']);
  });
  it('uses the nearest inherited route and never falls back through a dynamic override', async () => {
    setup();
    page('pages/blog/post', 'Post');
    page('pages/blog/other', 'Other');
    write('pages/blog/+route.ts', `export default '/inherited';`);
    write('pages/blog/post/+route.ts', `export default '/specific/@id';`);
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/inherited', '/specific/:id']);
    write('pages/blog/+route.ts', `export default ctx=>ctx.urlPathname;`);
    await cg.sync();
    expect(routes().map((n) => n.name)).toEqual(['/specific/:id']);
    write('pages/blog/post/+meta.ts', 'export default dynamic;');
    await cg.sync();
    expect(routes()).toEqual([]);
  });
  it('links a multiline default function value to its actual symbol', async () => {
    setup();
    write('pages/index/+Page.tsx', 'const Page =\n  () => <p/>;\nexport default Page;');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routeRoots(cg, routes()).get(routes()[0]!.id)?.node.name).toBe('Page');
  });
  it('supports named Page exports and route aliases', async () => {
    setup();
    write('pages/one/+Page.tsx', 'function Screen(){return <p/>};export {Screen as Page};');
    write('pages/one/+route.ts', `const value='/one/@id';export {value as route};`);
    write('pages/two/+Page.tsx', 'export const Page=()=> <p/>;');
    write('pages/two/+route.ts', `export const route='/two';`);
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/one/:id', '/two']);
    expect(
      routeRoots(cg, routes()).get(routes().find((n) => n.name === '/one/:id')!.id)!.node.name,
    ).toBe('Screen');
  });
  it.each([
    `export default pageContext=>({match:pageContext.urlPathname==='/x'});`,
    `export default prefix + '/x';`,
    `export {default} from './other';`,
    `export default '/one';export const route='/two';`,
    `function route(){};export type {route};`,
  ])('does not fall back when a route override is unsupported (%s)', async (source) => {
    setup();
    page('pages/guessed', 'Guessed');
    write('pages/guessed/+route.ts', source);
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
  });
  it('uses inheritance locations rather than normalized URLs when suppressing configs', async () => {
    setup();
    page('src/pages/a', 'A');
    page('src/admin/pages/b', 'B');
    page('pages/c', 'C');
    write('src/pages/+config.ts', `export default {filesystemRoutingRoot:'/custom'};`);
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes().map((n) => n.name)).toEqual(['/c']);
    write('pages/+config.ts', `export default {route:'/root'};`);
    await cg.sync();
    expect(routes()).toEqual([]);
  });
  it('accepts canonical vike-react rendering config but excludes unknown extensions and configured pages', async () => {
    setup();
    page('pages/one', 'One');
    page('pages/two', 'Two');
    write(
      'pages/+config.ts',
      `import vikeReact from 'vike-react/config';export default {extends:vikeReact,ssr:true,title:'Site'};`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/one', '/two']);
    write(
      'pages/+config.ts',
      `import vikeReact from 'vike-react/config';export default {extends:[vikeReact],ssr:false};`,
    );
    await cg.sync();
    expect(routes()).toHaveLength(2);
    write('pages/one/+config.ts', `import other from './custom';export default {extends:other};`);
    await cg.sync();
    expect(routes()).toEqual([]);
    fs.unlinkSync(path.join(dir, 'pages/one/+config.ts'));
    write('pages/two/+config.ts', `export default {Page:Other};`);
    await cg.sync();
    expect(routes().map((n) => n.name)).toEqual(['/one']);
  });
  it.each([
    `export default {extends:[{onBeforeRoute:()=>({pageContext:{urlLogical:'/changed'}})}]};`,
    `const hidden={onBeforeRoute:()=>({})};export default {...hidden};`,
    `export default {meta:custom};`,
    `export default {Page:Other,extends:[{onBeforeRoute:()=>({})}]};`,
    `const config={};Object.assign(config,{onBeforeRoute:()=>({})});export default config;`,
    `import vikeReact from 'vike-react/config';Object.assign(vikeReact,{onBeforeRoute:()=>({})});export default {extends:vikeReact};`,
  ])('unknown config cannot hide a global routing hook (%s)', async (source) => {
    setup();
    page('pages/one', 'One');
    page('pages/two', 'Two');
    write('pages/one/+config.ts', source);
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
  });
  it('treats onBeforeRoute as global and removes stale paths', async () => {
    setup();
    page('pages/one', 'One');
    page('src/pages/two', 'Two');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toHaveLength(2);
    write(
      'src/pages/+onBeforeRoute.ts',
      'export function onBeforeRoute(){return {pageContext:{}}}',
    );
    await cg.sync({ paths: ['src/pages/+onBeforeRoute.ts'] });
    expect(routes()).toEqual([]);
    fs.unlinkSync(path.join(dir, 'src/pages/+onBeforeRoute.ts'));
    await cg.sync();
    expect(routes()).toHaveLength(2);
    write('pages/+config.ts', 'export default {onBeforeRoute:custom};');
    await cg.sync();
    expect(routes()).toEqual([]);
  });
  it('excludes ambiguous, type-only, reassigned and re-exported page targets', async () => {
    setup();
    write(
      'pages/ambiguous/+Page.tsx',
      'export default function Default(){return <p/>}export function Page(){return <p/>}',
    );
    write('pages/typed/+Page.tsx', 'function Page(){return <p/>}export type {Page};');
    write('pages/mutated/+Page.tsx', 'export function Page(){return <p/>}Page=Other;');
    write('pages/reexport/+Page.tsx', `export {default} from './real';`);
    write('pages/decoy/+Page.tsx', 'function Decoy(){return <p/>}export default 1;');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
  });
  it.each([
    `import vike from 'other';export default {plugins:[vike()]};`,
    `import vike from 'vike/plugin';export default {root:'other',plugins:[vike()]};`,
    `import vike from 'vike/plugin';export default {plugins:[vike({pages:['custom']})]};`,
    `import vike from 'vike/plugin';export default {plugins:[vike()],...config};`,
  ])('requires supported plugin registration (%s)', async (source) => {
    setup();
    page('pages/index', 'Home');
    write('vite.config.js', source);
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
  });
  it('syncs route and target changes after reopening, including scoped config changes', async () => {
    setup();
    page('pages/index', 'Home');
    cg = await CodeGraph.init(dir, { index: true });
    cg.close();
    cg = await CodeGraph.open(dir);
    write('pages/index/+route.ts', `export default '/moved/@id';`);
    await cg.sync({ paths: ['pages/index/+route.ts'] });
    expect(routes().map((n) => n.name)).toEqual(['/moved/:id']);
    write('pages/index/+route.ts', 'export default dynamic;');
    await cg.sync();
    expect(routes()).toEqual([]);
    fs.unlinkSync(path.join(dir, 'pages/index/+route.ts'));
    await cg.sync();
    expect(routes().map((n) => n.name)).toEqual(['/']);
    page('pages/index', 'Renamed');
    await cg.sync();
    expect(routeRoots(cg, routes()).get(routes()[0]!.id)!.node.name).toBe('Renamed');
    write('vite.config.js', 'export default {}');
    await cg.sync({ paths: ['vite.config.js'] });
    expect(routes()).toEqual([]);
  });
  it.each([false, true])('detects newly introduced Vike (scoped=%s)', async (scoped) => {
    setup();
    write('package.json', '{}');
    page('pages/index', 'Home');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
    write('package.json', JSON.stringify({ dependencies: { vike: '0.4.266' } }));
    write('vite.config.js', config + '\n');
    await cg.sync(scoped ? { paths: ['vite.config.js'] } : undefined);
    expect(routes().map((n) => n.name)).toEqual(['/']);
  });
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'uses fresh compiled workers for exact page roots',
    () => {
      setup();
      page('pages/index', 'Home');
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route').find(r=>r.id.startsWith('route:vike:'));console.log(JSON.stringify([r?.name,r&&cg.getOutgoingEdges(r.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]));cg.close()})().catch(e=>{console.error(e);process.exit(1)})`;
      const output = execFileSync(process.execPath, ['-e', script], {
        encoding: 'utf8',
        timeout: 60000,
        env: {
          ...process.env,
          CODEGRAPH_PARSE_WORKERS: '2',
          CODEGRAPH_PARALLEL_RESOLVE_MIN: '1',
          CODEGRAPH_RESOLVE_WORKERS: '2',
        },
      });
      expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual(['/', ['Home']]);
    },
  );
});
