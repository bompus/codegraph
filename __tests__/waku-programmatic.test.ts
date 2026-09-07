import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { routeRoots } from '../src/ui-server/api/route-roots';

describe('Waku registered programmatic pages', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, content: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), content);
  };
  const setup = () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-waku-programmatic-'));
    write('package.json', '{"dependencies":{"waku":"1.0.0-rc.0"}}');
    write(
      'src/pages/index.tsx',
      'export default function HomePage() {\n  return <div>Home</div>;\n}\n',
    );
  };
  const entry = (body: string, parameter = '{createPage}', imports = '') =>
    `import {createPages} from 'waku';import adapter from 'waku/adapters/default';import HomePage from './pages/index.js';${imports}const pages=createPages(async (${parameter})=>${body});export default adapter(pages);`;
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
  it('indexes the pinned typegen fixture with the exact imported component', async () => {
    setup();
    // wakujs/waku@9f425e94996e018983cb629709544397b5f1e93b plugin-fs-router-typegen-with-createpages.
    write(
      'src/waku.server.tsx',
      `import { createPages } from 'waku';
import adapter from 'waku/adapters/default';
import HomePage from './pages/index.js';
const pages = createPages(async ({ createPage }) => [
  createPage({render: 'static', path: '/', component: HomePage}),
]);
export default adapter(pages);`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/']);
    const target = routeRoots(cg, routes()).get(routes()[0]!.id)!.node;
    expect([target.name, target.filePath]).toEqual(['HomePage', 'src/pages/index.tsx']);
  });
  it('registers direct calls after await, helper aliases and local components', async () => {
    setup();
    write(
      'src/waku.server.tsx',
      entry(
        `{await ready();page({render:'dynamic',path:'/users/[id]',component:HomePage});return [page({render:'static',path:'/local',component:Local})];}`,
        '{createPage:page}',
        'function Local(){return <p/>}function ready(){}',
      ),
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/local', '/users/:id']);
    expect(
      routeRoots(cg, routes()).get(routes().find((node) => node.name === '/local')!.id)!.node.name,
    ).toBe('Local');
  });
  it('expands static paths and preserves exact brackets', async () => {
    setup();
    write(
      'src/waku.server.tsx',
      entry(`[
createPage({render:'static',path:'/@[user]',staticPaths:['Jane Doe'],component:HomePage}),
createPage({render:'dynamic',path:'/docs/[...rest]',component:HomePage}),
createPage({render:'static',path:'/literal/[id]',exactPath:true,component:HomePage}),
createPage({render:'dynamic',path:'/(group)/nested/index.html',component:HomePage}),
]`),
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/@Jane-Doe', '/docs/*rest', '/literal/[id]', '/nested']);
  });
  it.each([
    `createPage({render:'dynamic',path:dynamic,component:HomePage})`,
    `createPage({render:'dynamic',path:'/x',component:HomePage,...options})`,
    `createPage({render:mode,path:'/x',component:HomePage})`,
    `createPage({render:'static',path:'/[id]',component:HomePage})`,
    `createPage({render:'dynamic',path:'/x',component:null})`,
    `createPage({render:'dynamic',path:'/x',component:HomePage,exactPath:flag})`,
    `createPage({render:'dynamic',path:'/x',component:wrap(HomePage)})`,
  ])('rejects unknown declarations: %s', async (call) => {
    setup();
    write('src/waku.server.tsx', entry(`[${call}]`));
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual([]);
  });
  it('excludes conditional calls, uncalled helpers, layouts and shadowed declarations', async () => {
    setup();
    write(
      'src/waku.server.tsx',
      entry(
        `{
function nested(){createPage({render:'static',path:'/nested',component:HomePage})}
if(flag) createPage({render:'static',path:'/conditional',component:HomePage});
createLayout({render:'static',path:'/layout',component:HomePage});
createPage({render:'static',path:'/real',component:HomePage});return [];
}`,
        '{createPage,createLayout}',
      ),
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/real']);
    write(
      'src/waku.server.tsx',
      entry(
        `{{const createPage=other;createPage({render:'static',path:'/fake',component:HomePage});}return [];}`,
      ),
    );
    await cg.sync();
    expect(names()).toEqual([]);
  });
  it.each(['if(true)return [];', 'if(flag)throw new Error();', 'while(true){}'])(
    'does not infer declarations after potentially terminating control flow: %s',
    async (prefix) => {
      setup();
      write(
        'src/waku.server.tsx',
        entry(
          `{${prefix}return [createPage({render:'static',path:'/never',component:HomePage})];}`,
        ),
      );
      cg = await CodeGraph.init(dir, { index: true });
      expect(names()).toEqual([]);
    },
  );
  it('requires the returned adapter registration and real imports', async () => {
    setup();
    const source = entry(`[createPage({render:'static',path:'/',component:HomePage})]`);
    for (const changed of [
      source.replace("from 'waku'", "from 'other'"),
      source.replace('adapter(pages)', 'adapter(other)'),
      source.replace('export default adapter(pages)', 'export default other'),
      source.replace('export default', 'pages.push(extra);export default'),
    ]) {
      write('src/waku.server.tsx', changed);
      if (cg) await cg.sync();
      else cg = await CodeGraph.init(dir, { index: true });
      expect(names()).toEqual([]);
    }
  });
  it('updates declaration and imported target changes through scoped reopened sync', async () => {
    setup();
    write(
      'src/waku.server.tsx',
      entry(`[createPage({render:'static',path:'/one',component:HomePage})]`),
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/one']);
    write(
      'src/waku.server.tsx',
      entry(`[createPage({render:'static',path:'/two',component:HomePage})]`),
    );
    await cg.sync({ paths: ['src/waku.server.tsx'] });
    expect(names()).toEqual(['/two']);
    cg.close();
    cg = await CodeGraph.open(dir);
    write('src/pages/index.tsx', 'function Decoy(){return <p/>}export default 1;');
    await cg.sync({ paths: ['src/pages/index.tsx'] });
    expect(
      [...routeRoots(cg, routes()).values()].filter((root) => root.node.kind === 'function'),
    ).toEqual([]);
    fs.unlinkSync(path.join(dir, 'src/waku.server.tsx'));
    await cg.sync();
    expect(names()).toEqual([]);
  });
  it('supports imported aliases, named exports, multiline functions and skip-build options', async () => {
    setup();
    write('src/components.tsx', 'export const Named =\n  () => <p/>;');
    write(
      'src/waku.server.tsx',
      `import {createPages as routes} from 'waku';import app from 'waku/adapters/default';import {Named as Page} from './components';const pages=routes(async({createPage:add})=>[add({render:'static',path:'/named',component:Page})],{unstable_skipBuild:()=>true});export default app(pages);`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/named']);
    expect(routeRoots(cg, routes()).get(routes()[0]!.id)!.node.name).toBe('Named');
  });
  it('switches from filesystem to programmatic routes and suppresses custom config', async () => {
    setup();
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual(['/']);
    write(
      'src/waku.server.tsx',
      entry(`[createPage({render:'static',path:'/registered',component:HomePage})]`),
    );
    await cg.sync({ paths: ['src/waku.server.tsx'] });
    expect(names()).toEqual(['/registered']);
    write('waku.config.ts', 'export default {basePath:"/base"};');
    await cg.sync({ paths: ['waku.config.ts'] });
    expect(names()).toEqual([]);
  });
  it('does not bind an imported component name shadowed by callback parameters', async () => {
    setup();
    write(
      'src/waku.server.tsx',
      entry(
        `[createPage({render:'static',path:'/fake',component:HomePage})]`,
        '{createPage,unknown:HomePage}',
      ),
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(names()).toEqual([]);
  });
  it('resolves exact imported components in fresh compiled workers', () => {
    setup();
    write(
      'src/waku.server.tsx',
      entry(`[createPage({render:'static',path:'/',component:HomePage})]`),
    );
    const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route').find(r=>r.id.startsWith('route:waku:'));const e=r&&cg.getOutgoingEdges(r.id).find(e=>e.kind==='references');console.log(JSON.stringify([r?.name,e&&cg.getNode(e.target)?.name]));cg.close()})().catch(e=>{console.error(e);process.exit(1)})`;
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
    expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual(['/', 'HomePage']);
  });
});
