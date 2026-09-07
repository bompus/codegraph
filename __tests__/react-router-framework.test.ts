import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { extractReactRouterConfig } from '../src/resolution/frameworks/react-router';
import { routeRoots } from '../src/ui-server/api/route-roots';

beforeAll(async () => {
  await initGrammars();
  await loadGrammarsForLanguages(['typescript', 'javascript', 'tsx', 'jsx']);
});

// remix-run/react-router@7aea711dd1ae2bc5a076d13ff17291829690fa74,
// docs/start/framework/routing.md:28–52. Expected URLs come from that route table.
const official = `import {type RouteConfig, route, index, layout, prefix} from '@react-router/dev/routes';
export default [
  index('./home.tsx'),
  route('about', './about.tsx'),
  layout('./auth/layout.tsx', [route('login', './auth/login.tsx'), route('register', './auth/register.tsx')]),
  ...prefix('concerts', [index('./concerts/home.tsx'), route(':city', './concerts/city.tsx'), route('trending', './concerts/trending.tsx')]),
] satisfies RouteConfig;`;
const expected = [
  '/',
  '/about',
  '/login',
  '/register',
  '/concerts',
  '/concerts/:city',
  '/concerts/trending',
];
const extract = (source: string) => extractReactRouterConfig('app/routes.ts', source);

describe('React Router framework route tree', () => {
  it('reads the official route tree without promoting its pathless layout', () => {
    expect(extract(official).nodes.map((n) => n.name)).toEqual(expected);
    expect(extract(official).references.map((r) => r.referenceName)).toEqual([
      'react-router-module:./home.tsx',
      'react-router-module:./about.tsx',
      'react-router-module:./auth/login.tsx',
      'react-router-module:./auth/register.tsx',
      'react-router-module:./concerts/home.tsx',
      'react-router-module:./concerts/city.tsx',
      'react-router-module:./concerts/trending.tsx',
    ]);
  });
  it('composes nested routes and recognizes import aliases and options', () => {
    expect(
      extract(`import {route as r, index as i} from '@react-router/dev/routes';
export default [r('teams', './teams.tsx', {id:'teams'}, [i('./list.tsx'), r(':id?', './team.tsx')])];`).nodes.map(
        (n) => n.name,
      ),
    ).toEqual(['/teams', '/teams/:id?']);
  });
  it.each([
    `const unused = [route('unused', './unused.tsx')]; export default [];`,
    `function other(route) { return [route('fake', './fake.tsx')] } export default other(route);`,
    `export default [route(variable, './x.tsx'), route('x', module), ...prefix(variable, [index('./x.tsx')])];`,
    `export default [route('x', './x.tsx', {...options})];`,
    `// route('comment','./x.tsx')\nexport default ["route('string','./x.tsx')"];`,
  ])('does not infer dynamic or unregistered routes: %s', (source) => {
    expect(
      extract(`import {route,index,prefix} from '@react-router/dev/routes';\n${source}`).nodes,
    ).toEqual([]);
  });
  it('requires the actual helper import and default config location', () => {
    expect(
      extract(`import {route} from 'unrelated'; export default [route('x','./x.tsx')];`).nodes,
    ).toEqual([]);
    expect(extractReactRouterConfig('app/other.ts', official).nodes).toEqual([]);
  });
});

describe('framework pages through indexing and sync', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  const write = (file: string, source: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), source);
  };
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'loads grammars in a fresh compiled process with parse and resolver workers',
    () => {
      dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rr-workers-'));
      write('package.json', JSON.stringify({ dependencies: { 'react-router': '*' } }));
      write(
        'app/routes.ts',
        `import {index} from '@react-router/dev/routes'; export default [index('./home.tsx')];`,
      );
      write('app/home.tsx', 'export function Home() { return <div/>; }\nexport default Home;');
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});
(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});
const route=cg.getNodesByKind('route')[0];
const edges=cg.getOutgoingEdges(route.id).filter(e=>e.kind==='references');
console.log(JSON.stringify(edges.map(e=>cg.getNode(e.target)?.name)));cg.close();})().catch(e=>{console.error(e);process.exit(1)});`;
      const output = execFileSync(process.execPath, ['-e', script], {
        encoding: 'utf8',
        timeout: 60000,
        env: {
          ...process.env,
          CODEGRAPH_PARALLEL_RESOLVE_MIN: '1',
          CODEGRAPH_RESOLVE_WORKERS: '2',
          CODEGRAPH_PARSE_WORKERS: '2',
        },
      });
      expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual(['Home']);
    },
  );
  it('binds exact modules, resolves navigation, and updates routes after config edits/deletion', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rr-framework-'));
    write(
      'package.json',
      JSON.stringify({
        dependencies: { react: '*', 'react-router': '*', '@react-router/dev': '7.9.0' },
      }),
    );
    write('app/routes.ts', official);
    for (const file of [
      'home',
      'about',
      'auth/login',
      'auth/register',
      'concerts/home',
      'concerts/city',
      'concerts/trending',
    ])
      write(`app/${file}.tsx`, 'export default function Page() { return <div/>; }');
    write('app/auth/layout.tsx', 'export default function Layout() { return <div/>; }');
    write(
      'app/nav.tsx',
      `import {redirect} from 'react-router'; export function leave() { return redirect('/about'); }`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    let routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual([...expected].sort());
    const roots = routeRoots(cg, routes);
    expect(roots.get(routes.find((n) => n.name === '/')!.id)?.node.filePath).toBe('app/home.tsx');
    expect(roots.get(routes.find((n) => n.name === '/concerts')!.id)?.node.filePath).toBe(
      'app/concerts/home.tsx',
    );
    expect(roots.size).toBe(7);
    const leave = cg.getNodesByKind('function').find((n) => n.name === 'leave')!;
    expect(cg.getOutgoingEdges(leave.id)).toContainEqual(
      expect.objectContaining({
        kind: 'navigates',
        target: routes.find((n) => n.name === '/about')!.id,
      }),
    );
    write(
      'app/routes.ts',
      `import {route} from '@react-router/dev/routes'; export default [route('new','./new.tsx')];`,
    );
    write('app/new.tsx', 'export default function NewPage() { return <div/>; }');
    await cg.sync();
    routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name)).toEqual(['/new']);
    expect(routeRoots(cg, routes).get(routes[0].id)?.node.name).toBe('NewPage');
    write(
      'app/new.tsx',
      `const note = "export default function Fake";
function Fake() { return null; }
export default () => <div/>;`,
    );
    await cg.sync();
    expect(routeRoots(cg, cg.getNodesByKind('route')).size).toBe(0);
    write('app/new.tsx', 'const Real = () => <div/>;\nexport default Real;');
    await cg.sync();
    expect(routeRoots(cg, cg.getNodesByKind('route')).get(routes[0].id)?.node.name).toBe('Real');
    write(
      'app/new.tsx',
      'const Real = () => <div/>;\nconst Next = () => <div/>;\nexport default Next;',
    );
    await cg.sync();
    expect(routeRoots(cg, cg.getNodesByKind('route')).get(routes[0].id)?.node.name).toBe('Next');
    fs.unlinkSync(path.join(dir, 'app/routes.ts'));
    await cg.sync();
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it('uses the nested index as a navigation destination and discovers newly added config', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rr-added-'));
    write('package.json', JSON.stringify({ dependencies: { 'react-router': '*' } }));
    write('app/home.tsx', 'export default function Home() { return <div/>; }');
    cg = await CodeGraph.init(dir, { index: true });
    write(
      'app/routes.ts',
      `import {route,index} from '@react-router/dev/routes';
export default [route('teams', './layout.tsx', [index('./home.tsx')])];`,
    );
    write('app/layout.tsx', 'export default function Layout() { return <div/>; }');
    write(
      'app/nav.tsx',
      `import {redirect} from 'react-router'; export function go() { return redirect('/teams'); }`,
    );
    await cg.sync();
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name)).toEqual(['/teams']);
    expect(routeRoots(cg, routes).get(routes[0].id)?.node.name).toBe('Home');
    const go = cg.getNodesByKind('function').find((n) => n.name === 'go')!;
    expect(cg.getOutgoingEdges(go.id)).toContainEqual(
      expect.objectContaining({ kind: 'navigates', target: routes[0].id }),
    );
  });
});
