import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { routeRoots } from '../src/ui-server/api/route-roots';

beforeAll(async () => {
  await initGrammars();
  await loadGrammarsForLanguages(['typescript', 'javascript']);
});
describe('registered Angular routes', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, source: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), source);
  };
  const setup = () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-angular-'));
    write('package.json', JSON.stringify({ dependencies: { '@angular/router': '21.0.0' } }));
    write('src/home.ts', 'export class Home {}');
    write('src/user.ts', 'export class User {}');
    write('src/other/home.ts', 'export class Home {}');
  };
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  // angular/angular@9a58353b1b680f162a55969965ae6a90ae20316d:
  // adev/src/content/tutorials/learn-angular/steps/14-routerLink/answer/src/app/app.routes.ts
  it('binds the official tutorial registration and refreshes imported arrays after reopening', async () => {
    setup();
    write(
      'src/app.routes.ts',
      `import {Routes} from '@angular/router'; import {Home} from './home'; import {User} from './user'; export const routes: Routes = [{path:'',component:Home},{path:'user',component:User}];`,
    );
    write(
      'src/app.config.ts',
      `import {provideRouter} from '@angular/router'; import {routes} from './app.routes'; export const appConfig = {providers:[provideRouter(routes)]};`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual(['/', '/user']);
    const roots = routeRoots(cg, routes);
    for (const [url, name, file] of [
      ['/', 'Home', 'src/home.ts'],
      ['/user', 'User', 'src/user.ts'],
    ]) {
      const root = roots.get(routes.find((n) => n.name === url)!.id)!.node;
      expect([root.name, root.filePath]).toEqual([name, file]);
    }
    cg.close();
    cg = await CodeGraph.open(dir);
    write(
      'src/app.routes.ts',
      `import {Home} from './home'; export const routes = [{path:'new',component:Home}];`,
    );
    await cg.sync();
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/new']);
    fs.unlinkSync(path.join(dir, 'src/app.routes.ts'));
    await cg.sync();
    expect(cg.getNodesByKind('route')).toEqual([]);
    write(
      'src/app.routes.ts',
      `import {Home} from './home'; export const routes = [{path:'added',component:Home}];`,
    );
    await cg.sync();
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/added']);
  });
  it('composes children and lazy components and arrays without indexing orphans', async () => {
    setup();
    write(
      'src/lazy.ts',
      `import {User} from './user'; export const CHILDREN = [{path:':id',component:User}];`,
    );
    write('src/default.ts', 'export default class Default {}');
    write(
      'src/app.ts',
      `import {provideRouter as router} from '@angular/router'; import {Home} from './home'; const unused = [{path:'orphan',component:Home}]; export const providers = [router([{path:'admin',children:[{path:'',component:Home},{path:'user',loadComponent:()=>import('./user').then(m=>m.User)}]},{path:'lazy',loadChildren:()=>import('./lazy').then(m=>m.CHILDREN)},{path:'default',loadComponent:()=>import('./default')},{path:'**',component:Home}])];`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(
      cg
        .getNodesByKind('route')
        .map((n) => n.name)
        .sort(),
    ).toEqual(['/*', '/admin', '/admin/user', '/default', '/lazy/:id']);
  });
  it('follows forChild only through a mounted lazy NgModule', async () => {
    setup();
    write(
      'src/child.ts',
      `import {NgModule} from '@angular/core'; import {RouterModule} from '@angular/router'; import {User} from './user'; @NgModule({imports:[RouterModule.forChild([{path:'user',component:User}])]}) export class ChildModule {}`,
    );
    write(
      'src/orphan.ts',
      `import {RouterModule} from '@angular/router'; import {Home} from './home'; const orphan = RouterModule.forChild([{path:'orphan',component:Home}]);`,
    );
    write(
      'src/app.ts',
      `import {RouterModule as Router} from '@angular/router'; export const routes = Router.forRoot([{path:'admin',loadChildren:()=>import('./child').then(m=>m.ChildModule)}]);`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/admin/user']);
  });
  it.each([
    `const routes = [{path:'orphan',component:Home}];`,
    `function wrapper(provideRouter){return provideRouter([{path:'shadow',component:Home}])}`,
    `provideRouter([{path:dynamic,component:Home}]);`,
    `provideRouter([{path:'x',matcher:match,component:Home}]);`,
    `provideRouter([{path:'x',outlet:'other',component:Home}]);`,
    `provideRouter([{path:'x',redirectTo:'other',component:Home}]);`,
    `provideRouter([{path:'x',component:Home,...extra}]);`,
    `provideRouter([{path:'x',component:Home,path:'other'}]);`,
    `provideRouter([{path:'x',loadComponent:()=>factory()}]);`,
    `{ const provideRouter = other; provideRouter([{path:'shadow',component:Home}]); }`,
    `false && provideRouter([{path:'never',component:Home}]);`,
    `provideRouter({path:'object',component:Home});`,
    `const routes = [{path:'old',component:Home}]; routes.pop(); provideRouter(routes);`,
    `const routes = [{path:'old',component:Home}]; routes[0].path = 'new'; provideRouter(routes);`,
  ])('rejects unsupported declarations: %s', async (body) => {
    setup();
    write(
      'src/app.ts',
      `import {provideRouter} from '@angular/router'; import {Home} from './home'; ${body}`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it.each([false, true])(
    'detects newly introduced Angular registrations (scoped=%s)',
    async (scoped) => {
      setup();
      write('package.json', '{}');
      cg = await CodeGraph.init(dir, { index: true });
      write('package.json', JSON.stringify({ dependencies: { '@angular/router': '21.0.0' } }));
      write(
        'src/app.ts',
        `import {provideRouter} from '@angular/router'; import {Home} from './home'; export const providers = [provideRouter([{path:'new',component:Home}])];`,
      );
      await cg.sync(scoped ? { paths: ['src/app.ts'] } : undefined);
      expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/new']);
    },
  );
  it('keeps matching parents, preferring a default child as the page root', async () => {
    setup();
    write(
      'src/app.ts',
      `import {provideRouter} from '@angular/router'; import {Home} from './home'; import {User} from './user'; provideRouter([{path:'empty',component:Home,children:[]},{path:'parent',component:Home,children:[{path:'child',component:User}]},{path:'index',component:Home,children:[{path:'',component:User}]}]);`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual([
      '/empty',
      '/index',
      '/parent',
      '/parent/child',
    ]);
    expect(routeRoots(cg, routes).get(routes.find((n) => n.name === '/index')!.id)!.node.name).toBe(
      'User',
    );
  });
  it.each(['routes.length=0;', 'const alias=routes; alias.length=0;', `routes['pop']();`])(
    'ignores mutated imported arrays: %s',
    async (mutation) => {
      setup();
      write(
        'src/routes.ts',
        `import {Home} from './home'; export const routes=[{path:'stale',component:Home}];`,
      );
      write(
        'src/app.ts',
        `import {provideRouter} from '@angular/router'; import {routes} from './routes'; ${mutation} provideRouter(routes);`,
      );
      cg = await CodeGraph.init(dir, { index: true });
      expect(cg.getNodesByKind('route')).toEqual([]);
    },
  );
  it('ignores forChild outside NgModule imports', async () => {
    setup();
    write(
      'src/child.ts',
      `import {NgModule} from '@angular/core'; import {RouterModule} from '@angular/router'; import {Home} from './home'; @NgModule({providers:[{provide:'token',useValue:RouterModule.forChild([{path:'fake',component:Home}])}]}) export class Child {}`,
    );
    write(
      'src/app.ts',
      `import {provideRouter} from '@angular/router'; provideRouter([{path:'parent',loadChildren:()=>import('./child').then(m=>m.Child)}]);`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'enriches fresh compiled parse/store workers',
    () => {
      setup();
      write(
        'src/routes.ts',
        `import {Home} from './home'; export const routes = [{path:'',component:Home}];`,
      );
      write(
        'src/app.ts',
        `import {provideRouter} from '@angular/router'; import {routes} from './routes'; export const providers=[provideRouter(routes)];`,
      );
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))}); (async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route')[0];console.log(JSON.stringify([r.name,cg.getOutgoingEdges(r.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]));cg.close();})().catch(e=>{console.error(e);process.exit(1)});`;
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
