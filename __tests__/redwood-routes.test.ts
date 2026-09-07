import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { extractRedwoodRoutes } from '../src/resolution/frameworks/redwood';
import { routeRoots } from '../src/ui-server/api/route-roots';

beforeAll(async () => {
  await initGrammars();
  await loadGrammarsForLanguages(['typescript', 'javascript', 'tsx', 'jsx']);
});
const imports = `import {route, index, render, layout, prefix} from 'rwsdk/router';
import {defineApp} from 'rwsdk/worker';\n`;
const extract = (body: string) => extractRedwoodRoutes('src/worker.tsx', imports + body);

describe('registered RedwoodSDK routes', () => {
  it('composes aliases, constant arrays, prefixes, index and layouts', () => {
    const result = extractRedwoodRoutes(
      'src/worker.tsx',
      `import {route as r, render as page, prefix as under, index, layout} from 'rwsdk/router';
import {defineApp as app} from 'rwsdk/worker';
const children = [index(Home), r('users/:id', [auth, User])];
const worker = app([page(Document, [under('/admin', layout(Layout, children))])]);
export default {fetch: worker.fetch};`,
    );
    expect(result.nodes.map((n) => n.name)).toEqual(['ANY /admin', 'ANY /admin/users/:id']);
    expect(result.references.map((r) => r.referenceName)).toEqual(['Home', 'User']);
  });
  it('reads standard method tables without config or interrupter handlers', () => {
    const result = extract(
      `export default defineApp([route('/api', {get: [auth, list], post: create, head: head, config: {disableOptions: true}})]);`,
    );
    expect(result.nodes.map((n) => n.name)).toEqual(['GET /api', 'POST /api', 'HEAD /api']);
    expect(result.references.map((r) => r.referenceName)).toEqual(['list', 'create', 'head']);
  });
  it('keeps inline JSX pages and response endpoints distinct', () => {
    const result = extract(
      `export default defineApp([render(Document, [route('/page', () => <Home/>), route('/health', () => new Response('ok')), route('/nested', () => {const unused = () => <Home/>; return new Response('ok')})])]);`,
    );
    expect(result.nodes.map((n) => n.name)).toEqual(['/page', 'ANY /health', 'ANY /nested']);
    expect(result.references.map((r) => r.referenceName)).toEqual(['Home']);
  });
  it('reads shorthand handlers and ordinary method definitions', () => {
    const result = extract(
      `export default defineApp([route('/api', {get, post(){return save()}})]);`,
    );
    expect(result.nodes.map((n) => n.name)).toEqual(['GET /api', 'POST /api']);
    expect(result.references.map((r) => r.referenceName)).toEqual(['get', 'save']);
  });
  it.each([
    `const unused = route('/orphan', Home); export default defineApp([]);`,
    `function wrapper(route){return [route('/shadow', Home)]} export default defineApp(wrapper(other));`,
    `export default defineApp([prefix(dynamic, [route('/child', Home)])]);`,
    `export default defineApp([route(path, Home)]);`,
    `export default defineApp([() => route('/middleware', Home)]);`,
    `export default defineApp([route('/api', {...methods, get: list})]);`,
    `export default defineApp([route('/api', {[method]: list})]);`,
    `export default defineApp([route('/api', {get: list, get: other})]);`,
    `export default defineApp([route('/api', {get get(){return list}})]);`,
    `export default defineApp([route('/api', {get: list, [method](){return other()}})]);`,
    `export default defineApp([route('/api', dynamic())]);`,
    `export default defineApp([route('/api', [...handlers])]);`,
    `const example = "route('/fake', Home)"; export default defineApp([]);`,
    `const app = defineApp([route('/fake', Home)]); export default {fetch: app.fetch, fetch: other};`,
    `const app = defineApp([route('/fake', Home)]); export default {fetch: app.fetch, ...other};`,
  ])('ignores unsupported/unregistered declarations: %s', (body) =>
    expect(extract(body).nodes).toEqual([]),
  );
  it('requires runtime framework helper imports', () => {
    for (const source of [
      `import {route} from 'other'; import {defineApp} from 'rwsdk/worker'; export default defineApp([route('/x', Home)]);`,
      `import type {route} from 'rwsdk/router'; import {defineApp} from 'rwsdk/worker'; export default defineApp([route('/x', Home)]);`,
      `import {route} from 'rwsdk/router'; import type {defineApp} from 'rwsdk/worker'; export default defineApp([route('/x', Home)]);`,
    ])
      expect(extractRedwoodRoutes('src/worker.tsx', source).nodes).toEqual([]);
  });
});

describe('RedwoodSDK through indexing', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, source: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), source);
  };
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  // redwoodjs/sdk@39da7118f712bd86450e493cb2c213815b1893bb (1.7.3),
  // playground/typed-routes/src/worker.tsx: expected route/handler pairs.
  it('binds the pinned worker pages, API handlers and incremental changes', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-redwood-'));
    write('package.json', JSON.stringify({ dependencies: { rwsdk: '1.7.3' } }));
    for (const page of ['Home', 'UserProfile', 'FileViewer', 'BlogPost'])
      write(`src/pages/${page}.tsx`, `export function ${page}(){return <div/>}`);
    write('src/other/Home.tsx', 'export function Home(){return <div/>}');
    write(
      'src/api.ts',
      `export function list(){return new Response('list')}
export function create(){return new Response('created')}
export function Misleading(){return new Response('not a page')}`,
    );
    const worker =
      imports +
      `import {Home} from './pages/Home'; import {UserProfile} from './pages/UserProfile';
import {FileViewer} from './pages/FileViewer'; import {BlogPost} from './pages/BlogPost';
import {list,create,Misleading} from './api';
function auth(){return undefined}
export const app = defineApp([render(Document, [route('/old', () => new Response(null,{status:301})), route('/',Home), route('/users/:id',[auth,UserProfile]), route('/files/*',FileViewer), route('/blog/:year/:slug',BlogPost), route('/misleading',Misleading)]), route('/api',{get:list,post:create})]);
export default {fetch: app.fetch};`;
    write('src/worker.tsx', worker);
    cg = await CodeGraph.init(dir, { index: true });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual(
      [
        'ANY /old',
        '/',
        '/users/:id',
        '/files/*',
        '/blog/:year/:slug',
        'ANY /misleading',
        'GET /api',
        'POST /api',
      ].sort(),
    );
    const roots = routeRoots(cg, routes);
    for (const [url, handler, file] of [
      ['/', 'Home', 'src/pages/Home.tsx'],
      ['/users/:id', 'UserProfile', 'src/pages/UserProfile.tsx'],
      ['/files/*', 'FileViewer', 'src/pages/FileViewer.tsx'],
      ['/blog/:year/:slug', 'BlogPost', 'src/pages/BlogPost.tsx'],
      ['GET /api', 'list', 'src/api.ts'],
      ['POST /api', 'create', 'src/api.ts'],
    ]) {
      const root = roots.get(routes.find((n) => n.name === url)!.id)!.node;
      expect([root.name, root.filePath]).toEqual([handler, file]);
    }
    const userRoute = routes.find((n) => n.name === '/users/:id')!;
    expect(cg.getOutgoingEdges(userRoute.id).map((e) => cg!.getNode(e.target)?.name)).not.toContain(
      'auth',
    );
    write(
      'src/pages/Home.tsx',
      `export function Home(){const unused = () => <div/>; return new Response('now an endpoint')}`,
    );
    await cg.sync({ paths: ['src/pages/Home.tsx'] });
    expect(cg.getNodesByKind('route').some((n) => n.name === 'ANY /')).toBe(true);
    fs.unlinkSync(path.join(dir, 'src/pages/UserProfile.tsx'));
    await cg.sync();
    expect(cg.getNodesByKind('route').some((n) => n.name === 'ANY /users/:id')).toBe(true);
    write(
      'src/worker.tsx',
      imports + `export default defineApp([route('/new', () => new Response('ok'))]);`,
    );
    await cg.sync();
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['ANY /new']);
    fs.unlinkSync(path.join(dir, 'src/worker.tsx'));
    await cg.sync();
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it.each([false, true])('discovers a newly added SDK registration (scoped=%s)', async (scoped) => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-redwood-new-'));
    write('package.json', '{}');
    write('src/plain.ts', 'export function plain(){}');
    cg = await CodeGraph.init(dir, { index: true });
    write('package.json', JSON.stringify({ dependencies: { rwsdk: '1.7.3' } }));
    write(
      'src/worker.tsx',
      imports + `export default defineApp([route('/new', () => new Response('ok'))]);`,
    );
    await cg.sync(scoped ? { paths: ['src/worker.tsx'] } : undefined);
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['ANY /new']);
  });
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'classifies pages in fresh compiled workers',
    () => {
      dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-redwood-workers-'));
      write('package.json', JSON.stringify({ dependencies: { rwsdk: '1.7.3' } }));
      write('src/Home.tsx', 'export function Home(){return <div/>}');
      write(
        'src/worker.tsx',
        imports + `import {Home} from './Home'; export default defineApp([route('/',Home)]);`,
      );
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});
(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route')[0];
console.log(JSON.stringify([r.name,cg.getOutgoingEdges(r.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]));cg.close();})().catch(e=>{console.error(e);process.exit(1)});`;
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
    },
  );
});
