import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { extractSolidRoutes } from '../src/resolution/frameworks/solid-router';
import { routeRoots } from '../src/ui-server/api/route-roots';

describe('Solid Router registered declarations', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  beforeAll(async () => {
    await initGrammars();
    await loadGrammarsForLanguages(['tsx']);
  });
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  const imports = `import {Router, Route} from '@solidjs/router';`;
  const names = (source: string) =>
    extractSolidRoutes('src/app.tsx', imports + source)
      .nodes.map((n) => n.name)
      .sort();
  it('composes nested paths and base while excluding layout components', () => {
    expect(
      names(
        `<Router base="/app"><Route path="/users" component={Layout}><Route path="/" component={Index}/><Route path="/:id" component={User}/></Route></Router>`,
      ),
    ).toEqual(['/app/users', '/app/users/:id']);
  });
  it('accepts pathless parents, arrays and registered local config', () => {
    expect(
      names(
        `const routes = [{children:[{path:['/one','/two'],component:Page}]}]; export const App = () => <Router>{routes}</Router>;`,
      ),
    ).toEqual(['/one', '/two']);
  });
  it('removes a parent splat before composing children', () => {
    expect(
      names(
        `<Router><Route path="/docs/*" component={Layout}><Route path="/child" component={Page}/></Route></Router>`,
      ),
    ).toEqual(['/docs/child']);
  });
  it('recognizes imported aliases', () => {
    expect(
      extractSolidRoutes(
        'src/app.tsx',
        `import {Router as R, Route as P} from '@solidjs/router'; const App=()=> <R><P path="/a" component={Page}/></R>;`,
      ).nodes.map((n) => n.name),
    ).toEqual(['/a']);
  });
  it('recognizes static lazy imports in identifiers and inline config', () => {
    const result = extractSolidRoutes(
      'src/app.tsx',
      imports +
        `import {lazy} from 'solid-js'; const Page=lazy(()=>import('./page')); const routes=[{path:'/inline',component:lazy(()=>import('./other'))}]; const App=()=> <Router><Route path="/named" component={Page}/>{routes}</Router>;`,
    );
    expect(result.nodes.map((n) => n.name)).toEqual(['/named', '/inline']);
    expect(result.references.map((n) => n.referenceName)).toEqual([
      'solid-lazy:./page',
      'solid-lazy:./other',
    ]);
  });
  it.each([
    `<Route path="/orphan" component={Page}/>`,
    `const unused = [{path:'/orphan',component:Page}];`,
    `function App(Router) {return <Router><Route path="/x" component={Page}/></Router>}`,
    `function App(Route) {return <Router><Route path="/x" component={Page}/></Router>}`,
    `<Router base={unknown}><Route path="/x" component={Page}/></Router>`,
    `<Router><Route path={unknown} component={Page}/></Router>`,
    `<Router><Other path="/x" component={Page}/></Router>`,
    `<Router><Route {...props} path="/x" component={Page}/></Router>`,
    `<Router>{[{path:'/x',...props,component:Page}]}</Router>`,
    `const routes=[{path:'/x',component:Page}]; routes.push(other); <Router>{routes}</Router>`,
    `const routes=[{path:'/x',component:Page}]; const alias=routes; alias[0].path='/other'; <Router>{routes}</Router>`,
    `const Page=foreign(()=>import('./page')); <Router><Route path="/x" component={Page}/></Router>`,
    `function App(){const {Route}=other;return <Router><Route path="/wrong" component={Page}/></Router>}`,
    `const children=[]; children.push({path:'child',component:Page}); const routes=[{path:'/parent',component:Page,children:children}]; <Router>{routes}</Router>`,
  ])('does not invent routes for unsupported declarations: %s', (source) => {
    expect(names(source)).toEqual([]);
  });
  it('indexes exact imported components without duplicate React routes and syncs edits/deletions', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-solid-'));
    fs.mkdirSync(path.join(dir, 'src'));
    fs.writeFileSync(
      path.join(dir, 'package.json'),
      JSON.stringify({ dependencies: { '@solidjs/router': '0.16.3', 'solid-js': '1.9.9' } }),
    );
    fs.writeFileSync(
      path.join(dir, 'src/page.tsx'),
      'export default function Page(){return <div/>}',
    );
    fs.writeFileSync(
      path.join(dir, 'src/other.tsx'),
      'export default function Page(){return <div/>}',
    );
    const app = path.join(dir, 'src/app.tsx');
    fs.writeFileSync(
      app,
      imports +
        `import Page from './page'; export const App=()=> <Router><Route path="/first" component={Page}/></Router>;`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    let routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name)).toEqual(['/first']);
    expect(routeRoots(cg, routes).get(routes[0]!.id)!.node.filePath).toBe('src/page.tsx');
    fs.writeFileSync(
      app,
      imports +
        `import Page from './page'; export const App=()=> <Router><Route path="/second" component={Page}/></Router>;`,
    );
    await cg.sync();
    routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name)).toEqual(['/second']);
    fs.unlinkSync(app);
    await cg.sync();
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it('indexes the pinned upstream README lazy example through exact defaults', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-solid-real-'));
    fs.mkdirSync(path.join(dir, 'pages'));
    fs.writeFileSync(
      path.join(dir, 'package.json'),
      JSON.stringify({ dependencies: { '@solidjs/router': '0.16.3', 'solid-js': '1.9.9' } }),
    );
    // solidjs/solid-router@e8d3a7f719020ef01f8879a0110d2123b8597caa README.md, lazy-load example.
    fs.writeFileSync(
      path.join(dir, 'app.tsx'),
      `import { lazy } from "solid-js";
import { render } from "solid-js/web";
import { Router, Route } from "@solidjs/router";
const Users = lazy(() => import("./pages/Users"));
const Home = lazy(() => import("./pages/Home"));
const App = (props) => (<><h1>My Site with lots of pages</h1>{props.children}</>);
render(() => (<Router root={App}><Route path="/users" component={Users} /><Route path="/" component={Home} /></Router>),document.getElementById("app"));`,
    );
    fs.writeFileSync(
      path.join(dir, 'pages/Home.tsx'),
      'export default function Home(){return <div/>}',
    );
    fs.writeFileSync(
      path.join(dir, 'pages/Users.tsx'),
      'export default function Users(){return <div/>}',
    );
    cg = await CodeGraph.init(dir, { index: true });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual(['/', '/users']);
    const roots = routeRoots(cg, routes);
    expect(routes.map((n) => [n.name, roots.get(n.id)!.node.name]).sort()).toEqual([
      ['/', 'Home'],
      ['/users', 'Users'],
    ]);
    cg.close();
    cg = await CodeGraph.open(dir);
    fs.writeFileSync(path.join(dir, 'pages/Home.tsx'), 'function Decoy(){}; export default 1;');
    await cg.sync();
    const home = cg.getNodesByKind('route').find((n) => n.name === '/')!;
    expect(cg.getOutgoingEdges(home.id).filter((e) => e.kind === 'references')).toEqual([]);
  });
  it('detects Solid introduced by scoped sync and adds new registered routes', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-solid-sync-'));
    fs.writeFileSync(path.join(dir, 'package.json'), '{}');
    fs.writeFileSync(path.join(dir, 'seed.ts'), 'export const seed=1;');
    cg = await CodeGraph.init(dir, { index: true });
    fs.writeFileSync(
      path.join(dir, 'package.json'),
      JSON.stringify({ dependencies: { '@solidjs/router': '0.16.3' } }),
    );
    fs.writeFileSync(path.join(dir, 'Home.tsx'), 'export default function Home(){return <div/>}');
    fs.writeFileSync(
      path.join(dir, 'app.tsx'),
      imports +
        `import Home from './Home'; export const App=()=> <Router><Route path="/added" component={Home}/></Router>;`,
    );
    await cg.sync({ paths: ['app.tsx', 'Home.tsx', 'package.json'] });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name)).toEqual(['/added']);
    expect(routeRoots(cg, routes).get(routes[0]!.id)!.node.name).toBe('Home');
  });
  it('extracts and resolves lazy components in fresh compiled workers', () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-solid-workers-'));
    fs.writeFileSync(
      path.join(dir, 'package.json'),
      JSON.stringify({ dependencies: { '@solidjs/router': '0.16.3' } }),
    );
    fs.writeFileSync(path.join(dir, 'Home.tsx'), 'export default function Home(){return <div/>}');
    fs.writeFileSync(
      path.join(dir, 'app.tsx'),
      imports +
        `import {lazy} from 'solid-js'; const Home=lazy(()=>import('./Home')); export const App=()=> <Router><Route path="/" component={Home}/></Router>;`,
    );
    const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route')[0];console.log(JSON.stringify([r?.name,r&&cg.getOutgoingEdges(r.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]));cg.close();})().catch(e=>{console.error(e);process.exit(1)});`;
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
    expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual(['/', ['Home']]);
  });
});
