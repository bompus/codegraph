import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { routeRoots } from '../src/ui-server/api/route-roots';

describe('Qwik City default route conventions', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, content: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), content);
  };
  const config = `import {defineConfig} from 'vite';import {qwikCity as city} from '@builder.io/qwik-city/vite';export default defineConfig(({command,mode})=>{return {plugins:[city()]}});`;
  const root = `import {component$} from '@builder.io/qwik';import {QwikCityProvider as Provider,RouterOutlet as Outlet} from '@builder.io/qwik-city';export default component$(()=>{return <Provider><head/><body><Outlet/></body></Provider>});`;
  const setup = () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-qwik-city-'));
    write(
      'package.json',
      JSON.stringify({
        dependencies: { '@builder.io/qwik-city': '1.20.0', '@builder.io/qwik': '1.20.0' },
      }),
    );
    write('vite.config.ts', config);
    write('src/root.tsx', root);
  };
  const page = (sub: string, name: string) =>
    write(
      `src/routes/${sub ? sub + '/' : ''}index.tsx`,
      `export default function ${name}(){return <p/>}`,
    );
  const routes = () =>
    cg!.getNodesByKind('route').filter((n) => n.id.startsWith('route:qwik-city:'));
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });

  // QwikDev/qwik@971465f941e44e5adf2b2c2e44566b590d0990d8:
  // starters/apps/qwikcity-test/src/routes/issue2441/abc.page/index.tsx,
  // packages/docs/src/routes/demo/qwikcity/middleware/json/index.tsx.
  // Extracted source examples share a minimal app with the starter's config callback shape.
  it('indexes pinned anonymous page and endpoint fixtures with exact roots', async () => {
    setup();
    write(
      'src/routes/issue2441/abc.page/index.tsx',
      'import { component$ } from "@builder.io/qwik";\n\nexport default component$(() => {\n  return <h1 id="issue2441">Issue 2441</h1>;\n});\n',
    );
    write(
      'src/routes/demo/qwikcity/middleware/json/index.tsx',
      "import { type RequestHandler } from '@builder.io/qwik-city';\n\nexport const onGet: RequestHandler = async ({ json }) => {\n  json(200, { hello: 'world' });\n};\n",
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/issue2441/abc.page/', 'GET /demo/qwikcity/middleware/json/']);
    const roots = routeRoots(cg, routes());
    const component = roots.get(routes().find((n) => n.name.startsWith('/'))!.id)!.node;
    expect([component.kind, component.name, component.filePath, component.startLine]).toEqual([
      'component',
      'default',
      'src/routes/issue2441/abc.page/index.tsx',
      3,
    ]);
    expect(roots.get(routes().find((n) => n.name.startsWith('GET'))!.id)!.node.name).toBe('onGet');
  });
  it('links multiline plain function defaults and method exports', async () => {
    setup();
    write(
      'src/routes/index.tsx',
      'const Page =\n  () => <p/>;\nexport default Page;\nexport const onGet =\n  () => 1;',
    );
    cg = await CodeGraph.init(dir, { index: true });
    const roots = routeRoots(cg, routes());
    expect(
      routes()
        .map((route) => [route.name, roots.get(route.id)?.node.name])
        .sort(),
    ).toEqual([
      ['/', 'Page'],
      ['GET /', 'onGet'],
    ]);
  });
  it.each([false, true])(
    'preserves component body calls without taking neighboring or named nested calls (named=%s)',
    async (named) => {
      setup();
      write(
        'src/routes/index.tsx',
        `import {component$} from '@builder.io/qwik';
function load(){return 1} function outside(){return 2} function nestedCall(){return 3}
outside(); ${named ? 'const Page =' : 'export default'} component$(()=>{function helper(){return nestedCall()}const x=load();return <button onClick={()=>load()}>{x}</button>}); outside(); ${named ? 'export default Page;' : ''}`,
      );
      cg = await CodeGraph.init(dir, { index: true });
      const component = routeRoots(cg, routes()).get(routes()[0]!.id)!.node;
      const calls = cg
        .getOutgoingEdges(component.id)
        .filter((e) => e.kind === 'calls')
        .map((e) => cg!.getNode(e.target)?.name);
      expect(calls).toContain('load');
      expect(calls).not.toContain('outside');
      expect(calls).not.toContain('nestedCall');
      expect(calls).not.toContain('component$');
      const helper = cg.getNodesByKind('function').find((n) => n.name === 'helper')!;
      expect(
        cg
          .getOutgoingEdges(helper.id)
          .some((e) => e.kind === 'calls' && cg!.getNode(e.target)?.name === 'nestedCall'),
      ).toBe(true);
    },
  );
  it('keeps parent indexes, route groups, parameters and explicit methods distinct from middleware', async () => {
    setup();
    for (const [sub, name] of [
      ['', 'Home'],
      ['users', 'Users'],
      ['users/[id]', 'User'],
      ['files/[...rest]', 'Files'],
      ['(auth)/login', 'Login'],
      ['__legacy/account', 'Account'],
      ['_private', 'Private'],
      ['[user].json', 'Json'],
    ])
      page(sub!, name!);
    write(
      'src/routes/layout.tsx',
      'export default function Layout(){return <p/>} export function onGet(){}',
    );
    write(
      'src/routes/other.tsx',
      'export default function Other(){return <p/>} export function onPost(){}',
    );
    write('src/routes/middleware/index.ts', 'export function onRequest(){}');
    write(
      'src/routes/api/index.ts',
      'export function onGet(){}\nexport function onHead(){}\nexport function onOptions(){}',
    );
    write(
      'src/routes/users/index.tsx',
      `import {component$ as component} from '@builder.io/qwik';const Users=component(()=> <p/>);export default Users;export const onPost=()=>1;export const onRequest=()=>2;`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual([
      '/',
      '/:user.json/',
      '/_private/',
      '/account/',
      '/files/*rest/',
      '/login/',
      '/users/',
      '/users/:id/',
      'GET /api/',
      'HEAD /api/',
      'OPTIONS /api/',
      'POST /users/',
    ]);
    const user = routes().find((n) => n.name === '/users/')!;
    expect(routeRoots(cg, routes()).get(user.id)!.node.name).toBe('Users');
  });
  it('does not need page rendering registration for an API endpoint', async () => {
    setup();
    write('src/root.tsx', 'export default function Root(){return <p/>}');
    page('page', 'Page');
    write('src/routes/api/index.ts', 'export const onGet=()=>1;');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes().map((n) => n.name)).toEqual(['GET /api/']);
  });
  it('rejects non-runtime exports, reassigned handlers and foreign factories', async () => {
    setup();
    write('src/routes/typed/index.ts', 'function onGet(){}; export type {onGet};');
    write('src/routes/mutated/index.ts', 'export function onGet(){};onGet=other;');
    write(
      'src/routes/foreign/index.tsx',
      `import {component$} from 'other';export default component$(()=> <p/>);`,
    );
    write('src/routes/decoy/index.tsx', 'function Decoy(){return <p/>}export default 1;');
    page('[[id]]', 'Unsupported');
    write('src/routes/index@other.tsx', 'export default function NamedLayout(){return <p/>}');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
  });
  it.each([
    [
      'vite.config.ts',
      `import {qwikCity} from '@builder.io/qwik-city/vite';export default {plugins:[qwikCity({routesDir:'other'})]}`,
    ],
    [
      'vite.config.ts',
      `import {qwikCity} from '@builder.io/qwik-city/vite';export default {plugins:[qwikCity({trailingSlash:false})]}`,
    ],
    [
      'vite.config.ts',
      `import {qwikCity} from '@builder.io/qwik-city/vite';export default {root:'other',plugins:[qwikCity()]}`,
    ],
    [
      'vite.config.ts',
      `import {qwikCity} from '@builder.io/qwik-city/vite';export default {plugins:[qwikCity()],...extra}`,
    ],
    [
      'vite.config.ts',
      `import {defineConfig} from 'vite';import {qwikCity} from '@builder.io/qwik-city/vite';export default defineConfig(qwikCity=>({plugins:[qwikCity()]}))`,
    ],
    [
      'vite.config.ts',
      `import {defineConfig} from 'vite';import {qwikCity} from '@builder.io/qwik-city/vite';export default defineConfig(()=>{if(flag)return {};return {plugins:[qwikCity()]}})`,
    ],
    [
      'src/root.tsx',
      `import {QwikCityProvider as Provider,RouterOutlet} from '@builder.io/qwik-city';export default function Root(){return <Provider>{false && <RouterOutlet/>}</Provider>}`,
    ],
    [
      'src/root.tsx',
      `import {QwikCityProvider,RouterOutlet} from '@builder.io/qwik-city';export default QwikCityProvider=><QwikCityProvider><RouterOutlet/></QwikCityProvider>`,
    ],
    [
      'src/root.tsx',
      `import {QwikCityProvider,RouterOutlet} from 'other';export default function Root(){return <QwikCityProvider><RouterOutlet/></QwikCityProvider>}`,
    ],
  ])('skips unsupported registration %s (%s)', async (file, source) => {
    setup();
    page('', 'Home');
    write(file, source);
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
  });
  it('supports an async literal config callback and a named root function', async () => {
    setup();
    write('vite.config.ts', config.replace('(({command,mode})', '(async ({command,mode})'));
    write(
      'src/root.tsx',
      `import {QwikCityProvider,RouterOutlet} from '@builder.io/qwik-city';export default function Root(){return <QwikCityProvider><body><RouterOutlet/></body></QwikCityProvider>}`,
    );
    page('', 'Home');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes().map((n) => n.name)).toEqual(['/']);
  });
  it('updates pages, endpoints and root configuration through scoped sync and reopening', async () => {
    setup();
    page('', 'Home');
    cg = await CodeGraph.init(dir, { index: true });
    cg.close();
    cg = await CodeGraph.open(dir);
    write('src/routes/api/index.ts', 'export function onGet(){return 1}');
    await cg.sync({ paths: ['src/routes/api/index.ts'] });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/', 'GET /api/']);
    write(
      'src/routes/index.tsx',
      `import {component$} from '@builder.io/qwik';export default component$(()=> <p/>);`,
    );
    await cg.sync();
    expect(routeRoots(cg, routes()).get(routes().find((n) => n.name === '/')!.id)!.node.kind).toBe(
      'component',
    );
    cg.close();
    cg = await CodeGraph.open(dir);
    write('src/root.tsx', 'export default function Root(){return <p/>}');
    await cg.sync({ paths: ['src/root.tsx'] });
    expect(routes().map((n) => n.name)).toEqual(['GET /api/']);
    expect(
      cg
        .getNodesByKind('component')
        .filter((n) => n.name === 'default' && n.filePath === 'src/routes/index.tsx'),
    ).toEqual([]);
    write('src/root.tsx', root);
    await cg.sync();
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/', 'GET /api/']);
    fs.unlinkSync(path.join(dir, 'src/routes/api/index.ts'));
    await cg.sync();
    expect(routes().map((n) => n.name)).toEqual(['/']);
  });
  it.each([false, true])('discovers newly introduced Qwik City (scoped=%s)', async (scoped) => {
    setup();
    write('package.json', '{}');
    page('', 'Home');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
    write('package.json', JSON.stringify({ dependencies: { '@builder.io/qwik-city': '1.20.0' } }));
    write('src/root.tsx', root + '\n');
    await cg.sync(scoped ? { paths: ['src/root.tsx'] } : undefined);
    expect(routes().map((n) => n.name)).toEqual(['/']);
  });
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'binds anonymous defaults and their calls in fresh compiled workers',
    () => {
      setup();
      write(
        'src/routes/index.tsx',
        `import {component$} from '@builder.io/qwik';function load(){return 1}export default component$(()=>{return <p>{load()}</p>});`,
      );
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route').find(r=>r.id.startsWith('route:qwik-city:'));const e=r&&cg.getOutgoingEdges(r.id).find(e=>e.kind==='references');const component=e&&cg.getNode(e.target);console.log(JSON.stringify([r?.name,component?.kind,component&&cg.getOutgoingEdges(component.id).filter(e=>e.kind==='calls').map(e=>cg.getNode(e.target)?.name)]));cg.close()})().catch(e=>{console.error(e);process.exit(1)})`;
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
    },
  );
});
