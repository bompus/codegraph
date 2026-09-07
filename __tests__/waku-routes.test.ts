import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { routeRoots } from '../src/ui-server/api/route-roots';

describe('Waku filesystem routes', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, content: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), content);
  };
  const setup = () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-waku-'));
    write('package.json', JSON.stringify({ dependencies: { waku: '1.0.0-rc.0' } }));
  };
  const page = (file: string, config = '') =>
    write(`src/pages/${file}.tsx`, `export default function Page(){return <p/>}\n${config}`);
  const routes = () =>
    cg!.getNodesByKind('route').filter((node) => node.id.startsWith('route:waku:'));
  const names = () =>
    routes()
      .map((node) => node.name)
      .sort();
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  it('indexes the pinned official home component through generated and explicit adapters', async () => {
    setup();
    // wakujs/waku@9f425e94996e018983cb629709544397b5f1e93b fs-router-build-split.
    write('src/pages/index.tsx', 'export default function Home() {\n  return <h1>Home</h1>;\n}\n');
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/']);
    expect(routeRoots(cg, routes()).get(routes()[0]!.id)!.node.name).toBe('Home');
    write(
      'src/waku.server.tsx',
      `import { fsRouter } from 'waku';
import adapter from 'waku/adapters/cloudflare';
import { queueState } from './queue-state.js';
export default adapter(fsRouter(import.meta.glob('./pages/**/*.{tsx,ts}')), {
 handlers: {async queue(batch: {messages: {body: unknown}[]}) {queueState.message = String(batch.messages[0]?.body ?? '');}},
});`,
    );
    await cg.sync();
    expect(names()).toEqual(['/']);
  });
  it('preserves ordinary underscores/dots and excludes framework files and directories', async () => {
    setup();
    for (const file of [
      'index',
      '(group)/about',
      'parent/index',
      'parent/child',
      '_private',
      'file.json',
      '_layout',
      '_root',
      '_slices/header',
      '_interceptors/auth',
      '_api/hello',
      'x/_components/A',
      '_hooks/useA',
      '_actions/a',
      '[path]',
    ])
      page(file);
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/', '/_private', '/about', '/file.json', '/parent', '/parent/child']);
  });
  it('requires staticPaths for static parameters and expands mixed and catchall slugs', async () => {
    setup();
    page('users/[id]');
    page('dynamic/[id]', `export const getConfig = () => ({render:'dynamic'});`);
    page(
      '@[username]',
      `export async function getConfig(){return {staticPaths:['Jane Doe','Sam']};}`,
    );
    page(
      'post/[id].json',
      `export function getConfig(){return {render:'static',staticPaths:['42']};}`,
    );
    page('docs/[...rest]', `export const getConfig=()=>({staticPaths:[['a','b'],['c']]});`);
    page('wild/[...rest]', `export const getConfig=()=>({render:'dynamic'});`);
    page('optional/[[id]]', `export const getConfig=()=>({render:'dynamic'});`);
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual([
      '/@Jane-Doe',
      '/@Sam',
      '/docs/a/b',
      '/docs/c',
      '/dynamic/:id',
      '/post/42.json',
      '/wild/*rest',
    ]);
  });
  it.each([
    'export const getConfig=()=>({path:"/changed"});',
    'export const getConfig=()=>({...options});',
    'export const getConfig=()=>({render:mode});',
    'export const getConfig=async()=>await loadConfig();',
    'export {getConfig} from "./config";',
    'export * from "./config";',
    'export function getConfig(){if(flag)return {};return {};}',
  ])('suppresses unknown or overriding configuration: %s', async (config) => {
    setup();
    page('about', config);
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual([]);
  });
  it.each([
    `export default {srcDir:'custom'};`,
    `export default {basePath:'/base'};`,
    `export default {...other};`,
    `export default load();`,
    `const config={};const alias=config;alias.srcDir='custom';export default config;`,
    `const config={};const a=config;const b=a;Object.assign(b,{srcDir:'custom'});export default config;`,
  ])('suppresses nondefault project configuration: %s', async (config) => {
    setup();
    page('index');
    write('waku.config.ts', config);
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual([]);
  });
  it('honors explicit glob extensions, aliases, and replacement server registrations', async () => {
    setup();
    page('index');
    write('src/pages/a.js', 'export default function A(){}');
    const entry = `import {fsRouter as routes} from 'waku';import app from 'waku/adapters/default';export default app(routes(import.meta.glob('./pages/**/*.{tsx,ts}')));`;
    write('src/waku.server.tsx', entry);
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/']);
    for (const source of [
      entry.replace('routes(import.meta.glob', 'other(import.meta.glob'),
      entry
        .replace('{tsx,ts}', '{js}')
        .replace('routes(import.meta.glob', 'routes(import.meta.glob')
        .replace(')));', '), {pagesDir:"custom"}));'),
      `export default programmatic;`,
    ]) {
      write('src/waku.server.tsx', source);
      await cg.sync();
      expect(names()).toEqual([]);
    }
  });
  it('links anonymous component calls and multiline named exports to exact nodes', async () => {
    setup();
    write(
      'src/pages/index.tsx',
      'function load(){}\nexport default () => {load();return <p onClick={()=>load()}/>};',
    );
    write('src/pages/named.tsx', 'const Page =\n  () => <p/>;\nexport default Page;');
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/', '/named']);
    const roots = routeRoots(cg, routes());
    const anonymous = roots.get(routes().find((node) => node.name === '/')!.id)!.node;
    expect(anonymous.name).toBe('default');
    expect(
      cg
        .getOutgoingEdges(anonymous.id)
        .filter((edge) => edge.kind === 'calls')
        .map((edge) => cg!.getNode(edge.target)?.name),
    ).toEqual(['load', 'load']);
    expect(roots.get(routes().find((node) => node.name === '/named')!.id)!.node.name).toBe('Page');
  });
  it('updates introduced framework, page edits/deletes and config across scoped reopened sync', async () => {
    setup();
    write('package.json', '{}');
    page('index');
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual([]);
    write('package.json', '{"dependencies":{"waku":"1.0.0-rc.0"}}');
    write(
      'src/waku.server.tsx',
      `import {fsRouter} from 'waku';import adapter from 'waku/adapters/default';export default adapter(fsRouter(import.meta.glob('./pages/**/*.{tsx,ts}')));`,
    );
    await cg.sync({ paths: ['src/waku.server.tsx'] });
    expect(names()).toEqual(['/']);
    page('about');
    await cg.sync();
    expect(names()).toEqual(['/', '/about']);
    write('src/pages/about.tsx', 'export default 1;');
    await cg.sync();
    expect(names()).toEqual(['/']);
    cg.close();
    cg = await CodeGraph.open(dir);
    write('waku.config.ts', 'export default {basePath:"/new"};');
    await cg.sync({ paths: ['waku.config.ts'] });
    expect(names()).toEqual([]);
    fs.unlinkSync(path.join(dir, 'waku.config.ts'));
    await cg.sync();
    expect(names()).toEqual(['/']);
    fs.unlinkSync(path.join(dir, 'src/pages/index.tsx'));
    await cg.sync();
    expect(names()).toEqual([]);
  });
  it('binds anonymous defaults and body calls in fresh compiled workers', () => {
    setup();
    write('src/pages/index.tsx', 'function load(){return 1}export default ()=> <p>{load()}</p>;');
    const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route').find(r=>r.id.startsWith('route:waku:'));const e=r&&cg.getOutgoingEdges(r.id).find(e=>e.kind==='references');const component=e&&cg.getNode(e.target);console.log(JSON.stringify([r?.name,component?.kind,component&&cg.getOutgoingEdges(component.id).filter(e=>e.kind==='calls').map(e=>cg.getNode(e.target)?.name)]));cg.close()})().catch(e=>{console.error(e);process.exit(1)})`;
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
    expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual(['/', 'component', ['load']]);
  });
});
