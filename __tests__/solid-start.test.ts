import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { routeRoots } from '../src/ui-server/api/route-roots';

describe('SolidStart 2 default routes', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, source: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), source);
  };
  const config = `import {defineConfig} from 'vite'; import {solidStart as start} from '@solidjs/start/config'; export default defineConfig({plugins:[start()]});`;
  const app = `import {Router as AppRouter} from '@solidjs/router'; import {FileRoutes as Pages} from '@solidjs/start/router'; export default function App(){return <AppRouter root={props => <main>{props.children}</main>}><Pages /></AppRouter>}`;
  const setup = () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-solid-start-'));
    write(
      'package.json',
      JSON.stringify({ dependencies: { '@solidjs/start': '2.0.4', '@solidjs/router': '0.16.3' } }),
    );
    write('vite.config.ts', config);
    write('src/app.tsx', app);
  };
  const page = (file: string, name: string) =>
    write(`src/routes/${file}.tsx`, `export default function ${name}(){return <main/>}`);
  const routes = () =>
    cg!.getNodesByKind('route').filter((n) => n.id.startsWith('route:solid-start:'));
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });

  it('indexes official page and API source fixtures with exact targets and calls', async () => {
    setup();
    // solidjs/solid-start@5d23efbcbb47997a70978be8b0e468df50d774a8:
    // apps/fixtures/basic/src/routes/about.tsx; experiments/src/routes/api/hello/[name].ts.
    // These extracted examples share a minimal app; the workspace config import uses the public package.
    write(
      'src/routes/about.tsx',
      'import { Title } from "@solidjs/meta";\n\nexport default function About() {\n  return (\n    <main>\n      <Title>About</Title>\n      <h1>About</h1>\n    </main>\n  );\n}\n',
    );
    write(
      'src/routes/api/hello/[name].ts',
      'import type { APIHandler } from "@solidjs/start/server";\n\nexport const GET: APIHandler = async ({ params }) => {\n  return `Hello ${params.name}!`;\n};\n',
    );
    write(
      'src/routes/both.tsx',
      'function load(){return "ok"}\nexport default function Both(){return <p/>}\nexport function GET(){return load()}\nexport const POST=()=>load();',
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual([
      '/about',
      '/both',
      'GET /api/hello/:name',
      'GET /both',
      'HEAD /api/hello/:name',
      'HEAD /both',
      'POST /both',
    ]);
    const roots = routeRoots(cg, routes());
    expect(roots.get(routes().find((n) => n.name === '/about')!.id)!.node.name).toBe('About');
    for (const method of ['GET', 'HEAD']) {
      const endpoint = routes().find((n) => n.name === `${method} /api/hello/:name`)!;
      const target = cg.getOutgoingEdges(endpoint.id).find((e) => e.kind === 'references')!;
      expect([cg.getNode(target.target)?.name, cg.getNode(target.target)?.filePath]).toEqual([
        'GET',
        'src/routes/api/hello/[name].ts',
      ]);
    }
    const get = cg
      .getNodesByKind('function')
      .find((n) => n.name === 'GET' && n.filePath === 'src/routes/both.tsx')!;
    expect(
      cg
        .getOutgoingEdges(get.id)
        .some((e) => e.kind === 'calls' && cg!.getNode(e.target)?.name === 'load'),
    ).toBe(true);
  });
  it('composes parameters, groups, literal dots and page-only hierarchy', async () => {
    setup();
    for (const [file, name] of [
      ['index', 'Home'],
      ['admin', 'Admin'],
      ['admin/index', 'AdminIndex'],
      ['admin/users/[id]', 'User'],
      ['test(named)/child', 'TestChild'],
      ['test', 'Test'],
      ['(auth)/login', 'Login'],
      ['[[id]]', 'Optional'],
      ['[...slug]', 'CatchAll'],
      ['_private', 'Private'],
      ['foo.bar', 'Dot'],
      ['api', 'ApiPage'],
    ])
      page(file!, name!);
    write('src/routes/api/child.ts', 'export function GET(){return 1}');
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual([
      '/',
      '/*slug',
      '/:id?',
      '/_private',
      '/admin',
      '/admin/users/:id',
      '/api',
      '/foo.bar',
      '/login',
      '/test',
      '/test/child',
      'GET /api/child',
      'HEAD /api/child',
    ]);
  });
  it('keeps explicit HEAD and skips optional APIs, route overrides and OPTIONS-only modules', async () => {
    setup();
    write(
      'src/routes/explicit.ts',
      'export function GET(){return 1}\nexport function HEAD(){return 2}\nexport function OPTIONS(){return 3}',
    );
    write('src/routes/options.ts', 'export function OPTIONS(){return 1}');
    write('src/routes/type-only.ts', 'function GET(){}; export type {GET};');
    write('src/routes/reassigned.ts', 'export function GET(){}; GET=other;');
    page('config/child', 'ChildOfOverride');
    write('src/routes/[[id]].ts', 'export function GET(){return 1}');
    write(
      'src/routes/config.tsx',
      'export const route={path:"other"}; export default function Config(){return <p/>}',
    );
    write(
      'src/routes/fake.tsx',
      'function Decoy(){return <p/>} export default 1; export const GET=1;',
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      routes()
        .map((n) => n.name)
        .sort(),
    ).toEqual(['GET /explicit', 'HEAD /explicit', 'OPTIONS /explicit']);
    const head = routes().find((n) => n.name === 'HEAD /explicit')!;
    expect(
      cg
        .getOutgoingEdges(head.id)
        .filter((e) => e.kind === 'references')
        .map((e) => cg!.getNode(e.target)?.name),
    ).toEqual(['HEAD']);
  });
  it.each([
    [
      'src/app.tsx',
      `import {Router} from '@solidjs/router';import {FileRoutes} from '@solidjs/start/router';export default function App(){return false && <Router><FileRoutes/></Router>}`,
    ],
    [
      'src/app.tsx',
      `import {Router} from '@solidjs/router';import {FileRoutes} from '@solidjs/start/router';export default Router => <Router><FileRoutes/></Router>`,
    ],
    [
      'src/app.tsx',
      `import {Router} from '@solidjs/router';import {FileRoutes} from '@solidjs/start/router';export default ({Router}) => <Router><FileRoutes/></Router>`,
    ],
    [
      'src/app.tsx',
      `import {Router} from '@solidjs/router';import {FileRoutes} from '@solidjs/start/router';const Unused=()=> <Router><FileRoutes/></Router>; export default ()=> <p/>`,
    ],
    ['vite.config.ts', `import {solidStart} from 'other';export default {plugins:[solidStart()]}`],
    [
      'vite.config.ts',
      `import {solidStart} from '@solidjs/start/config';export default {root:'custom',plugins:[solidStart()]}`,
    ],
    [
      'vite.config.ts',
      `import {solidStart} from '@solidjs/start/config';export default {plugins:[solidStart({routeDir:'custom'})]}`,
    ],
    [
      'vite.config.ts',
      `import {solidStart} from '@solidjs/start/config';export default {plugins:[solidStart()],...other}`,
    ],
    [
      'vite.config.ts',
      `import {solidStart} from '@solidjs/start/config';export default {plugins:[solidStart()],plugins:[]}`,
    ],
    [
      'src/app.tsx',
      `import {Router} from '@solidjs/router';import {FileRoutes} from 'other';export default function App(){return <Router><FileRoutes/></Router>}`,
    ],
    [
      'src/app.tsx',
      `import {Router} from '@solidjs/router';import {FileRoutes} from '@solidjs/start/router';export default function App(FileRoutes){return <Router><FileRoutes/></Router>}`,
    ],
    [
      'src/app.tsx',
      `import {Router} from '@solidjs/router';import {FileRoutes} from '@solidjs/start/router';export default function App(){return <Router base="/base"><FileRoutes/></Router>}`,
    ],
  ])('does not guess unsupported registration %s (%s)', async (file, source) => {
    setup();
    page('index', 'Home');
    write(file, source);
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
  });
  it('updates unchanged parents and config after scoped edits and reopening', async () => {
    setup();
    page('parent', 'Parent');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes().map((n) => n.name)).toEqual(['/parent']);
    cg.close();
    cg = await CodeGraph.open(dir);
    page('parent/child', 'Child');
    await cg.sync({ paths: ['src/routes/parent/child.tsx'] });
    expect(routes().map((n) => n.name)).toEqual(['/parent/child']);
    fs.unlinkSync(path.join(dir, 'src/routes/parent/child.tsx'));
    await cg.sync();
    expect(routes().map((n) => n.name)).toEqual(['/parent']);
    write('src/routes/parent.tsx', 'export default function Renamed(){return <p/>}');
    await cg.sync();
    expect(routeRoots(cg, routes()).get(routes()[0]!.id)!.node.name).toBe('Renamed');
    write('src/app.tsx', 'export default function App(){return <p/>}');
    await cg.sync({ paths: ['src/app.tsx'] });
    expect(routes()).toEqual([]);
    write('src/app.tsx', app);
    await cg.sync();
    expect(routes().map((n) => n.name)).toEqual(['/parent']);
    write('vite.config.ts', 'export default {}');
    await cg.sync();
    expect(routes()).toEqual([]);
  });
  it.each([false, true])('detects a newly introduced framework (scoped=%s)', async (scoped) => {
    setup();
    write('package.json', '{}');
    page('index', 'Home');
    cg = await CodeGraph.init(dir, { index: true });
    expect(routes()).toEqual([]);
    write('package.json', JSON.stringify({ dependencies: { '@solidjs/start': '2.0.4' } }));
    write('src/app.tsx', app + '\n');
    await cg.sync(scoped ? { paths: ['src/app.tsx'] } : undefined);
    expect(routes().map((n) => n.name)).toEqual(['/']);
  });
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'uses fresh compiled parse and resolution workers',
    () => {
      setup();
      page('index', 'Home');
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route').find(r=>r.id.startsWith('route:solid-start:'));console.log(JSON.stringify([r?.name,r&&cg.getOutgoingEdges(r.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]));cg.close()})().catch(e=>{console.error(e);process.exit(1)})`;
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
